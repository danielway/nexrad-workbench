//! Visualization state (canvas, zoom/pan, product selection).

use crate::core::canvas::CaptionContext;
use crate::core::storm_cell_view::StormCellPointer;
use crate::core::storm_tracks::StormTracks;
use crate::core::{ElevationSelection, RadarProduct};
use crate::geo::{Camera, Flat2DState, ViewMode};
use eframe::egui::Vec2;

/// Visualization state including view controls.
pub(crate) struct VizState {
    /// The camera state machine — the single source of truth for the view
    /// mode and all 2D/3D camera state. [`ViewMode`] is derived from the
    /// active variant via [`VizState::view_mode`] (no separate stored
    /// toggle). 2D pan/zoom live on the [`Flat2D`](crate::geo::Camera::Flat2D)
    /// variant; read/write via [`VizState::zoom`] / [`VizState::pan_offset`]
    /// / [`VizState::set_zoom`] / [`VizState::set_pan_offset`].
    pub camera: Camera,

    /// Where the map is: the `(lat, lon)` the 2D projection centres on (the
    /// pan offset moves the view from it), the 3D camera is centred over, the
    /// link's `lat`/`lon` carry, and the scale bar and the mPING query read.
    /// Set by a site pick, a focus change, "Show on map" and the boot (from
    /// the link, else the focus site). The radars' own positions are on
    /// their `SiteState`s; this is not one of them.
    pub map_centre: (f64, f64),

    /// Selected radar product
    pub product: RadarProduct,

    /// Elevation selection (specific VCP cut or auto/latest mode)
    pub elevation_selection: ElevationSelection,

    /// Stored Fixed selection to restore when toggling off auto mode.
    pub last_fixed_selection: Option<(u8, f32)>,

    /// Overlay info: current elevation/sweep, e.g. "0.5°". Timezone-independent,
    /// so it stays a baked string; the displayed-frame *timestamp* is NOT baked
    /// here — it is formatted at render time from the focus site's `displayed`
    /// (spec §11.4) so a local/UTC flip reformats it the same frame.
    pub elevation: String,

    /// Cached last sweep line position (azimuth, start_azimuth) for between-sweep display.
    pub last_sweep_line_cache: Option<(f32, f32)>,

    /// Whether 3D volumetric rendering is enabled (ray-marched volume).
    pub volume_3d_enabled: bool,

    /// Density cutoff for volume rendering (physical value, e.g. 5.0 dBZ).
    pub volume_density_cutoff: f32,

    /// Whether the data-probe tool is active (hover shows lat/lon and data
    /// value). Named "Data probe" to keep "inspector" for the scan inspector
    /// (the per-scan volume breakdown), which is an unrelated surface.
    pub data_probe_enabled: bool,

    /// Whether the distance measurement tool is active.
    pub distance_tool_active: bool,

    /// Distance measurement start point (lat, lon).
    pub distance_start: Option<(f64, f64)>,

    /// Distance measurement end point (lat, lon).
    pub distance_end: Option<(f64, f64)>,

    /// Whether storm cell detection overlay is visible.
    pub storm_cells_visible: bool,

    /// Minimum dBZ threshold for storm cell detection.
    pub storm_cell_threshold_dbz: f32,

    /// Whether storm cell tracks (trails and forecast arrows) are on. Needs
    /// `storm_cells_visible` too.
    pub storm_tracks_visible: bool,

    /// Tracks across volumes for the active site (see
    /// `crate::core::storm_tracks`).
    pub storm_tracks: StormTracks,
    /// The desktop pointer and the phone's selected point over the storm
    /// cell overlay (see `crate::core::storm_cell_view`).
    pub storm_cell_pointer: StormCellPointer,

    /// Last observed visible map bounds in 2D mode, as
    /// `(min_lon, min_lat, max_lon, max_lat)`. Updated each frame by the
    /// canvas renderer and consumed by top-bar / modal logic that needs
    /// to know what area the user is looking at without access to the
    /// canvas rect. `None` while in 3D globe mode.
    pub last_visible_bounds: Option<(f64, f64, f64, f64)>,

    /// What the render loop knows for the canvas caption rule this frame
    /// (one frame's length, a render in flight, downloads, the wanted tilt).
    /// Recomputed each frame in `advance_playback`; the session's caption
    /// rule reads it beside its frame and playhead facts.
    pub caption_context: CaptionContext,
}

