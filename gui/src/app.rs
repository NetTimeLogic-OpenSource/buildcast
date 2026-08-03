use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use egui::{
    Align, Button, Color32, ComboBox, CornerRadius, Frame, Label, Layout, ProgressBar, RichText,
    Stroke,
};

use buildcast_core::disks::{self, DiskInfo, human_size};
use buildcast_core::stream;
use buildcast_core::worker::{self, JobOpts, Notify, Progress, Stage};

const MAX_DISK_BYTES: u64 = 256 * 1024 * 1024 * 1024;
const BUF_MIB: [usize; 10] = [1, 2, 4, 8, 16, 32, 64, 128, 256, 512];

fn accent(dark: bool) -> Color32 {
    if dark {
        Color32::from_rgb(0x5b, 0x93, 0xff)
    } else {
        Color32::from_rgb(0x2f, 0x6f, 0xed)
    }
}
fn danger(dark: bool) -> Color32 {
    if dark {
        Color32::from_rgb(0xef, 0x5a, 0x52)
    } else {
        Color32::from_rgb(0xc6, 0x28, 0x28)
    }
}
fn success(dark: bool) -> Color32 {
    if dark {
        Color32::from_rgb(0x6f, 0xc5, 0x74)
    } else {
        Color32::from_rgb(0x2e, 0x7d, 0x32)
    }
}

pub struct App {
    image: Option<PathBuf>,
    image_info: Option<(stream::Format, u64, Option<u64>)>,
    image_err: Option<String>,
    bmap: Option<PathBuf>,
    bmap_note: Option<String>,
    disks: Vec<DiskInfo>,
    sel: Option<usize>,
    verify: bool,
    compress: bool,
    force: bool,
    eject: bool,
    show_all: bool,
    buf_idx: usize,
    progress: worker::Shared,
    cancel: Arc<AtomicBool>,
    running: bool,
    confirm_write: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> App {
        // styles are per-theme; set both
        for theme in [egui::Theme::Dark, egui::Theme::Light] {
            let dark = theme == egui::Theme::Dark;
            cc.egui_ctx.style_mut_of(theme, |s| {
                s.spacing.item_spacing = egui::vec2(10.0, 8.0);
                s.spacing.button_padding = egui::vec2(14.0, 8.0);
                s.spacing.interact_size.y = 26.0;
                for w in [
                    &mut s.visuals.widgets.noninteractive,
                    &mut s.visuals.widgets.inactive,
                    &mut s.visuals.widgets.hovered,
                    &mut s.visuals.widgets.active,
                    &mut s.visuals.widgets.open,
                ] {
                    w.corner_radius = CornerRadius::same(7);
                }
                // checkbox ticks, combo highlights and focus rings follow the accent
                s.visuals.selection.bg_fill =
                    accent(dark).gamma_multiply(if dark { 0.5 } else { 0.35 });
                s.visuals.selection.stroke = Stroke::new(1.0_f32, accent(dark));
                s.visuals.hyperlink_color = accent(dark);
                s.visuals.slider_trailing_fill = true;
            });
        }
        let mut app = App {
            image: None,
            image_info: None,
            image_err: None,
            bmap: None,
            bmap_note: None,
            disks: Vec::new(),
            sel: None,
            verify: true,
            compress: false,
            force: false,
            eject: true,
            show_all: false,
            buf_idx: 2,
            progress: Arc::new(Mutex::new(Progress::idle())),
            cancel: Arc::new(AtomicBool::new(false)),
            running: false,
            confirm_write: false,
        };
        app.refresh_disks();
        app
    }

    fn refresh_disks(&mut self) {
        let prev = self
            .sel
            .and_then(|i| self.disks.get(i))
            .map(|d| d.id.clone());
        self.disks = disks::list_disks(self.show_all, MAX_DISK_BYTES);
        self.sel = match prev {
            Some(id) => self.disks.iter().position(|d| d.id == id),
            None => None,
        };
        if self.sel.is_none() && self.disks.len() == 1 {
            self.sel = Some(0);
        }
    }

