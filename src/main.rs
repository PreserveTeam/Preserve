#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

mod api;
mod installer;
mod models;

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    time::Duration,
};

use eframe::egui::{
    self, Color32, CornerRadius, FontFamily, FontId, Margin, RichText, Stroke, TextureHandle, Vec2,
};
use models::{Game, InstallPhase, InstallProgress};

const BG: Color32 = Color32::from_rgb(9, 10, 11);
const PANEL: Color32 = Color32::from_rgb(17, 19, 19);
const LINE: Color32 = Color32::from_rgb(43, 46, 44);
const TEXT: Color32 = Color32::from_rgb(239, 241, 236);
const MUTED: Color32 = Color32::from_rgb(111, 116, 111);
const LIME: Color32 = Color32::from_rgb(200, 255, 61);
const ICON: &[u8] = include_bytes!("../assets/preserve.png");
const DM_SANS: &[u8] = include_bytes!("../assets/DMSans.ttf");
const MANROPE: &[u8] = include_bytes!("../assets/Manrope.ttf");

enum Event {
    Catalog(Result<Vec<Game>, String>),
    Cover(String, Vec<u8>),
    Progress(String, InstallProgress),
    Stopped(String, String, bool),
    Finished(String),
    CleanupFinished(String, Result<(), String>),
}

struct DownloadTask {
    target: PathBuf,
    progress: InstallProgress,
    cancel: Option<Arc<AtomicBool>>,
}

fn main() -> eframe::Result {
    lock_native_window();
    let icon = eframe::icon_data::from_png_bytes(ICON).expect("valid app icon");
    let viewport = egui::ViewportBuilder::default()
        .with_title("Preserve")
        .with_inner_size([960.0, 640.0])
        .with_min_inner_size([960.0, 640.0])
        .with_max_inner_size([960.0, 640.0])
        .with_resizable(false)
        .with_icon(Arc::new(icon));
    eframe::run_native(
        "Preserve",
        eframe::NativeOptions {
            viewport,
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(PreserveApp::new(cc)))),
    )
}

