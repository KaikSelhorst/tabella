use super::*;

#[test]
fn connections_list_by_group() {
    let c = |name: &str, group: &str| ConnConfig {
        name: name.into(),
        group: group.into(),
        ..ConnConfig::new(Kind::Postgres)
    };
    let saved = [
        c("a", "Prod"),
        c("b", ""),
        c("c", " Staging "),
        c("d", "Prod"),
        c("e", ""),
    ];
    assert_eq!(
        by_group(&saved),
        vec![("", vec![1, 4]), ("Prod", vec![0, 3]), ("Staging", vec![2])]
    );
    assert!(by_group(&[]).is_empty());
}

fn roundtrip(kind: Kind, port: u16, user: &str, pass: &str, db: &str) {
    let cfg = ConnConfig {
        password: pass.into(),
        database: db.into(),
        host: "127.0.0.1".into(),
        port,
        user: user.into(),
        name: "t".into(),
        ..ConnConfig::new(kind)
    };
    let mut c = Conn::connect(&cfg, None).expect("connect");
    // Leftovers from a run that failed half-way would block dropping the table.
    c.query("DROP VIEW IF EXISTS dbl_v").unwrap();
    c.query("DROP TABLE IF EXISTS dbl_t").unwrap();
    c.query("CREATE TABLE dbl_t (id INT PRIMARY KEY, name VARCHAR(50), at TIMESTAMP NULL)")
        .unwrap();
    let r = c
        .query("INSERT INTO dbl_t VALUES (1,'o''neil',NULL),(2,'b\\c','2024-01-02 03:04:05')")
        .unwrap();
    assert_eq!(r.affected, Some(2));
    let r = c.query("SELECT * FROM dbl_t ORDER BY id").unwrap();
    assert_eq!(r.columns, ["id", "name", "at"]);
    assert_eq!(r.rows.cell(0, 1), Some("o'neil"));
    assert_eq!(r.rows.cell(0, 2), None);
    assert_eq!(r.rows.cell(1, 2), Some("2024-01-02 03:04:05"));
    let cols = c
        .columns(if kind == Kind::MySql { db } else { "public" }, "dbl_t")
        .unwrap();
    assert_eq!(
        cols.iter().map(|c| c.pk).collect::<Vec<_>>(),
        [true, false, false]
    );
    let tables = c.tables().unwrap();
    assert!(tables.iter().any(|t| t.name == "dbl_t"));
    assert_eq!(c.count("SELECT COUNT(*) FROM dbl_t").unwrap(), 2);
    let cols = c.all_columns().unwrap();
    assert!(
        cols.iter()
            .any(|c| c.table == "dbl_t" && c.column == "name" && !c.data_type.is_empty())
    );
    // "Search value": finds the row by part of a value, in the right column, and the filter
    // it offers opens exactly there.
    {
        use crate::search::{search_table, targets};
        let (ts, _) = targets(&tables, &cols, "NEIL", true);
        let t = ts.iter().find(|t| t.table == "dbl_t").expect("dbl_t is searched");
        let hit = search_table(&mut c, kind, t, "NEIL", false)
            .unwrap()
            .expect("found");
        assert_eq!(hit.columns, ["name"], "{kind:?}");
        let r = c
            .query(&format!(
                "SELECT count(*) FROM {} WHERE {}",
                kind.qualified(&t.schema, "dbl_t"),
                hit.filter
            ))
            .unwrap();
        assert_eq!(r.rows.cell(0, 0), Some("1"), "{kind:?}");
        assert!(
            search_table(&mut c, kind, t, "o'neil", true).unwrap().is_some(),
            "{kind:?}: exact"
        );
        assert!(
            search_table(&mut c, kind, t, "none-of-this", false)
                .unwrap()
                .is_none()
        );
    }
    let lit = kind.quote_str("x'y");
    let r = c.query(&format!("SELECT {lit}")).unwrap();
    assert_eq!(r.rows.cell(0, 0), Some("x'y"));
    assert!(c.query("SELEC 1").is_err());
    // Booleans written the way the grid writes them, for both engines.
    c.query("DROP TABLE IF EXISTS dbl_b").unwrap();
    c.query("CREATE TABLE dbl_b (id INT PRIMARY KEY, ok BOOLEAN)")
        .unwrap();
    c.query("INSERT INTO dbl_b VALUES (1, NULL)").unwrap();
    let s = if kind == Kind::MySql { db } else { "public" };
    for b in [true, false] {
        let v = crate::sql::bool_value(kind, b);
        let wh = crate::sql::Stmt::new("id = 1");
        c.execute(&crate::sql::update_cell(kind, s, "dbl_b", "ok", Some(v), wh))
            .unwrap();
        let r = c.query("SELECT ok FROM dbl_b").unwrap();
        assert_eq!(r.rows.cell(0, 0).and_then(crate::sql::parse_bool), Some(b));
    }
    let cols = c.columns(s, "dbl_b").unwrap();
    assert!(
        cols[1].data_type == "boolean" || cols[1].data_type == "tinyint(1)",
        "{:?}",
        cols[1]
    );
    assert!(!cols[0].nullable && cols[1].nullable, "{cols:?}");
    assert!(!cols[0].has_default && !cols[1].has_default, "{cols:?}");
    c.query("DROP TABLE dbl_b").unwrap();

    // ENUM columns: their values come with the columns (arrays and SETs of them don't), and
    // a value with a quote or a comma goes in and out unchanged.
    c.query("DROP TABLE IF EXISTS dbl_e").unwrap();
    let ddl = match kind {
        Kind::Postgres => {
            c.query("DROP TYPE IF EXISTS dbl_mood").unwrap();
            c.query("CREATE TYPE dbl_mood AS ENUM ('sad', 'it''s ok', 'happy, really')")
                .unwrap();
            "CREATE TABLE dbl_e (id INT PRIMARY KEY, mood dbl_mood, moods dbl_mood[], note text)"
        }
        _ => {
            "CREATE TABLE dbl_e (id INT PRIMARY KEY, mood ENUM('sad', 'it''s ok', 'happy, really'), \
             moods SET('a', 'b'), note text)"
        }
    };
    c.query(ddl).unwrap();
    c.query("INSERT INTO dbl_e (id) VALUES (1)").unwrap();
    let cols = c.columns(s, "dbl_e").unwrap();
    assert_eq!(
        cols[1].enum_values,
        ["sad", "it's ok", "happy, really"],
        "{kind:?}: {cols:?}"
    );
    assert!(cols[0].enum_values.is_empty() && cols[2].enum_values.is_empty());
    assert!(cols[3].enum_values.is_empty());
    for v in &cols[1].enum_values {
        let wh = crate::sql::Stmt::new("id = 1");
        c.execute(&crate::sql::update_cell(kind, s, "dbl_e", "mood", Some(v), wh))
            .unwrap();
        let r = c.query("SELECT mood FROM dbl_e").unwrap();
        assert_eq!(r.rows.cell(0, 0), Some(v.as_str()), "{kind:?}");
    }
    if kind == Kind::Postgres {
        // The real type names, not information_schema's "USER-DEFINED" / "ARRAY".
        assert_eq!(cols[1].data_type, "dbl_mood");
        assert_eq!(cols[2].data_type, "dbl_mood[]");
        c.query("CREATE EXTENSION IF NOT EXISTS citext").unwrap();
        c.query("ALTER TABLE dbl_e ADD COLUMN email citext").unwrap();
        let cols = c.columns(s, "dbl_e").unwrap();
        assert_eq!(cols[4].data_type, "citext");
        assert!(cols[4].enum_values.is_empty());
    }
    c.query("DROP TABLE dbl_e").unwrap();
    if kind == Kind::Postgres {
        c.query("DROP TYPE dbl_mood").unwrap();
    }

    // Generated statements must store tricky values verbatim, even with the server settings
    // that break naive escaping.
    let risky = match kind {
        Kind::Postgres => "SET standard_conforming_strings = off",
        Kind::MySql => "SET SESSION sql_mode = CONCAT(@@sql_mode, ',NO_BACKSLASH_ESCAPES')",
        Kind::Sqlite => unreachable!("server test"),
    };
    c.query(risky).unwrap();
    let nasty = [r"a\", r"x\'; DROP TABLE dbl_t; --", "it's", r"\n\0", "café 🚀"];
    for v in nasty {
        let wh = crate::sql::pk_where(
            kind,
            &["id".to_string()],
            &["id".to_string()],
            &[Some("1".into())],
        );
        c.execute(&crate::sql::update_cell(kind, s, "dbl_t", "name", Some(v), wh))
            .unwrap();
        let r = c.query("SELECT name FROM dbl_t WHERE id = 1").unwrap();
        assert_eq!(r.rows.cell(0, 0), Some(v), "{kind:?}");
    }
    // The table survived the injection attempt.
    assert_eq!(c.count("SELECT COUNT(*) FROM dbl_t").unwrap(), 2);

    // Affected rows mean "matched": 0 only when the row is really gone (MySQL needs
    // CLIENT_FOUND_ROWS for an unchanged value to still count).
    let one =
        |id: &str| crate::sql::pk_where(kind, &["id".to_string()], &["id".to_string()], &[Some(id.into())]);
    let same = c
        .query("SELECT name FROM dbl_t WHERE id = 1")
        .unwrap()
        .rows
        .cell(0, 0)
        .map(str::to_owned);
    let r = c
        .execute(&crate::sql::update_cell(
            kind,
            s,
            "dbl_t",
            "name",
            same.as_deref(),
            one("1"),
        ))
        .unwrap();
    assert_eq!(r.affected, Some(1), "{kind:?}: same value still matches");
    let r = c
        .execute(&crate::sql::update_cell(
            kind,
            s,
            "dbl_t",
            "name",
            Some("x"),
            one("999"),
        ))
        .unwrap();
    assert_eq!(r.affected, Some(0), "{kind:?}: missing row");

    // A long statement can be stopped from another thread.
    let canceller = c.canceller(&cfg, None);
    let started = std::time::Instant::now();
    let stopper = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(500));
        canceller.cancel().unwrap();
    });
    let sleep = match kind {
        Kind::Postgres => "SELECT pg_sleep(20)",
        Kind::MySql => "SELECT SLEEP(20)",
        Kind::Sqlite => unreachable!("server test"),
    };
    let _ = c.query(sleep); // Postgres: error "canceling statement"; MySQL: returns 1
    stopper.join().unwrap();
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "{kind:?}: cancel took too long"
    );
    assert!(
        c.query("SELECT 1").is_ok(),
        "{kind:?}: connection still usable after cancel"
    );

    // Structure: the generated DDL must recreate the table (renamed) on the same server.
    c.query("CREATE INDEX dbl_t_name_idx ON dbl_t (name)").unwrap();
    let st = c.structure(s, "dbl_t").unwrap();
    assert_eq!(
        st.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
        ["id", "name", "at"]
    );
    assert!(
        st.columns[0].pk && !st.columns[0].nullable && st.columns[1].nullable,
        "{kind:?}"
    );
    assert!(
        st.indexes.iter().any(|(n, _)| n == "dbl_t_name_idx"),
        "{:?}",
        st.indexes
    );
    let _ = c.query("DROP TABLE IF EXISTS dbl_t2");
    c.query(&st.ddl.replace("dbl_t", "dbl_t2"))
        .unwrap_or_else(|e| panic!("{kind:?}: {e}\n{}", st.ddl));
    assert_eq!(c.count("SELECT COUNT(*) FROM dbl_t2").unwrap(), 0);
    c.query("DROP TABLE dbl_t2").unwrap();

    // Single-column foreign keys are found and point at the right place.
    c.query("DROP TABLE IF EXISTS dbl_child").unwrap();
    c.query("CREATE TABLE dbl_child (id INT PRIMARY KEY, parent_id INT REFERENCES dbl_t (id))")
        .unwrap();
    if kind == Kind::MySql {
        // MySQL ignores inline REFERENCES; it needs a table-level constraint.
        c.query(
            "ALTER TABLE dbl_child ADD CONSTRAINT dbl_child_fk FOREIGN KEY (parent_id) REFERENCES dbl_t (id)",
        )
        .unwrap();
    }
    let cols = c.columns_with_fks(s, "dbl_child").unwrap();
    assert_eq!(
        cols[1].references,
        Some(FkRef {
            schema: s.into(),
            table: "dbl_t".into(),
            column: "id".into()
        }),
        "{kind:?}"
    );
    assert_eq!(cols[0].references, None);
    let t = c
        .tables()
        .unwrap()
        .into_iter()
        .find(|t| t.name == "dbl_t")
        .unwrap();
    assert!(t.size.is_some_and(|b| b > 0), "{kind:?}: table size");
    // The ER diagram's edges include it.
    let fks = c.all_fks().unwrap();
    assert!(
        fks.iter().any(|e| e.table == "dbl_child"
            && e.column == "parent_id"
            && e.ref_table == "dbl_t"
            && e.ref_column == "id"),
        "{kind:?}: {fks:?}"
    );
    // And the other way round: dbl_t knows dbl_child points at it.
    let schema_name = if kind == Kind::MySql { db } else { "public" };
    let incoming = c.incoming_fks(schema_name, "dbl_t").unwrap();
    assert!(
        incoming
            .iter()
            .any(|f| f.table == "dbl_child" && f.target == "id"),
        "{kind:?}: {incoming:?}"
    );
    c.query("DROP TABLE dbl_child").unwrap();

    // Errors carry the server's message, not just "db error".
    let err = c
        .query("SELECT * FROM table_that_does_not_exist")
        .unwrap_err()
        .to_string();
    assert!(err.contains("table_that_does_not_exist"), "{kind:?}: {err}");

    // Plans come back as a tree, estimated or measured.
    let p = c.explain("SELECT * FROM dbl_t WHERE id > 0;", false).unwrap();
    assert!(!p.analyzed && p.root.cost.is_some(), "{kind:?}: {p:?}");
    let p = c.explain("SELECT count(*) FROM dbl_t", true).unwrap();
    assert!(p.analyzed, "{kind:?}: {p:?}");
    let mut labels = Vec::new();
    p.root.walk(0, &mut |n, _| labels.push(n.label.to_lowercase()));
    assert!(labels.iter().any(|l| l.contains("dbl_t")), "{kind:?}: {labels:?}");

    // A filter on a column without an index: the plan's full scan suggests one.
    let p = c
        .explain("SELECT * FROM dbl_t WHERE at = '2024-01-02 03:04:05'", false)
        .unwrap();
    let hints = crate::plan::index_hints(&p);
    assert!(
        hints.iter().any(|h| h.table == "dbl_t" && h.columns == ["at"]),
        "{kind:?}: {hints:?} from {p:?}"
    );

    // A script keeps the result set of each query, not only the last one.
    let r = c
        .query("SELECT 1 AS a; SELECT 2 AS b, 3 AS c; SELECT 4 AS d")
        .unwrap();
    assert_eq!(r.columns, ["d"], "{kind:?}");
    let earlier: Vec<_> = r.previous.iter().map(|p| p.columns.join(",")).collect();
    assert_eq!(earlier, ["a", "b,c"], "{kind:?}");

    // The activity monitor sees another session, and can stop its statement.
    let mut other = Conn::connect(&cfg, None).unwrap();
    let canceller_target = std::thread::spawn(move || {
        let started = std::time::Instant::now();
        let _ = other.query(match kind {
            Kind::Postgres => "SELECT pg_sleep(20)",
            Kind::MySql => "SELECT SLEEP(20)",
            Kind::Sqlite => unreachable!("server test"),
        });
        started.elapsed()
    });
    std::thread::sleep(std::time::Duration::from_millis(700));
    let act = c.activity().unwrap();
    let q = act.columns.iter().position(|c| c == "query").unwrap();
    let sleeper = act
        .rows
        .iter()
        .find(|r| r.cell(q).is_some_and(|t| t.to_lowercase().contains("sleep(20)")))
        .unwrap_or_else(|| panic!("{kind:?}: sleeping session not listed: {act:?}"));
    let id: i64 = sleeper.cell(0).unwrap().parse().unwrap();
    c.kill(id, false).unwrap();
    // A session that doesn't exist is reported, not silently ignored.
    assert!(
        c.kill(999_999, false).is_err(),
        "{kind:?}: kill of a missing session"
    );
    assert!(
        canceller_target.join().unwrap() < std::time::Duration::from_secs(5),
        "{kind:?}: kill"
    );

    // Locks: a session holding a row lock shows up as blocking another one.
    {
        let mut holder = Conn::connect(&cfg, None).unwrap();
        holder.begin().unwrap();
        holder
            .query("UPDATE dbl_t SET name = 'locker' WHERE id = 1")
            .unwrap();
        let mut waiter = Conn::connect(&cfg, None).unwrap();
        let blocked = std::thread::spawn(move || {
            let _ = waiter.query("UPDATE dbl_t SET name = 'waiter' WHERE id = 1");
        });
        std::thread::sleep(std::time::Duration::from_millis(800));
        let locks = c.activity_view(ActivityView::Locks).unwrap();
        assert_eq!(locks.rows.len(), 1, "{kind:?}: {locks:?}");
        let blocker: i64 = locks.rows.cell(0, 0).unwrap().parse().unwrap();
        c.kill(blocker, true).unwrap();
        blocked.join().unwrap();
        drop(holder);
    }
    // Slow queries: MySQL always has them; the test Postgres preloads pg_stat_statements.
    if kind == Kind::Postgres {
        c.query("CREATE EXTENSION IF NOT EXISTS pg_stat_statements")
            .unwrap();
    }
    let slow = c.activity_view(ActivityView::Slow).unwrap();
    assert!(
        slow.columns.contains(&"query".to_string()) && !slow.rows.is_empty(),
        "{kind:?}"
    );
    c.reset_query_stats().unwrap();

    // Functions, triggers, views (and Postgres sequences): listed, and their code comes back.
    {
        let schema = if kind == Kind::MySql { db } else { "public" };
        c.query("DROP VIEW IF EXISTS dbl_v").unwrap();
        c.query("CREATE VIEW dbl_v AS SELECT id FROM dbl_t").unwrap();
        match kind {
            Kind::Postgres => {
                c.query("CREATE OR REPLACE FUNCTION dbl_f(a integer) RETURNS integer LANGUAGE sql AS $$ SELECT a + 1; $$").unwrap();
                c.query("CREATE OR REPLACE FUNCTION dbl_tf() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END; $$").unwrap();
                c.query("DROP TRIGGER IF EXISTS dbl_tr ON dbl_t").unwrap();
                c.query(
                    "CREATE TRIGGER dbl_tr BEFORE UPDATE ON dbl_t FOR EACH ROW EXECUTE FUNCTION dbl_tf()",
                )
                .unwrap();
                c.query("CREATE SEQUENCE IF NOT EXISTS dbl_seq START 7").unwrap();
            }
            Kind::MySql => {
                c.query("DROP FUNCTION IF EXISTS dbl_f").unwrap();
                c.query("CREATE FUNCTION dbl_f(a INT) RETURNS INT DETERMINISTIC RETURN a + 1")
                    .unwrap();
                c.query("DROP TRIGGER IF EXISTS dbl_tr").unwrap();
                c.query("CREATE TRIGGER dbl_tr BEFORE UPDATE ON dbl_t FOR EACH ROW SET NEW.name = NEW.name")
                    .unwrap();
            }
            Kind::Sqlite => unreachable!("server test"),
        }
        let objs = c.objects().unwrap();
        let find = |name: &str| {
            objs.iter()
                .find(|o| o.schema == schema && o.name == name)
                .unwrap_or_else(|| panic!("{kind:?}: {name} in {objs:?}"))
        };
        assert_eq!(find("dbl_f").kind, ObjKind::Function);
        assert_eq!(find("dbl_tr").table, "dbl_t");
        assert_eq!(find("dbl_v").kind, ObjKind::View);
        let src = c.object_source(find("dbl_f")).unwrap();
        assert!(src.contains("dbl_f") && src.contains("a + 1"), "{kind:?}: {src}");
        // "Run…": the call built from the listed arguments, with the value filled in.
        {
            let f = find("dbl_f");
            let args = crate::sql::parse_args(&f.args);
            assert_eq!(args.len(), 1, "{kind:?}: {}", f.args);
            let call = crate::sql::call_template(kind, &f.schema, &f.name, &args, false);
            let sql = crate::sql::bind_params(kind, &call, |_| "41".into());
            let r = c.query(&sql).unwrap_or_else(|e| panic!("{kind:?}: {e}\n{sql}"));
            assert_eq!(r.rows.cell(0, 0), Some("42"), "{kind:?}: {sql}");
        }
        assert!(
            c.object_source(find("dbl_tr")).unwrap().contains("dbl_t"),
            "{kind:?}"
        );
        assert!(
            c.object_source(find("dbl_v"))
                .unwrap()
                .contains("CREATE OR REPLACE"),
            "{kind:?}"
        );
        if kind == Kind::Postgres {
            let src = c.object_source(find("dbl_seq")).unwrap();
            assert!(src.contains("START WITH 7"), "{src}");
            // The function's own code runs again as it came back.
            let again = c.object_source(find("dbl_f")).unwrap();
            c.query(&again).unwrap();
            c.query("DROP SEQUENCE dbl_seq").unwrap();
            c.query("DROP TRIGGER dbl_tr ON dbl_t").unwrap();
            c.query("DROP FUNCTION dbl_tf()").unwrap();
        } else {
            c.query("DROP TRIGGER dbl_tr").unwrap();
        }
        c.query("DROP FUNCTION dbl_f")
            .unwrap_or_else(|_| c.query("DROP FUNCTION dbl_f(integer)").unwrap());
        c.query("DROP VIEW dbl_v").unwrap();
        // Primary keys come with the column list.
        let cols = c.all_columns().unwrap();
        assert!(
            cols.iter()
                .any(|c| c.table == "dbl_t" && c.column == "id" && c.pk),
            "{kind:?}"
        );
        assert!(
            cols.iter()
                .any(|c| c.table == "dbl_t" && c.column == "name" && !c.pk),
            "{kind:?}"
        );
    }

    // Table actions: duplicate with rows, rename, empty, drop.
    {
        use crate::sql::{TableAction, table_action};
        let schema = if kind == Kind::MySql { db } else { "public" };
        c.query("DROP TABLE IF EXISTS dbl_copy").unwrap();
        c.query("DROP TABLE IF EXISTS dbl_copy2").unwrap();
        let count = |c: &mut Conn, t: &str| {
            c.query(&format!("SELECT count(*) FROM {t}"))
                .unwrap()
                .rows
                .cell(0, 0)
                .map(str::to_owned)
                .unwrap()
        };
        let n = count(&mut c, "dbl_t");
        c.query(&table_action(
            kind,
            schema,
            "dbl_t",
            TableAction::Duplicate(true),
            "dbl_copy",
        ))
        .unwrap();
        assert_eq!(count(&mut c, "dbl_copy"), n, "{kind:?}: rows copied");
        c.query(&table_action(
            kind,
            schema,
            "dbl_copy",
            TableAction::Rename,
            "dbl_copy2",
        ))
        .unwrap();
        c.query(&table_action(
            kind,
            schema,
            "dbl_copy2",
            TableAction::Truncate,
            "",
        ))
        .unwrap();
        assert_eq!(count(&mut c, "dbl_copy2"), "0", "{kind:?}");
        c.query(&table_action(kind, schema, "dbl_copy2", TableAction::Drop, ""))
            .unwrap();
        assert!(c.query("SELECT 1 FROM dbl_copy2").is_err(), "{kind:?}: dropped");
    }

    // Switching where unqualified names go.
    {
        let (dbs, here, schema) = c.location().unwrap();
        assert!(dbs.contains(&here), "{kind:?}: {dbs:?} / {here}");
        match kind {
            Kind::Postgres => {
                assert_eq!(schema, "public");
                c.query("CREATE SCHEMA IF NOT EXISTS dbl_other").unwrap();
                c.query("CREATE TABLE IF NOT EXISTS dbl_other.only_here (x int)")
                    .unwrap();
                c.use_schema("dbl_other").unwrap();
                assert_eq!(c.location().unwrap().2, "dbl_other");
                c.query("SELECT * FROM only_here").unwrap();
                // public still answers after it.
                c.query("SELECT * FROM dbl_t").unwrap();
                c.use_schema("public").unwrap();
                assert!(c.query("SELECT * FROM only_here").is_err());
                c.query("DROP SCHEMA dbl_other CASCADE").unwrap();
            }
            Kind::MySql => {
                c.query("CREATE DATABASE IF NOT EXISTS dbl_other").unwrap();
                c.query("CREATE TABLE IF NOT EXISTS dbl_other.only_here (x int)")
                    .unwrap();
                c.use_schema("dbl_other").unwrap();
                assert_eq!(c.location().unwrap().1, "dbl_other");
                c.query("SELECT * FROM only_here").unwrap();
                c.use_schema(db).unwrap();
                c.query("DROP DATABASE dbl_other").unwrap();
            }
            Kind::Sqlite => unreachable!("server test"),
        }
    }

    // Server overview: every fact present, and the test table among the biggest.
    let info = c.server_info().unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(info.facts.len(), 5, "{kind:?}: {:?}", info.facts);
    assert!(
        info.facts[0]
            .1
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_digit()),
        "{kind:?}: version {:?}",
        info.facts
    );
    assert!(!info.largest.rows.is_empty(), "{kind:?}");
    // dbl_child's index backs its foreign key; it is listed, and says so.
    let ix = &info.indexes;
    assert!(ix.columns.contains(&"scans".to_string()), "{kind:?}: {ix:?}");

    // A per-connection timeout stops long statements on the server.
    let timed = ConnConfig {
        timeout_secs: 1,
        ..cfg.clone()
    };
    let mut tc = Conn::connect(&timed, None).expect("connect with timeout");
    let started = std::time::Instant::now();
    let _ = tc.query(match kind {
        Kind::Postgres => "SELECT pg_sleep(5)",
        Kind::MySql => "SELECT SLEEP(5)",
        Kind::Sqlite => unreachable!("server test"),
    });
    assert!(
        started.elapsed() < std::time::Duration::from_secs(3),
        "{kind:?}: timeout not applied"
    );

    // Batches are all-or-nothing: a missing row rolls back the edits before it.
    let set = |id: &str, v: &str| {
        crate::sql::update_cell(
            kind,
            s,
            "dbl_t",
            "name",
            Some(v),
            crate::sql::pk_where(kind, &["id".to_string()], &["id".to_string()], &[Some(id.into())]),
        )
    };
    c.run_batch(&[set("1", "batch-1"), set("2", "batch-2")], false)
        .unwrap();
    let names = c.query("SELECT name FROM dbl_t ORDER BY id").unwrap();
    assert_eq!(
        names
            .rows
            .iter()
            .map(|r| r.cell(0).unwrap().to_owned())
            .collect::<Vec<_>>(),
        ["batch-1", "batch-2"]
    );
    let err = c
        .run_batch(&[set("1", "not-saved"), set("999", "x")], false)
        .unwrap_err();
    assert!(err.to_string().contains("Nothing was saved"), "{err}");
    let r = c.query("SELECT name FROM dbl_t WHERE id = 1").unwrap();
    assert_eq!(
        r.rows.cell(0, 0),
        Some("batch-1"),
        "{kind:?}: first edit rolled back"
    );

    // Read-only connections are enforced by the server.
    let ro = ConnConfig {
        read_only: true,
        ..cfg
    };
    let mut r = Conn::connect(&ro, None).expect("read-only connect");
    assert_eq!(r.count("SELECT COUNT(*) FROM dbl_t").unwrap(), 2);
    assert!(
        r.query("DELETE FROM dbl_t").is_err(),
        "write must fail on a read-only session"
    );
    assert_eq!(c.count("SELECT COUNT(*) FROM dbl_t").unwrap(), 2);
    // Preview of writes: rewritten as reads, or (Postgres, joined form) run and undone.
    {
        let p = c
            .preview("UPDATE dbl_t SET name = 'x' WHERE id = 1", false)
            .unwrap();
        assert_eq!((p.count, p.sample.rows.len(), p.after), (1, 1, false), "{kind:?}");
        let p = c.preview("DELETE FROM dbl_t", false).unwrap();
        assert!(p.count >= 2, "{kind:?}");
        let joined = "UPDATE dbl_t SET name = 'changed' FROM (SELECT 1 AS one) o WHERE dbl_t.id = o.one";
        if kind == Kind::Postgres {
            let p = c.preview(joined, false).unwrap();
            assert_eq!((p.count, p.after), (1, true));
            assert_eq!(
                p.sample.rows.cell(0, 1),
                Some("changed"),
                "shows the values after"
            );
            let r = c.query("SELECT name FROM dbl_t WHERE id = 1").unwrap();
            assert_ne!(r.rows.cell(0, 0), Some("changed"), "and undoes it");
        } else {
            assert!(
                c.preview(
                    "UPDATE dbl_t JOIN (SELECT 1 AS one) o ON dbl_t.id = o.one SET name = 'x'",
                    false
                )
                .is_err()
            );
        }
    }

    // Table designer: create, then alter, through real DDL.
    {
        use crate::design::{ColDef, Design, IndexDef};
        let schema = if kind == Kind::MySql { db } else { "public" };
        c.query("DROP TABLE IF EXISTS dbl_design").unwrap();
        let mut d = Design::new_table(kind, schema);
        d.table = "dbl_design".into();
        d.columns.push(ColDef {
            name: "name".into(),
            data_type: "varchar(50)".into(),
            ..ColDef::new()
        });
        d.indexes.push(IndexDef {
            existing: false,
            name: String::new(),
            columns: "name".into(),
            unique: false,
            dropped: false,
        });
        c.query(&d.script())
            .unwrap_or_else(|e| panic!("{kind:?}: {e}\n{}", d.script()));
        let st = c.structure(schema, "dbl_design").unwrap();
        let mut d = Design::from_structure(kind, schema, "dbl_design", &st);
        assert!(d.statements().is_empty(), "{kind:?}: {:?}", d.statements());
        assert_eq!(
            d.indexes.len(),
            1,
            "{kind:?}: only the non-key index: {:?}",
            d.indexes
        );
        let name_col = d.columns.iter_mut().find(|c| c.name == "name").unwrap();
        name_col.name = "full_name".into();
        name_col.nullable = false;
        name_col.default = "'x'".into();
        d.indexes[0].dropped = true;
        d.columns.push(ColDef {
            name: "active".into(),
            data_type: "boolean".into(),
            ..ColDef::new()
        });
        c.query(&d.script())
            .unwrap_or_else(|e| panic!("{kind:?}: {e}\n{}", d.script()));
        let st = c.structure(schema, "dbl_design").unwrap();
        let names: Vec<&str> = st.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["id", "full_name", "active"], "{kind:?}");
        assert!(!st.columns[1].nullable, "{kind:?}");
        c.query("DROP TABLE dbl_design").unwrap();
    }

    // Column profile through the database, with and without min/max.
    {
        let schema = if kind == Kind::MySql { db } else { "public" };
        let (sum, top) = crate::sql::column_stats(kind, schema, "dbl_t", "id", "", true);
        let st = crate::values::stats_from_queries(&c.query(&sum).unwrap(), &c.query(&top).unwrap()).unwrap();
        assert!(
            st.total >= 2 && st.nulls == 0 && st.distinct == st.total,
            "{kind:?}: {st:?}"
        );
        assert!(st.min.is_some() && st.max.is_some());
        let (sum, top) = crate::sql::column_stats(kind, schema, "dbl_t", "at", "id > 0", false);
        let st = crate::values::stats_from_queries(&c.query(&sum).unwrap(), &c.query(&top).unwrap()).unwrap();
        assert!(
            st.nulls >= 1 && st.top.iter().any(|(v, _)| v.is_none()),
            "{kind:?}: {st:?}"
        );
    }

    // CSV import: all rows in one transaction, or none when one fails.
    {
        use crate::import::{ImportSpec, run};
        let path = std::env::temp_dir().join(format!("tabella-import-{kind:?}-{}.csv", std::process::id()));
        std::fs::write(&path, "code;name\n10;ten\n11;\n").unwrap();
        let schema = if kind == Kind::MySql { db } else { "public" };
        let mut spec = ImportSpec {
            path: path.clone(),
            kind,
            schema: schema.into(),
            table: "dbl_t".into(),
            delimiter: b';',
            has_header: true,
            mapping: vec![Some("id".into()), Some("name".into())],
            bool_cols: vec![],
            number_cols: vec!["id".into()],
            empty_is_null: true,
        };
        assert_eq!(run(&mut c, &spec, false).unwrap(), 2, "{kind:?}");
        let r = c.query("SELECT name FROM dbl_t WHERE id = 11").unwrap();
        assert_eq!(r.rows.cell(0, 0), None, "{kind:?}: empty cell is NULL");
        std::fs::write(&path, "code;name\n12;twelve\n10;repeated\n").unwrap();
        let err = run(&mut c, &spec, false).unwrap_err().to_string();
        assert!(err.contains("Nothing was imported"), "{kind:?}: {err}");
        let r = c.query("SELECT count(*) FROM dbl_t WHERE id = 12").unwrap();
        assert_eq!(r.rows.cell(0, 0), Some("0"), "{kind:?}: rolled back");
        spec.mapping = vec![None, None];
        assert!(run(&mut c, &spec, false).is_err());
        let _ = std::fs::remove_file(path);

        // The same from Excel: id, name and a date-time column.
        let path = crate::import::tests::xlsx(&format!("{kind:?}"));
        let spec = ImportSpec {
            path: path.clone(),
            delimiter: 0,
            mapping: vec![
                Some("id".into()),
                Some("name".into()),
                None,
                None,
                None,
                Some("at".into()),
            ],
            ..spec
        };
        assert_eq!(run(&mut c, &spec, false).unwrap(), 2, "{kind:?}");
        let r = c
            .query("SELECT name, at FROM dbl_t WHERE id IN (20, 21) ORDER BY id")
            .unwrap();
        assert_eq!(r.rows.cell(0, 0), Some("Zoë Brontë"), "{kind:?}");
        assert!(
            r.rows
                .cell(0, 1)
                .is_some_and(|v| v.starts_with("2026-10-14 13:05:00")),
            "{kind:?}: {:?}",
            r.rows
        );
        assert_eq!(r.rows.row(1).to_vec(), [None, None], "{kind:?}");
        let _ = std::fs::remove_file(path);
    }
    // "Filter values": the list and the condition it builds run as they are.
    {
        let schema = if kind == Kind::MySql { db } else { "public" };
        let expr = kind.quote_ident("name");
        let list = c
            .query(&crate::sql::distinct_values(kind, schema, "dbl_t", &expr, "", ""))
            .unwrap();
        let values: Vec<Option<String>> = list.rows.iter().map(|r| r.cell(0).map(str::to_owned)).collect();
        assert!(values.len() >= 2, "{kind:?}: {values:?}");
        let (kept, dropped) = values.split_at(1);
        let cond = crate::sql::values_condition(kind, &expr, kept, dropped, true).unwrap();
        let n = c
            .query(&format!("SELECT count(*) FROM dbl_t WHERE {cond}"))
            .unwrap();
        assert_eq!(n.rows.cell(0, 0), list.rows.cell(0, 1), "{kind:?}: {cond}");
    }
    // The foreign-key picker's queries run as they are (the MySQL time-limit hint too).
    {
        let schema = if kind == Kind::MySql { db } else { "public" };
        let fk = FkRef {
            schema: schema.into(),
            table: "dbl_t".into(),
            column: "id".into(),
        };
        for (local, search) in [(true, ""), (false, ""), (false, "1"), (false, "x")] {
            let sql = crate::sql::fk_options(kind, &fk, Some("name"), true, local, search);
            c.query(&sql).unwrap_or_else(|e| panic!("{kind:?}: {sql}: {e}"));
        }
    }
    // Errors say where they are: a typo in a keyword, a column, a table.
    let pos = |c: &mut Conn, sql: &str| c.query(sql).expect_err(sql).position_in(sql);
    let at = |c: &mut Conn, sql: &str| {
        pos(c, sql).map(|i| sql[i..].split_whitespace().next().unwrap_or("").to_string())
    };
    let third_line = "SELECT id\nFROM dbl_t\nORDER id";
    assert_eq!(pos(&mut c, third_line), third_line.rfind("id"), "{kind:?}");
    assert_eq!(
        at(&mut c, "SELECT id, nmae FROM dbl_t").as_deref(),
        Some("nmae"),
        "{kind:?}"
    );
    assert_eq!(
        at(&mut c, "SELECT * FROM dbl_nothing").as_deref(),
        Some("dbl_nothing"),
        "{kind:?}"
    );
    c.query("DROP TABLE dbl_t").unwrap();
}

