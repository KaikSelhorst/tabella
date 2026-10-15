//! Frame times of the heaviest screens the app allows, measured the way egui runs them (the
//! whole UI rebuilt every frame) and with eframe's tessellation; a frame has 16.7 ms at 60 Hz.
//! `cargo test --release perf -- --ignored --nocapture`.

use std::time::{Duration, Instant};

use eframe::egui;

use super::screenshots::fake_app;
use crate::app::{App, Change, Editor, Palette, TableState};
use crate::db::{ColumnInfo, QueryResult, TableRef};

/// The app driven like eframe does (a frame, then tessellation), without the test harness:
/// that one turns AccessKit on, which the app only has with a screen reader and which costs a
/// node per line of text.
struct Driver {
    ctx: egui::Context,
    app: App,
}

impl Driver {
    fn new() -> Self {
        let ctx = egui::Context::default();
        super::theme::apply(&ctx);
        let app = fake_app(&ctx);
        Driver { ctx, app }
    }

    fn frame(&mut self) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1600.0, 900.0),
            )),
            ..Default::default()
        };
        let app = &mut self.app;
        let mut out = self.ctx.run_ui(input, |ui| app.draw(ui));
        // No renderer here: the font textures are let go.
        out.textures_delta.clear();
        std::hint::black_box(self.ctx.tessellate(out.shapes, out.pixels_per_point));
    }

    /// Average time of a frame over `frames` (after two to settle).
    fn time(&mut self, frames: u32) -> Duration {
        self.frame();
        self.frame();
        let start = Instant::now();
        for _ in 0..frames {
            self.frame();
        }
        start.elapsed() / frames
    }

    fn state_mut(&mut self) -> &mut App {
        &mut self.app
    }
}

fn big_result(rows: usize, cols: usize) -> (QueryResult, Vec<ColumnInfo>) {
    let columns: Vec<String> = (0..cols)
        .map(|c| {
            if c == 0 {
                "id".into()
            } else {
                format!("column_{c}")
            }
        })
        .collect();
    let info = columns
        .iter()
        .enumerate()
        .map(|(i, n)| ColumnInfo {
            name: n.clone(),
            data_type: if i % 3 == 0 {
                "numeric".into()
            } else {
                "text".into()
            },
            pk: i == 0,
            nullable: i != 0,
            has_default: false,
            references: None,
        })
        .collect();
    let rows = (0..rows)
        .map(|r| {
            (0..cols)
                .map(|c| match c % 3 {
                    0 => Some(format!("{}.{:02}", r * 7 + c, r % 100)),
                    1 => Some(format!("value {r} of column {c} with some text")),
                    _ => (r % 7 != 0).then(|| format!("x{r}")),
                })
                .collect()
        })
        .collect();
    let res = QueryResult {
        columns,
        rows,
        ..Default::default()
    };
    (res, info)
}

