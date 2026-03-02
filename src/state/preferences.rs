//! User preferences persisted to localStorage.
//!
//! Covers playback speed, visualization settings, and layer visibility.
//! Loaded on startup, saved automatically when changes are detected.

use serde::{Deserialize, Serialize};

use super::{AppState, PlaybackSpeed, RenderMode};

/// User preferences that persist across page reloads.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct UserPreferences {
    #[serde(default)]
    pub speed: PlaybackSpeed,
    #[serde(default)]
    pub render_mode: RenderMode,
    #[serde(default = "default_true")]
    pub layer_states: bool,
    #[serde(default = "default_true")]
    pub layer_counties: bool,
    #[serde(default = "default_true")]
    pub layer_labels: bool,
    #[serde(default)]
    pub layer_nexrad_sites: bool,
}

fn default_true() -> bool {
    true
}

impl Default for UserPreferences {
    fn default() -> Self {
        Self {
            speed: PlaybackSpeed::default(),
            render_mode: RenderMode::default(),
            layer_states: true,
            layer_counties: true,
            layer_labels: true,
            layer_nexrad_sites: false,
        }
    }
}

impl UserPreferences {
    const STORAGE_KEY: &'static str = "nexrad_user_preferences";

    /// Snapshot current preferences from application state.
    pub fn from_app_state(state: &AppState) -> Self {
        Self {
            speed: state.playback_state.speed,
            render_mode: state.viz_state.render_mode,
            layer_states: state.layer_state.geo.states,
            layer_counties: state.layer_state.geo.counties,
            layer_labels: state.layer_state.geo.labels,
            layer_nexrad_sites: state.layer_state.geo.nexrad_sites,
        }
    }

    /// Apply loaded preferences to application state.
    pub fn apply_to(&self, state: &mut AppState) {
        state.playback_state.speed = self.speed;
        state.viz_state.render_mode = self.render_mode;
        state.layer_state.geo.states = self.layer_states;
        state.layer_state.geo.counties = self.layer_counties;
        state.layer_state.geo.labels = self.layer_labels;
        state.layer_state.geo.nexrad_sites = self.layer_nexrad_sites;
    }

    /// Load preferences from localStorage.
    pub fn load() -> Self {
        let window = match web_sys::window() {
            Some(w) => w,
            None => return Self::default(),
        };

        let storage = match window.local_storage() {
            Ok(Some(s)) => s,
            _ => return Self::default(),
        };

        let json = match storage.get_item(Self::STORAGE_KEY) {
            Ok(Some(s)) => s,
            _ => return Self::default(),
        };

        match serde_json::from_str(&json) {
            Ok(prefs) => {
                log::info!("Loaded user preferences from localStorage");
                prefs
            }
            Err(e) => {
                log::warn!("Failed to parse user preferences: {}", e);
                Self::default()
            }
        }
    }

    /// Save preferences to localStorage.
    pub fn save(&self) {
        let window = match web_sys::window() {
            Some(w) => w,
            None => return,
        };

        let storage = match window.local_storage() {
            Ok(Some(s)) => s,
            _ => return,
        };

        let json = match serde_json::to_string(self) {
            Ok(s) => s,
            Err(e) => {
                log::warn!("Failed to serialize user preferences: {}", e);
                return;
            }
        };

        if let Err(e) = storage.set_item(Self::STORAGE_KEY, &json) {
            log::warn!("Failed to save user preferences: {:?}", e);
        }
    }
}
