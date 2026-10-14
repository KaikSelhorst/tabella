#![cfg_attr(windows, windows_subsystem = "windows")]

mod app;
mod config;
mod db;
mod design;
mod error;
mod export;
mod favorites;
mod filters;
mod history;
mod import;
mod plan;
mod secrets;
mod session;
mod sql;
mod tls;
mod tunnel;
mod ui;
mod url;
mod values;
mod worker;

use eframe::egui;

fn main() -> eframe::Result {
    // Same size as when it was last closed.
    let size = session::load().window.unwrap_or([1100.0, 700.0]);
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size(size)
            .with_title("dblite"),
        ..Default::default()
    };
    eframe::run_native(
        "dblite",
        opts,
        Box::new(|cc| {
            ui::theme::apply(&cc.egui_ctx);
            Ok(Box::new(app::App::new(&cc.egui_ctx)))
        }),
    )
}