#[cfg(windows)]
fn lock_native_window() {
    std::thread::spawn(|| {
        use windows_sys::Win32::{
            Foundation::{HWND, LPARAM},
            UI::WindowsAndMessaging::{
                EnumWindows, GWL_STYLE, GetWindowLongPtrW, GetWindowTextLengthW,
                GetWindowThreadProcessId, IsWindowVisible, SWP_FRAMECHANGED, SWP_NOMOVE,
                SWP_NOSIZE, SWP_NOZORDER, SetWindowLongPtrW, SetWindowPos, WS_MAXIMIZEBOX,
                WS_THICKFRAME,
            },
        };
        use windows_sys::core::BOOL;

        unsafe extern "system" fn find_window(hwnd: HWND, parameter: LPARAM) -> BOOL {
            let state = unsafe { &mut *(parameter as *mut (u32, HWND)) };
            let mut process_id = 0;
            unsafe { GetWindowThreadProcessId(hwnd, &mut process_id) };
            if process_id == state.0
                && unsafe { IsWindowVisible(hwnd) } != 0
                && unsafe { GetWindowTextLengthW(hwnd) } > 0
            {
                state.1 = hwnd;
                return 0;
            }
            1
        }

        for _ in 0..100 {
            let mut state: (u32, HWND) = (std::process::id(), std::ptr::null_mut());
            unsafe { EnumWindows(Some(find_window), &mut state as *mut _ as LPARAM) };
            if !state.1.is_null() {
                unsafe {
                    let style = GetWindowLongPtrW(state.1, GWL_STYLE);
                    let locked = style & !(WS_THICKFRAME as isize | WS_MAXIMIZEBOX as isize);
                    SetWindowLongPtrW(state.1, GWL_STYLE, locked);
                    SetWindowPos(
                        state.1,
                        std::ptr::null_mut(),
                        0,
                        0,
                        0,
                        0,
                        SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED,
                    );
                }
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    });
}

#[cfg(not(windows))]
fn lock_native_window() {}

struct PreserveApp {
    runtime: Arc<tokio::runtime::Runtime>,
    api_url: String,
    tx: Sender<Event>,
    rx: Receiver<Event>,
    games: Vec<Game>,
    selected: Option<usize>,
    dialog_open: bool,
    manager_open: bool,
    cancel_confirmation: Option<String>,
    install_dirs: HashMap<String, PathBuf>,
    downloads: HashMap<String, DownloadTask>,
    pending_cleanup: HashMap<String, PathBuf>,
    message: String,
    covers: HashMap<String, TextureHandle>,
    mark: TextureHandle,
}

impl PreserveApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        configure_style(&cc.egui_ctx);
        let mark = texture_from_bytes(&cc.egui_ctx, "preserve-mark", ICON).expect("valid mark");
        let runtime = Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("Tokio runtime"),
        );
        let (tx, rx) = mpsc::channel();
        let default_api_url = option_env!("PRESERVE_API_URL").unwrap_or("https://preserve.st/api");
        let api_url =
            std::env::var("PRESERVE_API_URL").unwrap_or_else(|_| default_api_url.to_string());
        let app = Self {
            runtime,
            api_url,
            tx,
            rx,
            games: Vec::new(),
            selected: None,
            dialog_open: false,
            manager_open: false,
            cancel_confirmation: None,
            install_dirs: HashMap::new(),
            downloads: HashMap::new(),
            pending_cleanup: HashMap::new(),
            message: "Loading catalog…".into(),
            covers: HashMap::new(),
            mark,
        };
        app.load_catalog();
        app
    }

    fn load_catalog(&self) {
        let tx = self.tx.clone();
        let api_url = self.api_url.clone();
        self.runtime.spawn(async move {
            let result = match api::Api::new(api_url) {
                Ok(api) => api.games().await.map_err(|error| error.to_string()),
                Err(error) => Err(error.to_string()),
            };
            let _ = tx.send(Event::Catalog(result));
        });
    }

    fn receive_events(&mut self, ctx: &egui::Context) {
        while let Ok(event) = self.rx.try_recv() {
            match event {
                Event::Catalog(Ok(games)) => {
                    self.selected = games
                        .iter()
                        .position(|game| game.status == "available")
                        .or_else(|| (!games.is_empty()).then_some(0));
                    self.message.clear();
                    for game in &games {
                        self.install_dirs
                            .entry(game.id.clone())
                            .or_insert_with(|| default_install_dir(&game.id));
                        if let Some(url) = game.cover_url.clone() {
                            let id = game.id.clone();
                            let tx = self.tx.clone();
                            self.runtime.spawn(async move {
                                if let Ok(response) = reqwest::get(url).await
                                    && let Ok(bytes) = response.bytes().await
                                {
                                    let _ = tx.send(Event::Cover(id, bytes.to_vec()));
                                }
                            });
                        }
                    }
                    self.games = games;
                }
                Event::Catalog(Err(error)) => self.message = error,
                Event::Cover(id, bytes) => {
                    if let Some(texture) = texture_from_bytes(ctx, &format!("cover-{id}"), &bytes) {
                        self.covers.insert(id, texture);
                    }
                }
                Event::Progress(game_id, progress) => {
                    if matches!(
                        progress.phase,
                        InstallPhase::Complete | InstallPhase::Cancelled | InstallPhase::Failed
                    ) && let Some(task) = self.downloads.get_mut(&game_id)
                    {
                        task.cancel = None;
                    }
                    if let Some(task) = self.downloads.get_mut(&game_id) {
                        task.progress = progress;
                    }
                }
                Event::Stopped(game_id, error, cancelled) => {
                    if let Some(target) = self.pending_cleanup.remove(&game_id) {
                        self.clear_install_dir(game_id, target);
                        continue;
                    }
                    if let Some(task) = self.downloads.get_mut(&game_id) {
                        task.cancel = None;
                        task.progress.phase = if cancelled {
                            InstallPhase::Cancelled
                        } else {
                            InstallPhase::Failed
                        };
                        task.progress.detail = if cancelled {
                            "Download paused. Resume whenever you're ready.".into()
                        } else {
                            error
                        };
                        task.progress.bytes_per_second = 0;
                        task.progress.active_downloads = 0;
                    }
                }
                Event::Finished(game_id) => {
                    if let Some(target) = self.pending_cleanup.remove(&game_id) {
                        self.clear_install_dir(game_id, target);
                    }
                }
                Event::CleanupFinished(game_id, result) => {
                    if let Err(error) = result {
                        self.message =
                            format!("Could not clear {game_id}'s download directory: {error}");
                    }
                }
            }
        }
    }

    fn begin_install(&mut self, game_id: &str) {
        let Some(game) = self.games.iter().find(|game| game.id == game_id) else {
            return;
        };
        let Some(target) = self.install_dirs.get(game_id).cloned() else {
            return;
        };
        if self.is_running(game_id) {
            return;
        }
        let game_id = game.id.clone();
        let api_url = self.api_url.clone();
        let tx = self.tx.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        let progress = InstallProgress {
            phase: InstallPhase::Preparing,
            percent: 0,
            detail: "Preparing".into(),
            bytes_done: 0,
            bytes_total: 0,
            bytes_per_second: 0,
            active_downloads: 0,
        };
        self.downloads.insert(
            game_id.clone(),
            DownloadTask {
                target: target.clone(),
                progress,
                cancel: Some(cancelled.clone()),
            },
        );
        let event_game_id = game_id.clone();
        self.runtime.spawn(async move {
            let result = match api::Api::new(api_url) {
                Ok(api) => {
                    installer::install(api, game_id, target, cancelled, |progress| {
                        let _ = tx.send(Event::Progress(event_game_id.clone(), progress));
                    })
                    .await
                }
                Err(error) => Err(error),
            };
            match result {
                Ok(()) => {
                    let _ = tx.send(Event::Finished(event_game_id));
                }
                Err(error) => {
                    let cancelled = error.to_string() == "Installation cancelled";
                    let _ = tx.send(Event::Stopped(event_game_id, error.to_string(), cancelled));
                }
            }
        });
    }

    fn is_running(&self, game_id: &str) -> bool {
        self.downloads.get(game_id).is_some_and(|task| {
            matches!(
                task.progress.phase,
                InstallPhase::Preparing
                    | InstallPhase::Downloading
                    | InstallPhase::Verifying
                    | InstallPhase::Prerequisites
            )
        })
    }

    fn pause_install(&self, game_id: &str) {
        if let Some(cancel) = self
            .downloads
            .get(game_id)
            .and_then(|task| task.cancel.as_ref())
        {
            cancel.store(true, Ordering::Relaxed);
        }
    }

    fn cancel_install(&mut self, game_id: &str) {
        if let Some(task) = self.downloads.remove(game_id) {
            if let Some(cancel) = task.cancel {
                self.pending_cleanup
                    .insert(game_id.to_string(), task.target);
                cancel.store(true, Ordering::Relaxed);
            } else {
                self.clear_install_dir(game_id.to_string(), task.target);
            }
        }
    }

    fn clear_install_dir(&self, game_id: String, target: PathBuf) {
        let tx = self.tx.clone();
        self.runtime.spawn(async move {
            let result = match tokio::fs::remove_dir_all(&target).await {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error.to_string()),
            };
            let _ = tx.send(Event::CleanupFinished(game_id, result));
        });
    }

    fn progress_for(&self, game_id: &str) -> InstallProgress {
        self.downloads
            .get(game_id)
            .map(|task| task.progress.clone())
            .unwrap_or_default()
    }
}

