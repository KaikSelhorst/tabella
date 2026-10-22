//! Backup and restore with the database's own tools (`pg_dump`, `pg_restore`, `psql`,
//! `mysqldump`, `mysql`), for what "Export database" doesn't cover: big databases and Postgres'
//! compressed format. The password goes in the environment, never on the command line.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use crate::db::{ConnConfig, Kind};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Op {
    Backup,
    Restore,
}

/// A `.sql` file is plain text; any other name is Postgres' compressed custom format.
fn is_plain(file: &Path) -> bool {
    file.extension().is_some_and(|e| e.eq_ignore_ascii_case("sql"))
}

/// The arguments and the environment variables a tool needs.
pub type Plan = (Vec<String>, Vec<(&'static str, String)>);

/// How the tool checks the server, from the connection's TLS settings.
pub enum Tls<'a> {
    /// Encrypted when the server offers it, like the app's own connection.
    Off,
    /// Encrypted, any certificate ("Accept invalid certificate").
    Unverified,
    /// Encrypted, with a certificate from one of these authorities (a PEM file) naming the host.
    Verified { ca: &'a Path },
}

/// Whose MySQL tools are installed: MariaDB's spell the TLS options differently.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Flavor {
    MySql,
    MariaDb,
}

/// The program that runs one operation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tool {
    PgDump,
    Psql,
    PgRestore,
    MySqlDump,
    MySql,
}

impl Tool {
    /// `None` for a kind that has no such tools (SQLite is a file).
    pub fn for_op(kind: Kind, op: Op, file: &Path) -> Option<Self> {
        Some(match (kind, op) {
            (Kind::Postgres, Op::Backup) => Self::PgDump,
            (Kind::Postgres, Op::Restore) if is_plain(file) => Self::Psql,
            (Kind::Postgres, Op::Restore) => Self::PgRestore,
            (Kind::MySql, Op::Backup) => Self::MySqlDump,
            (Kind::MySql, Op::Restore) => Self::MySql,
            (Kind::Sqlite, _) => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::PgDump => "pg_dump",
            Self::Psql => "psql",
            Self::PgRestore => "pg_restore",
            Self::MySqlDump => "mysqldump",
            Self::MySql => "mysql",
        }
    }
}

/// The arguments and the environment variables for `tool` to work on `file`.
pub fn plan(
    cfg: &ConnConfig,
    tool: Tool,
    file: &Path,
    tls: &Tls,
    flavor: Flavor,
) -> Result<Plan, BackupError> {
    let path = file.to_string_lossy().into_owned();
    Ok(match tool {
        Tool::PgDump => pg_plan(
            cfg,
            tls,
            ["-F", if is_plain(file) { "p" } else { "c" }, "-f", &path],
        ),
        Tool::Psql => pg_plan(cfg, tls, ["-v", "ON_ERROR_STOP=1", "-q", "-f", &path]),
        Tool::PgRestore => pg_plan(cfg, tls, ["--no-owner", &path]),
        Tool::MySqlDump => mysql_plan(
            cfg,
            tls,
            flavor,
            [
                "--single-transaction".into(),
                "--routines".into(),
                "--triggers".into(),
                format!("--result-file={path}"),
            ],
        )?,
        // The file is fed through the program's input (see `run`).
        Tool::MySql => mysql_plan(cfg, tls, flavor, [])?,
    })
}

/// The Postgres tools' connection, TLS and password, then `tail`.
fn pg_plan<'a>(cfg: &ConnConfig, tls: &Tls, tail: impl IntoIterator<Item = &'a str>) -> Plan {
    let port = cfg.port.to_string();
    let dbname = pg_dbname(&cfg.database);
    let common = ["-h", &cfg.host, "-p", &port, "-U", &cfg.user, "-d", &dbname];
    let args: Vec<String> = common
        .into_iter()
        .map(String::from)
        .chain(tail.into_iter().map(String::from))
        .collect();
    let mut env = vec![("PGPASSWORD", cfg.password.to_string())];
    match tls {
        Tls::Off => env.push(("PGSSLMODE", "prefer".into())),
        Tls::Unverified => env.push(("PGSSLMODE", "require".into())),
        Tls::Verified { ca } => {
            env.push(("PGSSLMODE", "verify-full".into()));
            env.push(("PGSSLROOTCERT", ca.to_string_lossy().into_owned()));
        }
    }
    (args, env)
}

