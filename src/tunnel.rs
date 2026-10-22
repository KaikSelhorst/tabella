//! SSH tunnels through the system `ssh` client (`ssh -N -L`). Using the installed client means the
//! user's keys, agent and `~/.ssh/config` just work, without bundling an SSH implementation.
//! Authentication must not need typing: keys or agent only (`BatchMode=yes`).

use std::io::{BufRead, BufReader};
use std::net::{Ipv4Addr, TcpListener};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
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

#[derive(Debug, thiserror::Error)]
pub enum TunnelError {
    #[error("SSH: enter the {0}")]
    Missing(&'static str),
    #[error("SSH: invalid {what}: {value:?}")]
    Invalid { what: &'static str, value: String },
    #[error("no free local port: {0}")]
    NoFreePort(std::io::Error),
    #[error("ssh client not found; install Windows' OpenSSH Client")]
    NoClient,
    #[error("couldn't start ssh ({0}); is the OpenSSH client installed?")]
    Spawn(std::io::Error),
    /// What ssh wrote to stderr before exiting.
    #[error("SSH: {0}")]
    Exited(String),
    #[error("ssh exited without opening the tunnel")]
    ExitedSilently,
    #[error("SSH: timed out opening the tunnel")]
    Timeout,
    #[error("SSH: the tunnel was closed")]
    Closed,
}

/// A running `ssh -L`. The process is stopped when this is dropped.
pub struct Tunnel {
    child: Child,
    pub local_port: u16,
}

impl Tunnel {
    /// Forwards a local port to `db_host:db_port` as seen from the SSH server.
    pub fn open(ssh: &SshConfig, db_host: &str, db_port: u16) -> Result<Tunnel, TunnelError> {
        check_arg("SSH user", &ssh.user, false)?;
        check_arg("SSH host", &ssh.host, true)?;
        check_arg("database host", db_host, true)?;
        let local_port = free_port().map_err(TunnelError::NoFreePort)?;
        Self::open_on(ssh, db_host, db_port, local_port)
    }

    /// `open` on a given local port.
    fn open_on(ssh: &SshConfig, db_host: &str, db_port: u16, local_port: u16) -> Result<Tunnel, TunnelError> {
        let mut cmd = ssh_command().ok_or(TunnelError::NoClient)?;
        cmd.args([
            "-N",
            // Says when the forward is in place (see `wait_ready`).
            "-v",
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
        let child = cmd.spawn().map_err(TunnelError::Spawn)?;
        #[cfg(windows)]
        tie_to_app(&child);
        let mut tunnel = Tunnel { child, local_port };
        tunnel.wait_ready()?;
        Ok(tunnel)
    }

    /// Whether ssh is still running.
    fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Waits for ssh to say the forward is in place. Not by trying the port: until ssh has bound
    /// it, another program could, and would then be taken for the tunnel and sent the database's
    /// password. ssh only enters the session once every forward is listening, and exits instead
    /// when one can't (`ExitOnForwardFailure`).
    fn wait_ready(&mut self) -> Result<(), TunnelError> {
        let stderr = self.child.stderr.take().ok_or(TunnelError::ExitedSilently)?;
        let (tx, lines) = channel::<String>();
        // Reads ssh's stderr for as long as ssh runs: once nobody listens the lines are dropped,
        // but still read, or the pipe fills and ssh, blocked writing, stops forwarding.
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).split(b'\n') {
                let Ok(line) = line else { break };
                let _ = tx.send(String::from_utf8_lossy(&line).trim_end().to_string());
            }
        });
        let deadline = Instant::now() + OPEN_TIMEOUT;
        // What ssh said besides its debug output, for the error.
        let mut said: Vec<String> = Vec::new();
        let mut keep = |line: String| {
            if !line.starts_with("debug") && !line.trim().is_empty() {
                said.push(line);
            }
        };
        loop {
            match lines.recv_timeout(Duration::from_millis(100)) {
                Ok(line) if line.contains(READY) => return Ok(()),
                Ok(line) => keep(line),
                Err(RecvTimeoutError::Timeout) => {}
                // stderr closed: ssh is exiting.
                Err(RecvTimeoutError::Disconnected) => {
                    let _ = self.child.wait();
                }
            }
            if let Ok(Some(_)) = self.child.try_wait() {
                // Its last words may still be on the way.
                while let Ok(line) = lines.recv_timeout(Duration::from_millis(200)) {
                    keep(line);
                }
                let err = said.join("\n");
                return Err(if err.is_empty() {
                    TunnelError::ExitedSilently
                } else {
                    TunnelError::Exited(err)
                });
            }
            if Instant::now() > deadline {
                return Err(TunnelError::Timeout);
            }
        }
    }
}

/// `ssh`, made to end with the app even when the app is killed instead of closed (its `Drop` then
/// never runs). On Linux `setpriv` asks the kernel to kill ssh when the thread that started it
/// ends: the session's threads live as long as the tunnel is used, and a tunnel that dies with
/// one is opened again by the next connection (see `Link`). Without `setpriv`, plain ssh.
#[cfg(target_os = "linux")]
fn ssh_command() -> Option<std::process::Command> {
    if !crate::programs::installed("ssh") {
        return None;
    }
    if !crate::programs::installed("setpriv") {
        return crate::programs::command("ssh");
    }
    let mut cmd = crate::programs::command("setpriv")?;
    cmd.args(["--pdeathsig", "KILL", "--", "ssh"]);
    Some(cmd)
}

#[cfg(not(target_os = "linux"))]
fn ssh_command() -> Option<std::process::Command> {
    crate::programs::command("ssh")
}

/// Puts ssh in a job object that Windows closes with the app's process, however it ends, and
/// closing it ends ssh.
#[cfg(windows)]
fn tie_to_app(child: &Child) {
    use std::os::windows::io::AsRawHandle;
    static JOB: std::sync::OnceLock<Option<win32job::Job>> = std::sync::OnceLock::new();
    let job = JOB.get_or_init(|| {
        let mut info = win32job::ExtendedLimitInfo::new();
        info.limit_kill_on_job_close();
        win32job::Job::create_with_limit_info(&info).ok()
    });
    if let Some(job) = job {
        let _ = job.assign_process(child.as_raw_handle() as isize);
    }
}

/// What ssh prints (with `-v`) once authenticated and every forward listens.
const READY: &str = "Entering interactive session";

impl Drop for Tunnel {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A session's tunnel, shared by its connections. When ssh has exited (the network dropped), the
/// next connection opens it again on a new local port: the old one may by then belong to another
/// program, which must not be sent the database's password.
pub struct Link {
    ssh: SshConfig,
    host: String,
    port: u16,
    tunnel: Mutex<Option<Tunnel>>,
    closed: AtomicBool,
}

impl Link {
    /// Nothing runs until a connection asks for `port`.
    pub fn new(ssh: SshConfig, db_host: &str, db_port: u16) -> Arc<Link> {
        Arc::new(Link {
            ssh,
            host: db_host.to_string(),
            port: db_port,
            tunnel: Mutex::new(None),
            closed: AtomicBool::new(false),
        })
    }

