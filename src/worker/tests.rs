use super::*;
use crate::db::Kind;

/// SQL that makes `Worker::handle` panic.
pub const PANIC: &str = "-- tabella test: panic";

#[test]
fn any_reply_can_report_a_lost_connection() {
    let dropped = || DbError::MySql(mysql::Error::IoError(std::io::ErrorKind::ConnectionReset.into()));
    let refused = || DbError::Other("syntax error".into());
    let exported = |e: DbError| Reply::Exported { tab: 1, res: Err(e) };
    assert!(exported(dropped()).connection_lost());
    assert!(!exported(refused()).connection_lost());
    assert!(
        Reply::Compared {
            tab: 1,
            res: Err(dropped())
        }
        .connection_lost()
    );
    let script = |lost: bool| Reply::ScriptDone {
        tab: 1,
        progress: ScriptProgress::default(),
        error: Some(ScriptError {
            message: String::new(),
            line: 1,
            statement: String::new(),
            cancelled: false,
            lost,
        }),
        slowest: Vec::new(),
    };
    assert!(script(true).connection_lost() && !script(false).connection_lost());
    let search = |lost: bool| Reply::SearchDone {
        tab: 1,
        cancelled: false,
        lost,
        error: None,
    };
    assert!(search(true).connection_lost() && !search(false).connection_lost());
}

/// Needs `docker compose --profile test up -d --wait`.
fn export_whole_table(kind: Kind, port: u16, db: &str) {
    let mut cfg = ConnConfig::new(kind);
    cfg.host = "127.0.0.1".into();
    cfg.port = port;
    cfg.password = "pw".into();
    cfg.database = db.into();
    let mut c = Conn::connect(&cfg, None).expect("connect");
    let schema = if kind == Kind::MySql { db } else { "public" };
    c.query("DROP TABLE IF EXISTS dbl_export").unwrap();
    c.query("CREATE TABLE dbl_export (id INT PRIMARY KEY, v VARCHAR(20))")
        .unwrap();
    // 12 000 rows: more than two export chunks.
    let fill = match kind {
        Kind::Postgres => "INSERT INTO dbl_export SELECT g, 'v' || g FROM generate_series(1, 12000) g",
        Kind::MySql => {
            c.query("SET SESSION cte_max_recursion_depth = 20000").unwrap();
            "INSERT INTO dbl_export WITH RECURSIVE s (n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM s WHERE n < 12000) \
             SELECT n, CONCAT('v', n) FROM s"
        }
        Kind::Sqlite => unreachable!("server test"),
    };
    c.query(fill).unwrap();
    let path = std::env::temp_dir().join(format!("tabella-export-{kind:?}-{}.csv", std::process::id()));
    let cols = c.columns(schema, "dbl_export").unwrap();
    let spec = ExportSpec {
        format: Format::Csv,
        path: path.clone(),
        table_name: kind.qualified(schema, "dbl_export"),
        cols,
        source: ExportSource::Table {
            schema: schema.into(),
            table: "dbl_export".into(),
            filter: "id > 10".into(),
            order: Vec::new(),
            pk: vec!["id".into()],
        },
    };
    let (_, rows) = export(&mut c, spec).unwrap();
    assert_eq!(rows, 11_990, "{kind:?}");
    let text = std::fs::read_to_string(&path).unwrap();
    let ids: Vec<u32> = text
        .lines()
        .skip(1)
        .map(|l| l.split(',').next().unwrap().parse().unwrap())
        .collect();
    // Every row once, in key order: no gaps or repeats at chunk boundaries.
    assert_eq!(ids, (11..=12000).collect::<Vec<_>>(), "{kind:?}");
    let _ = std::fs::remove_file(&path);

    // The "new row" form's INSERT: only filled columns, values as parameters.
    let values = vec![
        ("id".to_string(), Some("20000".to_string())),
        ("v".to_string(), Some("it's".to_string())),
    ];
    let stmt = crate::sql::insert_values(kind, schema, "dbl_export", &values);
    assert_eq!(c.execute(&stmt).unwrap().affected, Some(1));
    let r = c.query("SELECT v FROM dbl_export WHERE id = 20000").unwrap();
    assert_eq!(r.rows.cell(0, 0), Some("it's"));
    c.query("DROP TABLE dbl_export").unwrap();
}

/// A low row limit (a preference for the grid) doesn't cut the file short.
#[test]
fn export_past_the_row_limit() {
    let cfg = crate::sqlite::tests::sample_db("export-limit");
    let mut c = Conn::connect(&cfg, None).unwrap();
    c.query(
        "CREATE TABLE big (id INTEGER PRIMARY KEY, v TEXT);
         WITH RECURSIVE s (n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM s WHERE n < 12000)
         INSERT INTO big SELECT n, 'v' || n FROM s",
    )
    .unwrap();
    let path = std::env::temp_dir().join(format!("tabella-export-limit-{}.csv", std::process::id()));
    for (order, pk) in [
        (Vec::new(), vec!["id".to_string()]),
        (vec![("v".to_string(), true)], Vec::new()),
    ] {
        let spec = ExportSpec {
            format: Format::Csv,
            path: path.clone(),
            table_name: "big".into(),
            cols: c.columns("main", "big").unwrap(),
            source: ExportSource::Table {
                schema: "main".into(),
                table: "big".into(),
                filter: String::new(),
                order,
                pk,
            },
        };
        let (_, rows) = crate::db::with_max_rows(1000, || export(&mut c, spec)).unwrap();
        assert_eq!(rows, 12_000);
    }
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&cfg.database);
}