/// The MySQL tools' connection, TLS and password, then `tail` and the database.
fn mysql_plan(
    cfg: &ConnConfig,
    tls: &Tls,
    flavor: Flavor,
    tail: impl IntoIterator<Item = String>,
) -> Result<Plan, BackupError> {
    // The tools take any argument starting with `-` as an option, even after `--`, and some
    // load code (`--plugin-dir` with `--default-auth`).
    if cfg.database.starts_with('-') {
        return Err(BackupError::OptionLikeName);
    }
    let port = cfg.port.to_string();
    let mut args: Vec<String> = ["-h", &cfg.host, "-P", &port, "-u", &cfg.user]
        .map(String::from)
        .to_vec();
    args.extend(mysql_tls(tls, flavor));
    args.extend(tail);
    // Last, where the tools look for it.
    args.push(cfg.database.clone());
    Ok((args, vec![("MYSQL_PWD", cfg.password.to_string())]))
}

/// `-d` for the Postgres tools: a connection string holding only the name. Given as is, a name
/// with `=` (or starting with `postgres://`) is itself read as one, and its `host=` wins over
/// `-h`: the password would go to whatever server the name says. A database's name comes from
/// the server's list, where anyone allowed to create databases picks it.
fn pg_dbname(name: &str) -> String {
    format!("dbname='{}'", name.replace('\\', "\\\\").replace('\'', "\\'"))
}

/// The MySQL tools' TLS options. Left alone, they encrypt when the server offers it and check
/// nothing.
fn mysql_tls(tls: &Tls, flavor: Flavor) -> Vec<String> {
    match (tls, flavor) {
        (Tls::Off, _) => vec![],
        (Tls::Unverified, Flavor::MySql) => vec!["--ssl-mode=REQUIRED".into()],
        (Tls::Unverified, Flavor::MariaDb) => vec!["--ssl".into(), "--skip-ssl-verify-server-cert".into()],
        (Tls::Verified { ca }, Flavor::MySql) => vec![
            "--ssl-mode=VERIFY_IDENTITY".into(),
            format!("--ssl-ca={}", ca.display()),
        ],
        (Tls::Verified { ca }, Flavor::MariaDb) => vec![
            "--ssl".into(),
            "--ssl-verify-server-cert".into(),
            format!("--ssl-ca={}", ca.display()),
        ],
    }
}

/// Whose `program` is (its `--version` says MariaDB's).
fn detect_flavor(program: &str) -> Flavor {
    let mariadb = crate::programs::command(program)
        .and_then(|mut c| c.arg("--version").stdin(Stdio::null()).output().ok())
        .is_some_and(|out| String::from_utf8_lossy(&out.stdout).contains("MariaDB"));
    if mariadb { Flavor::MariaDb } else { Flavor::MySql }
}

/// The system's trusted certificate authorities as a PEM file, for the tools to check the
/// server's certificate against: they don't share the app's list, and without one they can't
/// check at all.
fn trusted_roots() -> Result<PathBuf, BackupError> {
    use base64::Engine;
    let found = rustls_native_certs::load_native_certs();
    if found.certs.is_empty() {
        let why = found
            .errors
            .first()
            .map_or("none found".into(), ToString::to_string);
        return Err(BackupError::Roots(why));
    }
    let mut pem = String::new();
    for cert in &found.certs {
        let b64 = base64::engine::general_purpose::STANDARD.encode(cert);
        pem.push_str("-----BEGIN CERTIFICATE-----\n");
        for line in b64.as_bytes().chunks(64) {
            pem.push_str(&String::from_utf8_lossy(line));
            pem.push('\n');
        }
        pem.push_str("-----END CERTIFICATE-----\n");
    }
    let path = crate::store::path("trusted-roots.pem").map_err(|e| BackupError::Roots(e.to_string()))?;
    crate::store::write_private(&path, &pem).map_err(|e| BackupError::Roots(e.to_string()))?;
    Ok(path)
}

#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    #[error("Backups through an SSH tunnel aren't supported: use Export database")]
    ViaSsh,
    #[error("SQLite is a single file: copy it instead")]
    Sqlite,
    /// The MySQL tools would read the name as one of their options.
    #[error("a database name starting with \"-\" can't be given to the MySQL tools: use Export database")]
    OptionLikeName,
    #[error(
        "couldn't read the system's trusted certificates ({0}): to back up anyway, tick “Accept invalid certificate”"
    )]
    Roots(String),
    #[error("{program} was not found: install the {client} client tools and try again")]
    NotFound {
        program: &'static str,
        client: &'static str,
    },
    #[error("couldn't open {}: {source}", path.display())]
    Open { path: PathBuf, source: std::io::Error },
    #[error("couldn't start {program}: {source}")]
    Start {
        program: &'static str,
        source: std::io::Error,
    },
    /// The tool ran and failed: the start of what it said, or its exit status.
    #[error("{program} failed: {detail}")]
    Failed { program: &'static str, detail: String },
    #[error("the backup tool stopped unexpectedly")]
    Stopped,
}