#[test]
#[ignore = "needs the test databases: docker compose --profile test up -d --wait"]
fn postgres() {
    roundtrip(Kind::Postgres, 55432, "postgres", "pw", "postgres");
}

#[test]
#[ignore = "needs the test databases: docker compose --profile test up -d --wait"]
fn mysql() {
    roundtrip(Kind::MySql, 53306, "root", "pw", "test");
}

/// Needs a TLS-enabled Postgres with a self-signed certificate on port 55433.
#[test]
#[ignore = "needs the test databases: docker compose --profile test up -d --wait"]
fn postgres_tls() {
    let mut cfg = ConnConfig::new(Kind::Postgres);
    cfg.host = "localhost".into();
    cfg.port = 55433;
    cfg.password = "pw".into();
    cfg.ssl = true;
    assert!(
        Conn::connect(&cfg, None).is_err(),
        "self-signed cert must be rejected"
    );
    cfg.ssl_insecure = true;
    let mut c = Conn::connect(&cfg, None).expect("insecure tls connect");
    let r = c
        .query("SELECT ssl FROM pg_stat_ssl WHERE pid = pg_backend_pid()")
        .unwrap();
    assert_eq!(r.rows.cell(0, 0), Some("t"));

    // With TLS not required, a TLS-capable server is still used encrypted (sslmode=prefer).
    cfg.ssl = false;
    let mut c = Conn::connect(&cfg, None).expect("prefer connect");
    let r = c
        .query("SELECT ssl FROM pg_stat_ssl WHERE pid = pg_backend_pid()")
        .unwrap();
    assert_eq!(r.rows.cell(0, 0), Some("t"));
}

