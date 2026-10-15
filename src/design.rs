//! "Nova tabela" / "Editar estrutura": the table as a list of columns and indexes, and the DDL
//! that turns the original into the edited version.

use crate::db::{Kind, TableStructure};

#[derive(Clone, Debug, PartialEq)]
pub struct ColDef {
    /// Name in the database; `None` for a column being added.
    pub original: Option<OrigCol>,
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    /// SQL expression (`0`, `now()`, `'novo'`); empty for none.
    pub default: String,
    pub pk: bool,
    pub dropped: bool,
}

/// How a column was before editing, to know what changed.
#[derive(Clone, Debug, PartialEq)]
pub struct OrigCol {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    pub default: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct IndexDef {
    /// Existing index (kept unless `dropped`), or a new one.
    pub existing: bool,
    pub name: String,
    /// Comma-separated column names, for new indexes; the definition, for existing ones.
    pub columns: String,
    pub unique: bool,
    pub dropped: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Design {
    pub kind: Kind,
    pub schema: String,
    pub table: String,
    /// Editing an existing table (ALTER) rather than creating one.
    pub existing: bool,
    pub columns: Vec<ColDef>,
    pub indexes: Vec<IndexDef>,
}

impl ColDef {
    pub fn new() -> Self {
        ColDef {
            original: None,
            name: String::new(),
            data_type: String::new(),
            nullable: true,
            default: String::new(),
            pk: false,
            dropped: false,
        }
    }
}

/// Types offered in the type field (any other can be typed).
pub fn common_types(kind: Kind) -> &'static [&'static str] {
    match kind {
        Kind::Postgres => &[
            "bigserial",
            "serial",
            "bigint",
            "integer",
            "smallint",
            "numeric(12, 2)",
            "double precision",
            "boolean",
            "text",
            "varchar(255)",
            "uuid",
            "date",
            "timestamp",
            "timestamptz",
            "jsonb",
        ],
        Kind::Sqlite => &[
            "INTEGER", "TEXT", "REAL", "NUMERIC", "BLOB", "BOOLEAN", "DATE", "DATETIME",
        ],
        Kind::MySql => &[
            "bigint",
            "int",
            "smallint",
            "decimal(12, 2)",
            "double",
            "boolean",
            "varchar(255)",
            "text",
            "date",
            "datetime",
            "timestamp",
            "json",
        ],
    }
}

impl Design {
    pub fn new_table(kind: Kind, schema: &str) -> Self {
        let id_type = match kind {
            Kind::Postgres => "bigserial",
            Kind::MySql => "bigint",
            // The INTEGER primary key is the row's own number: SQLite fills it in.
            Kind::Sqlite => "INTEGER",
        };
        Design {
            kind,
            schema: schema.to_string(),
            table: String::new(),
            existing: false,
            columns: vec![ColDef {
                name: "id".into(),
                data_type: id_type.into(),
                nullable: false,
                pk: true,
                ..ColDef::new()
            }],
            indexes: Vec::new(),
        }
    }

    pub fn from_structure(kind: Kind, schema: &str, table: &str, s: &TableStructure) -> Self {
        let columns = s
            .columns
            .iter()
            .map(|c| {
                let default = c.default.clone().unwrap_or_default();
                ColDef {
                    original: Some(OrigCol {
                        name: c.name.clone(),
                        data_type: c.data_type.clone(),
                        nullable: c.nullable,
                        default: default.clone(),
                    }),
                    name: c.name.clone(),
                    data_type: c.data_type.clone(),
                    nullable: c.nullable,
                    default,
                    pk: c.pk,
                    dropped: false,
                }
            })
            .collect();
        // The primary key's index goes with the key; it isn't offered for dropping.
        let indexes = s
            .indexes
            .iter()
            .filter(|(name, def)| {
                !(name == "PRIMARY" || name.ends_with("_pkey") || def.to_uppercase().contains("PRIMARY"))
            })
            .map(|(name, def)| IndexDef {
                existing: true,
                name: name.clone(),
                columns: def.clone(),
                unique: def.to_uppercase().contains("UNIQUE"),
                dropped: false,
            })
            .collect();
        Design {
            kind,
            schema: schema.to_string(),
            table: table.to_string(),
            existing: true,
            columns,
            indexes,
        }
    }

    /// What's wrong with the design, if anything (shown instead of running it).
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.table.trim().is_empty() {
            out.push("Dê um nome à tabela".to_string());
        }
        let live: Vec<&ColDef> = self.columns.iter().filter(|c| !c.dropped).collect();
        if live.is_empty() {
            out.push("A tabela precisa de pelo menos uma coluna".to_string());
        }
        for (i, c) in live.iter().enumerate() {
            if c.name.trim().is_empty() {
                out.push(format!("Coluna {} sem nome", i + 1));
            } else if live[..i]
                .iter()
                .any(|o| o.name.trim().eq_ignore_ascii_case(c.name.trim()))
            {
                out.push(format!("Coluna \"{}\" repetida", c.name.trim()));
            }
            if c.data_type.trim().is_empty() {
                out.push(format!("Coluna \"{}\" sem tipo", c.name.trim()));
            }
        }
        for ix in self.indexes.iter().filter(|i| !i.existing && !i.dropped) {
            if ix.columns.trim().is_empty() {
                out.push("Índice novo sem colunas".to_string());
            }
        }
        out
    }