impl eframe::App for PreserveApp {
    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = root.ctx().clone();
        self.receive_events(&ctx);
        ctx.request_repaint_after(Duration::from_millis(100));

        egui::Frame::new()
            .fill(BG)
            .inner_margin(Margin::same(30))
            .show(root, |ui| {
                ui.set_min_size(Vec2::new(900.0, 580.0));

                ui.horizontal(|ui| {
                    ui.image((self.mark.id(), Vec2::splat(24.0)));
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new("preserve")
                            .font(manrope(18.0))
                            .strong()
                            .color(TEXT),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if !self.downloads.is_empty() {
                            let running = self
                                .downloads
                                .values()
                                .filter(|task| {
                                    matches!(
                                        task.progress.phase,
                                        InstallPhase::Preparing
                                            | InstallPhase::Downloading
                                            | InstallPhase::Verifying
                                            | InstallPhase::Prerequisites
                                    )
                                })
                                .count();
                            let label = if running > 0 {
                                format!("Downloads  {running}")
                            } else {
                                "Downloads".into()
                            };
                            if animated_button(ui, &label, false, true, Vec2::new(124.0, 34.0))
                                .clicked()
                            {
                                self.dialog_open = false;
                                self.manager_open = true;
                            }
                        }
                    });
                });
                ui.add_space(44.0);
                if !self.message.is_empty() {
                    ui.label(RichText::new(&self.message).size(10.0).color(MUTED));
                    ui.add_space(16.0);
                }
                egui::ScrollArea::vertical()
                    .id_salt("catalog")
                    .max_height(485.0)
                    .show(ui, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            for index in 0..self.games.len() {
                                let card_progress = self
                                    .downloads
                                    .get(&self.games[index].id)
                                    .map(|task| task.progress.clone());
                                if game_card(
                                    ui,
                                    &self.games[index],
                                    self.covers.get(&self.games[index].id),
                                    card_progress.as_ref(),
                                ) {
                                    self.selected = Some(index);
                                    self.manager_open = false;
                                    self.dialog_open = true;
                                }
                                ui.add_space(16.0);
                            }
                        });
                    });
            });

        if self.dialog_open {
            let selected_game = self
                .selected
                .and_then(|index| self.games.get(index))
                .cloned();
            if let Some(game) = selected_game {
                let cover = self.covers.get(&game.id).cloned();
                let response = egui::Modal::new(egui::Id::new("install-dialog"))
                    .backdrop_color(Color32::from_black_alpha(205))
                    .frame(
                        egui::Frame::new()
                            .fill(Color32::from_rgba_premultiplied(17, 19, 19, 248))
                            .stroke(Stroke::new(1.0, LINE))
                            .corner_radius(CornerRadius::same(14))
                            .shadow(egui::epaint::Shadow {
                                offset: [0, 12],
                                blur: 36,
                                spread: 2,
                                color: Color32::from_black_alpha(180),
                            })
                            .inner_margin(Margin::same(26)),
                    )
                    .show(&ctx, |ui| install_dialog(ui, self, &game, cover.as_ref()));
                if response.should_close() || response.inner {
                    self.dialog_open = false;
                }
            }
        }

        if self.manager_open {
            let response = egui::Modal::new(egui::Id::new("download-manager"))
                .backdrop_color(Color32::from_black_alpha(205))
                .frame(
                    egui::Frame::new()
                        .fill(Color32::from_rgba_premultiplied(17, 19, 19, 248))
                        .stroke(Stroke::new(1.0, LINE))
                        .corner_radius(CornerRadius::same(14))
                        .shadow(egui::epaint::Shadow {
                            offset: [0, 12],
                            blur: 36,
                            spread: 2,
                            color: Color32::from_black_alpha(180),
                        })
                        .inner_margin(Margin::same(26)),
                )
                .show(&ctx, |ui| download_manager(ui, self));
            if response.should_close() || response.inner {
                self.manager_open = false;
            }
        }

        if let Some(game_id) = self.cancel_confirmation.clone() {
            let title = self
                .games
                .iter()
                .find(|game| game.id == game_id)
                .map(|game| game.title.as_str())
                .unwrap_or("this game");
            let response = egui::Modal::new(egui::Id::new("cancel-confirmation"))
                .backdrop_color(Color32::from_black_alpha(220))
                .frame(
                    egui::Frame::new()
                        .fill(Color32::from_rgba_premultiplied(17, 19, 19, 252))
                        .stroke(Stroke::new(1.0, LINE))
                        .corner_radius(CornerRadius::same(12))
                        .shadow(egui::epaint::Shadow {
                            offset: [0, 10],
                            blur: 30,
                            spread: 1,
                            color: Color32::from_black_alpha(190),
                        })
                        .inner_margin(Margin::same(24)),
                )
                .show(&ctx, |ui| cancel_confirmation(ui, title));
            if response.should_close() || response.inner == Some(false) {
                self.cancel_confirmation = None;
            } else if response.inner == Some(true) {
                self.cancel_install(&game_id);
                self.cancel_confirmation = None;
                if self.downloads.is_empty() {
                    self.manager_open = false;
                }
            }
        }
    }
}

