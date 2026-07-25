//! URL state encoding/decoding for shareable URLs.
//!
//! Encodes site, playback time, product, and map center in the URL query string
//! so reloading restores the view and URLs can be shared.
//!
//! The `v` parameter is an opaque base64-encoded JSON blob carrying
//! auxiliary view state (map zoom, timeline zoom, etc.) that may grow
//! over time without changing the URL schema.

use base64::Engine as _;

use crate::core::ViewState;

/// URL wire code for a [`CameraMode`](crate::geo::CameraMode):
/// 0 = PlanetOrbit, 1 = SiteOrbit, 2 = FreeLook.
fn camera_mode_code(mode: crate::geo::CameraMode) -> u8 {
    match mode {
        crate::geo::CameraMode::PlanetOrbit => 0,
        crate::geo::CameraMode::SiteOrbit => 1,
        crate::geo::CameraMode::FreeLook => 2,
    }
}

impl ViewState {
    /// Build a `ViewState` from the current [`super::AppState`] plus
    /// explicit `playback` and `is_live` slices.
    ///
    /// `playback` comes from [`crate::subsystem::Playback::state`] and
    /// `is_live` from [`crate::subsystem::Live::mode_state`] — the
    /// URL-state module doesn't reach for either subsystem so the
    /// dependency stays one-way.
    pub fn from_state(
        state: &super::AppState,
        playback: &crate::core::PlaybackState,
        is_live: bool,
    ) -> Self {
        let snap = state.viz_state.camera.url_snapshot();
        Self {
            mz: Some(state.viz_state.zoom()),
            tz: Some(playback.timeline_zoom),
            vm: Some(match state.viz_state.view_mode() {
                crate::geo::ViewMode::Flat2D => 0,
                crate::geo::ViewMode::Globe3D => 1,
            }),
            // The 3D camera mode to restore. In 2D the camera variant carries
            // no 3D mode, so persist the remembered `last_3d_mode` (the mode a
            // 2D → 3D toggle would re-enter) — this keeps a reloaded 2D link
            // returning to the same 3D mode the user last used.
            cm: Some(camera_mode_code(
                state
                    .viz_state
                    .camera
                    .camera_mode()
                    .unwrap_or(state.viz_state.last_3d_mode),
            )),
            cd: Some(snap.distance),
            clat: Some(snap.center_lat),
            clon: Some(snap.center_lon),
            ct: Some(snap.tilt),
            cr: Some(snap.rotation),
            ob: Some(snap.orbit_bearing),
            oe: Some(snap.orbit_elevation),
            fp: Some(snap.free_pos),
            fy: Some(snap.free_yaw),
            fpt: Some(snap.free_pitch),
            fs: Some(snap.free_speed),
            v3d: Some(state.viz_state.volume_3d_enabled),
            vdc: Some(state.viz_state.volume_density_cutoff),
            rt: is_live.then_some(true),
        }
    }
}

/// Assemble the full [`UrlPush`] payload from app state — the shell-side input
/// builder for the pure [`crate::core::decide_persist`]. Kept beside
/// [`ViewState::from_state`] so the whole state → URL field mapping lives in
/// one place.
pub fn build_url_push(
    state: &super::AppState,
    playback: &crate::core::PlaybackState,
    is_live: bool,
) -> crate::core::effect::UrlPush {
    crate::core::effect::UrlPush {
        site: state.viz_state.site_id.clone(),
        time: playback.playback_position(),
        product: state.viz_state.product.short_code().to_string(),
        lat: state.viz_state.center_lat,
        lon: state.viz_state.center_lon,
        view: ViewState::from_state(state, playback, is_live),
        dev: state.dev_mode,
    }
}

/// Parsed URL parameters.
pub struct UrlParams {
    pub site: Option<String>,
    pub time: Option<f64>,
    pub product: Option<String>,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub view: ViewState,
    /// Developer mode flag — `true` only when `?dev=true` is present.
    pub dev: bool,
    /// UI mode override: `Some(true)` = Advanced, `Some(false)` = Basic,
    /// `None` = use stored preference. Set via `?ui=advanced` / `?ui=basic`.
    pub ui_advanced: Option<bool>,
}

