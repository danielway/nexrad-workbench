//! NWS alerts state.
//!
//! Mirrors the alert list owned by `AlertsManager` plus transient UI
//! selections (which alert is open in the detail modal, is the list modal
//! open, etc.).

use crate::alerts::Alert;

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
        out.sort_by(|a, b| b.severity.rank().cmp(&a.severity.rank()));
        out
    }

    /// Look up an alert by id.
    pub fn find(&self, id: &str) -> Option<&Alert> {
        self.alerts.iter().find(|a| a.id == id)
    }
}