fn install_dialog(
    ui: &mut egui::Ui,
    app: &mut PreserveApp,
    game: &Game,
    cover: Option<&TextureHandle>,
) -> bool {
    ui.set_width(680.0);
    let mut close = false;
    let progress = app.progress_for(&game.id);
    let running = app.is_running(&game.id);
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.label(
                RichText::new(&game.title)
                    .font(manrope(30.0))
                    .strong()
                    .color(TEXT),
            );
            ui.label(
                RichText::new(format!("{}  ·  {}", game.year, game.size))
                    .size(11.0)
                    .color(MUTED),
            );
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
            if close_icon_button(ui).clicked() {
                close = true;
            }
        });
    });
    ui.add_space(22.0);
    ui.horizontal(|ui| {
        draw_cover(ui, game, cover, Vec2::new(168.0, 252.0));
        ui.add_space(24.0);
        ui.allocate_ui_with_layout(
            Vec2::new(448.0, 252.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.set_width(448.0);
                let location = app
                    .install_dirs
                    .get(&game.id)
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| default_install_dir(&game.id).display().to_string());
                if folder_picker(ui, &location, !running).clicked()
                    && let Some(folder) = rfd::FileDialog::new()
                        .set_title("Choose install location")
                        .pick_folder()
                {
                    app.install_dirs.insert(game.id.clone(), folder);
                }
                ui.add_space(12.0);
                if progress.phase != InstallPhase::Idle {
                    egui::Frame::new()
                        .fill(Color32::from_rgb(12, 14, 14))
                        .stroke(Stroke::new(1.0, LINE))
                        .corner_radius(CornerRadius::same(9))
                        .inner_margin(Margin::same(13))
                        .show(ui, |ui| {
                            ui.set_width(420.0);
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(phase_name(&progress.phase))
                                        .size(9.0)
                                        .color(LIME),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.label(
                                            RichText::new(format!("{}%", progress.percent))
                                                .font(manrope(14.0))
                                                .strong()
                                                .color(TEXT),
                                        );
                                    },
                                );
                            });
                            ui.add_space(5.0);
                            ui.label(RichText::new(&progress.detail).size(10.0).color(MUTED));
                            ui.add_space(9.0);
                            progress_bar(ui, progress.percent as f32 / 100.0, 420.0);
                            ui.add_space(10.0);
                            ui.horizontal(|ui| {
                                metric_card(
                                    ui,
                                    "DOWNLOADED",
                                    &format!(
                                        "{} / {}",
                                        format_bytes(progress.bytes_done),
                                        format_bytes(progress.bytes_total)
                                    ),
                                );
                                metric_card(ui, "SPEED", &format_speed(progress.bytes_per_second));
                                metric_card(ui, "REMAINING", &format_eta(&progress));
                            });
                        });
                    ui.add_space(12.0);
                }

                ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                    ui.horizontal(|ui| {
                        if running {
                            if animated_button(ui, "Cancel", false, true, Vec2::new(112.0, 42.0))
                                .clicked()
                            {
                                app.cancel_confirmation = Some(game.id.clone());
                            }
                            if animated_button(
                                ui,
                                "Click to pause",
                                true,
                                true,
                                Vec2::new(302.0, 42.0),
                            )
                            .clicked()
                            {
                                app.pause_install(&game.id);
                            }
                        } else {
                            let label = if progress.phase == InstallPhase::Complete {
                                "Installed"
                            } else if progress.phase == InstallPhase::Cancelled {
                                "Resume install"
                            } else {
                                "Install game"
                            };
                            let enabled = game.status == "available"
                                && app.install_dirs.contains_key(&game.id)
                                && progress.phase != InstallPhase::Complete;
                            let paused = progress.phase == InstallPhase::Cancelled;
                            if paused
                                && animated_button(
                                    ui,
                                    "Cancel",
                                    false,
                                    true,
                                    Vec2::new(112.0, 42.0),
                                )
                                .clicked()
                            {
                                app.cancel_confirmation = Some(game.id.clone());
                            }
                            let width = if paused { 302.0 } else { 422.0 };
                            if animated_button(ui, label, true, enabled, Vec2::new(width, 42.0))
                                .clicked()
                            {
                                app.begin_install(&game.id);
                            }
                        }
                    });
                });
            },
        );
    });
    close
}

