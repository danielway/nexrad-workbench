//! Diagnostics-feed state containers: NWS alerts, mPING storm reports, and
//! transient GPS location.

use futures_channel::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::alerts::Alert;
use crate::core::LocationResult;
use crate::mping::StormReport;

/// NWS alerts state.
///
/// Mirrors the alert list owned by `AlertsManager` plus transient UI
/// selections (which alert is open in the detail modal, is the list modal
/// open, etc.).
#[derive(Default)]
pub struct AlertsState {
    /// All currently-active alerts returned by the most recent successful fetch.
    pub alerts: Vec<Alert>,
    /// Wall-clock ms (JS Date.now) when the last fetch was started.
    pub last_poll_ms: f64,
    /// Wall-clock ms when the last fetch succeeded (including 304).
    pub last_success_ms: f64,
    /// ETag from the last successful response; sent as If-None-Match.
    pub last_etag: Option<String>,
    /// True while a fetch is in flight (for tooltip/status display).
    pub fetch_in_flight: bool,
    /// Last error message (cleared on success).
    pub last_error: Option<String>,
    /// When set, a manual refresh is requested on the next manager tick.
    pub refresh_requested: bool,
    /// Alert id currently shown in the detail modal.
    pub selected_alert_id: Option<String>,
    /// Whether the list modal is open.
    pub list_modal_open: bool,
}

impl AlertsState {
    /// Return alerts whose bbox intersects `bounds`, sorted by severity (high first).
    pub fn visible_in(&self, bounds: (f64, f64, f64, f64)) -> Vec<&Alert> {
        let mut out: Vec<&Alert> = self
            .alerts
            .iter()
            .filter(|a| crate::alerts::bbox_intersects(a, bounds))
            .collect();
        out.sort_by_key(|a| std::cmp::Reverse(a.severity.rank()));
        out
    }

    /// Look up an alert by id.
    pub fn find(&self, id: &str) -> Option<&Alert> {
        self.alerts.iter().find(|a| a.id == id)
    }
}

/// mPING storm reports state.
///
/// Mirrors the report list updated by `MpingManager` plus settings-modal UI
/// flags and the user's API key.
#[derive(Default)]
pub struct MpingState {
    /// Currently-loaded reports for the active fetch window.
    pub reports: Vec<StormReport>,
    /// Total reports the server reported for this query (may exceed
    /// `reports.len()` if the response was truncated to one page).
    pub total_count: usize,
    /// Lower bound (ms since epoch) of the time window the loaded reports
    /// were fetched for.
    pub window_min_ms: f64,
    /// Upper bound (ms since epoch) of the time window.
    pub window_max_ms: f64,
    /// Wall-clock ms when the last fetch was started.
    pub last_poll_ms: f64,
    /// Wall-clock ms when the last fetch succeeded.
    pub last_success_ms: f64,
    /// True while a fetch is in flight.
    pub fetch_in_flight: bool,
    /// Last error message (cleared on success).
    pub last_error: Option<String>,
    /// User's mPING API key. `None` means the integration is unconfigured
    /// and the layer toggle is disabled.
    pub api_key: Option<String>,
    /// Whether the mPING settings modal is currently open.
    pub settings_modal_open: bool,
    /// Latched when the user saves a new key — the main loop calls
    /// `MpingManager::invalidate()` and clears this flag, forcing a refetch.
    pub invalidate_requested: bool,
    /// `id` of the currently-selected report (clicked marker). `None` means
    /// no detail popover is visible. Cleared when the user clicks empty
    /// canvas, presses Escape, or the reports list refreshes.
    pub selected_report_id: Option<i64>,
}

/// Transient GPS-location state for the "My Location" map overlay.
///
/// One-shot only: when the user enables the layer, the right-panel checkbox
/// handler kicks off a single `navigator.geolocation.get_current_position`
/// call. The browser callback pushes its result through an `UnboundedSender`;
/// the main update loop drains the corresponding `UnboundedReceiver` into
/// [`GpsState::coords`] (or [`GpsState::error`] on failure). Not persisted
/// across reloads — geolocation permission is per-session in many browsers,
/// so a stored "on" state would silently re-prompt or fail.
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
