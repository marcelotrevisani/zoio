//! zoio — cross-platform process resource monitor.
//!
//! Launches the egui-based UI. All real logic lives in the `zoio` library
//! crate so it can be unit- and integration-tested headlessly.

use eframe::egui;
use zoio::app::ZoioApp;
use zoio::process_monitor::MonitorConfig;

fn main() -> eframe::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1200.0, 800.0])
            .with_min_inner_size([800.0, 600.0])
            .with_title("zoio — process monitor"),
        ..Default::default()
    };

    eframe::run_native(
        "zoio",
        native_options,
        Box::new(|_cc| Ok(Box::new(ZoioApp::new(MonitorConfig::default())))),
    )
}
