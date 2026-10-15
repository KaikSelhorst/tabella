//! A table's columns, keys, indexes and DDL, as each engine describes them.

use super::*;

impl Conn {
    pub fn structure(&mut self, schema: &str, table: &str) -> DbResult<TableStructure> {
        match self.kind() {
            Kind::Postgres => self.pg_structure(schema, table),
            Kind::MySql => self.my_structure(schema, table),
            Kind::Sqlite => self.lite_structure(table),
        }
    }

    /// SQLite keeps each table's CREATE as written: that is the DDL. Foreign keys can't be added
    /// afterwards there, so they stay inside it (a dump turns their checks off instead).
    fn lite_structure(&mut self, table: &str) -> DbResult<TableStructure> {
        let cell = |r: &Vec<Option<String>>, i: usize| r.get(i).cloned().flatten().unwrap_or_default();
        let cols = self.execute(
            &Stmt::new(
                "SELECT name, type, \"notnull\" = 0 AND pk = 0, dflt_value, pk > 0, hidden \
                 FROM pragma_table_xinfo(",
            )
            .value(Some(table))
            .sql(") WHERE hidden <> 1 ORDER BY cid"),
        )?;
        let columns: Vec<StructColumn> = cols
            .rows
            .iter()
            .map(|r| StructColumn {
                name: cell(r, 0),
                data_type: cell(r, 1),
                nullable: cell(r, 2) == "1",
                default: r[3].clone(),
                pk: cell(r, 4) == "1",
            })
            .collect();
        let generated = cols
            .rows
            .iter()
            .filter(|r| matches!(cell(r, 5).as_str(), "2" | "3"))
            .map(|r| cell(r, 0))
            .collect();
        // Indexes made by PRIMARY KEY / UNIQUE have no SQL of their own: described by columns.
        let auto = self.execute(
            &Stmt::new(
                "SELECT il.name, CASE il.origin WHEN 'pk' THEN 'PRIMARY KEY' ELSE 'UNIQUE' END || ' (' || \
                 (SELECT group_concat(ii.name, ', ') FROM pragma_index_info(il.name) ii) || ')' \
                 FROM pragma_index_list(",
            )
            .value(Some(table))
            .sql(") il WHERE il.origin <> 'c' ORDER BY 1"),
        )?;
        let own = self.execute(
            &Stmt::new("SELECT name, sql FROM sqlite_master WHERE type = 'index' AND sql IS NOT NULL AND tbl_name = ")
                .value(Some(table))
                .sql(" ORDER BY name"),
        )?;
        let fks = self.execute(
            &Stmt::new(
                "SELECT id, group_concat(\"from\", ', '), \"table\", group_concat(coalesce(\"to\", ''), ', ') \
                 FROM pragma_foreign_key_list(",
            )
            .value(Some(table))
            .sql(") GROUP BY id ORDER BY id"),
        )?;
        let create = self.execute(
            &Stmt::new("SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ").value(Some(table)),
        )?;
        let mut ddl = create
            .rows
            .first()
            .map(|r| format!("{};", cell(r, 0)))
            .unwrap_or_default();
        for r in &own.rows {
            let _ = write!(ddl, "\n{};", cell(r, 1));
        }
        Ok(TableStructure {
            columns,
            indexes: auto
                .rows
                .iter()
                .chain(&own.rows)
                .map(|r| (cell(r, 0), cell(r, 1)))
                .collect(),
            foreign_keys: fks
                .rows
                .iter()
                .map(|r| {
                    let to = cell(r, 3);
                    let to = if to.trim_matches([',', ' ']).is_empty() {
                        String::new()
                    } else {
                        format!(" ({to})")
                    };
                    (
                        format!("fk_{}", cell(r, 0)),
                        format!("FOREIGN KEY ({}) REFERENCES {}{to}", cell(r, 1), cell(r, 2)),
                    )
                })
                .collect(),
            ddl_without_fks: ddl.clone(),
            ddl,
            generated,
            ..Default::default()
        })
    }

