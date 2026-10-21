//! User preference persistence — the shell half of
//! [`Preferences`](crate::core::Preferences).
//!
//! The struct, its migration and the reset decision are pure
//! (`crate::core::prefs`); this module holds the impls that touch
//! localStorage (`load`/`save`) and the `AppState` snapshot/apply pair.
//! Loaded on startup, saved automatically when changes are detected.

use crate::core::panels::RailTab;
use crate::core::prefs::{self, Loaded, ToolPrefs};
use crate::core::Preferences;

use super::AppState;

/// The panel's tab and whether it is open — kept on the Chrome subsystem,
/// passed in so `state` doesn't reach for it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PanelPrefs {
    pub tab: RailTab,
    pub open: bool,
}

impl Preferences {
    /// Snapshot current preferences from application state.
    ///
    /// `mping_api_key` is sourced separately because the mPING state lives on
    /// the diagnostics subsystem, `panel` on the Chrome subsystem, and
    /// `playback` comes from the Playback subsystem. `prev` (the last saved
    /// snapshot) carries the speed of the mode not playing now.
    pub(crate) fn from_app_state(
        state: &AppState,
        playback: &crate::core::PlaybackState,
        panel: PanelPrefs,
        mping_api_key: Option<String>,
        prev: &Preferences,
    ) -> Self {
        let mut prefs = Self {
            version: prefs::VERSION,
            speed_frames: prev.speed_frames,
            speed_rays: prev.speed_rays,
            loop_style: playback.time_model.loop_mode,
            layer_states: state.layer_state.geo.states,
            layer_counties: state.layer_state.geo.counties,
            layer_labels: state.layer_state.geo.labels,
            layer_nexrad_sites: state.layer_state.geo.nexrad_sites,
            layer_cities: state.layer_state.geo.cities,
            layer_national_mosaic: state.layer_state.geo.national_mosaic,
            layer_alerts_warnings: state.layer_state.geo.alerts_warnings,
            layer_alerts_other: state.layer_state.geo.alerts_other,
            layer_mping: state.layer_state.geo.mping,
            interpolation: state.render_processing.interpolation,
            opacity: state.render_processing.opacity,
            data_age_desaturation: state.render_processing.data_age_desaturation,
            use_local_time: state.use_local_time,
            pause_stream_while_reviewing: state.pause_stream_while_reviewing,
            autofetch_while_scrubbing: state.autofetch_while_scrubbing,
            panel_tab: panel.tab,
            panel_open: panel.open,
            tools: ToolPrefs {
                probe: state.viz_state.data_probe_enabled,
                distance: state.viz_state.distance_tool_active,
                storm_cells: state.viz_state.storm_cells_visible,
                storm_cell_dbz: state.viz_state.storm_cell_threshold_dbz,
            },
            developer_mode: state.dev_mode_saved,
            mping_api_key,
            preferred_site: state.preferred_site.clone(),
            recent_sites: state.recent_sites.clone(),
            mobile_override: state.mobile_override,
        };
        prefs.remember_speed(playback.play_mode, playback.chosen_speed());
        prefs
    }

    /// Apply loaded preferences to application state. Returns the saved
    /// `mping_api_key` (if any) so the caller can apply it to the
    /// diagnostics subsystem; `AppState` no longer owns mPING state.
    /// `playback` is given the loop style and the saved speed for its mode
    /// (the link may then switch the mode and its speed, see `apply_link`).
    /// The panel is applied by the caller, which owns the Chrome subsystem.
    pub(crate) fn apply_to(
        &self,
        state: &mut AppState,
        playback: &mut crate::core::PlaybackState,
    ) -> Option<String> {
        playback.speed = self.speed_for(playback.play_mode);
        playback.restore_play_mode(playback.play_mode);
        playback.time_model.loop_mode = self.loop_style;
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
        state.recent_sites = self.recent_sites.clone();
        state.render_processing.interpolation = self.interpolation;
        state.render_processing.opacity = self.opacity;
        state.render_processing.data_age_desaturation = self.data_age_desaturation;
        state.mobile_override = self.mobile_override;
        state.pause_stream_while_reviewing = self.pause_stream_while_reviewing;
        state.autofetch_while_scrubbing = self.autofetch_while_scrubbing;
        state.viz_state.data_probe_enabled = self.tools.probe;
        state.viz_state.distance_tool_active = self.tools.distance;
        state.viz_state.storm_cells_visible = self.tools.storm_cells;
        state.viz_state.storm_cell_threshold_dbz =
            crate::core::storm_cells::clamp_threshold(self.tools.storm_cell_dbz);
        state.dev_mode = self.developer_mode;
        state.dev_mode_saved = self.developer_mode;
        self.mping_api_key.clone()
    }