fn close_icon_button(ui: &mut egui::Ui) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(34.0), egui::Sense::click());
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    let hover = ui
        .ctx()
        .animate_bool_responsive(response.id.with("close-hover"), response.hovered());
    let fill = Color32::from_rgb(23, 25, 25).lerp_to_gamma(Color32::from_rgb(37, 40, 38), hover);
    ui.painter().rect(
        rect,
        CornerRadius::same(8),
        fill,
        Stroke::new(
            1.0,
            LINE.lerp_to_gamma(Color32::from_rgb(90, 96, 91), hover),
        ),
        egui::StrokeKind::Inside,
    );
    let center = rect.center();
    let radius = 4.2;
    let color = MUTED.lerp_to_gamma(TEXT, hover);
    ui.painter().line_segment(
        [
            center + Vec2::new(-radius, -radius),
            center + Vec2::new(radius, radius),
        ],
        Stroke::new(1.6, color),
    );
    ui.painter().line_segment(
        [
            center + Vec2::new(radius, -radius),
            center + Vec2::new(-radius, radius),
        ],
        Stroke::new(1.6, color),
    );
    response
}

fn folder_picker(ui: &mut egui::Ui, path: &str, enabled: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(422.0, 52.0),
        if enabled {
            egui::Sense::click()
        } else {
            egui::Sense::hover()
        },
    );
    let response = response.on_hover_cursor(if enabled {
        egui::CursorIcon::PointingHand
    } else {
        egui::CursorIcon::NotAllowed
    });
    let hover = ui.ctx().animate_bool_responsive(
        response.id.with("folder-hover"),
        enabled && response.hovered(),
    );
    ui.painter().rect(
        rect,
        CornerRadius::same(10),
        Color32::from_rgb(12, 14, 14).lerp_to_gamma(Color32::from_rgb(17, 20, 18), hover),
        Stroke::new(
            1.0,
            LINE.lerp_to_gamma(Color32::from_rgb(78, 89, 58), hover),
        ),
        egui::StrokeKind::Inside,
    );
    let icon = egui::Rect::from_center_size(
        egui::pos2(rect.left() + 27.0, rect.center().y),
        Vec2::splat(28.0),
    );
    ui.painter()
        .rect_filled(icon, CornerRadius::same(7), Color32::from_rgb(25, 29, 24));
    let folder = icon.shrink2(Vec2::new(7.0, 8.0));
    let points = [
        egui::pos2(folder.left(), folder.top() + 4.0),
        egui::pos2(folder.left() + 6.0, folder.top() + 4.0),
        egui::pos2(folder.left() + 8.0, folder.top() + 1.0),
        egui::pos2(folder.right(), folder.top() + 1.0),
        egui::pos2(folder.right(), folder.bottom()),
        egui::pos2(folder.left(), folder.bottom()),
        egui::pos2(folder.left(), folder.top() + 4.0),
    ];
    ui.painter()
        .add(egui::Shape::line(points.to_vec(), Stroke::new(1.4, LIME)));
    let path_rect = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 50.0, rect.center().y - 10.0),
        egui::pos2(rect.right() - 102.0, rect.center().y + 10.0),
    );
    ui.scope_builder(egui::UiBuilder::new().max_rect(path_rect), |ui| {
        ui.add_sized(
            path_rect.size(),
            egui::Label::new(RichText::new(path).size(10.0).color(TEXT))
                .truncate()
                .selectable(false),
        );
    });
    let choose_rect = egui::Rect::from_center_size(
        egui::pos2(rect.right() - 48.0, rect.center().y),
        Vec2::new(76.0, 32.0),
    );
    ui.painter().rect(
        choose_rect,
        CornerRadius::same(7),
        Color32::from_rgb(24, 27, 25).lerp_to_gamma(Color32::from_rgb(34, 39, 34), hover),
        Stroke::new(
            1.0,
            LINE.lerp_to_gamma(Color32::from_rgb(75, 86, 57), hover),
        ),
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        choose_rect.center(),
        egui::Align2::CENTER_CENTER,
        "Choose",
        FontId::new(10.0, FontFamily::Proportional),
        MUTED.lerp_to_gamma(TEXT, hover),
    );
    response
}

fn progress_bar(ui: &mut egui::Ui, fraction: f32, width: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 7.0), egui::Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::same(4), Color32::from_rgb(29, 32, 30));
    let fill_width = rect.width() * fraction.clamp(0.0, 1.0);
    if fill_width > 1.0 {
        let fill = egui::Rect::from_min_size(rect.min, Vec2::new(fill_width, rect.height()));
        ui.painter().rect_filled(fill, CornerRadius::same(4), LIME);
    }
}

