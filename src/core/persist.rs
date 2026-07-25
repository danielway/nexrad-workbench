//! Pure persistence decision: when (and what) to push to the URL bar and save
//! to localStorage.
//!
//! This is the P1 reference for the effect-as-data boundary on a non-visual
//! path. The decision is split into two pure halves so the shell keeps its
//! laziness without core ever seeing `AppState`:
//!
//! 1. [`persist_due`] — the throttle gate. The shell checks it first and does
//!    no snapshot work on suppressed frames.
//! 2. [`decide_persist`] — given the pre-built [`UrlPush`] and the current
//!    preferences snapshot (both assembled by the shell from app state), emit
//!    the [`Effect`]s and the tracking updates.
//!
//! The shell ([`crate::nexrad::PersistenceManager`] +
//! [`crate::WorkbenchApp::apply_effects`]) injects the frame clock, builds the
//! inputs (`state::url_state::build_url_push`,
//! `UserPreferences::from_app_state`), applies the returned tracking updates,
//! and executes the [`Effect`]s.

use crate::core::effect::{Effect, UrlPush};
use crate::core::UserPreferences;

/// Minimum wall-clock seconds between URL-bar pushes. Preference saves piggyback
/// on the same throttle window. (Was a hardcoded `1.0` inside `persist_if_due`.)
pub const PERSIST_THROTTLE_SECS: f64 = 1.0;

/// Pure throttle gate: true when the persist window has elapsed since the last
/// push. Negative elapsed (clock ran backwards / marker in the future) is still
/// inside the window, so it suppresses.
pub fn persist_due(now_secs: f64, last_url_push_secs: f64) -> bool {
    now_secs - last_url_push_secs >= PERSIST_THROTTLE_SECS
}

/// Outcome of [`decide_persist`]: the effects to perform plus the throttle /
/// preference-snapshot tracking the shell should adopt.
///
/// Both tracking fields are `Some` *iff* they changed this call, so the shell
/// applies them verbatim and a no-op decision leaves its state untouched. This
/// keeps the bookkeeping pure and assertable (e.g. "an unchanged prefs snapshot
/// emits no `SavePreferences`") rather than hiding it in the manager.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct PersistDecision {
    /// Effects for the shell to execute, in order.
    pub effects: Vec<Effect>,
    /// New throttle marker; `Some(now)` iff the throttle gate passed this call.
    pub last_url_push_secs: Option<f64>,
    /// New saved-prefs snapshot; `Some` iff prefs changed and a save was emitted.
    pub saved_preferences: Option<UserPreferences>,
}

