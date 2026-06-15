//! Pure diagnostics-overlay core: alert hit-testing, the alerts/mPING/GPS intent
//! reducer, and the diagnostics view-model.
//!
//! This is the P2 reference slice proving the full `intent → core → view-model →
//! shell` loop on a low-risk, high-QA-value feature off the render hot path:
//!
//! - [`select_alert_at`] / [`compute_alert_focus`] — the alert hit-test +
//!   severity-rank tie-break and the "show on map" target, lifted out of the
//!   canvas / modal UI as pure functions.
//! - [`DiagnosticsIntent`] + [`reduce`] — the only way the UI changes the
//!   alerts/mPING/GPS overlay state. Pure: it mutates the passed-in state and
//!   returns [`Effect`]s (geolocation) for the shell to perform.
//! - [`DiagnosticsVm`] — the read-only projection the alerts chip + list modal
//!   render, so they don't recompute `visible_in` themselves.

use crate::alerts::{Alert, AlertSeverity};
use crate::core::Effect;
use crate::state::{AlertsState, GpsState, MpingState};

// ---------------------------------------------------------------------------
// Pure hit-test / focus decisions
// ---------------------------------------------------------------------------

/// Pick the alert to select for a click at geographic `(lon, lat)`.
///
/// Mirrors the canvas hit-test exactly: an alert qualifies only if its class is
/// visible (`is_warning ? show_warnings : show_other`), its bbox intersects
/// `bounds`, and it contains the point. Among qualifying alerts the highest
/// [`AlertSeverity::rank`] wins; ties break to the **first** alert encountered in
/// list order (strict `>`), matching the original behavior. Returns the alert id
/// or `None` if the click missed every visible alert.
pub fn select_alert_at(
    alerts: &[Alert],
    lon: f64,
    lat: f64,
    bounds: (f64, f64, f64, f64),
    show_warnings: bool,
    show_other: bool,
) -> Option<String> {
    let mut best: Option<(u8, &str)> = None;
    for alert in alerts {
        let class_visible = if alert.is_warning() {
            show_warnings
        } else {
            show_other
        };
        if !class_visible {
            continue;
        }
        if !crate::alerts::bbox_intersects(alert, bounds) {
            continue;
        }
        if crate::alerts::contains_point(alert, lon, lat) {
            let rank = alert.severity.rank();
            if best.as_ref().is_none_or(|(r, _)| rank > *r) {
                best = Some((rank, alert.id.as_str()));
            }
        }
    }
    best.map(|(_, id)| id.to_string())
}

/// What "Show on map" should do for an alert: which overlay class to enable and
/// where (if anywhere) to center the 2D view. Pure; the shell applies it to the
/// camera/layer state.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AlertFocus {
    /// Whether this alert is a warning (enable warnings layer) vs other
    /// (enable watches/advisories layer).
    pub is_warning: bool,
    /// 2D center `(lat, lon)` from the alert's bbox centroid, or `None` when the
    /// alert has no bbox (zone-only) so the view should stay put.
    pub center: Option<(f64, f64)>,
}

/// Compute the focus target for "Show on map" (the alert detail modal action).
pub fn compute_alert_focus(alert: &Alert) -> AlertFocus {
    let center = alert
        .geometry
        .bbox
        .map(|(min_lon, min_lat, max_lon, max_lat)| {
            ((min_lat + max_lat) * 0.5, (min_lon + max_lon) * 0.5)
        });
    AlertFocus {
        is_warning: alert.is_warning(),
        center,
    }
}

/// Whether the mPING layer toggle should be interactable: only when live data is
/// showing *and* an API key is configured. (Was an inline `live && has_key` in
/// the right panel.)
pub fn mping_layer_available(is_live: bool, has_api_key: bool) -> bool {
    is_live && has_api_key
}

// ---------------------------------------------------------------------------
// Intents + reducer
// ---------------------------------------------------------------------------

