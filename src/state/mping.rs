//! mPING storm reports state.
//!
//! Owned by `AppState`. Mirrors the report list updated by `MpingManager`
//! plus settings-modal UI flags and the user's API key.

use crate::mping::StormReport;

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
