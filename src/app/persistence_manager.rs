//! Persistence manager: URL state pushing and user preference saving.
//!
//! Holds the throttle marker and preference snapshot that the *pure* persistence
//! decision ([`crate::core::decide_persist`]) reads. This shell injects the
//! frame clock, adopts the decision's tracking updates, and returns the
//! [`Effect`]s for [`crate::WorkbenchApp::apply_effects`] to execute. The
//! throttle math and change-detection are tested headlessly in `core::persist`.

use crate::core::Preferences;
use crate::core::{decide_persist, persist_due, Effect, PersistDecision, PlaybackState, TimeModel};
use crate::state::AppState;

/// Manages URL state persistence, preference saving, and site change detection.
pub(crate) struct PersistenceManager {
    /// Wall-clock seconds of the last URL push (for throttling to ~1/sec).
    /// Wall-clock (the injected [`crate::core::FrameNow`]) rather than a
    /// monotonic `Instant`, so the decision is clock-injectable and testable.
    last_url_push_secs: f64,
    /// Last-saved user preferences snapshot (for change detection).
    last_saved_preferences: Preferences,
    /// The focus site's id at the last check, to detect site changes.
    previous_site_id: String,
}

impl PersistenceManager {
    pub(crate) fn new(initial_site_id: String, initial_prefs: Preferences) -> Self {
        Self {
            // Seed with the construction-time wall clock so the first push still
            // waits a throttle window, preserving the old `Instant::now()` seed.
            last_url_push_secs: TimeModel::wall_clock_time(),
            last_saved_preferences: initial_prefs,
            previous_site_id: initial_site_id,
        }
    }

    /// Returns true if the focus site's id has changed since the last check,
    /// updating the internal tracker.
    pub(crate) fn detect_site_change(&mut self, current_site_id: &str) -> bool {
        if current_site_id != self.previous_site_id {
            log::info!(
                "Site changed from {} to {}",
                self.previous_site_id,
                current_site_id
            );
            self.previous_site_id = current_site_id.to_string();
            true
        } else {
            false
        }
    }

    /// Decide and record persistence for this frame, returning the effects for
    /// the shell to execute (throttled URL push + conditional preference save).
    ///
    /// `now_secs` is the injected frame clock ([`crate::core::FrameNow::secs`]).
    /// `focus` is the site the link names; its position is the link's map
    /// centre until the map has a centre of its own.
    /// `mping_api_key` is the current value from the diagnostics subsystem;
    /// `playback` is the Playback subsystem's state. All are passed in so the persistence manager doesn't
    /// take back-references to subsystems.
    pub(crate) fn persist_if_due(
        &mut self,
        now_secs: f64,
        state: &AppState,
        playback: &PlaybackState,
        focus: &crate::subsystem::SiteState,
        panel: crate::state::PanelPrefs,
        mping_api_key: Option<String>,
    ) -> Vec<Effect> {
        // Gate first so suppressed frames do no snapshot work; then build the
        // core's inputs from app state and let the pure decision emit effects.
        if !persist_due(now_secs, self.last_url_push_secs) {
            return Vec::new();
        }
        let link = crate::state::url_state::build_link(
            state,
            playback,
            &focus.id.0,
            (focus.lat, focus.lon),
        );
        let current_prefs = Preferences::from_app_state(
            state,
            playback,
            panel,
            mping_api_key,
            &self.last_saved_preferences,
        );
        let PersistDecision {
            effects,
            last_url_push_secs,
            saved_preferences,
        } = decide_persist(now_secs, link, current_prefs, &self.last_saved_preferences);
        if let Some(t) = last_url_push_secs {
            self.last_url_push_secs = t;
        }
        if let Some(p) = saved_preferences {
            self.last_saved_preferences = p;
        }
        effects
    }
}
