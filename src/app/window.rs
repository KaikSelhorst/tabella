//! What the app needs from the window it runs in, so that this module never touches the
//! toolkit: the `ui` module implements it for egui, and tests can stand in for it.

use std::sync::Arc;

pub trait Window: Send + Sync {
    /// Draws again soon: something changed off the UI thread (a reply arrived).
    fn wake(&self);
    fn close(&self);
    fn set_title(&self, title: &str);
    /// Flashes the taskbar button.
    fn request_attention(&self);
    /// Whether the window has the focus; `None` when the system doesn't say.
    fn focused(&self) -> Option<bool>;
}

/// Called from other threads when there is something new to show.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// A window that isn't there, for tests of the app alone.
#[cfg(test)]
pub struct NoWindow;

#[cfg(test)]
impl Window for NoWindow {
    fn wake(&self) {}
    fn close(&self) {}
    fn set_title(&self, _: &str) {}
    fn request_attention(&self) {}
    fn focused(&self) -> Option<bool> {
        None
    }
}
