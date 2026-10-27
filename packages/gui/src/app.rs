mod about;
mod device;
mod image;
mod job;
mod whats_new;

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Instant;

use buildcast_core::Report;
use buildcast_core::disks::DiskInfo;
use buildcast_core::worker::{BufferSize, Notify, Progress, Shared};
use egui::{Align, Button, Frame, Layout, RichText, Stroke};

use self::about::AboutTab;
use self::image::{BmapChoice, Image};
use crate::card;
use crate::changelog::Release;
use crate::theme::{self, Palette};
use crate::update::{RELEASES_PAGE, UpdateState, Updater};

/// The BuildCast main window.
pub struct App {
    about: Option<AboutTab>,
    bmap: Option<BmapChoice>,
    buffer: BufferSize,
    cancel: Arc<AtomicBool>,
    cancelling: bool,
    compress: bool,
    confirm_close: bool,
    confirm_write: bool,
    disks: Vec<DiskInfo>,
    eject: bool,
    force: bool,
    image: Option<Image>,
    progress: Shared,
    quit_after_job: bool,
    #[cfg(windows)]
    remote: Option<crate::remote::RemoteJob>,
    repaint: Notify,
    // refresh requested mid-scan, the in-flight result is stale
    rescan: bool,
    // when the current result banner appeared, None once dismissed
    result_since: Option<Instant>,
    running: bool,
    // scans run off the UI thread: one unresponsive device can block
    // CreateFile and IOCTLs (or their Unix equivalents) for seconds
    scan: Option<mpsc::Receiver<Vec<DiskInfo>>>,
    sel: Option<usize>,
    show_all: bool,
    title: String,
    update_dismissed: bool,
    updater: Updater,
    verify: bool,
    whats_new: Vec<Release>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> App {
        theme::apply(&cc.egui_ctx);
        #[cfg(windows)]
        {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            if let Ok(handle) = cc.window_handle()
                && let RawWindowHandle::Win32(h) = handle.as_raw()
            {
                crate::win_drop::install(h.hwnd.get() as _, cc.egui_ctx.clone());
            }
        }
        let repaint = notify(&cc.egui_ctx);
        let mut app = App {
            about: None,
            bmap: None,
            buffer: BufferSize::default(),
            cancel: Arc::new(AtomicBool::new(false)),
            cancelling: false,
            compress: false,
            confirm_close: false,
            confirm_write: false,
            disks: Vec::new(),
            eject: true,
            force: false,
            image: None,
            progress: Arc::new(Mutex::new(Progress::idle())),
            quit_after_job: false,
            #[cfg(windows)]
            remote: None,
            repaint: repaint.clone(),
            rescan: false,
            result_since: None,
            running: false,
            scan: None,
            sel: None,
            show_all: false,
            title: String::new(),
            update_dismissed: false,
            updater: Updater::check(repaint),
            verify: true,
            whats_new: Release::unseen(),
        };
        app.refresh_disks();
        app
    }

