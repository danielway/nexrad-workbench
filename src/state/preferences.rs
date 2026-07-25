//! User preferences persisted to localStorage.
//!
//! Covers playback speed, visualization settings, and layer visibility.
//! Loaded on startup, saved automatically when changes are detected.

use serde::{Deserialize, Serialize};

use crate::core::{ElevationSelection, InterpolationMode};

use super::{AppState, PlaybackSpeed};

/// User preferences that persist across page reloads.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UserPreferences {
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

impl UserPreferences {
    const STORAGE_KEY: &'static str = "nexrad_user_preferences";

    /// Snapshot current preferences from application state.
    ///
    /// `mping_api_key` is sourced separately because the mPING state
    /// lives on the diagnostics subsystem, not on `AppState`.
    /// `playback` comes from the Playback subsystem.
    pub fn from_app_state(
        state: &AppState,
        playback: &super::PlaybackState,
        mping_api_key: Option<String>,
    ) -> Self {
        Self {
            speed: playback.speed,
            elevation_auto: state.viz_state.elevation_selection.is_auto(),
            preferred_elevation_angle: state.viz_state.elevation_selection.angle(),
            layer_states: state.layer_state.geo.states,
            layer_counties: state.layer_state.geo.counties,
            layer_labels: state.layer_state.geo.labels,
            layer_nexrad_sites: state.layer_state.geo.nexrad_sites,
            layer_cities: state.layer_state.geo.cities,
            layer_national_mosaic: state.layer_state.geo.national_mosaic,
            layer_alerts_warnings: state.layer_state.geo.alerts_warnings,
            layer_alerts_other: state.layer_state.geo.alerts_other,
            layer_alerts_legacy: None,
            layer_mping: state.layer_state.geo.mping,
            mping_api_key,
            use_local_time: state.use_local_time,
            preferred_site: state.preferred_site.clone(),
            interpolation: state.render_processing.interpolation,
            opacity: state.render_processing.opacity,
            sweep_animation: state.render_processing.sweep_animation,
            data_age_desaturation: state.render_processing.data_age_desaturation,
            mobile_override: state.mobile_override,
            advanced_mode: state.advanced_mode,
            pause_stream_while_reviewing: state.pause_stream_while_reviewing,
            autofetch_while_scrubbing: state.autofetch_while_scrubbing,
        }
    }