/// Emit the persistence effects for a frame whose [`persist_due`] gate passed.
///
/// Mirrors the historical `PersistenceManager::persist_if_due` exactly:
/// 1. Emit an [`Effect::PushUrl`] with the shell-built payload and advance the
///    throttle marker to `now_secs`.
/// 2. If `current_prefs` differs from `last_saved_preferences`, also emit
///    [`Effect::SavePreferences`] and report the new snapshot.
pub fn decide_persist(
    now_secs: f64,
    url_push: UrlPush,
    current_prefs: UserPreferences,
    last_saved_preferences: &UserPreferences,
) -> PersistDecision {
    let mut effects = vec![Effect::PushUrl(url_push)];

    // Save user preferences if changed (piggyback on the URL throttle).
    let saved_preferences = if current_prefs != *last_saved_preferences {
        effects.push(Effect::SavePreferences(Box::new(current_prefs.clone())));
        Some(current_prefs)
    } else {
        None
    };

    PersistDecision {
        effects,
        last_url_push_secs: Some(now_secs),
        saved_preferences,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ViewState;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn push() -> UrlPush {
        UrlPush {
            site: "KDMX".to_string(),
            time: 1_700_000_000.0,
            product: "REF".to_string(),
            lat: 41.7,
            lon: -93.7,
            view: ViewState::default(),
            dev: false,
        }
    }

    #[wasm_bindgen_test]
    fn throttle_suppresses_within_window() {
        // last push at t=100.0; now=100.5 → 0.5s < 1.0s throttle → not due.
        assert!(!persist_due(100.5, 100.0));
    }

    #[wasm_bindgen_test]
    fn throttle_boundary_exactly_one_second_passes() {
        // Exactly 1.0s elapsed → the historical `< 1.0` suppression is false.
        assert!(persist_due(101.0, 100.0));
    }

    #[wasm_bindgen_test]
    fn push_emitted_and_throttle_advanced_when_due() {
        let saved = UserPreferences::default();
        let d = decide_persist(200.0, push(), saved.clone(), &saved);
        assert_eq!(d.last_url_push_secs, Some(200.0));
        // Unchanged prefs → exactly one effect, the URL push.
        assert_eq!(d.effects.len(), 1);
        assert!(matches!(d.effects[0], Effect::PushUrl(_)));
        assert!(d.saved_preferences.is_none());
    }

    #[wasm_bindgen_test]
    fn changed_prefs_also_emit_save() {
        let saved = UserPreferences::default();
        let mut current = saved.clone();
        current.use_local_time = !saved.use_local_time;
        let d = decide_persist(200.0, push(), current.clone(), &saved);
        assert_eq!(d.effects.len(), 2);
        assert!(matches!(d.effects[0], Effect::PushUrl(_)));
        match &d.effects[1] {
            Effect::SavePreferences(p) => assert_eq!(p.use_local_time, current.use_local_time),
            other => panic!("expected SavePreferences, got {:?}", other),
        }
        assert_eq!(d.saved_preferences, Some(current));
    }

    #[wasm_bindgen_test]
    fn unchanged_prefs_emit_no_save() {
        let saved = UserPreferences::default();
        let d = decide_persist(200.0, push(), saved.clone(), &saved);
        assert!(!d
            .effects
            .iter()
            .any(|e| matches!(e, Effect::SavePreferences(_))));
        assert!(d.saved_preferences.is_none());
    }

    #[wasm_bindgen_test]
    fn changed_mping_key_emits_save_carrying_the_key() {
        // The mPING key is sourced separately by the shell (from the
        // diagnostics subsystem, not AppState), so a snapshot whose key differs
        // from the saved one must trigger a SavePreferences carrying the key.
        let saved = UserPreferences::default(); // mping_api_key == None
        let mut current = saved.clone();
        current.mping_api_key = Some("ABC123".to_string());
        let d = decide_persist(200.0, push(), current, &saved);
        assert_eq!(d.effects.len(), 2);
        match &d.effects[1] {
            Effect::SavePreferences(p) => {
                assert_eq!(p.mping_api_key.as_deref(), Some("ABC123"));
            }
            other => panic!("expected SavePreferences, got {:?}", other),
        }
        // The reported snapshot mirrors what was saved (key included).
        let snap = d.saved_preferences.expect("snapshot when changed");
        assert_eq!(snap.mping_api_key.as_deref(), Some("ABC123"));
    }

    #[wasm_bindgen_test]
    fn saved_snapshot_equals_save_effect_payload() {
        // When prefs change, the returned `saved_preferences` snapshot the shell
        // should adopt must be byte-for-byte the same value carried in the
        // SavePreferences effect — otherwise the shell's tracking would drift
        // from what was persisted and re-save every frame.
        let saved = UserPreferences::default();
        let mut current = saved.clone();
        current.advanced_mode = !saved.advanced_mode;
        let d = decide_persist(200.0, push(), current.clone(), &saved);
        let snap = d.saved_preferences.clone().expect("snapshot when changed");
        let payload = match &d.effects[1] {
            Effect::SavePreferences(p) => (**p).clone(),
            other => panic!("expected SavePreferences, got {:?}", other),
        };
        assert_eq!(snap, payload);
        // And it reflects the changed field, not the stale `saved`.
        assert_eq!(snap.advanced_mode, current.advanced_mode);
    }

    #[wasm_bindgen_test]
    fn url_push_payload_passes_through_verbatim() {
        let saved = UserPreferences::default();
        let d = decide_persist(200.0, push(), saved.clone(), &saved);
        match &d.effects[0] {
            Effect::PushUrl(p) => {
                assert_eq!(p.site, "KDMX");
                assert_eq!(p.product, "REF");
                assert_eq!(p.time, 1_700_000_000.0);
                assert_eq!(p.lat, 41.7);
                assert_eq!(p.lon, -93.7);
                assert!(!p.dev);
            }
            other => panic!("expected PushUrl, got {:?}", other),
        }
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use crate::core::ViewState;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn push() -> UrlPush {
        UrlPush {
            site: "KOAX".to_string(),
            time: 0.0,
            product: "REF".to_string(),
            lat: 0.0,
            lon: 0.0,
            view: ViewState::default(),
            dev: false,
        }
    }

    #[wasm_bindgen_test]
    fn throttle_constant_is_one_second() {
        // The gate compares against this constant; pin it so a silent change to
        // the throttle window is caught.
        assert!((PERSIST_THROTTLE_SECS - 1.0).abs() < 1e-12);
    }

    #[wasm_bindgen_test]
    fn now_before_last_push_is_suppressed() {
        // Negative elapsed (clock ran backwards / marker in the future) is still
        // inside the window, so the gate suppresses.
        assert!(!persist_due(50.0, 100.0));
    }

    #[wasm_bindgen_test]
    fn just_below_boundary_is_suppressed() {
        // 0.9999s elapsed is strictly inside the window → suppressed.
        assert!(!persist_due(100.9999, 100.0));
    }

    #[wasm_bindgen_test]
    fn just_above_boundary_fires_and_marks_exact_now() {
        // 1.0001s elapsed (just past the window) fires, and the new throttle
        // marker is exactly `now_secs` — not rounded, not the elapsed delta.
        let now = 100.0001 + 1.0; // 101.0001
        assert!(persist_due(now, 100.0));
        let saved = UserPreferences::default();
        let d = decide_persist(now, push(), saved.clone(), &saved);
        assert_eq!(d.last_url_push_secs, Some(now));
        assert!(matches!(d.effects.first(), Some(Effect::PushUrl(_))));
    }
}