    fn set_image(&mut self, path: PathBuf) {
        match stream::probe(&path) {
            Ok(info) => {
                self.image_info = Some(info);
                self.image_err = None;
            }
            Err(e) => {
                self.image_info = None;
                self.image_err = Some(e.to_string());
            }
        }
        match auto_bmap(&path) {
            Some(b) => self.set_bmap(b),
            None => {
                self.bmap = None;
                self.bmap_note = None;
            }
        }
        self.image = Some(path);
    }

    fn set_bmap(&mut self, path: PathBuf) {
        self.bmap_note = Some(
            match std::fs::read_to_string(&path)
                .map_err(|e| e.to_string())
                .and_then(|xml| buildcast_core::bmap::parse(&xml).map_err(|e| e.to_string()))
            {
                Ok(bm) => format!(
                    "writes only {} of {}",
                    human_size(bm.mapped_bytes()),
                    human_size(bm.image_size)
                ),
                Err(e) => {
                    self.bmap = None;
                    self.bmap_note = Some(format!("invalid bmap: {e}"));
                    return;
                }
            },
        );
        self.bmap = Some(path);
    }

    fn notify(ctx: &egui::Context) -> Notify {
        let ctx = ctx.clone();
        Arc::new(move || ctx.request_repaint())
    }

    fn browse_image(&mut self) {
        let dlg = rfd::FileDialog::new()
            .add_filter(
                "Disk images",
                &[
                    "img", "iso", "bin", "raw", "dd", "wic", "gz", "bz2", "xz", "zst", "zip",
                    "tar", "tgz", "txz", "tbz",
                ],
            )
            .add_filter("All files", &["*"]);
        if let Some(p) = dlg.pick_file() {
            self.set_image(p);
        }
    }

    fn job_opts(&self, compress: bool) -> JobOpts {
        JobOpts {
            verify: self.verify,
            buf_bytes: BUF_MIB[self.buf_idx] * 1024 * 1024,
            compress,
            force: self.force,
            eject: self.eject,
        }
    }

    fn start_job(&mut self) {
        self.cancel.store(false, Ordering::Relaxed);
        *self.progress.lock().unwrap() = Progress {
            stage: Stage::Starting,
            message: "Starting…".into(),
            ..Progress::idle()
        };
        self.running = true;
    }

    fn start_write(&mut self, ctx: &egui::Context) {
        let (Some(image), Some(disk)) = (
            self.image.clone(),
            self.sel.and_then(|i| self.disks.get(i)).cloned(),
        ) else {
            return;
        };
        self.start_job();
        match self.bmap.clone() {
            Some(bmap) => worker::spawn_write_bmap(
                image,
                bmap,
                disk,
                self.job_opts(false),
                self.progress.clone(),
                self.cancel.clone(),
                Self::notify(ctx),
            ),
            None => worker::spawn_write(
                image,
                disk,
                self.job_opts(false),
                self.progress.clone(),
                self.cancel.clone(),
                Self::notify(ctx),
            ),
        }
    }

