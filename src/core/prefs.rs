//! Preferences: how you like it (one-instrument §14).
//!
//! The other half of the rule in [`crate::core::link`]: the link is what you
//! see; preferences are how you like it. [`Preferences`] is the versioned
//! struct saved to localStorage under [`STORAGE_KEY`]; [`parse_stored`] reads
//! any stored blob, migrating older versions; [`reset_plan`] decides what
//! Settings › Reset clears and keeps. All pure: the shell
//! (`state::preferences`, `app::command_dispatch`) touches localStorage and
//! `AppState`.
//!
//! | Preference | Field |
//! |---|---|
//! | layers | `layer_*` |
//! | speed per mode | `speed_frames`, `speed_rays` |
//! | time zone | `use_local_time` |
//! | data-saving policies | `autofetch_while_scrubbing`, `pause_stream_while_reviewing` |
//! | loop style | `loop_style` |
//! | panel tab and collapse | `panel_tab`, `panel_open` |
//! | tools | `tools` |
//! | developer mode | `developer_mode` |
//! | mPING key | `mping_api_key` |
//! | preferred site | `preferred_site` |
//! | recent sites | `recent_sites` |
//! | rendering (interpolation, opacity, age desaturation) | `interpolation`, `opacity`, `data_age_desaturation` |
//! | storage quota | its own key, `nexrad_storage_settings` |
//!
//! Version 1 (no `version` field) is the pre-split blob. Its `speed` is
//! filed under the mode it was saved with; its `play_mode` and tilt
//! (`elevation_auto`, `preferred_elevation_angle`) moved to the link and are
//! dropped; the retired Basic/Advanced `advanced_mode` is read and discarded,
//! so every control is visible; the single `layer_alerts` toggle becomes the
//! warnings layer.

use serde::{Deserialize, Serialize};

use crate::core::panels::RailTab;
use crate::core::{InterpolationMode, LoopMode, PlaybackMode, PlaybackSpeed};

/// The localStorage key of the preferences blob.
pub(crate) const STORAGE_KEY: &str = "nexrad_user_preferences";

/// The localStorage key of the storage quota (a preference kept under its
/// own key).
pub(crate) const STORAGE_SETTINGS_KEY: &str = "nexrad_storage_settings";

/// The version [`Preferences`] are written at.
pub(crate) const VERSION: u32 = 2;

/// The map toolbar's tools.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct ToolPrefs {
    #[serde(default)]
    pub probe: bool,
    #[serde(default)]
    pub distance: bool,
    #[serde(default)]
    pub storm_cells: bool,
    #[serde(default = "default_storm_cell_dbz")]
    pub storm_cell_dbz: f32,
}

impl Default for ToolPrefs {
    fn default() -> Self {
        Self {
            probe: false,
            distance: false,
            storm_cells: false,
            storm_cell_dbz: default_storm_cell_dbz(),
        }
    }
}

/// How the user likes the instrument. Persisted across sessions.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Preferences {
    /// [`VERSION`] when written; absent (0) in version 1 blobs.
    #[serde(default)]
    pub version: u32,

    // Playback
    #[serde(default = "default_speed_frames")]
    pub speed_frames: PlaybackSpeed,
    #[serde(default = "default_speed_rays")]
    pub speed_rays: PlaybackSpeed,
    #[serde(default)]
    pub loop_style: LoopMode,

    // Layers
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
    #[serde(default)]
    pub layer_mping: bool,

    // Rendering
    #[serde(default)]
    pub interpolation: InterpolationMode,
    #[serde(default = "default_opacity")]
    pub opacity: f32,
    #[serde(default = "default_true")]
    pub data_age_desaturation: bool,

    /// Local time is the primary display; UTC is one tap away.
    #[serde(default = "default_true")]
    pub use_local_time: bool,

    // Data saving
    /// Stop the live stream when the playhead detaches.
    #[serde(default)]
    pub pause_stream_while_reviewing: bool,
    /// Fetch scans automatically as the user scrubs and seeks.
    #[serde(default = "default_true")]
    pub autofetch_while_scrubbing: bool,

    // Panel
    #[serde(default)]
    pub panel_tab: RailTab,
    #[serde(default = "default_true")]
    pub panel_open: bool,

    #[serde(default)]
    pub tools: ToolPrefs,

    /// Settings › Developer. A link's `dev=` wins for its session without
    /// being saved here.
    #[serde(default)]
    pub developer_mode: bool,

    /// User-supplied mPING API key. Empty/None disables the layer.
    #[serde(default)]
    pub mping_api_key: Option<String>,
    /// The site chosen on first visit; opening with no link opens here.
    #[serde(default)]
    pub preferred_site: Option<String>,

    /// Recently picked sites, newest first (the site picker's Recent group).
    #[serde(default)]
    pub recent_sites: Vec<String>,

    /// Mobile UI override: `None` = auto, `Some(true)` = force mobile,
    /// `Some(false)` = force desktop. No UI.
    #[serde(default)]
    pub mobile_override: Option<bool>,
}