#[test]
#[ignore = "needs the test databases: docker compose --profile test up -d --wait"]
fn mysql_tls() {
    let mut cfg = ConnConfig::new(Kind::MySql);
    cfg.host = "127.0.0.1".into();
    cfg.port = 53306;
    cfg.password = "pw".into();
    cfg.ssl = true;
    cfg.ssl_insecure = true;
    let mut c = Conn::connect(&cfg, None).expect("insecure tls connect");
    let r = c.query("SHOW STATUS LIKE 'Ssl_cipher'").unwrap();
    assert!(!r.rows.cell(0, 1).unwrap_or("").is_empty());
}

/// A single read is fetched through a cursor: only the rows kept come over, in or out of an
/// open transaction, and an error still points into the user's SQL.
#[test]
#[ignore = "needs the test databases: docker compose --profile test up -d --wait"]
fn postgres_reads_through_a_cursor() {
    let cfg = ConnConfig {
        password: "pw".into(),
        database: "postgres".into(),
        host: "127.0.0.1".into(),
        port: 55432,
        user: "postgres".into(),
        ..ConnConfig::new(Kind::Postgres)
    };
    let mut c = Conn::connect(&cfg, None).expect("connect");
    let big = "SELECT g, repeat('x', 100) FROM generate_series(1, 200000) g -- the rest";
    let r = c.query_up_to(big, 1000).unwrap();
    assert_eq!((r.rows.len(), r.truncated), (1000, true));
    assert_eq!(r.rows.cell(999, 0), Some("1000"));
    let r = c.query_up_to("SELECT 1 AS a UNION ALL SELECT 2;", 2).unwrap();
    assert_eq!(
        (r.columns.as_slice(), r.rows.len(), r.truncated),
        (&["a".to_string()][..], 2, false)
    );
    let r = c.query("SELECT 1 WHERE false").unwrap();
    assert_eq!((r.columns.len(), r.rows.len()), (1, 0));

    // Inside the user's transaction: sees its writes, and leaves it open.
    c.query("BEGIN").unwrap();
    c.query("CREATE TEMP TABLE dbl_cur (x int)").unwrap();
    c.query("INSERT INTO dbl_cur VALUES (1), (2), (3)").unwrap();
    assert_eq!(c.query_up_to("TABLE dbl_cur", 2).unwrap().rows.len(), 2);
    c.query("ROLLBACK").unwrap();
    assert!(
        c.query("TABLE dbl_cur").is_err(),
        "rolled back with the user's transaction"
    );

    let sql = "  SELECT nope FROM generate_series(1, 3) g";
    let e = c.query(sql).unwrap_err();
    assert_eq!(e.position_in(sql), sql.find("nope"));
    // Still usable after the error.
    assert_eq!(c.count("SELECT count(*) FROM generate_series(1, 7)").unwrap(), 7);
}

