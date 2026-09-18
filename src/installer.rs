use std::{
    collections::VecDeque,
    path::{Component, Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use directories::{BaseDirs, ProjectDirs};
use futures_util::StreamExt;
use reqwest::{Client, StatusCode, header::RANGE};
use sha2::{Digest, Sha256};
use tokio::{
    fs,
    io::{AsyncWriteExt, BufWriter},
    process::Command,
    time::sleep,
};
use url::Url;
use uuid::Uuid;

use crate::{
    api::Api,
    models::{DownloadPlan, InstallPhase, InstallProgress, ManifestFile, Prerequisite},
};

static INSTALL_INDEX_LOCK: Mutex<()> = Mutex::new(());

pub async fn install(
    api: Api,
    game_id: String,
    target: PathBuf,
    cancelled: Arc<AtomicBool>,
    mut report: impl FnMut(InstallProgress),
) -> Result<()> {
    report(progress(
        InstallPhase::Preparing,
        0,
        "Preparing a verified download",
        0,
        0,
        0,
        0,
    ));
    let plan = api.download_plan(&game_id).await?;
    fs::create_dir_all(&target).await?;

    let total = plan.manifest.total_size_bytes;
    const PARALLEL_DOWNLOADS: usize = 8;
    let completed = Arc::new(AtomicU64::new(0));
    let active = Arc::new(AtomicUsize::new(0));
    let current = Arc::new(Mutex::new(String::new()));
    let client = api.client().clone();
    let file_base_url = plan.file_base_url.clone();
    let content_addressed = plan.content_addressed;

    let downloads =
        futures_util::stream::iter(plan.manifest.files.clone().into_iter().map(|file| {
            let client = client.clone();
            let target = target.clone();
            let completed = completed.clone();
            let active = active.clone();
            let current = current.clone();
            let cancelled = cancelled.clone();
            let file_base_url = file_base_url.clone();
            async move {
                if cancelled.load(Ordering::Relaxed) {
                    bail!("Installation cancelled");
                }
                active.fetch_add(1, Ordering::Relaxed);
                if let Ok(mut label) = current.lock() {
                    *label = file.path.clone();
                }
                let destination = safe_join(&target, &file.path)?;
                let object_key = if content_addressed {
                    &file.sha256
                } else {
                    &file.path
                };
                let url = asset_url(&file_base_url, object_key)?;
                let mut latest = 0u64;
                let result =
                    download_file(&client, url, &destination, &file, &cancelled, |bytes| {
                        completed.fetch_add(bytes.saturating_sub(latest), Ordering::Relaxed);
                        latest = bytes;
                    })
                    .await;
                active.fetch_sub(1, Ordering::Relaxed);
                result
            }
        }))
        .buffer_unordered(PARALLEL_DOWNLOADS);
    tokio::pin!(downloads);

    let mut ticker = tokio::time::interval(Duration::from_millis(160));
    let mut samples = VecDeque::new();
    loop {
        tokio::select! {
            result = downloads.next() => match result {
                Some(result) => result?,
                None => break,
            },
            _ = ticker.tick() => {
                if cancelled.load(Ordering::Relaxed) {
                    bail!("Installation cancelled");
                }
                let now = Instant::now();
                let done = completed.load(Ordering::Relaxed).min(total);
                samples.push_back((now, done));
                while samples.front().is_some_and(|(at, _)| now.duration_since(*at) > Duration::from_secs(5)) {
                    samples.pop_front();
                }
                let speed = samples.front().map(|(at, bytes)| {
                    let seconds = now.duration_since(*at).as_secs_f64();
                    if seconds > 0.25 { ((done.saturating_sub(*bytes)) as f64 / seconds) as u64 } else { 0 }
                }).unwrap_or(0);
                let count = active.load(Ordering::Relaxed);
                let label = current.lock().map(|value| value.clone()).unwrap_or_default();
                let detail = if count > 1 {
                    format!("Downloading {count} files · {label}")
                } else {
                    format!("Downloading {label}")
                };
                let percent = ((done as f64 / total.max(1) as f64) * 94.0).round() as u8;
                report(progress(InstallPhase::Downloading, percent.min(94), &detail, done, total, speed, count));
            }
        }
    }

    let completed = completed.load(Ordering::Relaxed).min(total);
    report(progress(
        InstallPhase::Verifying,
        95,
        "All files verified",
        completed,
        total,
        0,
        0,
    ));

    install_prerequisites(api.client(), &plan, &target, &cancelled, &mut report).await?;
    write_marker(&plan, &target).await?;
    report(progress(
        InstallPhase::Complete,
        100,
        &format!("{} is ready", plan.game.title),
        total,
        total,
        0,
        0,
    ));
    Ok(())
}

fn progress(
    phase: InstallPhase,
    percent: u8,
    detail: &str,
    bytes_done: u64,
    bytes_total: u64,
    bytes_per_second: u64,
    active_downloads: usize,
) -> InstallProgress {
    InstallProgress {
        phase,
        percent,
        detail: detail.to_string(),
        bytes_done,
        bytes_total,
        bytes_per_second,
        active_downloads,
    }
}

fn safe_join(root: &Path, relative: &str) -> Result<PathBuf> {
    let path = Path::new(relative);
    if path
        .components()
        .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
    {
        bail!("Manifest contains an unsafe path");
    }
    Ok(root.join(path))
}

fn asset_url(base: &str, path: &str) -> Result<Url> {
    let mut url = Url::parse(&format!("{}/", base.trim_end_matches('/')))?;
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| anyhow::anyhow!("Invalid download origin"))?;
        segments.pop_if_empty();
        for segment in path.split('/') {
            segments.push(segment);
        }
    }
    Ok(url)
}

