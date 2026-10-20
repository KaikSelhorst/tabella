//! F1: every keyboard shortcut and snippet on one screen.

use eframe::egui::{self, Align, Layout, RichText};

use super::complete::SNIPPETS;
use super::{icon, theme};
use crate::app::{App, Dialog};

/// Width of the key column, the same in every group so the descriptions line up.
const KEY_COL: f32 = 250.0;

const GROUPS: &[(&str, &[(&str, &str)])] = &[
    (
        "General",
        &[
            (
                "Ctrl+P",
                "Palette: tables, columns (\"table.column\"), connections, queries, actions",
            ),
            ("Ctrl+Shift+F", "Search a value in all tables"),
            ("Ctrl+Shift+H", "Full query history"),
            ("Ctrl+T", "New SQL tab"),
            (
                "Ctrl+W",
                "Close tab (right-click the tab: rename, duplicate, close others)",
            ),
            ("Ctrl+Shift+T", "Reopen the last closed tab"),
            ("Ctrl+O", "Open .sql file (or drag the file onto the window)"),
            (
                "Ctrl+P → Run .sql file",
                "Runs a large dump/migration statement by statement, with progress",
            ),
            (
                "Ctrl+P → Export database",
                "Structure and data as .sql, with progress",
            ),
            (
                "Ctrl+P → Compare structure",
                "Diff against another schema or database, with the SQL to match",
            ),
            (
                "Ctrl+Tab / Ctrl+Shift+Tab",
                "Next / previous tab (in the focused pane)",
            ),
            (
                "Ctrl+\\",
                "Split right: move the tab to the other pane (SQL tabs are copied)",
            ),
            (
                "Drag a tab",
                "Along the strip to reorder; onto the right edge of the content to split; onto the other pane's strip to move",
            ),
            (
                "Alt+←",
                "Back: to where the last foreign key was followed from (also the mouse's back button)",
            ),
            ("F1", "This screen"),
            (
                "Ctrl + / Ctrl − / Ctrl 0",
                "Interface zoom (also in Ctrl+P, along with the theme)",
            ),
        ],
    ),
    (
        "Editor SQL",
        &[
            (
                "Ctrl+Enter",
                "Run the statement under the cursor (or the selection)",
            ),
            ("F5", "Run all"),
            ("Ctrl+E", "Execution plan (EXPLAIN)"),
            (
                "Ctrl+S / Ctrl+Shift+S",
                "Save the .sql file / save as (with edits in the result, Ctrl+S saves those)",
            ),
            ("Shift+Alt+F", "Format SQL"),
            ("Ctrl+F / Ctrl+H", "Find / replace"),
            ("Tab / Enter", "Accept autocomplete suggestion"),
            (":name", "Parameter: the value is asked for before running"),
            (
                "Ctrl+click a table",
                "Opens the table (hovering shows the columns)",
            ),
        ],
    ),
    (
        "Grid",
        &[
            ("Arrows / Tab", "Move the selected cell"),
            ("Shift+arrows, Shift+click, drag", "Select several cells"),
            ("Ctrl+A", "Select all"),
            ("Ctrl+C", "Copy (tab-separated text, pastes into Excel)"),
            ("Ctrl+V", "Paste from Excel as pending changes"),
            (
                "Ctrl+D",
                "Fill down (each column gets the value of the first selected row)",
            ),
            ("Delete", "Mark the selected rows for deletion"),
            ("Enter / F2 / double-click", "Edit the cell"),
            (
                "Shift+Enter",
                "Open the value in the large editor (formatted JSON, long texts)",
            ),
            ("Ctrl+click", "Open a foreign key's row"),
            ("Ctrl+F", "Find in result (tables: searches the database)"),
            (
                "Right-click the header",
                "Filter values (like Excel), statistics, pin, hide columns, copy the values as a (…) list for IN",
            ),
            (
                "Load more (below an editor result)",
                "A result stops at the row limit (Ctrl+P → Row limit); this runs the read again for more",
            ),
        ],
    ),
    (
        "Table",
        &[
            ("Ctrl+S", "Save pending changes"),
            ("Ctrl+Z", "Undo the last pending change"),
            ("F5", "Reload"),
            ("Enter in WHERE", "Apply the filter"),
        ],
    ),
];

impl App {
    pub fn help_ui(&mut self, ctx: &egui::Context) {
        if !matches!(self.dialog, Some(Dialog::Help)) {
            return;
        }
        let t = theme::tokens();
        let mut close = false;
        let modal = egui::Modal::new(egui::Id::new("help"))
            .frame(theme::modal_frame(16))
            .show(ctx, |ui| {
                ui.set_width(640.0);
                ui.horizontal(|ui| {
                    ui.label(theme::icon(icon::KEYBOARD).size(16.0).color(t.lime));
                    ui.label(RichText::new("Shortcuts").size(15.0).color(t.foreground));
                });
                ui.add_space(8.0);
                egui::ScrollArea::vertical().max_height(520.0).show(ui, |ui| {
                    for (group, keys) in GROUPS {
                        section(ui, group);
                        egui::Grid::new(("help", *group))
                            .num_columns(2)
                            .min_col_width(KEY_COL)
                            .spacing([16.0, 4.0])
                            .show(ui, |ui| {
                                for (k, what) in *keys {
                                    key(ui, k);
                                    ui.label(RichText::new(*what).color(t.muted_foreground));
                                    ui.end_row();
                                }
                            });
                    }
                    section(ui, "Snippets (type and accept in autocomplete)");
                    egui::Grid::new("help_snippets")
                        .num_columns(2)
                        .min_col_width(KEY_COL)
                        .spacing([16.0, 4.0])
                        .show(ui, |ui| {
                            for (trigger, what, _) in SNIPPETS {
                                key(ui, trigger);
                                ui.label(RichText::new(*what).color(t.muted_foreground));
                                ui.end_row();
                            }
                        });
                });
                ui.add_space(12.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.add(theme::outline_button("Close")).clicked() {
                        close = true;
                    }
                });
            });
        if close || modal.should_close() {
            self.dialog = None;
        }
    }
}

fn section(ui: &mut egui::Ui, title: &str) {
    let t = theme::tokens();
    ui.add_space(10.0);
    ui.label(RichText::new(title.to_uppercase()).size(11.0).color(t.subtle));
    ui.add_space(4.0);
}

/// A shortcut in a key-cap box.
fn key(ui: &mut egui::Ui, k: &str) {
    let t = theme::tokens();
    egui::Frame::new()
        .fill(t.sidebar)
        .stroke(egui::Stroke::new(1.0, t.border))
        .inner_margin(egui::Margin::symmetric(6, 2))
        .show(ui, |ui| {
            ui.label(RichText::new(k).monospace().size(12.0).color(t.foreground));
        });
}