    fn pg_structure(&mut self, schema: &str, table: &str) -> DbResult<TableStructure> {
        let k = Kind::Postgres;
        // The table's oid, from the names as values (never spliced into the SQL text).
        let rel = |sql: &str| {
            Stmt::new(sql)
                .sql("(quote_ident(")
                .value(Some(schema))
                .sql(") || '.' || quote_ident(")
                .value(Some(table))
                .sql("))::regclass")
        };
        let cols = self.execute(
            &rel(
                "SELECT a.attname, format_type(a.atttypid, a.atttypmod), NOT a.attnotnull, \
             pg_get_expr(d.adbin, d.adrelid), \
             EXISTS (SELECT 1 FROM pg_index i WHERE i.indrelid = a.attrelid AND i.indisprimary \
                     AND a.attnum = ANY (i.indkey)), \
             a.attidentity::text, a.attgenerated::text \
             FROM pg_attribute a LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
             WHERE a.attnum > 0 AND NOT a.attisdropped AND a.attrelid = ",
            )
            .sql(" ORDER BY a.attnum"),
        )?;
        // identity ('a' always, 'd' by default) and generated ('s' stored), per column.
        let flags: Vec<(String, String)> = cols
            .rows
            .iter()
            .map(|r| {
                let f = |i: usize| r.get(i).cloned().flatten().unwrap_or_default();
                (f(5), f(6))
            })
            .collect();
        let columns: Vec<StructColumn> = cols
            .rows
            .into_iter()
            .map(|r| StructColumn {
                name: r[0].clone().unwrap_or_default(),
                data_type: r[1].clone().unwrap_or_default(),
                nullable: r[2].as_deref() == Some("t"),
                default: r[3].clone(),
                pk: r[4].as_deref() == Some("t"),
            })
            .collect();
        let cons = self.execute(&rel(
            "SELECT conname, contype::text, pg_get_constraintdef(oid) FROM pg_constraint WHERE conrelid = ",
        ).sql(" ORDER BY CASE contype WHEN 'p' THEN 0 WHEN 'u' THEN 1 WHEN 'f' THEN 2 ELSE 3 END, conname"))?;
        let idx = self.execute(
            &rel("SELECT i.relname, pg_get_indexdef(i.oid), \
             EXISTS (SELECT 1 FROM pg_constraint c WHERE c.conindid = i.oid) \
             FROM pg_index x JOIN pg_class i ON i.oid = x.indexrelid WHERE x.indrelid = ")
            .sql(" ORDER BY 1"),
        )?;
        let cell = |r: &Vec<Option<String>>, i: usize| r.get(i).cloned().flatten().unwrap_or_default();

        let qualified = k.qualified(schema, table);
        let (mut generated, mut sequences, mut serial_columns) = (Vec::new(), Vec::new(), Vec::new());
        let lines: Vec<String> = columns
            .iter()
            .zip(&flags)
            .map(|(c, (identity, gen_kind))| {
                let mut l = format!("    {} {}", k.quote_ident(&c.name), c.data_type);
                if !c.nullable {
                    l += " NOT NULL";
                }
                match (identity.as_str(), gen_kind.as_str(), &c.default) {
                    (_, "s", Some(expr)) => {
                        let _ = write!(l, " GENERATED ALWAYS AS ({expr}) STORED");
                        generated.push(c.name.clone());
                    }
                    ("a" | "d", _, _) => {
                        let how = if identity == "a" { "ALWAYS" } else { "BY DEFAULT" };
                        let _ = write!(l, " GENERATED {how} AS IDENTITY");
                        serial_columns.push(c.name.clone());
                    }
                    (_, _, Some(d)) => {
                        let _ = write!(l, " DEFAULT {d}");
                        // nextval('customers_id_seq'::regclass): the name as SQL writes it.
                        if let Some(seq) = d
                            .strip_prefix("nextval('")
                            .and_then(|r| r.split_once("'::regclass)"))
                            .map(|(name, _)| name.replace("''", "'"))
                        {
                            sequences.push((c.name.clone(), seq));
                            serial_columns.push(c.name.clone());
                        }
                    }
                    _ => {}
                }
                l
            })
            .collect();
        let constraint =
            |r: &Vec<Option<String>>| format!("CONSTRAINT {} {}", k.quote_ident(&cell(r, 0)), cell(r, 2));
        let is_fk = |r: &&Vec<Option<String>>| r[1].as_deref() == Some("f");
        let create = |with_fks: bool| {
            let mut all = lines.clone();
            all.extend(
                cons.rows
                    .iter()
                    .filter(|r| with_fks || !is_fk(r))
                    .map(|r| format!("    {}", constraint(r))),
            );
            let mut ddl = format!("CREATE TABLE {qualified} (\n{}\n);", all.join(",\n"));
            // Indexes backing a constraint are created by it; the rest need their own statement.
            for r in idx.rows.iter().filter(|r| r[2].as_deref() != Some("t")) {
                let _ = write!(ddl, "\n{};", cell(r, 1));
            }
            ddl
        };
        let (ddl, ddl_without_fks) = (create(true), create(false));
        let fk_statements = cons
            .rows
            .iter()
            .filter(is_fk)
            .map(|r| format!("ALTER TABLE {qualified} ADD {};", constraint(r)))
            .collect();
        Ok(TableStructure {
            ddl_without_fks,
            fk_statements,
            generated,
            sequences,
            serial_columns,
            columns,
            indexes: idx.rows.iter().map(|r| (cell(r, 0), cell(r, 1))).collect(),
            foreign_keys: cons
                .rows
                .iter()
                .filter(|r| r[1].as_deref() == Some("f"))
                .map(|r| (cell(r, 0), cell(r, 2)))
                .collect(),
            ddl,
        })
    }