fn download_manager(ui: &mut egui::Ui, app: &mut PreserveApp) -> bool {
    ui.set_width(620.0);
    ui.set_min_height(440.0);
    let mut close = false;
    let mut cancel: Option<String> = None;
    let mut pause: Option<String> = None;
    let mut resume: Option<String> = None;
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.label(
                RichText::new("Downloads")
                    .font(manrope(28.0))
                    .strong()
                    .color(TEXT),
            );
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
            if close_icon_button(ui).clicked() {
                close = true;
            }
        });
    });
    ui.add_space(18.0);

    let ids: Vec<String> = app
        .games
        .iter()
        .filter(|game| app.downloads.contains_key(&game.id))
        .map(|game| game.id.clone())
        .collect();
    egui::ScrollArea::vertical()
        .id_salt("download-manager-list")
        .max_height(390.0)
        .show(ui, |ui| {
            for game_id in ids {
                let Some(task) = app.downloads.get(&game_id) else {
                    continue;
                };
                let progress = task.progress.clone();
                let target = task.target.display().to_string();
                let title = app
                    .games
                    .iter()
                    .find(|game| game.id == game_id)
                    .map(|game| game.title.clone())
                    .unwrap_or_else(|| game_id.clone());
                let running = matches!(
                    progress.phase,
                    InstallPhase::Preparing
                        | InstallPhase::Downloading
                        | InstallPhase::Verifying
                        | InstallPhase::Prerequisites
                );
                egui::Frame::new()
                    .fill(Color32::from_rgb(12, 14, 14))
                    .stroke(Stroke::new(1.0, LINE))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(Margin::same(14))
                    .show(ui, |ui| {
                        ui.set_width(588.0);
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.label(
                                    RichText::new(&title)
                                        .font(manrope(16.0))
                                        .strong()
                                        .color(TEXT),
                                );
                                ui.label(
                                    RichText::new(phase_name(&progress.phase)).size(8.0).color(
                                        if progress.phase == InstallPhase::Failed {
                                            Color32::from_rgb(255, 118, 102)
                                        } else {
                                            LIME
                                        },
                                    ),
                                );
                            });
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label(
                                        RichText::new(format!("{}%", progress.percent))
                                            .font(manrope(16.0))
                                            .strong()
                                            .color(TEXT),
                                    );
                                },
                            );
                        });
                        ui.add_space(9.0);
                        progress_bar(ui, progress.percent as f32 / 100.0, 588.0);
                        ui.add_space(9.0);
                        ui.horizontal(|ui| {
                            ui.add_sized(
                                [168.0, 16.0],
                                egui::Label::new(
                                    RichText::new(format!(
                                        "{} / {}",
                                        format_bytes(progress.bytes_done),
                                        format_bytes(progress.bytes_total)
                                    ))
                                    .size(9.5)
                                    .color(MUTED),
                                )
                                .truncate(),
                            );
                            ui.label(RichText::new("•").size(9.0).color(LINE));
                            ui.add_sized(
                                [84.0, 16.0],
                                egui::Label::new(
                                    RichText::new(format_speed(progress.bytes_per_second))
                                        .size(9.5)
                                        .color(TEXT),
                                )
                                .truncate(),
                            );
                            ui.label(RichText::new("•").size(9.0).color(LINE));
                            ui.add_sized(
                                [112.0, 16.0],
                                egui::Label::new(
                                    RichText::new(format!("{} remaining", format_eta(&progress)))
                                        .size(9.5)
                                        .color(MUTED),
                                )
                                .truncate(),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if running {
                                        if animated_button(
                                            ui,
                                            "Cancel",
                                            false,
                                            true,
                                            Vec2::new(74.0, 30.0),
                                        )
                                        .clicked()
                                        {
                                            cancel = Some(game_id.clone());
                                        }
                                        if animated_button(
                                            ui,
                                            "Pause",
                                            false,
                                            true,
                                            Vec2::new(74.0, 30.0),
                                        )
                                        .clicked()
                                        {
                                            pause = Some(game_id.clone());
                                        }
                                    } else if matches!(
                                        progress.phase,
                                        InstallPhase::Cancelled | InstallPhase::Failed
                                    ) {
                                        if animated_button(
                                            ui,
                                            "Resume",
                                            false,
                                            true,
                                            Vec2::new(74.0, 30.0),
                                        )
                                        .clicked()
                                        {
                                            resume = Some(game_id.clone());
                                        }
                                        if progress.phase == InstallPhase::Cancelled
                                            && animated_button(
                                                ui,
                                                "Cancel",
                                                false,
                                                true,
                                                Vec2::new(74.0, 30.0),
                                            )
                                            .clicked()
                                        {
                                            cancel = Some(game_id.clone());
                                        }
                                    }
                                },
                            );
                        });
                        ui.add_sized(
                            [580.0, 16.0],
                            egui::Label::new(
                                RichText::new(target)
                                    .size(8.5)
                                    .color(Color32::from_rgb(75, 80, 76)),
                            )
                            .truncate(),
                        );
                    });
                ui.add_space(10.0);
            }
        });

    if let Some(game_id) = cancel {
        app.cancel_confirmation = Some(game_id);
    }
    if let Some(game_id) = pause {
        app.pause_install(&game_id);
    }
    if let Some(game_id) = resume {
        app.begin_install(&game_id);
    }
    close
}