/// The grid's writes (insert, cell update, delete by key) and the export's INSERTs, with random
/// values into a table whose names need quoting, end up exactly where a plain model says:
/// quoting and escaping never change a value or reach past the row.
fn edits_match_a_model(c: &mut Conn, kind: Kind, schema: &str) {
    let table = "dbl edit \"q\" `b` 'a';--é";
    let cols = ["a \"q\"", "b `bt`", "c 'é;--"];
    let t = kind.qualified(schema, table);
    let _ = c.query(&format!("DROP TABLE IF EXISTS {t}"));
    let defs: Vec<String> = cols
        .iter()
        .map(|n| format!("{} TEXT", kind.quote_ident(n)))
        .collect();
    c.query(&format!(
        "CREATE TABLE {t} (id INT PRIMARY KEY, {})",
        defs.join(", ")
    ))
    .unwrap();
    let names: Vec<String> = std::iter::once("id".to_string())
        .chain(cols.iter().map(|s| s.to_string()))
        .collect();
    let key = |id: i64| crate::sql::pk_where(kind, &["id".into()], &names, &[Some(id.to_string())]);
    let mut g = crate::testgen::Gen::new(23);
    let mut model: std::collections::BTreeMap<i64, Vec<Option<String>>> = Default::default();
    let value = |g: &mut crate::testgen::Gen| (g.below(5) != 0).then(|| g.text_of(3));
    for step in 0..150 {
        match (g.below(4), model.keys().nth(g.below(model.len().max(1))).copied()) {
            (0, Some(id)) => {
                let col = g.below(cols.len());
                let v = value(&mut g);
                let stmt = crate::sql::update_cell(kind, schema, table, cols[col], v.as_deref(), key(id));
                assert_eq!(c.execute(&stmt).unwrap().affected, Some(1), "step {step}");
                model.get_mut(&id).unwrap()[col] = v;
            }
            (1, Some(id)) => {
                let stmt = crate::sql::delete_row(kind, schema, table, key(id));
                assert_eq!(c.execute(&stmt).unwrap().affected, Some(1), "step {step}");
                model.remove(&id);
            }
            _ => {
                let id = step as i64;
                let row: Vec<Option<String>> = cols.iter().map(|_| value(&mut g)).collect();
                let values: Vec<(String, Option<String>)> =
                    std::iter::once(("id".to_string(), Some(id.to_string())))
                        .chain(cols.iter().map(|n| n.to_string()).zip(row.iter().cloned()))
                        .collect();
                let stmt = crate::sql::insert_values(kind, schema, table, &values);
                assert_eq!(c.execute(&stmt).unwrap().affected, Some(1), "step {step}");
                model.insert(id, row);
            }
        }
    }
    let quoted: Vec<String> = cols.iter().map(|n| kind.quote_ident(n)).collect();
    let read = |c: &mut Conn| {
        c.query(&format!("SELECT {} FROM {t} ORDER BY id", quoted.join(", ")))
            .unwrap()
            .rows
    };
    let want: Rows =
        model
            .values()
            .map(|r| r.iter().map(|v| v.as_deref()))
            .fold(Rows::default(), |mut rows, r| {
                rows.push(r);
                rows
            });
    assert_eq!(read(c), want, "{kind:?}: grid writes");

    // The same rows as an export's INSERTs (literals, not parameters), into an empty table.
    c.query(&format!("DELETE FROM {t}")).unwrap();
    for (id, row) in &model {
        let id = id.to_string();
        let sql = crate::sql::insert_row(
            kind,
            &t,
            &names,
            std::iter::once(Some(id.as_str())).chain(row.iter().map(|v| v.as_deref())),
        );
        c.query(&sql).unwrap_or_else(|e| panic!("{kind:?}: {e}\n{sql}"));
    }
    assert_eq!(read(c), want, "{kind:?}: exported INSERTs");
    c.query(&format!("DROP TABLE {t}")).unwrap();
}

