use egui::{Color32, CornerRadius, Shadow, Stroke, Theme};

/// Semantic colors for the current light or dark theme.
#[derive(Clone, Copy)]
pub(crate) struct Palette {
    pub(crate) accent: Color32,
    pub(crate) danger: Color32,
    pub(crate) success: Color32,
}

impl Palette {
    pub(crate) fn of(ui: &egui::Ui) -> Palette {
        Palette::new(ui.visuals().dark_mode)
    }

    fn new(dark: bool) -> Palette {
        if dark {
            Palette {
                accent: Color32::from_rgb(0x5B, 0x93, 0xFF),
                danger: Color32::from_rgb(0xEF, 0x5A, 0x52),
                success: Color32::from_rgb(0x6F, 0xC5, 0x74),
            }
        } else {
            Palette {
                accent: Color32::from_rgb(0x2F, 0x6F, 0xED),
                danger: Color32::from_rgb(0xC6, 0x28, 0x28),
                success: Color32::from_rgb(0x2E, 0x7D, 0x32),
            }
        }
    }
}

/// Sets spacing, corner radii, shadows and accent colors for both themes.
pub(crate) fn apply(ctx: &egui::Context) {
    for theme in [Theme::Dark, Theme::Light] {
        let dark = theme == Theme::Dark;
        let accent = Palette::new(dark).accent;
        ctx.style_mut_of(theme, |s| {
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
                w.corner_radius = CornerRadius::same(8);
            }
            // checkbox ticks, combo highlights and focus rings follow the
            // accent
            s.visuals.selection.bg_fill = accent.gamma_multiply(if dark { 0.5 } else { 0.35 });
            s.visuals.selection.stroke = Stroke::new(1.0_f32, accent);
            s.visuals.hyperlink_color = accent;
            s.visuals.slider_trailing_fill = true;
            s.visuals.window_corner_radius = CornerRadius::same(12);
            s.visuals.window_shadow = Shadow {
                offset: [0, 6],
                blur: 24,
                spread: 0,
                color: Color32::from_black_alpha(if dark { 96 } else { 40 }),
            };
            s.visuals.popup_shadow = Shadow {
                offset: [0, 3],
                blur: 12,
                spread: 0,
                color: Color32::from_black_alpha(if dark { 80 } else { 28 }),
            };
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_differ_per_theme() {
        let (dark, light) = (Palette::new(true), Palette::new(false));
        assert_ne!(dark.accent, light.accent);
        assert_ne!(dark.danger, light.danger);
        assert_ne!(dark.success, light.success);
    }
}
