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

/// A program, its arguments and the environment variables it needs.
pub type Plan = (&'static str, Vec<String>, Vec<(&'static str, String)>);

/// The program, its arguments and the environment variables for one operation. `None` for a
/// kind that has no such tools (SQLite is just a file).
pub fn plan(cfg: &ConnConfig, op: Op, file: &Path) -> Option<Plan> {
    let path = file.to_string_lossy().into_owned();
    let port = cfg.port.to_string();
    let (program, args, env): Plan = match (cfg.kind, op) {
        (Kind::Postgres, Op::Backup) => {
            let format = if is_plain(file) { "p" } else { "c" };
            (
                "pg_dump",
                [
                    "-h",
                    &cfg.host,
                    "-p",
                    &port,
                    "-U",
                    &cfg.user,
                    "-d",
                    &cfg.database,
                    "-F",
                    format,
                    "-f",
                    &path,
                ]
                .map(String::from)
                .to_vec(),
                vec![("PGPASSWORD", cfg.password.to_string())],
            )
        }
        (Kind::Postgres, Op::Restore) => {
            let common = ["-h", &cfg.host, "-p", &port, "-U", &cfg.user, "-d", &cfg.database];
            let (program, tail): (&'static str, Vec<&str>) = if is_plain(file) {
                ("psql", vec!["-v", "ON_ERROR_STOP=1", "-q", "-f", &path])
            } else {
                ("pg_restore", vec!["--no-owner", &path])
            };
            (
                program,
                common.iter().copied().chain(tail).map(String::from).collect(),
                vec![("PGPASSWORD", cfg.password.to_string())],
            )
        }
        (Kind::MySql, Op::Backup) => (
            "mysqldump",
            [
                "-h",
                &cfg.host,
                "-P",
                &port,
                "-u",
                &cfg.user,
                "--single-transaction",
                "--routines",
                "--triggers",
                &format!("--result-file={path}"),
                &cfg.database,
            ]
            .map(String::from)
            .to_vec(),
            vec![("MYSQL_PWD", cfg.password.to_string())],
        ),
        // The file is fed through the program's input (see `run`).
        (Kind::MySql, Op::Restore) => (
            "mysql",
            ["-h", &cfg.host, "-P", &port, "-u", &cfg.user, &cfg.database]
                .map(String::from)
                .to_vec(),
            vec![("MYSQL_PWD", cfg.password.to_string())],
        ),
        (Kind::Sqlite, _) => return None,
    };
    Some((program, args, env))
}

#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    #[error("Backups through an SSH tunnel aren't supported: use Export database")]
    ViaSsh,
    #[error("SQLite is a single file: copy it instead")]
    Sqlite,
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
    let Some((program, args, env)) = plan(cfg, op, file) else {
        return Err(BackupError::Sqlite);
    };
    let mut command = crate::programs::command(program).ok_or(BackupError::NotFound {
        program,
        client: cfg.kind.label(),
    })?;
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

    #[test]
    fn commands_carry_the_password_in_the_environment_only() {
        for (kind, op, file, program) in [
            (Kind::Postgres, Op::Backup, "/b/shop.dump", "pg_dump"),
            (Kind::Postgres, Op::Restore, "/b/shop.dump", "pg_restore"),
            (Kind::Postgres, Op::Restore, "/b/shop.sql", "psql"),
            (Kind::MySql, Op::Backup, "/b/shop.sql", "mysqldump"),
            (Kind::MySql, Op::Restore, "/b/shop.sql", "mysql"),
        ] {
            let (p, args, env) = plan(&cfg(kind), op, Path::new(file)).unwrap();
            assert_eq!(p, program);
            assert!(args.iter().all(|a| !a.contains("s3cret")), "{p}: {args:?}");
            assert!(env.iter().any(|(_, v)| v == "s3cret"), "{p}");
            assert!(args.contains(&"db.example".to_string()) && args.contains(&"shop".to_string()));
        }
    }

    #[test]
    fn postgres_format_follows_the_extension() {
        let args = |f: &str| plan(&cfg(Kind::Postgres), Op::Backup, Path::new(f)).unwrap().1;
        assert!(args("a.sql").windows(2).any(|w| w == ["-F", "p"]));
        assert!(args("a.dump").windows(2).any(|w| w == ["-F", "c"]));
        assert!(plan(&cfg(Kind::Sqlite), Op::Backup, Path::new("a.sql")).is_none());
    }
}