#[test]
fn sqlite_edits_match_a_model() {
    let cfg = crate::sqlite::tests::sample_db("edits-model");
    let mut c = Conn::connect(&cfg, None).unwrap();
    edits_match_a_model(&mut c, Kind::Sqlite, "main");
    let _ = std::fs::remove_file(&cfg.database);
}

#[test]
#[ignore = "needs the test databases: docker compose --profile test up -d --wait"]
fn postgres_edits_match_a_model() {
    let cfg = ConnConfig {
        password: "pw".into(),
        database: "postgres".into(),
        host: "127.0.0.1".into(),
        port: 55432,
        user: "postgres".into(),
        ..ConnConfig::new(Kind::Postgres)
    };
    edits_match_a_model(&mut Conn::connect(&cfg, None).unwrap(), Kind::Postgres, "public");
}

#[test]
#[ignore = "needs the test databases: docker compose --profile test up -d --wait"]
fn mysql_edits_match_a_model() {
    let cfg = ConnConfig {
        password: "pw".into(),
        database: "test".into(),
        host: "127.0.0.1".into(),
        port: 53306,
        user: "root".into(),
        ..ConnConfig::new(Kind::MySql)
    };
    edits_match_a_model(&mut Conn::connect(&cfg, None).unwrap(), Kind::MySql, "test");
}

