#![cfg_attr(windows, windows_subsystem = "windows")]

mod advisor;
mod app;
mod backup;
mod cli;
mod compare;
mod config;
mod crash;
mod db;
mod definition;
mod design;
mod diagram;
mod dialect;
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
mod starred;
mod store;
#[cfg(test)]
mod testgen;
mod tls;
mod tunnel;
mod ui;
mod url;
mod values;
mod widths;
mod worker;

use eframe::egui;

fn main() -> eframe::Result {
    crash::install();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(text) = cli::info_request(&args) {
        cli::show(&text);
        return Ok(());
    }
    // A bad option is reported in the window (a Windows build has no console to print to).
    let (launch, launch_error) = match cli::parse(args) {
        Ok(l) => (l, None),
        Err(e) => (cli::Launch::default(), Some(format!("{e}\n{}", cli::USAGE))),
    };
    // Same size as when it was last closed.
    // Problems reading these files are reported by `App::new`, which reads them again.
    let size = session::load().value.window_size();
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
            let mut app = app::App::with_launch(cc.egui_ctx.clone(), launch);
            if let Some(e) = launch_error {
                app.notice = app::Notice::error(e);
            }
            Ok(Box::new(app))
        }),
    )
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    /// `cargo itest` (what CI runs) filters by module: a module that gains tests against the
    /// docker databases has to be added to the alias, or those tests never run anywhere.
    #[test]
    fn itest_alias_covers_every_database_test() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let config = std::fs::read_to_string(root.join(".cargo/config.toml")).unwrap();
        let alias = config.lines().find(|l| l.starts_with("itest")).unwrap();
        let mut missing = Vec::new();
        let mut dirs = vec![root.join("src")];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    dirs.push(path);
                    continue;
                }
                let text = std::fs::read_to_string(&path).unwrap();
                if !text.contains("ignore = \"needs the test databases") {
                    continue;
                }
                let rel = path.strip_prefix(root.join("src")).unwrap();
                let top = rel.iter().next().unwrap().to_string_lossy().replace(".rs", "");
                if !alias.contains(&format!(" {top}::")) {
                    missing.push(rel.display().to_string());
                }
            }
        }
        assert!(
            missing.is_empty(),
            "add to the itest alias in .cargo/config.toml: {missing:?}"
        );
    }
}
