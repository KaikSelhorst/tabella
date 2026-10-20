#![cfg_attr(windows, windows_subsystem = "windows")]

mod advisor;
mod app;
mod backup;
mod cli;
mod compare;
mod config;
mod db;
mod design;
mod diagram;
mod dump;
mod error;
mod export;
mod favorites;
mod files;
mod filters;
mod history;
mod import;
mod plan;
mod programs;
mod recent;
mod script;
mod search;
mod secrets;
mod session;
mod settings;
mod sql;
mod sqlite;
mod store;
mod tls;
mod tunnel;
mod ui;
mod url;
mod values;
mod widths;
mod worker;

use eframe::egui;

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        eprintln!("{}", cli::USAGE);
        return Ok(());
    }
    // A bad option is reported in the window (a Windows build has no console to print to).
    let (launch, launch_error) = match cli::parse(args) {
        Ok(l) => (l, None),
        Err(e) => (cli::Launch::default(), Some(format!("{e}\n{}", cli::USAGE))),
    };
    // Same size as when it was last closed.
    // Problems reading these files are reported by `App::new`, which reads them again.
    let size = session::load().value.window.unwrap_or([1100.0, 700.0]);
    // Preferences first: the theme is built from them.
    let prefs = settings::load().value;
    ui::theme::set_light(prefs.light);
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size(size)
        .with_title("tabella");
    // Window and taskbar icon (the .exe's own comes from build.rs).
    if let Ok(icon) = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png")) {
        viewport = viewport.with_icon(std::sync::Arc::new(icon));
    }
    let opts = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "tabella",
        opts,
        Box::new(|cc| {
            ui::theme::apply(&cc.egui_ctx);
            cc.egui_ctx.set_zoom_factor(prefs.zoom);
            let mut app = app::App::with_launch(&cc.egui_ctx, launch);
            if let Some(e) = launch_error {
                app.notice = app::Notice::error(e);
            }
            Ok(Box::new(app))
        }),
    )
}
