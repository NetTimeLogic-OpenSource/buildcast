use egui::{Color32, CornerRadius, Frame, RichText, Shadow};

pub(crate) fn frame(ui: &egui::Ui) -> Frame {
    let dark = ui.visuals().dark_mode;
    Frame::group(ui.style())
        .corner_radius(CornerRadius::same(10))
        .inner_margin(14.0)
        .fill(ui.visuals().faint_bg_color)
        .shadow(Shadow {
            offset: [0, 2],
            blur: 10,
            spread: 0,
            color: Color32::from_black_alpha(if dark { 64 } else { 20 }),
        })
}

/// Full-width section heading above a card.
pub(crate) fn heading(ui: &mut egui::Ui, title: &str) {
    ui.add_space(8.0);
    ui.label(
        RichText::new(title.to_uppercase())
            .weak()
            .size(11.0)
            .strong(),
    );
}

/// Shows `add` in a full-width card.
pub(crate) fn show<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    frame(ui)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}
