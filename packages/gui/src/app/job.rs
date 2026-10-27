use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use buildcast_core::disks::{DiskInfo, human_size};
use buildcast_core::worker::{BufferSize, Job, Progress, Stage};
use egui::{
    Align, Button, Color32, ComboBox, CornerRadius, Frame, Label, Layout, ProgressBar, RichText,
    Stroke, ViewportCommand,
};

use super::{App, file_name};
use crate::card;
use crate::theme::Palette;
use crate::update::UpdateState;

// success and cancel banners fade out after this long unless hovered
const BANNER_TTL: Duration = Duration::from_secs(6);

impl App {
    pub(super) fn action_buttons(&mut self, ui: &mut egui::Ui) {
        // an update download ends in process::exit, don't start jobs under it
        let updating = matches!(self.updater.state(), UpdateState::Downloading);
        let idle = !self.running && !updating;
        let has_disk = self.selected_disk().is_some();
        let bmap_ok = self.bmap.as_ref().is_none_or(|b| b.is_valid());
        let write_hint = if self.image.is_none() || !has_disk {
            "Select an image and a device first"
        } else if !bmap_ok {
            "Remove the invalid bmap first"
        } else {
            "Wait for the running job"
        };
        let half = (ui.available_width() - 10.0) / 2.0;
        ui.horizontal(|ui| {
            let write = Button::new(
                RichText::new("⬇  Write to device")
                    .color(Color32::WHITE)
                    .strong(),
            )
            .fill(Palette::of(ui).accent)
            .min_size(egui::vec2(half, 40.0));
            if ui
                .add_enabled(idle && has_disk && self.image.is_some() && bmap_ok, write)
                .on_disabled_hover_text(write_hint)
                .clicked()
            {
                self.confirm_write = true;
            }
            let read = Button::new(RichText::new("⬆  Read backup").strong())
                .min_size(egui::vec2(half, 40.0));
            if ui
                .add_enabled(idle && has_disk, read)
                .on_disabled_hover_text(if has_disk {
                    "Wait for the running job"
                } else {
                    "Select a device first"
                })
                .clicked()
            {
                self.start_read(ui.ctx());
            }
        });
    }