/// Manual commit through the worker: writes stay invisible to other sessions until
/// confirmed, undo reverts SQL and grid edits alike, and a failed grid batch doesn't spoil
/// the transaction.
fn manual_commit(kind: Kind, port: u16, db: &str) {
    use std::time::Duration;
    let mut cfg = ConnConfig::new(kind);
    cfg.host = "127.0.0.1".into();
    cfg.port = port;
    cfg.password = "pw".into();
    cfg.database = db.into();
    let mut other = Conn::connect(&cfg, None).expect("connect");
    other.query("DROP TABLE IF EXISTS dbl_tx").unwrap();
    other
        .query("CREATE TABLE dbl_tx (id INT PRIMARY KEY, v VARCHAR(20))")
        .unwrap();
    other.query("INSERT INTO dbl_tx VALUES (1, 'a')").unwrap();
    let seen = |other: &mut Conn| {
        other
            .query("SELECT v FROM dbl_tx WHERE id = 1")
            .unwrap()
            .rows
            .cell(0, 0)
            .unwrap()
            .to_owned()
    };

    cfg.manual_commit = true;
    let Spawned { jobs, replies, .. } = spawn(cfg, Arc::new(|| {}));
    // Next reply that `f` picks out, skipping the rest.
    let wait = |f: &dyn Fn(Reply) -> Option<String>| -> String {
        loop {
            let r = replies
                .recv_timeout(Duration::from_secs(10))
                .expect("worker reply")
                .reply;
            if let Some(v) = f(r) {
                return v;
            }
        }
    };
    let tx_state = |r: Reply| match r {
        Reply::Tx(t) => Some(format!("{}:{}:{}", t.open, t.writes, t.failed)),
        _ => None,
    };
    let saved = |r: Reply| match r {
        Reply::Saved { res, .. } => Some(match *res {
            Ok(_) => "ok".to_string(),
            Err(e) => e.to_string(),
        }),
        _ => None,
    };
    let ended = |r: Reply| match r {
        Reply::TxEnded { res, .. } => Some(res.map(|()| "ok".to_string()).unwrap_or_else(|e| e.to_string())),
        _ => None,
    };
    let set = |v: &str, id: &str| {
        Stmt::new("UPDATE dbl_tx SET v = ")
            .value(Some(v))
            .sql(" WHERE id = ")
            .value(Some(id))
    };

    jobs.send(Job::Query {
        tab: 1,
        sql: "SELECT 1".into(),
    })
    .unwrap();
    jobs.send(Job::Query {
        tab: 1,
        sql: "UPDATE dbl_tx SET v = 'b' WHERE id = 1".into(),
    })
    .unwrap();
    assert_eq!(wait(&tx_state), "true:1:false", "{kind:?}: a write opens it");
    assert_eq!(seen(&mut other), "a", "{kind:?}: not visible before commit");
    jobs.send(Job::Batch {
        tab: 1,
        stmts: vec![set("c", "1")],
        refresh: "SELECT 1".into(),
    })
    .unwrap();
    assert_eq!(wait(&saved), "ok");
    jobs.send(Job::Batch {
        tab: 1,
        stmts: vec![set("x", "999")],
        refresh: "SELECT 1".into(),
    })
    .unwrap();
    assert!(wait(&saved).contains("Nothing was saved"));
    jobs.send(Job::EndTx { commit: false }).unwrap();
    assert_eq!(wait(&ended), "ok", "{kind:?}");
    assert_eq!(seen(&mut other), "a", "{kind:?}: undo reverts SQL and grid edits");

    jobs.send(Job::Query {
        tab: 1,
        sql: "UPDATE dbl_tx SET v = 'd' WHERE id = 1".into(),
    })
    .unwrap();
    jobs.send(Job::EndTx { commit: true }).unwrap();
    assert_eq!(wait(&ended), "ok");
    assert_eq!(seen(&mut other), "d", "{kind:?}: committed");

    if kind == Kind::Postgres {
        // After an error Postgres only accepts ROLLBACK: "Commit" says so instead of
        // pretending it worked.
        assert_eq!(wait(&tx_state), "false:0:false", "closed after the commit");
        jobs.send(Job::Query {
            tab: 1,
            sql: "UPDATE dbl_tx SET v = 'e' WHERE id = 1".into(),
        })
        .unwrap();
        jobs.send(Job::Query {
            tab: 1,
            sql: "UPDATE dbl_tx SET nope = 1".into(),
        })
        .unwrap();
        assert_eq!(wait(&tx_state), "true:1:false");
        assert_eq!(wait(&tx_state), "true:1:true");
        jobs.send(Job::EndTx { commit: true }).unwrap();
        assert!(wait(&ended).contains("rolled back"));
        assert_eq!(seen(&mut other), "d");
    }
    drop(jobs);
    other.query("DROP TABLE dbl_tx").unwrap();
}