/// An intent to change the diagnostics overlays' state (alerts / mPING / GPS).
/// The only channel by which the UI mutates these overlays.
#[derive(Debug, Clone, PartialEq)]
pub enum DiagnosticsIntent {
    // ── Alerts ──
    /// Open the detail modal for this alert id.
    SelectAlert(String),
    /// Close the alert detail modal.
    ClearAlertSelection,
    /// Open the "alerts in view" list modal.
    OpenAlertList,
    /// Close the list modal.
    CloseAlertList,
    /// Request an immediate refresh of the NWS alerts feed (manager picks it up).
    RefreshAlerts,
    // ── mPING ──
    /// Select a storm report (clicked marker) for its detail popover.
    SelectMpingReport(i64),
    /// Dismiss the report detail popover.
    ClearMpingSelection,
    /// Open the mPING settings (API key) modal.
    OpenMpingSettings,
    /// Close the mPING settings modal.
    CloseMpingSettings,
    /// Save (or clear, with `None`) the mPING API key. No-op if unchanged.
    SaveMpingApiKey(Option<String>),
    /// Clear the configured key and all loaded reports.
    ClearMpingKey,
    // ── GPS ──
    /// User enabled the "My Location" overlay → start a one-shot geolocation.
    EnableGps,
    /// User disabled the overlay → drop any coords/error.
    DisableGps,
    /// A geolocation lookup resolved to coordinates.
    GpsResolved(f64, f64),
    /// A geolocation lookup failed; auto-disables the overlay layer.
    GpsFailed(String),
}

/// Mutable borrows of the diagnostics state the reducer touches. Bundled so
/// [`reduce`] keeps the `(state, intent) -> (state, effects)` shape with one
/// argument. `gps_layer_active` is the `layer_state.geo.gps_location` toggle,
/// which the GPS-failure path auto-clears.
pub struct DiagnosticsStateMut<'a> {
    pub alerts: &'a mut AlertsState,
    pub mping: &'a mut MpingState,
    pub gps: &'a mut GpsState,
    pub gps_layer_active: &'a mut bool,
}

/// Apply a [`DiagnosticsIntent`] to the diagnostics state, returning any effects
/// for the shell to perform. Pure: in-memory mutation only; the only effect is
/// [`Effect::StartGeolocation`] (browser I/O), executed by the shell.
pub fn reduce(state: DiagnosticsStateMut<'_>, intent: DiagnosticsIntent) -> Vec<Effect> {
    use DiagnosticsIntent as I;
    match intent {
        // ── Alerts ──
        I::SelectAlert(id) => state.alerts.selected_alert_id = Some(id),
        I::ClearAlertSelection => state.alerts.selected_alert_id = None,
        I::OpenAlertList => state.alerts.list_modal_open = true,
        I::CloseAlertList => state.alerts.list_modal_open = false,
        I::RefreshAlerts => state.alerts.refresh_requested = true,

        // ── mPING ──
        I::SelectMpingReport(id) => state.mping.selected_report_id = Some(id),
        I::ClearMpingSelection => state.mping.selected_report_id = None,
        I::OpenMpingSettings => state.mping.settings_modal_open = true,
        I::CloseMpingSettings => state.mping.settings_modal_open = false,
        I::SaveMpingApiKey(new_key) => {
            // Only invalidate/refetch when the key actually changes.
            if state.mping.api_key.as_deref() != new_key.as_deref() {
                state.mping.api_key = new_key;
                state.mping.last_error = None;
                state.mping.invalidate_requested = true;
            }
            state.mping.settings_modal_open = false;
        }
        I::ClearMpingKey => {
            state.mping.api_key = None;
            state.mping.reports.clear();
            state.mping.total_count = 0;
            state.mping.last_error = None;
            state.mping.last_success_ms = 0.0;
            state.mping.invalidate_requested = true;
        }

        // ── GPS ──
        I::EnableGps => {
            state.gps.coords = None;
            state.gps.error = None;
            return vec![Effect::StartGeolocation];
        }
        I::DisableGps => {
            state.gps.coords = None;
            state.gps.error = None;
        }
        I::GpsResolved(lat, lon) => {
            state.gps.coords = Some((lat, lon));
            state.gps.error = None;
        }
        I::GpsFailed(msg) => {
            state.gps.error = Some(msg);
            state.gps.coords = None;
            // A failed lookup turns the overlay layer back off (one-shot).
            *state.gps_layer_active = false;
        }
    }
    Vec::new()
}

