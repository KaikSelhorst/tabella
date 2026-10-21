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
    /// "Seq Scan on orders", "Hash Join", "Index lookup on c using PRIMARY"...
    pub label: String,
    /// Conditions and other notes: "Filter: (active)", "Index Cond: (id = 3)".
    pub details: Vec<String>,
    /// Planner cost of this node including its children.
    pub cost: Option<f64>,
    pub est_rows: Option<f64>,
    /// With ANALYZE: total time spent in this node and below (ms, all loops) and rows produced.
    pub actual_ms: Option<f64>,
    pub actual_rows: Option<f64>,
    pub children: Vec<Node>,
    /// Table a scan reads (Postgres "Relation Name"; MySQL the name after "Table scan on").
    pub relation: Option<String>,
    /// The scan's own filter condition (Postgres "Filter").
    pub filter: Option<String>,
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

/// What a number measures, for showing it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Unit {
    Ms,
    Cost,
    Rows,
    Count,
}

/// One figure of two plans side by side. Lower is better for every one of them.
#[derive(Clone, Debug, PartialEq)]
pub struct Metric {
    pub label: &'static str,
    pub unit: Unit,
    pub before: Option<f64>,
    pub after: Option<f64>,
}

impl Metric {
    /// How much it changed, in percent of the earlier value (negative: it dropped).
    pub fn change_pct(&self) -> Option<f64> {
        let (b, a) = (self.before?, self.after?);
        if b == 0.0 {
            return (a != 0.0).then_some(f64::INFINITY);
        }
        Some((a - b) / b * 100.0)
    }
}

/// A table that is read another way in the later plan.
#[derive(Clone, Debug, PartialEq)]
pub struct ScanChange {
    pub relation: String,
    pub before: String,
    pub after: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Comparison {
    pub metrics: Vec<Metric>,
    pub scan_changes: Vec<ScanChange>,
}

fn scans(plan: &Plan) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    plan.root.walk(0, &mut |n, _| {
        if let Some(r) = &n.relation
            && !out.iter().any(|(rel, _)| rel == r)
        {
            out.push((r.clone(), n.label.clone()));
        }
    });
    out
}

fn full_scans(plan: &Plan) -> f64 {
    let mut n = 0.0;
    plan.root.walk(0, &mut |node, _| {
        if node.is_full_scan() {
            n += 1.0;
        }
    });
    n
}

/// The plan as it was (`before`) against the plan now (`after`): times, cost, rows, how many
/// tables are read in full, and which tables switched from one kind of scan to another.
pub fn compare(before: &Plan, after: &Plan) -> Comparison {
    let both_ran = before.analyzed && after.analyzed;
    let metric = |label, unit, b: Option<f64>, a: Option<f64>| Metric {
        label,
        unit,
        before: b,
        after: a,
    };
    let mut metrics = Vec::new();
    if both_ran {
        metrics.push(metric(
            "Execution time",
            Unit::Ms,
            before.execution_ms,
            after.execution_ms,
        ));
        metrics.push(metric(
            "Planning time",
            Unit::Ms,
            before.planning_ms,
            after.planning_ms,
        ));
    }
    metrics.push(metric(
        "Planner cost",
        Unit::Cost,
        before.root.cost,
        after.root.cost,
    ));
    if both_ran {
        metrics.push(metric(
            "Rows read by the top step",
            Unit::Rows,
            before.root.actual_rows,
            after.root.actual_rows,
        ));
    }
    metrics.push(metric(
        "Tables read in full",
        Unit::Count,
        Some(full_scans(before)),
        Some(full_scans(after)),
    ));
    let after_scans = scans(after);
    let scan_changes = scans(before)
        .into_iter()
        .filter_map(|(relation, label)| {
            let (_, later) = after_scans.iter().find(|(r, _)| *r == relation)?;
            (*later != label).then(|| ScanChange {
                relation,
                before: label,
                after: later.clone(),
            })
        })
        .collect();
    Comparison {
        metrics,
        scan_changes,
    }
}

