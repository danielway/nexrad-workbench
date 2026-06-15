//! Persistence manager: URL state pushing and user preference saving.
//!
//! Holds the throttle marker and preference snapshot that the *pure* persistence
//! decision ([`crate::core::decide_persist`]) reads. This shell injects the
//! frame clock, adopts the decision's tracking updates, and returns the
//! [`Effect`]s for [`crate::WorkbenchApp::apply_effects`] to execute. The
//! throttle math and change-detection are tested headlessly in `core::persist`.

use crate::core::{decide_persist, Effect, PersistDecision};
use crate::state::{self, AppState, UserPreferences};

/// Manages URL state persistence, preference saving, and site change detection.
pub struct PersistenceManager {
    /// Wall-clock seconds of the last URL push (for throttling to ~1/sec).
    /// Wall-clock (the injected [`crate::state::FrameNow`]) rather than a
    /// monotonic `Instant`, so the decision is clock-injectable and testable.
    last_url_push_secs: f64,
    /// Last-saved user preferences snapshot (for change detection).
    last_saved_preferences: UserPreferences,
    /// Previous site ID to detect site changes.
    previous_site_id: String,
}

impl PersistenceManager {
    pub fn new(initial_site_id: String, initial_prefs: UserPreferences) -> Self {
        Self {
            // Seed with the construction-time wall clock so the first push still
            // waits a throttle window, preserving the old `Instant::now()` seed.
            last_url_push_secs: state::TimeModel::wall_clock_time(),
            last_saved_preferences: initial_prefs,
            previous_site_id: initial_site_id,
        }
    }

    /// Returns true if the site has changed since last check, updating the internal tracker.
    pub fn detect_site_change(&mut self, current_site_id: &str) -> bool {
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
    /// `now_secs` is the injected frame clock ([`crate::state::FrameNow::secs`]).
    /// `mping_api_key` is the current value from the diagnostics subsystem;
    /// `is_live` is sourced from the Live subsystem; `playback` is the Playback
    /// subsystem's state. All are passed in so the persistence manager doesn't
    /// take back-references to subsystems.
    pub fn persist_if_due(
        &mut self,
        now_secs: f64,
        state: &AppState,
        playback: &state::PlaybackState,
        mping_api_key: Option<String>,
        is_live: bool,
    ) -> Vec<Effect> {
        let PersistDecision {
            effects,
            last_url_push_secs,
            saved_preferences,
        } = decide_persist(
            now_secs,
            self.last_url_push_secs,
            state,
            playback,
            is_live,
            mping_api_key,
            &self.last_saved_preferences,
        );
        if let Some(t) = last_url_push_secs {
            self.last_url_push_secs = t;
        }
        if let Some(p) = saved_preferences {
            self.last_saved_preferences = p;
        }
        effects
    }
}
