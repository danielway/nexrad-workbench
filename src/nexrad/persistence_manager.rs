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
    pub fn persist_if_due(&mut self, state: &AppState) {
        let now = web_time::Instant::now();
        if now.duration_since(self.last_url_push).as_secs_f64() < 1.0 {
            return;
        }
        self.last_url_push = now;

        let cam = &state.viz_state.camera;
        let view = state::url_state::ViewState {
            mz: Some(state.viz_state.zoom),
            tz: Some(state.playback_state.timeline_zoom),
            vm: Some(match state.viz_state.view_mode {
                state::ViewMode::Flat2D => 0,
                state::ViewMode::Globe3D => 1,
            }),
            cm: Some(match cam.mode {
                state::CameraMode::PlanetOrbit => 0,
                state::CameraMode::SiteOrbit => 1,
                state::CameraMode::FreeLook => 2,
            }),
            cd: Some(cam.distance),
            clat: Some(cam.center_lat),
            clon: Some(cam.center_lon),
            ct: Some(cam.tilt),
            cr: Some(cam.rotation),
            ob: Some(cam.orbit_bearing),
            oe: Some(cam.orbit_elevation),
            fp: Some([cam.free_pos.x, cam.free_pos.y, cam.free_pos.z]),
            fy: Some(cam.free_yaw),
            fpt: Some(cam.free_pitch),
            fs: Some(cam.free_speed),
            v3d: Some(state.viz_state.volume_3d_enabled),
            vdc: Some(state.viz_state.volume_density_cutoff),
        };
        state::url_state::push_to_url(
            &state.viz_state.site_id,
            state.playback_state.playback_position(),
            state.viz_state.product.short_code(),
            state.viz_state.center_lat,
            state.viz_state.center_lon,
            &view,
        );

        // Save user preferences if changed (piggyback on URL throttle)
        let current_prefs = state::UserPreferences::from_app_state(state);
        if current_prefs != self.last_saved_preferences {
            current_prefs.save();
            self.last_saved_preferences = current_prefs;
        }
    }
}
