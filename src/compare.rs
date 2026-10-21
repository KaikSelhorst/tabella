//! "Compare structure": the tables of two schemas (two databases, or two schemas of one) and
//! the SQL that makes the target's structure match the source's. Only structure: data is left
//! alone.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::db::{Conn, Kind, TableStructure};
use crate::error::DbResult;

/// One table as compared: its parts by name, each with its definition text.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TableDef {
    /// `(name, definition)` in table order. Postgres: type, NOT NULL, DEFAULT; MySQL: the line
    /// of SHOW CREATE TABLE.
    pub columns: Vec<(String, Column)>,
    /// Primary key, unique, check and foreign key constraints.
    pub constraints: BTreeMap<String, String>,
    /// Indexes not backing a constraint (MySQL: every KEY line).
    pub indexes: BTreeMap<String, String>,
    /// The CREATE TABLE without foreign keys, and those as ALTERs.
    pub create: String,
    pub fks: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Column {
    pub data_type: String,
    pub nullable: bool,
    pub default: Option<String>,
    /// MySQL: the whole definition line.
    pub line: String,
}

pub struct Snapshot {
    pub schema: String,
    pub tables: BTreeMap<String, TableDef>,
}

/// Reads every table of `schema` (views left out).
pub fn snapshot(conn: &mut Conn, schema: &str) -> DbResult<Snapshot> {
    let kind = conn.kind();
    let names: Vec<String> = conn
        .tables()?
        .into_iter()
        .filter(|t| t.schema == schema && !t.is_view)
        .map(|t| t.name)
        .collect();
    let mut tables = BTreeMap::new();
    for name in names {
        let st = conn.structure(schema, &name)?;
        tables.insert(name, table_def(kind, &st));
    }
    Ok(Snapshot {
        schema: schema.to_string(),
        tables,
    })
}

pub fn table_def(kind: Kind, st: &TableStructure) -> TableDef {
    let mut def = TableDef {
        create: st.ddl_without_fks.clone(),
        fks: st.fk_statements.clone(),
        ..Default::default()
    };
    match kind {
        // SQLite: the columns as read; indexes with their own SQL; foreign keys by what they say
        // (their numbers aren't names).
        Kind::Sqlite => {
            def.columns = st
                .columns
                .iter()
                .map(|c| {
                    let col = Column {
                        data_type: c.data_type.clone(),
                        nullable: c.nullable,
                        default: c.default.clone(),
                        line: String::new(),
                    };
                    (c.name.clone(), col)
                })
                .collect();
            for (name, sql) in st.indexes.iter().filter(|(_, sql)| sql.starts_with("CREATE")) {
                def.indexes.insert(name.clone(), sql.clone());
            }
            for (_, fk) in &st.foreign_keys {
                def.constraints.insert(fk.clone(), fk.clone());
            }
        }
        Kind::Postgres => {
            def.columns = st
                .columns
                .iter()
                .map(|c| {
                    let col = Column {
                        data_type: c.data_type.clone(),
                        nullable: c.nullable,
                        default: c.default.clone(),
                        line: String::new(),
                    };
                    (c.name.clone(), col)
                })
                .collect();
            // From the generated DDL: `    CONSTRAINT "name" …` lines, then standalone indexes.
            for line in st.ddl.lines() {
                let l = line.trim().trim_end_matches(',');
                if let Some(rest) = l.strip_prefix("CONSTRAINT ") {
                    let (name, body) = split_name(rest);
                    def.constraints.insert(name, body.to_string());
                } else if l.starts_with("CREATE ") && l.contains(" INDEX ") {
                    let name = l
                        .split(" INDEX ")
                        .nth(1)
                        .and_then(|r| r.split_whitespace().next())
                        .map(|n| n.trim_matches('"').to_string())
                        .unwrap_or_default();
                    def.indexes.insert(name, l.trim_end_matches(';').to_string());
                }
            }
        }
        Kind::MySql => {
            // SHOW CREATE TABLE: one column, key or constraint per line.
            for line in st.ddl.lines().skip(1) {
                let l = line.trim().trim_end_matches(',');
                if l.starts_with(')') {
                    break;
                }
                if l.starts_with('`') {
                    let (name, _) = split_name(l);
                    let col = Column {
                        line: l.to_string(),
                        ..Default::default()
                    };
                    def.columns.push((name, col));
                } else if l.starts_with("PRIMARY KEY") {
                    def.indexes.insert("PRIMARY".into(), l.to_string());
                } else if let Some(rest) = l.strip_prefix("CONSTRAINT ") {
                    let (name, _) = split_name(rest);
                    def.constraints.insert(name, l.to_string());
                } else if let Some(at) = l.find("KEY `") {
                    let (name, _) = split_name(&l[at + 4..]);
                    def.indexes.insert(name, l.to_string());
                }
            }
        }
    }
    def
}

