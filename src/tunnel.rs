//! SSH tunnels through the system `ssh` client (`ssh -N -L`). Using the installed client means the
//! user's keys, agent and `~/.ssh/config` just work, without bundling an SSH implementation.
//! Authentication must not need typing: keys or agent only (`BatchMode=yes`).

use std::io::Read;
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use crate::db::SshConfig;

#[cfg(test)]
thread_local! {
    static KNOWN_HOSTS: std::cell::RefCell<Option<std::path::PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// Tests: tunnels opened by this thread use `file` as known_hosts (the environment is shared
/// by tests running in parallel).
#[cfg(test)]
fn use_known_hosts_in_this_thread(file: std::path::PathBuf) {
    KNOWN_HOSTS.with(|k| *k.borrow_mut() = Some(file));
}

/// A known_hosts file other than the user's, when one is asked for.
fn known_hosts_override() -> Option<std::ffi::OsString> {
    #[cfg(test)]
    if let Some(f) = KNOWN_HOSTS.with(|k| k.borrow().clone()) {
        return Some(f.into_os_string());
    }
    std::env::var_os("TABELLA_SSH_KNOWN_HOSTS")
}

/// How long to wait for the tunnel to accept connections.
const OPEN_TIMEOUT: Duration = Duration::from_secs(15);

/// A running `ssh -L`. The process is stopped when this is dropped.
pub struct Tunnel {
    child: Child,
    pub local_port: u16,
}

impl Tunnel {
    /// Forwards a local port to `db_host:db_port` as seen from the SSH server.
    pub fn open(ssh: &SshConfig, db_host: &str, db_port: u16) -> Result<Tunnel, String> {
        check_arg("SSH user", &ssh.user, false)?;
        check_arg("SSH host", &ssh.host, true)?;
        check_arg("database host", db_host, true)?;
        let local_port = free_port().map_err(|e| format!("no free local port: {e}"))?;
        let mut cmd =
            crate::programs::command("ssh").ok_or("ssh client not found; install Windows' OpenSSH Client")?;
        cmd.args([
            "-N",
            // Fail instead of prompting (there is no terminal) or silently not forwarding.
            "-o",
            "BatchMode=yes",
            "-o",
            "ExitOnForwardFailure=yes",
            // Trust a server on first use, but refuse if its key later changes.
            "-o",
            "StrictHostKeyChecking=accept-new",
            "-o",
            "ServerAliveInterval=30",
            "-p",
        ])
        .arg(ssh.port.to_string())
        .arg("-L")
        .arg(format!("127.0.0.1:{local_port}:{db_host}:{db_port}"));
        // Tests point this at a throwaway file instead of the user's ~/.ssh/known_hosts.
        if let Some(known) = known_hosts_override() {
            cmd.arg("-o")
                .arg(format!("UserKnownHostsFile={}", known.to_string_lossy()));
        }
        if !ssh.key_path.trim().is_empty() {
            cmd.arg("-i").arg(ssh.key_path.trim());
        }
        cmd.arg(format!("{}@{}", ssh.user, ssh.host))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let child = cmd
            .spawn()
            .map_err(|e| format!("couldn't start ssh ({e}); is the OpenSSH client installed?"))?;
        let mut tunnel = Tunnel { child, local_port };
        tunnel.wait_ready()?;
        Ok(tunnel)
    }

    fn wait_ready(&mut self) -> Result<(), String> {
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, self.local_port));
        let deadline = Instant::now() + OPEN_TIMEOUT;
        loop {
            if let Ok(Some(_)) = self.child.try_wait() {
                let mut err = String::new();
                if let Some(mut stderr) = self.child.stderr.take() {
                    let _ = stderr.read_to_string(&mut err);
                }
                let err = err.trim();
                return Err(if err.is_empty() {
                    "ssh exited without opening the tunnel".into()
                } else {
                    format!("SSH: {err}")
                });
            }
            if TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok() {
                return Ok(());
            }
            if Instant::now() > deadline {
                return Err("SSH: timed out opening the tunnel".into());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// These go on ssh's command line: a value starting with `-` would be read as an option (e.g.
/// `-oProxyCommand=…` runs a program), and spaces or control characters have no place in them.
/// Users may contain `@` (`joao@corp`: ssh splits at the last one); host names may not.
fn check_arg(what: &str, v: &str, host: bool) -> Result<(), String> {
    if v.trim().is_empty() {
        return Err(format!("SSH: enter the {what}"));
    }
    if v.starts_with('-') || v.chars().any(|c| c.is_whitespace() || c.is_control()) || host && v.contains('@')
    {
        return Err(format!("SSH: invalid {what}: {v:?}"));
    }
    Ok(())
}

/// A port nobody listens on right now (the OS picks it; it is released for ssh to bind).
fn free_port() -> std::io::Result<u16> {
    Ok(TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?.local_addr()?.port())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn option_like_fields_are_refused() {
        let mut cfg = SshConfig {
            host: "bastion.example.com".into(),
            port: 22,
            user: "-oProxyCommand=calc".into(),
            key_path: String::new(),
        };
        assert!(
            Tunnel::open(&cfg, "db", 5432)
                .err()
                .unwrap()
                .contains("invalid SSH user")
        );
        cfg.user = "deploy".into();
        cfg.host = "-F/tmp/x".into();
        assert!(
            Tunnel::open(&cfg, "db", 5432)
                .err()
                .unwrap()
                .contains("invalid SSH host")
        );
        cfg.host = "bastion.example.com".into();
        assert!(
            Tunnel::open(&cfg, "internal db", 5432)
                .err()
                .unwrap()
                .contains("database host")
        );
    }

    #[test]
    fn reports_ssh_failures_instead_of_hanging() {
        // Nothing listens on port 1: ssh exits quickly and the error comes back as text.
        let cfg = SshConfig {
            host: "127.0.0.1".into(),
            port: 1,
            user: "x".into(),
            key_path: String::new(),
        };
        let started = Instant::now();
        let err = Tunnel::open(&cfg, "localhost", 5432).err().expect("must fail");
        assert!(err.starts_with("SSH") || err.contains("ssh"), "{err}");
        assert!(started.elapsed() < OPEN_TIMEOUT);
    }

    /// Needs `docker compose --profile test up -d --wait` (test-ssh forwards to test-pg).
    #[test]
    #[ignore]
    fn connects_through_a_real_ssh_server() {
        use crate::db::{Conn, ConnConfig, Kind};
        let dir = std::env::temp_dir().join(format!("tabella-ssh-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // ssh refuses private keys readable by others; git doesn't keep file modes.
        let key = dir.join("key");
        std::fs::copy("dev/ssh/test_key", &key).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        use_known_hosts_in_this_thread(dir.join("known_hosts"));

        let ssh = SshConfig {
            host: "127.0.0.1".into(),
            port: 52222,
            user: "tunnel".into(),
            key_path: key.display().to_string(),
        };
        let mut cfg = ConnConfig::new(Kind::Postgres);
        cfg.host = "test-pg".into(); // as seen from the SSH server
        cfg.port = 5432;
        cfg.password = "pw".into();
        cfg.ssh = Some(ssh.clone());
        let tunnel = Tunnel::open(&ssh, &cfg.host, cfg.port).expect("tunnel");
        let mut c = Conn::connect(&cfg, Some(tunnel.local_port)).expect("connect through tunnel");
        let r = c.query("SELECT inet_server_addr() IS NOT NULL").unwrap();
        assert_eq!(r.rows[0][0].as_deref(), Some("t"));
        drop(c);
        let port = tunnel.local_port;
        drop(tunnel);
        // Dropping the tunnel stops ssh: the local port no longer accepts connections.
        std::thread::sleep(Duration::from_millis(300));
        assert!(
            TcpStream::connect_timeout(
                &SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
                Duration::from_millis(300)
            )
            .is_err()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