/// A file script keeps its ten slowest statements, slowest first, with their lines.
#[test]
fn a_file_script_keeps_its_slowest_statements() {
    let cfg = crate::sqlite::tests::sample_db("slowest");
    let mut conn = Conn::connect(&cfg, None).unwrap();
    let dir = std::env::temp_dir().join(format!("tabella-slowest-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("script.sql");
    // Fifteen quick statements, and one that counts a few million rows on line 8.
    let mut text = String::new();
    for i in 1..=15 {
        if i == 8 {
            text.push_str(
                "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM n WHERE x < 3000000) SELECT count(*) FROM n;\n",
            );
        }
        text.push_str(&format!("SELECT {i};\n"));
    }
    std::fs::write(&path, text).unwrap();
    let (p, err, slowest) = run_script(
        &mut conn,
        &mut TxState::default(),
        false,
        &path,
        false,
        &AtomicBool::new(false),
        |_| {},
    );
    assert!(err.is_none(), "{err:?}");
    assert_eq!(p.statements, 16);
    assert_eq!(slowest.len(), SLOWEST_KEPT);
    assert_eq!(slowest[0].line, 8, "the slow one first");
    assert!(slowest[0].sql.starts_with("WITH RECURSIVE"));
    assert!(slowest.windows(2).all(|w| w[0].elapsed >= w[1].elapsed));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A script from the editor is timed statement by statement and still ends with the same
/// result sets and error position as the whole text would.
#[test]
fn editor_script_is_timed_per_statement() {
    let cfg = crate::sqlite::tests::sample_db("timed");
    let mut conn = Conn::connect(&cfg, None).unwrap();
    let mut tx = TxState::default();
    let sql = "CREATE TABLE tm (n INTEGER);\nINSERT INTO tm VALUES (1), (2);\nSELECT n FROM tm;\nSELECT n * 10 AS x FROM tm;";
    let r = run_editor_sql(&mut conn, &mut tx, false, sql).unwrap();
    let lines: Vec<usize> = r.timings.iter().map(|t| t.line).collect();
    assert_eq!(lines, [1, 2, 3, 4]);
    assert_eq!(r.timings[1].rows, 2, "an INSERT reports what it affected");
    assert!(!r.timings[1].returned_rows && r.timings[2].returned_rows);
    assert_eq!(r.timings[2].sql, "SELECT n FROM tm");
    assert_eq!(r.columns, ["x"], "the last result set is on screen");
    assert_eq!(r.previous.len(), 1, "the first SELECT is kept as an earlier set");
    // A single statement is left alone.
    assert!(
        run_editor_sql(&mut conn, &mut tx, false, "SELECT 1")
            .unwrap()
            .timings
            .is_empty()
    );
    // An error still points into the editor text.
    let bad = "SELECT 1;\nSELECT * FROM missing_table;";
    let e = run_editor_sql(&mut conn, &mut tx, false, bad).unwrap_err();
    let at = e.position_in(bad).expect("a position");
    assert!(bad[at..].starts_with("missing_table"), "{}", &bad[at..]);
    // Transaction control keeps the old whole-text run.
    let r = run_editor_sql(&mut conn, &mut tx, false, "BEGIN; SELECT 1; COMMIT;").unwrap();
    assert!(r.timings.is_empty());
}

/// SQLite through the worker, like the app uses it: connect, catalog, location, a query,
/// and "commit manual" undoing an UPDATE. No server needed.
#[test]
fn a_job_that_panics_answers_its_tab_and_the_next_one_runs() {
    let cfg = crate::sqlite::tests::sample_db("worker-panic");
    let Spawned { jobs, replies, .. } = spawn(cfg, Arc::new(|| {}));
    let rows = |tab: u64| loop {
        let r = replies
            .recv_timeout(Duration::from_secs(10))
            .expect("worker reply");
        if let Reply::Rows { tab: t, res, .. } = r.reply
            && t == tab
        {
            break res.map(|r| r.rows.len()).map_err(|e| e.to_string());
        }
    };
    let query = |tab, sql: &str| jobs.send(Job::Query { tab, sql: sql.into() }).unwrap();
    query(1, PANIC);
    let err = rows(1).unwrap_err();
    assert!(err.contains("internal error"), "{err}");
    query(2, "SELECT 1");
    assert_eq!(rows(2), Ok(1), "the connection was replaced and still works");
}

#[test]
fn sqlite_through_the_worker() {
    use std::time::Duration;
    let mut cfg = crate::sqlite::tests::sample_db("worker");
    cfg.manual_commit = true;
    let Spawned { jobs, replies, .. } = spawn(cfg.clone(), Arc::new(|| {}));
    let wait = |f: &dyn Fn(Reply) -> Option<String>| -> String {
        loop {
            let r = replies
                .recv_timeout(Duration::from_secs(10))
                .expect("worker reply")
                .reply;
            if let Some(v) = f(r) {
                return v;
            }
        }
    };
    let tables = wait(&|r| match r {
        Reply::Connected(res) => Some(
            res.map(|c| c.tables.len().to_string())
                .unwrap_or_else(|e| e.to_string()),
        ),
        _ => None,
    });
    assert_eq!(tables, "3");
    let here = wait(&|r| match r {
        Reply::Location(res) => Some(res.map(|(_, db, _)| db).unwrap_or_else(|e| e.to_string())),
        _ => None,
    });
    assert!(here.starts_with("tabella-worker"), "{here}");
    let rows = |r: Reply| match r {
        Reply::Rows { res, .. } => Some(match *res {
            Ok(r) => r
                .rows
                .first()
                .and_then(|row| row.cell(0).map(str::to_owned))
                .unwrap_or_default(),
            Err(e) => e.to_string(),
        }),
        _ => None,
    };
    jobs.send(Job::Query {
        tab: 1,
        sql: "UPDATE customer SET name = 'Z' WHERE id = 1".into(),
    })
    .unwrap();
    wait(&rows);
    let tx = wait(&|r| match r {
        Reply::Tx(t) => Some(format!("{}:{}", t.open, t.writes)),
        _ => None,
    });
    assert_eq!(tx, "true:1", "a write opens the transaction");
    jobs.send(Job::EndTx { commit: false }).unwrap();
    wait(&|r| matches!(r, Reply::TxEnded { .. }).then(String::new));
    jobs.send(Job::Query {
        tab: 1,
        sql: "SELECT name FROM customer WHERE id = 1".into(),
    })
    .unwrap();
    assert_eq!(wait(&rows), "Anna", "undone");
    drop(jobs);
    let _ = std::fs::remove_file(&cfg.database);
}

/// A session killed on the server is replaced: the next job runs on a new connection, the
/// picked schema is back, and an open transaction is reported as lost.
fn reconnects_after_the_session_is_killed(kind: Kind, port: u16, db: &str) {
    use std::time::Duration;
    let mut cfg = ConnConfig::new(kind);
    cfg.host = "127.0.0.1".into();
    cfg.port = port;
    cfg.password = "pw".into();
    cfg.database = db.into();
    let mut other = Conn::connect(&cfg, None).expect("connect");
    let Spawned { jobs, replies, .. } = spawn(cfg, Arc::new(|| {}));
    let next = || {
        replies
            .recv_timeout(Duration::from_secs(15))
            .expect("worker reply")
            .reply
    };
    let rows = |sql: &str| {
        jobs.send(Job::Query {
            tab: 1,
            sql: sql.into(),
        })
        .unwrap();
        loop {
            if let Reply::Rows { res, .. } = next() {
                return *res;
            }
        }
    };
    let id_sql = if kind == Kind::Postgres {
        "SELECT pg_backend_pid()"
    } else {
        "SELECT CONNECTION_ID()"
    };
    let id = rows(id_sql).unwrap().rows.cell(0, 0).map(str::to_owned).unwrap();
    other
        .query(&if kind == Kind::Postgres {
            format!("SELECT pg_terminate_backend({id})")
        } else {
            format!("KILL {id}")
        })
        .unwrap();
    std::thread::sleep(Duration::from_millis(500));
    // Whatever this one does, a Reconnected reply follows; the next job runs on the new session.
    let _ = rows("SELECT 1");
    let mut seen = false;
    jobs.send(Job::Query {
        tab: 1,
        sql: id_sql.into(),
    })
    .unwrap();
    loop {
        match next() {
            Reply::Reconnected { res, .. } => {
                assert!(res.is_ok(), "{kind:?}");
                seen = true;
            }
            Reply::Rows { res, .. } => {
                if let Ok(r) = *res {
                    let new_id = r.rows.cell(0, 0).map(str::to_owned).unwrap();
                    assert!(seen && new_id != id, "{kind:?}: a new session after Reconnected");
                    return;
                }
            }
            _ => {}
        }
    }
}

/// "Suggest index" on a real server: asks for one, gets it, creates it, and is told it is covered.
fn suggests_an_index(kind: Kind, port: u16, db: &str, schema: &str) {
    use std::time::Duration;
    let mut cfg = ConnConfig::new(kind);
    cfg.host = "127.0.0.1".into();
    cfg.port = port;
    cfg.password = "pw".into();
    cfg.database = db.into();
    let mut c = Conn::connect(&cfg, None).expect("connect");
    c.query("DROP TABLE IF EXISTS dbl_adv").unwrap();
    c.query("CREATE TABLE dbl_adv (id INT PRIMARY KEY, owner INT, status VARCHAR(20), created TIMESTAMP)")
        .unwrap();
    let Spawned { jobs, replies, .. } = spawn(cfg, Arc::new(|| {}));
    let ask = |sql: &str| {
        jobs.send(Job::SuggestIndex {
            tab: 1,
            sql: sql.into(),
            schema: schema.into(),
        })
        .unwrap();
        loop {
            if let Reply::IndexAdvice { res, .. } = replies
                .recv_timeout(Duration::from_secs(15))
                .expect("reply")
                .reply
            {
                return res;
            }
        }
    };
    let q = "SELECT * FROM dbl_adv WHERE owner = ? AND status = ? ORDER BY created DESC LIMIT 10";
    let a = ask(q).unwrap();
    assert_eq!(a.columns, ["owner", "status", "created"], "{kind:?}");
    assert!(a.ddl.starts_with("CREATE INDEX"), "{}", a.ddl);
    c.query(&a.ddl)
        .unwrap_or_else(|e| panic!("{kind:?}: the suggestion runs: {e}\n{}", a.ddl));
    let err = ask(q).unwrap_err();
    assert!(err.to_string().contains("Already indexed"), "{kind:?}: {err}");
    c.query("DROP TABLE dbl_adv").unwrap();
}

/// A bulk UPDATE by expression, as the form builds it: the preview counts the right rows and
/// running it changes only those.
fn bulk_expression_update(kind: Kind, port: u16, db: &str) {
    let mut cfg = ConnConfig::new(kind);
    cfg.host = "127.0.0.1".into();
    cfg.port = port;
    cfg.password = "pw".into();
    cfg.database = db.into();
    let mut c = Conn::connect(&cfg, None).expect("connect");
    c.query("DROP TABLE IF EXISTS dbl_bulk").unwrap();
    c.query("CREATE TABLE dbl_bulk (id INT PRIMARY KEY, price DECIMAL(10,2), tag VARCHAR(10))")
        .unwrap();
    c.query("INSERT INTO dbl_bulk VALUES (1, 10, 'a'), (2, 20, 'b'), (3, 30, 'c')")
        .unwrap();
    let schema = if kind == Kind::Postgres { "public" } else { db };
    let pk = vec!["id".to_string()];
    let rows = vec![vec![Some("1".to_string())], vec![Some("3".to_string())]];
    let sql = crate::sql::bulk_update(
        kind,
        schema,
        "dbl_bulk",
        "price",
        "price * 1.5",
        &crate::sql::BulkScope::Keys { pk: &pk, rows: &rows },
    );
    let preview = c
        .preview(&sql, false)
        .unwrap_or_else(|e| panic!("{kind:?}: {e}\n{sql}"));
    assert_eq!(preview.count, 2, "{kind:?}: two rows would change\n{sql}");
    let prices = |c: &mut Conn| {
        c.query("SELECT CONCAT(price, '') FROM dbl_bulk ORDER BY id")
            .unwrap()
            .rows
            .into_iter()
            .map(|r| r[0].clone().unwrap())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        prices(&mut c),
        ["10.00", "20.00", "30.00"],
        "the preview changed nothing"
    );
    c.query(&sql).unwrap();
    assert_eq!(prices(&mut c), ["15.00", "20.00", "45.00"], "{kind:?}");
    // By filter, an expression that reads another column's text.
    let sql = crate::sql::bulk_update(
        kind,
        schema,
        "dbl_bulk",
        "tag",
        "upper(tag)",
        &crate::sql::BulkScope::Filter("price > 20"),
    );
    assert_eq!(c.preview(&sql, false).unwrap().count, 1);
    c.query(&sql).unwrap();
    c.query("DROP TABLE dbl_bulk").unwrap();
}

#[test]
#[ignore = "needs the test databases: docker compose --profile test up -d --wait"]
fn bulk_expression_postgres() {
    bulk_expression_update(Kind::Postgres, 55432, "postgres");
}

#[test]
#[ignore = "needs the test databases: docker compose --profile test up -d --wait"]
fn bulk_expression_mysql() {
    bulk_expression_update(Kind::MySql, 53306, "test");
}

#[test]
#[ignore = "needs the test databases: docker compose --profile test up -d --wait"]
fn index_advice_postgres() {
    suggests_an_index(Kind::Postgres, 55432, "postgres", "public");
}

#[test]
#[ignore = "needs the test databases: docker compose --profile test up -d --wait"]
fn index_advice_mysql() {
    suggests_an_index(Kind::MySql, 53306, "test", "test");
}

#[test]
#[ignore = "needs the test databases: docker compose --profile test up -d --wait"]
fn reconnect_postgres() {
    reconnects_after_the_session_is_killed(Kind::Postgres, 55432, "postgres");
}

#[test]
#[ignore = "needs the test databases: docker compose --profile test up -d --wait"]
fn reconnect_mysql() {
    reconnects_after_the_session_is_killed(Kind::MySql, 53306, "test");
}

#[test]
#[ignore = "needs the test databases: docker compose --profile test up -d --wait"]
fn manual_commit_postgres() {
    manual_commit(Kind::Postgres, 55432, "postgres");
}

#[test]
#[ignore = "needs the test databases: docker compose --profile test up -d --wait"]
fn manual_commit_mysql() {
    manual_commit(Kind::MySql, 53306, "test");
}

/// Reads for the sidebar run on the second connection while the first is busy, but not
/// inside an open transaction (they must see its changes); cancelling a tab stops only
/// that tab's statement.
fn background_connection(kind: Kind, port: u16, db: &str) {
    let mut cfg = ConnConfig::new(kind);
    cfg.host = "127.0.0.1".into();
    cfg.port = port;
    cfg.password = "pw".into();
    cfg.database = db.into();
    cfg.manual_commit = true;
    let mut setup = Conn::connect(&cfg, None).expect("connect");
    setup.query("DROP TABLE IF EXISTS dbl_bg").unwrap();
    setup.query("CREATE TABLE dbl_bg (id INT PRIMARY KEY)").unwrap();
    let sleep = |secs: u32| match kind {
        Kind::Postgres => format!("SELECT pg_sleep({secs})"),
        _ => format!("SELECT SLEEP({secs})"),
    };
    let Spawned {
        jobs,
        replies,
        running,
        cancels,
        ..
    } = spawn(cfg, Arc::new(|| {}));
    // Which replies came, in order: "rows:<tab>" (ok or err) and "tables".
    let next = |timeout: u64| -> Option<String> {
        let deadline = Instant::now() + Duration::from_secs(timeout);
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match replies.recv_timeout(left).ok()?.reply {
                Reply::Rows { tab, res, .. } => {
                    return Some(format!("rows:{tab}:{}", if res.is_ok() { "ok" } else { "err" }));
                }
                Reply::Tables(_) => return Some("tables".into()),
                _ => {}
            }
        }
        None
    };
    // The second connection opens once the first is up.
    thread::sleep(Duration::from_secs(1));
    if kind == Kind::Postgres {
        let named = setup
            .query("SELECT count(*) FROM pg_stat_activity WHERE application_name = 'tabella (background)'")
            .unwrap();
        assert_ne!(
            named.rows.cell(0, 0),
            Some("0"),
            "the second session says what it is"
        );
    }

    jobs.send(Job::Query {
        tab: 1,
        sql: sleep(2),
    })
    .unwrap();
    thread::sleep(Duration::from_millis(200));
    jobs.send(Job::Tables).unwrap();
    assert_eq!(
        next(1).as_deref(),
        Some("tables"),
        "{kind:?}: the sidebar didn't wait"
    );
    assert_eq!(next(5).as_deref(), Some("rows:1:ok"));

    // A tab waiting in line isn't the one running: cancelling it stops nothing.
    jobs.send(Job::Query {
        tab: 2,
        sql: sleep(30),
    })
    .unwrap();
    thread::sleep(Duration::from_millis(300));
    running.iter().for_each(|r| r.cancel(Some(3)));
    assert_eq!(next(1), None, "{kind:?}: another tab's statement was cancelled");
    // Stopped long before its 30 s (MySQL's SLEEP then returns 1 rather than failing).
    running.iter().for_each(|r| r.cancel(Some(2)));
    let stopped = next(10).unwrap_or_default();
    assert!(stopped.starts_with("rows:2:"), "{kind:?}: its own is ({stopped})");

    // A tab's job still in line behind another's is skipped, not run, once cancelled.
    jobs.send(Job::Query {
        tab: 4,
        sql: sleep(1),
    })
    .unwrap();
    jobs.send(Job::Query {
        tab: 5,
        sql: sleep(30),
    })
    .unwrap();
    thread::sleep(Duration::from_millis(200));
    cancels.mark(5);
    running.iter().for_each(|r| r.cancel(Some(5)));
    assert_eq!(
        next(5).as_deref(),
        Some("rows:4:ok"),
        "{kind:?}: the other tab ran"
    );
    assert_eq!(
        next(2).as_deref(),
        Some("rows:5:err"),
        "{kind:?}: skipped at once"
    );

    // With a transaction open, the sidebar reads on the first connection, after it.
    jobs.send(Job::Query {
        tab: 1,
        sql: "INSERT INTO dbl_bg VALUES (1)".into(),
    })
    .unwrap();
    assert_eq!(next(5).as_deref(), Some("rows:1:ok"));
    jobs.send(Job::Query {
        tab: 1,
        sql: sleep(1),
    })
    .unwrap();
    jobs.send(Job::Tables).unwrap();
    assert_eq!(
        next(5).as_deref(),
        Some("rows:1:ok"),
        "{kind:?}: in order inside a transaction"
    );
    assert_eq!(next(5).as_deref(), Some("tables"));
    jobs.send(Job::EndTx { commit: false }).unwrap();
    drop(jobs);
    setup.query("DROP TABLE dbl_bg").unwrap();
}

/// With `background_reads` off, the sidebar waits in line like before.
#[test]
#[ignore = "needs the test databases: docker compose --profile test up -d --wait"]
fn background_reads_can_be_turned_off() {
    let mut cfg = ConnConfig::new(Kind::Postgres);
    cfg.host = "127.0.0.1".into();
    cfg.port = 55432;
    cfg.password = "pw".into();
    cfg.database = "postgres".into();
    cfg.background_reads = false;
    let Spawned { jobs, replies, .. } = spawn(cfg, Arc::new(|| {}));
    thread::sleep(Duration::from_secs(1));
    jobs.send(Job::Query {
        tab: 1,
        sql: "SELECT pg_sleep(1)".into(),
    })
    .unwrap();
    jobs.send(Job::Tables).unwrap();
    let order: Vec<&str> = std::iter::from_fn(|| replies.recv_timeout(Duration::from_secs(5)).ok())
        .filter_map(|r| match r.reply {
            Reply::Rows { .. } => Some("rows"),
            Reply::Tables(_) => Some("tables"),
            _ => None,
        })
        .take(2)
        .collect();
    assert_eq!(order, ["rows", "tables"]);
}

#[test]
#[ignore = "needs the test databases: docker compose --profile test up -d --wait"]
fn background_connection_postgres() {
    background_connection(Kind::Postgres, 55432, "postgres");
}

#[test]
#[ignore = "needs the test databases: docker compose --profile test up -d --wait"]
fn background_connection_mysql() {
    background_connection(Kind::MySql, 53306, "test");
}

/// Timed editor scripts on a real server: Postgres keeps the all-or-nothing of a
/// multi-statement text, and an error points at the failing statement in the text.
fn timed_script(kind: Kind, port: u16, db: &str) {
    let mut cfg = ConnConfig::new(kind);
    cfg.host = "127.0.0.1".into();
    cfg.port = port;
    cfg.password = "pw".into();
    cfg.database = db.into();
    if kind == Kind::Postgres {
        cfg.user = "postgres".into();
    } else {
        cfg.user = "root".into();
    }
    let mut conn = Conn::connect(&cfg, None).unwrap();
    let pg = kind == Kind::Postgres;
    let mut tx = TxState::default();
    conn.query("DROP TABLE IF EXISTS dbl_timed").unwrap();
    conn.query("CREATE TABLE dbl_timed (n INT PRIMARY KEY)").unwrap();
    let sql = "INSERT INTO dbl_timed VALUES (1);\nSELECT n FROM dbl_timed;\nINSERT INTO dbl_timed VALUES (2)";
    let r = run_editor_sql(&mut conn, &mut tx, pg, sql).unwrap();
    assert_eq!(r.timings.len(), 3, "{kind:?}");
    assert_eq!(r.timings[1].rows, 1);
    let bad = "INSERT INTO dbl_timed VALUES (3);\nSELECT * FROM missing_tbl";
    let e = run_editor_sql(&mut conn, &mut tx, pg, bad).unwrap_err();
    let at = e
        .position_in(bad)
        .unwrap_or_else(|| panic!("{kind:?}: no position in {e}"));
    assert!(bad[at..].contains("missing_tbl"), "{kind:?}: {}", &bad[at..]);
    let n = conn.query("SELECT count(*) FROM dbl_timed WHERE n = 3").unwrap();
    if pg {
        assert_eq!(n.rows.cell(0, 0), Some("0"), "Postgres rolls the whole text back");
    }
    conn.query("DROP TABLE dbl_timed").unwrap();
}

/// A database with nothing in `public` opens in its first schema with tables.
#[test]
#[ignore = "needs the test databases: docker compose --profile test up -d --wait"]
fn starts_in_a_schema_with_tables() {
    let mut cfg = ConnConfig::new(Kind::Postgres);
    cfg.host = "127.0.0.1".into();
    cfg.port = 55432;
    cfg.user = "postgres".into();
    cfg.password = "pw".into();
    cfg.database = "postgres".into();
    let mut admin = Conn::connect(&cfg, None).unwrap();
    admin.query("DROP DATABASE IF EXISTS dbl_start").unwrap();
    admin.query("CREATE DATABASE dbl_start").unwrap();
    cfg.database = "dbl_start".into();
    {
        let mut conn = Conn::connect(&cfg, None).unwrap();
        conn.query("CREATE SCHEMA zeta; CREATE TABLE zeta.z (n INT); CREATE SCHEMA beta; CREATE TABLE beta.b (n INT)")
            .unwrap();
        let tables = conn.tables().unwrap();
        assert_eq!(start_schema(&mut conn, &tables).as_deref(), Some("beta"));
        assert_eq!(conn.location().unwrap().2, "beta");
        conn.query("SELECT n FROM b")
            .expect("unqualified names find its tables");
        // With a table of its own, public is where it opens.
        conn.query("CREATE TABLE public.p (n INT)").unwrap();
        let mut conn = Conn::connect(&cfg, None).unwrap();
        let tables = conn.tables().unwrap();
        assert_eq!(start_schema(&mut conn, &tables), None);
        assert_eq!(conn.location().unwrap().2, "public");
    }
    admin.query("DROP DATABASE dbl_start").unwrap();
}

#[test]
#[ignore = "needs the test databases: docker compose --profile test up -d --wait"]
fn timed_script_postgres() {
    timed_script(Kind::Postgres, 55432, "postgres");
}

#[test]
#[ignore = "needs the test databases: docker compose --profile test up -d --wait"]
fn timed_script_mysql() {
    timed_script(Kind::MySql, 53306, "test");
}

/// A file with a routine body, a COPY block / DELIMITER, then a failing statement: in a
/// transaction nothing stays; without one, what ran before the error does.
fn script_file(kind: Kind, port: u16, db: &str) {
    let mut cfg = ConnConfig::new(kind);
    cfg.host = "127.0.0.1".into();
    cfg.port = port;
    cfg.password = "pw".into();
    cfg.database = db.into();
    let mut c = Conn::connect(&cfg, None).expect("connect");
    let pg = kind == Kind::Postgres;
    let body = if pg {
        "\\set ON_ERROR_STOP on\nCREATE TABLE dbl_script (id int PRIMARY KEY, v text);\n\
         COPY dbl_script (id, v) FROM stdin;\n1\tone\n2\ttwo; still\n\\.\n\
         CREATE FUNCTION dbl_script_f() RETURNS int AS $$ BEGIN RETURN 1; END; $$ LANGUAGE plpgsql;\n\
         DROP FUNCTION dbl_script_f();\n"
    } else {
        "CREATE TABLE dbl_script (id int PRIMARY KEY, v text);\n\
         INSERT INTO dbl_script VALUES (1, 'one'), (2, 'two; still');\n\
         DELIMITER //\nCREATE PROCEDURE dbl_script_p() BEGIN SELECT 1; SELECT 2; END //\nDELIMITER ;\n\
         DROP PROCEDURE dbl_script_p;\n"
    };
    let path = std::env::temp_dir().join(format!("tabella-script-{kind:?}-{}.sql", std::process::id()));
    let count = |c: &mut Conn| {
        c.query("SELECT count(*) FROM dbl_script")
            .ok()
            .and_then(|r| r.rows.cell(0, 0).map(str::to_owned))
    };
    let run = |c: &mut Conn, text: &str, transaction: bool| {
        std::fs::write(&path, text).unwrap();
        let mut tx = TxState::default();
        let (p, err, _) = run_script(
            c,
            &mut tx,
            pg,
            &path,
            transaction,
            &AtomicBool::new(false),
            |_| {},
        );
        (p, err)
    };
    c.query("DROP TABLE IF EXISTS dbl_script").unwrap();

    let (p, err) = run(&mut c, body, true);
    assert_eq!(err, None);
    assert_eq!(p.bytes, body.len() as u64);
    assert_eq!(count(&mut c).as_deref(), Some("2"));
    if pg {
        assert_eq!((p.statements, p.copied, p.skipped), (3, 2, 1));
    } else {
        assert_eq!(p.statements, 4);
    }
    c.query("DROP TABLE dbl_script").unwrap();

    // Fails on line 3 of the tail: rolled back as a whole (Postgres undoes the CREATE too).
    let broken =
        format!("{body}INSERT INTO dbl_script VALUES (3, 'three');\nSELECT * FROM does_not_exist;\n");
    let (_, err) = run(&mut c, &broken, true);
    let err = err.expect("error");
    assert!(
        err.statement.contains("does_not_exist") && !err.cancelled,
        "{err:?}"
    );
    assert_eq!(err.line, broken.lines().count() as u64);
    if pg {
        assert_eq!(count(&mut c), None, "the table went away with the rollback");
    } else {
        // MySQL commits on DDL (the CREATE PROCEDURE keeps rows 1 and 2), but the INSERT
        // after the last one is undone.
        assert_eq!(count(&mut c).as_deref(), Some("2"));
        c.query("DROP TABLE dbl_script").unwrap();
    }

    // Without a transaction what ran before the error stays.
    let (p, err) = run(&mut c, &broken, false);
    assert!(err.is_some());
    assert_eq!(count(&mut c).as_deref(), Some("3"));
    assert!(p.statements >= 3);
    c.query("DROP TABLE dbl_script").unwrap();

    // Cancelled before it starts: nothing runs.
    std::fs::write(&path, body).unwrap();
    let (p, err, _) = run_script(
        &mut c,
        &mut TxState::default(),
        pg,
        &path,
        true,
        &AtomicBool::new(true),
        |_| {},
    );
    assert!(err.is_some_and(|e| e.cancelled) && p.statements == 0);
    let _ = std::fs::remove_file(&path);
}

/// A search that runs too long stops at 2 s, says so, and leaves an open transaction (and
/// the session's timeout) as they were.
#[test]
#[ignore = "needs the test databases: docker compose --profile test up -d --wait"]
fn capped_reads_never_break_the_transaction() {
    let mut cfg = ConnConfig::new(Kind::Postgres);
    cfg.host = "127.0.0.1".into();
    cfg.port = 55432;
    cfg.password = "pw".into();
    cfg.database = "postgres".into();
    let mut c = Conn::connect(&cfg, None).expect("connect");
    let timeout = |c: &mut Conn| {
        c.query("SHOW statement_timeout")
            .unwrap()
            .rows
            .cell(0, 0)
            .map(str::to_owned)
    };
    let before = timeout(&mut c);
    assert!(capped_read(&mut c, true, false, "SELECT 1").is_ok());
    let err = capped_read(&mut c, true, false, "SELECT pg_sleep(3)")
        .unwrap_err()
        .to_string();
    assert!(err.contains("2 s"), "{err}");
    assert_eq!(timeout(&mut c), before);
    c.query("BEGIN").unwrap();
    c.query("CREATE TEMP TABLE dbl_cap (x int)").unwrap();
    assert!(capped_read(&mut c, true, true, "SELECT pg_sleep(3)").is_err());
    // Still usable, with what was done before the search.
    c.query("INSERT INTO dbl_cap VALUES (1)").unwrap();
    c.query("COMMIT").unwrap();
    assert_eq!(
        c.query("SELECT count(*) FROM dbl_cap").unwrap().rows.cell(0, 0),
        Some("1")
    );
    assert_eq!(timeout(&mut c), before);
}

#[test]
#[ignore = "needs the test databases: docker compose --profile test up -d --wait"]
fn connection_test_says_who_answered() {
    let mut cfg = ConnConfig::new(Kind::Postgres);
    cfg.host = "127.0.0.1".into();
    cfg.port = 55432;
    cfg.password = "pw".into();
    cfg.database = "postgres".into();
    let ctx: Waker = Arc::new(|| {});
    let ok = test_connection(cfg.clone(), ctx.clone()).recv().unwrap().unwrap();
    assert!(
        ok.starts_with("Connected in ") && ok.contains("PostgreSQL 1"),
        "{ok}"
    );
    cfg.password = "wrong".into();
    let err = test_connection(cfg.clone(), ctx.clone())
        .recv()
        .unwrap()
        .unwrap_err();
    assert!(err.contains("password"), "{err}");
    cfg.kind = Kind::MySql;
    cfg.port = 53306;
    cfg.password = "pw".into();
    cfg.user = "root".into();
    cfg.database = "test".into();
    let ok = test_connection(cfg, ctx).recv().unwrap().unwrap();
    assert!(ok.contains("MySQL 8"), "{ok}");
}

/// Same connection (two schemas) and a separate one to the same server: same differences.
#[test]
#[ignore = "needs the test databases: docker compose --profile test up -d --wait"]
fn compare_here_and_elsewhere() {
    let mut cfg = ConnConfig::new(Kind::Postgres);
    cfg.host = "127.0.0.1".into();
    cfg.port = 55432;
    cfg.password = "pw".into();
    cfg.database = "postgres".into();
    let mut c = Conn::connect(&cfg, None).expect("connect");
    for sql in [
        "DROP SCHEMA IF EXISTS cw_a CASCADE",
        "DROP SCHEMA IF EXISTS cw_b CASCADE",
        "CREATE SCHEMA cw_a",
        "CREATE SCHEMA cw_b",
        "CREATE TABLE cw_a.t (id int PRIMARY KEY, name text NOT NULL)",
        "CREATE TABLE cw_b.t (id int PRIMARY KEY)",
    ] {
        c.query(sql).unwrap();
    }
    let here = compare_with(&mut c, "cw_a", None, "cw_b").unwrap();
    let there = compare_with(&mut c, "cw_a", Some(&cfg), "cw_b").unwrap();
    assert_eq!(here, there);
    assert_eq!(here.len(), 1);
    assert!(here[0].items[0].sql.contains("ADD COLUMN \"name\" text NOT NULL"));
    let mut other = cfg.clone();
    other.kind = Kind::MySql;
    assert!(compare_with(&mut c, "cw_a", Some(&other), "cw_b").is_err());
    c.query("DROP SCHEMA cw_a CASCADE").unwrap();
    c.query("DROP SCHEMA cw_b CASCADE").unwrap();
}

#[test]
#[ignore = "needs the test databases: docker compose --profile test up -d --wait"]
fn script_file_postgres() {
    script_file(Kind::Postgres, 55432, "postgres");
}

#[test]
#[ignore = "needs the test databases: docker compose --profile test up -d --wait"]
fn script_file_mysql() {
    script_file(Kind::MySql, 53306, "test");
}

/// A real table, by hand: `TABELLA_EXPORT_CHECK=host:port:user:password:database:schema.table`
/// (Postgres). Exports it whole as CSV and reports the time.
#[test]
#[ignore = "by hand against a real database: set TABELLA_EXPORT_CHECK"]
fn export_real_table() {
    let Ok(spec) = std::env::var("TABELLA_EXPORT_CHECK") else {
        return;
    };
    let [host, port, user, password, database, qualified] = spec.split(':').collect::<Vec<_>>()[..] else {
        panic!("host:port:user:password:database:schema.table")
    };
    let (schema, table) = qualified.split_once('.').unwrap();
    let cfg = ConnConfig {
        host: host.into(),
        port: port.parse().unwrap(),
        user: user.into(),
        password: password.into(),
        database: database.into(),
        ..ConnConfig::new(Kind::Postgres)
    };
    let mut c = Conn::connect(&cfg, None).expect("connect");
    let cols = c.columns(schema, table).unwrap();
    let pk = cols.iter().filter(|c| c.pk).map(|c| c.name.clone()).collect();
    let path = std::env::temp_dir().join("tabella-export-real.csv");
    let started = std::time::Instant::now();
    let (_, rows) = export(
        &mut c,
        ExportSpec {
            format: Format::Csv,
            path: path.clone(),
            table_name: Kind::Postgres.qualified(schema, table),
            cols,
            source: ExportSource::Table {
                schema: schema.into(),
                table: table.into(),
                filter: String::new(),
                order: Vec::new(),
                pk,
            },
        },
    )
    .unwrap();
    eprintln!("{rows} rows in {:?}", started.elapsed());
    let _ = std::fs::remove_file(path);
}

#[test]
#[ignore = "needs the test databases: docker compose --profile test up -d --wait"]
fn export_postgres() {
    export_whole_table(Kind::Postgres, 55432, "postgres");
}

#[test]
#[ignore = "needs the test databases: docker compose --profile test up -d --wait"]
fn export_mysql() {
    export_whole_table(Kind::MySql, 53306, "test");
}
