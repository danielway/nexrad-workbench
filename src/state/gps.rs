//! Transient GPS-location state for the "My Location" map overlay.
//!
//! One-shot only: when the user enables the layer, the right-panel checkbox
//! handler kicks off a single `navigator.geolocation.get_current_position`
//! call. The browser callback pushes its result through an `UnboundedSender`;
//! the main update loop drains the corresponding `UnboundedReceiver` into
//! [`GpsState::coords`] (or [`GpsState::error`] on failure). Not persisted
//! across reloads — geolocation permission is per-session in many browsers,
//! so a stored "on" state would silently re-prompt or fail.

use futures_channel::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::core::LocationResult;

pub struct GpsState {
    /// Last successfully fetched coordinates, as (latitude, longitude).
    pub coords: Option<(f64, f64)>,
    /// Most recent error, surfaced next to the layer checkbox. Cleared
    /// on the next successful fetch or when the layer is toggled off.
    pub error: Option<String>,
    /// Sender for the geolocation result queue. `Clone` to hand to async
    /// callbacks; calling [`Self::start_geolocation`] does this.
    results_tx: UnboundedSender<LocationResult>,
    /// Receiver drained each frame by the main loop.
    results_rx: UnboundedReceiver<LocationResult>,
}

impl GpsState {
    /// A clone-able sink that browser callbacks can push results into.
    pub fn result_sender(&self) -> UnboundedSender<LocationResult> {
        self.results_tx.clone()
    }

    /// Drain all results that have arrived since the last call.
    pub fn drain_results(&mut self) -> Vec<LocationResult> {
        let mut out = Vec::new();
        while let Ok(r) = self.results_rx.try_recv() {
            out.push(r);
        }
        out
    }
}

impl Default for GpsState {
    fn default() -> Self {
        let (results_tx, results_rx) = futures_channel::mpsc::unbounded();
        Self {
            coords: None,
            error: None,
            results_tx,
            results_rx,
        }
    }
}