/// Parse URL query parameters from the current browser URL.
pub fn parse_from_url() -> UrlParams {
    let mut params = UrlParams {
        site: None,
        time: None,
        product: None,
        lat: None,
        lon: None,
        view: ViewState::default(),
        dev: false,
        ui_advanced: None,
    };

    let Ok(search) = web_sys::window().expect("no window").location().search() else {
        return params;
    };

    let query = search.trim_start_matches('?');
    if query.is_empty() {
        return params;
    }

    for pair in query.split('&') {
        let mut kv = pair.splitn(2, '=');
        let key = kv.next().unwrap_or("");
        let value = kv.next().unwrap_or("");
        match key {
            "site" => params.site = Some(value.to_string()),
            "t" => params.time = value.parse().ok(),
            "product" => params.product = Some(value.to_string()),
            "lat" => params.lat = value.parse().ok(),
            "lon" => params.lon = value.parse().ok(),
            "v" => {
                if let Ok(bytes) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(value) {
                    if let Ok(vs) = serde_json::from_slice::<ViewState>(&bytes) {
                        params.view = vs;
                    }
                }
            }
            "dev" => params.dev = value == "true",
            "ui" => {
                params.ui_advanced = match value {
                    "advanced" => Some(true),
                    "basic" => Some(false),
                    _ => None,
                };
            }
            _ => {}
        }
    }

    params
}

