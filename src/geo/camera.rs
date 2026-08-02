//! Camera state machine for the radar view.
//!
//! A single [`Camera`] enum owns every camera state — the flat 2D
//! top-down view plus one unified 3D orbit camera. The orbit camera is
//! Google-Earth-style: it orbits a pivot point on the earth's surface at
//! a distance, with a tilt off the surface normal and a heading about it.
//! Zoomed out it behaves like a planet-scale globe camera; zoomed in it
//! orbits the pivot like a site camera — there is no mode boundary.
//!
//! Controls (wired in `src/ui/canvas_interaction.rs` / `src/ui/shortcuts.rs`):
//! - Left drag: pan the pivot across the globe (grab feel)
//! - Right / Ctrl+left / middle drag: tilt + heading
//! - Scroll: zoom (log-space distance)
//! - Double-click: move the pivot to the clicked point
//! - W/A/S/D: pan the pivot · Q/E: rotate heading · R/F/N: reset/focus/north
//!
//! [`ViewMode`] is a *derived* view of the active variant
//! ([`Camera::view_mode`]); it is not an independent toggle to keep in
//! sync.

use eframe::egui::{Pos2, Rect, Vec2};
use glam::{Mat4, Vec3, Vec4};

/// Map view mode — derived from the active [`Camera`] variant.
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum ViewMode {
    /// Classic flat equirectangular map.
    #[default]
    Flat2D,
    /// 3D globe.
    Globe3D,
}

// Distance clamp range: eye distance above the pivot SURFACE point, in
// Earth radii. Storing surface distance (not center distance) makes the
// zoom range continuous down to the minimum — there is no render-side
// `.max()` floor and therefore no dead zone.
/// Closest approach (~6.4 km above the surface).
const MIN_SURFACE_DIST: f32 = 0.001;
/// Farthest distance (matches the historical 20 Earth radii from center).
const MAX_SURFACE_DIST: f32 = 19.0;

/// Default camera height above the site (~637 km). Provides a view
/// comparable to the 2D flat view's ~500 km radius.
const DEFAULT_SITE_SURFACE_DIST: f32 = 0.10;

/// Tilt clamp, degrees off the surface normal. 0 = straight down; the cap
/// keeps the pivot in front of the camera (no zenith crossing possible).
const MAX_TILT_DEG: f32 = 85.0;

/// Default vertical field-of-view (radians) for the 3D view.
const DEFAULT_FOV_Y: f32 = std::f32::consts::FRAC_PI_4; // 45°

/// Held-key heading rotation rate (degrees per second).
const KEY_ROTATE_DEG_PER_SEC: f32 = 60.0;

/// Held-key pivot pan rate: degrees per second per Earth radius of
/// altitude, capped at 1 radius so planet-scale panning stays controllable.
const KEY_PAN_DEG_PER_SEC_PER_RADIUS: f32 = 120.0;

/// Above this tilt the cursor ray grazes the surface near the horizon,
/// where cursor-anchored zoom / grab panning become ill-conditioned —
/// both fall back to their centered/delta variants.
const GRAB_MAX_TILT_DEG: f32 = 60.0;

/// Fly-to time constant (seconds): the exponential approach covers ~63%
/// of the remaining distance per TAU, so transitions settle in ~1 s.
const FLY_TO_TAU_SECS: f32 = 0.25;

/// Shared 2D + 3D scroll feel: log-space zoom change per scroll unit
/// (~+27% per 120-unit wheel tick). The 2D canvas handler uses the same
/// constant so the wheel feels identical in both views.
pub(crate) const ZOOM_LOG_PER_SCROLL_UNIT: f32 = 0.002;

// ── Flat 2D ─────────────────────────────────────────────────────────

/// State for the flat 2D top-down view.
///
/// Owns the pan/zoom that previously lived on `viz_state`. The actual
/// equirectangular projection math lives in
/// [`MapProjection`](crate::geo::projection::MapProjection), which is
/// rebuilt each frame from these values + the site center + the canvas
/// rect; this struct is purely the user-controlled view state.
#[derive(Clone, Copy, PartialEq)]
pub struct Flat2DState {
    /// Current zoom level (1.0 = 100%).
    pub zoom: f32,
    /// Pan offset from center, in screen pixels.
    pub pan_offset: Vec2,
}

impl Default for Flat2DState {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            pan_offset: Vec2::ZERO,
        }
    }
}

/// The flat 2D view plus the carried 3D camera.
///
/// 2D itself only needs pan/zoom, but the full orbit camera is carried as
/// `saved` so a 2D excursion round-trips the exact 3D view: switching
/// 2D → 3D restores `saved` verbatim. Site changes while in 2D update
/// `saved` in place (see [`Camera::center_on`]).
#[derive(Clone, Copy)]
pub struct Flat2D {
    pub view: Flat2DState,
    pub saved: OrbitState,
}

// ── 3D orbit camera ─────────────────────────────────────────────────

/// Frustum + radar-site state shared by the 3D camera.
///
/// `fov_y`/`aspect` define the perspective frustum; `site_lat`/`site_lon`
/// track the radar site (the focus/reset target).
#[derive(Clone, Copy)]
pub struct Globe3DCommon {
    /// Vertical field-of-view in radians.
    pub fov_y: f32,
    /// Viewport aspect ratio (width / height), updated each frame.
    pub aspect: f32,
    /// Site latitude (degrees).
    pub site_lat: f32,
    /// Site longitude (degrees).
    pub site_lon: f32,
}

impl Globe3DCommon {
    /// Common 3D state centered on the given site.
    fn centered_on(lat_deg: f64, lon_deg: f64) -> Self {
        Self {
            fov_y: DEFAULT_FOV_Y,
            aspect: 1.0,
            site_lat: lat_deg as f32,
            site_lon: lon_deg as f32,
        }
    }
}

/// The unified Google-Earth-style orbit camera.
///
/// The camera orbits a pivot point on the earth's surface. The defining
/// invariant (test-enforced): the pivot projects to the exact screen
/// center for every tilt/heading/distance.
#[derive(Clone, Copy)]
pub struct OrbitState {
    pub common: Globe3DCommon,
    /// Surface pivot latitude (degrees), clamped to ±89.9.
    pub pivot_lat: f32,
    /// Surface pivot longitude (degrees), wrapped to ±180.
    pub pivot_lon: f32,
    /// Eye distance above the pivot surface point, in Earth radii.
    /// Clamped to `[MIN_SURFACE_DIST, MAX_SURFACE_DIST]`.
    pub distance: f32,
    /// Pitch off the pivot's surface normal, degrees. 0 = straight down,
    /// clamped to `[0, MAX_TILT_DEG]`.
    pub tilt: f32,
    /// View rotation about the pivot's surface normal, degrees.
    /// 0 = north up, positive = view rotates clockwise on screen.
    /// Wrapped to ±180.
    pub heading: f32,
    /// Active fly-to target while a transition is animating (see
    /// [`Camera::fly_to`] / [`Camera::tick_animation`]). Any direct input
    /// cancels it so the camera never fights the user.
    pub anim: Option<OrbitTarget>,
}

impl OrbitState {
    /// A default orbit camera hovering straight above the given site.
    fn over_site(common: Globe3DCommon) -> Self {
        Self {
            pivot_lat: common.site_lat,
            pivot_lon: common.site_lon,
            distance: DEFAULT_SITE_SURFACE_DIST,
            tilt: 0.0,
            heading: 0.0,
            anim: None,
            common,
        }
    }

    /// The pose a share-link or a completed animation should land on: the
    /// fly-to target while one is in flight, else the live pose.
    fn target_pose(&self) -> OrbitTarget {
        self.anim.unwrap_or(OrbitTarget {
            pivot_lat: self.pivot_lat,
            pivot_lon: self.pivot_lon,
            distance: self.distance,
            tilt: self.tilt,
            heading: self.heading,
        })
    }
}

/// A destination pose for an animated camera transition.
#[derive(Clone, Copy)]
pub struct OrbitTarget {
    pub pivot_lat: f32,
    pub pivot_lon: f32,
    pub distance: f32,
    pub tilt: f32,
    pub heading: f32,
}

/// The camera state machine: the flat 2D view or the 3D orbit camera.
#[derive(Clone)]
pub enum Camera {
    Flat2D(Flat2D),
    Orbit(OrbitState),
}

impl Default for Camera {
    fn default() -> Self {
        // Matches the historical default: 2D view, camera seeded as if
        // centered on the equator/prime-meridian. Callers immediately
        // `center_on` the active site, so the seed is rarely observed.
        Camera::centered_on(0.0, 0.0)
    }
}

// ── Free-standing 3D math ───────────────────────────────────────────

/// Convert geographic (lat°, lon°) to a point on the unit sphere.
fn geo_to_world(lat_deg: f64, lon_deg: f64) -> Vec3 {
    let lat = (lat_deg as f32).to_radians();
    let lon = (lon_deg as f32).to_radians();
    Vec3::new(lat.cos() * lon.sin(), lat.sin(), lat.cos() * lon.cos())
}

/// East/north tangent basis at a unit-sphere point with radial `up`.
/// Handles pole degeneracy by switching the reference vector.
fn enu_basis(up: Vec3) -> (Vec3, Vec3) {
    let ref_vec = if up.y.abs() > 0.99 { Vec3::Z } else { Vec3::Y };
    let east = ref_vec.cross(up).normalize();
    let north = up.cross(east).normalize();
    (east, north)
}

