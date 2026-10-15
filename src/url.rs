//! Connection URLs like `postgres://user:pass@host:5432/db?sslmode=require`, as given by cloud
//! providers, `.env` files and other tools.

use crate::db::{ConnConfig, Kind};

/// Fills the connection fields of `cfg` from `url`. Name, color and other app settings are kept.
pub fn apply(cfg: &mut ConnConfig, url: &str) -> Result<(), String> {
    let url = url.trim();
    // sqlite:///C:/dados/loja.db, sqlite:///home/eu/loja.db, sqlite:loja.db: a file path.
    if url.len() > 7 && url[..7].eq_ignore_ascii_case("sqlite:") {
        let path = url[7..].trim_start_matches("//");
        // "/C:/…" is a Windows drive, not a path under the root.
        let windows_drive = path.len() > 2 && path.starts_with('/') && path.as_bytes()[2] == b':';
        let path = decode(if windows_drive { &path[1..] } else { path })?;
        if path.is_empty() {
            return Err("URL sem o caminho do arquivo".into());
        }
        cfg.kind = Kind::Sqlite;
        cfg.database = path;
        cfg.host.clear();
        cfg.user.clear();
        cfg.password.clear();
        cfg.port = 0;
        cfg.ssl = false;
        cfg.ssh = None;
        return Ok(());
    }
    let (scheme, rest) = url
        .split_once("://")
        .ok_or("URL sem esquema (ex: postgres://…)")?;
    let kind = match scheme.to_ascii_lowercase().as_str() {
        "postgres" | "postgresql" => Kind::Postgres,
        "mysql" | "mariadb" => Kind::MySql,
        other => return Err(format!("esquema não suportado: {other}")),
    };
    let (rest, query) = rest.split_once('?').unwrap_or((rest, ""));
    let (authority, database) = rest.split_once('/').unwrap_or((rest, ""));
    let (userinfo, hostport) = match authority.rsplit_once('@') {
        Some((u, h)) => (Some(u), h),
        None => (None, authority),
    };
    let (host, port) = split_host_port(hostport)?;
    if host.is_empty() {
        return Err("URL sem host".into());
    }

    let mut next = ConnConfig::new(kind);
    next.host = host;
    next.port = port.unwrap_or(kind.default_port());
    if let Some(u) = userinfo {
        let (user, pass) = u.split_once(':').unwrap_or((u, ""));
        next.user = decode(user)?;
        next.password = decode(pass)?;
    }
    next.database = decode(database.trim_end_matches('/'))?;
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let v = decode(v)?.to_ascii_lowercase();
        match k.to_ascii_lowercase().as_str() {
            // libpq's "require" skips the certificate check, but cloud providers hand out URLs
            // with it as the secure choice: the certificate is checked here too. A server with
            // its own CA fails with a hint to tick "Aceitar certificado inválido".
            "sslmode" | "ssl-mode" | "ssl_mode" => match v.as_str() {
                "require" | "required" | "verify-ca" | "verify-full" | "verify_ca" | "verify_identity" => {
                    (next.ssl, next.ssl_insecure) = (true, false)
                }
                _ => (next.ssl, next.ssl_insecure) = (false, false),
            },
            "ssl" | "tls" => next.ssl = matches!(v.as_str(), "true" | "1" | "yes"),
            _ => {}
        }
    }

    cfg.kind = next.kind;
    cfg.host = next.host;
    cfg.port = next.port;
    cfg.user = next.user;
    cfg.password = next.password;
    cfg.database = next.database;
    cfg.ssl = next.ssl;
    cfg.ssl_insecure = next.ssl_insecure;
    Ok(())
}

fn split_host_port(s: &str) -> Result<(String, Option<u16>), String> {
    let port = |p: &str| {
        p.parse::<u16>()
            .map(Some)
            .map_err(|_| format!("porta inválida: {p}"))
    };
    if let Some(v6) = s.strip_prefix('[') {
        let (host, after) = v6.split_once(']').ok_or("IPv6 sem ']'")?;
        let p = match after.strip_prefix(':') {
            Some(p) => port(p)?,
            None => None,
        };
        return Ok((host.to_string(), p));
    }
    match s.rsplit_once(':') {
        Some((h, p)) => Ok((decode(h)?, port(p)?)),
        None => Ok((decode(s)?, None)),
    }
}

/// Percent-decoding (`%40` → `@`), for passwords with special characters.
fn decode(s: &str) -> Result<String, String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = s.get(i + 1..i + 3).ok_or("escape % incompleto")?;
            out.push(u8::from_str_radix(hex, 16).map_err(|_| format!("escape inválido: %{hex}"))?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| "texto inválido após decodificar %".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(url: &str) -> ConnConfig {
        let mut c = ConnConfig::new(Kind::Postgres);
        c.name = "manter".into();
        apply(&mut c, url).unwrap();
        c
    }

    #[test]
    fn postgres_url_with_everything() {
        let c = parse("postgresql://app%40corp:p%3Ass%2Fw@db.exemplo.com:6543/loja?sslmode=require");
        assert_eq!(
            (c.kind, c.host.as_str(), c.port),
            (Kind::Postgres, "db.exemplo.com", 6543)
        );
        assert_eq!(
            (c.user.as_str(), c.password.as_str(), c.database.as_str()),
            ("app@corp", "p:ss/w", "loja")
        );
        assert!(c.ssl && !c.ssl_insecure, "require still checks the certificate");
        assert_eq!(c.name, "manter", "app settings are kept");
    }

    #[test]
    fn mysql_defaults_and_variants() {
        let c = parse("mysql://root@localhost/test");
        assert_eq!((c.kind, c.port, c.password.as_str()), (Kind::MySql, 3306, ""));
        let c = parse("mysql://u:p@[::1]:3307/?ssl-mode=VERIFY_IDENTITY");
        assert_eq!((c.host.as_str(), c.port, c.database.as_str()), ("::1", 3307, ""));
        assert!(c.ssl && !c.ssl_insecure);
    }

    #[test]
    fn bad_urls_are_explained() {
        let mut c = ConnConfig::new(Kind::Postgres);
        assert!(apply(&mut c, "localhost:5432").is_err());
        assert!(apply(&mut c, "redis://x").unwrap_err().contains("redis"));
        assert!(
            apply(&mut c, "postgres://h:abc/db")
                .unwrap_err()
                .contains("porta")
        );
        assert!(apply(&mut c, "postgres://u:%zz@h/db").is_err());
    }

    #[test]
    fn sqlite_urls_are_file_paths() {
        let mut c = ConnConfig::new(Kind::Postgres);
        apply(&mut c, "sqlite:///C:/dados/loja%20nova.db").unwrap();
        assert_eq!(
            (c.kind, c.database.as_str(), c.host.as_str()),
            (Kind::Sqlite, "C:/dados/loja nova.db", "")
        );
        apply(&mut c, "sqlite:///home/eu/app.sqlite3").unwrap();
        assert_eq!(c.database, "/home/eu/app.sqlite3");
        apply(&mut c, "SQLITE:dev.db").unwrap();
        assert_eq!(c.database, "dev.db");
        assert!(apply(&mut c, "sqlite://").is_err());
    }
}
