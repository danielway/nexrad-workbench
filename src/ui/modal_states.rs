//! Aggregator for the modal UI states that live alongside `AppState`.
//!
//! [`SiteModalState`], [`EventModalState`], and [`MpingModalState`] each
//! hold transient form state that should not survive a reload. They live
//! outside `AppState` so they don't need `Default + Clone` (one of them
//! owns an `Rc<RefCell<Vec<LocationResult>>>` shared with async
//! callbacks), and so they're not serialized into the URL/preferences
//! roundtrip.
//!
//! Before this aggregator existed, the three states were threaded through
//! `WorkbenchApp` as separate fields and passed individually to render
//! functions. Collecting them into one struct keeps the placement rule
//! consistent and gives a single hook point for cross-cutting concerns
//! (e.g. resetting all modal forms on site change).
//!
//! Render functions still take `&mut SiteModalState` / `&mut
//! EventModalState` / `&mut MpingModalState` directly so each one
//! receives only what it touches — the aggregator exists for ownership
//! and threading, not for hiding the per-modal API.

use super::{EventModalState, MpingModalState, SiteModalState};

/// Owns all transient modal UI state held outside [`crate::state::AppState`].
pub struct ModalStates {
    /// Site-selection modal: search filter, async location queue, and
    /// the welcome/list/zip view mode.
    pub site: SiteModalState,
    /// Event create/edit modal: form fields for name, site, time range.
    pub event: EventModalState,
    /// mPING settings modal: in-flight API-key text entry.
    pub mping: MpingModalState,
}

impl ModalStates {
    /// Construct a fresh set of modal states.
    ///
    /// `has_preferred_site` controls the site modal's first-visit
    /// welcome verbiage — `true` for returning users (shorter "change
    /// site" heading), `false` for the first visit.
    pub fn new(has_preferred_site: bool) -> Self {
        let mut site = SiteModalState::default();
        if has_preferred_site {
            site.is_first_visit = false;
        }
        Self {
            site,
            event: EventModalState::default(),
            mping: MpingModalState::default(),
        }
    }
}