/// Postgres `EXPLAIN (FORMAT JSON[, ANALYZE])` output: `[{"Plan": {...}, "Execution Time": ...}]`.
pub fn from_pg_json(text: &str) -> Result<Plan, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("invalid plan: {e}"))?;
    let top = v.get(0).ok_or("empty plan")?;
    let root = pg_node(top.get("Plan").ok_or("plan without \"Plan\"")?);
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
        details.push(format!("Rows removed by filter: {r}"));
    }
    let loops = n("Actual Loops").unwrap_or(1.0);
    Node {
        relation: s("Relation Name").map(str::to_string),
        filter: s("Filter").map(str::to_string),
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
        0 => return Err("empty plan".into()),
        1 => roots.pop().unwrap(),
        _ => Node {
            label: "Plan".into(),
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

/// "Index this": a table read in full whose filter could use an index on these columns.
#[derive(Clone, Debug, PartialEq)]
pub struct IndexHint {
    pub table: String,
    /// Equality columns first, then range ones: the order a composite index wants.
    pub columns: Vec<String>,
}

/// Full scans with a filter, and the columns their filter compares. MySQL puts the filter in a
/// "Filter" node above the scan, so a parent's condition counts for its scan child.
pub fn index_hints(plan: &Plan) -> Vec<IndexHint> {
    fn walk(n: &Node, parent_filter: Option<&str>, out: &mut Vec<IndexHint>) {
        let own = n.filter.as_deref().or_else(|| {
            (n.label == "Filter")
                .then(|| n.details.first().map(String::as_str))
                .flatten()
        });
        if n.is_full_scan() {
            let table = n.relation.clone().or_else(|| {
                let l = n.label.as_str();
                l.strip_prefix("Table scan on ")
                    .map(|t| t.split_whitespace().next().unwrap_or(t).to_string())
            });
            if let (Some(table), Some(cond)) = (table, own.or(parent_filter)) {
                let columns = filter_columns(cond);
                if !columns.is_empty() && !out.iter().any(|h| h.table == table && h.columns == columns) {
                    out.push(IndexHint { table, columns });
                }
            }
        }
        for c in &n.children {
            walk(c, own, out);
        }
    }
    let mut out = Vec::new();
    walk(&plan.root, None, &mut out);
    out
}

/// Columns a condition compares with `=`, `<`, `>`, `<=`, `>=`, LIKE (`~~` in Postgres) or IN,
/// equality first, at most three. `(p.status = 'paid'::text) AND (total > 100)` → status, total.
pub fn filter_columns(cond: &str) -> Vec<String> {
    let chars: Vec<char> = cond.chars().collect();
    let mut eq: Vec<String> = Vec::new();
    let mut range: Vec<String> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' {
            // Skip string literals.
            i += 1;
            while i < chars.len() && chars[i] != '\'' {
                i += 1;
            }
            i += 1;
            continue;
        }
        let quoted = c == '"' || c == '`';
        if !(quoted || c.is_alphabetic() || c == '_') {
            i += 1;
            continue;
        }
        // Inside a function call (`lower(name)`) a column isn't usable by a plain index.
        let in_call = {
            let mut k = i;
            while k > 0 && chars[k - 1] == ' ' {
                k -= 1;
            }
            k >= 2 && chars[k - 1] == '(' && (chars[k - 2].is_alphanumeric() || chars[k - 2] == '_')
        };
        // An identifier, possibly qualified (a.b, "a"."b"): keep its last part.
        let mut last;
        let mut last_start;
        loop {
            last_start = i;
            let mut part = String::new();
            if i < chars.len() && (chars[i] == '"' || chars[i] == '`') {
                let q = chars[i];
                i += 1;
                while i < chars.len() && chars[i] != q {
                    part.push(chars[i]);
                    i += 1;
                }
                i += 1;
            } else {
                while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                    part.push(chars[i]);
                    i += 1;
                }
            }
            last = part;
            if i < chars.len() && chars[i] == '.' {
                i += 1;
                continue;
            }
            break;
        }
        // Function calls and casts (`lower(x)`, `::text`) aren't plain columns.
        // (Where the name starts, in chars: its length in bytes, or quotes, would land elsewhere.)
        if chars.get(i) == Some(&'(') || (last_start >= 2 && chars[last_start - 1] == ':') {
            continue;
        }
        // Postgres writes `(name)::text = …`: closing parens and casts sit between the column and
        // the operator.
        let mut j = i;
        loop {
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            if chars.get(j) == Some(&')') {
                j += 1;
            } else if chars.get(j) == Some(&':') && chars.get(j + 1) == Some(&':') {
                j += 2;
                // Type name, possibly several words (`character varying`) or with `(n)`.
                while j < chars.len()
                    && (chars[j].is_alphanumeric() || chars[j] == '_' || chars[j] == ' ' || chars[j] == '"')
                    && !matches!(chars[j], '=' | '<' | '>' | '~')
                {
                    // Stop before a word operator (`x::text LIKE …`).
                    let tail: String = chars[j..].iter().take(9).collect::<String>().to_uppercase();
                    if chars[j] == ' '
                        && [" LIKE ", " IN ", " IN(", " IS ", " BETWEEN "]
                            .iter()
                            .any(|w| tail.starts_with(w))
                    {
                        break;
                    }
                    j += 1;
                }
                if chars.get(j) == Some(&'(') {
                    while j < chars.len() && chars[j] != ')' {
                        j += 1;
                    }
                    j += 1;
                }
            } else {
                break;
            }
        }
        let rest: String = chars[j.min(chars.len())..].iter().collect::<String>();
        let op = rest.trim_start();
        let upper = op.to_uppercase();
        let is_eq = op.starts_with('=')
            || upper.starts_with("IN ")
            || upper.starts_with("IN(")
            || upper.starts_with("IS NULL");
        let is_range = (op.starts_with('<') && !op.starts_with("<>"))
            || op.starts_with('>')
            || op.starts_with("~~")
            || upper.starts_with("LIKE ")
            || upper.starts_with("BETWEEN ");
        const WORDS: &[&str] = &["AND", "OR", "NOT", "NULL", "TRUE", "FALSE", "ANY", "ALL"];
        if in_call || WORDS.contains(&last.to_uppercase().as_str()) || last.is_empty() {
            continue;
        }
        if is_eq && !eq.contains(&last) {
            eq.push(last);
        } else if is_range && !range.contains(&last) && !eq.contains(&last) {
            range.push(last);
        }
    }
    eq.extend(range);
    eq.truncate(3);
    eq
}

