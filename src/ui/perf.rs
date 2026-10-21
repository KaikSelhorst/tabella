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
    /// Where the pointer rests, with Ctrl held (hovering a name in the editor).
    ctrl_hover: Option<egui::Pos2>,
}

impl Driver {
    fn new() -> Self {
        let ctx = egui::Context::default();
        super::theme::apply(&ctx);
        let app = fake_app(&ctx);
        Driver {
            ctx,
            app,
            ctrl_hover: None,
        }
    }

    fn frame(&mut self) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1600.0, 900.0),
            )),
            events: self
                .ctrl_hover
                .map(|p| {
                    vec![
                        egui::Event::ModifiersChanged(egui::Modifiers::COMMAND),
                        egui::Event::PointerMoved(p),
                    ]
                })
                .unwrap_or_default(),
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
            enum_values: Vec::new(),
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
#[ignore = "a benchmark: renders with wgpu and prints frame times"]
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
        let mut src = TableState::new("public", "large", None);
        src.cols = info;
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
                key: vec![res.rows.cell(r * 10, 0).map(str::to_owned)],
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
            now.rows.set(r, 1, Some(&format!("changed {r}")));
        }
        now.elapsed = Duration::from_millis(1);
        tab.result = Some(now);
    }
    report(
        "editor: comparing 5000 x 20 with the pinned one",
        harness.time(30),
    );
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
    report("Ctrl+P: 3000 tables + 40k columns, searching", harness.time(30));
    harness.state_mut().dialog = None;

    // Typing in a big paste (~700 KB of JSON, 5000 lines): every keystroke changes the text, so
    // nothing laid out for the previous frame can be reused.
    {
        let app = harness.state_mut();
        let text: String = (0..5000)
            .map(|i| {
                format!(
                    "  {{\"id\":{i},\"table_name\":\"sales.orders\",\"row_id\":\"{}\",\"action\":\"UPDATE\",\"changed_by\":\"svc_fulfillment\",\"old_values\":\"{{\\\"status\\\": \\\"confirmed\\\"}}\",\"changed_at\":\"2025-04-08 08:10:35+00\"}},\n",
                    i * 37
                )
            })
            .collect();
        app.open_sql(text);
        app.active = app.tabs.len() - 1;
    }
    report("editor: 700 KB paste, at rest", harness.time(10));
    let typing = {
        harness.frame();
        let start = Instant::now();
        for _ in 0..10 {
            let app = harness.state_mut();
            let i = app.active;
            app.tabs[i].editor_mut().unwrap().text.push(' ');
            harness.frame();
        }
        start.elapsed() / 10
    };
    report("editor: 700 KB paste, typing", typing);

    // Ctrl held over a name in the editor, with a big catalog: "go to definition" looks it up
    // every frame the pointer is there.
    {
        let app = harness.state_mut();
        let s = app.session.as_mut().unwrap();
        s.schema_cols = (0..40_000)
            .map(|i| crate::db::SchemaColumn {
                schema: format!("schema_{}", i / 3900),
                table: format!("table_{}", i / 13),
                column: format!("column_{}", i % 13),
                data_type: "text".into(),
                pk: false,
            })
            .collect();
        let text: String = (0..200)
            .map(|i| {
                format!(
                    "SELECT column_{} FROM table_{i} t JOIN table_{} u ON u.column_1 = t.column_2;\n",
                    i % 13,
                    i + 1
                )
            })
            .collect();
        app.open_sql(text);
        app.active = app.tabs.len() - 1;
    }
    harness.ctrl_hover = Some(egui::pos2(330.0, 140.0));
    report("editor: Ctrl+hover a name, 40k columns", harness.time(30));
    harness.ctrl_hover = None;
}

