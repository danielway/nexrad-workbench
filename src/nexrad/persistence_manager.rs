//! Persistence manager: URL state pushing and user preference saving.
//!
//! Throttles URL bar updates to ~1/sec and detects site changes.

use crate::state::{self, AppState};

/// Manages URL state persistence, preference saving, and site change detection.
pub struct PersistenceManager {
    /// Monotonic instant of last URL push (for throttling to ~1/sec).
    last_url_push: web_time::Instant,
    /// Last-saved user preferences snapshot (for change detection).
    last_saved_preferences: state::UserPreferences,
    /// Previous site ID to detect site changes.
    previous_site_id: String,
}

impl PersistenceManager {
    pub fn new(initial_site_id: String, initial_prefs: state::UserPreferences) -> Self {
        Self {
            last_url_push: web_time::Instant::now(),
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

    /// Push current app state to the URL bar and save user preferences (throttled).
    ///
    /// `mping_api_key` is the current value from the diagnostics subsystem;
    /// `is_live` is sourced from the Live subsystem. Both are passed in
    /// rather than reached for so the persistence manager doesn't take
    /// back-references to subsystems.
    pub fn persist_if_due(
        &mut self,
        state: &AppState,
        mping_api_key: Option<String>,
        is_live: bool,
    ) {
        let now = web_time::Instant::now();
        if now.duration_since(self.last_url_push).as_secs_f64() < 1.0 {
            return;
        }
        self.last_url_push = now;

        // `ViewState::from_state` keeps the field-by-field mapping in
        // one place; adding a new auxiliary view field is then a single
        // edit there instead of two synchronized lists.
        let view = state::url_state::ViewState::from_state(state, is_live);
        state::url_state::push_to_url(
            &state.viz_state.site_id,
            state.playback_state.playback_position(),
            state.viz_state.product.short_code(),
            state.viz_state.center_lat,
            state.viz_state.center_lon,
            &view,
            state.dev_mode,
        );

        // Save user preferences if changed (piggyback on URL throttle)
        let current_prefs = state::UserPreferences::from_app_state(state, mping_api_key);
        if current_prefs != self.last_saved_preferences {
            current_prefs.save();
            self.last_saved_preferences = current_prefs;
        }
    }
}
