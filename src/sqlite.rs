//! SQLite: a database file on this computer, opened directly (no server, no user). Scripts run
//! statement by statement like on the servers; values come back as text the same way.

use fallible_iterator::FallibleIterator;
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags};

use crate::db::{ConnConfig, QueryResult};
use crate::error::{DbError, DbResult};

/// Opens the file in `cfg.database`. An existing file only: a typo shouldn't leave an empty
/// database behind (new files are made from the connection form).
pub fn open(cfg: &ConnConfig) -> DbResult<Connection> {
    let path = cfg.database.trim();
    if path.is_empty() {
        return Err(DbError::Other("escolha o arquivo do banco SQLite".into()));
    }
    if !std::path::Path::new(path).is_file() {
        return Err(DbError::Other(format!("arquivo não encontrado: {path}")));
    }
    let flags = if cfg.read_only {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    } else {
        OpenFlags::SQLITE_OPEN_READ_WRITE
    } | OpenFlags::SQLITE_OPEN_URI
        | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let c = Connection::open_with_flags(path, flags)?;
    // Another program writing the file: wait a little instead of failing at once.
    c.busy_timeout(std::time::Duration::from_secs(5))?;
    // Foreign keys are only enforced when asked, per connection.
    c.execute_batch("PRAGMA foreign_keys = ON")?;
    // A quick check that it is a database at all (any file opens).
    c.query_row("SELECT count(*) FROM sqlite_master", [], |_| Ok(()))
        .map_err(|_| DbError::Other(format!("{path} não é um banco SQLite")))?;
    Ok(c)
}

/// Creates an empty database file (the form's "Criar arquivo novo").
pub fn create(path: &str) -> DbResult<()> {
    if std::path::Path::new(path).exists() {
        return Err(DbError::Other(format!("já existe um arquivo em {path}")));
    }
    let c = Connection::open(path)?;
    // SQLite writes nothing until there is something to write.
    c.execute_batch("PRAGMA user_version = 0; VACUUM")?;
    Ok(())
}

/// Every statement of `sql`; the last result set with the earlier ones in `previous`.
pub fn query(c: &Connection, sql: &str, limit: usize) -> DbResult<QueryResult> {
    let mut out = QueryResult::default();
    let mut batch = rusqlite::Batch::new(c, sql);
    while let Some(mut stmt) = batch.next()? {
        if stmt.column_count() == 0 {
            stmt.raw_execute()?;
            out.affected = Some(c.changes());
            continue;
        }
        collect(&mut stmt, &mut out, limit, &[])?;
    }
    Ok(out)
}

/// One statement with `?` placeholders bound to `values` (see `sql::Stmt`).
pub fn exec(c: &Connection, sql: &str, values: &[String], limit: usize) -> DbResult<QueryResult> {
    let mut out = QueryResult::default();
    let mut stmt = c.prepare(sql)?;
    if stmt.column_count() == 0 {
        let n = stmt.execute(rusqlite::params_from_iter(values))?;
        out.affected = Some(n as u64);
        return Ok(out);
    }
    collect(&mut stmt, &mut out, limit, values)?;
    Ok(out)
}

fn collect(
    stmt: &mut rusqlite::Statement,
    out: &mut QueryResult,
    limit: usize,
    values: &[String],
) -> DbResult<()> {
    out.next_set();
    out.columns = stmt.column_names().iter().map(|c| c.to_string()).collect();
    out.rows.clear();
    out.truncated = false;
    let width = out.columns.len();
    let mut rows = stmt.query(rusqlite::params_from_iter(values))?;
    while let Some(row) = rows.next()? {
        if out.rows.len() >= limit {
            out.truncated = true;
            break;
        }
        out.rows
            .push((0..width).map(|i| text(row.get_ref(i).ok()?)).collect());
    }
    Ok(())
}