    /// Load preferences from localStorage, migrating an older blob. A
    /// migrated blob is written back at once, so the retired fields are read
    /// once and gone.
    pub(crate) fn load() -> Self {
        let stored = web_sys::window()
            .and_then(|w| w.local_storage().ok().flatten())
            .and_then(|s| s.get_item(prefs::STORAGE_KEY).ok().flatten());
        let (prefs, loaded) = prefs::parse_stored(stored.as_deref());
        match loaded {
            Loaded::Fresh => log::debug!("No stored preferences; using defaults"),
            Loaded::Current => log::debug!("Loaded user preferences from localStorage"),
            Loaded::Migrated { from } => {
                log::info!("Migrated user preferences from version {from}");
                prefs.save();
            }
        }
        prefs
    }

    /// Save preferences to localStorage.
    pub(crate) fn save(&self) {
        let Some(storage) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) else {
            return;
        };
        match serde_json::to_string(self) {
            Ok(json) => {
                if let Err(e) = storage.set_item(prefs::STORAGE_KEY, &json) {
                    log::warn!("Failed to save user preferences: {:?}", e);
                }
            }
            Err(e) => log::warn!("Failed to serialize user preferences: {}", e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{InterpolationMode, LoopMode, PlaybackMode, PlaybackSpeed};
    use wasm_bindgen_test::wasm_bindgen_test;

    fn states() -> (AppState, crate::core::PlaybackState) {
        (AppState::default(), crate::core::PlaybackState::default())
    }

    fn panel() -> PanelPrefs {
        PanelPrefs {
            tab: RailTab::View,
            open: true,
        }
    }

    fn snapshot(state: &AppState, playback: &crate::core::PlaybackState) -> Preferences {
        Preferences::from_app_state(state, playback, panel(), None, &Preferences::default())
    }

    #[wasm_bindgen_test]
    fn from_app_state_passes_mping_key_and_panel_through() {
        let (state, playback) = states();
        let p = Preferences::from_app_state(
            &state,
            &playback,
            PanelPrefs {
                tab: RailTab::Events,
                open: false,
            },
            Some("KEY".to_string()),
            &Preferences::default(),
        );
        assert_eq!(p.mping_api_key.as_deref(), Some("KEY"));
        assert_eq!(p.panel_tab, RailTab::Events);
        assert!(!p.panel_open);
        assert_eq!(p.version, prefs::VERSION);
    }

    #[wasm_bindgen_test]
    fn the_speed_of_the_other_mode_is_kept() {
        let (state, mut playback) = states();
        playback.speed = PlaybackSpeed::Double;
        let prev = Preferences {
            speed_rays: PlaybackSpeed::FifteenToOne,
            ..Preferences::default()
        };
        let p = Preferences::from_app_state(&state, &playback, panel(), None, &prev);
        assert_eq!(p.speed_frames, PlaybackSpeed::Double);
        assert_eq!(p.speed_rays, PlaybackSpeed::FifteenToOne);
    }

    #[wasm_bindgen_test]
    fn the_live_lock_in_rays_is_not_saved() {
        let (state, mut playback) = states();
        playback.restore_play_mode(PlaybackMode::Micro);
        playback.speed = PlaybackSpeed::Quadruple;
        playback.enter_pinned_live(1_000.0);
        assert_eq!(playback.speed, PlaybackSpeed::Realtime);
        assert_eq!(
            snapshot(&state, &playback).speed_rays,
            PlaybackSpeed::Quadruple
        );
    }

    #[wasm_bindgen_test]
    fn a_link_dev_flag_is_not_saved() {
        // The link sets dev_mode for the session; only the toggle sets the
        // saved flag.
        let (mut state, playback) = states();
        state.dev_mode = true;
        assert!(!snapshot(&state, &playback).developer_mode);
        state.dev_mode_saved = true;
        assert!(snapshot(&state, &playback).developer_mode);
    }

    #[wasm_bindgen_test]
    fn snapshot_then_apply_round_trips_observable_fields() {
        let (mut src, mut src_pb) = states();
        src_pb.speed = PlaybackSpeed::Quadruple;
        src_pb.time_model.loop_mode = LoopMode::PingPong;
        src.layer_state.geo.states = false;
        src.layer_state.geo.nexrad_sites = true;
        src.layer_state.geo.national_mosaic = true;
        src.layer_state.geo.alerts_other = true;
        src.layer_state.geo.mping = true;
        src.use_local_time = false;
        src.preferred_site = Some("KTLX".to_string());
        src.recent_sites = vec!["KTLX".to_string(), "KDMX".to_string()];
        src.render_processing.interpolation = InterpolationMode::Bilinear;
        src.render_processing.opacity = 0.3;
        src.render_processing.data_age_desaturation = false;
        src.mobile_override = Some(false);
        src.pause_stream_while_reviewing = true;
        src.autofetch_while_scrubbing = false;
        src.viz_state.data_probe_enabled = true;
        src.viz_state.storm_cells_visible = true;
        src.viz_state.storm_cell_threshold_dbz = 45.0;
        src.dev_mode_saved = true;

        let prefs = snapshot(&src, &src_pb);
        let (mut dst, mut dst_pb) = states();
        prefs.apply_to(&mut dst, &mut dst_pb);

        assert_eq!(dst_pb.speed, PlaybackSpeed::Quadruple);
        assert_eq!(dst_pb.time_model.loop_mode, LoopMode::PingPong);
        assert!(!dst.layer_state.geo.states);
        assert!(dst.layer_state.geo.nexrad_sites);
        assert!(dst.layer_state.geo.national_mosaic);
        assert!(dst.layer_state.geo.alerts_other);
        assert!(dst.layer_state.geo.mping);
        assert!(!dst.use_local_time);
        assert_eq!(dst.preferred_site.as_deref(), Some("KTLX"));
        assert_eq!(dst.recent_sites, src.recent_sites);
        assert!(dst.render_processing.interpolation == InterpolationMode::Bilinear);
        assert!((dst.render_processing.opacity - 0.3).abs() < 1e-6);
        assert!(!dst.render_processing.data_age_desaturation);
        assert_eq!(dst.mobile_override, Some(false));
        assert!(dst.pause_stream_while_reviewing);
        assert!(!dst.autofetch_while_scrubbing);
        assert!(dst.viz_state.data_probe_enabled);
        assert!(dst.viz_state.storm_cells_visible);
        assert!((dst.viz_state.storm_cell_threshold_dbz - 45.0).abs() < 1e-6);
        assert!(dst.dev_mode && dst.dev_mode_saved);
    }

    #[wasm_bindgen_test]
    fn stored_storm_cell_threshold_is_clamped_to_slider_range() {
        for (stored, applied) in [(5.0, 20.0), (99.0, 60.0), (42.0, 42.0)] {
            let mut prefs = Preferences::default();
            prefs.tools.storm_cell_dbz = stored;
            let (mut state, mut playback) = states();
            prefs.apply_to(&mut state, &mut playback);
            assert_eq!(state.viz_state.storm_cell_threshold_dbz, applied);
        }
    }

    #[wasm_bindgen_test]
    fn default_prefs_match_default_state() {
        // Applying defaults to a default state changes nothing a fresh
        // snapshot would see.
        let (mut state, mut playback) = states();
        Preferences::default().apply_to(&mut state, &mut playback);
        assert_eq!(snapshot(&state, &playback), Preferences::default());
    }
}