fn cancel_confirmation(ui: &mut egui::Ui, title: &str) -> Option<bool> {
    ui.set_width(360.0);
    ui.label(
        RichText::new("Cancel download?")
            .font(manrope(24.0))
            .strong()
            .color(TEXT),
    );
    ui.add_space(10.0);
    ui.label(
        RichText::new(format!(
            "Are you sure you want to cancel {title}? Its downloaded files will be removed from the selected install folder."
        ))
        .size(11.0)
        .line_height(Some(17.0))
        .color(MUTED),
    );
    ui.add_space(22.0);
    let mut choice = None;
    ui.horizontal(|ui| {
        if animated_button(ui, "Go back", false, true, Vec2::new(156.0, 40.0)).clicked() {
            choice = Some(false);
        }
        if danger_button(ui, "Cancel download", Vec2::new(196.0, 40.0)).clicked() {
            choice = Some(true);
        }
    });
    choice
}

fn metric_card(ui: &mut egui::Ui, label: &str, value: &str) {
    egui::Frame::new()
        .fill(Color32::from_rgb(17, 19, 19))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(9, 7))
        .show(ui, |ui| {
            ui.set_width(110.0);
            ui.set_min_height(28.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new(label).size(8.0).color(MUTED));
                ui.label(RichText::new(value).size(10.0).color(TEXT));
            });
        });
}

fn default_install_dir(game_id: &str) -> PathBuf {
    directories::BaseDirs::new()
        .map(|dirs| {
            dirs.home_dir()
                .join(".preserve")
                .join("games")
                .join(game_id)
        })
        .unwrap_or_else(|| PathBuf::from(".preserve").join("games").join(game_id))
}

fn format_bytes(bytes: u64) -> String {
    if bytes == 0 {
        return "0 B".into();
    }
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn format_speed(bytes_per_second: u64) -> String {
    if bytes_per_second == 0 {
        "—".into()
    } else {
        format!("{}/s", format_bytes(bytes_per_second))
    }
}

fn format_eta(progress: &InstallProgress) -> String {
    if progress.bytes_per_second == 0 || progress.bytes_total <= progress.bytes_done {
        return "—".into();
    }
    let seconds = (progress.bytes_total - progress.bytes_done) / progress.bytes_per_second.max(1);
    if seconds < 60 {
        "< 1 min".into()
    } else if seconds < 3600 {
        format!("{} min", seconds / 60)
    } else {
        format!("{}h {}m", seconds / 3600, (seconds % 3600) / 60)
    }
}

fn phase_name(phase: &InstallPhase) -> &'static str {
    match phase {
        InstallPhase::Preparing => "PREPARING",
        InstallPhase::Downloading => "DOWNLOADING",
        InstallPhase::Verifying => "VERIFIED",
        InstallPhase::Prerequisites => "PREREQUISITES",
        InstallPhase::Complete => "COMPLETE",
        InstallPhase::Cancelled => "PAUSED",
        InstallPhase::Failed => "NEEDS ATTENTION",
        InstallPhase::Idle => "READY",
    }
}

fn game_card(
    ui: &mut egui::Ui,
    game: &Game,
    texture: Option<&TextureHandle>,
    progress: Option<&InstallProgress>,
) -> bool {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(144.0, 232.0), egui::Sense::click());
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    let hover = ui
        .ctx()
        .animate_bool_responsive(response.id.with("hover"), response.hovered());
    let cover = egui::Rect::from_min_size(rect.min + Vec2::new(3.0, 3.0), Vec2::new(138.0, 207.0));
    let border = LINE.lerp_to_gamma(LIME, hover);
    ui.painter().rect(
        cover,
        CornerRadius::same(7),
        PANEL,
        Stroke::new(1.0 + hover, border),
        egui::StrokeKind::Inside,
    );
    let image_rect = cover.shrink(3.0);
    if let Some(texture) = texture {
        egui::Image::new((texture.id(), image_rect.size()))
            .corner_radius(CornerRadius::same(4))
            .paint_at(ui, image_rect);
    } else {
        ui.painter().text(
            image_rect.center(),
            egui::Align2::CENTER_CENTER,
            &game.short_title,
            FontId::new(18.0, FontFamily::Proportional),
            LIME,
        );
    }
    if let Some(progress) = progress
        && progress.phase != InstallPhase::Idle
    {
        let track = egui::Rect::from_min_max(
            egui::pos2(image_rect.left() + 7.0, image_rect.bottom() - 12.0),
            egui::pos2(image_rect.right() - 7.0, image_rect.bottom() - 7.0),
        );
        ui.painter()
            .rect_filled(track, CornerRadius::same(3), Color32::from_black_alpha(190));
        let fill = egui::Rect::from_min_size(
            track.min,
            Vec2::new(
                track.width() * progress.percent as f32 / 100.0,
                track.height(),
            ),
        );
        ui.painter().rect_filled(fill, CornerRadius::same(3), LIME);
    }
    ui.painter().text(
        egui::pos2(rect.left() + 2.0, rect.bottom() - 8.0),
        egui::Align2::LEFT_BOTTOM,
        &game.title,
        FontId::new(11.0, FontFamily::Proportional),
        TEXT.lerp_to_gamma(LIME, hover * 0.35),
    );
    response.clicked()
}

