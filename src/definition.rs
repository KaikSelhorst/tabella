//! "Go to definition" for a name in the SQL editor: what a Ctrl+click on `orders`, `calc_total`
//! or `o.status` should open. Pure: it looks the name up in the catalog the sidebar already has.

use crate::db::{DbObject, ObjKind, SchemaColumn, TableRef};

/// Where a name leads.
#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    /// A table: its data (what Ctrl+click on a table has always opened).
    Table { schema: String, name: String },
    /// A view, function, procedure, trigger or sequence: its code.
    Object(DbObject),
    /// A column: the structure of its table.
    Column {
        schema: String,
        table: String,
        column: String,
    },
}

impl Target {
    /// "view orders_open", for the tooltip.
    pub fn describe(&self) -> String {
        match self {
            Target::Table { schema, name } => format!("table {schema}.{name}"),
            Target::Object(o) => {
                let what = match o.kind {
                    ObjKind::Function => "function",
                    ObjKind::Procedure => "procedure",
                    ObjKind::Trigger => "trigger",
                    ObjKind::View => "view",
                    ObjKind::Sequence => "sequence",
                };
                // Overloads differ only in their arguments.
                if matches!(o.kind, ObjKind::Function | ObjKind::Procedure) {
                    format!("{what} {}.{}({})", o.schema, o.name, o.args)
                } else {
                    format!("{what} {}.{}", o.schema, o.name)
                }
            }
            Target::Column {
                schema,
                table,
                column,
            } => format!("column {column} of {schema}.{table}"),
        }
    }
}

/// What the lookup can see.
pub struct Catalog<'a> {
    pub tables: &'a [TableRef],
    pub objects: &'a [DbObject],
    pub columns: &'a [SchemaColumn],
    /// Where unqualified names go: wins over other schemas.
    pub current_schema: &'a str,
}

/// Words that follow a table in `FROM t WHERE …` without being its alias.
const NOT_ALIASES: &[&str] = &[
    "where",
    "on",
    "join",
    "left",
    "right",
    "inner",
    "outer",
    "full",
    "cross",
    "natural",
    "group",
    "order",
    "limit",
    "union",
    "having",
    "using",
    "set",
    "values",
    "select",
    "from",
    "and",
    "or",
    "offset",
    "returning",
    "lateral",
    "window",
    "fetch",
    "for",
    "except",
    "intersect",
    "into",
    "when",
    "then",
    "else",
    "end",
    "case",
    "is",
    "not",
    "in",
    "like",
    "between",
    "as",
];

/// Where `word` (as the editor shows it: `t`, `s.t`, `"S"."T"`, `alias.col`, `s.t.col`) leads,
/// most likely first; empty when it isn't a name the database knows. `statement` is the SQL
/// around it, which tells aliases and the tables a bare column can belong to.
pub fn resolve(cat: &Catalog, word: &str, statement: &str) -> Vec<Target> {
    let parts: Vec<String> = word
        .split('.')
        .map(|p| p.trim_matches(|c| c == '"' || c == '`').to_string())
        .collect();
    if parts.is_empty() || parts.len() > 3 || parts.iter().any(String::is_empty) {
        return Vec::new();
    }
    // An exact spelling wins; failing that, case doesn't matter.
    let exact = lookup(cat, &parts, statement, true);
    if !exact.is_empty() {
        return exact;
    }
    lookup(cat, &parts, statement, false)
}

fn lookup(cat: &Catalog, parts: &[String], statement: &str, exact: bool) -> Vec<Target> {
    let same = |a: &str, b: &str| if exact { a == b } else { a.eq_ignore_ascii_case(b) };
    let named = |schema: Option<&str>, name: &str| -> Vec<Target> {
        let in_schema = |s: &str| schema.is_none_or(|want| same(s, want));
        let mut out: Vec<Target> = cat
            .tables
            .iter()
            .filter(|t| same(&t.name, name) && in_schema(&t.schema))
            .map(|t| table_target(cat, t))
            .collect();
        out.extend(
            cat.objects
                .iter()
                .filter(|o| o.kind != ObjKind::View && same(&o.name, name) && in_schema(&o.schema))
                .cloned()
                .map(Target::Object),
        );
        // The current schema first, then the rest in catalog order.
        out.sort_by_key(|t| match t {
            Target::Table { schema, .. } => schema != cat.current_schema,
            Target::Object(o) => o.schema != cat.current_schema,
            Target::Column { .. } => true,
        });
        out
    };
    let column_of = |schema: &str, table: &str, column: &str| -> Option<Target> {
        cat.columns
            .iter()
            .find(|c| c.schema == schema && c.table == table && same(&c.column, column))
            .map(|c| Target::Column {
                schema: c.schema.clone(),
                table: c.table.clone(),
                column: c.column.clone(),
            })
    };
    match parts {
        [name] => {
            let found = named(None, name);
            if !found.is_empty() {
                return found;
            }
            // A bare column: in the tables this statement reads from.
            let mut out = Vec::new();
            for (schema, table) in tables_in(cat, statement, exact) {
                if let Some(t) = column_of(&schema, &table, name)
                    && !out.contains(&t)
                {
                    out.push(t);
                }
            }
            out
        }
        [first, second] => {
            let found = named(Some(first), second);
            if !found.is_empty() {
                return found;
            }
            // `alias.column` or `table.column`.
            table_for(cat, first, statement, exact)
                .and_then(|(schema, table)| column_of(&schema, &table, second))
                .into_iter()
                .collect()
        }
        [schema, table, column] => cat
            .tables
            .iter()
            .find(|t| same(&t.schema, schema) && same(&t.name, table))
            .and_then(|t| column_of(&t.schema, &t.name, column))
            .into_iter()
            .collect(),
        _ => Vec::new(),
    }
}

