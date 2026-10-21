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

    /// A string literal for display, clipboard text and the Postgres execution path.
    ///
    /// Postgres: with a backslash present this uses the `E'...'` form (like libpq's
    /// `PQescapeLiteral`), which reads the same whatever `standard_conforming_strings` is set to.
    /// MySQL: escapes backslashes, which is wrong under `NO_BACKSLASH_ESCAPES`, so MySQL never
    /// executes these; its statements bind values as parameters instead (see `sql::Stmt`).
    pub fn quote_str(self, s: &str) -> String {
        match self {
            Kind::Postgres if s.contains('\\') => {
                format!("E'{}'", s.replace('\\', "\\\\").replace('\'', "''"))
            }
            // SQLite has no backslash escapes at all.
            Kind::Postgres | Kind::Sqlite => format!("'{}'", s.replace('\'', "''")),
            Kind::MySql => format!("'{}'", s.replace('\\', "\\\\").replace('\'', "''")),
        }
    }

    /// `schema.table` fully quoted. In MySQL the "schema" is the database.
    pub fn qualified(self, schema: &str, table: &str) -> String {
        format!("{}.{}", self.quote_ident(schema), self.quote_ident(table))
    }
}