impl Default for VizState {
    fn default() -> Self {
        Self {
            camera: Camera::centered_on(41.7312, -93.7229),
            map_centre: (41.7312, -93.7229),
            product: RadarProduct::default(),
            elevation_selection: ElevationSelection::default(),
            last_fixed_selection: None,
            elevation: "-- deg".to_string(),
            last_sweep_line_cache: None,
            volume_3d_enabled: true,
            volume_density_cutoff: 5.0,
            data_probe_enabled: false,
            distance_tool_active: false,
            distance_start: None,
            distance_end: None,
            storm_cells_visible: false,
            storm_cell_threshold_dbz: 35.0,
            storm_tracks_visible: false,
            storm_tracks: StormTracks::default(),
            storm_cell_pointer: StormCellPointer::default(),
            last_visible_bounds: None,
            caption_context: CaptionContext::default(),
        }
    }
}

impl VizState {
    /// Put the map's centre at `(lat, lon)` and centre the view on it: the
    /// 2D pan is reset and the 3D camera hovers over it as it does over a
    /// site.
    pub(crate) fn centre_map_on(&mut self, lat: f64, lon: f64) {
        self.map_centre = (lat, lon);
        self.set_pan_offset(Vec2::ZERO);
        self.camera.center_on(lat, lon);
    }

    /// The active [`ViewMode`], derived from the camera variant.
    pub(crate) fn view_mode(&self) -> ViewMode {
        self.camera.view_mode()
    }

    /// Volume rendering is on and the camera is in 3D: the one predicate
    /// every volume-render gate and the fetch scope share.
    pub(crate) fn volume_3d_active(&self) -> bool {
        self.volume_3d_enabled && self.view_mode() == ViewMode::Globe3D
    }

    /// What the tilt controls do: every cut while volume rendering is
    /// active, else the one you pick.
    pub(crate) fn tilt_role(&self) -> crate::core::session::TiltRole {
        crate::core::session::TiltRole::from_volume_3d(self.volume_3d_active())
    }

    /// The tilt scope archive fetches store: the whole volume while volume
    /// rendering is active, else the picked tilt.
    pub(crate) fn fetch_scope(&self) -> ElevationSelection {
        crate::core::acquisition::fetch_scope(&self.elevation_selection, self.volume_3d_active())
    }

    /// Whether the flat 2D view is active.
    pub(crate) fn is_2d(&self) -> bool {
        self.camera.is_2d()
    }

    /// Current 2D zoom level (1.0 = 100%). Falls back to the default zoom in
    /// 3D modes (the 2D pan/zoom is only meaningful in the Flat2D variant).
    pub(crate) fn zoom(&self) -> f32 {
        self.camera
            .flat_2d()
            .map(|s| s.zoom)
            .unwrap_or(crate::geo::DEFAULT_FLAT_ZOOM)
    }

    /// Current 2D pan offset. `ZERO` in 3D modes.
    pub(crate) fn pan_offset(&self) -> Vec2 {
        self.camera
            .flat_2d()
            .map(|s| s.pan_offset)
            .unwrap_or(Vec2::ZERO)
    }

    /// Set the 2D zoom level. No-op in 3D modes.
    pub(crate) fn set_zoom(&mut self, zoom: f32) {
        if let Some(s) = self.camera.flat_2d_mut() {
            s.zoom = zoom;
        }
    }

    /// Set the 2D pan offset. No-op in 3D modes.
    pub(crate) fn set_pan_offset(&mut self, pan_offset: Vec2) {
        if let Some(s) = self.camera.flat_2d_mut() {
            s.pan_offset = pan_offset;
        }
    }

    /// Mutable access to the 2D pan offset, if the flat view is active.
    /// Used by the WASD pan path that increments each axis.
    pub(crate) fn flat_pan_mut(&mut self) -> Option<&mut Vec2> {
        self.camera.flat_2d_mut().map(|s| &mut s.pan_offset)
    }

    /// Switch the camera to the 3D globe view, restoring the last 3D
    /// camera pose (carried on the Flat2D variant across 2D excursions).
    pub(crate) fn switch_to_3d_view(&mut self) {
        self.camera.switch_to_globe();
    }

    /// Switch the camera to the flat 2D view (default pan/zoom).
    pub(crate) fn switch_to_2d(&mut self) {
        self.camera.switch_to_flat_2d(Flat2DState::default());
    }

    /// Toggle between the flat 2D view and the 3D globe. Mirrors the
    /// historical `T` shortcut; the 3D side restores the saved orbit pose.
    pub(crate) fn toggle_2d_3d(&mut self) {
        if self.camera.is_2d() {
            self.switch_to_3d_view();
        } else {
            self.switch_to_2d();
        }
    }