/// Eye position, pivot point, and camera-up vector for an orbit state —
/// the single geometric core shared by the view matrix and
/// `camera_world_pos`.
fn orbit_eye_pivot_up(s: &OrbitState) -> (Vec3, Vec3, Vec3) {
    let pivot = geo_to_world(s.pivot_lat as f64, s.pivot_lon as f64);
    let up = pivot; // radial surface normal (unit sphere)
    let (_east, north) = enu_basis(up);
    let tilt = s.tilt.to_radians();
    let heading = s.heading.to_radians();
    // Positive heading rotates the *view* clockwise on screen, which
    // means the camera swings counter-clockwise around the pivot.
    let spin = glam::Quat::from_axis_angle(up, -heading);
    // tilt = 0 puts the eye straight above the pivot; tilting swings it
    // toward "south of the view" so the view looks toward heading-north.
    let offset_dir = spin * (up * tilt.cos() - north * tilt.sin());
    let cam_up = spin * (north * tilt.cos() + up * tilt.sin());
    let eye = pivot + offset_dir * s.distance;
    (eye, pivot, cam_up)
}

/// View matrix for the orbit camera. The pivot is always the look-at
/// target, so it projects to the exact screen center.
fn orbit_view_matrix(s: &OrbitState) -> Mat4 {
    let (eye, pivot, cam_up) = orbit_eye_pivot_up(s);
    Mat4::look_at_rh(eye, pivot, cam_up)
}

/// Near-plane distance for a given surface distance: proportional to
/// altitude (no z-fighting up close) and continuous (no pop at a
/// threshold), clamped to a sane range.
fn near_plane_for(surface_distance: f32) -> f32 {
    (surface_distance * 0.1).clamp(2e-5, 0.01)
}

/// Exponential (log-space) zoom step applied to a surface distance, with
/// clamps. Positive `delta` zooms in (closer). Each scroll unit moves a
/// consistent percentage of the altitude.
fn zoom_distance(distance: f32, delta: f32) -> f32 {
    let new_log = distance.ln() - delta * ZOOM_LOG_PER_SCROLL_UNIT;
    new_log
        .clamp(MIN_SURFACE_DIST.ln(), MAX_SURFACE_DIST.ln())
        .exp()
}

/// Clamp a pivot latitude away from the poles (avoids gimbal flip).
fn clamp_lat(lat: f32) -> f32 {
    lat.clamp(-89.9, 89.9)
}

/// Wrap an angle to the ±180° range.
fn wrap_deg(mut deg: f32) -> f32 {
    while deg > 180.0 {
        deg -= 360.0;
    }
    while deg < -180.0 {
        deg += 360.0;
    }
    deg
}

impl Camera {
    // ── Construction ────────────────────────────────────────────────

    /// Create a camera centered on the given geographic coordinates.
    ///
    /// Starts in the flat 2D view (the historical default view mode) with
    /// the saved 3D camera hovering above the site, so a later switch to
    /// 3D is centered correctly.
    pub fn centered_on(lat_deg: f64, lon_deg: f64) -> Self {
        Camera::Flat2D(Flat2D {
            view: Flat2DState::default(),
            saved: OrbitState::over_site(Globe3DCommon::centered_on(lat_deg, lon_deg)),
        })
    }

    // ── Derived view mode ───────────────────────────────────────────

    /// The [`ViewMode`] derived from the active variant. This is the
    /// single source of truth — there is no separately-stored toggle to
    /// keep in sync.
    pub fn view_mode(&self) -> ViewMode {
        match self {
            Camera::Flat2D(_) => ViewMode::Flat2D,
            Camera::Orbit(_) => ViewMode::Globe3D,
        }
    }

    /// Whether the camera is in the flat 2D view.
    pub fn is_2d(&self) -> bool {
        matches!(self, Camera::Flat2D(_))
    }

    /// The flat 2D view state (pan/zoom), if active.
    pub fn flat_2d(&self) -> Option<&Flat2DState> {
        match self {
            Camera::Flat2D(f) => Some(&f.view),
            Camera::Orbit(_) => None,
        }
    }

    /// Mutable flat 2D view state, if active.
    pub fn flat_2d_mut(&mut self) -> Option<&mut Flat2DState> {
        match self {
            Camera::Flat2D(f) => Some(&mut f.view),
            Camera::Orbit(_) => None,
        }
    }

    /// The active orbit state, if the 3D view is active.
    pub fn orbit_state(&self) -> Option<&OrbitState> {
        match self {
            Camera::Flat2D(_) => None,
            Camera::Orbit(s) => Some(s),
        }
    }

    /// The live orbit state in 3D, or the saved one in 2D.
    fn orbit_or_saved_mut(&mut self) -> &mut OrbitState {
        match self {
            Camera::Flat2D(f) => &mut f.saved,
            Camera::Orbit(s) => s,
        }
    }

    fn orbit_or_saved(&self) -> &OrbitState {
        match self {
            Camera::Flat2D(f) => &f.saved,
            Camera::Orbit(s) => s,
        }
    }

    /// Shared 3D state (frustum + site). Live on the orbit variant, or
    /// the carried camera in 2D — so the site survives a 2D excursion.
    fn common(&self) -> &Globe3DCommon {
        &self.orbit_or_saved().common
    }

    fn common_mut(&mut self) -> &mut Globe3DCommon {
        &mut self.orbit_or_saved_mut().common
    }

    // ── Matrices ────────────────────────────────────────────────────

    /// View matrix (world → eye). Identity for the 2D variant (the 2D path
    /// never reads it; it uses [`MapProjection`](crate::geo::projection::MapProjection)).
    pub fn view_matrix(&self) -> Mat4 {
        match self {
            Camera::Flat2D(_) => Mat4::IDENTITY,
            Camera::Orbit(s) => orbit_view_matrix(s),
        }
    }

    /// Perspective projection matrix.
    pub fn projection_matrix(&self) -> Mat4 {
        let (fov_y, aspect, surface_dist) = match self {
            // 2D never paints through this matrix; return a benign frustum.
            Camera::Flat2D(_) => (DEFAULT_FOV_Y, 1.0, DEFAULT_SITE_SURFACE_DIST),
            Camera::Orbit(s) => (s.common.fov_y, s.common.aspect, s.distance),
        };
        Mat4::perspective_rh_gl(fov_y, aspect, near_plane_for(surface_dist), 100.0)
    }

    /// Combined view-projection matrix.
    pub fn view_projection(&self) -> Mat4 {
        self.projection_matrix() * self.view_matrix()
    }

    // ── Coordinate conversions ──────────────────────────────────────

    /// Convert geographic (lat°, lon°) to a point on the unit sphere.
    pub fn geo_to_world(lat_deg: f64, lon_deg: f64) -> Vec3 {
        geo_to_world(lat_deg, lon_deg)
    }

    /// Camera position in world space. Meaningful only in 3D; the 2D
    /// variant returns the historical default straight above the equator.
    pub fn camera_world_pos(&self) -> Vec3 {
        match self {
            Camera::Flat2D(_) => Vec3::new(0.0, 0.0, 1.0 + DEFAULT_SITE_SURFACE_DIST),
            Camera::Orbit(s) => orbit_eye_pivot_up(s).0,
        }
    }

    /// Project a 3D world position to screen coordinates.
    /// Returns `None` if the point is on the far side of the globe.
    pub fn world_to_screen(&self, pos: Vec3, screen_rect: Rect) -> Option<Pos2> {
        // Back-face test: point must face the camera.
        let cam_pos = self.camera_world_pos();
        let to_cam = (cam_pos - pos).normalize();
        if to_cam.dot(pos.normalize()) < -0.01 {
            return None; // on far side
        }

        let vp = self.view_projection();
        let clip = vp * Vec4::new(pos.x, pos.y, pos.z, 1.0);
        if clip.w <= 0.0 {
            return None;
        }
        let ndc = clip.truncate() / clip.w;

        // NDC (-1..1) → screen pixels
        let sx = screen_rect.center().x + ndc.x * screen_rect.width() * 0.5;
        let sy = screen_rect.center().y - ndc.y * screen_rect.height() * 0.5; // flip Y
        Some(Pos2::new(sx, sy))
    }

    /// Project geographic (lat°, lon°) to screen. Convenience wrapper.
    pub fn geo_to_screen(&self, lat_deg: f64, lon_deg: f64, screen_rect: Rect) -> Option<Pos2> {
        self.world_to_screen(geo_to_world(lat_deg, lon_deg), screen_rect)
    }

    /// Ray-sphere intersection: screen position → geographic (lat°, lon°).
    /// Returns `None` if the ray misses the globe.
    pub fn screen_to_geo(&self, pos: Pos2, screen_rect: Rect) -> Option<(f64, f64)> {
        // Screen → NDC
        let ndc_x = (pos.x - screen_rect.center().x) / (screen_rect.width() * 0.5);
        let ndc_y = -(pos.y - screen_rect.center().y) / (screen_rect.height() * 0.5);

        // Unproject through inverse VP
        let inv_vp = self.view_projection().inverse();
        let near = inv_vp * Vec4::new(ndc_x, ndc_y, -1.0, 1.0);
        let far = inv_vp * Vec4::new(ndc_x, ndc_y, 1.0, 1.0);
        let near = near.truncate() / near.w;
        let far = far.truncate() / far.w;

        let ray_origin = near;
        let ray_dir = (far - near).normalize();

        // Intersect with unit sphere
        let a = ray_dir.dot(ray_dir);
        let b = 2.0 * ray_origin.dot(ray_dir);
        let c = ray_origin.dot(ray_origin) - 1.0;
        let discriminant = b * b - 4.0 * a * c;
        if discriminant < 0.0 {
            return None;
        }
        let t = (-b - discriminant.sqrt()) / (2.0 * a);
        if t < 0.0 {
            return None;
        }

        let hit = ray_origin + ray_dir * t;
        // Convert unit-sphere point → lat/lon
        let lat = hit.y.asin().to_degrees() as f64;
        let lon = hit.x.atan2(hit.z).to_degrees() as f64;
        Some((lat, lon))
    }

