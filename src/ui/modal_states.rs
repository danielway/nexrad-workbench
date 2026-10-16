//! Aggregator for the modal UI states that live alongside `AppState`.
//!
//! **Placement rule.** Transient UI state has exactly two homes, split on
//! one question: *is it "what is on screen" or "what has the user typed"?*
//!
//! - **What is on screen** — which panel/modal is open, which tab is active,
//!   what a modal was opened *for* — lives on [`crate::subsystem::Chrome`].
//!   Both the UI (a toggle) and the shell (an effect opening the site modal)
//!   write it, and the layout tree reads it to decide what to render.
//! - **What the user has typed** — search filters, form fields, text buffers
//!   two-way bound to egui widgets — lives here. Nothing outside the widget
//!   that owns it ever reads it.
//!
//! Anything that is neither is domain state and belongs on `AppState` or a
//! subsystem.
//!
//! [`SiteModalState`], [`EventModalState`] and [`MpingModalState`] each hold
//! transient form state that should not survive a reload. They live
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
pub(crate) struct ModalStates {
    /// Site picker: the search field, the Near you lookup and its async
    /// location queue.
    pub site: SiteModalState,
    /// Event create/edit modal: form fields for name, site, time range.
    pub event: EventModalState,
    /// mPING settings modal: in-flight API-key text entry.
    pub mping: MpingModalState,
}

impl ModalStates {
    /// Construct a fresh set of modal states.
    pub(crate) fn new() -> Self {
        Self {
            site: SiteModalState::default(),
            event: EventModalState::default(),
            mping: MpingModalState::default(),
        }
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn new_site_picker_starts_empty_and_idle() {
        let states = ModalStates::new();
        assert_eq!(states.site.query, "");
        assert!(states.site.near == crate::core::site_picker::NearYou::Idle);
    }

    #[wasm_bindgen_test]
    fn new_event_modal_starts_with_empty_name() {
        let states = ModalStates::new();
        assert_eq!(states.event.name, "");
        assert_eq!(states.event.site_id, "");
    }

    #[wasm_bindgen_test]
    fn new_mping_modal_starts_with_empty_key_input() {
        let states = ModalStates::new();
        assert_eq!(states.mping.key_input, "");
    }
}