/// Runs the operation and waits for it. `Ok` carries a line to show; `Err` the tool's own
/// complaint. Blocking: call it from a thread.
pub fn run(cfg: &ConnConfig, op: Op, file: &Path) -> Result<String, BackupError> {
    if cfg.ssh.is_some() {
        return Err(BackupError::ViaSsh);
    }
    let tool = Tool::for_op(cfg.kind, op, file).ok_or(BackupError::Sqlite)?;
    let program = tool.name();
    let mut command = crate::programs::command(program).ok_or(BackupError::NotFound {
        program,
        client: cfg.kind.label(),
    })?;
    let roots = if cfg.ssl && !cfg.ssl_insecure {
        Some(trusted_roots()?)
    } else {
        None
    };
    let tls = match (&roots, cfg.ssl) {
        (Some(ca), _) => Tls::Verified { ca },
        (None, true) => Tls::Unverified,
        (None, false) => Tls::Off,
    };
    // Only the MySQL tools care.
    let flavor = if cfg.kind == Kind::MySql {
        detect_flavor(program)
    } else {
        Flavor::MySql
    };
    let (args, env) = plan(cfg, tool, file, &tls, flavor)?;
    command.args(&args);
    for (k, v) in env {
        command.env(k, v);
    }
    let feeds_input = cfg.kind == Kind::MySql && op == Op::Restore;
    if feeds_input {
        let input = std::fs::File::open(file).map_err(|source| BackupError::Open {
            path: file.to_path_buf(),
            source,
        })?;
        command.stdin(Stdio::from(input));
    } else {
        command.stdin(Stdio::null());
    }
    let out = command
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|source| BackupError::Start { program, source })?;
    if out.status.success() {
        let what = match op {
            Op::Backup => format!("Backup written to {}", file.display()),
            Op::Restore => format!("Restored {}", file.display()),
        };
        return Ok(what);
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let first: String = stderr
        .lines()
        .filter(|l| !l.trim().is_empty())
        .take(3)
        .collect::<Vec<_>>()
        .join(" · ");
    Err(BackupError::Failed {
        program,
        detail: if first.is_empty() {
            out.status.to_string()
        } else {
            first
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(kind: Kind) -> ConnConfig {
        ConnConfig {
            host: "db.example".into(),
            port: 5433,
            user: "me".into(),
            password: "s3cret".into(),
            database: "shop".into(),
            ..ConnConfig::new(kind)
        }
    }

    /// What the tools can't do is refused before looking for them.
    #[test]
    fn refused_before_running_anything() {
        let mut c = cfg(Kind::Postgres);
        c.ssh = Some(crate::db::SshConfig {
            host: "bastion".into(),
            port: 22,
            user: "me".into(),
            key_path: String::new(),
        });
        let file = Path::new("/nonexistent/shop.dump");
        assert!(matches!(run(&c, Op::Backup, file), Err(BackupError::ViaSsh)));
        assert!(matches!(
            run(&cfg(Kind::Sqlite), Op::Restore, file),
            Err(BackupError::Sqlite)
        ));
    }

    #[test]
    fn commands_carry_the_password_in_the_environment_only() {
        for (kind, op, file, program) in [
            (Kind::Postgres, Op::Backup, "/b/shop.dump", "pg_dump"),
            (Kind::Postgres, Op::Restore, "/b/shop.dump", "pg_restore"),
            (Kind::Postgres, Op::Restore, "/b/shop.sql", "psql"),
            (Kind::MySql, Op::Backup, "/b/shop.sql", "mysqldump"),
            (Kind::MySql, Op::Restore, "/b/shop.sql", "mysql"),
        ] {
            let tool = Tool::for_op(kind, op, Path::new(file)).unwrap();
            let p = tool.name();
            assert_eq!(p, program);
            let (args, env) = plan(&cfg(kind), tool, Path::new(file), &Tls::Off, Flavor::MySql).unwrap();
            assert!(args.iter().all(|a| !a.contains("s3cret")), "{p}: {args:?}");
            assert!(env.iter().any(|(_, v)| v == "s3cret"), "{p}");
            assert!(args.contains(&"db.example".to_string()));
            assert!(
                args.iter().any(|a| a == "shop" || a == "dbname='shop'"),
                "{p}: {args:?}"
            );
        }
    }

    /// A name that is a connection string would send the password to its `host=`.
    #[test]
    fn postgres_database_names_are_only_names() {
        let mut c = cfg(Kind::Postgres);
        c.database = r"host=evil.example dbname='x\'".into();
        for tool in [Tool::PgDump, Tool::PgRestore, Tool::Psql] {
            let args = plan(&c, tool, Path::new("a.dump"), &Tls::Off, Flavor::MySql)
                .unwrap()
                .0;
            assert!(
                args.windows(2)
                    .any(|w| w == ["-d", r"dbname='host=evil.example dbname=\'x\\\''"]),
                "{args:?}"
            );
            assert!(args.windows(2).any(|w| w == ["-h", "db.example"]));
        }
    }

    #[test]
    fn option_like_mysql_names_are_refused() {
        let mut c = cfg(Kind::MySql);
        c.database = "--plugin-dir=/tmp/x".into();
        for tool in [Tool::MySqlDump, Tool::MySql] {
            for flavor in [Flavor::MySql, Flavor::MariaDb] {
                assert!(matches!(
                    plan(&c, tool, Path::new("a.sql"), &Tls::Off, flavor),
                    Err(BackupError::OptionLikeName)
                ));
            }
        }
    }

    /// The tools check the server as the app does, not their own (unchecked) default.
    #[test]
    fn tls_follows_the_connection() {
        let ca = Path::new("/cfg/trusted-roots.pem");
        let pg_env = |tls: &Tls| {
            plan(
                &cfg(Kind::Postgres),
                Tool::PgDump,
                Path::new("a.dump"),
                tls,
                Flavor::MySql,
            )
            .unwrap()
            .1
        };
        let has = |env: &[(&str, String)], k: &str, v: &str| env.iter().any(|(ek, ev)| *ek == k && ev == v);
        assert!(has(&pg_env(&Tls::Off), "PGSSLMODE", "prefer"));
        assert!(has(&pg_env(&Tls::Unverified), "PGSSLMODE", "require"));
        let verified = pg_env(&Tls::Verified { ca });
        assert!(has(&verified, "PGSSLMODE", "verify-full"));
        assert!(has(&verified, "PGSSLROOTCERT", "/cfg/trusted-roots.pem"));

        let my_args = |tls: &Tls, flavor: Flavor| {
            plan(
                &cfg(Kind::MySql),
                Tool::MySqlDump,
                Path::new("a.sql"),
                tls,
                flavor,
            )
            .unwrap()
            .0
        };
        let has_arg = |args: &[String], a: &str| args.iter().any(|x| x == a);
        assert!(
            !my_args(&Tls::Off, Flavor::MySql)
                .iter()
                .any(|a| a.contains("ssl"))
        );
        assert!(has_arg(
            &my_args(&Tls::Unverified, Flavor::MySql),
            "--ssl-mode=REQUIRED"
        ));
        let mysql = my_args(&Tls::Verified { ca }, Flavor::MySql);
        assert!(has_arg(&mysql, "--ssl-mode=VERIFY_IDENTITY"));
        assert!(has_arg(&mysql, "--ssl-ca=/cfg/trusted-roots.pem"));
        let maria = my_args(&Tls::Verified { ca }, Flavor::MariaDb);
        assert!(has_arg(&maria, "--ssl-verify-server-cert") && has_arg(&maria, "--ssl"));
        assert!(has_arg(
            &my_args(&Tls::Unverified, Flavor::MariaDb),
            "--skip-ssl-verify-server-cert"
        ));
        // The database stays last, where the tools look for it.
        assert_eq!(mysql.last().map(String::as_str), Some("shop"));
    }

    #[test]
    fn postgres_format_follows_the_extension() {
        let args = |f: &str| {
            plan(
                &cfg(Kind::Postgres),
                Tool::PgDump,
                Path::new(f),
                &Tls::Off,
                Flavor::MySql,
            )
            .unwrap()
            .0
        };
        assert!(args("a.sql").windows(2).any(|w| w == ["-F", "p"]));
        assert!(args("a.dump").windows(2).any(|w| w == ["-F", "c"]));
        assert_eq!(Tool::for_op(Kind::Sqlite, Op::Backup, Path::new("a.sql")), None);
    }
}
