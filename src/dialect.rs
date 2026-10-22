//! The database engines and how each one quotes names and strings: the base the SQL builders
//! (`sql`) and the connections (`db`) share.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum Kind {
    Postgres,
    MySql,
    /// A database file on this computer: no server, no host or user.
    Sqlite,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Postgres => "PostgreSQL",
            Kind::MySql => "MySQL",
            Kind::Sqlite => "SQLite",
        }
    }

    pub fn default_port(self) -> u16 {
        match self {
            Kind::Postgres => 5432,
            Kind::MySql => 3306,
            Kind::Sqlite => 0,
        }
    }

    pub fn quote_ident(self, s: &str) -> String {
        match self {
            Kind::Postgres | Kind::Sqlite => format!("\"{}\"", s.replace('"', "\"\"")),
            Kind::MySql => format!("`{}`", s.replace('`', "``")),
        }
    }

    /// A string literal that reads back as `s` whatever the server's settings: run in filters,
    /// searches and dumps, and written to exported files.
    ///
    /// Postgres: with a backslash present this uses the `E'...'` form (like libpq's
    /// `PQescapeLiteral`), which reads the same whatever `standard_conforming_strings` is set to.
    /// MySQL: a backslash means one thing by default and another under `NO_BACKSLASH_ESCAPES`
    /// (a dump read back on such a server would double them), so text with one goes as UTF-8
    /// hex, which no mode reads differently.
    pub fn quote_str(self, s: &str) -> String {
        match self {
            Kind::Postgres if s.contains('\\') => {
                format!("E'{}'", s.replace('\\', "\\\\").replace('\'', "''"))
            }
            Kind::MySql if s.contains('\\') => {
                let hex: String = s.bytes().map(|b| format!("{b:02X}")).collect();
                format!("_utf8mb4 X'{hex}'")
            }
            // SQLite has no backslash escapes at all.
            Kind::Postgres | Kind::Sqlite | Kind::MySql => format!("'{}'", s.replace('\'', "''")),
        }
    }

    /// `schema.table` fully quoted. In MySQL the "schema" is the database.
    pub fn qualified(self, schema: &str, table: &str) -> String {
        format!("{}.{}", self.quote_ident(schema), self.quote_ident(table))
    }
}
