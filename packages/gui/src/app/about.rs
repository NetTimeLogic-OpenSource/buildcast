use egui::{Label, RichText, ScrollArea};

use super::App;
use crate::changelog::Release;
use crate::sbom::{self, Component, Notice};

const LICENSE: &str = include_str!("../../../../LICENSE");
const REPO_URL: &str = "https://github.com/NetTimeLogic-OpenSource/buildcast";

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum AboutTab {
    #[default]
    About,
    Changelog,
    ThirdParty,
}

impl App {
    pub(super) fn about_modal(&mut self, ctx: &egui::Context) {
        let Some(mut tab) = self.about else { return };
        let mut close = false;
        let width = modal_width(ctx, 520.0);
        let modal = egui::Modal::new(egui::Id::unique("about")).show(ctx, |ui| {
            ui.set_width(width);
            ui.set_max_width(width);
            ui.horizontal(|ui| {
                ui.selectable_value(&mut tab, AboutTab::About, "About");
                ui.selectable_value(&mut tab, AboutTab::Changelog, "Changelog");
                ui.selectable_value(&mut tab, AboutTab::ThirdParty, "Third-party software");
            });
            ui.separator();
            ScrollArea::vertical()
                .max_height(380.0)
                .auto_shrink([false, true])
                .show(ui, |ui| match tab {
                    AboutTab::About => about_tab(ui),
                    AboutTab::Changelog => {
                        for release in Release::all() {
                            release.show(ui);
                        }
                    }
                    AboutTab::ThirdParty => third_party_tab(ui),
                });
            ui.separator();
            egui::Sides::new().show(
                ui,
                |ui| {
                    if tab == AboutTab::ThirdParty && ui.button("Save SBOM…").clicked() {
                        save_sbom();
                    }
                },
                |ui| close = ui.button("Close").clicked(),
            );
        });
        self.about = (!close && !modal.should_close()).then_some(tab);
    }
}

/// `preferred`, narrowed to fit the window with a margin.
pub(super) fn modal_width(ctx: &egui::Context, preferred: f32) -> f32 {
    preferred.min(ctx.content_rect().width() - 64.0)
}

fn about_tab(ui: &mut egui::Ui) {
    ui.label(RichText::new("BuildCast").size(20.0).strong());
    ui.label(concat!("Version ", env!("CARGO_PKG_VERSION")));
    ui.label(env!("CARGO_PKG_DESCRIPTION"));
    ui.add_space(6.0);
    ui.hyperlink_to("Source code", REPO_URL);
    ui.hyperlink_to("Releases", crate::update::RELEASES_PAGE);
    ui.add_space(6.0);
    ui.label(RichText::new("License").strong());
    ui.add(Label::new(RichText::new(LICENSE).monospace().size(11.0)).wrap());
}

fn save_sbom() {
    if let Some(path) = rfd::FileDialog::new()
        .set_file_name(concat!(
            "buildcast-",
            env!("CARGO_PKG_VERSION"),
            ".cdx.json"
        ))
        .add_filter("CycloneDX SBOM", &["json"])
        .save_file()
        && let Err(err) = sbom::save(&path)
    {
        rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Error)
            .set_title("Cannot save the SBOM")
            .set_description(err.to_string())
            .show();
    }
}

fn third_party_tab(ui: &mut egui::Ui) {
    ui.label(
        "BuildCast includes the following open source packages. Their licenses are listed below.",
    );
    ui.add_space(6.0);
    egui::Grid::new("components")
        .num_columns(3)
        .striped(true)
        .spacing([12.0, 2.0])
        .show(ui, |ui| {
            for c in Component::all() {
                ui.label(&c.name);
                ui.label(RichText::new(&c.version).weak());
                ui.add(Label::new(RichText::new(&c.license).weak()).truncate());
                ui.end_row();
            }
        });
    ui.add_space(8.0);
    ui.label(RichText::new("License texts").strong());
    for (i, notice) in Notice::all().iter().enumerate() {
        egui::CollapsingHeader::new(&notice.title)
            .id_salt(("notice", i))
            .show(ui, |ui| {
                ui.add(
                    Label::new(RichText::new(format!("Used by {}", notice.packages)).weak()).wrap(),
                );
                ui.add(Label::new(RichText::new(&notice.text).monospace().size(11.0)).wrap());
            });
    }
}
