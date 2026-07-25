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
use crate::core::{AlertsState, GpsState, MpingState};
use crate::mping::{MpingManager, MpingTickInputs};
use crate::nexrad::NetworkMonitor;
use crate::state::ErrorContext;
use eframe::egui;

/// Per-frame inputs the diagnostics subsystem needs to tick its managers.
///
/// Computed by the caller from `AppState` so this subsystem stays
/// decoupled from the god struct.
pub struct DiagnosticsInputs<'a> {
    /// Recency gate for the alerts overlay. (mPING no longer gates on
    /// this — it shows reports for historical playback too.)
    pub is_live: bool,
    /// Whether the mPING overlay layer is currently visible.
    pub mping_layer_visible: bool,
    /// Whether the playhead is tracking the live edge (pinned to now or
    /// replaying the lookback loop). Selects mPING's live-tailing regime.
    pub mping_pinned_to_now: bool,
    /// Active radar site id.
    pub site_id: &'a str,
    /// Current playback position in Unix seconds (drives the mPING fetch
    /// window and refetch decision).
    pub playback_secs: f64,
}

/// Owner of telemetry/observability state and the managers that drive it.
pub struct Diagnostics {
    /// NWS active-alerts overlay state.
    pub alerts: AlertsState,
    /// Lifecycle for the NWS alerts polling loop.
    pub alerts_manager: AlertsManager,
    /// mPING crowd-sourced storm-report overlay state.
    pub mping: MpingState,
    /// Lifecycle for the mPING polling loop.
    pub mping_manager: MpingManager,
    /// Transient one-shot GPS-location state for the "My Location" overlay.
    /// Not persisted across reloads (geolocation permission is per-session
    /// in many browsers).
    pub gps: GpsState,
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
            mping: MpingState::default(),
            mping_manager: MpingManager::new(),
            gps: GpsState::default(),
            network_monitor: None,
        }
    }

    /// Per-frame tick: poll managers, drain results into their state.
    ///
    /// `errors` is the app-wide error collector that managers push into
    /// when they encounter a failure, so the unified errors view sees
    /// every report alongside worker / download failures.
    pub fn tick(
        &mut self,
        ctx: &egui::Context,
        inputs: DiagnosticsInputs<'_>,
        errors: &mut ErrorContext,
    ) {
        self.alerts_manager
            .tick(ctx, &mut self.alerts, inputs.is_live, errors);

        // mPING: drain any invalidation request the modal posted, then
        // tick the manager.
        if std::mem::take(&mut self.mping.invalidate_requested) {
            self.mping_manager.invalidate();
        }
        self.mping_manager.tick(
            ctx,
            &mut self.mping,
            MpingTickInputs {
                layer_visible: inputs.mping_layer_visible,
                pinned_to_now: inputs.mping_pinned_to_now,
                site_id: inputs.site_id,
                playback_secs: inputs.playback_secs,
            },
            errors,
        );
    }
}

impl Default for Diagnostics {
    fn default() -> Self {
        Self::new()
    }
}