/// The usual ways a connection fails get their plain reason, from a real server.
#[test]
#[ignore = "needs the test databases: docker compose --profile test up -d --wait"]
fn connection_failures_explained() {
    let cfg = |port: u16, password: &str, database: &str| ConnConfig {
        password: password.into(),
        database: database.into(),
        host: "127.0.0.1".into(),
        port,
        user: "postgres".into(),
        ..ConnConfig::new(Kind::Postgres)
    };
    let why = |c: ConnConfig| Conn::connect(&c, None).err().and_then(|e| e.plain_reason());
    assert_eq!(
        why(cfg(55432, "wrong", "postgres")),
        Some("Wrong user or password.")
    );
    assert_eq!(
        why(cfg(55432, "pw", "no_such_db")),
        Some("That database doesn't exist on the server.")
    );
    assert!(
        why(cfg(1, "pw", "postgres")).is_some_and(|w| w.contains("port")),
        "nothing listens on port 1"
    );
    let mut my = cfg(53306, "wrong", "test");
    my.kind = Kind::MySql;
    my.user = "root".into();
    assert_eq!(why(my), Some("Wrong user or password."));
}

/// MySQL through an SSH tunnel only knows 127.0.0.1: the server's name can't be checked, but its
/// certificate still must come from a trusted CA (not "accept invalid" in disguise).
#[test]
fn mysql_tls_through_a_tunnel_still_checks_the_certificate() {
    let ssl = |cfg: &ConnConfig, via| mysql::Opts::from(my_opts(cfg, via)).get_ssl_opts().cloned();
    let mut cfg = ConnConfig {
        host: "db.example.com".into(),
        ..ConnConfig::new(Kind::MySql)
    };
    assert!(ssl(&cfg, None).is_none());
    cfg.ssl = true;
    let direct = ssl(&cfg, None).unwrap();
    assert!(!direct.skip_domain_validation() && !direct.accept_invalid_certs());
    let tunneled = ssl(&cfg, Some(40000)).unwrap();
    assert!(tunneled.skip_domain_validation() && !tunneled.accept_invalid_certs());
    cfg.ssl_insecure = true;
    assert!(ssl(&cfg, None).unwrap().accept_invalid_certs());
}