/// Push current state to the URL query string using `replaceState`.
///
/// `dev` is appended as `&dev=true` only when true; when false the parameter
/// is omitted so off-mode URLs stay clean.
pub fn push_to_url(
    site: &str,
    time: f64,
    product: &str,
    lat: f64,
    lon: f64,
    view: &ViewState,
    dev: bool,
) {
    let v_json = serde_json::to_vec(view).unwrap_or_default();
    let v_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&v_json);

    let mut query = format!(
        "?site={}&t={:.0}&product={}&lat={:.4}&lon={:.4}&v={}",
        site, time, product, lat, lon, v_b64
    );
    if dev {
        query.push_str("&dev=true");
    }

    let window = web_sys::window().expect("no window");
    let history = window.history().expect("no history");
    let _ = history.replace_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(&query));
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn rt_persisted_only_when_caller_reports_attached_live() {
        // The persistence rule (spec §7): `rt=true` (reload re-tethers) is
        // written ONLY while the playhead is attached to the live edge. The
        // caller passes that as `is_live` (`live.app_mode == AppMode::Live`,
        // which requires both a stream session AND an attached playhead), so a
        // detached background stream persists `t` only — no `rt`.
        let state = crate::state::AppState::default();
        let playback = crate::core::PlaybackState::default();

        // Attached live → rt = Some(true).
        let attached = ViewState::from_state(&state, &playback, true);
        assert_eq!(attached.rt, Some(true));

        // Detached (or idle) → rt = None, so reload restores the archive
        // position rather than re-tethering and losing it.
        let detached = ViewState::from_state(&state, &playback, false);
        assert_eq!(detached.rt, None);
    }

    #[wasm_bindgen_test]
    fn url_push_carries_view_fields() {
        let mut state = crate::state::AppState::default();
        let playback = crate::core::PlaybackState::default();
        state.viz_state.site_id = "KDMX".to_string();
        state.dev_mode = true;
        let p = build_url_push(&state, &playback, true);
        assert_eq!(p.site, "KDMX");
        assert!(p.dev);
        // `is_live=true` is encoded into the view blob's `rt` flag.
        assert_eq!(p.view.rt, Some(true));
    }

    #[wasm_bindgen_test]
    fn url_push_carries_product_lat_lon_and_time() {
        // Cover the scalar payload: product short_code, center coords, and the
        // playback position threaded through as `time`.
        let mut state = crate::state::AppState::default();
        let mut playback = crate::core::PlaybackState::default();
        state.viz_state.product = crate::core::RadarProduct::Velocity; // "VEL"
        state.viz_state.center_lat = 41.25;
        state.viz_state.center_lon = -93.75;
        playback.set_playback_position(1_700_000_000.5);
        let p = build_url_push(&state, &playback, false);
        assert_eq!(p.product, "VEL");
        assert_eq!(p.lat, 41.25);
        assert_eq!(p.lon, -93.75);
        assert_eq!(p.time, 1_700_000_000.5);
    }

    #[wasm_bindgen_test]
    fn url_push_view_matches_from_state() {
        // The view blob in the push must be exactly what ViewState::from_state
        // produces for the same (state, playback, is_live) — the doc-comment
        // contract that the whole state → URL mapping lives in one place.
        let state = crate::state::AppState::default();
        let playback = crate::core::PlaybackState::default();
        let is_live = true;
        let p = build_url_push(&state, &playback, is_live);
        assert_eq!(p.view, ViewState::from_state(&state, &playback, is_live));
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    // Mirror the exact codec parse_from_url uses for the `v` parameter.
    fn encode_v(vs: &ViewState) -> String {
        let json = serde_json::to_vec(vs).unwrap();
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&json)
    }
    fn decode_v(s: &str) -> Option<ViewState> {
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(s)
            .ok()?;
        serde_json::from_slice::<ViewState>(&bytes).ok()
    }

    #[wasm_bindgen_test]
    fn camera_mode_code_maps_each_variant() {
        assert_eq!(camera_mode_code(crate::geo::CameraMode::PlanetOrbit), 0);
        assert_eq!(camera_mode_code(crate::geo::CameraMode::SiteOrbit), 1);
        assert_eq!(camera_mode_code(crate::geo::CameraMode::FreeLook), 2);
    }

    #[wasm_bindgen_test]
    fn default_view_state_serializes_to_empty_object() {
        // Every field is `skip_serializing_if = Option::is_none`, so a default
        // ViewState carries no keys — keeps shared URLs minimal.
        let json = serde_json::to_string(&ViewState::default()).unwrap();
        assert_eq!(json, "{}");
    }

    #[wasm_bindgen_test]
    fn view_state_round_trips_through_v_blob_codec() {
        let vs = ViewState {
            mz: Some(1.5),
            tz: Some(4.0),
            vm: Some(1),
            cm: Some(2),
            cd: Some(3.25),
            fp: Some([1.0, 2.0, 3.0]),
            v3d: Some(true),
            rt: Some(true),
            ..Default::default()
        };
        let decoded = decode_v(&encode_v(&vs)).expect("round trip");
        assert_eq!(decoded, vs);
    }

    #[wasm_bindgen_test]
    fn round_trip_preserves_none_fields_as_none() {
        // Fields left None must come back None (not defaulted to Some(0)).
        let vs = ViewState {
            mz: Some(2.0),
            ..Default::default()
        };
        let decoded = decode_v(&encode_v(&vs)).unwrap();
        assert_eq!(decoded.mz, Some(2.0));
        assert_eq!(decoded.tz, None);
        assert_eq!(decoded.cm, None);
        assert_eq!(decoded.rt, None);
    }

    #[wasm_bindgen_test]
    fn malformed_v_blob_is_rejected_not_panicking() {
        // parse_from_url silently ignores a bad `v`; the codec must return None
        // rather than panic on invalid base64 or invalid JSON.
        assert!(decode_v("!!!not base64!!!").is_none());
        // Valid base64 but not valid ViewState JSON.
        let not_json = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b"\xff\xff\xff");
        assert!(decode_v(&not_json).is_none());
    }

    #[wasm_bindgen_test]
    fn from_state_default_sets_core_view_fields() {
        let state = crate::state::AppState::default();
        let playback = crate::core::PlaybackState::default();
        let vs = ViewState::from_state(&state, &playback, false);
        // These are always populated (Some) regardless of live state.
        assert!(vs.mz.is_some());
        assert!(vs.tz.is_some());
        assert!(vs.vm.is_some());
        assert!(vs.cm.is_some());
        assert_eq!(vs.tz, Some(playback.timeline_zoom));
    }
}
