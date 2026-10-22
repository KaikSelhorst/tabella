//! What the database holds: schemas, tables, columns, foreign keys and other objects.

use super::*;

impl Conn {
    pub fn tables(&mut self) -> DbResult<Vec<TableRef>> {
        let sql = match self {
            Conn::Pg(_) => {
                "SELECT t.table_schema, t.table_name, t.table_type, c.reltuples::bigint, \
                 CASE WHEN c.relkind IN ('r', 'p', 'm') THEN pg_total_relation_size(c.oid) END \
                 FROM information_schema.tables t \
                 LEFT JOIN pg_catalog.pg_namespace n ON n.nspname = t.table_schema \
                 LEFT JOIN pg_catalog.pg_class c ON c.relnamespace = n.oid AND c.relname = t.table_name \
                 WHERE t.table_schema NOT IN ('pg_catalog', 'information_schema') \
                 AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.objid = c.oid AND d.deptype = 'e') \
                 ORDER BY 1, 2"
            }
            Conn::My(_) => {
                "SELECT table_schema, table_name, table_type, table_rows, data_length + index_length \
                 FROM information_schema.tables \
                 WHERE table_schema NOT IN ('information_schema', 'mysql', 'performance_schema', 'sys') \
                 ORDER BY 1, 2"
            }
            Conn::Lite(_) => crate::sqlite::TABLES,
        };
        let r = self.query(sql)?;
        r.rows
            .into_iter()
            .map(|row| {
                let [schema, name, ty, rows, size] = cells(row)?;
                let is_view = ty.contains("VIEW");
                Ok(TableRef {
                    size: size.parse().ok().filter(|_| !is_view),
                    search_key: name.to_lowercase(),
                    schema,
                    name,
                    is_view,
                    // Postgres reports -1 for tables that were never analyzed.
                    estimate: rows.parse().ok().filter(|n: &i64| *n >= 0 && !is_view),
                })
            })
            .collect()
    }

    /// Every `(table, column)` outside system schemas, for editor autocomplete.
    /// Every column of every table, for autocomplete, the schema search and the ER diagram.
    pub fn all_columns(&mut self) -> DbResult<Vec<SchemaColumn>> {
        let system = match self {
            Conn::Pg(_) => "'pg_catalog', 'information_schema'",
            Conn::My(_) | Conn::Lite(_) => "'information_schema', 'mysql', 'performance_schema', 'sys'",
        };
        let sql = match self {
            Conn::Pg(_) => format!(
                "SELECT c.table_schema, c.table_name, c.column_name, CASE c.data_type WHEN 'USER-DEFINED' THEN c.udt_name WHEN 'ARRAY' THEN substr(c.udt_name, 2) || '[]' ELSE c.data_type END, \
                 CASE WHEN pk.col IS NULL THEN '0' ELSE '1' END \
                 FROM information_schema.columns c \
                 LEFT JOIN (SELECT n.nspname AS s, cl.relname AS t, a.attname AS col \
                   FROM pg_index i JOIN pg_class cl ON cl.oid = i.indrelid \
                   JOIN pg_namespace n ON n.oid = cl.relnamespace \
                   JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = ANY (i.indkey) \
                   WHERE i.indisprimary) pk \
                   ON pk.s = c.table_schema AND pk.t = c.table_name AND pk.col = c.column_name \
                 WHERE c.table_schema NOT IN ({system}) ORDER BY 1, 2, c.ordinal_position"
            ),
            Conn::My(_) => format!(
                "SELECT table_schema, table_name, column_name, data_type, IF(column_key = 'PRI', '1', '0') \
                 FROM information_schema.columns \
                 WHERE table_schema NOT IN ({system}) ORDER BY 1, 2, ordinal_position"
            ),
            Conn::Lite(_) => crate::sqlite::ALL_COLUMNS.to_string(),
        };
        let r = self.query_limit(&sql, 100_000)?;
        r.rows
            .into_iter()
            .map(|row| {
                let [schema, table, column, data_type, pk] = cells(row)?;
                Ok(SchemaColumn {
                    schema,
                    table,
                    column,
                    data_type,
                    pk: pk == "1",
                })
            })
            .collect()
    }