/// The object list on a busy server: other sessions creating and dropping tables (with their
/// sequences and views) meanwhile never fail it.
#[test]
#[ignore = "needs the test databases: docker compose --profile test up -d --wait"]
fn postgres_objects_while_others_drop_tables() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let cfg = ConnConfig {
        password: "pw".into(),
        database: "postgres".into(),
        host: "127.0.0.1".into(),
        port: 55432,
        user: "postgres".into(),
        ..ConnConfig::new(Kind::Postgres)
    };
    let mut c = Conn::connect(&cfg, None).expect("connect");
    c.query("DROP SCHEMA IF EXISTS dbl_churn CASCADE").unwrap();
    c.query("CREATE SCHEMA dbl_churn").unwrap();
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let churn = {
        let stop = stop.clone();
        std::thread::spawn(move || {
            let mut c = Conn::connect(&cfg, None).expect("connect");
            let mut i = 0u64;
            while !stop.load(Ordering::Relaxed) {
                c.query(&format!(
                    "CREATE TABLE dbl_churn.t{i} (id serial PRIMARY KEY); \
                     CREATE VIEW dbl_churn.v{i} AS SELECT id FROM dbl_churn.t{i}"
                ))
                .unwrap();
                // Dropped in a transaction that takes a while: a list started meanwhile still
                // sees the objects, and would wait for them and find them gone.
                c.query(&format!(
                    "BEGIN; DROP TABLE dbl_churn.t{i} CASCADE; SELECT pg_sleep(0.05); COMMIT"
                ))
                .unwrap();
                i += 1;
            }
            i
        })
    };
    let started = std::time::Instant::now();
    let mut lists = 0;
    while started.elapsed() < std::time::Duration::from_secs(3) {
        c.objects().unwrap_or_else(|e| panic!("after {lists} lists: {e}"));
        lists += 1;
    }
    stop.store(true, Ordering::Relaxed);
    assert!(churn.join().unwrap() > 0);
    c.query("DROP SCHEMA dbl_churn CASCADE").unwrap();
}