fn draw_cover(ui: &mut egui::Ui, game: &Game, texture: Option<&TextureHandle>, size: Vec2) {
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    ui.painter().rect(
        rect,
        CornerRadius::same(7),
        PANEL,
        Stroke::new(1.0, LINE),
        egui::StrokeKind::Inside,
    );
    let image_rect = rect.shrink(3.0);
    if let Some(texture) = texture {
        egui::Image::new((texture.id(), image_rect.size()))
            .corner_radius(CornerRadius::same(4))
            .paint_at(ui, image_rect);
    } else {
        ui.painter().text(
            image_rect.center(),
            egui::Align2::CENTER_CENTER,
            &game.short_title,
            FontId::new(20.0, FontFamily::Proportional),
            LIME,
        );
    }
}

fn animated_button(
    ui: &mut egui::Ui,
    label: &str,
    primary: bool,
    enabled: bool,
    size: Vec2,
) -> egui::Response {
    let sense = if enabled {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    let response = response.on_hover_cursor(if enabled {
        egui::CursorIcon::PointingHand
    } else {
        egui::CursorIcon::NotAllowed
    });
    let hover = ui
        .ctx()
        .animate_bool_responsive(response.id.with("hover"), enabled && response.hovered());
    let (base, over, text) = if primary {
        (LIME, Color32::from_rgb(220, 255, 118), BG)
    } else {
        (
            Color32::from_rgb(25, 27, 27),
            Color32::from_rgb(36, 40, 37),
            TEXT,
        )
    };
    let mut fill = base.lerp_to_gamma(over, hover);
    let mut foreground = text;
    if !enabled {
        fill = fill.gamma_multiply(0.45);
        foreground = foreground.gamma_multiply(0.48);
    }
    ui.painter().rect(
        rect,
        CornerRadius::same(7),
        fill,
        Stroke::new(
            1.0,
            if primary {
                fill
            } else {
                LINE.lerp_to_gamma(LIME, hover * 0.5)
            },
        ),
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        FontId::new(11.0, FontFamily::Proportional),
        foreground,
    );
    response
}

fn danger_button(ui: &mut egui::Ui, label: &str, size: Vec2) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    let hover = ui
        .ctx()
        .animate_bool_responsive(response.id.with("danger-hover"), response.hovered());
    let accent = Color32::from_rgb(255, 118, 102);
    ui.painter().rect(
        rect,
        CornerRadius::same(7),
        Color32::from_rgb(38, 23, 22).lerp_to_gamma(Color32::from_rgb(58, 29, 26), hover),
        Stroke::new(
            1.0,
            Color32::from_rgb(90, 43, 38).lerp_to_gamma(accent, hover),
        ),
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        FontId::new(11.0, FontFamily::Proportional),
        Color32::from_rgb(255, 193, 184).lerp_to_gamma(Color32::WHITE, hover),
    );
    response
}

fn texture_from_bytes(ctx: &egui::Context, name: &str, bytes: &[u8]) -> Option<TextureHandle> {
    let image = image::load_from_memory(bytes).ok()?.to_rgba8();
    let size = [image.width() as usize, image.height() as usize];
    Some(ctx.load_texture(
        name,
        egui::ColorImage::from_rgba_unmultiplied(size, image.as_raw()),
        egui::TextureOptions::LINEAR,
    ))
}

fn manrope(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("Manrope".into()))
}

fn configure_style(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "dm_sans".into(),
        Arc::new(egui::FontData::from_static(DM_SANS)),
    );
    fonts.font_data.insert(
        "manrope".into(),
        Arc::new(egui::FontData::from_static(MANROPE)),
    );
    fonts
        .families
        .get_mut(&FontFamily::Proportional)
        .expect("proportional family")
        .insert(0, "dm_sans".into());
    fonts.families.insert(
        FontFamily::Name("Manrope".into()),
        vec!["manrope".into(), "dm_sans".into()],
    );
    ctx.set_fonts(fonts);
    ctx.set_theme(egui::Theme::Dark);
    let mut style = (*ctx.style_of(egui::Theme::Dark)).clone();
    style.spacing.item_spacing = Vec2::new(8.0, 8.0);
    style.spacing.button_padding = Vec2::new(16.0, 11.0);
    style.visuals = egui::Visuals::dark();
    style.visuals.panel_fill = BG;
    style.visuals.window_fill = BG;
    style.visuals.override_text_color = Some(TEXT);
    style.visuals.widgets.inactive.bg_fill = Color32::from_rgb(25, 27, 27);
    style.visuals.widgets.hovered.bg_fill = Color32::from_rgb(34, 37, 35);
    style.visuals.widgets.active.bg_fill = Color32::from_rgb(39, 42, 39);
    style.visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, LINE);
    style.visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, LIME);
    style.visuals.selection.bg_fill = LIME;
    ctx.set_style_of(egui::Theme::Dark, style);
}

#[cfg(test)]
mod ui_tests {
    use super::*;

    #[test]
    fn formats_binary_scale_without_skipping_kilobytes() {
        assert_eq!(format_bytes(54_300_000_000), "54.3 GB");
        assert_eq!(format_speed(870_400), "870.4 KB/s");
    }
}
