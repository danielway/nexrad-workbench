//! Channel plumbing for async mPING fetches.
//!
//! Mirrors `crate::alerts::channel::AlertsChannel` — a shared `Vec<Event>`
//! that the spawned future writes into and the UI loop drains each frame.

use std::cell::RefCell;
use std::rc::Rc;

use super::types::StormReport;

/// An event delivered by the fetch future into the UI loop.
pub enum MpingEvent {
    /// Fetch succeeded with a parsed report set.
    Updated {
        reports: Vec<StormReport>,
        total_count: usize,
    },
    /// Fetch failed with a human-readable reason.
    Error(String),
}

/// Shared buffer for events produced by the async fetch.
#[derive(Clone, Default)]
pub struct MpingChannel {
    events: Rc<RefCell<Vec<MpingEvent>>>,
}

impl MpingChannel {
    pub fn new() -> Self {
        Self::default()
    }

    /// Push an event from inside an async task.
    pub fn push(&self, event: MpingEvent) {
        self.events.borrow_mut().push(event);
    }

    /// Drain all pending events; called once per frame.
    pub fn drain(&self) -> Vec<MpingEvent> {
        std::mem::take(&mut *self.events.borrow_mut())
    }
}
