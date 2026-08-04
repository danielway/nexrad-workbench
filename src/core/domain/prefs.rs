//! User preference vocabulary — the persisted settings struct.
//!
//! Covers playback speed, visualization settings, and layer visibility.
//! The localStorage load/save and the `AppState` snapshot/apply impls live
//! in the shell layer (`src/state/preferences.rs`).

use serde::{Deserialize, Serialize};

use crate::core::{InterpolationMode, PlaybackSpeed};

/// User preferences that persist across page reloads.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct UserPreferences {
    #[serde(default)]
    pub speed: PlaybackSpeed,
    #[serde(default)]
    pub elevation_auto: bool,
    #[serde(default = "default_elevation_angle")]
    pub preferred_elevation_angle: f32,
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
    pub layer_national_mosaic: bool,
    #[serde(default = "default_true")]
    pub layer_alerts_warnings: bool,
    #[serde(default)]
    pub layer_alerts_other: bool,
    /// Legacy single alerts toggle, kept only to migrate pre-split preferences
    /// into the two fields above. Never written going forward.
    #[serde(default, rename = "layer_alerts")]
    pub layer_alerts_legacy: Option<bool>,
    #[serde(default)]
    pub layer_mping: bool,
    /// User-supplied mPING API key. Persisted in localStorage so the
    /// integration survives reloads. Empty/None disables the layer.
    #[serde(default)]
    pub mping_api_key: Option<String>,
    // Local time is the primary display (spec §11.4); UTC is one tap away.
    // `default_true` flips new stores and any predating this field to local.
    #[serde(default = "default_true")]
    pub use_local_time: bool,
    /// Preferred NEXRAD site from first-visit selection. When `Some`, the
    /// first-visit modal is skipped and this site is used as the default.
    #[serde(default)]
    pub preferred_site: Option<String>,

    // Rendering options
    #[serde(default)]
    pub interpolation: InterpolationMode,
    #[serde(default = "default_opacity")]
    pub opacity: f32,
    #[serde(default)]
    pub sweep_animation: bool,
    #[serde(default = "default_true")]
    pub data_age_desaturation: bool,

    /// Mobile UI override: `None` = auto, `Some(true)` = force mobile,
    /// `Some(false)` = force desktop.
    #[serde(default)]
    pub mobile_override: Option<bool>,

    /// Whether advanced controls are visible. `false` = Basic (default for
    /// new users), `true` = Advanced. Existing users with stored preferences
    /// from before this feature are migrated to `true` on first load —
    /// see [`UserPreferences::load`].
    #[serde(default)]
    pub advanced_mode: bool,

    /// Data-saver policy (spec §7 / alignment §5): when `true`, detaching the
    /// playhead (scrubbing away to review) stops the background live stream
    /// immediately rather than letting it keep filling the cache. Default off;
    /// the settings UI lands next phase, but the behavior is wired now.
    #[serde(default)]
    pub pause_stream_while_reviewing: bool,

    /// Acquisition policy (spec §10 / alignment §5 queue-sheet toggles): when
    /// `true` (default), the playhead-driven reactive prefetch + anchor
    /// fast-path run as the user scrubs/seeks, so the canvas fills
    /// automatically. When `false` (data-saver), that automatic fetch is
    /// suppressed — explicit range-selection fetches and the inspector's
    /// tap-to-fetch still work, because the user asked for those. Migrates to
    /// `true` for existing stored preferences (see [`UserPreferences::load`]).
    #[serde(default = "default_true")]
    pub autofetch_while_scrubbing: bool,
}

fn default_true() -> bool {
    true
}

fn default_elevation_angle() -> f32 {
    0.5
}

fn default_opacity() -> f32 {
    1.0
}

impl Default for UserPreferences {
    fn default() -> Self {
        Self {
            speed: PlaybackSpeed::default(),
            elevation_auto: false,
            preferred_elevation_angle: 0.5,
            layer_states: true,
            layer_counties: true,
            layer_labels: true,
            layer_nexrad_sites: false,
            layer_cities: true,
            layer_national_mosaic: false,
            layer_alerts_warnings: true,
            layer_alerts_other: false,
            layer_alerts_legacy: None,
            layer_mping: false,
            mping_api_key: None,
            use_local_time: true,
            preferred_site: None,
            interpolation: InterpolationMode::default(),
            opacity: 1.0,
            sweep_animation: false,
            data_age_desaturation: true,
            mobile_override: None,
            advanced_mode: false,
            pause_stream_while_reviewing: false,
            autofetch_while_scrubbing: true,
        }
    }
}
