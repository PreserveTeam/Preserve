use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Game {
    pub id: String,
    pub title: String,
    pub short_title: String,
    pub year: String,
    pub edition: String,
    pub size: String,
    pub status: String,
    pub version: Option<String>,
    pub accent: String,
    pub cover_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct GamesResponse {
    pub games: Vec<Game>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ManifestFile {
    pub path: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ExtractedPrerequisite {
    pub exe: String,
    pub args: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Prerequisite {
    pub name: String,
    pub file: ManifestFile,
    pub args: String,
    pub extracted: Option<ExtractedPrerequisite>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct GameManifest {
    pub version: String,
    pub entry_exe: String,
    pub total_size_bytes: u64,
    pub files: Vec<ManifestFile>,
    #[serde(default)]
    pub prerequisites: Vec<Prerequisite>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionResponse {
    pub access_token: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadPlan {
    pub game: Game,
    pub manifest: GameManifest,
    pub file_base_url: String,
    pub prerequisite_base_url: String,
    #[serde(default)]
    pub content_addressed: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceExchange<'a> {
    pub hwid: &'a str,
    pub nonce: &'a str,
    pub proof: &'a str,
}

#[derive(Clone, Debug, PartialEq)]
pub enum InstallPhase {
    Idle,
    Preparing,
    Downloading,
    Verifying,
    Prerequisites,
    Complete,
    Cancelled,
    Failed,
}

#[derive(Clone, Debug, PartialEq)]
pub struct InstallProgress {
    pub phase: InstallPhase,
    pub percent: u8,
    pub detail: String,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub bytes_per_second: u64,
    pub active_downloads: usize,
}

impl Default for InstallProgress {
    fn default() -> Self {
        Self {
            phase: InstallPhase::Idle,
            percent: 0,
            detail: String::new(),
            bytes_done: 0,
            bytes_total: 0,
            bytes_per_second: 0,
            active_downloads: 0,
        }
    }
}