    /// Postgres reports `USER-DEFINED` (enums, citext…) and `ARRAY` as `data_type`: the real name
    /// is in `udt_name`.
    pub fn columns(&mut self, schema: &str, table: &str) -> DbResult<Vec<ColumnInfo>> {
        let stmt = match self.kind() {
            Kind::Sqlite => return self.column_rows(&crate::sqlite::columns(table)),
            Kind::Postgres => Stmt::new(
                "SELECT c.column_name, CASE c.data_type WHEN 'USER-DEFINED' THEN c.udt_name WHEN 'ARRAY' THEN substr(c.udt_name, 2) || '[]' ELSE c.data_type END, EXISTS (\
                   SELECT 1 FROM information_schema.table_constraints tc \
                   JOIN information_schema.key_column_usage u \
                     USING (constraint_schema, constraint_name, table_schema, table_name) \
                   WHERE tc.constraint_type = 'PRIMARY KEY' AND tc.table_schema = c.table_schema \
                     AND tc.table_name = c.table_name AND u.column_name = c.column_name), \
                 c.is_nullable, \
                 c.column_default IS NOT NULL OR c.is_identity = 'YES' OR c.is_generated = 'ALWAYS' \
                 FROM information_schema.columns c WHERE c.table_schema = ",
            ),
            Kind::MySql => Stmt::new(
                "SELECT column_name, column_type, column_key = 'PRI', is_nullable, \
                 column_default IS NOT NULL OR extra LIKE '%auto_increment%' OR extra LIKE '%GENERATED%' \
                 FROM information_schema.columns WHERE table_schema = ",
            ),
        };
        let (order, table_col) = match self.kind() {
            Kind::Postgres => (" ORDER BY c.ordinal_position", " AND c.table_name = "),
            Kind::MySql | Kind::Sqlite => (" ORDER BY ordinal_position", " AND table_name = "),
        };
        let stmt = stmt
            .value(Some(schema))
            .sql(table_col)
            .value(Some(table))
            .sql(order);
        let mut cols = self.column_rows(&stmt)?;
        if self.kind() == Kind::Postgres {
            self.fill_enum_values(schema, table, &mut cols)?;
        }
        Ok(cols)
    }

    /// Fills `enum_values` of the columns whose type is a Postgres enum (not arrays or domains
    /// of one: those are written as text).
    fn fill_enum_values(&mut self, schema: &str, table: &str, cols: &mut [ColumnInfo]) -> DbResult<()> {
        let stmt = Stmt::new(
            "SELECT a.attname, json_agg(e.enumlabel ORDER BY e.enumsortorder)::text \
             FROM pg_attribute a \
             JOIN pg_class c ON c.oid = a.attrelid \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             JOIN pg_enum e ON e.enumtypid = a.atttypid \
             WHERE a.attnum > 0 AND NOT a.attisdropped AND n.nspname = ",
        )
        .value(Some(schema))
        .sql(" AND c.relname = ")
        .value(Some(table))
        .sql(" GROUP BY a.attname");
        for row in self.execute(&stmt)?.rows {
            let [name, labels] = cells(row)?;
            let labels: Vec<String> = serde_json::from_str(&labels)
                .map_err(|e| DbError::Other(format!("unreadable enum values of {name}: {e}")))?;
            if let Some(c) = cols.iter_mut().find(|c| c.name == name) {
                c.enum_values = labels;
            }
        }
        Ok(())
    }