/// A view leads to its code, a table to its data.
fn table_target(cat: &Catalog, t: &TableRef) -> Target {
    if t.is_view
        && let Some(o) = cat
            .objects
            .iter()
            .find(|o| o.kind == ObjKind::View && o.schema == t.schema && o.name == t.name)
    {
        return Target::Object(o.clone());
    }
    Target::Table {
        schema: t.schema.clone(),
        name: t.name.clone(),
    }
}

/// Identifier-ish words of the statement, in order (dots and quotes kept in each word).
fn words(statement: &str) -> Vec<String> {
    let is_word = |c: char| c.is_alphanumeric() || matches!(c, '_' | '$' | '.' | '"' | '`');
    let mut out = Vec::new();
    let mut current = String::new();
    for c in statement.chars() {
        if is_word(c) {
            current.push(c);
        } else if !current.is_empty() {
            out.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

fn unquote(w: &str) -> Vec<String> {
    w.split('.')
        .map(|p| p.trim_matches(|c| c == '"' || c == '`').to_string())
        .collect()
}

/// The table a word names, as (schema, table).
fn table_word(cat: &Catalog, word: &str, exact: bool) -> Option<(String, String)> {
    let same = |a: &str, b: &str| if exact { a == b } else { a.eq_ignore_ascii_case(b) };
    let parts = unquote(word);
    let (schema, name) = match parts.as_slice() {
        [n] => (None, n.as_str()),
        [s, n] => (Some(s.as_str()), n.as_str()),
        _ => return None,
    };
    let mut hits = cat
        .tables
        .iter()
        .filter(|t| same(&t.name, name) && schema.is_none_or(|s| same(&t.schema, s)));
    let first = hits.next()?;
    // Unqualified: the current schema's table when there are several.
    let best = if schema.is_none() && first.schema != cat.current_schema {
        cat.tables
            .iter()
            .find(|t| same(&t.name, name) && t.schema == cat.current_schema)
            .unwrap_or(first)
    } else {
        first
    };
    Some((best.schema.clone(), best.name.clone()))
}

/// Tables the statement mentions, in order, without repeats.
fn tables_in(cat: &Catalog, statement: &str, exact: bool) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for w in words(statement) {
        if let Some(t) = table_word(cat, &w, exact)
            && !out.contains(&t)
        {
            out.push(t);
        }
    }
    out
}

/// The table `name` stands for in the statement: an alias (`FROM orders o`, `JOIN orders AS o`)
/// or the table's own name.
fn table_for(cat: &Catalog, name: &str, statement: &str, exact: bool) -> Option<(String, String)> {
    let same = |a: &str, b: &str| if exact { a == b } else { a.eq_ignore_ascii_case(b) };
    let ws = words(statement);
    for (i, w) in ws.iter().enumerate() {
        let Some(table) = table_word(cat, w, exact) else {
            continue;
        };
        let mut next = i + 1;
        if ws.get(next).is_some_and(|a| a.eq_ignore_ascii_case("as")) {
            next += 1;
        }
        if let Some(alias) = ws.get(next)
            && !alias.contains('.')
            && !NOT_ALIASES.contains(&alias.to_ascii_lowercase().as_str())
            && same(alias.trim_matches(|c| c == '"' || c == '`'), name)
        {
            return Some(table);
        }
    }
    table_word(cat, name, exact)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(schema: &str, name: &str, is_view: bool) -> TableRef {
        TableRef {
            schema: schema.into(),
            name: name.into(),
            is_view,
            estimate: None,
            search_key: name.to_lowercase(),
            size: None,
        }
    }

    fn object(schema: &str, name: &str, kind: ObjKind, args: &str) -> DbObject {
        DbObject {
            schema: schema.into(),
            name: name.into(),
            kind,
            args: args.into(),
            table: String::new(),
            id: String::new(),
        }
    }

    fn column(schema: &str, table: &str, column: &str) -> SchemaColumn {
        SchemaColumn {
            schema: schema.into(),
            table: table.into(),
            column: column.into(),
            data_type: "text".into(),
            pk: false,
        }
    }

    struct Fixture {
        tables: Vec<TableRef>,
        objects: Vec<DbObject>,
        columns: Vec<SchemaColumn>,
    }

    impl Fixture {
        fn new() -> Self {
            Fixture {
                tables: vec![
                    table("public", "orders", false),
                    table("public", "customers", false),
                    table("public", "orders_open", true),
                    table("archive", "orders", false),
                ],
                objects: vec![
                    object("public", "orders_open", ObjKind::View, ""),
                    object("public", "calc_total", ObjKind::Function, "integer"),
                    object("public", "orders_id_seq", ObjKind::Sequence, ""),
                ],
                columns: vec![
                    column("public", "orders", "id"),
                    column("public", "orders", "status"),
                    column("public", "customers", "id"),
                    column("public", "customers", "name"),
                    column("archive", "orders", "legacy_flag"),
                ],
            }
        }

        fn at(&self, word: &str, statement: &str) -> Vec<Target> {
            let cat = Catalog {
                tables: &self.tables,
                objects: &self.objects,
                columns: &self.columns,
                current_schema: "public",
            };
            resolve(&cat, word, statement)
        }
    }

    fn col(schema: &str, table: &str, column: &str) -> Target {
        Target::Column {
            schema: schema.into(),
            table: table.into(),
            column: column.into(),
        }
    }

    #[test]
    fn tables_views_and_functions() {
        let f = Fixture::new();
        // The current schema's table comes first.
        assert_eq!(
            f.at("orders", "SELECT * FROM orders")[0],
            Target::Table {
                schema: "public".into(),
                name: "orders".into()
            }
        );
        assert_eq!(
            f.at("archive.orders", "")[0],
            Target::Table {
                schema: "archive".into(),
                name: "orders".into()
            }
        );
        // A view leads to its code, not its rows.
        assert!(matches!(&f.at("orders_open", "")[0], Target::Object(o) if o.kind == ObjKind::View));
        assert!(
            matches!(&f.at("calc_total", "SELECT calc_total(1)")[0], Target::Object(o) if o.kind == ObjKind::Function)
        );
        assert!(
            matches!(&f.at("\"Orders_Id_Seq\"", "")[0], Target::Object(o) if o.kind == ObjKind::Sequence)
        );
        // Not a name the database knows.
        assert!(f.at("select", "select 1").is_empty());
        assert!(f.at("a.b.c.d", "").is_empty());
    }

    /// The picker lists `describe()`: overloads must read differently.
    #[test]
    fn descriptions_tell_overloads_and_tables_apart() {
        let f = |args: &str| Target::Object(object("public", "calc_total", ObjKind::Function, args));
        assert_eq!(f("integer").describe(), "function public.calc_total(integer)");
        assert_ne!(f("integer").describe(), f("integer, numeric").describe());
        assert_eq!(
            col("archive", "orders", "id").describe(),
            "column id of archive.orders"
        );
    }

    #[test]
    fn columns_through_aliases_and_the_statement() {
        let f = Fixture::new();
        let sql =
            "SELECT o.status, c.name FROM orders o JOIN customers AS c ON c.id = o.id WHERE status = 'x'";
        assert_eq!(f.at("o.status", sql), [col("public", "orders", "status")]);
        assert_eq!(f.at("c.name", sql), [col("public", "customers", "name")]);
        assert_eq!(f.at("orders.status", sql), [col("public", "orders", "status")]);
        // A bare column: the tables the statement reads from; `id` is in two of them.
        assert_eq!(f.at("status", sql), [col("public", "orders", "status")]);
        assert_eq!(
            f.at("id", sql),
            [col("public", "orders", "id"), col("public", "customers", "id")]
        );
        // A column of a table the statement doesn't read isn't guessed.
        assert!(f.at("name", "SELECT * FROM orders").is_empty());
        assert_eq!(
            f.at("public.customers.name", ""),
            [col("public", "customers", "name")]
        );
        // `WHERE` after a table is not its alias.
        assert!(
            f.at("where.status", "SELECT * FROM orders WHERE status = 1")
                .is_empty()
        );
    }

    /// Ctrl+hover resolves whatever word is under the pointer, in whatever statement.
    #[test]
    fn any_word_any_statement() {
        let cat = Catalog {
            tables: &[],
            objects: &[],
            columns: &[],
            current_schema: "public",
        };
        crate::testgen::each_text(11, 2000, |s, g| {
            let word = g.text_of(3);
            let _ = resolve(&cat, &word, s);
            let _ = resolve(&cat, s, &word);
        });
    }
}