    fn my_structure(&mut self, schema: &str, table: &str) -> DbResult<TableStructure> {
        let filter = |sql: &str| {
            Stmt::new(sql)
                .value(Some(schema))
                .sql(" AND table_name = ")
                .value(Some(table))
        };
        let cols = self.execute(&filter(
            "SELECT column_name, column_type, is_nullable = 'YES', column_default, column_key = 'PRI', extra \
             FROM information_schema.columns WHERE table_schema = ",
        ).sql(" ORDER BY ordinal_position"))?;
        let generated = cols
            .rows
            .iter()
            .filter(|r| r[5].as_deref().is_some_and(|e| e.contains("GENERATED")))
            .filter_map(|r| r[0].clone())
            .collect();
        let columns = cols
            .rows
            .into_iter()
            .map(|r| {
                let extra = r[5].clone().unwrap_or_default();
                StructColumn {
                    name: r[0].clone().unwrap_or_default(),
                    data_type: r[1].clone().unwrap_or_default(),
                    nullable: r[2].as_deref() == Some("1"),
                    // auto_increment has no default value, but it is what the column gets.
                    default: r[3].clone().or_else(|| (!extra.is_empty()).then_some(extra)),
                    pk: r[4].as_deref() == Some("1"),
                }
            })
            .collect();
        let idx = self.execute(
            &filter(
                "SELECT index_name, IF(non_unique = 0, 'UNIQUE ', ''), \
             GROUP_CONCAT(column_name ORDER BY seq_in_index SEPARATOR ', ') \
             FROM information_schema.statistics WHERE table_schema = ",
            )
            .sql(" GROUP BY index_name, non_unique ORDER BY index_name"),
        )?;
        let fks = self.execute(&filter(
            "SELECT constraint_name, CONCAT('(', GROUP_CONCAT(column_name ORDER BY ordinal_position SEPARATOR ', '), \
             ') REFERENCES ', referenced_table_name, ' (', \
             GROUP_CONCAT(referenced_column_name ORDER BY ordinal_position SEPARATOR ', '), ')') \
             FROM information_schema.key_column_usage WHERE referenced_table_name IS NOT NULL AND table_schema = ",
        ).sql(" GROUP BY constraint_name, referenced_table_name ORDER BY constraint_name"))?;
        let create = self.query(&format!(
            "SHOW CREATE TABLE {}",
            Kind::MySql.qualified(schema, table)
        ))?;
        let cell = |r: &Vec<Option<String>>, i: usize| r.get(i).cloned().flatten().unwrap_or_default();
        let ddl = create
            .rows
            .first()
            .map(|r| format!("{};", cell(r, 1)))
            .unwrap_or_default();
        Ok(TableStructure {
            // A dump turns FOREIGN_KEY_CHECKS off, so they can stay in the CREATE.
            ddl_without_fks: ddl.clone(),
            generated,
            columns,
            indexes: idx
                .rows
                .iter()
                .map(|r| (cell(r, 0), format!("{}({})", cell(r, 1), cell(r, 2))))
                .collect(),
            foreign_keys: fks.rows.iter().map(|r| (cell(r, 0), cell(r, 1))).collect(),
            ddl,
            ..Default::default()
        })
    }
}
