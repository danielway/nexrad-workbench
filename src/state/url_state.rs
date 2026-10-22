//! The link's shell half: read it from `location.search`, assemble it from
//! app state, and write it with `history.replaceState`.
//!
//! The codec, the table of what the link carries and the boot decision are
//! pure and live in [`crate::core::link`]; this module only touches the
//! browser and reads `AppState`.

use crate::core::link::{LinkSelection, LinkState, LinkTilt};
use crate::core::{ElevationSelection, Selection, ViewState};

impl ViewState {
    /// Build the `v` blob from the current [`super::AppState`] plus the
    /// explicit `playback` slice (from [`crate::subsystem::Playback::state`];
    /// the URL-state module doesn't reach for the subsystem so the dependency
    /// stays one-way). The legacy `rt` flag is no longer written: a pinned
    /// selection travels as the link's `sel`.
    pub(crate) fn from_state(
        state: &super::AppState,
        playback: &crate::core::PlaybackState,
    ) -> Self {
        let snap = state.viz_state.camera.url_snapshot();
        Self {
            mz: Some(state.viz_state.zoom()),
            tz: Some(playback.timeline_zoom),
            vm: Some(match state.viz_state.view_mode() {
                crate::geo::ViewMode::Flat2D => 0,
                crate::geo::ViewMode::Globe3D => 1,
            }),
            // The unified orbit camera: pivot (clat/clon), center-convention
            // distance (cd), tilt (ct) and heading (cr). The legacy per-mode
            // fields (cm/ob/oe and the free-look set) are no longer written;
            // they remain parse-only so pre-overhaul links still restore.
            cd: Some(snap.distance),
            clat: Some(snap.center_lat),
            clon: Some(snap.center_lon),
            ct: Some(snap.tilt),
            cr: Some(snap.rotation),
            v3d: Some(state.viz_state.volume_3d_enabled),
            vdc: Some(state.viz_state.volume_density_cutoff),
            ..Default::default()
        }
    }
}

/// The link's selection for a [`Selection`].
pub(crate) fn link_selection(selection: Selection) -> LinkSelection {
    match selection {
        Selection::Point { live: true } => LinkSelection::PinnedNow,
        Selection::Point { live: false } => LinkSelection::Point,
        Selection::Range(r) if r.live => LinkSelection::PinnedRange(r.basis),
        Selection::Range(r) => match r.bounds {
            Some((start, end)) => LinkSelection::Range { start, end },
            None => LinkSelection::Point,
        },
    }
}

/// Assemble the link for what is on screen — the shell-side input builder
/// for the pure [`crate::core::decide_persist`]. Kept beside
/// [`ViewState::from_state`] so the whole state → link field mapping lives in
/// one place.
pub(crate) fn build_link(
    state: &super::AppState,
    playback: &crate::core::PlaybackState,
) -> LinkState {
    LinkState {
        site: Some(state.viz_state.site_id.clone()),
        time: Some(playback.playback_position()),
        selection: Some(link_selection(playback.selection)),
        play_mode: Some(playback.chosen_play_mode()),
        product: Some(state.viz_state.product.short_code().to_string()),
        tilt: Some(match state.viz_state.elevation_selection {
            ElevationSelection::Latest => LinkTilt::Latest,
            ElevationSelection::Fixed { angle, .. } => LinkTilt::Angle(angle),
        }),
        lat: Some(state.viz_state.center_lat),
        lon: Some(state.viz_state.center_lon),
        view: ViewState::from_state(state, playback),
        cells: state
            .viz_state
            .storm_cells_visible
            .then_some(state.viz_state.storm_cell_threshold_dbz),
        tracks: (state.viz_state.storm_cells_visible && state.viz_state.storm_tracks_visible)
            .then_some(true),
        dev: Some(state.dev_mode),
    }
}

/// Read the link from the current browser URL.
pub(crate) fn read_link() -> LinkState {
    web_sys::window()
        .and_then(|w| w.location().search().ok())
        .map(|search| crate::core::link::parse(&search))
        .unwrap_or_default()
}