    pub(super) fn confirm_close_modal(&mut self, ctx: &egui::Context) {
        let modal = egui::Modal::new(egui::Id::unique("confirm_close")).show(ctx, |ui| {
            ui.set_max_width(400.0);
            ui.heading("Stop the running job?");
            ui.add_space(8.0);
            ui.label(
                "BuildCast stops the job before it quits. A stopped write leaves the device \
                 contents undefined.",
            );
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.button("Keep running").clicked() {
                    self.confirm_close = false;
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let stop = Button::new(
                        RichText::new("Stop and quit")
                            .color(Color32::WHITE)
                            .strong(),
                    )
                    .fill(Palette::of(ui).danger);
                    if ui.add(stop).clicked() {
                        self.confirm_close = false;
                        self.quit_after_job = true;
                        self.request_cancel();
                    }
                });
            });
        });
        if modal.should_close() {
            self.confirm_close = false;
        }
    }

    pub(super) fn confirm_write_modal(&mut self, ctx: &egui::Context) {
        let disk_label = self
            .selected_disk()
            .map(DiskInfo::label_bus)
            .unwrap_or_default();
        let image_name = self
            .image
            .as_ref()
            .map(|i| file_name(&i.path))
            .unwrap_or_default();
        let write_size = match (&self.bmap, &self.image) {
            (Some(b), _) => Some(b.summary()),
            (None, Some(i)) => i.write_size().map(|n| format!("writes {}", human_size(n))),
            (None, None) => None,
        };
        let modal = egui::Modal::new(egui::Id::unique("confirm_write")).show(ctx, |ui| {
            ui.set_max_width(400.0);
            ui.heading("⚠ Erase and write?");
            ui.add_space(8.0);
            egui::Grid::new("confirm_grid")
                .num_columns(2)
                .spacing([12.0, 4.0])
                .show(ui, |ui| {
                    ui.label(RichText::new("Image").weak());
                    ui.label(image_name);
                    ui.end_row();
                    if let Some(size) = write_size {
                        ui.label(RichText::new("Size").weak());
                        ui.label(size);
                        ui.end_row();
                    }
                    ui.label(RichText::new("Device").weak());
                    ui.label(RichText::new(disk_label).strong());
                    ui.end_row();
                });
            ui.add_space(8.0);
            ui.label(
                RichText::new("All data on this device will be permanently destroyed.")
                    .color(Palette::of(ui).danger)
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
                    .fill(Palette::of(ui).danger);
                    if ui.add(erase).clicked() {
                        self.confirm_write = false;
                        self.start_write(ui.ctx());
                    }
                });
            });
        });
        if modal.should_close() {
            self.confirm_write = false;
        }
    }

    pub(super) fn options_row(&mut self, ui: &mut egui::Ui, busy: bool) {
        ui.add_enabled_ui(!busy, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.checkbox(&mut self.verify, "Verify after write")
                    .on_hover_text("Read the device back and compare it with the image");
                ui.checkbox(&mut self.compress, "Compress backup")
                    .on_hover_text("Save backups as zstd-compressed .dd.zst files");
                ui.checkbox(&mut self.force, "Force write").on_hover_text(
                    "Write every block without comparing first. Faster on a blank or fully \
                     different drive, slower when reflashing a mostly identical image.",
                );
                ui.checkbox(&mut self.eject, "Eject when done")
                    .on_hover_text("Eject the media after a successful write or backup");
            });
            ComboBox::from_id_salt("buf")
                .selected_text(format!("Buffer: {} MiB", self.buffer.mib()))
                .width(140.0)
                .show_ui(ui, |ui| {
                    for b in BufferSize::CHOICES {
                        ui.selectable_value(&mut self.buffer, b, format!("{} MiB", b.mib()));
                    }
                });
        });
    }

    pub(super) fn progress_card(&mut self, ui: &mut egui::Ui, prog: &Progress) {
        card::show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(prog.stage.label()).strong().size(15.0));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let label = if self.cancelling {
                        "Cancelling…"
                    } else {
                        "Cancel"
                    };
                    if ui
                        .add_enabled(!self.cancelling, Button::new(label))
                        .clicked()
                    {
                        self.request_cancel();
                    }
                });
            });
            let bar = match prog.fraction() {
                Some(f) => ProgressBar::new(f).show_percentage(),
                None => ProgressBar::new(1.0).animate(true),
            };
            let palette = Palette::of(ui);
            let fill = if prog.stage == Stage::Verifying {
                palette.success
            } else {
                palette.accent
            };
            ui.add(
                bar.desired_height(20.0)
                    .corner_radius(CornerRadius::same(6))
                    .fill(fill),
            );
            let line = if prog.stage == Stage::Starting {
                prog.message.clone()
            } else {
                prog.status_line()
            };
            ui.label(RichText::new(line).weak().size(12.5));
        });
    }

    // errors stay until the next job, other results fade out
    pub(super) fn result_banner(&mut self, ui: &mut egui::Ui, prog: &Progress) {
        let dark = ui.visuals().dark_mode;
        let palette = Palette::of(ui);
        let (color, icon) = match prog.stage {
            Stage::Done => (palette.success, "✔"),
            Stage::Error => (palette.danger, "✖"),
            _ => (ui.visuals().weak_text_color(), "•"),
        };
        let resp = Frame::group(ui.style())
            .corner_radius(CornerRadius::same(10))
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
        if prog.stage != Stage::Error
            && let Some(since) = self.result_since
        {
            let ctx = ui.ctx();
            if ui.rect_contains_pointer(resp.response.rect) {
                self.result_since = Some(Instant::now());
                ctx.request_repaint_after(BANNER_TTL);
            } else if since.elapsed() >= BANNER_TTL {
                self.result_since = None;
                ctx.request_repaint();
            } else {
                ctx.request_repaint_after(BANNER_TTL - since.elapsed());
            }
        }
    }

    /// Notices the end of a job and keeps the window title current.
    pub(super) fn track_job(&mut self, ctx: &egui::Context, prog: &Progress) {
        if self.running && prog.stage.is_terminal() {
            self.running = false;
            self.cancelling = false;
            self.result_since = Some(Instant::now());
            #[cfg(windows)]
            {
                self.remote = None;
            }
            if self.quit_after_job {
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
            // an ejected drive is gone, a written one may show new partitions
            self.refresh_disks();
        }
        if self.running {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        let title = match (self.running, prog.fraction()) {
            (true, Some(f)) => format!("{} {:.0}% - BuildCast", prog.stage.label(), f * 100.0),
            (true, None) => format!("{} - BuildCast", prog.stage.label()),
            (false, _) => "BuildCast".into(),
        };
        if title != self.title {
            ctx.send_viewport_cmd(ViewportCommand::Title(title.clone()));
            self.title = title;
        }
    }

    fn request_cancel(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        self.cancelling = true;
        #[cfg(windows)]
        if let Some(r) = &self.remote {
            r.cancel();
        }
    }

    fn start_read(&mut self, ctx: &egui::Context) {
        let Some(disk) = self.selected_disk().cloned() else {
            return;
        };
        let stamp = chrono::Local::now().format("%Y%m%dT%H%M%S");
        let ext = if self.compress { "dd.zst" } else { "dd" };
        let dir = dirs::desktop_dir()
            .or_else(dirs::download_dir)
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("."));
        let Some(output) = rfd::FileDialog::new()
            .set_directory(dir)
            .set_file_name(format!("buildcast-{stamp}.{ext}"))
            .add_filter("Disk image backup", &[ext])
            .save_file()
        else {
            return;
        };
        self.start(
            ctx,
            disk,
            Job::Read {
                compress: self.compress,
                output,
            },
        );
    }

    fn start_write(&mut self, ctx: &egui::Context) {
        let (Some(image), Some(disk)) = (&self.image, self.selected_disk().cloned()) else {
            return;
        };
        let job = Job::Write {
            bmap: self
                .bmap
                .as_ref()
                .filter(|b| b.is_valid())
                .map(|b| b.path.clone()),
            compare: !self.force,
            image: image.path.clone(),
            verify: self.verify,
        };
        self.start(ctx, disk, job);
    }

    // in-process job thread on Linux and macOS
    #[cfg(not(windows))]
    fn start(&mut self, ctx: &egui::Context, disk: DiskInfo, job: Job) {
        // mirrors the CLI --force gate: a stale selection must never reach a
        // non-removable drive after "Show all drives" was turned off
        if !disk.removable && !self.show_all {
            let mut p = self.progress.lock().unwrap();
            p.stage = Stage::Error;
            p.message = format!("{} is not removable. Turn on \"Show all drives\".", disk.id);
            self.result_since = Some(Instant::now());
            return;
        }
        self.begin();
        let opts = buildcast_core::worker::JobOpts {
            buffer: self.buffer,
            eject: self.eject,
        };
        job.spawn(
            disk,
            opts,
            self.progress.clone(),
            self.cancel.clone(),
            super::notify(ctx),
        );
    }

    // elevated CLI, one UAC prompt per job
    #[cfg(windows)]
    fn start(&mut self, ctx: &egui::Context, disk: DiskInfo, job: Job) {
        let mut args = match job {
            Job::Read { compress, output } => {
                let mut args = vec!["read".into(), output.display().to_string()];
                if compress {
                    args.push("--compress".into());
                }
                args
            }
            Job::Write {
                bmap,
                compare,
                image,
                verify,
            } => {
                let mut args = vec!["copy".into(), image.display().to_string()];
                match bmap {
                    Some(b) => args.extend(["--bmap".into(), b.display().to_string()]),
                    None => args.push("--raw".into()),
                }
                if !verify {
                    args.push("--no-verify".into());
                }
                if !compare {
                    args.push("--skip-compare".into());
                }
                args
            }
        };
        args.extend([
            "--device".into(),
            disk.id,
            "--yes".into(),
            "--buffer-mib".into(),
            self.buffer.mib().to_string(),
        ]);
        if self.eject {
            args.push("--eject".into());
        }
        if self.show_all {
            args.push("--force".into());
        }
        self.begin();
        match crate::remote::RemoteJob::spawn(args, self.progress.clone(), super::notify(ctx)) {
            Ok(job) => self.remote = Some(job),
            Err(err) => {
                let mut p = self.progress.lock().unwrap();
                p.stage = Stage::Error;
                p.message = buildcast_core::Report(&err).to_string();
            }
        }
    }

    fn begin(&mut self) {
        self.cancel.store(false, Ordering::Relaxed);
        self.cancelling = false;
        *self.progress.lock().unwrap() = Progress {
            message: "Starting…".into(),
            stage: Stage::Starting,
            ..Progress::idle()
        };
        self.running = true;
    }
}
