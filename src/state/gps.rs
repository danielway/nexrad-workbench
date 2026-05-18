//! Transient GPS-location state for the "My Location" map overlay.
//!
//! One-shot only: when the user enables the layer, the right-panel checkbox
//! handler kicks off a single `navigator.geolocation.get_current_position`
//! call and pushes the result into [`GpsState::results`]. The main update
//! loop drains that queue into [`GpsState::coords`] (or [`GpsState::error`]
//! on failure). Not persisted across reloads — geolocation permission is
//! per-session in many browsers, so a stored "on" state would silently
//! re-prompt or fail.

use std::cell::RefCell;
use std::rc::Rc;

use crate::ui::LocationResult;

#[derive(Default)]
pub struct GpsState {
    /// Last successfully fetched coordinates, as (latitude, longitude).
    pub coords: Option<(f64, f64)>,
    /// Async result queue for the in-flight one-shot fetch. Shared with
    /// the JS callbacks via `Rc<RefCell<…>>`.
    pub results: Rc<RefCell<Vec<LocationResult>>>,
    /// Most recent error, surfaced next to the layer checkbox. Cleared
    /// on the next successful fetch or when the layer is toggled off.
    pub error: Option<String>,
}