async fn download_file(
    client: &Client,
    url: Url,
    destination: &Path,
    expected: &ManifestFile,
    cancelled: &AtomicBool,
    mut on_bytes: impl FnMut(u64),
) -> Result<()> {
    if cancelled.load(Ordering::Relaxed) {
        bail!("Installation cancelled");
    }
    if is_valid(destination, expected).await? {
        on_bytes(expected.size);
        return Ok(());
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).await?;
    }
    let partial = destination.with_extension(format!(
        "{}preserve-part",
        destination
            .extension()
            .map(|v| format!("{}.", v.to_string_lossy()))
            .unwrap_or_default()
    ));
    let mut offset = fs::metadata(&partial).await.map(|m| m.len()).unwrap_or(0);
    if offset > expected.size {
        fs::remove_file(&partial).await.ok();
        offset = 0;
    }

    let mut hash = if offset > 0 {
        hash_state(&partial).await?
    } else {
        Sha256::new()
    };
    let mut response = fetch_with_retry(client, url.clone(), offset).await?;
    if offset > 0 && response.status() != StatusCode::PARTIAL_CONTENT {
        fs::remove_file(&partial).await.ok();
        offset = 0;
        hash = Sha256::new();
        response = fetch_with_retry(client, url.clone(), 0).await?;
    }
    on_bytes(offset);
    let mut received = offset;
    let mut reconnects = 0u32;
    loop {
        if cancelled.load(Ordering::Relaxed) {
            bail!("Installation cancelled");
        }
        if received > 0 && response.status() != StatusCode::PARTIAL_CONTENT {
            bail!(
                "Download server does not support resuming {}",
                expected.path
            );
        }
        let file = fs::OpenOptions::new()
            .create(true)
            .append(received > 0)
            .write(true)
            .truncate(received == 0)
            .open(&partial)
            .await?;
        let mut writer = BufWriter::new(file);
        let mut stream = response.bytes_stream();
        let mut stream_failed = false;
        while let Some(chunk) = stream.next().await {
            if cancelled.load(Ordering::Relaxed) {
                writer.flush().await?;
                bail!("Installation cancelled");
            }
            match chunk {
                Ok(chunk) => {
                    writer.write_all(&chunk).await?;
                    hash.update(&chunk);
                    received += chunk.len() as u64;
                    on_bytes(received);
                }
                Err(_) => {
                    stream_failed = true;
                    break;
                }
            }
        }
        writer.flush().await?;
        drop(writer);
        if received == expected.size {
            break;
        }
        if received > expected.size {
            bail!("Download exceeded the expected size for {}", expected.path);
        }
        reconnects += 1;
        if reconnects > 6 {
            if stream_failed {
                bail!("Download repeatedly disconnected for {}", expected.path);
            }
            bail!("Download ended early for {}", expected.path);
        }
        sleep(Duration::from_millis(500 * reconnects as u64)).await;
        response = fetch_with_retry(client, url.clone(), received).await?;
    }

    if !hex::encode(hash.finalize()).eq_ignore_ascii_case(&expected.sha256) {
        bail!("Integrity check failed for {}", expected.path);
    }
    fs::remove_file(destination).await.ok();
    fs::rename(partial, destination).await?;
    Ok(())
}