    // ── Controls ────────────────────────────────────────────────────

    /// Pan the pivot across the globe by a screen-space drag delta
    /// (pixels). Grab feel: dragging right moves the globe right (the
    /// pivot moves toward screen-left). Heading-aware — dragging up always
    /// moves the pivot toward screen-up regardless of rotation — and
    /// distance-scaled so close-up panning stays precise. No-op in 2D.
    pub fn pan_pivot(&mut self, dx: f32, dy: f32, viewport_height: f32) {
        let Camera::Orbit(s) = self else {
            return;
        };
        s.anim = None; // direct input cancels any fly-to
        let deg_per_px =
            s.common.fov_y / viewport_height * (180.0 / std::f32::consts::PI) * s.distance;
        let h = s.heading.to_radians();
        // Screen-up corresponds to compass bearing `heading`; decompose
        // the drag into north/east components of pivot motion. Dragging
        // down (dy > 0) moves the pivot toward screen-up.
        let dnorth = (dx * h.sin() + dy * h.cos()) * deg_per_px;
        let deast = (-dx * h.cos() + dy * h.sin()) * deg_per_px;

        s.pivot_lat = clamp_lat(s.pivot_lat + dnorth);
        // Scale longitude motion so panning speed is isotropic at high
        // latitudes (a degree of longitude shrinks by cos(lat)).
        let coslat = s.pivot_lat.to_radians().cos().max(0.01);
        s.pivot_lon = wrap_deg(s.pivot_lon + deast / coslat);
    }

    /// Adjust tilt and heading by a screen-space drag delta. Horizontal
    /// drag rotates the heading; dragging up tilts toward the horizon.
    /// No-op in 2D.
    pub fn adjust_tilt_heading(&mut self, dx: f32, dy: f32, viewport_height: f32) {
        let Camera::Orbit(s) = self else {
            return;
        };
        s.anim = None; // direct input cancels any fly-to
        let deg_per_px = s.common.fov_y / viewport_height * (180.0 / std::f32::consts::PI);
        s.heading = wrap_deg(s.heading + dx * deg_per_px);
        s.tilt = (s.tilt - dy * deg_per_px).clamp(0.0, MAX_TILT_DEG);
    }

    /// Zoom by scroll delta. Positive = zoom in (closer). No-op in 2D
    /// (the 2D path handles its own zoom on `Flat2DState`).
    pub fn zoom(&mut self, delta: f32) {
        if let Camera::Orbit(s) = self {
            s.anim = None; // direct input cancels any fly-to
            s.distance = zoom_distance(s.distance, delta);
        }
    }

    /// Cursor-anchored zoom: like [`Self::zoom`], but slides the pivot
    /// toward the surface point under `cursor` so that point stays put on
    /// screen while zooming in — the same anchoring the 2D view does.
    /// Falls back to a plain centered zoom when the cursor is absent,
    /// misses the globe, or the view is tilted near the horizon (where
    /// the flat-earth re-anchoring linearization is poor). No-op in 2D.
    pub fn zoom_about(&mut self, delta: f32, cursor: Option<Pos2>, screen_rect: Rect) {
        let anchor = cursor
            .filter(|_| {
                self.orbit_state()
                    .is_some_and(|s| s.tilt <= GRAB_MAX_TILT_DEG)
            })
            .and_then(|pos| self.screen_to_geo(pos, screen_rect));
        let Camera::Orbit(s) = self else {
            return;
        };
        s.anim = None; // direct input cancels any fly-to
        let old_dist = s.distance;
        s.distance = zoom_distance(old_dist, delta);
        if let Some((alat, alon)) = anchor {
            // Exact in the flat-earth limit: the pivot's offset from the
            // anchor shrinks by the same ratio as the distance.
            let ratio = s.distance / old_dist;
            let alat = alat as f32;
            let alon = alon as f32;
            s.pivot_lat = clamp_lat(alat + (s.pivot_lat - alat) * ratio);
            s.pivot_lon = wrap_deg(alon + wrap_deg(s.pivot_lon - alon) * ratio);
        }
    }

    /// Grab-style pan: shift the pivot so the surface point that was under
    /// `from` lands under `to` (the globe sticks to the cursor). Returns
    /// false — with no state change — when either screen point misses the
    /// globe or the view is tilted near the horizon; the caller falls back
    /// to the delta-based [`Self::pan_pivot`].
    pub fn pan_pivot_grab(&mut self, from: Pos2, to: Pos2, screen_rect: Rect) -> bool {
        if !self
            .orbit_state()
            .is_some_and(|s| s.tilt <= GRAB_MAX_TILT_DEG)
        {
            return false;
        }
        let Some((from_lat, from_lon)) = self.screen_to_geo(from, screen_rect) else {
            return false;
        };
        let Some((to_lat, to_lon)) = self.screen_to_geo(to, screen_rect) else {
            return false;
        };
        let Camera::Orbit(s) = self else {
            return false;
        };
        s.anim = None; // direct input cancels any fly-to
        s.pivot_lat = clamp_lat(s.pivot_lat + (from_lat - to_lat) as f32);
        s.pivot_lon = wrap_deg(s.pivot_lon + wrap_deg((from_lon - to_lon) as f32));
        true
    }

    /// Center the camera on the given lat/lon and reset the view.
    ///
    /// Records the new site and resets the orbit camera to hover above
    /// it. In 2D this updates the *saved* camera (the 2D projection
    /// re-centers off `viz_state.center_lat/lon`); switching to 3D later
    /// picks it up.
    pub fn center_on(&mut self, lat_deg: f64, lon_deg: f64) {
        let s = self.orbit_or_saved_mut();
        s.common.site_lat = lat_deg as f32;
        s.common.site_lon = lon_deg as f32;
        *s = OrbitState::over_site(s.common);
    }

    /// Reset the 3D camera to the default view above the site (animated).
    /// R key handler (the 2D branch of R is handled by the shortcut itself).
    pub fn reset(&mut self) {
        if let Camera::Orbit(s) = self {
            let d = OrbitState::over_site(s.common);
            self.fly_to(OrbitTarget {
                pivot_lat: d.pivot_lat,
                pivot_lon: d.pivot_lon,
                distance: d.distance,
                tilt: d.tilt,
                heading: d.heading,
            });
        }
    }

    /// Fly the pivot back to the radar site, keeping distance, tilt and
    /// heading. F key handler.
    pub fn focus_site(&mut self) {
        if let Camera::Orbit(s) = self {
            let mut target = s.target_pose();
            target.pivot_lat = s.common.site_lat;
            target.pivot_lon = s.common.site_lon;
            self.fly_to(target);
        }
    }

    /// Rotate the view so North is up (animated), keeping tilt. N key
    /// handler.
    pub fn align_north(&mut self) {
        if let Camera::Orbit(s) = self {
            let mut target = s.target_pose();
            target.heading = 0.0;
            self.fly_to(target);
        }
    }

    /// Fly the pivot to a specific geographic point. Double-click handler.
    pub fn move_pivot_to(&mut self, lat_deg: f64, lon_deg: f64) {
        if let Camera::Orbit(s) = self {
            let mut target = s.target_pose();
            target.pivot_lat = lat_deg as f32;
            target.pivot_lon = lon_deg as f32;
            self.fly_to(target);
        }
    }

    // ── Fly-to animation ────────────────────────────────────────────

    /// Begin an animated transition toward `target` (clamped through the
    /// same helpers the live controls use). No-op in 2D.
    pub fn fly_to(&mut self, target: OrbitTarget) {
        if let Camera::Orbit(s) = self {
            s.anim = Some(OrbitTarget {
                pivot_lat: clamp_lat(target.pivot_lat),
                pivot_lon: wrap_deg(target.pivot_lon),
                distance: target.distance.clamp(MIN_SURFACE_DIST, MAX_SURFACE_DIST),
                tilt: target.tilt.clamp(0.0, MAX_TILT_DEG),
                heading: wrap_deg(target.heading),
            });
        }
    }