/// Write the link to the URL bar with `replaceState`.
pub(crate) fn push_link(link: &LinkState) {
    let query = crate::core::link::to_query(link);
    let Some(history) = web_sys::window().and_then(|w| w.history().ok()) else {
        return;
    };
    let _ = history.replace_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(&query));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{LoopBasis, PlaybackMode};
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn storm_cells_join_the_link_only_when_on() {
        let mut state = crate::state::AppState::default();
        let playback = crate::core::PlaybackState::default();
        assert_eq!(build_link(&state, &playback).cells, None);
        state.viz_state.storm_cells_visible = true;
        state.viz_state.storm_cell_threshold_dbz = 45.0;
        let q = crate::core::link::to_query(&build_link(&state, &playback));
        assert!(q.contains("cells=45"), "{q}");
        assert!(!q.contains("tracks"), "{q}");
        state.viz_state.storm_tracks_visible = true;
        let q = crate::core::link::to_query(&build_link(&state, &playback));
        assert!(q.contains("cells=45&tracks=1"), "{q}");
        // Tracks without cells draw nothing, so the link leaves them out.
        state.viz_state.storm_cells_visible = false;
        assert_eq!(build_link(&state, &playback).tracks, None);
    }

    #[wasm_bindgen_test]
    fn a_pinned_point_travels_as_sel_now_not_rt() {
        let state = crate::state::AppState::default();
        let mut playback = crate::core::PlaybackState::default();
        playback.enter_pinned_live(1_700_000_000.0);
        let link = build_link(&state, &playback);
        assert_eq!(link.selection, Some(LinkSelection::PinnedNow));
        assert_eq!(link.view.rt, None);
        // And it opens pinned again.
        let back = crate::core::link::parse(&crate::core::link::to_query(&link));
        assert_eq!(back.selection, Some(LinkSelection::PinnedNow));
    }

    #[wasm_bindgen_test]
    fn a_detached_point_travels_as_its_time() {
        let state = crate::state::AppState::default();
        let mut playback = crate::core::PlaybackState::default();
        playback.set_playback_position(1_700_000_000.0);
        let link = build_link(&state, &playback);
        assert_eq!(link.selection, Some(LinkSelection::Point));
        let q = crate::core::link::to_query(&link);
        assert!(q.contains("t=1700000000"), "{q}");
        assert!(!q.contains("sel="), "{q}");
    }

    #[wasm_bindgen_test]
    fn ranges_map_to_the_link() {
        let mut playback = crate::core::PlaybackState::default();
        playback.set_playback_position(1_500.0);
        assert!(playback.select_range(1_000.0, 2_000.0, false));
        assert_eq!(
            link_selection(playback.selection),
            LinkSelection::Range {
                start: 1_000.0,
                end: 2_000.0
            }
        );
        assert!(playback.select_range(1_000.0, 2_000.0, true));
        assert_eq!(
            link_selection(playback.selection),
            LinkSelection::PinnedRange(LoopBasis::Duration(1_000.0))
        );
    }

    #[wasm_bindgen_test]
    fn link_carries_mode_product_tilt_center_and_dev() {
        let mut state = crate::state::AppState::default();
        let mut playback = crate::core::PlaybackState::default();
        state.viz_state.site_id = "KDMX".to_string();
        state.viz_state.product = crate::core::RadarProduct::Velocity;
        state.viz_state.center_lat = 41.25;
        state.viz_state.center_lon = -93.75;
        state.viz_state.elevation_selection = ElevationSelection::Latest;
        state.dev_mode = true;
        playback.restore_play_mode(PlaybackMode::Micro);
        let link = build_link(&state, &playback);
        assert_eq!(link.site.as_deref(), Some("KDMX"));
        assert_eq!(link.product.as_deref(), Some("VEL"));
        assert_eq!(link.tilt, Some(LinkTilt::Latest));
        assert_eq!(link.play_mode, Some(PlaybackMode::Micro));
        assert_eq!(link.lat, Some(41.25));
        assert_eq!(link.lon, Some(-93.75));
        assert_eq!(link.dev, Some(true));
        assert_eq!(link.view, ViewState::from_state(&state, &playback));
    }

    #[wasm_bindgen_test]
    fn from_state_default_sets_core_view_fields() {
        let state = crate::state::AppState::default();
        let playback = crate::core::PlaybackState::default();
        let vs = ViewState::from_state(&state, &playback);
        // These are always populated (Some).
        assert!(vs.mz.is_some());
        assert!(vs.tz.is_some());
        assert!(vs.vm.is_some());
        assert!(vs.cd.is_some());
        // Legacy per-mode fields are parse-only — never written anymore.
        assert!(vs.cm.is_none());
        assert!(vs.rt.is_none());
        assert_eq!(vs.tz, Some(playback.timeline_zoom));
    }
}
