//! Query plans (EXPLAIN) as one tree for both engines: Postgres `EXPLAIN (FORMAT JSON)` and
//! MySQL `EXPLAIN FORMAT=TREE` / `EXPLAIN ANALYZE`, which is indented text.

use serde_json::Value;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plan {
    pub root: Node,
    /// Whether the statement actually ran (ANALYZE): nodes then have real times and rows.
    pub analyzed: bool,
    /// Postgres reports these separately (ms).
    pub planning_ms: Option<f64>,
    pub execution_ms: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Node {
    /// "Seq Scan on pedidos", "Hash Join", "Index lookup on c using PRIMARY"...
    pub label: String,
    /// Conditions and other notes: "Filter: (ativo)", "Index Cond: (id = 3)".
    pub details: Vec<String>,
    /// Planner cost of this node including its children.
    pub cost: Option<f64>,
    pub est_rows: Option<f64>,
    /// With ANALYZE: total time spent in this node and below (ms, all loops) and rows produced.
    pub actual_ms: Option<f64>,
    pub actual_rows: Option<f64>,
    pub children: Vec<Node>,
}

impl Node {
    /// Cost of this node alone (its children's share taken out).
    pub fn self_cost(&self) -> Option<f64> {
        let below: f64 = self.children.iter().filter_map(|c| c.cost).sum();
        self.cost.map(|c| (c - below).max(0.0))
    }

    pub fn self_ms(&self) -> Option<f64> {
        let below: f64 = self.children.iter().filter_map(|c| c.actual_ms).sum();
        self.actual_ms.map(|t| (t - below).max(0.0))
    }

    /// Reads the whole table: worth pointing out when the table is big.
    pub fn is_full_scan(&self) -> bool {
        let l = self.label.to_lowercase();
        l.starts_with("seq scan") || l.starts_with("table scan") || l.starts_with("parallel seq scan")
    }

    /// Visits nodes depth first with their depth.
    pub fn walk<'a>(&'a self, depth: usize, f: &mut impl FnMut(&'a Node, usize)) {
        f(self, depth);
        for c in &self.children {
            c.walk(depth + 1, f);
        }
    }
}

/// Postgres `EXPLAIN (FORMAT JSON[, ANALYZE])` output: `[{"Plan": {...}, "Execution Time": ...}]`.
pub fn from_pg_json(text: &str) -> Result<Plan, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("plano inválido: {e}"))?;
    let top = v.get(0).ok_or("plano vazio")?;
    let root = pg_node(top.get("Plan").ok_or("plano sem \"Plan\"")?);
    let ms = |k: &str| top.get(k).and_then(Value::as_f64);
    Ok(Plan {
        analyzed: root.actual_ms.is_some(),
        root,
        planning_ms: ms("Planning Time"),
        execution_ms: ms("Execution Time"),
    })
}

fn pg_node(v: &Value) -> Node {
    let s = |k: &str| v.get(k).and_then(Value::as_str);
    let n = |k: &str| v.get(k).and_then(Value::as_f64);
    let mut label = s("Node Type").unwrap_or("?").to_string();
    if let Some(j) = s("Join Type")
        && label.ends_with("Join")
        && j != "Inner"
    {
        label = format!("{label} ({j})");
    }
    if let Some(dir) = s("Scan Direction").filter(|d| *d == "Backward") {
        label = format!("{label} {dir}");
    }
    if let Some(idx) = s("Index Name") {
        label = format!("{label} using {idx}");
    }
    if let Some(rel) = s("Relation Name") {
        let alias = s("Alias").filter(|a| *a != rel);
        label = match alias {
            Some(a) => format!("{label} on {rel} {a}"),
            None => format!("{label} on {rel}"),
        };
    }
    let mut details = Vec::new();
    for key in [
        "Index Cond",
        "Recheck Cond",
        "Hash Cond",
        "Merge Cond",
        "Join Filter",
        "Filter",
        "Sort Key",
        "Group Key",
    ] {
        match v.get(key) {
            Some(Value::String(c)) => details.push(format!("{key}: {c}")),
            Some(Value::Array(items)) => {
                let items: Vec<&str> = items.iter().filter_map(Value::as_str).collect();
                details.push(format!("{key}: {}", items.join(", ")));
            }
            _ => {}
        }
    }
    if let Some(r) = n("Rows Removed by Filter").filter(|r| *r > 0.0) {
        details.push(format!("Linhas descartadas pelo filtro: {r}"));
    }
    let loops = n("Actual Loops").unwrap_or(1.0);
    Node {
        label,
        details,
        cost: n("Total Cost"),
        est_rows: n("Plan Rows"),
        actual_ms: n("Actual Total Time").map(|t| t * loops),
        actual_rows: n("Actual Rows").map(|r| r * loops),
        children: v
            .get("Plans")
            .and_then(Value::as_array)
            .map(|ps| ps.iter().map(pg_node).collect())
            .unwrap_or_default(),
    }
}

