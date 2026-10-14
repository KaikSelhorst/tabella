#![cfg_attr(windows, windows_subsystem = "windows")]

mod app;
mod config;
mod db;
mod error;
mod export;
mod history;
mod secrets;
mod sql;
mod tls;
mod tunnel;
mod ui;
mod values;
mod worker;

use eframe::egui;

fn main() -> eframe::Result {
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 700.0])
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
