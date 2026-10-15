//! Other programs tabella starts (`ssh`, the file manager), found by full path.
//!
//! On Windows a bare name is looked up in the app's own folder first, so a `ssh.exe` dropped
//! next to `tabella.exe` would run instead of the real one. System programs come from the Windows
//! folder; anything else from absolute `PATH` entries other than the app's folder.

use std::path::PathBuf;
use std::process::Command;

/// A command for `name` (without `.exe`), or `None` when it isn't installed.
pub fn command(name: &str) -> Option<Command> {
    find(name).map(Command::new)
}

#[cfg(windows)]
fn find(name: &str) -> Option<PathBuf> {
    let windows = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    let exe = format!("{name}.exe");
    let system = [
        windows.join(&exe),
        windows.join("System32").join(&exe),
        // The OpenSSH client that ships with Windows.
        windows.join("System32").join("OpenSSH").join(&exe),
    ];
    if let Some(p) = system.into_iter().find(|p| p.is_file()) {
        return Some(p);
    }
    let own = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.to_path_buf()));
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .filter(|d| d.is_absolute() && Some(d) != own.as_ref())
        .map(|d| d.join(&exe))
        .find(|p| p.is_file())
}

/// Elsewhere programs are not looked up in the app's folder: `PATH` as usual.
#[cfg(not(windows))]
fn find(name: &str) -> Option<PathBuf> {
    Some(PathBuf::from(name))
}