/// MySQL `EXPLAIN FORMAT=TREE` (or `EXPLAIN ANALYZE`): one `-> node` per line, indented by depth.
pub fn from_mysql_tree(text: &str) -> Result<Plan, String> {
    let mut stack: Vec<(usize, Node)> = Vec::new();
    let mut roots = Vec::new();
    for line in text.lines() {
        let indent = line.len() - line.trim_start().len();
        let Some(body) = line.trim_start().strip_prefix("-> ") else {
            continue;
        };
        let node = mysql_node(body);
        // Close nodes at the same depth or deeper: they have all their children now.
        while stack.last().is_some_and(|(d, _)| *d >= indent) {
            let (_, done) = stack.pop().unwrap();
            attach(&mut stack, &mut roots, done);
        }
        stack.push((indent, node));
    }
    while let Some((_, done)) = stack.pop() {
        attach(&mut stack, &mut roots, done);
    }
    let root = match roots.len() {
        0 => return Err("plano vazio".into()),
        1 => roots.pop().unwrap(),
        _ => Node {
            label: "Plano".into(),
            children: roots,
            ..Default::default()
        },
    };
    Ok(Plan {
        analyzed: root.actual_ms.is_some(),
        root,
        planning_ms: None,
        execution_ms: None,
    })
}

fn attach(stack: &mut [(usize, Node)], roots: &mut Vec<Node>, node: Node) {
    match stack.last_mut() {
        Some((_, parent)) => parent.children.push(node),
        None => roots.push(node),
    }
}

