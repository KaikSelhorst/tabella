//! What the command line asks for: `tabella [--conn NAME] [URL] [FILE.sql …]`.

use std::path::PathBuf;

pub const USAGE: &str = "Usage: tabella [--conn NAME] [URL] [FILE.sql ...]\n\
  --conn NAME, -c NAME   connect to the saved connection with this name (or id)\n\
  URL                    postgres://, mysql:// or sqlite: connection, not saved\n\
                         (a password in a URL is visible to other programs on this computer)\n\
  FILE.sql               open the file in an editor tab once connected";

#[derive(Debug, Default, PartialEq)]
pub struct Launch {
    pub conn: Option<String>,
    pub url: Option<String>,
    pub files: Vec<PathBuf>,
}

fn is_url(arg: &str) -> bool {
    let lower = arg.to_ascii_lowercase();
    [
        "postgres://",
        "postgresql://",
        "mysql://",
        "mariadb://",
        "sqlite:",
    ]
    .iter()
    .any(|p| lower.starts_with(p))
}

/// The arguments after the program name. Files become absolute paths, so they still mean the
/// same thing if the working directory changes.
pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Launch, String> {
    let mut out = Launch::default();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if let Some(name) = arg.strip_prefix("--conn=") {
            out.conn = Some(name.to_string());
        } else if arg == "--conn" || arg == "-c" {
            out.conn = Some(args.next().ok_or("--conn needs the name of a saved connection")?);
        } else if arg.starts_with('-') && arg.len() > 1 {
            return Err(format!("unknown option {arg}"));
        } else if is_url(&arg) {
            out.url = Some(arg);
        } else {
            let path = PathBuf::from(&arg);
            out.files.push(std::path::absolute(&path).unwrap_or(path));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Launch, String> {
        super::parse(args.iter().map(|a| a.to_string()))
    }

    #[test]
    fn connection_url_and_files() {
        assert_eq!(parse(&[]).unwrap(), Launch::default());
        // Absolute on every system (`/tmp/a.sql` lacks a drive on Windows, so it gets one).
        let file = std::env::temp_dir().join("a.sql");
        let l = parse(&[
            "--conn",
            "Northstar",
            &file.to_string_lossy(),
            "postgres://u@h/db",
        ])
        .unwrap();
        assert_eq!(l.conn.as_deref(), Some("Northstar"));
        assert_eq!(l.url.as_deref(), Some("postgres://u@h/db"));
        assert_eq!(l.files, [file]);
        assert_eq!(parse(&["-c", "x"]).unwrap().conn.as_deref(), Some("x"));
        assert_eq!(parse(&["--conn=y"]).unwrap().conn.as_deref(), Some("y"));
        assert!(parse(&["sqlite:shop.db"]).unwrap().url.is_some());
        assert!(parse(&["relative.sql"]).unwrap().files[0].is_absolute());
        assert!(parse(&["--conn"]).is_err());
        assert!(parse(&["--nope"]).is_err());
    }
}
