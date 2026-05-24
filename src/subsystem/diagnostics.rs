//! Diagnostics subsystem: observability + peripheral telemetry overlays.
//!
//! The grouping is by "stuff that doesn't sit on the playback critical path
//! but reports on the world": NWS warnings, crowd-sourced mPING reports,
//! the user's GPS location, and service-worker network metrics. Each was
//! historically a separate manager hanging off `WorkbenchApp` paired with
//! a state slice on `AppState`; here they collapse into one owner.
//!
//! **Migration status**: alerts has landed (state + manager). mPING, GPS,
//! and network monitor are slated to fold in next as separate PRs — they
//! share the same pattern (manager + state) and benefit from being
//! reached through a single subsystem handle for cross-cutting work
//! (e.g. recency-driven gating).

use crate::alerts::AlertsManager;
use crate::nexrad::NetworkMonitor;
use crate::state::AlertsState;
use eframe::egui;

/// Owner of telemetry/observability state and the managers that drive it.
pub struct Diagnostics {
    /// NWS active-alerts overlay state (alert list, modal open/close,
    /// last-error chip).
    pub alerts: AlertsState,
    /// Lifecycle for the NWS alerts polling loop.
    pub alerts_manager: AlertsManager,
    /// Service worker network monitor. Lazily initialized the first time
    /// dev mode becomes active so the listener isn't attached when the
    /// user can't see the metrics.
    pub network_monitor: Option<NetworkMonitor>,
}

impl Diagnostics {
    pub fn new() -> Self {
        Self {
            alerts: AlertsState::default(),
            alerts_manager: AlertsManager::new(),
            network_monitor: None,
        }
    }

    /// Per-frame tick: poll managers, drain results into their state.
    ///
    /// `is_live` lets each manager skip polling when the user is viewing
    /// archive data far from wall-clock (e.g. scrubbed weeks into the
    /// past) — there's nothing fresh to report against. Computed by the
    /// caller from [`crate::state::recency::data_is_live`] so this
    /// subsystem doesn't have to reach back into `AppState`.
    pub fn tick(&mut self, ctx: &egui::Context, is_live: bool) {
        self.alerts_manager.tick(ctx, &mut self.alerts, is_live);
    }
}

impl Default for Diagnostics {
    fn default() -> Self {
        Self::new()
    }
}
