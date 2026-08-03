#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> eframe::Result {
    let icon = egui::IconData {
        rgba: include_bytes!("../assets/buildcast-64.rgba").to_vec(),
        width: 64,
        height: 64,
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([560.0, 600.0])
            .with_min_inner_size([520.0, 570.0])
            .with_title("BuildCast")
            .with_icon(icon),
        ..Default::default()
    };
    eframe::run_native(
        "BuildCast",
        options,
        Box::new(|cc| Ok(Box::new(buildcast_gui::App::new(cc)))),
    )
}