    /// Apply loaded preferences to application state. Returns the saved
    /// `mping_api_key` (if any) so the caller can apply it to the
    /// diagnostics subsystem; `AppState` no longer owns mPING state.
    /// `playback` is mutated to carry the persisted speed.
    pub fn apply_to(
        &self,
        state: &mut AppState,
        playback: &mut super::PlaybackState,
    ) -> Option<String> {
        playback.speed = self.speed;
        if self.elevation_auto {
            state.viz_state.elevation_selection = ElevationSelection::Latest;
        } else {
            state.viz_state.elevation_selection = ElevationSelection::Fixed {
                elevation_number: 1,
                angle: self.preferred_elevation_angle,
            };
            // Will be re-resolved when VCP data arrives
        }
        state.layer_state.geo.states = self.layer_states;
        state.layer_state.geo.counties = self.layer_counties;
        state.layer_state.geo.labels = self.layer_labels;
        state.layer_state.geo.nexrad_sites = self.layer_nexrad_sites;
        state.layer_state.geo.cities = self.layer_cities;
        state.layer_state.geo.national_mosaic = self.layer_national_mosaic;
        state.layer_state.geo.alerts_warnings = self.layer_alerts_warnings;
        state.layer_state.geo.alerts_other = self.layer_alerts_other;
        state.layer_state.geo.mping = self.layer_mping;
        state.use_local_time = self.use_local_time;
        state.preferred_site = self.preferred_site.clone();
        state.render_processing.interpolation = self.interpolation;
        state.render_processing.opacity = self.opacity;
        state.render_processing.sweep_animation = self.sweep_animation;
        state.render_processing.data_age_desaturation = self.data_age_desaturation;
        state.mobile_override = self.mobile_override;
        state.advanced_mode = self.advanced_mode;
        state.pause_stream_while_reviewing = self.pause_stream_while_reviewing;
        state.autofetch_while_scrubbing = self.autofetch_while_scrubbing;
        self.mping_api_key.clone()
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

        // Two-phase parse: first as raw JSON to detect missing fields for
        // migration, then to the typed struct. Existing users (stored prefs
        // from before `advanced_mode` existed) are promoted to Advanced so
        // their familiar UI is unchanged.
        let raw: serde_json::Value = match serde_json::from_str(&json) {
            Ok(v) => v,
            Err(e) => {
                log::warn!("Failed to parse user preferences: {}", e);
                return Self::default();
            }
        };
        let had_advanced_mode_field = raw.get("advanced_mode").is_some();
        let had_split_alert_layers =
            raw.get("layer_alerts_warnings").is_some() || raw.get("layer_alerts_other").is_some();

        match serde_json::from_value::<Self>(raw) {
            Ok(mut prefs) => {
                if !had_advanced_mode_field {
                    log::info!(
                        "Migrating existing user preferences to Advanced mode (no advanced_mode field)"
                    );
                    prefs.advanced_mode = true;
                }
                // Migrate the old single `layer_alerts` toggle into the split
                // warnings/other fields: warnings inherit the old on/off state,
                // watches stay off (the new default).
                if !had_split_alert_layers {
                    if let Some(legacy) = prefs.layer_alerts_legacy {
                        prefs.layer_alerts_warnings = legacy;
                        prefs.layer_alerts_other = false;
                    }
                }
                prefs.layer_alerts_legacy = None;
                log::debug!("Loaded user preferences from localStorage");
                prefs
            }
            Err(e) => {
                log::warn!("Failed to deserialize user preferences: {}", e);
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

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn pause_stream_while_reviewing_defaults_off() {
        // New users / fresh defaults: the data-saver policy starts off (spec §7
        // / alignment §5 default).
        assert!(!UserPreferences::default().pause_stream_while_reviewing);
    }

    #[wasm_bindgen_test]
    fn legacy_prefs_without_pause_stream_field_deserialize_to_false() {
        // A stored blob from before the field existed must still parse (serde
        // default), landing the policy off rather than failing the whole load.
        let legacy = r#"{"speed":"Normal","advanced_mode":true}"#;
        let prefs: UserPreferences = serde_json::from_str(legacy).expect("legacy parse");
        assert!(!prefs.pause_stream_while_reviewing);
        // A sibling field still round-trips, confirming the blob really lacked
        // the new field rather than silently defaulting everything.
        assert!(prefs.advanced_mode);
    }

    #[wasm_bindgen_test]
    fn pause_stream_while_reviewing_round_trips() {
        let mut prefs = UserPreferences::default();
        prefs.pause_stream_while_reviewing = true;
        let json = serde_json::to_string(&prefs).expect("serialize");
        let back: UserPreferences = serde_json::from_str(&json).expect("deserialize");
        assert!(back.pause_stream_while_reviewing);
    }

    #[wasm_bindgen_test]
    fn autofetch_while_scrubbing_defaults_on() {
        // The default is ON — automatic fetch is the ~95% path (spec §10).
        assert!(UserPreferences::default().autofetch_while_scrubbing);
    }

    #[wasm_bindgen_test]
    fn legacy_prefs_without_autofetch_field_migrate_to_on() {
        // A stored blob from before the field existed must keep autofetch ON
        // (the serde default), so existing users' navigation still fills the
        // canvas — the field's default IS the migration target.
        let legacy = r#"{"speed":"Normal","advanced_mode":true}"#;
        let prefs: UserPreferences = serde_json::from_str(legacy).expect("legacy parse");
        assert!(prefs.autofetch_while_scrubbing);
    }

    #[wasm_bindgen_test]
    fn autofetch_while_scrubbing_round_trips_when_disabled() {
        let mut prefs = UserPreferences::default();
        prefs.autofetch_while_scrubbing = false;
        let json = serde_json::to_string(&prefs).expect("serialize");
        let back: UserPreferences = serde_json::from_str(&json).expect("deserialize");
        assert!(!back.autofetch_while_scrubbing);
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    // Re-declared builder idioms (sibling `mod tests` helpers are private).
    // `AppState::default()` / `PlaybackState::default()` are the constructors
    // used throughout the state suite (see src/core/persist.rs tests).
    fn states() -> (AppState, super::super::PlaybackState) {
        (AppState::default(), super::super::PlaybackState::default())
    }

    // --- serde default table (`#[serde(default = ...)]`) -------------------

    #[wasm_bindgen_test]
    fn partial_json_fills_serde_defaults() {
        // A blob with only `speed` set must hydrate every other field from its
        // serde default function, NOT panic / fail the load.
        let prefs: UserPreferences =
            serde_json::from_str(r#"{"speed":"Normal"}"#).expect("partial parse");
        // `default = "default_true"` fields:
        assert!(prefs.layer_states);
        assert!(prefs.layer_counties);
        assert!(prefs.layer_labels);
        assert!(prefs.layer_cities);
        assert!(prefs.layer_alerts_warnings);
        assert!(prefs.use_local_time);
        assert!(prefs.data_age_desaturation);
        assert!(prefs.autofetch_while_scrubbing);
        // `#[serde(default)]` bool fields → false:
        assert!(!prefs.elevation_auto);
        assert!(!prefs.layer_nexrad_sites);
        assert!(!prefs.layer_national_mosaic);
        assert!(!prefs.layer_alerts_other);
        assert!(!prefs.layer_mping);
        assert!(!prefs.sweep_animation);
        assert!(!prefs.advanced_mode);
        assert!(!prefs.pause_stream_while_reviewing);
        // Numeric defaults from explicit default fns:
        assert!((prefs.preferred_elevation_angle - 0.5).abs() < 1e-6);
        assert!((prefs.opacity - 1.0).abs() < 1e-6);
        // Option defaults → None:
        assert!(prefs.mping_api_key.is_none());
        assert!(prefs.preferred_site.is_none());
        assert!(prefs.mobile_override.is_none());
        assert!(prefs.layer_alerts_legacy.is_none());
    }

    #[wasm_bindgen_test]
    fn default_matches_serde_default_construction() {
        // The hand-written `Default` impl must agree field-for-field with what
        // serde produces from an empty object (every field defaulted).
        let from_empty: UserPreferences = serde_json::from_str("{}").expect("empty parse");
        assert!(from_empty == UserPreferences::default());
    }

    // --- legacy `layer_alerts` rename capture (serde-pure half) -----------

    #[wasm_bindgen_test]
    fn legacy_layer_alerts_field_captured_into_option() {
        // The `#[serde(rename = "layer_alerts")]` mapping pulls the old single
        // toggle into `layer_alerts_legacy` as `Some(_)`. (The promotion into
        // the split fields happens in `load()` and is not exercised here.)
        let prefs: UserPreferences =
            serde_json::from_str(r#"{"layer_alerts":false}"#).expect("legacy alerts parse");
        assert!(prefs.layer_alerts_legacy == Some(false));
        let prefs_on: UserPreferences =
            serde_json::from_str(r#"{"layer_alerts":true}"#).expect("legacy alerts on parse");
        assert!(prefs_on.layer_alerts_legacy == Some(true));
    }

    #[wasm_bindgen_test]
    fn legacy_field_never_serialized_back_as_some_default() {
        // Default has `layer_alerts_legacy: None`; serializing must NOT bring
        // the legacy key back as a real value the next load would re-capture.
        let json = serde_json::to_string(&UserPreferences::default()).expect("serialize");
        let back: UserPreferences = serde_json::from_str(&json).expect("deserialize");
        assert!(back.layer_alerts_legacy.is_none());
    }

    // --- full round-trip of the serde struct ------------------------------

    #[wasm_bindgen_test]
    fn full_struct_round_trips_through_json() {
        let mut prefs = UserPreferences::default();
        prefs.speed = PlaybackSpeed::Double;
        prefs.elevation_auto = true;
        prefs.preferred_elevation_angle = 3.25;
        prefs.layer_states = false;
        prefs.layer_nexrad_sites = true;
        prefs.layer_alerts_other = true;
        prefs.mping_api_key = Some("abc-123".to_string());
        prefs.preferred_site = Some("KDMX".to_string());
        prefs.interpolation = InterpolationMode::Bilinear;
        prefs.opacity = 0.42;
        prefs.mobile_override = Some(true);
        prefs.advanced_mode = true;
        prefs.use_local_time = false;
        let json = serde_json::to_string(&prefs).expect("serialize");
        let back: UserPreferences = serde_json::from_str(&json).expect("deserialize");
        assert!(back == prefs);
    }

    // --- from_app_state snapshot mapping ----------------------------------

    #[wasm_bindgen_test]
    fn from_app_state_default_reads_fixed_elevation() {
        // Default VizState elevation is Fixed{1, 0.5} → not auto, angle 0.5.
        let (state, playback) = states();
        let prefs = UserPreferences::from_app_state(&state, &playback, None);
        assert!(!prefs.elevation_auto);
        assert!((prefs.preferred_elevation_angle - 0.5).abs() < 1e-6);
        // Default layer mirror.
        assert!(prefs.layer_states);
        assert!(prefs.layer_cities);
        assert!(!prefs.layer_nexrad_sites);
        // mping key is sourced separately, passed through verbatim.
        assert!(prefs.mping_api_key.is_none());
    }

    #[wasm_bindgen_test]
    fn from_app_state_passes_mping_key_through() {
        let (state, playback) = states();
        let prefs = UserPreferences::from_app_state(&state, &playback, Some("KEY".to_string()));
        assert!(prefs.mping_api_key == Some("KEY".to_string()));
    }

    #[wasm_bindgen_test]
    fn from_app_state_reflects_auto_elevation() {
        let (mut state, playback) = states();
        state.viz_state.elevation_selection = ElevationSelection::Latest;
        let prefs = UserPreferences::from_app_state(&state, &playback, None);
        assert!(prefs.elevation_auto);
        // Latest reports angle() == 0.5 by convention.
        assert!((prefs.preferred_elevation_angle - 0.5).abs() < 1e-6);
    }

    #[wasm_bindgen_test]
    fn from_app_state_captures_fixed_angle() {
        let (mut state, playback) = states();
        state.viz_state.elevation_selection = ElevationSelection::Fixed {
            elevation_number: 4,
            angle: 2.4,
        };
        let prefs = UserPreferences::from_app_state(&state, &playback, None);
        assert!(!prefs.elevation_auto);
        assert!((prefs.preferred_elevation_angle - 2.4).abs() < 1e-6);
    }

    // --- apply_to: elevation branch behavior ------------------------------

    #[wasm_bindgen_test]
    fn apply_to_auto_sets_latest_and_returns_key() {
        let (mut state, mut playback) = states();
        let mut prefs = UserPreferences::default();
        prefs.elevation_auto = true;
        prefs.mping_api_key = Some("mk".to_string());
        let returned = prefs.apply_to(&mut state, &mut playback);
        assert!(state.viz_state.elevation_selection == ElevationSelection::Latest);
        // apply_to returns the persisted mping key for the diagnostics subsystem.
        assert!(returned == Some("mk".to_string()));
    }

    #[wasm_bindgen_test]
    fn apply_to_fixed_resets_number_to_one_keeps_angle() {
        // Non-auto applies as Fixed{elevation_number:1, angle}: number is reset
        // to 1 (re-resolved later from VCP), the angle is preserved verbatim.
        let (mut state, mut playback) = states();
        let mut prefs = UserPreferences::default();
        prefs.elevation_auto = false;
        prefs.preferred_elevation_angle = 1.75;
        prefs.apply_to(&mut state, &mut playback);
        match state.viz_state.elevation_selection {
            ElevationSelection::Fixed {
                elevation_number,
                angle,
            } => {
                assert_eq!(elevation_number, 1);
                assert!((angle - 1.75).abs() < 1e-6);
            }
            ElevationSelection::Latest => panic!("expected Fixed selection"),
        }
    }

    // --- from_app_state -> apply_to persistence contract ------------------

    #[wasm_bindgen_test]
    fn snapshot_then_apply_round_trips_observable_fields() {
        // Mutate a spread of preference-backed AppState/Playback fields, snapshot
        // them, apply onto a fresh state, and confirm the snapshot survives.
        let (mut src, mut src_pb) = states();
        src_pb.speed = PlaybackSpeed::Quadruple;
        src.viz_state.elevation_selection = ElevationSelection::Fixed {
            elevation_number: 7,
            angle: 3.5,
        };
        src.layer_state.geo.states = false;
        src.layer_state.geo.nexrad_sites = true;
        src.layer_state.geo.national_mosaic = true;
        src.layer_state.geo.alerts_other = true;
        src.layer_state.geo.mping = true;
        src.use_local_time = false;
        src.preferred_site = Some("KTLX".to_string());
        src.render_processing.interpolation = InterpolationMode::Bilinear;
        src.render_processing.opacity = 0.3;
        src.render_processing.sweep_animation = true;
        src.render_processing.data_age_desaturation = false;
        src.mobile_override = Some(false);
        src.advanced_mode = true;
        src.pause_stream_while_reviewing = true;
        src.autofetch_while_scrubbing = false;

        let prefs = UserPreferences::from_app_state(&src, &src_pb, None);

        let (mut dst, mut dst_pb) = states();
        prefs.apply_to(&mut dst, &mut dst_pb);

        assert!(dst_pb.speed == PlaybackSpeed::Quadruple);
        // Fixed angle preserved; elevation_number resets to 1 by contract.
        match dst.viz_state.elevation_selection {
            ElevationSelection::Fixed {
                elevation_number,
                angle,
            } => {
                assert_eq!(elevation_number, 1);
                assert!((angle - 3.5).abs() < 1e-6);
            }
            ElevationSelection::Latest => panic!("expected Fixed selection"),
        }
        assert!(!dst.layer_state.geo.states);
        assert!(dst.layer_state.geo.nexrad_sites);
        assert!(dst.layer_state.geo.national_mosaic);
        assert!(dst.layer_state.geo.alerts_other);
        assert!(dst.layer_state.geo.mping);
        assert!(!dst.use_local_time);
        assert!(dst.preferred_site == Some("KTLX".to_string()));
        assert!(dst.render_processing.interpolation == InterpolationMode::Bilinear);
        assert!((dst.render_processing.opacity - 0.3).abs() < 1e-6);
        assert!(dst.render_processing.sweep_animation);
        assert!(!dst.render_processing.data_age_desaturation);
        assert!(dst.mobile_override == Some(false));
        assert!(dst.advanced_mode);
        assert!(dst.pause_stream_while_reviewing);
        assert!(!dst.autofetch_while_scrubbing);
    }
}
