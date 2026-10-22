//! What a crash leaves behind. A Windows build has no console, so a panic's message would go
//! nowhere: it is appended to `crash.log` in the config folder. Work that can fail on its own
//! (a job, a key derivation, a backup) runs under `guard`, which turns a panic into an error for
//! whoever waits for it. Any other panic is announced: in the window right away when one of its
//! threads died (see `take_unguarded`), at the next start when the window itself did.

use std::cell::Cell;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

thread_local! {
    /// Inside `guard` on this thread: the panic is caught and reported by the caller.
    static GUARDED: Cell<bool> = const { Cell::new(false) };
}

/// A thread of the app died of a panic nothing caught.
static UNGUARDED: AtomicBool = AtomicBool::new(false);

const LOG: &str = "crash.log";
/// Left by a panic on the window's own thread (the one that ends the app), until the next start.
const PENDING: &str = "crash.pending";

pub fn log_path() -> Option<PathBuf> {
    crate::store::path(LOG).ok()
}

/// Logs every panic, on any thread, before it unwinds (or aborts).
pub fn install() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default(info);
        let thread = std::thread::current();
        let text = format!(
            "--- {} · tabella {} · thread {}\n{info}\n\n",
            crate::history::now(),
            env!("CARGO_PKG_VERSION"),
            thread.name().unwrap_or("unnamed"),
        );
        if let Some(path) = log_path() {
            // A first run may not have made the folder yet.
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .and_then(|mut f| f.write_all(text.as_bytes()));
        }
        if GUARDED.get() {
            return;
        }
        if thread.name() == Some("main") {
            if let Ok(p) = crate::store::path(PENDING) {
                let _ = std::fs::write(p, "");
            }
        } else {
            UNGUARDED.store(true, Ordering::SeqCst);
        }
    }));
}

/// The message to show when the last run ended in a crash (once).
pub fn take_previous() -> Option<String> {
    let pending = crate::store::path(PENDING).ok()?;
    std::fs::remove_file(pending).ok()?;
    let log = log_path().map(|p| p.display().to_string()).unwrap_or_default();
    Some(format!(
        "tabella closed unexpectedly last time. What happened was saved to {log}"
    ))
}

/// Runs `f`; a panic in it comes back as the message to show, and the thread goes on.
pub fn guard<T>(f: impl FnOnce() -> T) -> Result<T, String> {
    let outer = GUARDED.replace(true);
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    GUARDED.set(outer);
    res.map_err(|_| internal_error())
}

/// What to tell the user about a panic caught by `guard`.
pub fn internal_error() -> String {
    let log = log_path().map(|p| p.display().to_string()).unwrap_or_default();
    format!("internal error in tabella (details in {log})")
}

/// Whether one of the app's threads died of a panic since the last call: the window goes on, but
/// whatever that thread did (a connection, saving the history) has stopped.
pub fn take_unguarded() -> bool {
    UNGUARDED.swap(false, Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_guarded_panic_is_an_error_and_an_unguarded_one_is_announced() {
        let dir = std::env::temp_dir().join(format!("tabella-crash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        crate::store::use_dir_in_this_thread(dir.clone());
        install();
        let err = guard(|| -> u8 {
            // Inside: the hook leaves this panic to the caller instead of announcing it.
            assert!(GUARDED.get());
            panic!("boom")
        })
        .unwrap_err();
        assert!(err.contains("internal error"), "{err}");
        assert!(!GUARDED.get(), "unguarded again after");
        let log = std::fs::read_to_string(dir.join(LOG)).unwrap();
        assert!(log.contains("boom"), "{log}");
        let worker = std::thread::Builder::new().name("worker".into());
        let _ = worker.spawn(|| panic!("lost")).unwrap().join();
        assert!(take_unguarded());
        // The window's own thread dying is told at the next start, once.
        std::fs::write(dir.join(PENDING), "").unwrap();
        assert!(take_previous().unwrap().contains("closed unexpectedly"));
        assert!(take_previous().is_none());
        let _ = std::panic::take_hook();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