#[cfg(test)]
mod tests {
    /// Before and after an index: the time drops, a sequential scan turns into an index scan.
    #[test]
    fn comparing_plans_shows_what_changed() {
        let before = from_pg_json(
            r#"[{"Plan": {"Node Type": "Seq Scan", "Relation Name": "orders", "Total Cost": 1500.0,
                "Plan Rows": 40000, "Actual Total Time": 120.0, "Actual Rows": 40, "Actual Loops": 1},
                "Planning Time": 0.3, "Execution Time": 120.5}]"#,
        )
        .unwrap();
        let after = from_pg_json(
            r#"[{"Plan": {"Node Type": "Index Scan", "Relation Name": "orders", "Index Name": "idx_o",
                "Total Cost": 8.2, "Plan Rows": 40, "Actual Total Time": 0.05, "Actual Rows": 40,
                "Actual Loops": 1}, "Planning Time": 0.4, "Execution Time": 0.1}]"#,
        )
        .unwrap();
        let c = compare(&before, &after);
        let metric = |label: &str| c.metrics.iter().find(|m| m.label == label).unwrap().clone();
        let time = metric("Execution time");
        assert!(time.change_pct().unwrap() < -99.0, "{time:?}");
        assert_eq!(metric("Tables read in full").before, Some(1.0));
        assert_eq!(metric("Tables read in full").after, Some(0.0));
        assert!(metric("Planner cost").change_pct().unwrap() < -99.0);
        assert_eq!(c.scan_changes.len(), 1);
        assert_eq!(c.scan_changes[0].relation, "orders");
        assert!(c.scan_changes[0].before.starts_with("Seq Scan"));
        assert!(c.scan_changes[0].after.starts_with("Index Scan"));
        // Estimated plans have no times to compare, only cost and scans.
        let mut est = before.clone();
        est.analyzed = false;
        let c = compare(&est, &after);
        assert!(c.metrics.iter().all(|m| m.unit != Unit::Ms));
        // The same plan: nothing moved.
        let same = compare(&before, &before);
        assert!(same.scan_changes.is_empty());
        assert!(
            same.metrics
                .iter()
                .all(|m| m.change_pct().is_none_or(|p| p == 0.0))
        );
    }

    use super::*;

    #[test]
    fn reads_postgres_json() {
        let json = r#"[{"Plan": {"Node Type": "Hash Join", "Join Type": "Left", "Total Cost": 120.5,
            "Plan Rows": 40, "Actual Total Time": 3.0, "Actual Rows": 38, "Actual Loops": 1,
            "Hash Cond": "(p.customer_id = c.id)",
            "Plans": [
              {"Node Type": "Seq Scan", "Relation Name": "orders", "Alias": "p", "Total Cost": 80,
               "Plan Rows": 1000, "Actual Total Time": 1.5, "Actual Rows": 1000, "Actual Loops": 1,
               "Filter": "(paid)", "Rows Removed by Filter": 12},
              {"Node Type": "Index Scan", "Index Name": "customers_pkey", "Relation Name": "customers",
               "Alias": "customers", "Total Cost": 20, "Plan Rows": 1, "Actual Total Time": 0.25,
               "Actual Rows": 1, "Actual Loops": 2}
            ]},
            "Planning Time": 0.2, "Execution Time": 3.1}]"#;
        let p = from_pg_json(json).unwrap();
        assert!(p.analyzed);
        assert_eq!(p.execution_ms, Some(3.1));
        assert_eq!(p.root.label, "Hash Join (Left)");
        assert_eq!(p.root.details, ["Hash Cond: (p.customer_id = c.id)"]);
        let [scan, idx] = &p.root.children[..] else {
            panic!()
        };
        assert_eq!(scan.label, "Seq Scan on orders p");
        assert!(scan.is_full_scan());
        assert_eq!(scan.details[1], "Rows removed by filter: 12");
        assert_eq!(idx.label, "Index Scan using customers_pkey on customers");
        // Loops multiply time and rows.
        assert_eq!((idx.actual_ms, idx.actual_rows), (Some(0.5), Some(2.0)));
        assert_eq!(p.root.self_cost(), Some(20.5));
        assert_eq!(p.root.self_ms(), Some(1.0));
    }

    #[test]
    fn suggests_indexes_for_filtered_full_scans() {
        assert_eq!(
            filter_columns(
                "((p.status = 'paid'::text) AND (total > '100'::numeric) AND (lower(name) = 'x'))"
            ),
            ["status", "total"]
        );
        assert_eq!(
            filter_columns("(created_at >= '2024-01-01' AND state IN ('NY', 'CA'))"),
            ["state", "created_at"]
        );
        assert_eq!(filter_columns("(\"Name\" ~~ 'a%'::text)"), ["Name"]);
        // As Postgres prints them: the column wrapped and cast before the operator.
        assert_eq!(filter_columns("((name)::text = 'x'::text)"), ["name"]);
        assert_eq!(
            filter_columns("((sku)::character varying(20) = 'A'::bpchar)"),
            ["sku"]
        );
        assert!(filter_columns("(a <> 1)").is_empty(), "<> rarely uses an index");

        let pg = from_pg_json(r#"[{"Plan": {"Node Type": "Seq Scan", "Relation Name": "orders", "Alias": "p", "Total Cost": 1, "Plan Rows": 5, "Filter": "(status = 'paid'::text)"}}]"#).unwrap();
        assert_eq!(
            index_hints(&pg),
            [IndexHint {
                table: "orders".into(),
                columns: vec!["status".into()]
            }]
        );
        let my = from_mysql_tree("-> Filter: (sales.channel = 'app')  (cost=1 rows=3)\n    -> Table scan on sales  (cost=1 rows=30)\n").unwrap();
        assert_eq!(
            index_hints(&my),
            [IndexHint {
                table: "sales".into(),
                columns: vec!["channel".into()]
            }]
        );
        // An index scan needs nothing.
        let ok = from_pg_json(r#"[{"Plan": {"Node Type": "Index Scan", "Relation Name": "t", "Index Name": "i", "Total Cost": 1, "Plan Rows": 1, "Filter": "(a = 1)"}}]"#).unwrap();
        assert!(index_hints(&ok).is_empty());
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

    /// A plan from a server we didn't expect (or a truncated one): an error, not a crash.
    #[test]
    fn any_text_is_an_answer() {
        crate::testgen::each_text(6, 3000, |s, _| {
            let _ = (from_pg_json(s), from_mysql_tree(s), filter_columns(s));
        });
        // JSON-shaped but not a plan.
        for s in [
            "[]",
            "[{}]",
            "[{\"Plan\": 1}]",
            "[{\"Plan\": {\"Plans\": [1]}}]",
            "{\"Plan\": null}",
        ] {
            let _ = from_pg_json(s);
        }
    }

    /// MySQL's tree plans of any shape (indents up and down, odd numbers, missing parts).
    #[test]
    fn any_mysql_tree_is_an_answer() {
        let mut g = crate::testgen::Gen::new(30);
        let numbers = [
            "0",
            "1.5",
            "-3",
            "1e400",
            "NaN",
            "",
            "99999999999999999999",
            "0.1..",
            "..2",
        ];
        for _ in 0..1500 {
            let text: String = (0..g.below(12))
                .map(|_| {
                    let n = |g: &mut crate::testgen::Gen| numbers[g.below(numbers.len())];
                    format!(
                        "{}-> {}{} (cost={} rows={}) (actual time={}..{} rows={} loops={})\n",
                        " ".repeat(g.below(12)),
                        g.text_of(3),
                        ["", " on t", " using idx"][g.below(3)],
                        n(&mut g),
                        n(&mut g),
                        n(&mut g),
                        n(&mut g),
                        n(&mut g),
                        n(&mut g),
                    )
                })
                .collect();
            if let Ok(plan) = from_mysql_tree(&text) {
                let _ = plan.root.label.len();
            }
        }
    }
}
