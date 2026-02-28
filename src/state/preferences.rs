//! User preferences persisted to localStorage.
//!
//! Covers playback speed, visualization settings, and layer visibility.
//! Loaded on startup, saved automatically when changes are detected.

use serde::{Deserialize, Serialize};

use super::{
    AppState, ColorPalette, InterpolationMode, PlaybackSpeed, RenderMode, SmoothingMode,
};

/// User preferences that persist across page reloads.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct UserPreferences {
    #[serde(default)]
    pub speed: PlaybackSpeed,
    #[serde(default)]
    pub palette: ColorPalette,
    #[serde(default)]
    pub interpolation: InterpolationMode,
    #[serde(default)]
    pub render_mode: RenderMode,
    #[serde(default)]
    pub processing_enabled: bool,
    #[serde(default)]
    pub threshold_min: Option<f32>,
    #[serde(default)]
    pub threshold_max: Option<f32>,
    #[serde(default)]
    pub smoothing: SmoothingMode,
    #[serde(default = "default_smoothing_strength")]
    pub smoothing_strength: u8,
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

fn default_smoothing_strength() -> u8 {
    3
}

impl Default for UserPreferences {
    fn default() -> Self {
        Self {
            speed: PlaybackSpeed::default(),
            palette: ColorPalette::default(),
            interpolation: InterpolationMode::default(),
            render_mode: RenderMode::default(),
            processing_enabled: false,
            threshold_min: None,
            threshold_max: None,
            smoothing: SmoothingMode::default(),
            smoothing_strength: 3,
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
            palette: state.viz_state.palette,
            interpolation: state.viz_state.interpolation,
            render_mode: state.viz_state.render_mode,
            processing_enabled: state.viz_state.processing.enabled,
            threshold_min: state.viz_state.processing.threshold_min,
            threshold_max: state.viz_state.processing.threshold_max,
            smoothing: state.viz_state.processing.smoothing,
            smoothing_strength: state.viz_state.processing.smoothing_strength,
            layer_states: state.layer_state.geo.states,
            layer_counties: state.layer_state.geo.counties,
            layer_labels: state.layer_state.geo.labels,
            layer_nexrad_sites: state.layer_state.geo.nexrad_sites,
        }
    }

    /// Apply loaded preferences to application state.
    pub fn apply_to(&self, state: &mut AppState) {
        state.playback_state.speed = self.speed;
        state.viz_state.palette = self.palette;
        state.viz_state.interpolation = self.interpolation;
        state.viz_state.render_mode = self.render_mode;
        state.viz_state.processing.enabled = self.processing_enabled;
        state.viz_state.processing.threshold_min = self.threshold_min;
        state.viz_state.processing.threshold_max = self.threshold_max;
        state.viz_state.processing.smoothing = self.smoothing;
        state.viz_state.processing.smoothing_strength = self.smoothing_strength;
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
