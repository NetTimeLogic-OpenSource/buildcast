use egui::{Align, Layout, ScrollArea};

use super::App;

impl App {
    pub(super) fn whats_new_modal(&mut self, ctx: &egui::Context) {
        let mut close = false;
        let modal = egui::Modal::new(egui::Id::unique("whats_new")).show(ctx, |ui| {
            ui.set_width(460.0);
            ui.heading(concat!(
                "What's new in BuildCast ",
                env!("CARGO_PKG_VERSION")
            ));
            ui.separator();
            ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                for release in &self.whats_new {
                    release.show(ui);
                }
            });
            ui.separator();
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                close = ui.button("Close").clicked();
            });
        });
        if close || modal.should_close() {
            self.whats_new.clear();
        }
    }
}