/// A value as the grid shows it: blobs in hex like Postgres's bytea (`\x00ff`).
fn text(v: ValueRef) -> Option<String> {
    match v {
        ValueRef::Null => None,
        ValueRef::Integer(i) => Some(i.to_string()),
        // `20.0`, not `20`: a REAL shows it is one (like the sqlite3 shell).
        ValueRef::Real(f) => Some(format!("{f:?}")),
        ValueRef::Text(t) => Some(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => Some(format!(
            "\\x{}",
            b.iter().map(|x| format!("{x:02x}")).collect::<String>()
        )),
    }
}

// The catalog, through SQLite's own tables and pragma functions. SQLite has one "schema" here:
// `main` (the file itself).

pub const TABLES: &str = "SELECT 'main', name, CASE type WHEN 'view' THEN 'VIEW' ELSE 'BASE TABLE' END, NULL, NULL \
     FROM sqlite_master WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite_%' ORDER BY 2";

pub const ALL_COLUMNS: &str = "SELECT 'main', m.name, p.name, p.type, CASE WHEN p.pk > 0 THEN '1' ELSE '0' END \
     FROM sqlite_master m JOIN pragma_table_info(m.name) p \
     WHERE m.type IN ('table', 'view') AND m.name NOT LIKE 'sqlite_%' ORDER BY 2, p.cid";

/// A single-column foreign key's target column (`NULL` in the pragma means "its primary key").
const FK_TARGET: &str =
    "coalesce(f.\"to\", (SELECT p.name FROM pragma_table_info(f.\"table\") p WHERE p.pk = 1))";

pub fn all_fks() -> String {
    format!(
        "SELECT 'main', m.name, f.\"from\", 'main', f.\"table\", {FK_TARGET} \
         FROM sqlite_master m JOIN pragma_foreign_key_list(m.name) f WHERE m.type = 'table' \
         AND (SELECT count(*) FROM pragma_foreign_key_list(m.name) f2 WHERE f2.id = f.id) = 1 ORDER BY 2, 3"
    )
}

/// `(name, type, pk, nullable YES/NO, gets a value by itself)`: a default, a generated column,
/// or the INTEGER PRIMARY KEY that SQLite numbers.
pub fn columns(table: &str) -> crate::sql::Stmt {
    crate::sql::Stmt::new(
        "SELECT p.name, p.type, p.pk > 0, CASE WHEN p.\"notnull\" = 1 OR p.pk > 0 THEN 'NO' ELSE 'YES' END, \
         p.dflt_value IS NOT NULL OR p.hidden IN (2, 3) OR (p.pk = 1 AND upper(p.type) = 'INTEGER' \
         AND (SELECT count(*) FROM pragma_table_info(",
    )
    .value(Some(table))
    .sql(") WHERE pk > 0) = 1) FROM pragma_table_xinfo(")
    .value(Some(table))
    .sql(") p WHERE p.hidden <> 1 ORDER BY p.cid")
}

/// `(column, schema, table, column)` of the table's single-column foreign keys.
pub fn column_fks(table: &str) -> crate::sql::Stmt {
    crate::sql::Stmt::new(format!(
        "SELECT f.\"from\", 'main', f.\"table\", {FK_TARGET} FROM pragma_foreign_key_list("
    ))
    .value(Some(table))
    .sql(") f WHERE (SELECT count(*) FROM pragma_foreign_key_list(")
    .value(Some(table))
    .sql(") f2 WHERE f2.id = f.id) = 1")
}

/// Other tables' single-column foreign keys pointing at `table`.
pub fn incoming_fks(table: &str) -> crate::sql::Stmt {
    crate::sql::Stmt::new(format!(
        "SELECT 'main', m.name, f.\"from\", {FK_TARGET} FROM sqlite_master m \
         JOIN pragma_foreign_key_list(m.name) f WHERE m.type = 'table' \
         AND (SELECT count(*) FROM pragma_foreign_key_list(m.name) f2 WHERE f2.id = f.id) = 1 \
         AND lower(f.\"table\") = lower("
    ))
    .value(Some(table))
    .sql(") ORDER BY 2, 3")
}

pub const OBJECTS: &str = "SELECT 'main', name, CASE type WHEN 'view' THEN 'v' ELSE 't' END, '', \
     CASE type WHEN 'trigger' THEN tbl_name ELSE '' END, '' \
     FROM sqlite_master WHERE type IN ('view', 'trigger') AND name NOT LIKE 'sqlite_%' ORDER BY 1, 3, 2";

pub const FILE: &str = "SELECT file FROM pragma_database_list WHERE name = 'main'";

/// What has no meaning without a server.
pub fn no_server(what: &str) -> DbError {
    DbError::Other(format!(
        "{what} não existe no SQLite: é um arquivo, sem servidor nem sessões"
    ))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE t (id INTEGER PRIMARY KEY, nome TEXT, preco REAL, foto BLOB);
             INSERT INTO t (nome, preco, foto) VALUES ('ana', 1.5, x'00ff'), ('bia', NULL, NULL);",
        )
        .unwrap();
        c
    }

    #[test]
    fn scripts_return_every_result() {
        let c = db();
        let r = query(
            &c,
            "SELECT count(*) AS n FROM t; UPDATE t SET preco = 2 WHERE id = 2; SELECT * FROM t ORDER BY id",
            10,
        )
        .unwrap();
        assert_eq!(r.previous.len(), 1);
        assert_eq!(r.previous[0].rows, vec![vec![Some("2".into())]]);
        assert_eq!(r.columns, ["id", "nome", "preco", "foto"]);
        assert_eq!(
            r.rows[0],
            vec![
                Some("1".into()),
                Some("ana".into()),
                Some("1.5".into()),
                Some("\\x00ff".into())
            ]
        );
        assert_eq!(r.rows[1][2].as_deref(), Some("2.0"));
        let r = query(&c, "SELECT * FROM t", 1).unwrap();
        assert!(r.truncated && r.rows.len() == 1);
        let r = exec(
            &c,
            "UPDATE t SET nome = ? WHERE id = ?",
            &["o'neil".into(), "1".into()],
            10,
        )
        .unwrap();
        assert_eq!(r.affected, Some(1));
        // ("FRM" would be an alias of id, as everywhere: the error is at "t".)
        let e = query(&c, "SELECT id\nFROM t\nORDER id", 10).unwrap_err();
        let sql = "SELECT id\nFROM t\nORDER id";
        assert_eq!(e.position_in(sql), sql.rfind("id"), "{e}");
        let e = query(&c, "SELECT emial FROM t", 10).unwrap_err();
        assert_eq!(e.position_in("SELECT emial FROM t"), Some(7), "{e}");
    }

    /// A fresh database file with two related tables, an index, a view and a trigger.
    pub fn sample_db(name: &str) -> crate::db::ConnConfig {
        let path = std::env::temp_dir().join(format!("dblite-{name}-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        create(&path.display().to_string()).unwrap();
        let mut cfg = crate::db::ConnConfig::new(crate::db::Kind::Sqlite);
        cfg.database = path.display().to_string();
        let mut c = crate::db::Conn::connect(&cfg, None).unwrap();
        c.query(
            "CREATE TABLE cliente (id INTEGER PRIMARY KEY, nome TEXT NOT NULL, email TEXT UNIQUE, ativo BOOLEAN DEFAULT 1);
             CREATE TABLE pedido (id INTEGER PRIMARY KEY, cliente_id INTEGER REFERENCES cliente (id), total REAL, foto BLOB, obs TEXT);
             CREATE INDEX pedido_cliente ON pedido (cliente_id);
             CREATE VIEW resumo AS SELECT cliente_id, count(*) AS n FROM pedido GROUP BY cliente_id;
             CREATE TRIGGER pedido_obs AFTER INSERT ON pedido BEGIN
               UPDATE pedido SET obs = coalesce(obs, 'auto') WHERE id = NEW.id;
             END;
             INSERT INTO cliente (nome, email) VALUES ('Ana', 'ana@x.com'), ('Bia', 'bia@x.com');
             INSERT INTO pedido (cliente_id, total, foto) VALUES (1, 10.5, x'00ff'), (1, 20, NULL), (2, 7.25, NULL);",
        )
        .unwrap();
        cfg
    }

    #[test]
    fn catalog_editing_and_safety_through_the_app_paths() {
        use crate::db::{Conn, Kind};
        use crate::sql;
        let k = Kind::Sqlite;
        let cfg = sample_db("catalog");
        let mut c = Conn::connect(&cfg, None).unwrap();
        // Catalog.
        let tables = c.tables().unwrap();
        let names: Vec<(&str, bool)> = tables.iter().map(|t| (t.name.as_str(), t.is_view)).collect();
        assert_eq!(names, [("cliente", false), ("pedido", false), ("resumo", true)]);
        assert!(tables.iter().all(|t| t.schema == "main"));
        let cols = c.columns_with_fks("main", "pedido").unwrap();
        let id = &cols[0];
        assert!(
            id.pk && id.has_default && !id.nullable,
            "INTEGER PRIMARY KEY numbers itself: {id:?}"
        );
        let fk = cols
            .iter()
            .find(|c| c.name == "cliente_id")
            .unwrap()
            .references
            .clone()
            .unwrap();
        assert_eq!(
            (fk.schema.as_str(), fk.table.as_str(), fk.column.as_str()),
            ("main", "cliente", "id")
        );
        let nome = &c.columns("main", "cliente").unwrap()[1];
        assert!(!nome.nullable && !nome.has_default);
        assert!(
            c.all_columns()
                .unwrap()
                .iter()
                .any(|c| c.table == "resumo" && c.column == "n")
        );
        assert_eq!(c.all_fks().unwrap().len(), 1);
        let inc = c.incoming_fks("main", "cliente").unwrap();
        assert_eq!(
            (
                inc[0].table.as_str(),
                inc[0].column.as_str(),
                inc[0].target.as_str()
            ),
            ("pedido", "cliente_id", "id")
        );
        let objs = c.objects().unwrap();
        assert_eq!(objs.len(), 2, "{objs:?}");
        let trig = objs.iter().find(|o| o.name == "pedido_obs").unwrap();
        assert_eq!(trig.table, "pedido");
        assert!(c.object_source(trig).unwrap().contains("END;"));
        let (dbs, here, schema) = c.location().unwrap();
        assert!(dbs.is_empty() && here.starts_with("dblite-catalog") && schema.is_empty());
        let st = c.structure("main", "pedido").unwrap();
        assert!(
            st.ddl.contains("CREATE TABLE pedido") && st.ddl.contains("CREATE INDEX pedido_cliente"),
            "{}",
            st.ddl
        );
        assert_eq!(st.foreign_keys.len(), 1);
        assert!(st.indexes.iter().any(|(n, _)| n == "pedido_cliente"));
        // Values as the grid shows them.
        let r = c.query("SELECT total, foto FROM pedido ORDER BY id").unwrap();
        assert_eq!(r.rows[0], vec![Some("10.5".into()), Some("\\x00ff".into())]);
        assert_eq!(r.rows[1][0].as_deref(), Some("20.0"), "REAL stays a real number");
        // Generated SQL runs here: a page, the count, search, stats, value filter, FK picker.
        let pk = vec!["id".to_string()];
        let q = sql::PageQuery {
            schema: "main",
            table: "pedido",
            filter: "total > 5",
            order: Some(("total", false)),
            pk: &pk,
            page: 0,
        };
        assert_eq!(c.query(&sql::select_page(k, &q)).unwrap().rows.len(), 3);
        assert_eq!(c.count(&sql::count(k, "main", "pedido", "total > 8")).unwrap(), 2);
        let found = sql::search_condition(k, &["obs".to_string()], "AUT");
        assert_eq!(
            c.query(&format!("SELECT count(*) FROM pedido WHERE {found}"))
                .unwrap()
                .rows[0][0]
                .as_deref(),
            Some("3")
        );
        let (summary, top) = sql::column_stats(k, "main", "pedido", "cliente_id", "", true);
        c.query(&summary).unwrap();
        assert_eq!(
            c.query(&top).unwrap().rows[0],
            vec![Some("1".into()), Some("2".into())]
        );
        let expr = sql::value_expr(k, "cliente_id", "INTEGER");
        assert_eq!(
            c.query(&sql::distinct_values(k, "main", "pedido", &expr, "", ""))
                .unwrap()
                .rows
                .len(),
            2
        );
        let opts = sql::fk_options(k, &fk, Some("nome"), true, false, "bi");
        assert_eq!(
            c.query(&opts).unwrap().rows,
            vec![vec![Some("2".into()), Some("Bia".into())]]
        );
        // Grid edits: values stored verbatim, the batch all or nothing.
        for v in [r"a\", "it's", r"x\'; DROP TABLE pedido; --", "ção 🚀"] {
            let wh = sql::pk_where(k, &pk, &pk, &[Some("1".into())]);
            c.run_batch(
                &[sql::update_cell(k, "main", "cliente", "nome", Some(v), wh)],
                false,
            )
            .unwrap();
            assert_eq!(
                c.query("SELECT nome FROM cliente WHERE id = 1").unwrap().rows[0][0].as_deref(),
                Some(v)
            );
        }
        let wh = |id: &str| sql::pk_where(k, &pk, &pk, &[Some(id.into())]);
        let batch = [
            sql::update_cell(k, "main", "cliente", "nome", Some("X"), wh("2")),
            sql::update_cell(k, "main", "cliente", "nome", Some("Y"), wh("99")),
        ];
        assert!(c.run_batch(&batch, false).is_err());
        assert_eq!(
            c.query("SELECT nome FROM cliente WHERE id = 2").unwrap().rows[0][0].as_deref(),
            Some("Bia")
        );
        // Foreign keys are enforced; the trigger ran.
        assert!(c.query("INSERT INTO pedido (cliente_id) VALUES (999)").is_err());
        assert_eq!(
            c.query("SELECT obs FROM pedido WHERE id = 3").unwrap().rows[0][0].as_deref(),
            Some("auto")
        );
        // Table actions.
        for (action, name) in [
            (sql::TableAction::Duplicate(true), "copia"),
            (sql::TableAction::Rename, "copia2"),
            (sql::TableAction::Truncate, ""),
        ] {
            let table = if matches!(action, sql::TableAction::Duplicate(_)) {
                "cliente"
            } else {
                "copia"
            };
            let table = if matches!(action, sql::TableAction::Truncate) {
                "copia2"
            } else {
                table
            };
            c.query(&sql::table_action(k, "main", table, action, name))
                .unwrap();
        }
        assert_eq!(c.count("SELECT count(*) FROM copia2").unwrap(), 0);
        c.query(&sql::table_action(
            k,
            "main",
            "copia2",
            sql::TableAction::Drop,
            "",
        ))
        .unwrap();
        // What needs a server says so.
        assert!(c.activity().unwrap_err().to_string().contains("SQLite"));
        assert!(c.explain("SELECT 1", false).is_err());
        assert!(c.use_schema("x").is_err());
        // Cancelling a long statement from another thread.
        let canceller = c.canceller(&cfg, None);
        let stopper = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(300));
            canceller.cancel().unwrap();
        });
        let started = std::time::Instant::now();
        let long = "WITH RECURSIVE s(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM s) SELECT count(*) FROM s";
        assert!(c.query(long).is_err());
        assert!(started.elapsed() < std::time::Duration::from_secs(3));
        stopper.join().unwrap();
        assert!(c.query("SELECT 1").is_ok());
        // Read-only: the file opens, writes are refused.
        drop(c);
        let ro = crate::db::ConnConfig {
            read_only: true,
            ..cfg.clone()
        };
        let mut r = Conn::connect(&ro, None).unwrap();
        assert!(r.query("SELECT count(*) FROM cliente").is_ok());
        assert!(r.query("DELETE FROM cliente").is_err());
        // A missing file or one that isn't a database: clear errors, nothing created.
        let missing = crate::db::ConnConfig {
            database: format!("{}.nao-existe", cfg.database),
            ..cfg.clone()
        };
        assert!(
            Conn::connect(&missing, None)
                .err()
                .unwrap()
                .to_string()
                .contains("não encontrado")
        );
        assert!(!std::path::Path::new(&missing.database).exists());
        let _ = std::fs::remove_file(&cfg.database);
    }
}