    /// Advance an active fly-to by `dt` seconds: an exponential
    /// (critically-damped first-order) approach toward the target, with
    /// distance interpolated in log space (constant perceived zoom rate)
    /// and longitude/heading along the shortest arc. Snaps and clears the
    /// target once every delta is below threshold. Returns true while a
    /// transition is still animating (the shell requests a repaint).
    pub fn tick_animation(&mut self, dt: f32) -> bool {
        let Camera::Orbit(s) = self else {
            return false;
        };
        let Some(t) = s.anim else {
            return false;
        };
        let alpha = 1.0 - (-dt.max(0.0) / FLY_TO_TAU_SECS).exp();
        s.pivot_lat += (t.pivot_lat - s.pivot_lat) * alpha;
        s.pivot_lon = wrap_deg(s.pivot_lon + wrap_deg(t.pivot_lon - s.pivot_lon) * alpha);
        s.heading = wrap_deg(s.heading + wrap_deg(t.heading - s.heading) * alpha);
        s.tilt += (t.tilt - s.tilt) * alpha;
        let log_dist = s.distance.ln() + (t.distance.ln() - s.distance.ln()) * alpha;
        s.distance = log_dist.exp();

        let arrived = (t.pivot_lat - s.pivot_lat).abs() < 0.005
            && wrap_deg(t.pivot_lon - s.pivot_lon).abs() < 0.005
            && wrap_deg(t.heading - s.heading).abs() < 0.01
            && (t.tilt - s.tilt).abs() < 0.01
            && (t.distance.ln() - s.distance.ln()).abs() < 0.001;
        if arrived {
            s.pivot_lat = t.pivot_lat;
            s.pivot_lon = t.pivot_lon;
            s.heading = t.heading;
            s.tilt = t.tilt;
            s.distance = t.distance;
            s.anim = None;
        }
        true
    }

    /// Handle held-key movement. Returns true if any state changed.
    /// `forward`: +1 = W (pivot toward screen-up), -1 = S
    /// `right`: +1 = D (pivot toward screen-right), -1 = A
    /// `rotate`: +1 = E (heading clockwise), -1 = Q
    /// `speed_mult`: 2.0 for Shift, 0.25 for Ctrl, 1.0 otherwise.
    /// `dt`: frame delta time in seconds.
    pub fn keyboard_move(
        &mut self,
        forward: f32,
        right: f32,
        rotate: f32,
        speed_mult: f32,
        dt: f32,
    ) -> bool {
        let Camera::Orbit(s) = self else {
            return false;
        };
        if forward == 0.0 && right == 0.0 && rotate == 0.0 {
            return false;
        }
        s.anim = None; // direct input cancels any fly-to

        if forward != 0.0 || right != 0.0 {
            // Pan the pivot, heading-aware and distance-scaled (capped so
            // planet-scale panning stays controllable).
            let rate = KEY_PAN_DEG_PER_SEC_PER_RADIUS * s.distance.min(1.0) * speed_mult * dt;
            let h = s.heading.to_radians();
            let dnorth = (forward * h.cos() - right * h.sin()) * rate;
            let deast = (forward * h.sin() + right * h.cos()) * rate;
            s.pivot_lat = clamp_lat(s.pivot_lat + dnorth);
            let coslat = s.pivot_lat.to_radians().cos().max(0.01);
            s.pivot_lon = wrap_deg(s.pivot_lon + deast / coslat);
        }
        if rotate != 0.0 {
            s.heading = wrap_deg(s.heading + rotate * KEY_ROTATE_DEG_PER_SEC * speed_mult * dt);
        }
        true
    }

    /// Update the site position without moving the camera view.
    pub fn set_site(&mut self, lat_deg: f64, lon_deg: f64) {
        let c = self.common_mut();
        c.site_lat = lat_deg as f32;
        c.site_lon = lon_deg as f32;
    }

    /// Update aspect ratio from the current viewport.
    pub fn set_aspect(&mut self, screen_rect: Rect) {
        let w = screen_rect.width();
        let h = screen_rect.height();
        if h > 0.0 {
            self.common_mut().aspect = w / h;
        }
    }

    // ── Mode transitions ────────────────────────────────────────────

    /// Switch to the 3D globe, restoring the saved orbit camera (a fresh
    /// session's saved camera hovers above the site). No-op if already 3D.
    pub fn switch_to_globe(&mut self) {
        if let Camera::Flat2D(f) = self {
            *self = Camera::Orbit(f.saved);
        }
    }

    /// Switch to the flat 2D view with the given 2D view state. The live
    /// orbit camera is carried as `saved` so a later switch back to 3D
    /// restores the exact view; an in-flight fly-to is completed (the
    /// destination is where the user intended to be). No-op if already 2D.
    pub fn switch_to_flat_2d(&mut self, state: Flat2DState) {
        if let Camera::Orbit(s) = self {
            let t = s.target_pose();
            let saved = OrbitState {
                common: s.common,
                pivot_lat: t.pivot_lat,
                pivot_lon: t.pivot_lon,
                distance: t.distance,
                tilt: t.tilt,
                heading: t.heading,
                anim: None,
            };
            *self = Camera::Flat2D(Flat2D { view: state, saved });
        }
    }
}

// ── URL persistence ─────────────────────────────────────────────────

/// Flattened camera values for URL persistence.
///
/// `distance` stays in the historical center-distance convention
/// (`surface + 1.0`) on the wire so pre-overhaul links restore exactly;
/// the codec converts at this boundary. `rotation` carries the heading
/// (the historical `cr` wire field).
#[derive(Clone, Copy)]
pub struct UrlCameraSnapshot {
    /// Camera distance from the globe *center*, in Earth radii.
    pub distance: f32,
    pub center_lat: f32,
    pub center_lon: f32,
    pub tilt: f32,
    pub rotation: f32,
}

impl Default for UrlCameraSnapshot {
    fn default() -> Self {
        Self {
            distance: 1.0 + DEFAULT_SITE_SURFACE_DIST,
            center_lat: 0.0,
            center_lon: 0.0,
            tilt: 0.0,
            rotation: 0.0,
        }
    }
}

impl Camera {
    /// Flatten the camera state for URL persistence. In 2D the saved
    /// orbit camera is snapshotted so a reloaded 2D link still restores
    /// the last 3D view on a 2D → 3D toggle. An in-flight fly-to
    /// snapshots its *target* — a shared link captures the destination.
    pub fn url_snapshot(&self) -> UrlCameraSnapshot {
        let t = self.orbit_or_saved().target_pose();
        UrlCameraSnapshot {
            distance: 1.0 + t.distance,
            center_lat: t.pivot_lat,
            center_lon: t.pivot_lon,
            tilt: t.tilt,
            rotation: t.heading,
        }
    }

    /// Reconstruct the 3D orbit camera from a saved URL snapshot, on a
    /// base camera that already has the site centered (via
    /// [`Camera::centered_on`]). All fields are clamped through the same
    /// helpers the live controls use.
    pub fn restore_from_url(&mut self, snap: &UrlCameraSnapshot) {
        let common = *self.common();
        *self = Camera::Orbit(OrbitState {
            common,
            pivot_lat: clamp_lat(snap.center_lat),
            pivot_lon: wrap_deg(snap.center_lon),
            distance: (snap.distance - 1.0).clamp(MIN_SURFACE_DIST, MAX_SURFACE_DIST),
            tilt: snap.tilt.clamp(0.0, MAX_TILT_DEG),
            heading: wrap_deg(snap.rotation),
            anim: None,
        });
    }

    /// Restore the 3D camera from raw URL view fields, mapping legacy
    /// per-mode links (`cm` = 0 PlanetOrbit / 1 SiteOrbit / 2 FreeLook)
    /// onto the unified orbit camera. New-format links carry no `cm` and
    /// restore directly. Absent fields fall back to the default view over
    /// the already-centered site.
    pub fn restore_from_url_fields(&mut self, f: &UrlOrbitFields) {
        let site_lat = self.common().site_lat;
        let site_lon = self.common().site_lon;
        let distance = f.cd.unwrap_or(1.0 + DEFAULT_SITE_SURFACE_DIST);
        let snap = match f.cm {
            // Legacy FreeLook: no meaningful orbit equivalent — restore the
            // default view over the site (documented degradation).
            Some(2) => UrlCameraSnapshot {
                distance: 1.0 + DEFAULT_SITE_SURFACE_DIST,
                center_lat: site_lat,
                center_lon: site_lon,
                tilt: 0.0,
                rotation: 0.0,
            },
            // Legacy SiteOrbit: the camera sat at bearing `ob` looking
            // toward the site, so the view heading is ob − 180; elevation
            // `oe` (90 = overhead) becomes tilt off vertical.
            Some(1) => UrlCameraSnapshot {
                distance,
                center_lat: f.clat.unwrap_or(site_lat),
                center_lon: f.clon.unwrap_or(site_lon),
                tilt: 90.0 - f.oe.unwrap_or(45.0),
                rotation: wrap_deg(f.ob.unwrap_or(180.0) - 180.0),
            },
            // Legacy PlanetOrbit (cm = 0) and new-format links (no cm):
            // pivot/heading map directly; legacy planet tilt was a
            // view-space pitch, so magnitude is the closest analogue.
            _ => UrlCameraSnapshot {
                distance,
                center_lat: f.clat.unwrap_or(site_lat),
                center_lon: f.clon.unwrap_or(site_lon),
                tilt: f.ct.unwrap_or(0.0).abs(),
                rotation: f.cr.unwrap_or(0.0),
            },
        };
        self.restore_from_url(&snap);
    }
}