fn default_true() -> bool {
    true
}

fn default_opacity() -> f32 {
    1.0
}

fn default_storm_cell_dbz() -> f32 {
    35.0
}

fn default_speed_frames() -> PlaybackSpeed {
    PlaybackSpeed::default()
}

fn default_speed_rays() -> PlaybackSpeed {
    PlaybackSpeed::default()
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            version: VERSION,
            speed_frames: default_speed_frames(),
            speed_rays: default_speed_rays(),
            loop_style: LoopMode::default(),
            layer_states: true,
            layer_counties: true,
            layer_labels: true,
            layer_nexrad_sites: false,
            layer_cities: true,
            layer_national_mosaic: false,
            layer_alerts_warnings: true,
            layer_alerts_other: false,
            layer_mping: false,
            interpolation: InterpolationMode::default(),
            opacity: 1.0,
            data_age_desaturation: true,
            use_local_time: true,
            pause_stream_while_reviewing: false,
            autofetch_while_scrubbing: true,
            panel_tab: RailTab::default(),
            panel_open: true,
            tools: ToolPrefs::default(),
            developer_mode: false,
            mping_api_key: None,
            preferred_site: None,
            recent_sites: Vec::new(),
            mobile_override: None,
        }
    }
}

impl Preferences {
    /// The saved speed for a mode.
    pub(crate) fn speed_for(&self, mode: PlaybackMode) -> PlaybackSpeed {
        match mode {
            PlaybackMode::Macro => self.speed_frames,
            PlaybackMode::Micro => self.speed_rays,
        }
    }

    /// Record the speed the user chose in `mode`, keeping the other mode's.
    /// Pass the *chosen* speed ([`crate::core::PlaybackState::chosen_speed`]),
    /// so going live in Rays, which locks the speed to real time, never
    /// overwrites the saved one.
    pub(crate) fn remember_speed(&mut self, mode: PlaybackMode, chosen: PlaybackSpeed) {
        match mode {
            PlaybackMode::Macro if chosen.macro_frames_per_second().is_some() => {
                self.speed_frames = chosen;
            }
            PlaybackMode::Macro => {}
            PlaybackMode::Micro => self.speed_rays = chosen,
        }
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Reading a stored blob, with migration
// ───────────────────────────────────────────────────────────────────────────

/// The version 1 fields that do not survive into version 2 as they are.
#[derive(Deserialize, Default)]
struct V1Extras {
    #[serde(default)]
    speed: Option<PlaybackSpeed>,
    #[serde(default)]
    play_mode: Option<PlaybackMode>,
    #[serde(default)]
    layer_alerts: Option<bool>,
}

/// What reading the stored blob found, for the shell's log line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Loaded {
    /// Nothing stored, or nothing readable: defaults.
    Fresh,
    /// A current blob.
    Current,
    /// An older blob, migrated (and saved back at the next persist).
    Migrated { from: u32 },
}

/// Read a stored preferences blob. `None` (nothing stored) and unreadable
/// JSON both give defaults; a version 1 blob is migrated (see the module
/// docs). A blob from a newer version is read for the fields this version
/// knows.
pub(crate) fn parse_stored(json: Option<&str>) -> (Preferences, Loaded) {
    let Some(json) = json else {
        return (Preferences::default(), Loaded::Fresh);
    };
    let Ok(raw) = serde_json::from_str::<serde_json::Value>(json) else {
        return (Preferences::default(), Loaded::Fresh);
    };
    let Ok(mut prefs) = serde_json::from_value::<Preferences>(raw.clone()) else {
        return (Preferences::default(), Loaded::Fresh);
    };
    if prefs.version >= VERSION {
        return (prefs, Loaded::Current);
    }

    let from = prefs.version.max(1);
    let v1: V1Extras = serde_json::from_value(raw.clone()).unwrap_or_default();
    // The speed was one field; file it under the mode it was saved in.
    if let Some(speed) = v1.speed {
        prefs.remember_speed(v1.play_mode.unwrap_or_default(), speed);
    }
    // The single alerts toggle predates the warnings / other split.
    let had_split =
        raw.get("layer_alerts_warnings").is_some() || raw.get("layer_alerts_other").is_some();
    if let (false, Some(legacy)) = (had_split, v1.layer_alerts) {
        prefs.layer_alerts_warnings = legacy;
        prefs.layer_alerts_other = false;
    }
    // `advanced_mode`, `play_mode`, `elevation_auto` and
    // `preferred_elevation_angle` are not fields any more: serde has already
    // read past them, and they are not written back.
    prefs.version = VERSION;
    (prefs, Loaded::Migrated { from })
}

// ───────────────────────────────────────────────────────────────────────────
// Reset
// ───────────────────────────────────────────────────────────────────────────

/// What Settings › Reset does: the lists the confirmation shows, the
/// localStorage keys to remove, and the preferences to write back.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ResetPlan {
    /// What the reset clears, in the user's words.
    pub clears: Vec<&'static str>,
    /// What it keeps, in the user's words.
    pub keeps: Vec<&'static str>,
    /// localStorage keys to remove.
    pub remove_keys: Vec<&'static str>,
    /// Preferences to write after removing (the kept key and site), or
    /// `None` to leave none.
    pub write: Option<Preferences>,
}