/// `"name" rest` / `` `name` rest `` → (name, rest).
fn split_name(s: &str) -> (String, &str) {
    let s = s.trim_start();
    let quote = s.chars().next().filter(|c| *c == '"' || *c == '`');
    match quote {
        Some(q) => {
            let end = s[1..].find(q).map_or(s.len(), |i| i + 1);
            (s[1..end].to_string(), s.get(end + 1..).unwrap_or("").trim_start())
        }
        None => {
            let end = s.find(char::is_whitespace).unwrap_or(s.len());
            (s[..end].to_string(), s[end..].trim_start())
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    /// Only in the source: created in the target.
    New,
    /// Only in the target: dropping it is left commented out.
    Extra,
    Changed,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    /// For people: "column email: text → varchar(120)".
    pub what: String,
    pub sql: String,
    /// Loses data (drops a column, a table).
    pub destructive: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TableDiff {
    pub table: String,
    pub status: Status,
    pub items: Vec<Item>,
}

/// What differs, table by table (sorted by name; equal tables left out).
pub fn diff(kind: Kind, a: &Snapshot, b: &Snapshot) -> Vec<TableDiff> {
    let names: std::collections::BTreeSet<&String> = a.tables.keys().chain(b.tables.keys()).collect();
    let target = |t: &str| kind.qualified(&b.schema, t);
    let mut out = Vec::new();
    for name in names {
        match (a.tables.get(name), b.tables.get(name)) {
            (Some(ta), None) => {
                let mut sql = retarget(kind, &ta.create, &a.schema, &b.schema);
                if kind == Kind::MySql {
                    // SHOW CREATE TABLE leaves the database out.
                    sql = sql.replacen(
                        &format!("CREATE TABLE {}", kind.quote_ident(name)),
                        &format!("CREATE TABLE {}", target(name)),
                        1,
                    );
                }
                let mut items = vec![Item {
                    what: "new table".into(),
                    sql,
                    destructive: false,
                }];
                items.extend(ta.fks.iter().map(|f| Item {
                    what: "foreign key".into(),
                    sql: retarget(kind, f, &a.schema, &b.schema),
                    destructive: false,
                }));
                out.push(TableDiff {
                    table: name.clone(),
                    status: Status::New,
                    items,
                });
            }
            (None, Some(_)) => out.push(TableDiff {
                table: name.clone(),
                status: Status::Extra,
                items: vec![Item {
                    what: "only in the target".into(),
                    sql: format!("-- DROP TABLE {};", target(name)),
                    destructive: true,
                }],
            }),
            (Some(ta), Some(tb)) => {
                let items = table_items(kind, &target(name), ta, tb, &a.schema, &b.schema);
                if !items.is_empty() {
                    out.push(TableDiff {
                        table: name.clone(),
                        status: Status::Changed,
                        items,
                    });
                }
            }
            (None, None) => {}
        }
    }
    out
}

/// The whole script: new tables first, foreign keys of new tables last (their targets exist
/// by then), changes in between.
pub fn script(diffs: &[TableDiff]) -> String {
    let mut first = Vec::new();
    let mut middle = Vec::new();
    let mut last = Vec::new();
    for d in diffs {
        for (i, it) in d.items.iter().enumerate() {
            match d.status {
                Status::New if i == 0 => first.push(it.sql.clone()),
                Status::New => last.push(it.sql.clone()),
                _ => middle.push(it.sql.clone()),
            }
        }
    }
    first
        .into_iter()
        .chain(middle)
        .chain(last)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Statements and names of the source schema, pointed at the target schema.
fn retarget(kind: Kind, sql: &str, from: &str, to: &str) -> String {
    if from == to {
        return sql.to_string();
    }
    replace_schema(kind, sql, from, &format!("{}.", kind.quote_ident(to)))
}

/// A definition with its schema names taken out, for comparing across schemas.
fn plain(kind: Kind, sql: &str, schema: &str) -> String {
    replace_schema(kind, sql, schema, "")
}

/// `schema.` (quoted, or bare on Postgres where the server writes it so — `ON public.t`,
/// `nextval('public.t_id_seq')`) replaced by `with`; a bare one only as a whole name, so
/// `x_public.t` stays.
fn replace_schema(kind: Kind, sql: &str, schema: &str, with: &str) -> String {
    let s = sql.replace(&format!("{}.", kind.quote_ident(schema)), with);
    if kind == Kind::MySql {
        return s;
    }
    let bare = format!("{schema}.");
    let mut out = String::with_capacity(s.len());
    let mut rest = s.as_str();
    while let Some(i) = rest.find(&bare) {
        out.push_str(&rest[..i]);
        let before = out.chars().next_back();
        if before.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '"') {
            out.push_str(&bare);
        } else {
            out.push_str(with);
        }
        rest = &rest[i + bare.len()..];
    }
    out.push_str(rest);
    out
}

fn table_items(kind: Kind, table: &str, a: &TableDef, b: &TableDef, sa: &str, sb: &str) -> Vec<Item> {
    let mut items = Vec::new();
    let alter = |rest: String| format!("ALTER TABLE {table} {rest};");
    let item = |what: String, sql: String, destructive: bool| Item {
        what,
        sql,
        destructive,
    };
    let q = |n: &str| kind.quote_ident(n);

    // Constraints and indexes that go away or change are dropped before the columns change.
    let drop_index = |name: &str| match kind {
        Kind::Postgres => format!("DROP INDEX {}.{};", q(sb), q(name)),
        Kind::Sqlite => format!("DROP INDEX {};", q(name)),
        Kind::MySql if name == "PRIMARY" => alter("DROP PRIMARY KEY".into()),
        Kind::MySql => alter(format!("DROP INDEX {}", q(name))),
    };
    let drop_constraint = |name: &str, def: &str| match kind {
        Kind::Postgres => alter(format!("DROP CONSTRAINT {}", q(name))),
        Kind::MySql if def.contains("FOREIGN KEY") => alter(format!("DROP FOREIGN KEY {}", q(name))),
        Kind::MySql => alter(format!("DROP CHECK {}", q(name))),
        Kind::Sqlite => {
            format!("-- SQLite can't drop a constraint of an existing table ({table}): recreate the table")
        }
    };
    let same = |x: &str, y: &str| plain(kind, x, sa) == plain(kind, y, sb);
    let mut adds = Vec::new();
    for (name, def) in &b.constraints {
        match a.constraints.get(name) {
            None => items.push(item(
                format!("extra constraint {name}"),
                drop_constraint(name, def),
                false,
            )),
            Some(da) if !same(da, def) => {
                items.push(item(
                    format!("constraint {name} differs"),
                    drop_constraint(name, def),
                    false,
                ));
                adds.push(name);
            }
            _ => {}
        }
    }
    for (name, def) in &b.indexes {
        match a.indexes.get(name) {
            None => items.push(item(format!("extra index {name}"), drop_index(name), false)),
            Some(da) if !same(da, def) => {
                items.push(item(format!("index {name} differs"), drop_index(name), false))
            }
            _ => {}
        }
    }

    // Columns.
    let cols_b: BTreeMap<&String, &Column> = b.columns.iter().map(|(n, c)| (n, c)).collect();
    let cols_a: BTreeMap<&String, &Column> = a.columns.iter().map(|(n, c)| (n, c)).collect();
    for (name, ca) in &a.columns {
        let c = q(name);
        match (kind, cols_b.get(name)) {
            (Kind::Postgres | Kind::Sqlite, None) => {
                let mut def = format!("ADD COLUMN {c} {}", ca.data_type);
                if !ca.nullable {
                    def += " NOT NULL";
                }
                if let Some(d) = &ca.default {
                    let _ = write!(def, " DEFAULT {}", retarget(kind, d, sa, sb));
                }
                items.push(item(format!("new column {name}"), alter(def), false));
            }
            (Kind::MySql, None) => {
                items.push(item(
                    format!("new column {name}"),
                    alter(format!("ADD COLUMN {}", ca.line)),
                    false,
                ));
            }
            (Kind::Postgres, Some(cb)) => {
                if ca.data_type != cb.data_type {
                    let t = &ca.data_type;
                    items.push(item(
                        format!("column {name}: {} → {t}", cb.data_type),
                        alter(format!("ALTER COLUMN {c} TYPE {t} USING {c}::{t}")),
                        false,
                    ));
                }
                if ca.nullable != cb.nullable {
                    let (what, sql) = if ca.nullable {
                        ("allows NULL", "DROP NOT NULL")
                    } else {
                        ("NOT NULL", "SET NOT NULL")
                    };
                    items.push(item(
                        format!("column {name}: {what}"),
                        alter(format!("ALTER COLUMN {c} {sql}")),
                        false,
                    ));
                }
                let (da, db) = (
                    ca.default.as_deref().map(|d| plain(kind, d, sa)),
                    cb.default.as_deref().map(|d| plain(kind, d, sb)),
                );
                if da != db {
                    let sql = match &ca.default {
                        Some(d) => format!("ALTER COLUMN {c} SET DEFAULT {}", retarget(kind, d, sa, sb)),
                        None => format!("ALTER COLUMN {c} DROP DEFAULT"),
                    };
                    let what = format!(
                        "column {name}: default {}",
                        ca.default.as_deref().unwrap_or("none")
                    );
                    items.push(item(what, alter(sql), false));
                }
            }
            (Kind::Sqlite, Some(cb)) => {
                let differs =
                    ca.data_type != cb.data_type || ca.nullable != cb.nullable || ca.default != cb.default;
                if differs {
                    items.push(item(
                        format!("column {name}: {} → {}", cb.data_type, ca.data_type),
                        format!("-- SQLite can't alter an existing column ({table}.{c}): recreate the table"),
                        false,
                    ));
                }
            }
            (Kind::MySql, Some(cb)) => {
                if plain(kind, &ca.line, sa) != plain(kind, &cb.line, sb) {
                    items.push(item(
                        format!("column {name}: {} → {}", cb.line, ca.line),
                        alter(format!("MODIFY COLUMN {}", ca.line)),
                        false,
                    ));
                }
            }
        }
    }
    for (name, _) in &b.columns {
        if !cols_a.contains_key(name) {
            items.push(item(
                format!("column {name} only exists in the target (its data is lost)"),
                alter(format!("DROP COLUMN {}", q(name))),
                true,
            ));
        }
    }

    // New or changed indexes and constraints, now that the columns are there.
    for (name, def) in &a.indexes {
        let changed = b.indexes.get(name).is_some_and(|db| !same(def, db));
        if b.indexes.contains_key(name) && !changed {
            continue;
        }
        let what = if changed { "recreate" } else { "new" };
        let sql = match kind {
            Kind::Postgres | Kind::Sqlite => format!("{};", retarget(kind, def, sa, sb)),
            Kind::MySql => alter(format!("ADD {def}")),
        };
        items.push(item(format!("index {name} ({what})"), sql, false));
    }
    for (name, def) in &a.constraints {
        if b.constraints.contains_key(name) && !adds.contains(&name) {
            continue;
        }
        let sql = match kind {
            Kind::Postgres => alter(format!(
                "ADD CONSTRAINT {} {}",
                q(name),
                retarget(kind, def, sa, sb)
            )),
            Kind::MySql => alter(format!("ADD {}", retarget(kind, def, sa, sb))),
            Kind::Sqlite => {
                format!("-- SQLite can't add {def} to an existing table ({table}): recreate the table")
            }
        };
        items.push(item(format!("constraint {name}"), sql, false));
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pg_table(
        cols: &[(&str, &str, bool, Option<&str>)],
        constraints: &[(&str, &str)],
        indexes: &[(&str, &str)],
    ) -> TableDef {
        TableDef {
            columns: cols
                .iter()
                .map(|(n, t, null, d)| {
                    let c = Column {
                        data_type: t.to_string(),
                        nullable: *null,
                        default: d.map(String::from),
                        line: String::new(),
                    };
                    (n.to_string(), c)
                })
                .collect(),
            constraints: constraints
                .iter()
                .map(|(n, d)| (n.to_string(), d.to_string()))
                .collect(),
            indexes: indexes
                .iter()
                .map(|(n, d)| (n.to_string(), d.to_string()))
                .collect(),
            create: "CREATE TABLE \"public\".\"t\" ();".into(),
            fks: vec![],
        }
    }

    #[test]
    fn postgres_changes_become_alters() {
        let a = Snapshot {
            schema: "public".into(),
            tables: [
                (
                    "customers".to_string(),
                    pg_table(
                        &[
                            ("id", "integer", false, None),
                            ("email", "character varying(120)", false, None),
                            ("active", "boolean", true, Some("true")),
                        ],
                        &[("customers_pkey", "PRIMARY KEY (id)")],
                        &[(
                            "customers_email_idx",
                            "CREATE INDEX customers_email_idx ON public.customers USING btree (email)",
                        )],
                    ),
                ),
                (
                    "fresh".to_string(),
                    pg_table(&[("id", "integer", false, None)], &[], &[]),
                ),
            ]
            .into(),
        };
        let b = Snapshot {
            schema: "staging".into(),
            tables: [
                (
                    "customers".to_string(),
                    pg_table(
                        &[
                            ("id", "integer", false, None),
                            ("email", "text", true, None),
                            ("old", "text", true, None),
                        ],
                        &[("customers_pkey", "PRIMARY KEY (id)")],
                        &[],
                    ),
                ),
                (
                    "leftover".to_string(),
                    pg_table(&[("id", "integer", false, None)], &[], &[]),
                ),
            ]
            .into(),
        };
        let d = diff(Kind::Postgres, &a, &b);
        let status: Vec<(&str, &Status)> = d.iter().map(|t| (t.table.as_str(), &t.status)).collect();
        assert_eq!(
            status,
            [
                ("customers", &Status::Changed),
                ("fresh", &Status::New),
                ("leftover", &Status::Extra)
            ]
        );
        let sql = script(&d);
        let t = "ALTER TABLE \"staging\".\"customers\"";
        for expected in [
            format!(
                "{t} ALTER COLUMN \"email\" TYPE character varying(120) USING \"email\"::character varying(120);"
            ),
            format!("{t} ALTER COLUMN \"email\" SET NOT NULL;"),
            format!("{t} ADD COLUMN \"active\" boolean DEFAULT true;"),
            format!("{t} DROP COLUMN \"old\";"),
            // The index points at the target schema.
            "CREATE INDEX customers_email_idx ON \"staging\".customers USING btree (email);".to_string(),
            "CREATE TABLE \"staging\".\"t\" ();".to_string(),
            "-- DROP TABLE \"staging\".\"leftover\";".to_string(),
        ] {
            assert!(sql.contains(&expected), "missing {expected}\n{sql}");
        }
        // The same primary key isn't touched.
        assert!(!sql.contains("customers_pkey"), "{sql}");
        assert!(d[0].items.iter().any(|i| i.destructive && i.sql.contains("old")));
    }

    #[test]
    fn schema_names_are_swapped_only_as_names() {
        let k = Kind::Postgres;
        assert_eq!(
            retarget(k, "nextval('a.t_id_seq'::regclass)", "a", "b"),
            "nextval('\"b\".t_id_seq'::regclass)"
        );
        assert_eq!(
            retarget(k, "ON a.t USING btree (a.x)", "a", "b"),
            "ON \"b\".t USING btree (\"b\".x)"
        );
        assert_eq!(retarget(k, "REFERENCES data.t", "a", "b"), "REFERENCES data.t");
        assert_eq!(plain(k, "REFERENCES \"a\".t(id)", "a"), "REFERENCES t(id)");
    }

    #[test]
    fn mysql_reads_show_create_table() {
        let st = TableStructure {
            ddl: "CREATE TABLE `purchase` (\n  `id` int NOT NULL AUTO_INCREMENT,\n  `customer_id` int NOT NULL,\n  `note` text,\n  \
                  PRIMARY KEY (`id`),\n  KEY `customer_id` (`customer_id`),\n  CONSTRAINT `purchase_ibfk_1` FOREIGN KEY (`customer_id`) \
                  REFERENCES `customer` (`id`)\n) ENGINE=InnoDB AUTO_INCREMENT=9 DEFAULT CHARSET=utf8mb4;"
                .into(),
            ..Default::default()
        };
        let a = table_def(Kind::MySql, &st);
        assert_eq!(
            a.columns.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
            ["id", "customer_id", "note"]
        );
        assert_eq!(a.indexes.keys().collect::<Vec<_>>(), ["PRIMARY", "customer_id"]);
        assert!(a.constraints["purchase_ibfk_1"].contains("FOREIGN KEY"));
        let mut b = a.clone();
        b.columns[2].1.line = "`note` varchar(50) DEFAULT NULL".into();
        b.constraints.clear();
        let snap = |t: TableDef, schema: &str| Snapshot {
            schema: schema.into(),
            tables: [("purchase".to_string(), t)].into(),
        };
        let sql = script(&diff(Kind::MySql, &snap(a, "shop"), &snap(b, "shop_test")));
        assert!(
            sql.contains("ALTER TABLE `shop_test`.`purchase` MODIFY COLUMN `note` text;"),
            "{sql}"
        );
        assert!(
            sql.contains("ALTER TABLE `shop_test`.`purchase` ADD CONSTRAINT `purchase_ibfk_1` FOREIGN KEY"),
            "{sql}"
        );
    }

    /// Needs `docker compose --profile test up -d --wait`. Two schemas that differ in every
    /// way handled; the script applied to the target leaves nothing to compare.
    fn converges(kind: Kind, port: u16) {
        use crate::db::ConnConfig;
        let mut cfg = ConnConfig::new(kind);
        cfg.host = "127.0.0.1".into();
        cfg.port = port;
        cfg.password = "pw".into();
        cfg.database = if kind == Kind::Postgres {
            "postgres"
        } else {
            "test"
        }
        .into();
        let mut c = Conn::connect(&cfg, None).expect("connect");
        let run = |c: &mut Conn, sql: &str| {
            for s in crate::sql::statement_ranges(sql, false) {
                let stmt = &sql[s.0..s.1];
                c.query(stmt).unwrap_or_else(|e| panic!("{kind:?}: {stmt}: {e}"));
            }
        };
        let (reset, a, b) = match kind {
            Kind::Postgres => (
                "DROP SCHEMA IF EXISTS cmp_a CASCADE; DROP SCHEMA IF EXISTS cmp_b CASCADE; \
                 CREATE SCHEMA cmp_a; CREATE SCHEMA cmp_b;",
                "CREATE TABLE cmp_a.customer (id serial PRIMARY KEY, email varchar(120) NOT NULL UNIQUE, active boolean DEFAULT true); \
                 CREATE INDEX customer_active_idx ON cmp_a.customer (active); \
                 CREATE TABLE cmp_a.purchase (id int PRIMARY KEY, customer_id int REFERENCES cmp_a.customer (id), total numeric(10,2) CHECK (total >= 0)); \
                 CREATE TABLE cmp_a.fresh (id int PRIMARY KEY, customer_id int REFERENCES cmp_a.customer (id));",
                "CREATE TABLE cmp_b.customer (id serial PRIMARY KEY, email text, old text); \
                 CREATE TABLE cmp_b.purchase (id int PRIMARY KEY, customer_id int, total numeric(8,2)); \
                 CREATE INDEX purchase_total_idx ON cmp_b.purchase (total); \
                 CREATE TABLE cmp_b.leftover (id int);",
            ),
            Kind::MySql => (
                "DROP DATABASE IF EXISTS cmp_a; DROP DATABASE IF EXISTS cmp_b; CREATE DATABASE cmp_a; CREATE DATABASE cmp_b;",
                "CREATE TABLE cmp_a.customer (id int AUTO_INCREMENT PRIMARY KEY, email varchar(120) NOT NULL UNIQUE, active boolean DEFAULT true); \
                 CREATE INDEX customer_active_idx ON cmp_a.customer (active); \
                 CREATE TABLE cmp_a.purchase (id int PRIMARY KEY, customer_id int, total decimal(10,2), \
                 CONSTRAINT purchase_customer FOREIGN KEY (customer_id) REFERENCES cmp_a.customer (id)); \
                 CREATE TABLE cmp_a.fresh (id int PRIMARY KEY);",
                "CREATE TABLE cmp_b.customer (id int AUTO_INCREMENT PRIMARY KEY, email text, old text); \
                 CREATE TABLE cmp_b.purchase (id int PRIMARY KEY, customer_id int, total decimal(8,2)); \
                 CREATE INDEX purchase_total_idx ON cmp_b.purchase (total); \
                 CREATE TABLE cmp_b.leftover (id int);",
            ),
            Kind::Sqlite => unreachable!("server test"),
        };
        run(&mut c, reset);
        run(&mut c, a);
        run(&mut c, b);
        let both = |c: &mut Conn| (snapshot(c, "cmp_a").unwrap(), snapshot(c, "cmp_b").unwrap());
        let (sa, sb) = both(&mut c);
        let d = diff(kind, &sa, &sb);
        assert_eq!(d.len(), 4, "{kind:?}: {d:#?}");
        let sql = script(&d);
        run(&mut c, &sql);
        let (sa, sb) = both(&mut c);
        let left = diff(kind, &sa, &sb);
        // Only the extra table stays (its DROP is commented out).
        assert_eq!(left.len(), 1, "{kind:?}: script was\n{sql}\nleft: {left:#?}");
        assert_eq!(left[0].status, Status::Extra);
        if kind == Kind::Postgres {
            // The new column default and the serial point at the target's own objects.
            let d = c
                .query("SELECT string_agg(column_default, ' ') FROM information_schema.columns WHERE table_schema = 'cmp_b'")
                .unwrap();
            let defaults = d.rows.cell(0, 0).map(str::to_owned).unwrap_or_default();
            assert!(
                defaults.contains("cmp_b.customer_id_seq") && !defaults.contains("cmp_a"),
                "{defaults}"
            );
        }
        run(
            &mut c,
            if kind == Kind::Postgres {
                "DROP SCHEMA cmp_a CASCADE; DROP SCHEMA cmp_b CASCADE;"
            } else {
                "DROP DATABASE cmp_a; DROP DATABASE cmp_b;"
            },
        );
    }

    #[test]
    #[ignore = "needs the test databases: docker compose --profile test up -d --wait"]
    fn converges_postgres() {
        converges(Kind::Postgres, 55432);
    }

    #[test]
    #[ignore = "needs the test databases: docker compose --profile test up -d --wait"]
    fn converges_mysql() {
        converges(Kind::MySql, 53306);
    }

    /// SQLite: two files; what SQLite can change is changed, the rest is said in comments.
    #[test]
    fn sqlite_files_compare() {
        use crate::db::{Conn, ConnConfig};
        let a_cfg = crate::sqlite::tests::sample_db("cmp-a");
        let b_cfg = ConnConfig {
            database: format!("{}-b.db", a_cfg.database),
            ..a_cfg.clone()
        };
        let _ = std::fs::remove_file(&b_cfg.database);
        crate::sqlite::create(&b_cfg.database).unwrap();
        let mut a = Conn::connect(&a_cfg, None).unwrap();
        let mut b = Conn::connect(&b_cfg, None).unwrap();
        b.query(
            "CREATE TABLE customer (id INTEGER PRIMARY KEY, name TEXT, old TEXT);
             CREATE TABLE spare (x INTEGER);",
        )
        .unwrap();
        let d = diff(
            Kind::Sqlite,
            &snapshot(&mut a, "main").unwrap(),
            &snapshot(&mut b, "main").unwrap(),
        );
        let status: Vec<(&str, &Status)> = d.iter().map(|t| (t.table.as_str(), &t.status)).collect();
        assert_eq!(
            status,
            [
                ("customer", &Status::Changed),
                ("purchase", &Status::New),
                ("spare", &Status::Extra)
            ]
        );
        let sql = script(&d);
        assert!(sql.contains("ADD COLUMN \"email\" TEXT"), "{sql}");
        assert!(sql.contains("ADD COLUMN \"active\" BOOLEAN DEFAULT 1"), "{sql}");
        assert!(sql.contains("DROP COLUMN \"old\""), "{sql}");
        // "name" became NOT NULL: SQLite can't, and the script says so instead of failing.
        assert!(sql.contains("-- SQLite can't alter an existing column"), "{sql}");
        b.query(&sql).unwrap_or_else(|e| panic!("{e}\n{sql}"));
        let left = diff(
            Kind::Sqlite,
            &snapshot(&mut a, "main").unwrap(),
            &snapshot(&mut b, "main").unwrap(),
        );
        // Left: the column SQLite can't alter, and the table only the target has.
        let left: Vec<&str> = left.iter().map(|t| t.table.as_str()).collect();
        assert_eq!(left, ["customer", "spare"]);
        let _ = std::fs::remove_file(&a_cfg.database);
        let _ = std::fs::remove_file(&b_cfg.database);
    }

    /// Definitions and schema names of any shape: renaming schemas in them never panics, and
    /// changes nothing when there is nothing to rename.
    #[test]
    fn schema_renames_take_any_text() {
        crate::testgen::each_text(25, 2000, |sql, g| {
            let schema = g.text_of(2);
            for kind in [Kind::Postgres, Kind::MySql, Kind::Sqlite] {
                let _ = (retarget(kind, sql, &schema, "dst"), plain(kind, sql, &schema));
                if !schema.is_empty() && !sql.contains(&format!("{schema}.")) {
                    assert_eq!(plain(kind, sql, &schema), sql);
                }
            }
            let _ = split_name(sql);
        });
    }
}