/// Raw camera fields parsed from a URL `v` blob, including the legacy
/// per-mode fields written by pre-overhaul builds. The mapping onto the
/// unified camera lives in [`Camera::restore_from_url_fields`].
#[derive(Clone, Copy, Default)]
pub struct UrlOrbitFields {
    /// Legacy camera mode: 0 = PlanetOrbit, 1 = SiteOrbit, 2 = FreeLook.
    pub cm: Option<u8>,
    /// Distance from the globe center (Earth radii).
    pub cd: Option<f32>,
    /// Pivot latitude (degrees).
    pub clat: Option<f32>,
    /// Pivot longitude (degrees).
    pub clon: Option<f32>,
    /// Tilt (degrees).
    pub ct: Option<f32>,
    /// Heading / rotation (degrees).
    pub cr: Option<f32>,
    /// Legacy site-orbit bearing (degrees).
    pub ob: Option<f32>,
    /// Legacy site-orbit elevation above horizon (degrees).
    pub oe: Option<f32>,
}

/// 3D adapter implementing [`crate::geo::projection::Projection`].
///
/// `Camera`'s `geo_to_screen` / `screen_to_geo` need a `screen_rect` to
/// project against, but the [`Projection`](crate::geo::projection::Projection)
/// trait can't take one as a parameter because
/// [`MapProjection`](crate::geo::projection::MapProjection) stores its own.
/// This wrapper binds a camera + rect together so callers that want a
/// uniform `&dyn Projection` can pass it.
///
/// 2D-only overlays (sites, alerts, scale bar) call sites that take
/// `&dyn Projection` today; the 3D side is wired through this wrapper for
/// the future hybrid path or 3D overlays that may want to share the same
/// call sites.
#[allow(dead_code)] // Doc above: 3D overlays don't share the &dyn Projection call sites yet.
pub(crate) struct GlobeProjection<'a> {
    pub camera: &'a Camera,
    pub screen_rect: Rect,
}