/// Contact sheets of the screenshots, for reviewing many screens at once:
/// `TABELLA_SHOTS=<dir> cargo test contact_sheets -- --ignored` writes `<dir>/sheets/NN.png`
/// (2 x 3 screens each, half size) and lists what is on each.
#[test]
#[ignore = "reads and writes PNGs under TABELLA_SHOTS"]
fn contact_sheets() {
    use image::{GenericImage, RgbaImage, imageops};
    let Ok(dir) = std::env::var("TABELLA_SHOTS") else {
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
/// `TABELLA_CROP=<in.png>:<x>,<y>,<w>,<h>:<scale>:<out.png> cargo test crop_shot -- --ignored`.
#[test]
#[ignore = "by hand: set TABELLA_CROP"]
fn crop_shot() {
    use image::imageops;
    let Ok(spec) = std::env::var("TABELLA_CROP") else {
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

/// Random typing and editing keys in the SQL editor, frame after frame (find bar, completion,
/// undo, running): nothing panics, which in the release build would close the window.
#[test]
fn editor_survives_random_keys() {
    use egui::{Event, Key, Modifiers};
    const KEYS: &[(Key, Modifiers)] = &[
        (Key::Enter, Modifiers::NONE),
        (Key::Backspace, Modifiers::NONE),
        (Key::Delete, Modifiers::NONE),
        (Key::Tab, Modifiers::NONE),
        (Key::Tab, Modifiers::SHIFT),
        (Key::ArrowLeft, Modifiers::NONE),
        (Key::ArrowRight, Modifiers::SHIFT),
        (Key::ArrowUp, Modifiers::NONE),
        (Key::ArrowDown, Modifiers::NONE),
        (Key::Home, Modifiers::NONE),
        (Key::End, Modifiers::SHIFT),
        (Key::PageUp, Modifiers::NONE),
        (Key::PageDown, Modifiers::NONE),
        (Key::Escape, Modifiers::NONE),
        (Key::Space, Modifiers::COMMAND),
        (Key::Z, Modifiers::COMMAND),
        (Key::Y, Modifiers::COMMAND),
        (Key::A, Modifiers::COMMAND),
        (Key::F, Modifiers::COMMAND),
        (Key::H, Modifiers::COMMAND),
        (Key::Enter, Modifiers::COMMAND),
        (Key::E, Modifiers::COMMAND),
        (Key::F5, Modifiers::NONE),
    ];
    let mut h = Driver::new();
    let id = {
        let app = h.state_mut();
        app.open_sql("SELECT * FROM customers c\nWHERE c.id = :id;\n".into());
        app.active = app.tabs.len() - 1;
        app.tabs[app.active].id
    };
    let start = "SELECT * FROM customers c\nWHERE c.id = :id;\n";
    let mut typed = false;
    let mut g = crate::testgen::Gen::new(15);
    for frame in 0..1000 {
        // Back in the editor whatever the last keys opened.
        h.ctx.memory_mut(|m| m.request_focus(egui::Id::new(("sql", id))));
        let events: Vec<Event> = (0..1 + g.below(3))
            .flat_map(|_| {
                if g.below(3) == 0 {
                    vec![Event::Text(g.text_of(2))]
                } else {
                    let (key, modifiers) = KEYS[g.below(KEYS.len())];
                    [true, false]
                        .map(|pressed| Event::Key {
                            key,
                            physical_key: None,
                            pressed,
                            repeat: false,
                            modifiers,
                        })
                        .to_vec()
                }
            })
            .collect();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1200.0, 800.0),
            )),
            events: events.clone(),
            ..Default::default()
        };
        let app = &mut h.app;
        let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut out = h.ctx.run_ui(input, |ui| app.draw(ui));
            out.textures_delta.clear();
        }));
        let text = h
            .app
            .tabs
            .iter()
            .find(|t| t.id == id)
            .and_then(|t| t.editor())
            .map(|e| e.text.clone());
        assert!(run.is_ok(), "frame {frame}: {events:?} on {text:?}");
        typed |= text.is_some_and(|t| t != start);
    }
    assert!(typed, "the keys reached the editor");
}

/// A closed tab leaves nothing behind in egui's memory: its laid-out text (hundreds of MB for a
/// big script) and other caches go with it, whichever way it was closed.
#[test]
fn closed_tabs_free_their_caches() {
    let mut h = Driver::new();
    h.frame();
    let id = {
        let app = h.state_mut();
        // Two lines: laid out a paragraph at a time.
        app.open_sql("SELECT 1;\nSELECT 2;".into());
        app.active = app.tabs.len() - 1;
        app.tabs[app.active].id
    };
    h.frame();
    h.frame();
    let held = |h: &Driver| {
        h.ctx.data_mut(|d| {
            (
                d.get_temp::<(egui::Id, std::sync::Arc<egui::Galley>)>(egui::Id::new(("editor_galley", id)))
                    .is_some(),
                d.get_temp::<(egui::Id, Vec<(usize, usize)>)>(egui::Id::new(("statement_ranges", id)))
                    .is_some(),
                d.get_persisted::<egui::text_edit::TextEditState>(egui::Id::new(("sql", id)))
                    .is_some(),
                d.get_temp::<std::collections::HashMap<u64, std::sync::Arc<egui::Galley>>>(egui::Id::new((
                    "editor_paragraphs",
                    id,
                )))
                .is_some(),
            )
        })
    };
    assert_eq!(
        held(&h),
        (true, true, true, true),
        "an open editor keeps its layout, statements, undo history and paragraphs"
    );
    {
        let app = h.state_mut();
        let i = app.active;
        app.tabs.remove(i);
        app.active = 0;
    }
    h.frame();
    assert_eq!(held(&h), (false, false, false, false), "gone with the tab");
}

/// Browsing rows with the detail panel open keeps one buffer per field, not one per row seen.
#[test]
fn browsing_rows_in_the_detail_panel_keeps_memory_flat() {
    let mut h = Driver::new();
    h.state_mut().active = 0;
    let mut entries = Vec::new();
    for r in 0..40 {
        h.state_mut().tabs[0].selected = Some((r, 1));
        h.frame();
        if r == 1 || r == 39 {
            entries.push(h.ctx.data(|d| d.len()));
        }
    }
    assert_eq!(entries[0], entries[1], "egui memory after 2 rows and after 40");
}