    /// The DDL, one statement per entry. Empty when nothing changed.
    pub fn statements(&self) -> Vec<String> {
        if self.existing {
            self.alter()
        } else {
            self.create()
        }
    }

    pub fn script(&self) -> String {
        self.statements()
            .iter()
            .map(|s| format!("{s};"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn table_name(&self) -> String {
        self.kind.qualified(self.schema.trim(), self.table.trim())
    }

    fn column_sql(&self, c: &ColDef) -> String {
        let mut s = format!("{} {}", self.kind.quote_ident(c.name.trim()), c.data_type.trim());
        if !c.nullable {
            s.push_str(" NOT NULL");
        }
        if !c.default.trim().is_empty() {
            s.push_str(&format!(" DEFAULT {}", c.default.trim()));
        }
        s
    }

    fn create(&self) -> Vec<String> {
        let live: Vec<&ColDef> = self.columns.iter().filter(|c| !c.dropped).collect();
        let mut parts: Vec<String> = live.iter().map(|c| format!("  {}", self.column_sql(c))).collect();
        let pk: Vec<String> = live
            .iter()
            .filter(|c| c.pk)
            .map(|c| self.kind.quote_ident(c.name.trim()))
            .collect();
        if !pk.is_empty() {
            parts.push(format!("  PRIMARY KEY ({})", pk.join(", ")));
        }
        let mut out = vec![format!(
            "CREATE TABLE {} (\n{}\n)",
            self.table_name(),
            parts.join(",\n")
        )];
        out.extend(self.new_indexes());
        out
    }

    fn alter(&self) -> Vec<String> {
        let k = self.kind;
        let t = self.table_name();
        let q = |s: &str| k.quote_ident(s.trim());
        let mut out = Vec::new();
        for ix in self.indexes.iter().filter(|i| i.existing && i.dropped) {
            out.push(match k {
                Kind::Postgres | Kind::Sqlite => {
                    format!("DROP INDEX {}", k.qualified(self.schema.trim(), &ix.name))
                }
                Kind::MySql => format!("DROP INDEX {} ON {t}", q(&ix.name)),
            });
        }
        for c in &self.columns {
            match (&c.original, c.dropped) {
                (Some(o), true) => out.push(format!("ALTER TABLE {t} DROP COLUMN {}", q(&o.name))),
                (None, true) => {}
                (None, false) => out.push(format!("ALTER TABLE {t} ADD COLUMN {}", self.column_sql(c))),
                (Some(o), false) => {
                    let renamed = c.name.trim() != o.name;
                    if renamed {
                        out.push(format!(
                            "ALTER TABLE {t} RENAME COLUMN {} TO {}",
                            q(&o.name),
                            q(&c.name)
                        ));
                    }
                    let name = q(&c.name);
                    let type_changed = !c.data_type.trim().eq_ignore_ascii_case(o.data_type.trim());
                    let null_changed = c.nullable != o.nullable;
                    let default_changed = c.default.trim() != o.default.trim();
                    match k {
                        Kind::Postgres => {
                            if type_changed {
                                out.push(format!(
                                    "ALTER TABLE {t} ALTER COLUMN {name} TYPE {ty} USING {name}::{ty}",
                                    ty = c.data_type.trim()
                                ));
                            }
                            if null_changed {
                                let what = if c.nullable { "DROP" } else { "SET" };
                                out.push(format!("ALTER TABLE {t} ALTER COLUMN {name} {what} NOT NULL"));
                            }
                            if default_changed {
                                out.push(if c.default.trim().is_empty() {
                                    format!("ALTER TABLE {t} ALTER COLUMN {name} DROP DEFAULT")
                                } else {
                                    format!(
                                        "ALTER TABLE {t} ALTER COLUMN {name} SET DEFAULT {}",
                                        c.default.trim()
                                    )
                                });
                            }
                        }
                        // SQLite can't change a column in place: say so in the SQL shown.
                        Kind::Sqlite => {
                            if type_changed || null_changed || default_changed {
                                out.push(format!(
                                    "-- SQLite não altera tipo, NULL ou padrão de uma coluna existente ({}): \
                                     crie uma tabela nova e copie os dados",
                                    c.name.trim()
                                ));
                            }
                        }
                        // MySQL redefines the whole column at once.
                        Kind::MySql => {
                            if type_changed || null_changed || default_changed {
                                out.push(format!("ALTER TABLE {t} MODIFY COLUMN {}", self.column_sql(c)));
                            }
                        }
                    }
                }
            }
        }
        out.extend(self.new_indexes());
        out
    }

    fn new_indexes(&self) -> Vec<String> {
        let k = self.kind;
        self.indexes
            .iter()
            .filter(|i| !i.existing && !i.dropped && !i.columns.trim().is_empty())
            .map(|ix| {
                let cols: Vec<String> = ix
                    .columns
                    .split(',')
                    .map(str::trim)
                    .filter(|c| !c.is_empty())
                    .map(|c| k.quote_ident(c))
                    .collect();
                let name = if ix.name.trim().is_empty() {
                    let base: Vec<&str> = ix
                        .columns
                        .split(',')
                        .map(str::trim)
                        .filter(|c| !c.is_empty())
                        .collect();
                    format!("{}_{}_idx", self.table.trim(), base.join("_"))
                } else {
                    ix.name.trim().to_string()
                };
                format!(
                    "CREATE {}INDEX {} ON {} ({})",
                    if ix.unique { "UNIQUE " } else { "" },
                    k.quote_ident(&name),
                    self.table_name(),
                    cols.join(", ")
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::StructColumn;

    #[test]
    fn creates_with_primary_key_and_indexes() {
        let mut d = Design::new_table(Kind::Postgres, "public");
        d.table = "clientes".into();
        d.columns.push(ColDef {
            name: "nome".into(),
            data_type: "text".into(),
            nullable: false,
            ..ColDef::new()
        });
        d.columns.push(ColDef {
            name: "criado_em".into(),
            data_type: "timestamptz".into(),
            default: "now()".into(),
            ..ColDef::new()
        });
        d.indexes.push(IndexDef {
            existing: false,
            name: String::new(),
            columns: "nome, criado_em".into(),
            unique: true,
            dropped: false,
        });
        assert!(d.problems().is_empty());
        assert_eq!(
            d.script(),
            "CREATE TABLE \"public\".\"clientes\" (\n  \"id\" bigserial NOT NULL,\n  \"nome\" text NOT NULL,\n  \"criado_em\" timestamptz DEFAULT now(),\n  PRIMARY KEY (\"id\")\n);\nCREATE UNIQUE INDEX \"clientes_nome_criado_em_idx\" ON \"public\".\"clientes\" (\"nome\", \"criado_em\");"
        );
    }

    fn existing(kind: Kind) -> Design {
        let col = |name: &str, ty: &str, nullable, default: Option<&str>, pk| StructColumn {
            name: name.into(),
            data_type: ty.into(),
            nullable,
            default: default.map(Into::into),
            pk,
        };
        let s = TableStructure {
            columns: vec![
                col("id", "integer", false, None, true),
                col("nome", "text", true, None, false),
                col("velho", "text", true, None, false),
                col("saldo", "numeric(10,2)", true, Some("0"), false),
            ],
            indexes: vec![("t_nome_idx".into(), "CREATE INDEX t_nome_idx ON t (nome)".into())],
            foreign_keys: vec![],
            ddl: String::new(),
            ..Default::default()
        };
        Design::from_structure(kind, "s", "t", &s)
    }

    #[test]
    fn alters_only_what_changed() {
        for kind in [Kind::Postgres, Kind::MySql] {
            let d = existing(kind);
            assert!(d.statements().is_empty(), "{kind:?}: untouched means no DDL");
        }
        let mut d = existing(Kind::Postgres);
        d.columns[1].name = "nome_completo".into();
        d.columns[1].nullable = false;
        d.columns[2].dropped = true;
        d.columns[3].data_type = "numeric(12,2)".into();
        d.columns[3].default = String::new();
        d.columns.push(ColDef {
            name: "ativo".into(),
            data_type: "boolean".into(),
            default: "true".into(),
            ..ColDef::new()
        });
        d.indexes[0].dropped = true;
        assert_eq!(
            d.statements(),
            [
                "DROP INDEX \"s\".\"t_nome_idx\"",
                "ALTER TABLE \"s\".\"t\" RENAME COLUMN \"nome\" TO \"nome_completo\"",
                "ALTER TABLE \"s\".\"t\" ALTER COLUMN \"nome_completo\" SET NOT NULL",
                "ALTER TABLE \"s\".\"t\" DROP COLUMN \"velho\"",
                "ALTER TABLE \"s\".\"t\" ALTER COLUMN \"saldo\" TYPE numeric(12,2) USING \"saldo\"::numeric(12,2)",
                "ALTER TABLE \"s\".\"t\" ALTER COLUMN \"saldo\" DROP DEFAULT",
                "ALTER TABLE \"s\".\"t\" ADD COLUMN \"ativo\" boolean DEFAULT true",
            ]
        );

        let mut d = existing(Kind::MySql);
        d.columns[3].nullable = false;
        d.indexes[0].dropped = true;
        assert_eq!(
            d.statements(),
            [
                "DROP INDEX `t_nome_idx` ON `s`.`t`",
                "ALTER TABLE `s`.`t` MODIFY COLUMN `saldo` numeric(10,2) NOT NULL DEFAULT 0",
            ]
        );
    }

    #[test]
    fn reports_problems() {
        let mut d = Design::new_table(Kind::MySql, "db");
        d.columns.push(ColDef {
            name: "ID".into(),
            ..ColDef::new()
        });
        let p = d.problems();
        assert!(p.iter().any(|m| m.contains("nome à tabela")));
        assert!(p.iter().any(|m| m.contains("repetida")));
        assert!(p.iter().any(|m| m.contains("sem tipo")));
    }
}
