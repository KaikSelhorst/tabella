//! ER diagram of a schema: tables as boxes, single-column foreign keys as lines, laid out
//! automatically (and movable by hand afterwards).

use std::collections::{HashMap, VecDeque};

use crate::db::{FkEdge, SchemaColumn, TableRef};

/// Box width and the size of one line inside it, in diagram units (points at 100% zoom).
pub const BOX_W: f32 = 220.0;
pub const ROW_H: f32 = 20.0;
/// Columns listed per box; the rest is summarized as "… +N".
pub const MAX_COLS: usize = 14;
const GAP_X: f32 = 90.0;
const GAP_Y: f32 = 36.0;
/// A row of components wraps past this width.
const WRAP_W: f32 = 2600.0;

#[derive(Clone, Debug, PartialEq)]
pub struct DiagramTable {
    pub name: String,
    /// `(column, type)`, in table order.
    pub columns: Vec<(String, String)>,
}

impl DiagramTable {
    /// Header plus listed columns (plus the "… +N" line).
    pub fn height(&self) -> f32 {
        let lines = self.columns.len().min(MAX_COLS) + usize::from(self.columns.len() > MAX_COLS);
        ROW_H * (1 + lines.max(1)) as f32 + 6.0
    }

    /// Vertical offset of a column's line inside the box (the header's when it isn't listed).
    pub fn column_y(&self, column: &str) -> f32 {
        match self.columns.iter().position(|(c, _)| c == column) {
            Some(i) if i < MAX_COLS => ROW_H * (i as f32 + 1.5),
            _ => ROW_H * 0.5,
        }
    }
}

pub struct DiagramState {
    pub schema: String,
    /// Foreign keys inside the schema, once loaded.
    pub edges: Option<Result<Vec<FkEdge>, String>>,
    /// Also draw tables that have no foreign key in or out.
    pub show_all: bool,
    pub tables: Vec<DiagramTable>,
    /// Top-left corner of each table's box, by table name.
    pub positions: HashMap<String, (f32, f32)>,
    /// View: screen = origin + pan + position × zoom.
    pub pan: (f32, f32),
    pub zoom: f32,
    /// Fit the view to the boxes on the next frame.
    pub fit: bool,
}

impl DiagramState {
    pub fn new(schema: &str) -> Self {
        DiagramState {
            schema: schema.to_string(),
            edges: None,
            show_all: false,
            tables: Vec::new(),
            positions: HashMap::new(),
            pan: (0.0, 0.0),
            zoom: 1.0,
            fit: true,
        }
    }