/// Decide a reset. It clears every preference and the storage quota, keeps
/// saved events, cached radar data and the learned live-stream timing, and
/// keeps the mPING key and the preferred site unless `forget_key_and_site`.
/// The shell then reloads with a clean URL, so the link cannot re-apply
/// what was just cleared.
pub(crate) fn reset_plan(current: &Preferences, forget_key_and_site: bool) -> ResetPlan {
    let mut clears = vec![
        "Layers, rendering and tools",
        "Playback speeds and loop style",
        "Time zone and data saving",
        "Panel layout",
        "Recent sites",
        "Developer mode",
        "Storage quota",
    ];
    let mut keeps = vec![
        "Saved events",
        "Cached radar data",
        "Learned live-stream timing",
    ];
    let kept_key = current.mping_api_key.is_some();
    let kept_site = current.preferred_site.is_some();
    if forget_key_and_site {
        if kept_key {
            clears.push("mPING API key");
        }
        if kept_site {
            clears.push("Preferred site");
        }
    } else {
        if kept_key {
            keeps.push("mPING API key");
        }
        if kept_site {
            keeps.push("Preferred site");
        }
    }
    let write = (!forget_key_and_site && (kept_key || kept_site)).then(|| Preferences {
        mping_api_key: current.mping_api_key.clone(),
        preferred_site: current.preferred_site.clone(),
        ..Preferences::default()
    });
    ResetPlan {
        clears,
        keeps,
        remove_keys: vec![STORAGE_KEY, STORAGE_SETTINGS_KEY],
        write,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{FreezeAt, PlaybackState};
    use wasm_bindgen_test::wasm_bindgen_test;

    // ── Shape ──────────────────────────────────────────────────────────────

    #[wasm_bindgen_test]
    fn default_is_current_and_matches_serde_defaults() {
        assert_eq!(Preferences::default().version, VERSION);
        let (from_empty, loaded) = parse_stored(Some(r#"{"version":2}"#));
        assert_eq!(loaded, Loaded::Current);
        assert_eq!(from_empty, Preferences::default());
    }

    #[wasm_bindgen_test]
    fn round_trips_every_field() {
        let prefs = Preferences {
            speed_frames: PlaybackSpeed::Double,
            speed_rays: PlaybackSpeed::FifteenToOne,
            loop_style: LoopMode::PingPong,
            layer_states: false,
            layer_nexrad_sites: true,
            layer_mping: true,
            interpolation: InterpolationMode::Bilinear,
            opacity: 0.4,
            use_local_time: false,
            pause_stream_while_reviewing: true,
            autofetch_while_scrubbing: false,
            panel_tab: RailTab::Layers,
            panel_open: false,
            tools: ToolPrefs {
                probe: true,
                distance: false,
                storm_cells: true,
                storm_cell_dbz: 45.0,
            },
            developer_mode: true,
            mping_api_key: Some("abc".into()),
            preferred_site: Some("KTLX".into()),
            recent_sites: vec!["KTLX".into(), "KDMX".into()],
            mobile_override: Some(true),
            ..Preferences::default()
        };
        let json = serde_json::to_string(&prefs).unwrap();
        assert_eq!(parse_stored(Some(&json)), (prefs, Loaded::Current));
    }

    #[wasm_bindgen_test]
    fn nothing_or_garbage_gives_defaults() {
        assert_eq!(parse_stored(None), (Preferences::default(), Loaded::Fresh));
        assert_eq!(
            parse_stored(Some("not json")),
            (Preferences::default(), Loaded::Fresh)
        );
        assert_eq!(
            parse_stored(Some(r#"{"opacity":"very"}"#)),
            (Preferences::default(), Loaded::Fresh)
        );
    }

    // ── Migration from version 1 ───────────────────────────────────────────

    /// A version 1 blob as the app wrote it before the split.
    const V1: &str = r#"{
        "speed":"Double","play_mode":"Macro","elevation_auto":false,
        "preferred_elevation_angle":2.4,"layer_states":false,"layer_counties":true,
        "layer_labels":true,"layer_nexrad_sites":true,"layer_cities":true,
        "layer_national_mosaic":false,"layer_alerts_warnings":true,
        "layer_alerts_other":true,"layer_mping":false,"mping_api_key":"KEY",
        "use_local_time":false,"preferred_site":"KDMX","interpolation":"Nearest",
        "opacity":0.8,"data_age_desaturation":true,"mobile_override":null,
        "advanced_mode":false,"pause_stream_while_reviewing":true,
        "autofetch_while_scrubbing":false
    }"#;

    #[wasm_bindgen_test]
    fn v1_keeps_every_preference_it_had() {
        let (p, loaded) = parse_stored(Some(V1));
        assert_eq!(loaded, Loaded::Migrated { from: 1 });
        assert_eq!(p.version, VERSION);
        assert!(!p.layer_states);
        assert!(p.layer_nexrad_sites);
        assert!(p.layer_alerts_other);
        assert_eq!(p.mping_api_key.as_deref(), Some("KEY"));
        assert!(!p.use_local_time);
        assert_eq!(p.preferred_site.as_deref(), Some("KDMX"));
        assert!((p.opacity - 0.8).abs() < 1e-6);
        assert!(p.pause_stream_while_reviewing);
        assert!(!p.autofetch_while_scrubbing);
    }

    #[wasm_bindgen_test]
    fn v1_basic_mode_is_discarded_and_leaves_everything_visible() {
        // A Basic user (advanced_mode false) and an Advanced one migrate to
        // the same preferences: there is no mode left to hide anything.
        let basic = parse_stored(Some(V1)).0;
        let advanced = parse_stored(Some(
            &V1.replace(r#""advanced_mode":false"#, r#""advanced_mode":true"#),
        ))
        .0;
        assert_eq!(basic, advanced);
        // And the discarded value is not written back.
        let json = serde_json::to_string(&basic).unwrap();
        assert!(!json.contains("advanced_mode"), "{json}");
        // Nothing hidden keeps running: tools start off.
        assert_eq!(basic.tools, ToolPrefs::default());
    }

    #[wasm_bindgen_test]
    fn v1_speed_is_filed_under_its_mode() {
        let frames = parse_stored(Some(V1)).0;
        assert_eq!(frames.speed_frames, PlaybackSpeed::Double);
        assert_eq!(frames.speed_rays, default_speed_rays());

        let rays = parse_stored(Some(r#"{"speed":"FifteenToOne","play_mode":"Micro"}"#)).0;
        assert_eq!(rays.speed_rays, PlaybackSpeed::FifteenToOne);
        assert_eq!(rays.speed_frames, default_speed_frames());

        // A Rays-only multiple saved without a mode cannot be a frame rate.
        let odd = parse_stored(Some(r#"{"speed":"Realtime"}"#)).0;
        assert_eq!(odd.speed_frames, default_speed_frames());
    }

    #[wasm_bindgen_test]
    fn v1_tilt_and_play_mode_moved_to_the_link() {
        let json = serde_json::to_string(&parse_stored(Some(V1)).0).unwrap();
        for gone in [
            "play_mode",
            "elevation_auto",
            "preferred_elevation_angle",
            "\"speed\"",
        ] {
            assert!(!json.contains(gone), "{gone} in {json}");
        }
    }

    #[wasm_bindgen_test]
    fn v1_single_alerts_toggle_becomes_warnings() {
        let (p, _) = parse_stored(Some(r#"{"layer_alerts":false}"#));
        assert!(!p.layer_alerts_warnings);
        assert!(!p.layer_alerts_other);
        let (p, _) = parse_stored(Some(r#"{"layer_alerts":true}"#));
        assert!(p.layer_alerts_warnings);
        // The split fields win over the legacy one when both exist.
        let (p, _) = parse_stored(Some(
            r#"{"layer_alerts":false,"layer_alerts_warnings":true}"#,
        ));
        assert!(p.layer_alerts_warnings);
    }

    #[wasm_bindgen_test]
    fn v1_new_preferences_take_their_defaults() {
        let p = parse_stored(Some(V1)).0;
        assert_eq!(p.loop_style, LoopMode::Loop);
        assert_eq!(p.panel_tab, RailTab::View);
        assert!(p.panel_open);
        assert!(!p.developer_mode);
    }

    #[wasm_bindgen_test]
    fn v1_empty_object_migrates_to_defaults() {
        let (p, loaded) = parse_stored(Some("{}"));
        assert_eq!(loaded, Loaded::Migrated { from: 1 });
        assert_eq!(p, Preferences::default());
    }

    // ── Speed per mode and the live lock ───────────────────────────────────

    #[wasm_bindgen_test]
    fn remember_speed_keeps_the_other_mode() {
        let mut p = Preferences::default();
        p.remember_speed(PlaybackMode::Micro, PlaybackSpeed::Quadruple);
        p.remember_speed(PlaybackMode::Macro, PlaybackSpeed::Half);
        assert_eq!(p.speed_for(PlaybackMode::Micro), PlaybackSpeed::Quadruple);
        assert_eq!(p.speed_for(PlaybackMode::Macro), PlaybackSpeed::Half);
    }

    #[wasm_bindgen_test]
    fn going_live_in_rays_locks_without_overwriting_the_saved_speed() {
        let mut ps = PlaybackState::default();
        ps.restore_play_mode(PlaybackMode::Micro);
        ps.speed = PlaybackSpeed::Quadruple;
        ps.enter_pinned_live(1_000.0);
        // Locked to real time on screen...
        assert_eq!(ps.speed, PlaybackSpeed::Realtime);
        // ...but the chosen speed is what the preferences save.
        let mut p = Preferences::default();
        p.remember_speed(ps.play_mode, ps.chosen_speed());
        assert_eq!(p.speed_rays, PlaybackSpeed::Quadruple);
        // Leaving live gives the chosen speed back.
        ps.exit_live(FreezeAt::Keep);
        assert_eq!(ps.speed, PlaybackSpeed::Quadruple);
        assert_eq!(ps.held_speed, None);
    }

    #[wasm_bindgen_test]
    fn going_live_in_frames_holds_nothing() {
        let mut ps = PlaybackState::default();
        ps.speed = PlaybackSpeed::Double;
        ps.enter_pinned_live(1_000.0);
        assert_eq!(ps.held_speed, None);
        assert_eq!(ps.chosen_speed(), PlaybackSpeed::Double);
    }

    // ── Reset ──────────────────────────────────────────────────────────────

    fn with_key_and_site() -> Preferences {
        Preferences {
            mping_api_key: Some("KEY".into()),
            preferred_site: Some("KTLX".into()),
            developer_mode: true,
            layer_states: false,
            ..Preferences::default()
        }
    }

    #[wasm_bindgen_test]
    fn reset_keeps_the_key_and_site_unless_asked() {
        let plan = reset_plan(&with_key_and_site(), false);
        assert!(plan.keeps.contains(&"mPING API key"));
        assert!(plan.keeps.contains(&"Preferred site"));
        let write = plan.write.expect("kept preferences are written back");
        assert_eq!(write.mping_api_key.as_deref(), Some("KEY"));
        assert_eq!(write.preferred_site.as_deref(), Some("KTLX"));
        // Everything else is back to its default.
        assert!(!write.developer_mode);
        assert!(write.layer_states);
    }

    #[wasm_bindgen_test]
    fn reset_can_forget_the_key_and_site() {
        let plan = reset_plan(&with_key_and_site(), true);
        assert!(plan.clears.contains(&"mPING API key"));
        assert!(plan.clears.contains(&"Preferred site"));
        assert!(!plan.keeps.contains(&"Preferred site"));
        assert_eq!(plan.write, None);
    }

    #[wasm_bindgen_test]
    fn reset_lists_what_it_clears_and_keeps() {
        let plan = reset_plan(&Preferences::default(), false);
        assert!(plan.clears.contains(&"Developer mode"));
        assert!(plan.clears.contains(&"Storage quota"));
        assert!(plan.keeps.contains(&"Saved events"));
        assert!(plan.keeps.contains(&"Cached radar data"));
        // Nothing to keep beyond the fixed list without a key or site.
        assert!(!plan.keeps.contains(&"mPING API key"));
        assert_eq!(plan.write, None);
        assert_eq!(plan.remove_keys, vec![STORAGE_KEY, STORAGE_SETTINGS_KEY]);
        // Saved events live under their own key, which a reset never touches.
        assert!(!plan.remove_keys.contains(&"nexrad_saved_events"));
    }
}