    fn start_read(&mut self, ctx: &egui::Context) {
        let Some(disk) = self.sel.and_then(|i| self.disks.get(i)).cloned() else {
            return;
        };
        let stamp = chrono::Local::now().format("%Y%m%dT%H%M%S");
        let ext = if self.compress { ".dd.zst" } else { ".dd" };
        let dir = dirs::desktop_dir()
            .or_else(dirs::download_dir)
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("."));
        let Some(out) = rfd::FileDialog::new()
            .set_directory(dir)
            .set_file_name(format!("buildcast-{stamp}{ext}"))
            .save_file()
        else {
            return;
        };
        let compress = self.compress;
        self.start_job();
        worker::spawn_read(
            out,
            disk,
            self.job_opts(compress),
            self.progress.clone(),
            self.cancel.clone(),
            Self::notify(ctx),
        );
    }

    fn card_frame(ui: &egui::Ui) -> Frame {
        Frame::group(ui.style())
            .corner_radius(CornerRadius::same(9))
            .inner_margin(12.0)
            .fill(ui.visuals().faint_bg_color)
    }

    fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
        Self::card_frame(ui)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                add(ui)
            })
            .inner
    }

    fn section(ui: &mut egui::Ui, title: &str) {
        ui.add_space(8.0);
        ui.label(
            RichText::new(title.to_uppercase())
                .weak()
                .size(11.0)
                .strong(),
        );
    }

    fn image_card(&mut self, ui: &mut egui::Ui, busy: bool, drag_hover: bool) {
        let stroke = if drag_hover {
            Stroke::new(2.0_f32, accent(ui.visuals().dark_mode))
        } else {
            ui.visuals().widgets.noninteractive.bg_stroke
        };
        Self::card_frame(ui).stroke(stroke).show(ui, |ui| {
            ui.set_width(ui.available_width());
            match self.image.clone() {
                Some(p) => {
                    ui.horizontal(|ui| {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui
                                .add_enabled(!busy, Button::new("✖").frame(false))
                                .on_hover_text("Clear")
                                .clicked()
                            {
                                self.image = None;
                                self.image_info = None;
                                self.image_err = None;
                                self.bmap = None;
                                self.bmap_note = None;
                            }
                            if ui.add_enabled(!busy, Button::new("Browse…")).clicked() {
                                self.browse_image();
                            }
                            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                ui.vertical(|ui| {
                                    let name = p
                                        .file_name()
                                        .map(|s| s.to_string_lossy().to_string())
                                        .unwrap_or_default();
                                    ui.add(Label::new(RichText::new(name).strong()).truncate());
                                    if let Some((fmt, comp, uncomp)) = &self.image_info {
                                        let mut s =
                                            format!("{} · {}", fmt.name(), human_size(*comp));
                                        if let Some(u) = uncomp
                                            && *fmt != stream::Format::Raw
                                        {
                                            s += &format!(" ({} uncompressed)", human_size(*u));
                                        }
                                        ui.label(RichText::new(s).weak().size(12.0));
                                    } else if let Some(e) = &self.image_err {
                                        ui.label(
                                            RichText::new(format!("⚠ {e}"))
                                                .color(ui.visuals().warn_fg_color)
                                                .size(12.0),
                                        );
                                    }
                                    ui.add(
                                        Label::new(
                                            RichText::new(p.to_string_lossy().to_string())
                                                .weak()
                                                .size(11.0),
                                        )
                                        .truncate(),
                                    );
                                    self.bmap_row(ui, busy);
                                });
                            });
                        });
                    });
                }
                None => {
                    ui.vertical_centered(|ui| {
                        ui.add_space(8.0);
                        ui.label(RichText::new("💾").size(28.0));
                        if drag_hover {
                            ui.label(
                                RichText::new("Release to select this image")
                                    .strong()
                                    .color(accent(ui.visuals().dark_mode)),
                            );
                        } else {
                            ui.label(RichText::new("Drop a disk image here").strong());
                        }
                        ui.label(
                            RichText::new("raw · gz · bz2 · xz · zst · zip · tar")
                                .weak()
                                .size(12.0),
                        );
                        ui.add_space(2.0);
                        if ui
                            .add_enabled(!busy, Button::new("Browse files…"))
                            .clicked()
                        {
                            self.browse_image();
                        }
                        ui.add_space(6.0);
                    });
                }
            }
        });
    }

    fn bmap_row(&mut self, ui: &mut egui::Ui, busy: bool) {
        ui.horizontal(|ui| match (self.bmap.clone(), self.bmap_note.clone()) {
            (Some(b), note) => {
                let name = b
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                let suffix = note.map(|n| format!("; {n}")).unwrap_or_default();
                ui.label(
                    RichText::new(format!("bmap: {name}{suffix}"))
                        .color(accent(ui.visuals().dark_mode))
                        .size(12.0),
                );
                if ui
                    .add_enabled(
                        !busy,
                        Button::new(RichText::new("✖").size(11.0)).frame(false),
                    )
                    .on_hover_text("Don't use the bmap (write full image)")
                    .clicked()
                {
                    self.bmap = None;
                    self.bmap_note = None;
                }
            }
            (None, Some(note)) => {
                ui.label(
                    RichText::new(format!("⚠ {note}"))
                        .color(ui.visuals().warn_fg_color)
                        .size(12.0),
                );
                self.bmap_button(ui, busy);
            }
            (None, None) => {
                self.bmap_button(ui, busy);
            }
        });
    }

    fn bmap_button(&mut self, ui: &mut egui::Ui, busy: bool) {
        if ui
            .add_enabled(!busy, Button::new(RichText::new("Add bmap…").size(12.0)))
            .on_hover_text("Optional .bmap: write only mapped blocks, verify with its sha256 sums")
            .clicked()
            && let Some(p) = rfd::FileDialog::new()
                .add_filter("bmap", &["bmap"])
                .pick_file()
        {
            self.set_bmap(p);
        }
    }

    fn device_card(&mut self, ui: &mut egui::Ui, busy: bool) {
        Self::card(ui, |ui| {
            ui.horizontal(|ui| {
                let sel_text = self
                    .sel
                    .and_then(|i| self.disks.get(i))
                    .map(|d| d.label_bus())
                    .unwrap_or_else(|| {
                        if self.disks.is_empty() {
                            "No removable drives found; plug one in".into()
                        } else {
                            "Select a device…".into()
                        }
                    });
                ui.add_enabled_ui(!busy, |ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .add(Button::new(RichText::new("🔄").size(16.0)))
                            .on_hover_text("Refresh device list (also refreshes when the window regains focus)")
                            .clicked()
                        {
                            self.refresh_disks();
                        }
                        ComboBox::from_id_salt("device")
                            .width(ui.available_width())
                            .selected_text(sel_text)
                            .show_ui(ui, |ui| {
                                for (i, d) in self.disks.iter().enumerate() {
                                    ui.selectable_value(&mut self.sel, Some(i), d.label_bus());
                                }
                            });
                    });
                });
            });
            let changed = ui
                .add_enabled(
                    !busy,
                    egui::Checkbox::new(
                        &mut self.show_all,
                        RichText::new("Show all drives").size(12.0),
                    ),
                )
                .on_hover_text(
                    "Also list internal and large disks. Writing to the wrong one \
                     destroys its data; be careful.",
                )
                .changed();
            if changed {
                self.refresh_disks();
            }
            if self.show_all {
                ui.label(
                    RichText::new("⚠ Internal disks are listed; double-check your selection")
                        .color(ui.visuals().warn_fg_color)
                        .size(12.0),
                );
            }
        });
    }

    fn options_row(&mut self, ui: &mut egui::Ui, busy: bool) {
        ui.add_enabled_ui(!busy, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.checkbox(&mut self.verify, "Verify after write");
                ui.checkbox(&mut self.compress, "Compress backup");
                ui.checkbox(&mut self.force, "Force write").on_hover_text(
                    "Write every block without comparing first. Faster on a \
                         blank or fully different drive; slower when reflashing a \
                         mostly identical image.",
                );
                ui.checkbox(&mut self.eject, "Eject when done")
                    .on_hover_text("Eject the media after a successful write or backup");
            });
            ComboBox::from_id_salt("buf")
                .selected_text(format!("Buffer: {} MiB", BUF_MIB[self.buf_idx]))
                .width(140.0)
                .show_ui(ui, |ui| {
                    for (i, m) in BUF_MIB.iter().enumerate() {
                        ui.selectable_value(&mut self.buf_idx, i, format!("{m} MiB"));
                    }
                });
        });
    }

    fn progress_card(&mut self, ui: &mut egui::Ui, prog: &Progress) {
        Self::card(ui, |ui| {
            let stage = match prog.stage {
                Stage::Starting => "Preparing",
                Stage::Writing => "Writing",
                Stage::Verifying => "Verifying",
                Stage::Reading => "Reading",
                _ => "",
            };
            ui.horizontal(|ui| {
                ui.label(RichText::new(stage).strong().size(15.0));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("Cancel").clicked() {
                        self.cancel.store(true, Ordering::Relaxed);
                    }
                });
            });
            let bar = match prog.fraction() {
                Some(f) => ProgressBar::new(f).show_percentage(),
                None => ProgressBar::new(1.0).animate(true),
            };
            let fill = match prog.stage {
                Stage::Verifying => success(ui.visuals().dark_mode),
                _ => accent(ui.visuals().dark_mode),
            };
            ui.add(
                bar.desired_height(20.0)
                    .corner_radius(CornerRadius::same(6))
                    .fill(fill),
            );
            let line = if prog.stage == Stage::Starting {
                prog.message.clone()
            } else {
                status_line(prog)
            };
            ui.label(RichText::new(line).weak().size(12.5));
        });
    }

    fn result_banner(ui: &mut egui::Ui, prog: &Progress) {
        let dark = ui.visuals().dark_mode;
        let (color, icon) = match prog.stage {
            Stage::Done => (success(dark), "✔"),
            Stage::Error => (danger(dark), "✖"),
            _ => (ui.visuals().weak_text_color(), "•"),
        };
        Frame::group(ui.style())
            .corner_radius(CornerRadius::same(9))
            .inner_margin(12.0)
            .stroke(Stroke::new(1.0_f32, color.gamma_multiply(0.6)))
            .fill(color.gamma_multiply(if dark { 0.10 } else { 0.06 }))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                if prog.stage == Stage::Error {
                    ui.add(
                        ProgressBar::new(prog.fraction().unwrap_or(1.0))
                            .desired_height(20.0)
                            .corner_radius(CornerRadius::same(6))
                            .fill(color),
                    );
                }
                ui.horizontal(|ui| {
                    ui.label(RichText::new(icon).color(color).size(16.0));
                    ui.add(Label::new(RichText::new(&prog.message).color(color).size(13.5)).wrap());
                });
            });
    }

    fn confirm_modal(&mut self, ctx: &egui::Context) {
        let disk_label = self
            .sel
            .and_then(|i| self.disks.get(i))
            .map(|d| d.label_bus())
            .unwrap_or_default();
        let file_name = self
            .image
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let write_size = match (&self.bmap, &self.image_info) {
            (Some(_), _) => self.bmap_note.clone().unwrap_or_default(),
            (None, Some((_, comp, uncomp))) => {
                format!("writes {}", human_size(uncomp.unwrap_or(*comp)))
            }
            _ => String::new(),
        };
        let modal = egui::Modal::new(egui::Id::new("confirm_write")).show(ctx, |ui| {
            ui.set_max_width(400.0);
            ui.heading("⚠ Erase and write?");
            ui.add_space(8.0);
            egui::Grid::new("confirm_grid")
                .num_columns(2)
                .spacing([12.0, 4.0])
                .show(ui, |ui| {
                    ui.label(RichText::new("Image").weak());
                    ui.label(file_name);
                    ui.end_row();
                    if !write_size.is_empty() {
                        ui.label(RichText::new("Size").weak());
                        ui.label(write_size);
                        ui.end_row();
                    }
                    ui.label(RichText::new("Device").weak());
                    ui.label(RichText::new(disk_label).strong());
                    ui.end_row();
                });
            ui.add_space(8.0);
            ui.label(
                RichText::new("All data on this device will be permanently destroyed.")
                    .color(danger(ui.visuals().dark_mode))
                    .strong(),
            );
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    self.confirm_write = false;
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let erase = Button::new(
                        RichText::new("Erase and write")
                            .color(Color32::WHITE)
                            .strong(),
                    )
                    .fill(danger(ui.visuals().dark_mode));
                    if ui.add(erase).clicked() {
                        self.confirm_write = false;
                        self.start_write(ctx);
                    }
                });
            });
        });
        if modal.should_close() {
            self.confirm_write = false;
        }
    }
}