// ---------------------------------------------------------------------------
// View-model
// ---------------------------------------------------------------------------

/// One alert as projected for the chip / list modal: just the fields they render,
/// pre-sorted by severity (high first).
#[derive(Debug, Clone, PartialEq)]
pub struct VisibleAlert {
    pub id: String,
    pub event: String,
    pub area_desc: String,
    pub severity: AlertSeverity,
    pub is_warning: bool,
    pub expires_secs: Option<f64>,
}

/// Read-only projection of the diagnostics overlays for the UI to render — the
/// "view-model out" half of the contract. Currently carries the visible-alerts
/// list (the one genuine UI-side computation, `AlertsState::visible_in`), so the
/// top-bar chip and the list modal read it instead of each recomputing.
#[derive(Debug, Clone, Default)]
pub struct DiagnosticsVm {
    /// Alerts whose bbox intersects the current view, severity-sorted. Empty in
    /// 3D / before the first canvas frame (no bounds), or when none match.
    pub visible_alerts: Vec<VisibleAlert>,
}

impl DiagnosticsVm {
    /// Build the view-model from the diagnostics state and this frame's visible
    /// bounds (`None` in 3D globe mode / before the canvas has rendered).
    pub fn build(
        diagnostics: &crate::subsystem::Diagnostics,
        bounds: Option<(f64, f64, f64, f64)>,
    ) -> Self {
        let visible_alerts = match bounds {
            Some(b) => diagnostics
                .alerts
                .visible_in(b)
                .into_iter()
                .map(|a| VisibleAlert {
                    id: a.id.clone(),
                    event: a.event.clone(),
                    area_desc: a.area_desc.clone(),
                    severity: a.severity,
                    is_warning: a.is_warning(),
                    expires_secs: a.expires_secs,
                })
                .collect(),
            None => Vec::new(),
        };
        Self { visible_alerts }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alerts::types::{Alert, AlertGeometry, Ring};
    use wasm_bindgen_test::wasm_bindgen_test;

    fn square(min: f64, max: f64) -> Ring {
        vec![(min, min), (max, min), (max, max), (min, max), (min, min)]
    }

    fn alert(id: &str, severity: AlertSeverity, event: &str, polygons: Vec<Vec<Ring>>) -> Alert {
        let mut geometry = AlertGeometry {
            polygons,
            bbox: None,
        };
        geometry.recompute_bbox();
        Alert {
            id: id.into(),
            event: event.into(),
            headline: String::new(),
            description: String::new(),
            instruction: String::new(),
            severity,
            urgency: String::new(),
            certainty: String::new(),
            area_desc: String::new(),
            sender: String::new(),
            effective_secs: None,
            onset_secs: None,
            expires_secs: None,
            ends_secs: None,
            geometry,
            affected_zones: Vec::new(),
            fill_triangles: Vec::new(),
        }
    }

    const WORLD: (f64, f64, f64, f64) = (-180.0, -90.0, 180.0, 90.0);

    // "Tornado Warning" is a warning event; "Flood Watch" is not. `is_warning`
    // keys off the event string, so name fixtures accordingly.
    fn warning(id: &str, severity: AlertSeverity, poly: Vec<Vec<Ring>>) -> Alert {
        alert(id, severity, "Tornado Warning", poly)
    }
    fn advisory(id: &str, severity: AlertSeverity, poly: Vec<Vec<Ring>>) -> Alert {
        alert(id, severity, "Flood Watch", poly)
    }

    #[wasm_bindgen_test]
    fn overlapping_alerts_select_highest_severity() {
        // Two warnings cover the same point; the more severe one wins.
        let alerts = vec![
            warning("minor", AlertSeverity::Minor, vec![vec![square(0.0, 10.0)]]),
            warning(
                "extreme",
                AlertSeverity::Extreme,
                vec![vec![square(0.0, 10.0)]],
            ),
        ];
        let id = select_alert_at(&alerts, 5.0, 5.0, WORLD, true, true);
        assert_eq!(id.as_deref(), Some("extreme"));
    }

    #[wasm_bindgen_test]
    fn warning_outranks_advisory_at_same_point() {
        // A severe warning and a (higher-rank) extreme advisory overlap; with
        // both classes visible the highest rank wins regardless of class.
        let alerts = vec![
            warning("warn", AlertSeverity::Severe, vec![vec![square(0.0, 10.0)]]),
            advisory("adv", AlertSeverity::Extreme, vec![vec![square(0.0, 10.0)]]),
        ];
        let id = select_alert_at(&alerts, 5.0, 5.0, WORLD, true, true);
        assert_eq!(id.as_deref(), Some("adv"));
    }

    #[wasm_bindgen_test]
    fn class_visibility_gates_hit_test() {
        let alerts = vec![advisory(
            "adv",
            AlertSeverity::Extreme,
            vec![vec![square(0.0, 10.0)]],
        )];
        // Advisory class hidden → no hit even though the point is inside.
        assert_eq!(select_alert_at(&alerts, 5.0, 5.0, WORLD, true, false), None);
        // Advisory class visible → hit.
        assert_eq!(
            select_alert_at(&alerts, 5.0, 5.0, WORLD, true, true).as_deref(),
            Some("adv")
        );
    }

    #[wasm_bindgen_test]
    fn tie_breaks_to_first_in_list_order() {
        // Equal severity, both contain the point → first in list order wins
        // (strict `>` never replaces an equal-rank incumbent).
        let alerts = vec![
            warning(
                "first",
                AlertSeverity::Severe,
                vec![vec![square(0.0, 10.0)]],
            ),
            warning(
                "second",
                AlertSeverity::Severe,
                vec![vec![square(0.0, 10.0)]],
            ),
        ];
        assert_eq!(
            select_alert_at(&alerts, 5.0, 5.0, WORLD, true, true).as_deref(),
            Some("first")
        );
    }

    #[wasm_bindgen_test]
    fn miss_returns_none() {
        let alerts = vec![warning(
            "a",
            AlertSeverity::Severe,
            vec![vec![square(0.0, 10.0)]],
        )];
        assert_eq!(
            select_alert_at(&alerts, 50.0, 50.0, WORLD, true, true),
            None
        );
    }

    #[wasm_bindgen_test]
    fn compute_focus_centroid_and_class() {
        let a = warning("a", AlertSeverity::Severe, vec![vec![square(0.0, 10.0)]]);
        let focus = compute_alert_focus(&a);
        assert!(focus.is_warning);
        assert_eq!(focus.center, Some((5.0, 5.0)));
        // Zone-only alert (no polygons → no bbox) → no center.
        let z = advisory("z", AlertSeverity::Minor, vec![]);
        assert_eq!(compute_alert_focus(&z).center, None);
    }

    #[wasm_bindgen_test]
    fn mping_toggle_gating() {
        assert!(mping_layer_available(true, true));
        assert!(!mping_layer_available(false, true)); // not live
        assert!(!mping_layer_available(true, false)); // no key
        assert!(!mping_layer_available(false, false));
    }

    // ── reducer ──

    fn state_bundle() -> (AlertsState, MpingState, GpsState, bool) {
        (
            AlertsState::default(),
            MpingState::default(),
            GpsState::default(),
            false,
        )
    }

    fn run(
        alerts: &mut AlertsState,
        mping: &mut MpingState,
        gps: &mut GpsState,
        gps_layer: &mut bool,
        intent: DiagnosticsIntent,
    ) -> Vec<Effect> {
        reduce(
            DiagnosticsStateMut {
                alerts,
                mping,
                gps,
                gps_layer_active: gps_layer,
            },
            intent,
        )
    }

    #[wasm_bindgen_test]
    fn enable_gps_clears_state_and_emits_effect() {
        let (mut a, mut m, mut g, mut layer) = state_bundle();
        g.coords = Some((1.0, 2.0));
        g.error = Some("old".into());
        let effects = run(
            &mut a,
            &mut m,
            &mut g,
            &mut layer,
            DiagnosticsIntent::EnableGps,
        );
        assert_eq!(effects, vec![Effect::StartGeolocation]);
        assert_eq!(g.coords, None);
        assert_eq!(g.error, None);
    }

    #[wasm_bindgen_test]
    fn gps_failed_auto_disables_layer() {
        let (mut a, mut m, mut g, _) = state_bundle();
        let mut layer = true; // layer is on when the failure arrives
        g.coords = Some((1.0, 2.0));
        let effects = run(
            &mut a,
            &mut m,
            &mut g,
            &mut layer,
            DiagnosticsIntent::GpsFailed("denied".into()),
        );
        assert!(effects.is_empty());
        assert_eq!(g.error.as_deref(), Some("denied"));
        assert_eq!(g.coords, None);
        assert!(!layer, "failed lookup must auto-disable the overlay layer");
    }

    #[wasm_bindgen_test]
    fn gps_resolved_sets_coords() {
        let (mut a, mut m, mut g, mut layer) = state_bundle();
        g.error = Some("prev".into());
        run(
            &mut a,
            &mut m,
            &mut g,
            &mut layer,
            DiagnosticsIntent::GpsResolved(40.5, -90.2),
        );
        assert_eq!(g.coords, Some((40.5, -90.2)));
        assert_eq!(g.error, None);
    }

    #[wasm_bindgen_test]
    fn save_mping_key_change_invalidates() {
        let (mut a, mut m, mut g, mut layer) = state_bundle();
        m.settings_modal_open = true;
        run(
            &mut a,
            &mut m,
            &mut g,
            &mut layer,
            DiagnosticsIntent::SaveMpingApiKey(Some("KEY".into())),
        );
        assert_eq!(m.api_key.as_deref(), Some("KEY"));
        assert!(m.invalidate_requested);
        assert!(!m.settings_modal_open, "saving closes the settings modal");
    }

    #[wasm_bindgen_test]
    fn save_mping_key_unchanged_is_noop_but_closes() {
        let (mut a, mut m, mut g, mut layer) = state_bundle();
        m.api_key = Some("KEY".into());
        m.settings_modal_open = true;
        run(
            &mut a,
            &mut m,
            &mut g,
            &mut layer,
            DiagnosticsIntent::SaveMpingApiKey(Some("KEY".into())),
        );
        // Same key → no invalidation, but the modal still closes.
        assert!(!m.invalidate_requested);
        assert!(!m.settings_modal_open);
    }

    #[wasm_bindgen_test]
    fn clear_mping_key_wipes_reports() {
        let (mut a, mut m, mut g, mut layer) = state_bundle();
        m.api_key = Some("KEY".into());
        m.total_count = 7;
        m.last_success_ms = 123.0;
        run(
            &mut a,
            &mut m,
            &mut g,
            &mut layer,
            DiagnosticsIntent::ClearMpingKey,
        );
        assert_eq!(m.api_key, None);
        assert_eq!(m.total_count, 0);
        assert_eq!(m.last_success_ms, 0.0);
        assert!(m.invalidate_requested);
    }

    #[wasm_bindgen_test]
    fn alert_selection_round_trip() {
        let (mut a, mut m, mut g, mut layer) = state_bundle();
        run(
            &mut a,
            &mut m,
            &mut g,
            &mut layer,
            DiagnosticsIntent::SelectAlert("x".into()),
        );
        assert_eq!(a.selected_alert_id.as_deref(), Some("x"));
        run(
            &mut a,
            &mut m,
            &mut g,
            &mut layer,
            DiagnosticsIntent::ClearAlertSelection,
        );
        assert_eq!(a.selected_alert_id, None);
    }
}