    /// `(name, type, pk, nullable YES/NO, has default)` rows as columns.
    fn column_rows(&mut self, stmt: &Stmt) -> DbResult<Vec<ColumnInfo>> {
        let r = self.execute(stmt)?;
        r.rows
            .into_iter()
            .map(|row| {
                let [name, data_type, pk, nullable, has_default] = cells(row)?;
                let yes = |v: &str| matches!(v, "t" | "1" | "true");
                Ok(ColumnInfo {
                    enum_values: parse_mysql_enum(&data_type),
                    name,
                    data_type,
                    pk: yes(&pk),
                    nullable: nullable == "YES",
                    has_default: yes(&has_default),
                    references: None,
                })
            })
            .collect()
    }

    /// Columns of a table with their foreign keys filled in (single-column keys only: those are
    /// the ones a cell value can be followed through).
    pub fn columns_with_fks(&mut self, schema: &str, table: &str) -> DbResult<Vec<ColumnInfo>> {
        let mut cols = self.columns(schema, table)?;
        let stmt = match self.kind() {
            Kind::Sqlite => crate::sqlite::column_fks(table),
            Kind::Postgres => Stmt::new(
                "SELECT a.attname, fn.nspname, fc.relname, fa.attname \
                 FROM pg_constraint k \
                 JOIN pg_class c ON c.oid = k.conrelid JOIN pg_namespace n ON n.oid = c.relnamespace \
                 JOIN pg_class fc ON fc.oid = k.confrelid JOIN pg_namespace fn ON fn.oid = fc.relnamespace \
                 JOIN pg_attribute a ON a.attrelid = k.conrelid AND a.attnum = k.conkey[1] \
                 JOIN pg_attribute fa ON fa.attrelid = k.confrelid AND fa.attnum = k.confkey[1] \
                 WHERE k.contype = 'f' AND array_length(k.conkey, 1) = 1 AND n.nspname = ",
            )
            .value(Some(schema))
            .sql(" AND c.relname = ")
            .value(Some(table)),
            Kind::MySql => Stmt::new(
                "SELECT k.column_name, k.referenced_table_schema, k.referenced_table_name, k.referenced_column_name \
                 FROM information_schema.key_column_usage k \
                 WHERE k.referenced_table_name IS NOT NULL AND k.table_schema = ",
            )
            .value(Some(schema))
            .sql(" AND k.table_name = ")
            .value(Some(table))
            .sql(
                " AND (SELECT COUNT(*) FROM information_schema.key_column_usage k2 \
                 WHERE k2.constraint_schema = k.constraint_schema AND k2.constraint_name = k.constraint_name \
                 AND k2.table_name = k.table_name) = 1",
            ),
        };
        for row in self.execute(&stmt)?.rows {
            let [col, s, t, c] = cells(row)?;
            if let Some(info) = cols.iter_mut().find(|i| i.name == col) {
                info.references = Some(FkRef {
                    schema: s,
                    table: t,
                    column: c,
                });
            }
        }
        Ok(cols)
    }

    /// Where unqualified names go now, and where else they could: `(databases, current database,
    /// current schema)`. MySQL has no schemas inside a database (the schema is empty).
    pub fn location(&mut self) -> DbResult<(Vec<String>, String, String)> {
        // A SQLite file is its only database: the file's name stands for it.
        if self.kind() == Kind::Sqlite {
            let file = self
                .query(crate::sqlite::FILE)?
                .rows
                .into_iter()
                .next()
                .and_then(|r| r.into_iter().next().flatten());
            let name = file
                .as_deref()
                .and_then(|f| std::path::Path::new(f).file_stem())
                .map_or("main".to_string(), |n| n.to_string_lossy().into_owned());
            return Ok((Vec::new(), name, String::new()));
        }
        let (list, here) = match self.kind() {
            Kind::Postgres => (
                "SELECT datname FROM pg_database WHERE datallowconn AND NOT datistemplate ORDER BY 1",
                "SELECT current_database(), coalesce(current_schema(), '')",
            ),
            Kind::MySql | Kind::Sqlite => (
                "SELECT schema_name FROM information_schema.schemata \
                 WHERE schema_name NOT IN ('mysql', 'sys', 'performance_schema', 'information_schema') ORDER BY 1",
                "SELECT coalesce(DATABASE(), ''), ''",
            ),
        };
        let dbs = self
            .query(list)?
            .rows
            .into_iter()
            .filter_map(|r| r.into_iter().next().flatten())
            .collect();
        let [db, schema] = cells(self.query(here)?.rows.into_iter().next().unwrap_or_default())?;
        Ok((dbs, db, schema))
    }

