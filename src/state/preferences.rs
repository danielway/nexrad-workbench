//! User preferences persisted to localStorage.
//!
//! Covers playback speed, visualization settings, and layer visibility.
//! Loaded on startup, saved automatically when changes are detected.

use serde::{Deserialize, Serialize};

use super::{AppState, InterpolationMode, PlaybackSpeed, RenderMode};

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
    #[serde(default = "default_true")]
    pub layer_cities: bool,
    #[serde(default)]
    pub use_local_time: bool,
    /// Preferred NEXRAD site from first-visit selection. When `Some`, the
    /// first-visit modal is skipped and this site is used as the default.
    #[serde(default)]
    pub preferred_site: Option<String>,

    // Rendering options
    #[serde(default)]
    pub interpolation: InterpolationMode,
    #[serde(default)]
    pub smoothing_enabled: bool,
    #[serde(default = "default_smoothing_radius")]
    pub smoothing_radius: f32,
    #[serde(default)]
    pub despeckle_enabled: bool,
    #[serde(default = "default_despeckle_threshold")]
    pub despeckle_threshold: u32,
    #[serde(default = "default_opacity")]
    pub opacity: f32,
    #[serde(default)]
    pub edge_softening: bool,
    #[serde(default)]
    pub sweep_animation: bool,
    #[serde(default = "default_true")]
    pub data_age_indicator: bool,
}

fn default_true() -> bool {
    true
}

fn default_smoothing_radius() -> f32 {
    2.0
}

fn default_despeckle_threshold() -> u32 {
    3
}

fn default_opacity() -> f32 {
    1.0
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
            layer_cities: true,
            use_local_time: false,
            preferred_site: None,
            interpolation: InterpolationMode::default(),
            smoothing_enabled: false,
            smoothing_radius: 2.0,
            despeckle_enabled: false,
            despeckle_threshold: 3,
            opacity: 1.0,
            edge_softening: false,
            sweep_animation: false,
            data_age_indicator: true,
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
            layer_cities: state.layer_state.geo.cities,
            use_local_time: state.use_local_time,
            preferred_site: state.preferred_site.clone(),
            interpolation: state.render_processing.interpolation,
            smoothing_enabled: state.render_processing.smoothing_enabled,
            smoothing_radius: state.render_processing.smoothing_radius,
            despeckle_enabled: state.render_processing.despeckle_enabled,
            despeckle_threshold: state.render_processing.despeckle_threshold,
            opacity: state.render_processing.opacity,
            edge_softening: state.render_processing.edge_softening,
            sweep_animation: state.render_processing.sweep_animation,
            data_age_indicator: state.render_processing.data_age_indicator,
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
        state.layer_state.geo.cities = self.layer_cities;
        state.use_local_time = self.use_local_time;
        state.preferred_site = self.preferred_site.clone();
        state.render_processing.interpolation = self.interpolation;
        state.render_processing.smoothing_enabled = self.smoothing_enabled;
        state.render_processing.smoothing_radius = self.smoothing_radius;
        state.render_processing.despeckle_enabled = self.despeckle_enabled;
        state.render_processing.despeckle_threshold = self.despeckle_threshold;
        state.render_processing.opacity = self.opacity;
        state.render_processing.edge_softening = self.edge_softening;
        state.render_processing.sweep_animation = self.sweep_animation;
        state.render_processing.data_age_indicator = self.data_age_indicator;
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