    /// The schema's edges (both ends inside it).
    pub fn schema_edges(&self) -> Vec<&FkEdge> {
        match &self.edges {
            Some(Ok(e)) => e
                .iter()
                .filter(|e| e.schema == self.schema && e.ref_schema == self.schema)
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Picks the tables to draw and places them; keeps positions the user already moved.
    pub fn build(&mut self, tables: &[TableRef], columns: &[SchemaColumn]) {
        let edges: Vec<FkEdge> = self.schema_edges().into_iter().cloned().collect();
        let linked = |name: &str| edges.iter().any(|e| e.table == name || e.ref_table == name);
        self.tables = tables
            .iter()
            .filter(|t| t.schema == self.schema && !t.is_view && (self.show_all || linked(&t.name)))
            .map(|t| DiagramTable {
                name: t.name.clone(),
                columns: columns
                    .iter()
                    .filter(|c| c.schema == t.schema && c.table == t.name)
                    .map(|c| (c.column.clone(), c.data_type.clone()))
                    .collect(),
            })
            .collect();
        let fresh = layout(&self.tables, &edges);
        for (name, p) in fresh {
            self.positions.entry(name).or_insert(p);
        }
        self.positions
            .retain(|n, _| self.tables.iter().any(|t| t.name == *n));
    }

    /// Bounds of every box: (min x, min y, max x, max y).
    pub fn bounds(&self) -> Option<(f32, f32, f32, f32)> {
        self.tables
            .iter()
            .filter_map(|t| {
                let (x, y) = *self.positions.get(&t.name)?;
                Some((x, y, x + BOX_W, y + t.height()))
            })
            .reduce(|a, b| (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3)))
    }
}

/// Connected groups side by side (wrapping into rows). Inside a group, the most connected
/// table starts the first column and each next column holds the tables one link further.
/// Unlinked tables go in a grid at the end.
pub fn layout(tables: &[DiagramTable], edges: &[FkEdge]) -> HashMap<String, (f32, f32)> {
    let index: HashMap<&str, usize> = tables
        .iter()
        .enumerate()
        .map(|(i, t)| (t.name.as_str(), i))
        .collect();
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); tables.len()];
    for e in edges {
        if let (Some(&a), Some(&b)) = (index.get(e.table.as_str()), index.get(e.ref_table.as_str()))
            && a != b
        {
            if !adj[a].contains(&b) {
                adj[a].push(b);
            }
            if !adj[b].contains(&a) {
                adj[b].push(a);
            }
        }
    }
    let mut pos = HashMap::new();
    let mut seen = vec![false; tables.len()];
    // Most connected first, so big groups lead.
    let mut order: Vec<usize> = (0..tables.len()).collect();
    order.sort_by_key(|&i| (std::cmp::Reverse(adj[i].len()), tables[i].name.clone()));
    let (mut x0, mut y0, mut band_h) = (0.0f32, 0.0f32, 0.0f32);
    let mut lonely = Vec::new();
    for &root in &order {
        if seen[root] {
            continue;
        }
        if adj[root].is_empty() {
            seen[root] = true;
            lonely.push(root);
            continue;
        }
        // BFS levels become columns.
        let mut levels: Vec<Vec<usize>> = Vec::new();
        let mut queue = VecDeque::from([(root, 0usize)]);
        seen[root] = true;
        while let Some((n, d)) = queue.pop_front() {
            if levels.len() <= d {
                levels.push(Vec::new());
            }
            levels[d].push(n);
            let mut next: Vec<usize> = adj[n].iter().copied().filter(|&m| !seen[m]).collect();
            next.sort_by_key(|&m| std::cmp::Reverse(adj[m].len()));
            for m in next {
                seen[m] = true;
                queue.push_back((m, d + 1));
            }
        }
        let width = levels.len() as f32 * (BOX_W + GAP_X);
        if x0 > 0.0 && x0 + width > WRAP_W {
            x0 = 0.0;
            y0 += band_h + GAP_Y * 2.0;
            band_h = 0.0;
        }
        for (d, level) in levels.iter().enumerate() {
            let x = x0 + d as f32 * (BOX_W + GAP_X);
            let mut y = y0;
            for &n in level {
                pos.insert(tables[n].name.clone(), (x, y));
                y += tables[n].height() + GAP_Y;
            }
            band_h = band_h.max(y - y0);
        }
        x0 += width + GAP_X;
    }
    // Tables without links: a grid below everything else.
    if !lonely.is_empty() {
        let (mut x, mut y) = (
            0.0f32,
            if pos.is_empty() {
                0.0
            } else {
                y0 + band_h + GAP_Y * 2.0
            },
        );
        let mut row_h = 0.0f32;
        lonely.sort_by_key(|&i| tables[i].name.clone());
        for i in lonely {
            if x > 0.0 && x + BOX_W > WRAP_W {
                x = 0.0;
                y += row_h + GAP_Y;
                row_h = 0.0;
            }
            pos.insert(tables[i].name.clone(), (x, y));
            row_h = row_h.max(tables[i].height());
            x += BOX_W + GAP_X;
        }
    }
    pos
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(name: &str, cols: usize) -> DiagramTable {
        DiagramTable {
            name: name.into(),
            columns: (0..cols).map(|i| (format!("c{i}"), "int".into())).collect(),
        }
    }

    fn fk(from: &str, to: &str) -> FkEdge {
        FkEdge {
            schema: "s".into(),
            table: from.into(),
            column: "c0".into(),
            ref_schema: "s".into(),
            ref_table: to.into(),
            ref_column: "c0".into(),
        }
    }

    #[test]
    fn hub_first_neighbours_next_column_loners_below() {
        let tables = [t("customers", 3), t("orders", 3), t("events", 3), t("loose", 2)];
        let edges = [fk("orders", "customers"), fk("events", "customers")];
        let p = layout(&tables, &edges);
        assert_eq!(p["customers"], (0.0, 0.0), "the most connected table leads");
        assert_eq!(
            p["orders"].0, p["events"].0,
            "its neighbours share the next column"
        );
        assert!(p["orders"].0 > 0.0);
        assert_ne!(p["orders"].1, p["events"].1, "stacked, not on top of each other");
        let below = tables
            .iter()
            .take(3)
            .map(|t| p[&t.name].1 + t.height())
            .fold(0.0f32, f32::max);
        assert!(p["loose"].1 > below, "unlinked tables go under the groups");
    }

    #[test]
    fn long_tables_list_some_columns() {
        let big = t("wide", 40);
        assert_eq!(big.height(), ROW_H * (1 + MAX_COLS + 1) as f32 + 6.0);
        assert_eq!(big.column_y("c0"), ROW_H * 1.5);
        assert_eq!(
            big.column_y("c30"),
            ROW_H * 0.5,
            "unlisted columns attach to the header"
        );
    }
}