impl crate::geo::projection::Projection for GlobeProjection<'_> {
    fn geo_to_screen(&self, lat: f64, lon: f64) -> Option<Pos2> {
        self.camera.geo_to_screen(lat, lon, self.screen_rect)
    }

    fn screen_to_geo(&self, pos: Pos2) -> Option<(f64, f64)> {
        self.camera.screen_to_geo(pos, self.screen_rect)
    }

    fn visible_bounds(&self) -> Option<(f64, f64, f64, f64)> {
        // 3D: the whole globe is potentially visible, and an axis-aligned
        // lon/lat bbox isn't a useful hit-test bound at planet scale.
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// An orbit camera over the continental US with a fixed screen rect.
    fn orbit_with_rect() -> (Camera, Rect) {
        let rect = Rect::from_min_size(Pos2::new(0.0, 0.0), eframe::egui::Vec2::new(800.0, 600.0));
        let mut c = Camera::centered_on(39.0, -98.0);
        c.set_aspect(rect);
        c.switch_to_globe();
        c.orbit_state().expect("switch_to_globe must enter Orbit");
        (c, rect)
    }

    fn orbit(c: &Camera) -> &OrbitState {
        c.orbit_state().expect("camera must be in 3D")
    }

    /// Run any active fly-to to completion with fixed dt steps.
    fn settle(c: &mut Camera) {
        for _ in 0..500 {
            if !c.tick_animation(0.05) {
                return;
            }
        }
        panic!("fly-to did not converge within 500 ticks");
    }

    #[wasm_bindgen_test]
    fn default_is_flat_2d_and_centered_on_seeds_site() {
        let c = Camera::centered_on(39.0, -98.0);
        assert_eq!(c.view_mode(), ViewMode::Flat2D);
        assert!(c.is_2d());
        let Camera::Flat2D(f) = &c else {
            unreachable!()
        };
        assert!((f.saved.common.site_lat - 39.0).abs() < 1e-4);
        assert!((f.saved.common.site_lon - -98.0).abs() < 1e-4);
        assert!((f.saved.pivot_lat - 39.0).abs() < 1e-4);
        assert!((f.saved.distance - 0.10).abs() < 1e-6);
    }

    #[wasm_bindgen_test]
    fn switch_to_globe_enters_orbit_over_site_and_is_idempotent() {
        let (mut c, _) = orbit_with_rect();
        assert_eq!(c.view_mode(), ViewMode::Globe3D);
        {
            let s = orbit(&c);
            assert!((s.pivot_lat - 39.0).abs() < 1e-4);
            assert!((s.pivot_lon - -98.0).abs() < 1e-4);
            assert_eq!(s.tilt, 0.0);
            assert_eq!(s.heading, 0.0);
        }
        // Idempotent: switching again keeps the (possibly modified) state.
        c.adjust_tilt_heading(50.0, -50.0, 600.0);
        let tilt_before = orbit(&c).tilt;
        c.switch_to_globe();
        assert_eq!(orbit(&c).tilt, tilt_before);
    }

    #[wasm_bindgen_test]
    fn two_d_excursion_round_trips_full_orbit_state() {
        let (mut c, _) = orbit_with_rect();
        // Put the camera in a distinctive pose.
        c.move_pivot_to(41.5, -95.25);
        settle(&mut c);
        c.adjust_tilt_heading(120.0, -80.0, 600.0);
        c.zoom(500.0);
        let before = *orbit(&c);
        // 2D excursion and back.
        c.switch_to_flat_2d(Flat2DState::default());
        assert!(c.is_2d());
        c.switch_to_globe();
        let after = orbit(&c);
        assert!((after.pivot_lat - before.pivot_lat).abs() < 1e-6);
        assert!((after.pivot_lon - before.pivot_lon).abs() < 1e-6);
        assert!((after.distance - before.distance).abs() < 1e-6);
        assert!((after.tilt - before.tilt).abs() < 1e-6);
        assert!((after.heading - before.heading).abs() < 1e-6);
    }

    #[wasm_bindgen_test]
    fn switch_to_flat_2d_is_noop_when_already_2d() {
        let mut c = Camera::centered_on(39.0, -98.0);
        c.flat_2d_mut().unwrap().zoom = 4.0;
        // Must not clobber the live 2D view with the passed default state.
        c.switch_to_flat_2d(Flat2DState::default());
        assert!((c.flat_2d().unwrap().zoom - 4.0).abs() < 1e-6);
    }

    #[wasm_bindgen_test]
    fn center_on_updates_site_and_resets_view_in_both_variants() {
        // In 3D: re-pivots to the new site at defaults.
        let (mut c, _) = orbit_with_rect();
        c.adjust_tilt_heading(100.0, -100.0, 600.0);
        c.center_on(35.3, -97.3);
        {
            let s = orbit(&c);
            assert!((s.common.site_lat - 35.3).abs() < 1e-4);
            assert!((s.pivot_lat - 35.3).abs() < 1e-4);
            assert_eq!(s.tilt, 0.0);
            assert_eq!(s.heading, 0.0);
            assert!((s.distance - 0.10).abs() < 1e-6);
        }
        // In 2D: updates the saved camera the next 3D switch restores.
        let mut c2 = Camera::centered_on(39.0, -98.0);
        c2.center_on(44.9, -93.6);
        c2.switch_to_globe();
        let s2 = orbit(&c2);
        assert!((s2.common.site_lat - 44.9).abs() < 1e-4);
        assert!((s2.pivot_lat - 44.9).abs() < 1e-4);
    }

    // ---- Matrix invariants -------------------------------------------------

    #[wasm_bindgen_test]
    fn pivot_projects_to_screen_center_for_all_poses() {
        let (mut c, rect) = orbit_with_rect();
        let poses: &[(f64, f64, f32, f32, f32)] = &[
            (39.0, -98.0, 0.0, 0.0, 0.10),
            (39.0, -98.0, 45.0, 90.0, 0.10),
            (39.0, -98.0, 85.0, -135.0, 0.001),
            (-45.0, 170.0, 45.0, 30.0, 1.0),
            (70.0, 10.0, 85.0, 179.0, 19.0),
            (0.0, 0.0, 30.0, -90.0, 5.0),
        ];
        for &(lat, lon, tilt, heading, dist) in poses {
            {
                let Camera::Orbit(s) = &mut c else {
                    unreachable!()
                };
                s.pivot_lat = lat as f32;
                s.pivot_lon = lon as f32;
                s.tilt = tilt;
                s.heading = heading;
                s.distance = dist;
            }
            let px = c
                .geo_to_screen(lat, lon, rect)
                .expect("pivot must be visible");
            // The pivot is the look-at target: exact screen center.
            assert!(
                (px.x - rect.center().x).abs() < 0.1 && (px.y - rect.center().y).abs() < 0.1,
                "pivot off-center at tilt={tilt} heading={heading} dist={dist}: {px:?}"
            );
        }
    }

    #[wasm_bindgen_test]
    fn untilted_eye_sits_radially_above_pivot() {
        let (mut c, _) = orbit_with_rect();
        c.move_pivot_to(39.0, -98.0);
        let pivot_dir = Camera::geo_to_world(39.0, -98.0);
        let eye = c.camera_world_pos();
        let expected = pivot_dir * (1.0 + orbit(&c).distance);
        assert!((eye - expected).length() < 1e-4);
    }

    #[wasm_bindgen_test]
    fn eye_distance_from_pivot_matches_state_under_tilt() {
        let (mut c, _) = orbit_with_rect();
        c.adjust_tilt_heading(200.0, -300.0, 600.0);
        let s = orbit(&c);
        let pivot = Camera::geo_to_world(s.pivot_lat as f64, s.pivot_lon as f64);
        let eye = c.camera_world_pos();
        assert!(((eye - pivot).length() - s.distance).abs() < 1e-5);
    }

    #[wasm_bindgen_test]
    fn screen_geo_round_trips_near_center() {
        let (c, rect) = orbit_with_rect();
        let probe = Pos2::new(rect.center().x + 60.0, rect.center().y - 40.0);
        let (lat, lon) = c.screen_to_geo(probe, rect).expect("on-globe probe");
        let back = c.geo_to_screen(lat, lon, rect).expect("round trip");
        assert!((back.x - probe.x).abs() < 0.5);
        assert!((back.y - probe.y).abs() < 0.5);
    }

    #[wasm_bindgen_test]
    fn near_plane_is_continuous_and_clamped() {
        // Continuity across the historical 1.1-center-distance boundary.
        let a = near_plane_for(0.0999);
        let b = near_plane_for(0.1001);
        assert!((a - b).abs() < 1e-4);
        // Proportional in the mid range, clamped at both ends.
        assert!((near_plane_for(0.05) - 0.005).abs() < 1e-7);
        assert!((near_plane_for(MIN_SURFACE_DIST) - 1e-4).abs() < 1e-9);
        assert_eq!(near_plane_for(0.0), 2e-5);
        assert_eq!(near_plane_for(MAX_SURFACE_DIST), 0.01);
    }

    // ---- Zoom: no dead zone ------------------------------------------------

    #[wasm_bindgen_test]
    fn zoom_in_strictly_decreases_distance_until_min_with_no_plateau() {
        let (mut c, _) = orbit_with_rect();
        let mut prev = orbit(&c).distance;
        let mut hit_min = false;
        for _ in 0..3000 {
            c.zoom(120.0);
            let d = orbit(&c).distance;
            if (d - MIN_SURFACE_DIST).abs() < 1e-9 {
                hit_min = true;
                break;
            }
            // Every tick before the clamp must strictly reduce distance —
            // the regression test for the old 1.001..1.05 dead zone.
            assert!(d < prev, "zoom plateaued at {d}");
            prev = d;
        }
        assert!(hit_min, "zoom-in never reached MIN_SURFACE_DIST");
        // One tick back out immediately increases distance.
        c.zoom(-120.0);
        assert!(orbit(&c).distance > MIN_SURFACE_DIST);
    }

    #[wasm_bindgen_test]
    fn zoom_about_keeps_the_anchor_point_under_the_cursor() {
        let (mut c, rect) = orbit_with_rect();
        let cursor = Pos2::new(rect.center().x + 150.0, rect.center().y - 100.0);
        let (lat0, lon0) = c.screen_to_geo(cursor, rect).expect("on-globe cursor");
        c.zoom_about(240.0, Some(cursor), rect);
        let (lat1, lon1) = c.screen_to_geo(cursor, rect).expect("still on globe");
        assert!(
            (lat1 - lat0).abs() < 0.05,
            "anchor lat drifted: {lat0} → {lat1}"
        );
        assert!(
            (lon1 - lon0).abs() < 0.05,
            "anchor lon drifted: {lon0} → {lon1}"
        );
        // And it actually zoomed.
        assert!(orbit(&c).distance < 0.10);
    }

    #[wasm_bindgen_test]
    fn zoom_about_falls_back_to_centered_zoom() {
        // Off-globe cursor: distance changes, pivot does not.
        let (mut c, rect) = orbit_with_rect();
        {
            let Camera::Orbit(s) = &mut c else {
                unreachable!()
            };
            s.distance = 10.0; // globe small on screen; corners miss it
        }
        let pivot0 = (orbit(&c).pivot_lat, orbit(&c).pivot_lon);
        c.zoom_about(240.0, Some(Pos2::new(1.0, 1.0)), rect);
        assert_eq!((orbit(&c).pivot_lat, orbit(&c).pivot_lon), pivot0);
        assert!(orbit(&c).distance < 10.0);

        // No cursor at all: same fallback.
        let (mut c2, rect2) = orbit_with_rect();
        let pivot2 = (orbit(&c2).pivot_lat, orbit(&c2).pivot_lon);
        c2.zoom_about(240.0, None, rect2);
        assert_eq!((orbit(&c2).pivot_lat, orbit(&c2).pivot_lon), pivot2);

        // Tilted near the horizon: re-anchoring is skipped.
        let (mut c3, rect3) = orbit_with_rect();
        {
            let Camera::Orbit(s) = &mut c3 else {
                unreachable!()
            };
            s.tilt = 80.0;
        }
        let pivot3 = (orbit(&c3).pivot_lat, orbit(&c3).pivot_lon);
        c3.zoom_about(240.0, Some(rect3.center()), rect3);
        assert_eq!((orbit(&c3).pivot_lat, orbit(&c3).pivot_lon), pivot3);
    }

    #[wasm_bindgen_test]
    fn grab_pan_keeps_the_grabbed_point_under_the_cursor() {
        let (mut c, rect) = orbit_with_rect();
        let from = Pos2::new(rect.center().x + 40.0, rect.center().y + 30.0);
        let to = Pos2::new(rect.center().x + 120.0, rect.center().y - 50.0);
        let (glat, glon) = c.screen_to_geo(from, rect).expect("grab point on globe");
        assert!(c.pan_pivot_grab(from, to, rect));
        let (nlat, nlon) = c.screen_to_geo(to, rect).expect("target on globe");
        assert!(
            (nlat - glat).abs() < 0.05,
            "grabbed lat slipped: {glat} → {nlat}"
        );
        assert!(
            (nlon - glon).abs() < 0.05,
            "grabbed lon slipped: {glon} → {nlon}"
        );
    }

    #[wasm_bindgen_test]
    fn grab_pan_reports_false_when_ill_conditioned() {
        // Off-globe endpoint → false, no state change.
        let (mut c, rect) = orbit_with_rect();
        {
            let Camera::Orbit(s) = &mut c else {
                unreachable!()
            };
            s.distance = 10.0;
        }
        let pivot0 = (orbit(&c).pivot_lat, orbit(&c).pivot_lon);
        assert!(!c.pan_pivot_grab(Pos2::new(1.0, 1.0), rect.center(), rect));
        assert_eq!((orbit(&c).pivot_lat, orbit(&c).pivot_lon), pivot0);

        // High tilt → false (caller falls back to delta pan).
        let (mut c2, rect2) = orbit_with_rect();
        {
            let Camera::Orbit(s) = &mut c2 else {
                unreachable!()
            };
            s.tilt = 80.0;
        }
        assert!(!c2.pan_pivot_grab(rect2.center(), rect2.center(), rect2));

        // 2D → false.
        let mut flat = Camera::centered_on(39.0, -98.0);
        let r = Rect::from_min_size(Pos2::ZERO, eframe::egui::Vec2::new(800.0, 600.0));
        assert!(!flat.pan_pivot_grab(r.center(), r.center(), r));
    }

    #[wasm_bindgen_test]
    fn zoom_step_matches_the_shared_scroll_constant() {
        // One 120-unit wheel tick moves log-distance by exactly
        // 120 × ZOOM_LOG_PER_SCROLL_UNIT — the same step the 2D handler
        // applies to its zoom factor, so both views share one feel.
        let (mut c, _) = orbit_with_rect();
        let before = orbit(&c).distance.ln();
        c.zoom(120.0);
        let after = orbit(&c).distance.ln();
        assert!((before - after - 120.0 * ZOOM_LOG_PER_SCROLL_UNIT).abs() < 1e-6);
    }

    #[wasm_bindgen_test]
    fn zoom_out_clamps_at_max() {
        let (mut c, _) = orbit_with_rect();
        for _ in 0..3000 {
            c.zoom(-500.0);
        }
        assert!((orbit(&c).distance - MAX_SURFACE_DIST).abs() < 1e-6);
    }

    // ---- Drag controls -----------------------------------------------------

    #[wasm_bindgen_test]
    fn pan_is_heading_aware() {
        let (mut c, _) = orbit_with_rect();
        // Heading 0: drag right moves pivot west; drag down moves it north.
        let lat0 = orbit(&c).pivot_lat;
        let lon0 = orbit(&c).pivot_lon;
        c.pan_pivot(50.0, 0.0, 600.0);
        assert!(
            orbit(&c).pivot_lon < lon0,
            "drag right must move pivot west"
        );
        c.pan_pivot(0.0, 50.0, 600.0);
        assert!(
            orbit(&c).pivot_lat > lat0,
            "drag down must move pivot north"
        );

        // Heading 90 (view rotated clockwise, north at screen-left):
        // dragging down moves the pivot toward screen-up = east.
        let (mut c2, _) = orbit_with_rect();
        {
            let Camera::Orbit(s) = &mut c2 else {
                unreachable!()
            };
            s.heading = 90.0;
        }
        let lon_before = orbit(&c2).pivot_lon;
        let lat_before = orbit(&c2).pivot_lat;
        c2.pan_pivot(0.0, 50.0, 600.0);
        assert!(
            orbit(&c2).pivot_lon > lon_before,
            "heading 90: drag down = east"
        );
        assert!((orbit(&c2).pivot_lat - lat_before).abs() < 1e-3);
    }

    #[wasm_bindgen_test]
    fn pan_sensitivity_scales_with_distance() {
        let (mut near_cam, _) = orbit_with_rect();
        let (mut far_cam, _) = orbit_with_rect();
        {
            let Camera::Orbit(s) = &mut far_cam else {
                unreachable!()
            };
            s.distance = orbit(&near_cam).distance * 10.0;
        }
        let lat0 = orbit(&near_cam).pivot_lat;
        near_cam.pan_pivot(0.0, 30.0, 600.0);
        far_cam.pan_pivot(0.0, 30.0, 600.0);
        let near_delta = orbit(&near_cam).pivot_lat - lat0;
        let far_delta = orbit(&far_cam).pivot_lat - lat0;
        assert!((far_delta / near_delta - 10.0).abs() < 0.01);
    }

    #[wasm_bindgen_test]
    fn tilt_clamps_at_zero_and_max_heading_wraps() {
        let (mut c, _) = orbit_with_rect();
        // Massive drag down: tilt clamps at 0 (cannot go negative).
        c.adjust_tilt_heading(0.0, 100_000.0, 600.0);
        assert_eq!(orbit(&c).tilt, 0.0);
        // Massive drag up: clamps at MAX_TILT_DEG — no zenith crossing.
        c.adjust_tilt_heading(0.0, -100_000.0, 600.0);
        assert_eq!(orbit(&c).tilt, MAX_TILT_DEG);
        // Heading wraps into ±180.
        c.adjust_tilt_heading(100_000.0, 0.0, 600.0);
        let h = orbit(&c).heading;
        assert!((-180.0..=180.0).contains(&h));
    }

    #[wasm_bindgen_test]
    fn pivot_latitude_clamps_near_poles() {
        let (mut c, _) = orbit_with_rect();
        c.move_pivot_to(89.999, 0.0);
        settle(&mut c);
        assert!((orbit(&c).pivot_lat - 89.9).abs() < 1e-4);
        for _ in 0..200 {
            c.pan_pivot(0.0, 500.0, 600.0);
        }
        assert!(orbit(&c).pivot_lat <= 89.9);
    }

    // ---- Keyboard ----------------------------------------------------------

    #[wasm_bindgen_test]
    fn keyboard_move_never_changes_distance() {
        let (mut c, _) = orbit_with_rect();
        let d0 = orbit(&c).distance;
        assert!(c.keyboard_move(1.0, 1.0, 1.0, 2.0, 0.016));
        assert!(c.keyboard_move(-1.0, 0.0, 0.0, 1.0, 0.016));
        assert_eq!(orbit(&c).distance, d0);
    }

    #[wasm_bindgen_test]
    fn keyboard_move_pans_and_rotates() {
        let (mut c, _) = orbit_with_rect();
        let s0 = *orbit(&c);
        // W pans north at heading 0.
        assert!(c.keyboard_move(1.0, 0.0, 0.0, 1.0, 0.1));
        assert!(orbit(&c).pivot_lat > s0.pivot_lat);
        // D pans east at heading 0.
        assert!(c.keyboard_move(0.0, 1.0, 0.0, 1.0, 0.1));
        assert!(orbit(&c).pivot_lon > s0.pivot_lon);
        // E rotates heading clockwise; wraps.
        assert!(c.keyboard_move(0.0, 0.0, 1.0, 1.0, 0.1));
        assert!(orbit(&c).heading > 0.0);
        for _ in 0..100 {
            c.keyboard_move(0.0, 0.0, 1.0, 2.0, 0.1);
        }
        assert!((-180.0..=180.0).contains(&orbit(&c).heading));
    }

    #[wasm_bindgen_test]
    fn keyboard_move_is_dt_scaled() {
        let (mut a, _) = orbit_with_rect();
        let (mut b, _) = orbit_with_rect();
        let lat0 = orbit(&a).pivot_lat;
        a.keyboard_move(1.0, 0.0, 0.0, 1.0, 0.02);
        b.keyboard_move(1.0, 0.0, 0.0, 1.0, 0.04);
        let da = orbit(&a).pivot_lat - lat0;
        let db = orbit(&b).pivot_lat - lat0;
        assert!((db / da - 2.0).abs() < 1e-3);
    }

    #[wasm_bindgen_test]
    fn keyboard_move_reports_false_when_inert() {
        let mut flat = Camera::centered_on(39.0, -98.0);
        assert!(!flat.keyboard_move(1.0, 1.0, 1.0, 1.0, 0.016));
        let (mut c, _) = orbit_with_rect();
        assert!(!c.keyboard_move(0.0, 0.0, 0.0, 1.0, 0.016));
    }

    // ---- One-shot camera actions -------------------------------------------

    #[wasm_bindgen_test]
    fn reset_restores_site_defaults() {
        let (mut c, _) = orbit_with_rect();
        c.move_pivot_to(10.0, 10.0);
        settle(&mut c);
        c.adjust_tilt_heading(100.0, -100.0, 600.0);
        c.zoom(1000.0);
        c.reset();
        settle(&mut c);
        let s = orbit(&c);
        assert!((s.pivot_lat - 39.0).abs() < 1e-4);
        assert!((s.pivot_lon - -98.0).abs() < 1e-4);
        assert!((s.distance - 0.10).abs() < 1e-6);
        assert_eq!(s.tilt, 0.0);
        assert_eq!(s.heading, 0.0);
    }

    #[wasm_bindgen_test]
    fn focus_site_moves_pivot_but_keeps_pose() {
        let (mut c, _) = orbit_with_rect();
        c.move_pivot_to(10.0, 10.0);
        settle(&mut c);
        c.adjust_tilt_heading(100.0, -100.0, 600.0);
        c.zoom(1000.0);
        let (tilt, heading, dist) = {
            let s = orbit(&c);
            (s.tilt, s.heading, s.distance)
        };
        c.focus_site();
        settle(&mut c);
        let s = orbit(&c);
        assert!((s.pivot_lat - 39.0).abs() < 1e-4);
        assert_eq!(s.tilt, tilt);
        assert_eq!(s.heading, heading);
        assert_eq!(s.distance, dist);
    }

    #[wasm_bindgen_test]
    fn align_north_zeroes_heading_only() {
        let (mut c, _) = orbit_with_rect();
        c.adjust_tilt_heading(100.0, -100.0, 600.0);
        let tilt = orbit(&c).tilt;
        c.align_north();
        settle(&mut c);
        assert_eq!(orbit(&c).heading, 0.0);
        assert!((orbit(&c).tilt - tilt).abs() < 1e-4);
    }

    // ---- Fly-to animation --------------------------------------------------

    #[wasm_bindgen_test]
    fn fly_to_converges_monotonically() {
        let (mut c, _) = orbit_with_rect();
        c.fly_to(OrbitTarget {
            pivot_lat: 45.0,
            pivot_lon: -80.0,
            distance: 1.5,
            tilt: 40.0,
            heading: 90.0,
        });
        // The live pose is untouched until ticked.
        assert!((orbit(&c).pivot_lat - 39.0).abs() < 1e-4);
        let mut prev_lat_gap = (45.0 - orbit(&c).pivot_lat).abs();
        let mut prev_log_gap = (1.5f32.ln() - orbit(&c).distance.ln()).abs();
        let mut ticks = 0;
        while c.tick_animation(0.016) {
            ticks += 1;
            assert!(ticks < 2000, "did not converge");
            let lat_gap = (45.0 - orbit(&c).pivot_lat).abs();
            let log_gap = (1.5f32.ln() - orbit(&c).distance.ln()).abs();
            assert!(lat_gap <= prev_lat_gap + 1e-6, "lat gap grew");
            assert!(log_gap <= prev_log_gap + 1e-6, "log-distance gap grew");
            prev_lat_gap = lat_gap;
            prev_log_gap = log_gap;
        }
        let s = orbit(&c);
        assert!(s.anim.is_none());
        assert_eq!(s.pivot_lat, 45.0);
        assert_eq!(s.distance, 1.5);
        assert_eq!(s.tilt, 40.0);
        assert_eq!(s.heading, 90.0);
    }

    #[wasm_bindgen_test]
    fn fly_to_takes_the_shortest_arc() {
        let (mut c, _) = orbit_with_rect();
        {
            let Camera::Orbit(s) = &mut c else {
                unreachable!()
            };
            s.heading = 179.0;
        }
        let mut target = orbit(&c).target_pose();
        target.heading = -179.0;
        c.fly_to(target);
        // 179 → −179 is 2° across the wrap, not 358° back through 0.
        c.tick_animation(0.016);
        let h = orbit(&c).heading;
        assert!(
            wrap_deg(h - 179.0).abs() < 2.0,
            "heading went the long way: {h}"
        );
        settle(&mut c);
        assert!((wrap_deg(orbit(&c).heading - -179.0)).abs() < 1e-4);
    }

    #[wasm_bindgen_test]
    fn direct_input_cancels_fly_to() {
        let (mut c, _) = orbit_with_rect();
        c.move_pivot_to(45.0, -80.0);
        assert!(orbit(&c).anim.is_some());
        c.pan_pivot(5.0, 5.0, 600.0);
        assert!(orbit(&c).anim.is_none());

        c.move_pivot_to(45.0, -80.0);
        c.zoom(120.0);
        assert!(orbit(&c).anim.is_none());

        c.move_pivot_to(45.0, -80.0);
        c.keyboard_move(1.0, 0.0, 0.0, 1.0, 0.016);
        assert!(orbit(&c).anim.is_none());

        c.move_pivot_to(45.0, -80.0);
        c.adjust_tilt_heading(5.0, 5.0, 600.0);
        assert!(orbit(&c).anim.is_none());
    }

    #[wasm_bindgen_test]
    fn url_snapshot_reads_the_fly_to_target_mid_flight() {
        let (mut c, _) = orbit_with_rect();
        c.move_pivot_to(45.0, -80.0);
        // Not yet ticked: live pose is still the site, but a link shared
        // now captures the destination.
        let snap = c.url_snapshot();
        assert!((snap.center_lat - 45.0).abs() < 1e-4);
        assert!((snap.center_lon - -80.0).abs() < 1e-4);
    }

    #[wasm_bindgen_test]
    fn switch_to_2d_completes_the_fly_to() {
        let (mut c, _) = orbit_with_rect();
        c.move_pivot_to(45.0, -80.0);
        c.switch_to_flat_2d(Flat2DState::default());
        c.switch_to_globe();
        let s = orbit(&c);
        assert!((s.pivot_lat - 45.0).abs() < 1e-4);
        assert!(s.anim.is_none());
    }

    #[wasm_bindgen_test]
    fn tick_animation_is_inert_when_idle_or_2d() {
        let (mut c, _) = orbit_with_rect();
        assert!(!c.tick_animation(0.016));
        let mut flat = Camera::centered_on(39.0, -98.0);
        assert!(!flat.tick_animation(0.016));
    }

    // ---- URL persistence ---------------------------------------------------

    #[wasm_bindgen_test]
    fn url_snapshot_round_trips_orbit_state() {
        let (mut c, _) = orbit_with_rect();
        c.move_pivot_to(41.5, -95.25);
        settle(&mut c);
        c.adjust_tilt_heading(120.0, -80.0, 600.0);
        c.zoom(700.0);
        let before = *orbit(&c);
        let snap = c.url_snapshot();
        // Wire distance keeps the historical center convention.
        assert!((snap.distance - (1.0 + before.distance)).abs() < 1e-6);

        let mut restored = Camera::centered_on(39.0, -98.0);
        restored.restore_from_url(&snap);
        let after = orbit(&restored);
        assert!((after.pivot_lat - before.pivot_lat).abs() < 1e-5);
        assert!((after.pivot_lon - before.pivot_lon).abs() < 1e-5);
        assert!((after.distance - before.distance).abs() < 1e-5);
        assert!((after.tilt - before.tilt).abs() < 1e-5);
        assert!((after.heading - before.heading).abs() < 1e-5);
    }

    #[wasm_bindgen_test]
    fn url_restore_clamps_out_of_range_fields() {
        let mut c = Camera::centered_on(39.0, -98.0);
        c.restore_from_url(&UrlCameraSnapshot {
            distance: 500.0,
            center_lat: 95.0,
            center_lon: 400.0,
            tilt: -30.0,
            rotation: 270.0,
        });
        let s = orbit(&c);
        assert!((s.distance - MAX_SURFACE_DIST).abs() < 1e-6);
        assert!((s.pivot_lat - 89.9).abs() < 1e-4);
        assert!((-180.0..=180.0).contains(&s.pivot_lon));
        assert_eq!(s.tilt, 0.0);
        assert!((-180.0..=180.0).contains(&s.heading));
    }

    #[wasm_bindgen_test]
    fn legacy_planet_orbit_url_maps_directly() {
        let mut c = Camera::centered_on(39.0, -98.0);
        c.restore_from_url_fields(&UrlOrbitFields {
            cm: Some(0),
            cd: Some(1.5),
            clat: Some(30.0),
            clon: Some(-85.0),
            ct: Some(-30.0), // legacy view-space pitch: magnitude maps
            cr: Some(45.0),
            ..Default::default()
        });
        let s = orbit(&c);
        assert!((s.pivot_lat - 30.0).abs() < 1e-5);
        assert!((s.pivot_lon - -85.0).abs() < 1e-5);
        assert!((s.distance - 0.5).abs() < 1e-6);
        assert!((s.tilt - 30.0).abs() < 1e-5);
        assert!((s.heading - 45.0).abs() < 1e-5);
    }

    #[wasm_bindgen_test]
    fn legacy_site_orbit_url_maps_bearing_and_elevation() {
        // Camera at bearing 180 looking north at elevation 45 → heading 0,
        // tilt 45.
        let mut c = Camera::centered_on(39.0, -98.0);
        c.restore_from_url_fields(&UrlOrbitFields {
            cm: Some(1),
            cd: Some(1.2),
            ob: Some(180.0),
            oe: Some(45.0),
            ..Default::default()
        });
        {
            let s = orbit(&c);
            // Pivot falls back to the site when clat/clon are absent.
            assert!((s.pivot_lat - 39.0).abs() < 1e-4);
            assert!((s.distance - 0.2).abs() < 1e-6);
            assert!((s.heading - 0.0).abs() < 1e-5);
            assert!((s.tilt - 45.0).abs() < 1e-5);
        }
        // Camera at bearing 0 (due north of the site) → view faces south.
        let mut c2 = Camera::centered_on(39.0, -98.0);
        c2.restore_from_url_fields(&UrlOrbitFields {
            cm: Some(1),
            ob: Some(0.0),
            oe: Some(90.0),
            ..Default::default()
        });
        let s2 = orbit(&c2);
        assert!(s2.heading.abs() > 179.0);
        assert_eq!(s2.tilt, 0.0);
        // Past-horizon legacy elevation clamps into the tilt range.
        let mut c3 = Camera::centered_on(39.0, -98.0);
        c3.restore_from_url_fields(&UrlOrbitFields {
            cm: Some(1),
            oe: Some(2.0),
            ..Default::default()
        });
        assert_eq!(orbit(&c3).tilt, MAX_TILT_DEG);
    }

    #[wasm_bindgen_test]
    fn legacy_free_look_url_degrades_to_default_site_view() {
        let mut c = Camera::centered_on(39.0, -98.0);
        c.restore_from_url_fields(&UrlOrbitFields {
            cm: Some(2),
            cd: Some(5.0),
            clat: Some(10.0),
            ..Default::default()
        });
        let s = orbit(&c);
        assert!((s.pivot_lat - 39.0).abs() < 1e-4);
        assert!((s.distance - 0.10).abs() < 1e-6);
        assert_eq!(s.tilt, 0.0);
        assert_eq!(s.heading, 0.0);
    }

    #[wasm_bindgen_test]
    fn new_format_url_fields_restore_without_cm() {
        let mut c = Camera::centered_on(39.0, -98.0);
        c.restore_from_url_fields(&UrlOrbitFields {
            cd: Some(1.3),
            clat: Some(41.0),
            clon: Some(-93.0),
            ct: Some(20.0),
            cr: Some(-60.0),
            ..Default::default()
        });
        let s = orbit(&c);
        assert!((s.pivot_lat - 41.0).abs() < 1e-5);
        assert!((s.distance - 0.3).abs() < 1e-6);
        assert!((s.tilt - 20.0).abs() < 1e-5);
        assert!((s.heading - -60.0).abs() < 1e-5);
        // Entirely empty fields → default view over the site.
        let mut c2 = Camera::centered_on(39.0, -98.0);
        c2.restore_from_url_fields(&UrlOrbitFields::default());
        let s2 = orbit(&c2);
        assert!((s2.pivot_lat - 39.0).abs() < 1e-4);
        assert!((s2.distance - 0.10).abs() < 1e-6);
    }

    #[wasm_bindgen_test]
    fn url_snapshot_in_2d_carries_saved_camera() {
        let (mut c, _) = orbit_with_rect();
        c.move_pivot_to(41.5, -95.25);
        c.switch_to_flat_2d(Flat2DState::default());
        let snap = c.url_snapshot();
        assert!((snap.center_lat - 41.5).abs() < 1e-4);
    }

    // ---- Misc --------------------------------------------------------------

    #[wasm_bindgen_test]
    fn set_site_does_not_move_the_view() {
        let (mut c, _) = orbit_with_rect();
        let pivot_before = (orbit(&c).pivot_lat, orbit(&c).pivot_lon);
        c.set_site(45.0, -100.0);
        let s = orbit(&c);
        assert!((s.common.site_lat - 45.0).abs() < 1e-4);
        assert_eq!((s.pivot_lat, s.pivot_lon), pivot_before);
    }

    #[wasm_bindgen_test]
    fn wrap_deg_uses_shortest_representation() {
        assert!((wrap_deg(190.0) - -170.0).abs() < 1e-6);
        assert!((wrap_deg(-190.0) - 170.0).abs() < 1e-6);
        assert!((wrap_deg(720.0) - 0.0).abs() < 1e-6);
        assert!((wrap_deg(179.0) - 179.0).abs() < 1e-6);
    }

    #[wasm_bindgen_test]
    fn controls_are_noops_in_2d() {
        let mut c = Camera::centered_on(39.0, -98.0);
        let saved_before = match &c {
            Camera::Flat2D(f) => f.saved,
            _ => unreachable!(),
        };
        c.pan_pivot(50.0, 50.0, 600.0);
        c.adjust_tilt_heading(50.0, 50.0, 600.0);
        c.zoom(120.0);
        c.move_pivot_to(10.0, 10.0);
        c.reset();
        c.focus_site();
        c.align_north();
        let Camera::Flat2D(f) = &c else {
            unreachable!()
        };
        assert_eq!(f.saved.pivot_lat, saved_before.pivot_lat);
        assert_eq!(f.saved.tilt, saved_before.tilt);
        assert_eq!(f.saved.distance, saved_before.distance);
    }
}