/// `Filter: (x > 5)  (cost=1.2 rows=3) (actual time=0.1..0.4 rows=2 loops=1)`
fn mysql_node(body: &str) -> Node {
    let mut node = Node::default();
    let mut text = body.trim_end();
    // Parenthesized stats at the end, last first.
    while let Some(open) = text.rfind(" (") {
        let group = &text[open + 2..];
        let Some(inner) = group.strip_suffix(')') else {
            break;
        };
        if inner.starts_with("cost=") || inner.starts_with("rows=") {
            for kv in inner.split_whitespace() {
                match kv.split_once('=') {
                    Some(("cost", v)) => node.cost = v.parse().ok(),
                    Some(("rows", v)) => node.est_rows = v.parse().ok(),
                    _ => {}
                }
            }
        } else if let Some(actual) = inner.strip_prefix("actual time=") {
            let mut parts = actual.split_whitespace();
            let last_ms: Option<f64> = parts
                .next()
                .and_then(|t| t.split("..").nth(1))
                .and_then(|t| t.parse().ok());
            let (mut rows, mut loops) = (None, 1.0);
            for kv in parts {
                match kv.split_once('=') {
                    Some(("rows", v)) => rows = v.parse::<f64>().ok(),
                    Some(("loops", v)) => loops = v.parse().unwrap_or(1.0),
                    _ => {}
                }
            }
            node.actual_ms = last_ms.map(|t| t * loops);
            node.actual_rows = rows.map(|r| r * loops);
        } else if inner == "never executed" {
            node.actual_ms = Some(0.0);
            node.actual_rows = Some(0.0);
        } else {
            break;
        }
        text = text[..open].trim_end();
    }
    // "Filter: (cond)" keeps its condition as a detail, like Postgres.
    match text.split_once(": ") {
        Some((kind, cond)) if !kind.contains(' ') || kind == "Index range scan" => {
            node.label = kind.to_string();
            node.details.push(cond.to_string());
        }
        _ => node.label = text.to_string(),
    }
    node
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_postgres_json() {
        let json = r#"[{"Plan": {"Node Type": "Hash Join", "Join Type": "Left", "Total Cost": 120.5,
            "Plan Rows": 40, "Actual Total Time": 3.0, "Actual Rows": 38, "Actual Loops": 1,
            "Hash Cond": "(p.cliente_id = c.id)",
            "Plans": [
              {"Node Type": "Seq Scan", "Relation Name": "pedidos", "Alias": "p", "Total Cost": 80,
               "Plan Rows": 1000, "Actual Total Time": 1.5, "Actual Rows": 1000, "Actual Loops": 1,
               "Filter": "(pago)", "Rows Removed by Filter": 12},
              {"Node Type": "Index Scan", "Index Name": "clientes_pkey", "Relation Name": "clientes",
               "Alias": "clientes", "Total Cost": 20, "Plan Rows": 1, "Actual Total Time": 0.25,
               "Actual Rows": 1, "Actual Loops": 2}
            ]},
            "Planning Time": 0.2, "Execution Time": 3.1}]"#;
        let p = from_pg_json(json).unwrap();
        assert!(p.analyzed);
        assert_eq!(p.execution_ms, Some(3.1));
        assert_eq!(p.root.label, "Hash Join (Left)");
        assert_eq!(p.root.details, ["Hash Cond: (p.cliente_id = c.id)"]);
        let [scan, idx] = &p.root.children[..] else {
            panic!()
        };
        assert_eq!(scan.label, "Seq Scan on pedidos p");
        assert!(scan.is_full_scan());
        assert_eq!(scan.details[1], "Linhas descartadas pelo filtro: 12");
        assert_eq!(idx.label, "Index Scan using clientes_pkey on clientes");
        // Loops multiply time and rows.
        assert_eq!((idx.actual_ms, idx.actual_rows), (Some(0.5), Some(2.0)));
        assert_eq!(p.root.self_cost(), Some(20.5));
        assert_eq!(p.root.self_ms(), Some(1.0));
    }

    #[test]
    fn reads_mysql_trees() {
        let text = "-> Aggregate: count(0)  (cost=1.9 rows=1) (actual time=0.437..0.437 rows=1 loops=1)\n    -> Inner hash join (u.`User` = d.`User`)  (cost=1.7 rows=2) (actual time=0.409..0.412 rows=2 loops=1)\n        -> Table scan on u  (cost=0.175 rows=5) (actual time=0.02..0.03 rows=5 loops=2)\n        -> Hash\n            -> Covering index scan on d using User  (cost=0.45 rows=2) (never executed)\n";
        let p = from_mysql_tree(text).unwrap();
        assert!(p.analyzed);
        assert_eq!(p.root.label, "Aggregate");
        assert_eq!(p.root.details, ["count(0)"]);
        assert_eq!(p.root.cost, Some(1.9));
        let join = &p.root.children[0];
        assert_eq!(join.label, "Inner hash join (u.`User` = d.`User`)");
        let [scan, hash] = &join.children[..] else {
            panic!("{join:?}")
        };
        assert!(scan.is_full_scan());
        assert_eq!((scan.actual_ms, scan.actual_rows), (Some(0.06), Some(10.0)));
        assert_eq!(hash.label, "Hash");
        assert_eq!(hash.children[0].label, "Covering index scan on d using User");
        assert_eq!(hash.children[0].actual_ms, Some(0.0));

        let plain = from_mysql_tree(
            "-> Filter: (t.a > 5)  (cost=10 rows=3)\n    -> Table scan on t  (cost=10 rows=30)\n",
        )
        .unwrap();
        assert!(!plain.analyzed);
        assert_eq!(plain.root.details, ["(t.a > 5)"]);
        assert_eq!(plain.root.children[0].est_rows, Some(30.0));
    }
}