// foo.wic.xz -> foo.wic.bmap, foo.wic -> foo.wic.bmap
fn auto_bmap(image: &std::path::Path) -> Option<PathBuf> {
    let mut base = image.to_path_buf();
    if let Some(ext) = base.extension().map(|e| e.to_string_lossy().to_lowercase())
        && ["gz", "xz", "zst", "bz2", "bzip2"].contains(&ext.as_str())
    {
        base.set_extension("");
    }
    let cand = PathBuf::from(format!("{}.bmap", base.display()));
    cand.is_file().then_some(cand)
}

fn fmt_eta(secs: f64) -> String {
    if !secs.is_finite() || secs < 0.0 {
        return "--:--".into();
    }
    let s = secs as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

fn status_line(p: &Progress) -> String {
    let speed = if p.speed > 0.0 {
        format!("  ·  {}/s", human_size(p.speed as u64))
    } else {
        String::new()
    };
    match p.total {
        Some(t) if t > 0 => {
            let eta = if p.speed > 0.0 {
                format!(
                    "  ·  {} left",
                    fmt_eta((t.saturating_sub(p.done)) as f64 / p.speed)
                )
            } else {
                String::new()
            };
            format!("{} / {}{speed}{eta}", human_size(p.done), human_size(t))
        }
        _ => format!("{} so far{speed}", human_size(p.done)),
    }
}

impl eframe::App for App {
    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = root.ctx().clone();
        let ctx = &ctx;
        let dropped: Option<PathBuf> =
            ctx.input(|i| i.raw.dropped_files.iter().find_map(|f| f.path.clone()));
        let drag_hover = ctx.input(|i| !i.raw.hovered_files.is_empty());
        if let Some(p) = dropped
            && !self.running
        {
            self.set_image(p);
        }

        // a freshly plugged stick shows up when the user tabs back
        let refocused = ctx.input(|i| {
            i.events
                .iter()
                .any(|e| matches!(e, egui::Event::WindowFocused(true)))
        });
        if refocused && !self.running {
            self.refresh_disks();
        }

        let prog = self.progress.lock().unwrap().clone();
        if self.running && prog.stage.is_terminal() {
            self.running = false;
        }
        if self.running {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        let busy = self.running;

        Frame::default().inner_margin(18.0).show(root, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("BuildCast").size(22.0).strong());
                        ui.label(
                            RichText::new(concat!("v", env!("CARGO_PKG_VERSION")))
                                .weak()
                                .size(11.0),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            egui::global_theme_preference_switch(ui);
                        });
                    });
                    ui.label(RichText::new("Write disk images to USB drives").weak());

                    Self::section(ui, "Image");
                    self.image_card(ui, busy, drag_hover);

                    Self::section(ui, "Device");
                    self.device_card(ui, busy);

                    self.options_row(ui, busy);
                    ui.add_space(4.0);

                    let can_write = !busy && self.image.is_some() && self.sel.is_some();
                    let can_read = !busy && self.sel.is_some();
                    let half = (ui.available_width() - 10.0) / 2.0;
                    ui.horizontal(|ui| {
                        let write = Button::new(
                            RichText::new("⬇  Write to device")
                                .color(Color32::WHITE)
                                .strong(),
                        )
                        .fill(accent(ui.visuals().dark_mode))
                        .min_size(egui::vec2(half, 40.0));
                        if ui.add_enabled(can_write, write).clicked() {
                            self.confirm_write = true;
                        }
                        let read = Button::new(RichText::new("⬆  Read backup").strong())
                            .min_size(egui::vec2(half, 40.0));
                        if ui.add_enabled(can_read, read).clicked() {
                            self.start_read(ctx);
                        }
                    });
                    ui.add_space(6.0);

                    if busy {
                        self.progress_card(ui, &prog);
                    } else if prog.stage.is_terminal() {
                        Self::result_banner(ui, &prog);
                    }
                });
        });

        if self.confirm_write {
            self.confirm_modal(ctx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use buildcast_core::worker::{Progress, Stage};

    #[test]
    fn eta_formats() {
        assert_eq!(fmt_eta(f64::NAN), "--:--");
        assert_eq!(fmt_eta(-1.0), "--:--");
        assert_eq!(fmt_eta(f64::INFINITY), "--:--");
        assert_eq!(fmt_eta(0.0), "0:00");
        assert_eq!(fmt_eta(59.0), "0:59");
        assert_eq!(fmt_eta(75.0), "1:15");
        assert_eq!(fmt_eta(3599.0), "59:59");
        assert_eq!(fmt_eta(3661.0), "1:01:01");
    }

    #[test]
    fn status_lines() {
        let mut p = Progress::idle();
        p.stage = Stage::Writing;
        p.done = 512 * 1024 * 1024;
        p.total = Some(1024 * 1024 * 1024);
        p.speed = (32 * 1024 * 1024) as f64;
        let s = status_line(&p);
        assert!(s.contains("512.0 MiB / 1.0 GiB"));
        assert!(s.contains("32.0 MiB/s"));
        assert!(s.contains("left"));

        p.speed = 0.0;
        let s = status_line(&p);
        assert!(!s.contains("/s"));
        assert!(!s.contains("left"));

        p.total = None;
        p.done = 10 * 1024 * 1024;
        assert!(status_line(&p).contains("10.0 MiB so far"));
    }

    #[test]
    fn theme_colors_differ_per_mode() {
        assert_ne!(accent(true), accent(false));
        assert_ne!(danger(true), danger(false));
        assert_ne!(success(true), success(false));
    }
}
