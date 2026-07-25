//! Pure persistence decision: when (and what) to push to the URL bar and save
//! to localStorage.
//!
//! This is the P1 reference for the effect-as-data boundary on a non-visual
//! path. The decision is pure — `(now, throttle/prefs tracking, view state) ->
//! (effects, updated tracking)` — so the throttle gate and preference
//! change-detection are unit-testable with no browser, no `web_sys`, and no
//! real clock. The shell ([`crate::nexrad::PersistenceManager`] +
//! [`crate::WorkbenchApp::apply_effects`]) injects the frame clock, applies the
//! returned tracking updates, and executes the [`Effect`]s.

use crate::core::effect::{Effect, UrlPush};
use crate::state::url_state::ViewState;
use crate::state::{AppState, PlaybackState, UserPreferences};

/// Minimum wall-clock seconds between URL-bar pushes. Preference saves piggyback
/// on the same throttle window. (Was a hardcoded `1.0` inside `persist_if_due`.)
pub const PERSIST_THROTTLE_SECS: f64 = 1.0;

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

/// Decide whether to push the URL state and/or save preferences this frame.
///
/// Mirrors the historical `PersistenceManager::persist_if_due` exactly:
/// 1. Throttle: if `now - last_url_push_secs < PERSIST_THROTTLE_SECS`, do nothing.
/// 2. Otherwise emit a [`Effect::PushUrl`] and advance the throttle marker.
/// 3. Snapshot current preferences; if they differ from `last_saved_preferences`,
///    also emit [`Effect::SavePreferences`] and report the new snapshot.
pub fn decide_persist(
    now_secs: f64,
    last_url_push_secs: f64,
    state: &AppState,
    playback: &PlaybackState,
    is_live: bool,
    mping_api_key: Option<String>,
    last_saved_preferences: &UserPreferences,
) -> PersistDecision {
    if now_secs - last_url_push_secs < PERSIST_THROTTLE_SECS {
        return PersistDecision::default();
    }

    // `ViewState::from_state` keeps the field-by-field mapping in one place.
    let view = ViewState::from_state(state, playback, is_live);
    let mut effects = vec![Effect::PushUrl(UrlPush {
        site: state.viz_state.site_id.clone(),
        time: playback.playback_position(),
        product: state.viz_state.product.short_code().to_string(),
        lat: state.viz_state.center_lat,
        lon: state.viz_state.center_lon,
        view,
        dev: state.dev_mode,
    })];

    // Save user preferences if changed (piggyback on the URL throttle).
    let current_prefs = UserPreferences::from_app_state(state, playback, mping_api_key);
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
    use wasm_bindgen_test::wasm_bindgen_test;

    fn fixture() -> (AppState, PlaybackState, UserPreferences) {
        let state = AppState::default();
        let playback = PlaybackState::default();
        // Baseline snapshot matching the fixture state, so a no-change frame
        // emits no SavePreferences unless we mutate something.
        let prefs = UserPreferences::from_app_state(&state, &playback, None);
        (state, playback, prefs)
    }

    #[wasm_bindgen_test]
    fn throttle_suppresses_within_window() {
        let (state, playback, saved) = fixture();
        // last push at t=100.0; now=100.5 → 0.5s < 1.0s throttle → nothing.
        let d = decide_persist(100.5, 100.0, &state, &playback, false, None, &saved);
        assert_eq!(d, PersistDecision::default());
        assert!(d.effects.is_empty());
        assert!(d.last_url_push_secs.is_none());
    }

    #[wasm_bindgen_test]
    fn throttle_boundary_exactly_one_second_passes() {
        let (state, playback, saved) = fixture();
        // Exactly 1.0s elapsed → `< 1.0` is false → push fires (matches `< 1.0`).
        let d = decide_persist(101.0, 100.0, &state, &playback, false, None, &saved);
        assert_eq!(d.last_url_push_secs, Some(101.0));
        assert!(matches!(d.effects.first(), Some(Effect::PushUrl(_))));
    }

    #[wasm_bindgen_test]
    fn push_emitted_and_throttle_advanced_when_due() {
        let (state, playback, saved) = fixture();
        let d = decide_persist(200.0, 100.0, &state, &playback, false, None, &saved);
        assert_eq!(d.last_url_push_secs, Some(200.0));
        // Unchanged prefs → exactly one effect, the URL push.
        assert_eq!(d.effects.len(), 1);
        assert!(matches!(d.effects[0], Effect::PushUrl(_)));
        assert!(d.saved_preferences.is_none());
    }

    #[wasm_bindgen_test]
    fn changed_prefs_also_emit_save() {
        let (mut state, playback, saved) = fixture();
        // Flip a preference-backed field so the snapshot differs from `saved`.
        state.use_local_time = !saved.use_local_time;
        let d = decide_persist(200.0, 100.0, &state, &playback, false, None, &saved);
        assert_eq!(d.effects.len(), 2);
        assert!(matches!(d.effects[0], Effect::PushUrl(_)));
        match &d.effects[1] {
            Effect::SavePreferences(p) => assert_eq!(p.use_local_time, state.use_local_time),
            other => panic!("expected SavePreferences, got {:?}", other),
        }
        assert!(d.saved_preferences.is_some());
    }

    #[wasm_bindgen_test]
    fn unchanged_prefs_emit_no_save() {
        let (state, playback, saved) = fixture();
        let d = decide_persist(200.0, 100.0, &state, &playback, false, None, &saved);
        assert!(!d
            .effects
            .iter()
            .any(|e| matches!(e, Effect::SavePreferences(_))));
        assert!(d.saved_preferences.is_none());
    }

    #[wasm_bindgen_test]
    fn url_push_carries_view_fields() {
        let (mut state, playback, saved) = fixture();
        state.viz_state.site_id = "KDMX".to_string();
        state.dev_mode = true;
        let d = decide_persist(200.0, 100.0, &state, &playback, true, None, &saved);
        match &d.effects[0] {
            Effect::PushUrl(p) => {
                assert_eq!(p.site, "KDMX");
                assert!(p.dev);
                // `is_live=true` is encoded into the view blob's `rt` flag.
                assert_eq!(p.view.rt, Some(true));
            }
            other => panic!("expected PushUrl, got {:?}", other),
        }
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use crate::core::RadarProduct;
    use wasm_bindgen_test::wasm_bindgen_test;

    // Same baseline-snapshot idiom as `mod tests::fixture`: a prefs snapshot
    // matching the (possibly mutated) fixture state, so a no-change frame emits
    // no SavePreferences. Callers mutate `state`/`playback` BEFORE snapshotting
    // when they want an unchanged-prefs frame, or pass the un-mutated `saved`
    // when they want to force a change.
    fn fixture() -> (AppState, PlaybackState, UserPreferences) {
        let state = AppState::default();
        let playback = PlaybackState::default();
        let prefs = UserPreferences::from_app_state(&state, &playback, None);
        (state, playback, prefs)
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
        // `< 1.0`, so the gate suppresses and the no-op leaves tracking untouched.
        let (state, playback, saved) = fixture();
        let d = decide_persist(50.0, 100.0, &state, &playback, false, None, &saved);
        assert_eq!(d, PersistDecision::default());
        assert!(d.effects.is_empty());
        assert!(d.last_url_push_secs.is_none());
        assert!(d.saved_preferences.is_none());
    }

    #[wasm_bindgen_test]
    fn just_below_boundary_is_suppressed() {
        // 0.9999s elapsed is strictly `< 1.0` → suppressed. Guards the boundary
        // from the suppressed side (the existing test only checks 0.5s and the
        // exactly-1.0 pass).
        let (state, playback, saved) = fixture();
        let d = decide_persist(100.9999, 100.0, &state, &playback, false, None, &saved);
        assert!(d.effects.is_empty());
        assert!(d.last_url_push_secs.is_none());
    }

    #[wasm_bindgen_test]
    fn just_above_boundary_fires_and_marks_exact_now() {
        // 1.0001s elapsed (just past the window) fires, and the new throttle
        // marker is exactly `now_secs` — not rounded, not the elapsed delta.
        let (state, playback, saved) = fixture();
        let now = 100.0001 + 1.0; // 101.0001
        let d = decide_persist(now, 100.0, &state, &playback, false, None, &saved);
        assert_eq!(d.last_url_push_secs, Some(now));
        assert!(matches!(d.effects.first(), Some(Effect::PushUrl(_))));
    }

    #[wasm_bindgen_test]
    fn url_push_carries_product_lat_lon_and_time() {
        // The existing `url_push_carries_view_fields` only asserts site/dev/rt.
        // Cover the remaining scalar payload: product short_code, center coords,
        // and the playback position threaded through as `time`.
        let (mut state, mut playback, _) = fixture();
        state.viz_state.product = RadarProduct::Velocity; // short_code "VEL"
        state.viz_state.center_lat = 41.25;
        state.viz_state.center_lon = -93.75;
        playback.set_playback_position(1_700_000_000.5);
        // Snapshot AFTER mutating so prefs are unchanged → single URL effect.
        let saved = UserPreferences::from_app_state(&state, &playback, None);

        let d = decide_persist(200.0, 100.0, &state, &playback, false, None, &saved);
        match &d.effects[0] {
            Effect::PushUrl(p) => {
                assert_eq!(p.product, "VEL");
                assert!((p.lat - 41.25).abs() < 1e-9);
                assert!((p.lon - (-93.75)).abs() < 1e-9);
                assert!((p.time - 1_700_000_000.5).abs() < 1e-6);
            }
            other => panic!("expected PushUrl, got {:?}", other),
        }
    }

    #[wasm_bindgen_test]
    fn url_push_view_matches_from_state() {
        // The view blob in the effect must be exactly what ViewState::from_state
        // produces for the same (state, playback, is_live) — the doc-comment
        // contract that the mapping lives in one place.
        let (state, playback, saved) = fixture();
        let is_live = true;
        let d = decide_persist(200.0, 100.0, &state, &playback, is_live, None, &saved);
        let expected = ViewState::from_state(&state, &playback, is_live);
        match &d.effects[0] {
            Effect::PushUrl(p) => assert_eq!(p.view, expected),
            other => panic!("expected PushUrl, got {:?}", other),
        }
    }

    #[wasm_bindgen_test]
    fn changed_mping_key_emits_save_carrying_the_key() {
        // Distinct from the existing `use_local_time` change test: the mPING key
        // is sourced separately (passed in, not on AppState), so a key that
        // differs from the saved snapshot's None must trigger a SavePreferences
        // whose payload carries the new key.
        let (state, playback, saved) = fixture(); // saved.mping_api_key == None
        let d = decide_persist(
            200.0,
            100.0,
            &state,
            &playback,
            false,
            Some("ABC123".to_string()),
            &saved,
        );
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
        let (mut state, playback, saved) = fixture();
        state.advanced_mode = !saved.advanced_mode; // flip a prefs-backed field
        let d = decide_persist(200.0, 100.0, &state, &playback, false, None, &saved);
        let snap = d.saved_preferences.clone().expect("snapshot when changed");
        let payload = match &d.effects[1] {
            Effect::SavePreferences(p) => (**p).clone(),
            other => panic!("expected SavePreferences, got {:?}", other),
        };
        assert_eq!(snap, payload);
        // And it reflects the mutated field, not the stale `saved`.
        assert_eq!(snap.advanced_mode, state.advanced_mode);
    }
}