    /// Unqualified names from now on: MySQL `USE db`; Postgres the schema first in
    /// `search_path` (public stays after it).
    pub fn use_schema(&mut self, name: &str) -> DbResult<()> {
        let k = self.kind();
        let sql = match k {
            Kind::MySql => format!("USE {}", k.quote_ident(name)),
            Kind::Postgres if name == "public" => "SET search_path TO public".to_string(),
            Kind::Postgres => format!("SET search_path TO {}, public", k.quote_ident(name)),
            Kind::Sqlite => {
                return Err(DbError::Other(
                    "SQLite has no other databases or schemas to switch to".into(),
                ));
            }
        };
        self.query(&sql).map(|_| ())
    }

    /// Functions, procedures, triggers, views and sequences outside the system schemas (and not
    /// installed by extensions).
    pub fn objects(&mut self) -> DbResult<Vec<DbObject>> {
        let rows: Vec<[String; 6]> = match self.kind() {
            Kind::Sqlite => self
                .query_limit(crate::sqlite::OBJECTS, 100_000)?
                .rows
                .into_iter()
                .map(cells)
                .collect::<DbResult<_>>()?,
            Kind::Postgres => {
                // Straight from the catalogs, without views like pg_sequences that open every
                // object (its last_value): one dropped meanwhile by another session would fail the
                // whole list. A function dropped meanwhile just has no arguments to show.
                let sql = "SELECT n.nspname, p.proname, CASE p.prokind WHEN 'p' THEN 'p' ELSE 'f' END, \
                     coalesce(pg_get_function_identity_arguments(p.oid), ''), '', p.oid::text \
                     FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace \
                     WHERE n.nspname NOT IN ('pg_catalog', 'information_schema') AND p.prokind IN ('f', 'p') \
                     AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.objid = p.oid AND d.deptype = 'e') \
                     UNION ALL \
                     SELECT n.nspname, t.tgname, 't', '', c.relname, '' \
                     FROM pg_trigger t JOIN pg_class c ON c.oid = t.tgrelid JOIN pg_namespace n ON n.oid = c.relnamespace \
                     WHERE NOT t.tgisinternal AND n.nspname NOT IN ('pg_catalog', 'information_schema') \
                     UNION ALL \
                     SELECT n.nspname, c.relname, 'v', '', '', '' \
                     FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
                     WHERE c.relkind = 'v' AND n.nspname NOT IN ('pg_catalog', 'information_schema') \
                     AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.deptype = 'e' AND d.objid = c.oid) \
                     UNION ALL \
                     SELECT n.nspname, c.relname, 's', '', '', '' \
                     FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
                     WHERE c.relkind = 'S' AND NOT pg_is_other_temp_schema(n.oid) \
                     ORDER BY 1, 3, 2";
                self.query_limit(sql, 100_000)?
                    .rows
                    .into_iter()
                    .map(cells)
                    .collect::<DbResult<_>>()?
            }
            Kind::MySql => {
                let sql = "SELECT routine_schema, routine_name, IF(routine_type = 'PROCEDURE', 'p', 'f'), \
                     coalesce((SELECT GROUP_CONCAT(CONCAT(coalesce(p.parameter_mode, 'IN'), ' ', p.parameter_name, ' ', \
                       p.dtd_identifier) ORDER BY p.ordinal_position SEPARATOR ', ') \
                       FROM information_schema.parameters p WHERE p.specific_schema = r.routine_schema \
                       AND p.specific_name = r.routine_name AND p.ordinal_position > 0), ''), '', '' \
                     FROM information_schema.routines r \
                     WHERE routine_schema NOT IN ('mysql', 'sys', 'performance_schema', 'information_schema') \
                     UNION ALL \
                     SELECT trigger_schema, trigger_name, 't', '', event_object_table, '' FROM information_schema.triggers \
                     WHERE trigger_schema NOT IN ('mysql', 'sys', 'performance_schema', 'information_schema') \
                     UNION ALL \
                     SELECT table_schema, table_name, 'v', '', '', '' FROM information_schema.views \
                     WHERE table_schema NOT IN ('mysql', 'sys', 'performance_schema', 'information_schema') \
                     ORDER BY 1, 3, 2";
                self.query_limit(sql, 100_000)?
                    .rows
                    .into_iter()
                    .map(cells)
                    .collect::<DbResult<_>>()?
            }
        };
        Ok(rows
            .into_iter()
            .map(|[schema, name, kind, args, table, id]| DbObject {
                schema,
                name,
                kind: match kind.as_str() {
                    "p" => ObjKind::Procedure,
                    "t" => ObjKind::Trigger,
                    "v" => ObjKind::View,
                    "s" => ObjKind::Sequence,
                    _ => ObjKind::Function,
                },
                args,
                table,
                id,
            })
            .collect())
    }

    /// The object's definition as SQL that recreates it (CREATE OR REPLACE where the engine has it).
    pub fn object_source(&mut self, o: &DbObject) -> DbResult<String> {
        let first_cell = |r: QueryResult, col: usize| -> DbResult<String> {
            r.rows
                .into_iter()
                .next()
                .and_then(|row| row.into_iter().nth(col).flatten())
                .ok_or_else(|| DbError::Other(format!("couldn't find the definition of {}", o.name)))
        };
        let k = self.kind();
        match k {
            Kind::Sqlite => {
                // No CREATE OR REPLACE either: like MySQL, the DROP is there to uncomment.
                let r = self.execute(
                    &Stmt::new("SELECT sql FROM sqlite_master WHERE name = ").value(Some(&o.name)),
                )?;
                let body = first_cell(r, 0)?;
                let what = if o.kind == ObjKind::View {
                    "VIEW"
                } else {
                    "TRIGGER"
                };
                Ok(format!(
                    "-- To recreate: uncomment the DROP and run everything (F5).\n-- DROP {what} IF EXISTS {};\n{body};",
                    k.quote_ident(&o.name)
                ))
            }
            Kind::Postgres => {
                // Names go in as values; the server quotes them into a regprocedure / regclass.
                let qualified = |name: &str| {
                    Stmt::new("quote_ident(")
                        .value(Some(&o.schema))
                        .sql(") || '.' || quote_ident(")
                        .value(Some(name))
                        .sql(")")
                };
                let stmt = match o.kind {
                    ObjKind::Function | ObjKind::Procedure => Stmt::new("SELECT pg_get_functiondef(CAST(")
                        .value(Some(&o.id))
                        .sql(" AS oid))"),
                    ObjKind::Trigger => Stmt::new(
                        "SELECT pg_get_triggerdef(t.oid, true) || ';' FROM pg_trigger t \
                         JOIN pg_class c ON c.oid = t.tgrelid JOIN pg_namespace n ON n.oid = c.relnamespace \
                         WHERE n.nspname = ",
                    )
                    .value(Some(&o.schema))
                    .sql(" AND c.relname = ")
                    .value(Some(&o.table))
                    .sql(" AND t.tgname = ")
                    .value(Some(&o.name)),
                    ObjKind::View => Stmt::new("SELECT 'CREATE OR REPLACE VIEW ' || ")
                        .sql(qualified(&o.name).render(k))
                        .sql(" || E' AS\\n' || pg_get_viewdef((")
                        .sql(qualified(&o.name).render(k))
                        .sql(")::regclass, true)"),
                    ObjKind::Sequence => Stmt::new(
                        "SELECT 'CREATE SEQUENCE ' || quote_ident(schemaname) || '.' || quote_ident(sequencename) \
                         || E'\\n  INCREMENT BY ' || increment_by || E'\\n  MINVALUE ' || min_value \
                         || E'\\n  MAXVALUE ' || max_value || E'\\n  START WITH ' || start_value \
                         || E';\\n-- current value: ' || coalesce(last_value::text, 'not used yet') \
                         FROM pg_sequences WHERE schemaname = ",
                    )
                    .value(Some(&o.schema))
                    .sql(" AND sequencename = ")
                    .value(Some(&o.name)),
                };
                first_cell(self.execute(&stmt)?, 0)
            }
            Kind::MySql => {
                let what = match o.kind {
                    ObjKind::Function => "FUNCTION",
                    ObjKind::Procedure => "PROCEDURE",
                    ObjKind::Trigger => "TRIGGER",
                    ObjKind::View => "VIEW",
                    ObjKind::Sequence => return Err(DbError::Other("MySQL has no sequences".into())),
                };
                let r = self.query(&format!("SHOW CREATE {what} {}", k.qualified(&o.schema, &o.name)))?;
                // The definition column: 2nd for views, 3rd for routines and triggers.
                let col = if o.kind == ObjKind::View { 1 } else { 2 };
                let body = first_cell(r, col)?;
                // MySQL has no CREATE OR REPLACE for these: dropping first is left to the user.
                // The body has its own ";" inside, so it runs with F5 (the whole text), not
                // Ctrl+Enter.
                Ok(match o.kind {
                    ObjKind::View => format!("{};", body.replacen("CREATE ", "CREATE OR REPLACE ", 1)),
                    _ => format!(
                        "-- To recreate: uncomment the DROP and run everything (F5).\n-- DROP {what} IF EXISTS {};\n{body}",
                        k.qualified(&o.schema, &o.name)
                    ),
                })
            }
        }
    }

    /// Every single-column foreign key of the connection (for the ER diagram).
    pub fn all_fks(&mut self) -> DbResult<Vec<FkEdge>> {
        let lite = crate::sqlite::all_fks();
        let sql = match self.kind() {
            Kind::Sqlite => lite.as_str(),
            Kind::Postgres => {
                "SELECT n.nspname, c.relname, a.attname, fn.nspname, fc.relname, fa.attname \
                 FROM pg_constraint k \
                 JOIN pg_class c ON c.oid = k.conrelid JOIN pg_namespace n ON n.oid = c.relnamespace \
                 JOIN pg_class fc ON fc.oid = k.confrelid JOIN pg_namespace fn ON fn.oid = fc.relnamespace \
                 JOIN pg_attribute a ON a.attrelid = k.conrelid AND a.attnum = k.conkey[1] \
                 JOIN pg_attribute fa ON fa.attrelid = k.confrelid AND fa.attnum = k.confkey[1] \
                 WHERE k.contype = 'f' AND array_length(k.conkey, 1) = 1 \
                 AND n.nspname NOT IN ('pg_catalog', 'information_schema') ORDER BY 1, 2, 3"
            }
            Kind::MySql => {
                "SELECT k.table_schema, k.table_name, k.column_name, \
                 k.referenced_table_schema, k.referenced_table_name, k.referenced_column_name \
                 FROM information_schema.key_column_usage k \
                 WHERE k.referenced_table_name IS NOT NULL \
                 AND k.table_schema NOT IN ('mysql', 'sys', 'performance_schema', 'information_schema') \
                 AND (SELECT COUNT(*) FROM information_schema.key_column_usage k2 \
                  WHERE k2.constraint_schema = k.constraint_schema AND k2.constraint_name = k.constraint_name \
                  AND k2.table_name = k.table_name) = 1 ORDER BY 1, 2, 3"
            }
        };
        self.query_limit(sql, 100_000)?
            .rows
            .into_iter()
            .map(|row| {
                let [schema, table, column, ref_schema, ref_table, ref_column] = cells(row)?;
                Ok(FkEdge {
                    schema,
                    table,
                    column,
                    ref_schema,
                    ref_table,
                    ref_column,
                })
            })
            .collect()
    }

    /// Single-column foreign keys in other tables that point at this one ("who points here").
    pub fn incoming_fks(&mut self, schema: &str, table: &str) -> DbResult<Vec<IncomingFk>> {
        let stmt = match self.kind() {
            Kind::Sqlite => crate::sqlite::incoming_fks(table),
            Kind::Postgres => Stmt::new(
                "SELECT n.nspname, c.relname, a.attname, fa.attname \
                 FROM pg_constraint k \
                 JOIN pg_class c ON c.oid = k.conrelid JOIN pg_namespace n ON n.oid = c.relnamespace \
                 JOIN pg_class fc ON fc.oid = k.confrelid JOIN pg_namespace fn ON fn.oid = fc.relnamespace \
                 JOIN pg_attribute a ON a.attrelid = k.conrelid AND a.attnum = k.conkey[1] \
                 JOIN pg_attribute fa ON fa.attrelid = k.confrelid AND fa.attnum = k.confkey[1] \
                 WHERE k.contype = 'f' AND array_length(k.conkey, 1) = 1 AND fn.nspname = ",
            )
            .value(Some(schema))
            .sql(" AND fc.relname = ")
            .value(Some(table))
            .sql(" ORDER BY 1, 2, 3"),
            Kind::MySql => Stmt::new(
                "SELECT k.table_schema, k.table_name, k.column_name, k.referenced_column_name \
                 FROM information_schema.key_column_usage k \
                 WHERE k.referenced_table_schema = ",
            )
            .value(Some(schema))
            .sql(" AND k.referenced_table_name = ")
            .value(Some(table))
            .sql(
                " AND (SELECT COUNT(*) FROM information_schema.key_column_usage k2 \
                 WHERE k2.constraint_schema = k.constraint_schema AND k2.constraint_name = k.constraint_name \
                 AND k2.table_name = k.table_name) = 1 ORDER BY 1, 2, 3",
            ),
        };
        self.execute(&stmt)?
            .rows
            .into_iter()
            .map(|row| {
                let [schema, table, column, target] = cells(row)?;
                Ok(IncomingFk {
                    schema,
                    table,
                    column,
                    target,
                })
            })
            .collect()
    }
}

