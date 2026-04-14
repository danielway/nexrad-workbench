//! Channel plumbing for async NWS alert fetches.
//!
//! Follows the `Rc<RefCell<Vec<_>>>` pattern used elsewhere (e.g.
//! `SiteModalState::location_results`) so results produced inside a
//! `spawn_local` future can be drained synchronously each UI frame.

use std::cell::RefCell;
use std::rc::Rc;

use super::types::Alert;

/// An event delivered by the fetch future into the UI loop.
pub enum AlertsEvent {
    /// Fetch succeeded with the full parsed alert set and an ETag (if the
    /// server returned one).
    Updated {
        alerts: Vec<Alert>,
        etag: Option<String>,
    },
    /// Server returned 304 Not Modified — keep existing alerts.
    NotModified,
    /// Fetch failed with a human-readable reason.
    Error(String),
}

/// Shared buffer for events produced by the async fetch.
#[derive(Clone, Default)]
pub struct AlertsChannel {
    events: Rc<RefCell<Vec<AlertsEvent>>>,
}

impl AlertsChannel {
    pub fn new() -> Self {
        Self::default()
    }

    /// Push an event from inside an async task.
    pub fn push(&self, event: AlertsEvent) {
        self.events.borrow_mut().push(event);
    }

    /// Drain all pending events; called once per frame.
    pub fn drain(&self) -> Vec<AlertsEvent> {
        std::mem::take(&mut *self.events.borrow_mut())
    }
}
