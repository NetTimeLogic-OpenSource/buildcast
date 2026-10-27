use std::path::PathBuf;

use buildcast_core::bmap::{self, Bmap};
use buildcast_core::disks::human_size;
use buildcast_core::stream::{self, Format, Source};
use buildcast_core::{Error, Report};
use egui::{Align, Button, Label, Layout, RichText, Stroke};

use super::{App, file_name};
use crate::card;
use crate::theme::Palette;

const IMAGE_EXTENSIONS: [&str; 15] = [
    "img", "iso", "bin", "raw", "dd", "wic", "gz", "bz2", "xz", "zst", "zip", "tar", "tgz", "txz",
    "tbz",
];

/// Bmap picked by the user or found next to the image.
pub(super) struct BmapChoice {
    map: Result<Bmap, Error>,
    pub(super) path: PathBuf,
}

/// Selected image and what probing it found.
pub(super) struct Image {
    info: Result<ImageInfo, stream::Error>,
    pub(super) path: PathBuf,
}

struct ImageInfo {
    comp_size: u64,
    format: Format,
    uncomp_size: Option<u64>,
}

impl BmapChoice {
    pub(super) fn is_valid(&self) -> bool {
        self.map.is_ok()
    }

    fn load(path: PathBuf) -> BmapChoice {
        BmapChoice {
            map: bmap::load(&path),
            path,
        }
    }

    /// What a write with this bmap does, for display.
    pub(super) fn summary(&self) -> String {
        match &self.map {
            Ok(bm) => format!(
                "writes only {} of {}",
                human_size(bm.mapped_bytes()),
                human_size(bm.image_size())
            ),
            Err(err) => format!("invalid: {}", Report(err)),
        }
    }
}

impl Image {
    fn open(path: PathBuf) -> Image {
        let info = Source::open(&path).map(|s| ImageInfo {
            comp_size: s.comp_size,
            format: s.format,
            uncomp_size: s.uncomp_size,
        });
        Image { info, path }
    }

    /// Bytes a full write puts on the device, if known.
    pub(super) fn write_size(&self) -> Option<u64> {
        let info = self.info.as_ref().ok()?;
        Some(info.uncomp_size.unwrap_or(info.comp_size))
    }
}

impl App {
    pub(super) fn image_card(&mut self, ui: &mut egui::Ui, busy: bool) {
        let drag_hover = ui.ctx().input(|i| !i.raw.hovered_files.is_empty());
        let stroke = if drag_hover {
            Stroke::new(2.0_f32, Palette::of(ui).accent)
        } else {
            ui.visuals().widgets.noninteractive.bg_stroke
        };
        card::frame(ui).stroke(stroke).show(ui, |ui| {
            ui.set_width(ui.available_width());
            if self.image.is_some() {
                self.selected_image(ui, busy);
            } else {
                self.drop_zone(ui, busy, drag_hover);
            }
        });
    }

    pub(super) fn set_image(&mut self, path: PathBuf) {
        let sidecar = bmap::sidecar_path(&path);
        self.bmap = sidecar.is_file().then(|| BmapChoice::load(sidecar));
        self.image = Some(Image::open(path));
    }

    fn bmap_row(&mut self, ui: &mut egui::Ui, busy: bool) {
        ui.horizontal(|ui| {
            let Some(choice) = &self.bmap else {
                self.browse_bmap_button(ui, busy);
                return;
            };
            let text = format!("bmap: {}, {}", file_name(&choice.path), choice.summary());
            let color = if choice.is_valid() {
                Palette::of(ui).accent
            } else {
                ui.visuals().warn_fg_color
            };
            ui.add(Label::new(RichText::new(text).color(color).size(12.0)).wrap());
            if ui
                .add_enabled(
                    !busy,
                    Button::new(RichText::new("✖").size(11.0)).frame(false),
                )
                .on_hover_text("Don't use the bmap (write the full image)")
                .clicked()
            {
                self.bmap = None;
            }
        });
    }

    fn browse_bmap_button(&mut self, ui: &mut egui::Ui, busy: bool) {
        if ui
            .add_enabled(!busy, Button::new(RichText::new("Add bmap…").size(12.0)))
            .on_hover_text("Optional .bmap: write only mapped blocks, verify with its sha256 sums")
            .clicked()
            && let Some(p) = rfd::FileDialog::new()
                .add_filter("bmap", &["bmap"])
                .pick_file()
        {
            self.bmap = Some(BmapChoice::load(p));
        }
    }

    fn browse_image(&mut self) {
        if let Some(p) = rfd::FileDialog::new()
            .add_filter("Disk images", &IMAGE_EXTENSIONS)
            .add_filter("All files", &["*"])
            .pick_file()
        {
            self.set_image(p);
        }
    }

    fn drop_zone(&mut self, ui: &mut egui::Ui, busy: bool, drag_hover: bool) {
        ui.vertical_centered(|ui| {
            ui.add_space(8.0);
            ui.label(RichText::new("💾").size(28.0));
            if drag_hover {
                ui.label(
                    RichText::new("Release to select this image")
                        .strong()
                        .color(Palette::of(ui).accent),
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

    fn selected_image(&mut self, ui: &mut egui::Ui, busy: bool) {
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add_enabled(!busy, Button::new("✖").frame(false))
                    .on_hover_text("Clear")
                    .clicked()
                {
                    self.image = None;
                    self.bmap = None;
                    return;
                }
                if ui.add_enabled(!busy, Button::new("Browse…")).clicked() {
                    self.browse_image();
                }
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.vertical(|ui| {
                        let Some(image) = &self.image else { return };
                        ui.add(
                            Label::new(RichText::new(file_name(&image.path)).strong()).truncate(),
                        );
                        match &image.info {
                            Ok(info) => {
                                let mut s = format!(
                                    "{} · {}",
                                    info.format.name(),
                                    human_size(info.comp_size)
                                );
                                if let Some(u) = info.uncomp_size
                                    && info.format != Format::Raw
                                {
                                    s += &format!(" ({} uncompressed)", human_size(u));
                                }
                                ui.label(RichText::new(s).weak().size(12.0));
                            }
                            Err(err) => {
                                ui.label(
                                    RichText::new(format!("⚠ {}", Report(err)))
                                        .color(ui.visuals().warn_fg_color)
                                        .size(12.0),
                                );
                            }
                        }
                        ui.add(
                            Label::new(
                                RichText::new(image.path.to_string_lossy())
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
}
