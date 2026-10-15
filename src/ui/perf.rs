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
                format!("coluna_{c}")
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
                    1 => Some(format!("valor {r} da coluna {c} com algum texto")),
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

    report("base: tabela de 40 linhas", harness.time(30));

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
    report("editor: resultado 5000 x 20", harness.time(30));

    // 300 pending edits on it: every frame used to copy the result to show them.
    {
        let tab = &mut harness.state_mut().tabs[1];
        let ed = tab.editor_mut().unwrap();
        let src = ed.source.as_mut().unwrap();
        for r in 0..300 {
            src.changes.push(Change::Set {
                key: vec![res.rows[r * 10][0].clone()],
                col: "coluna_1".into(),
                value: Some(format!("novo {r}")),
                before: None,
            });
        }
    }
    report("editor: + 300 alterações pendentes", harness.time(30));

    // Everything selected (Ctrl+A): sum / average under the grid.
    {
        let tab = &mut harness.state_mut().tabs[1];
        tab.editor_mut().unwrap().source.as_mut().unwrap().changes.clear();
        tab.anchor = Some((0, 0));
        tab.selected = Some((4999, 19));
    }
    report("editor: seleção de 100 mil células", harness.time(30));
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
            now.rows[r][1] = Some(format!("mudou {r}"));
        }
        now.elapsed = Duration::from_millis(1);
        tab.result = Some(now);
    }
    report("editor: comparando 5000 x 20 com o fixado", harness.time(30));
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
                format!("SELECT id, nome, 'texto {i}' FROM clientes WHERE id = {i} AND ativo; -- linha {i}\n")
            })
            .collect();
        *tab.editor_mut().unwrap() = Editor::new(text);
    }
    report("editor: script de 5000 linhas", harness.time(30));

    // Large catalog: 3000 tables, 40 000 columns, sidebar filtered, palette open.
    {
        let app = harness.state_mut();
        let s = app.session.as_mut().unwrap();
        s.tables = (0..3000)
            .map(|i| TableRef {
                schema: format!("schema_{}", i / 300),
                name: format!("tabela_{i}"),
                is_view: false,
                estimate: Some(i),
                search_key: format!("tabela_{i}"),
                size: Some(i * 1000),
            })
            .collect();
        s.columns = (0..40_000)
            .map(|i| (format!("tabela_{}", i / 13), format!("coluna_{i}")))
            .collect();
        app.tabs.truncate(1);
        app.active = 0;
    }
    report("sidebar: 3000 tabelas", harness.time(30));
    harness.state_mut().filter = "tabela_1".into();
    report("sidebar: 3000 tabelas filtradas", harness.time(30));
    harness.state_mut().filter.clear();
    harness.state_mut().palette = Some(Palette {
        query: "col".into(),
        selected: 0,
        started: false,
    });
    report(
        "Ctrl+P: 3000 tabelas + 40 mil colunas, buscando",
        harness.time(30),
    );
    harness.state_mut().palette = None;
}