    /// Set the canvas overlay's elevation text from a freshly decoded sweep.
    /// The displayed-frame *timestamp* is not baked here: it is formatted at
    /// render time from the focus site's `displayed` so a local/UTC flip
    /// reformats it the same frame (spec §11.4).
    pub(crate) fn set_elevation_label(&mut self, elevation_deg: f32) {
        self.elevation = format!("{:.1}\u{00B0}", elevation_deg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn view_mode_and_zoom_pan_derive_from_camera() {
        let mut viz = VizState::default();
        assert_eq!(viz.view_mode(), ViewMode::Flat2D);
        assert!(viz.is_2d());
        // 2D pan/zoom round-trip through the accessors.
        viz.set_zoom(3.0);
        viz.set_pan_offset(Vec2::new(12.0, -4.0));
        assert!((viz.zoom() - 3.0).abs() < 1e-6);
        assert!((viz.pan_offset().x - 12.0).abs() < 1e-6);
        // Switching to 3D makes view_mode derive to Globe3D; zoom/pan
        // fall back to the 2D defaults (only meaningful in Flat2D).
        viz.switch_to_3d_view();
        assert_eq!(viz.view_mode(), ViewMode::Globe3D);
        assert!(!viz.is_2d());
        assert!((viz.zoom() - crate::geo::DEFAULT_FLAT_ZOOM).abs() < 1e-6);
        assert_eq!(viz.pan_offset(), Vec2::ZERO);
    }

    #[wasm_bindgen_test]
    fn toggle_restores_saved_3d_pose() {
        let mut viz = VizState::default();
        // Enter 3D and put the camera in a distinctive pose.
        viz.switch_to_3d_view();
        viz.camera.move_pivot_to(30.0, -80.0);
        viz.camera.adjust_tilt_heading(100.0, -60.0, 600.0);
        let before = *viz.camera.orbit_state().unwrap();
        viz.switch_to_2d();
        assert!(viz.is_2d());
        // Toggling back restores the exact orbit pose, not the defaults.
        viz.toggle_2d_3d();
        let after = viz.camera.orbit_state().unwrap();
        assert!((after.pivot_lat - before.pivot_lat).abs() < 1e-6);
        assert!((after.tilt - before.tilt).abs() < 1e-6);
        assert!((after.heading - before.heading).abs() < 1e-6);
        // Toggling again returns to 2D.
        viz.toggle_2d_3d();
        assert!(viz.is_2d());
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    // ---- enum defaults -----------------------------------------------------

    #[wasm_bindgen_test]
    fn enum_defaults() {
        assert_eq!(ViewMode::default(), ViewMode::Flat2D);
    }

    // ---- VizState::set_elevation_label -----------------------------------

    #[wasm_bindgen_test]
    fn elevation_label_formats_to_a_tenth_of_a_degree() {
        let mut viz = VizState::default();
        viz.set_elevation_label(0.5);
        assert_eq!(viz.elevation, "0.5\u{00B0}");
        viz.set_elevation_label(12.34);
        assert_eq!(viz.elevation, "12.3\u{00B0}");
    }

    #[wasm_bindgen_test]
    fn centre_map_on_moves_the_centre_resets_the_pan_and_the_camera() {
        let mut viz = VizState::default();
        viz.set_pan_offset(Vec2::new(40.0, -12.0));
        viz.centre_map_on(28.2, -81.9);
        assert_eq!(viz.map_centre, (28.2, -81.9));
        assert_eq!(viz.pan_offset(), Vec2::ZERO);
        // The 3D camera hovers over the centre.
        let snap = viz.camera.url_snapshot();
        assert!((snap.center_lat - 28.2).abs() < 1e-4);
        assert!((snap.center_lon + 81.9).abs() < 1e-4);
        // The 2D zoom is left alone.
        assert!((viz.zoom() - crate::geo::DEFAULT_FLAT_ZOOM).abs() < 1e-6);
    }

    // ---- VizState 2D/3D accessor no-ops ----------------------------------

    #[wasm_bindgen_test]
    fn set_zoom_pan_are_noops_in_3d() {
        let mut viz = VizState::default();
        viz.switch_to_3d_view();
        assert!(!viz.is_2d());
        // No flat-2D state to write -> setters are no-ops, getters fall back.
        viz.set_zoom(5.0);
        viz.set_pan_offset(Vec2::new(9.0, 9.0));
        assert!((viz.zoom() - crate::geo::DEFAULT_FLAT_ZOOM).abs() < 1e-6);
        assert_eq!(viz.pan_offset(), Vec2::ZERO);
        // flat_pan_mut yields None in 3D.
        assert!(viz.flat_pan_mut().is_none());
    }
}
