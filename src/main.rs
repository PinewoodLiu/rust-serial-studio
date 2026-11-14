#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod layout;
fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("串口助手 · Rust Serial Studio")
            .with_inner_size([1240.0, 820.0])
            .with_min_inner_size([1050.0, 700.0]),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    eframe::run_native(
        "Rust Serial Assistant",
        options,
        Box::new(|cc| Ok(Box::new(app::SerialApp::new(cc)))),
    )
}