    fn tunnel(&self) -> MutexGuard<'_, Option<Tunnel>> {
        self.tunnel.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The local port to connect to, starting ssh first when it isn't running.
    pub fn port(&self) -> Result<u16, TunnelError> {
        {
            let mut t = self.tunnel();
            if self.closed.load(Ordering::SeqCst) {
                return Err(TunnelError::Closed);
            }
            if let Some(t) = t.as_mut()
                && t.alive()
            {
                return Ok(t.local_port);
            }
            *t = None;
        }
        // Opening takes seconds: not while holding the lock, which `close` waits for.
        let new = Tunnel::open(&self.ssh, &self.host, self.port)?;
        let mut t = self.tunnel();
        if self.closed.load(Ordering::SeqCst) {
            return Err(TunnelError::Closed);
        }
        // The session's other connection may have opened one meanwhile: that one stays.
        if let Some(other) = t.as_mut()
            && other.alive()
        {
            return Ok(other.local_port);
        }
        let port = new.local_port;
        *t = Some(new);
        Ok(port)
    }

    /// Stops ssh for good. The session's threads may be busy (a long query) or about to end with
    /// the process: they can't be counted on to drop the tunnel themselves.
    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.tunnel().take();
    }
}

/// These go on ssh's command line: a value starting with `-` would be read as an option (e.g.
/// `-oProxyCommand=…` runs a program), and spaces or control characters have no place in them.
/// Users may contain `@` (`joao@corp`: ssh splits at the last one); host names may not.
fn check_arg(what: &'static str, v: &str, host: bool) -> Result<(), TunnelError> {
    if v.trim().is_empty() {
        return Err(TunnelError::Missing(what));
    }
    if v.starts_with('-') || v.chars().any(|c| c.is_whitespace() || c.is_control()) || host && v.contains('@')
    {
        return Err(TunnelError::Invalid {
            what,
            value: v.to_string(),
        });
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
    use std::net::{SocketAddr, TcpStream};

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
                .to_string()
                .contains("invalid SSH user")
        );
        cfg.user = "deploy".into();
        cfg.host = "-F/tmp/x".into();
        assert!(
            Tunnel::open(&cfg, "db", 5432)
                .err()
                .unwrap()
                .to_string()
                .contains("invalid SSH host")
        );
        cfg.host = "bastion.example.com".into();
        assert!(
            Tunnel::open(&cfg, "internal db", 5432)
                .err()
                .unwrap()
                .to_string()
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
        let err = Tunnel::open(&cfg, "localhost", 5432)
            .err()
            .expect("must fail")
            .to_string();
        assert!(err.starts_with("SSH") || err.contains("ssh"), "{err}");
        assert!(started.elapsed() < OPEN_TIMEOUT);
    }

    /// Needs `docker compose --profile test up -d --wait` (test-ssh forwards to test-pg).
    #[test]
    #[ignore = "needs the test databases: docker compose --profile test up -d --wait"]
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
        assert_eq!(r.rows.cell(0, 0), Some("t"));
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

        // A session's link: ssh that died (the network dropped) is started again, on a new
        // port, and closing it stops ssh for good.
        let link = Link::new(ssh.clone(), &cfg.host, cfg.port);
        link.port().expect("tunnel");
        let pid = |link: &Link| link.tunnel().as_ref().unwrap().child.id();
        let first = pid(&link);
        {
            // As when ssh exits on its own: gone before the next connection asks.
            let mut t = link.tunnel();
            let child = &mut t.as_mut().unwrap().child;
            child.kill().unwrap();
            child.wait().unwrap();
        }
        // The OS may hand out the same free port again: what matters is a new ssh behind it.
        let second = link.port().expect("reopened");
        assert_ne!(pid(&link), first);
        let mut c = Conn::connect(&cfg, Some(second)).expect("connect through the new tunnel");
        assert!(c.query("SELECT 1").is_ok());
        drop(c);
        link.close();
        assert!(matches!(link.port(), Err(TunnelError::Closed)));
        std::thread::sleep(Duration::from_millis(300));
        assert!(
            TcpStream::connect_timeout(
                &SocketAddr::from((Ipv4Addr::LOCALHOST, second)),
                Duration::from_millis(300)
            )
            .is_err()
        );
        // ssh ends with the thread that started it, as it would with the app killed outright.
        #[cfg(target_os = "linux")]
        {
            let (ssh2, host) = (ssh.clone(), cfg.host.clone());
            let pid = std::thread::spawn(move || {
                let t = Tunnel::open(&ssh2, &host, 5432).expect("tunnel");
                let pid = t.child.id();
                // Not dropped: only the thread's end may stop it.
                std::mem::forget(t);
                pid
            })
            .join()
            .unwrap();
            std::thread::sleep(Duration::from_millis(500));
            let state = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
            // Gone, or a zombie waiting for us to reap it: either way no longer forwarding.
            assert!(
                state.is_empty() || state.contains(") Z "),
                "ssh {pid} still runs: {state}"
            );
        }
        // Another program took the port between picking it and ssh binding it: that program
        // must not pass for the tunnel.
        let squatter = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let taken = squatter.local_addr().unwrap().port();
        let err = Tunnel::open_on(&ssh, &cfg.host, cfg.port, taken)
            .err()
            .expect("refused");
        assert!(err.to_string().contains("forwarding"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