async fn hash_state(path: &Path) -> Result<Sha256> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || -> Result<Sha256> {
        use std::io::Read;
        let mut file = std::fs::File::open(path)?;
        let mut hash = Sha256::new();
        let mut buffer = vec![0u8; 1024 * 1024];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hash.update(&buffer[..read]);
        }
        Ok(hash)
    })
    .await?
}

async fn fetch_with_retry(client: &Client, url: Url, offset: u64) -> Result<reqwest::Response> {
    let mut last_error = None;
    for attempt in 0..3 {
        let mut request = client.get(url.clone());
        if offset > 0 {
            request = request.header(RANGE, format!("bytes={offset}-"));
        }
        match request.send().await {
            Ok(response) if response.status().is_success() => return Ok(response),
            Ok(response) => {
                last_error = Some(anyhow::anyhow!("Download returned {}", response.status()))
            }
            Err(error) => last_error = Some(error.into()),
        }
        sleep(Duration::from_millis(700 * (attempt + 1))).await;
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("Download failed")))
}

async fn is_valid(path: &Path, expected: &ManifestFile) -> Result<bool> {
    let metadata = match fs::metadata(path).await {
        Ok(value) => value,
        Err(_) => return Ok(false),
    };
    if metadata.len() != expected.size {
        return Ok(false);
    }
    let path = path.to_owned();
    let digest = tokio::task::spawn_blocking(move || -> Result<String> {
        use std::io::Read;
        let mut file = std::fs::File::open(path)?;
        let mut hash = Sha256::new();
        let mut buffer = vec![0u8; 1024 * 1024];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hash.update(&buffer[..read]);
        }
        Ok(hex::encode(hash.finalize()))
    })
    .await??;
    Ok(digest.eq_ignore_ascii_case(&expected.sha256))
}

async fn install_prerequisites(
    client: &Client,
    plan: &DownloadPlan,
    target: &Path,
    cancelled: &AtomicBool,
    report: &mut impl FnMut(InstallProgress),
) -> Result<()> {
    let root = target.join(".preserve").join("prerequisites");
    for prerequisite in &plan.manifest.prerequisites {
        if cancelled.load(Ordering::Relaxed) {
            bail!("Installation cancelled");
        }
        report(progress(
            InstallPhase::Prerequisites,
            97,
            &format!("Installing {}", prerequisite.name),
            0,
            0,
            0,
            0,
        ));
        let destination = safe_join(&root, &prerequisite.file.path)?;
        let object_key = if plan.content_addressed {
            &prerequisite.file.sha256
        } else {
            &prerequisite.file.path
        };
        let url = asset_url(&plan.prerequisite_base_url, object_key)?;
        download_file(
            client,
            url,
            &destination,
            &prerequisite.file,
            cancelled,
            |_| {},
        )
        .await?;
        run_prerequisite(prerequisite, &destination, target).await?;
    }
    Ok(())
}