/// The values of a MySQL `enum('a','b')` column type (`''` is a quote inside a value); empty for
/// any other type, or one that doesn't parse.
pub(super) fn parse_mysql_enum(ty: &str) -> Vec<String> {
    let Some(rest) = ty
        .get(..5)
        .filter(|p| p.eq_ignore_ascii_case("enum("))
        .map(|_| &ty[5..])
    else {
        return Vec::new();
    };
    let mut values = Vec::new();
    let mut chars = rest.chars().peekable();
    loop {
        if chars.next() != Some('\'') {
            return Vec::new();
        }
        let mut value = String::new();
        loop {
            match chars.next() {
                Some('\'') if chars.peek() == Some(&'\'') => {
                    chars.next();
                    value.push('\'');
                }
                Some('\'') => break,
                Some('\\') => value.extend(chars.next()),
                Some(c) => value.push(c),
                None => return Vec::new(),
            }
        }
        values.push(value);
        match chars.next() {
            Some(',') => {}
            Some(')') => return values,
            _ => return Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_mysql_enum;

    #[test]
    fn mysql_enum_types_give_their_values() {
        assert_eq!(parse_mysql_enum("enum('a','b','c')"), ["a", "b", "c"]);
        assert_eq!(parse_mysql_enum("ENUM('x')"), ["x"]);
        // Commas, parentheses and doubled quotes live inside values.
        assert_eq!(
            parse_mysql_enum("enum('a,b','c)','it''s','')"),
            ["a,b", "c)", "it's", ""]
        );
        for not_enum in ["varchar(20)", "set('a','b')", "int", "enum('a'", "enum(a)", ""] {
            assert!(parse_mysql_enum(not_enum).is_empty(), "{not_enum}");
        }
    }
}
