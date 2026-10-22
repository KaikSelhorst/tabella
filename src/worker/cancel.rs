//! Stopping what a session runs: the statement of one tab, and its jobs still in line.

use super::*;

/// What one of a session's connections is running, so that cancelling a tab stops that tab's
/// statement and not whichever one happens to be running.
#[derive(Default)]
pub struct Running {
    tab: Mutex<Option<u64>>,
    canceller: Mutex<Option<Arc<Canceller>>>,
    /// When a cancel was last sent (see `start`).
    cancelled_at: Mutex<Option<Instant>>,
}

/// How long after a cancel the connection's next statement waits.
pub(super) const CANCEL_GRACE: Duration = Duration::from_millis(100);

impl Running {
    /// A job for `tab` starts. Right after a cancel it waits a moment: Postgres hands the cancel
    /// to the session asynchronously, and a statement started meanwhile would be the one stopped.
    pub(super) fn start(&self, tab: Option<u64>) {
        let Ok(mut t) = self.tab.lock() else { return };
        let since = self
            .cancelled_at
            .lock()
            .ok()
            .and_then(|at| *at)
            .map(|at| at.elapsed());
        if let Some(since) = since
            && since < CANCEL_GRACE
        {
            thread::sleep(CANCEL_GRACE - since);
        }
        *t = tab;
    }

    pub(super) fn set_tab(&self, tab: Option<u64>) {
        if let Ok(mut t) = self.tab.lock() {
            *t = tab;
        }
    }

    pub(super) fn set_canceller(&self, c: Canceller) {
        if let Ok(mut slot) = self.canceller.lock() {
            *slot = Some(Arc::new(c));
        }
    }

    /// Stops the statement running for `tab`, or whatever runs with `None`. Blocks (it may
    /// open a connection to ask the server): call it off the UI thread.
    pub fn cancel(&self, tab: Option<u64>) {
        // Held while asking: the job can't end and another tab's start in between, so the
        // statement stopped is the one checked here. (The worker waits for it at most that
        // long before its next job.)
        let Ok(running) = self.tab.lock() else { return };
        if running.is_none() || (tab.is_some() && *running != tab) {
            return;
        }
        let canceller = self.canceller.lock().ok().and_then(|c| c.clone());
        if let Some(c) = canceller {
            let _ = c.cancel();
            if let Ok(mut at) = self.cancelled_at.lock() {
                *at = Some(Instant::now());
            }
        }
    }
}

/// When each tab was last cancelled: its jobs taken before that, still waiting in line, are
/// answered as cancelled instead of run.
#[derive(Default)]
pub struct Cancels(Mutex<HashMap<u64, Instant>>);

impl Cancels {
    pub fn mark(&self, tab: u64) {
        if let Ok(mut m) = self.0.lock() {
            let now = Instant::now();
            // A job doesn't wait in line that long: older marks can't skip anything any more.
            m.retain(|_, at| now.duration_since(*at) < Duration::from_secs(3600));
            m.insert(tab, now);
        }
    }

    pub(super) fn skips(&self, tab: Option<u64>, taken: Instant) -> bool {
        let Some(tab) = tab else { return false };
        self.0
            .lock()
            .ok()
            .and_then(|m| m.get(&tab).copied())
            .is_some_and(|at| at >= taken)
    }
}