async fn run_prerequisite(prerequisite: &Prerequisite, file: &Path, target: &Path) -> Result<()> {
    if let Some(extracted) = &prerequisite.extracted {
        let extraction = target
            .join(".preserve")
            .join(format!("prereq-{}", Uuid::new_v4()));
        fs::create_dir_all(&extraction).await?;
        let args = prerequisite
            .args
            .split_ascii_whitespace()
            .map(|arg| arg.replace("{extract}", &extraction.to_string_lossy()))
            .collect::<Vec<_>>();
        ensure_success(
            Command::new(file).args(args).status().await?,
            &prerequisite.name,
        )?;
        let nested = extraction.join(&extracted.exe);
        ensure_success(
            Command::new(nested)
                .args(extracted.args.split_ascii_whitespace())
                .status()
                .await?,
            &prerequisite.name,
        )?;
        fs::remove_dir_all(extraction).await.ok();
    } else if file
        .extension()
        .is_some_and(|value| value.eq_ignore_ascii_case("msi"))
    {
        let mut args = vec!["/i".to_string(), file.to_string_lossy().into_owned()];
        args.extend(
            prerequisite
                .args
                .split_ascii_whitespace()
                .map(str::to_string),
        );
        ensure_success(
            Command::new("msiexec.exe").args(args).status().await?,
            &prerequisite.name,
        )?;
    } else {
        ensure_success(
            Command::new(file)
                .args(prerequisite.args.split_ascii_whitespace())
                .status()
                .await?,
            &prerequisite.name,
        )?;
    }
    Ok(())
}

fn ensure_success(status: std::process::ExitStatus, name: &str) -> Result<()> {
    if status.success() {
        Ok(())
    } else {
        bail!("{} failed with {}", name, status)
    }
}

async fn write_marker(plan: &DownloadPlan, target: &Path) -> Result<()> {
    let marker = serde_json::json!({
        "schema": 1, "id": plan.game.id, "title": plan.game.title,
        "version": plan.manifest.version, "path": target,
        "executable": target.join(&plan.manifest.entry_exe), "verified": true
    });
    let project =
        ProjectDirs::from("games", "Preserve", "Preserve").context("Could not locate app data")?;
    let markers = project.data_dir().join("installations");
    fs::create_dir_all(&markers).await?;
    fs::write(
        markers.join(format!("{}.json", plan.game.id)),
        serde_json::to_vec_pretty(&marker)?,
    )
    .await?;

    let index_game = marker.clone();
    let game_id = plan.game.id.clone();
    tokio::task::spawn_blocking(move || -> Result<()> {
        let _guard = INSTALL_INDEX_LOCK
            .lock()
            .map_err(|_| anyhow::anyhow!("Installation index lock failed"))?;
        let home = BaseDirs::new().context("Could not locate the user profile")?;
        let preserve = home.home_dir().join(".preserve");
        std::fs::create_dir_all(&preserve)?;
        let index_path = preserve.join("index.json");
        let mut index: serde_json::Value = if index_path.exists() {
            serde_json::from_slice(&std::fs::read(&index_path)?)
                .unwrap_or_else(|_| serde_json::json!({ "schema": 1, "games": {} }))
        } else {
            serde_json::json!({ "schema": 1, "games": {} })
        };
        if !index.get("games").is_some_and(serde_json::Value::is_object) {
            index["games"] = serde_json::json!({});
        }
        index["schema"] = serde_json::json!(1);
        index["games"][&game_id] = index_game;
        let temporary = preserve.join("index.json.tmp");
        std::fs::write(&temporary, serde_json::to_vec_pretty(&index)?)?;
        if index_path.exists() {
            std::fs::remove_file(&index_path)?;
        }
        std::fs::rename(temporary, index_path)?;
        Ok(())
    })
    .await??;

    #[cfg(windows)]
    {
        use winreg::{RegKey, enums::HKEY_CURRENT_USER};
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (key, _) =
            hkcu.create_subkey(format!("Software\\Preserve\\Games\\{}", plan.game.id))?;
        key.set_value("InstallPath", &target.to_string_lossy().into_owned())?;
        key.set_value("Version", &plan.manifest.version)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_manifest_path_traversal() {
        assert!(safe_join(Path::new("C:\\Games"), "..\\escape.exe").is_err());
        assert!(safe_join(Path::new("C:\\Games"), "C:\\Windows\\bad.exe").is_err());
        assert!(safe_join(Path::new("C:\\Games"), "data\\game.bin").is_ok());
    }

    #[test]
    fn encodes_asset_paths() {
        let url = asset_url("https://cdn.example.test/files", "folder/a file.bin").unwrap();
        assert_eq!(
            url.as_str(),
            "https://cdn.example.test/files/folder/a%20file.bin"
        );
    }
}