#[test]
#[ignore]
fn frame_times() {
    let mut harness = Driver::new();
    let report = |name: &str, d: Duration| eprintln!("{name:<58} {:>8.2} ms/frame", d.as_secs_f64() * 1000.0);

    report("base: 40-row table", harness.time(30));

    // An editor result at the cap: 5000 rows x 20 columns, editable (source with columns).
    let (res, info) = big_result(5000, 20);
    {
        let app = harness.state_mut();
        let tab = &mut app.tabs[1];
        tab.result = Some(res.clone());
        let ed = tab.editor_mut().unwrap();
        let mut src = TableState::new("public", "grande", None);
        src.cols = info.clone();
        ed.source = Some(Box::new(src));
        app.active = 1;
    }
    report("editor: 5000 x 20 result", harness.time(30));

    // 300 pending edits on it: every frame used to copy the result to show them.
    {
        let tab = &mut harness.state_mut().tabs[1];
        let ed = tab.editor_mut().unwrap();
        let src = ed.source.as_mut().unwrap();
        for r in 0..300 {
            src.changes.push(Change::Set {
                key: vec![res.rows[r * 10][0].clone()],
                col: "column_1".into(),
                value: Some(format!("new {r}")),
                before: None,
            });
        }
    }
    report("editor: + 300 pending changes", harness.time(30));

    // Everything selected (Ctrl+A): sum / average under the grid.
    {
        let tab = &mut harness.state_mut().tabs[1];
        tab.editor_mut().unwrap().source.as_mut().unwrap().changes.clear();
        tab.anchor = Some((0, 0));
        tab.selected = Some((4999, 19));
    }
    report("editor: 100k-cell selection", harness.time(30));
    {
        let tab = &mut harness.state_mut().tabs[1];
        tab.anchor = None;
        tab.selected = None;
    }

    // The 5000 x 20 result pinned, then compared with a changed copy of itself.
    {
        let tab = &mut harness.state_mut().tabs[1];
        let before = tab.result.clone().unwrap();
        let ed = tab.editor_mut().unwrap();
        ed.pin(&before, None);
        ed.comparing = true;
        ed.hide_equal = false;
        let mut now = before.clone();
        for r in (0..5000).step_by(7) {
            now.rows[r][1] = Some(format!("changed {r}"));
        }
        now.elapsed = Duration::from_millis(1);
        tab.result = Some(now);
    }
    report("editor: comparing 5000 x 20 with the pinned one", harness.time(30));
    {
        let ed = harness.state_mut().tabs[1].editor_mut().unwrap();
        ed.pinned = None;
        ed.comparing = false;
    }

    // A big script in the editor (5000 lines).
    {
        let tab = &mut harness.state_mut().tabs[1];
        tab.result = None;
        let text: String = (0..5000)
            .map(|i| {
                format!("SELECT id, name, 'text {i}' FROM customers WHERE id = {i} AND active; -- line {i}\n")
            })
            .collect();
        *tab.editor_mut().unwrap() = Editor::new(text);
    }
    report("editor: 5000-line script", harness.time(30));

    // Large catalog: 3000 tables, 40 000 columns, sidebar filtered, palette open.
    {
        let app = harness.state_mut();
        let s = app.session.as_mut().unwrap();
        s.tables = (0..3000)
            .map(|i| TableRef {
                schema: format!("schema_{}", i / 300),
                name: format!("table_{i}"),
                is_view: false,
                estimate: Some(i),
                search_key: format!("table_{i}"),
                size: Some(i * 1000),
            })
            .collect();
        s.columns = (0..40_000)
            .map(|i| (format!("table_{}", i / 13), format!("column_{i}")))
            .collect();
        app.tabs.truncate(1);
        app.active = 0;
    }
    report("sidebar: 3000 tables", harness.time(30));
    harness.state_mut().filter = "table_1".into();
    report("sidebar: 3000 tables filtered", harness.time(30));
    harness.state_mut().filter.clear();
    harness.state_mut().dialog = Some(crate::app::Dialog::Palette(Palette {
        query: "col".into(),
        selected: 0,
        started: false,
    }));
    report(
        "Ctrl+P: 3000 tables + 40k columns, searching",
        harness.time(30),
    );
    harness.state_mut().dialog = None;
}

/// Contact sheets of the screenshots, for reviewing many screens at once:
/// `DBLITE_SHOTS=<dir> cargo test contact_sheets -- --ignored` writes `<dir>/sheets/NN.png`
/// (2 x 3 screens each, half size) and lists what is on each.
#[test]
#[ignore]
fn contact_sheets() {
    use image::{GenericImage, RgbaImage, imageops};
    let Ok(dir) = std::env::var("DBLITE_SHOTS") else {
        return;
    };
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "png"))
        .collect();
    files.sort();
    let out = std::path::Path::new(&dir).join("sheets");
    std::fs::create_dir_all(&out).unwrap();
    let (w, h) = (640u32, 380u32);
    for (n, chunk) in files.chunks(6).enumerate() {
        let mut sheet = RgbaImage::from_pixel(w * 2 + 8, h * 3 + 16, image::Rgba([255, 0, 255, 255]));
        for (i, f) in chunk.iter().enumerate() {
            let img = image::open(f).unwrap().to_rgba8();
            let small = imageops::resize(&img, w, h, imageops::FilterType::Triangle);
            sheet
                .copy_from(&small, (i as u32 % 2) * (w + 8), (i as u32 / 2) * (h + 8))
                .unwrap();
        }
        sheet.save(out.join(format!("{n:02}.png"))).unwrap();
        let names: Vec<String> = chunk
            .iter()
            .map(|f| f.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        eprintln!("{n:02}: {}", names.join(" | "));
    }
}

/// A zoomed piece of a screenshot, to look at details:
/// `DBLITE_CROP=<in.png>:<x>,<y>,<w>,<h>:<scale>:<out.png> cargo test crop_shot -- --ignored`.
#[test]
#[ignore]
fn crop_shot() {
    use image::imageops;
    let Ok(spec) = std::env::var("DBLITE_CROP") else {
        return;
    };
    let parts: Vec<&str> = spec.split(':').collect();
    let [input, rect, scale, output] = parts[..] else {
        panic!("in.png:x,y,w,h:scale:out.png")
    };
    let n: Vec<u32> = rect.split(',').map(|v| v.parse().unwrap()).collect();
    let scale: u32 = scale.parse().unwrap();
    let img = image::open(input).unwrap().to_rgba8();
    let piece = imageops::crop_imm(&img, n[0], n[1], n[2], n[3]).to_image();
    let big = imageops::resize(&piece, n[2] * scale, n[3] * scale, imageops::FilterType::Nearest);
    big.save(output).unwrap();
}