    fn header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("BuildCast").size(22.0).strong());
            ui.label(
                RichText::new(concat!("v", env!("CARGO_PKG_VERSION")))
                    .weak()
                    .size(11.0),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                egui::global_theme_preference_switch(ui);
                if ui
                    .add(Button::new("ℹ").frame(false))
                    .on_hover_text("About BuildCast")
                    .clicked()
                {
                    self.about = Some(AboutTab::default());
                }
            });
        });
        ui.label(RichText::new("Write disk images to USB drives").weak());
    }

    // dropped files and a refresh when the user tabs back with a new stick
    fn handle_input(&mut self, ctx: &egui::Context) {
        let dropped = ctx.input(|i| i.raw.dropped_files.iter().find_map(|f| f.path.clone()));
        #[cfg(windows)]
        let dropped = dropped.or_else(|| crate::win_drop::take().into_iter().next());
        if let Some(p) = dropped
            && !self.running
        {
            self.set_image(p);
        }

        let refocused = ctx.input(|i| {
            i.events
                .iter()
                .any(|e| matches!(e, egui::Event::WindowFocused(true)))
        });
        if refocused && !self.running {
            self.refresh_disks();
        }

        if ctx.input(|i| i.viewport().close_requested()) && self.running && !self.quit_after_job {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.confirm_close = true;
        }
    }

    fn update_banner(&mut self, ui: &mut egui::Ui) {
        if self.update_dismissed {
            return;
        }
        let acc = Palette::of(ui).accent;
        let (text, color, spinner) = match self.updater.state() {
            UpdateState::Available(info) => {
                card::frame(ui)
                    .stroke(Stroke::new(1.0_f32, acc.gamma_multiply(0.6)))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!(
                                    "Update available: v{} (you have v{})",
                                    info.version,
                                    env!("CARGO_PKG_VERSION")
                                ))
                                .color(acc),
                            );
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                self.dismiss_button(ui);
                                // installing exits the app, never allow it
                                // mid-job
                                #[cfg(windows)]
                                if info.has_installer()
                                    && ui
                                        .add_enabled(!self.running, Button::new("Install now"))
                                        .on_disabled_hover_text("Wait for the running job")
                                        .clicked()
                                {
                                    self.updater.install(info.clone(), self.repaint.clone());
                                }
                                if ui.button("Release notes").clicked() {
                                    ui.ctx().open_url(egui::OpenUrl::new_tab(RELEASES_PAGE));
                                }
                            });
                        });
                    });
                return;
            }
            UpdateState::Downloading => (
                "Downloading the update. The installer starts shortly.".to_owned(),
                ui.visuals().text_color(),
                true,
            ),
            UpdateState::Failed(err) => (
                format!("Update failed: {}", Report(&*err)),
                ui.visuals().warn_fg_color,
                false,
            ),
            UpdateState::Unknown | UpdateState::UpToDate => return,
        };
        card::show(ui, |ui| {
            ui.horizontal(|ui| {
                if spinner {
                    ui.spinner();
                }
                ui.label(RichText::new(text).color(color).size(12.0));
                if !spinner {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        self.dismiss_button(ui);
                    });
                }
            });
        });
    }

    fn dismiss_button(&mut self, ui: &mut egui::Ui) {
        if ui
            .add(Button::new(RichText::new("✖").size(11.0)).frame(false))
            .on_hover_text("Dismiss")
            .clicked()
        {
            self.update_dismissed = true;
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = root.ctx().clone();
        self.handle_input(&ctx);
        self.poll_disks();
        let prog = self.progress.lock().unwrap().clone();
        self.track_job(&ctx, &prog);
        let busy = self.running;

        let panel = Frame::central_panel(root.style()).inner_margin(20.0);
        egui::CentralPanel::default().frame(panel).show(root, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    self.header(ui);
                    self.update_banner(ui);

                    card::heading(ui, "Image");
                    self.image_card(ui, busy);

                    card::heading(ui, "Device");
                    self.device_card(ui, busy);

                    self.options_row(ui, busy);
                    ui.add_space(4.0);
                    self.action_buttons(ui);
                    ui.add_space(6.0);

                    if busy {
                        self.progress_card(ui, &prog);
                    } else if prog.stage.is_terminal()
                        && (prog.stage == buildcast_core::worker::Stage::Error
                            || self.result_since.is_some())
                    {
                        self.result_banner(ui, &prog);
                    }
                });
        });

        if self.confirm_write {
            self.confirm_write_modal(&ctx);
        }
        if self.confirm_close {
            self.confirm_close_modal(&ctx);
        }
        if self.about.is_some() {
            self.about_modal(&ctx);
        } else if !self.whats_new.is_empty() {
            self.whats_new_modal(&ctx);
        }
    }
}

fn notify(ctx: &egui::Context) -> Notify {
    let ctx = ctx.clone();
    Arc::new(move || ctx.request_repaint())
}

fn file_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}
