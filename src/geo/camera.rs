//! Camera state machine for the radar view.
//!
//! A single [`Camera`] enum owns every camera state — the flat 2D
//! top-down view plus three disjoint 3D modes (planet orbit, site orbit,
//! free look). Each variant carries ONLY the fields valid in that mode,
//! so an invalid-field-in-wrong-mode access (or a cross-mode state leak
//! like `free_pos` surviving a switch to orbit) is impossible by
//! construction. Mode transitions are explicit `switch_to_*` methods that
//! build the new variant from the old, preserving what makes sense and
//! dropping what doesn't.
//!
//! Each mode has distinct mouse/keyboard controls following a consistent
//! paradigm:
//! - Left mouse: primary navigation
//! - Right mouse: orientation adjustment
//! - Middle mouse / Shift+left: pan/translate
//! - Scroll: zoom or speed
//! - WASD / arrows: directional movement
//!
//! [`ViewMode`](crate::state::ViewMode) is a *derived* view of the active
//! variant ([`Camera::view_mode`]); it is not an independent toggle to
//! keep in sync.

use crate::state::ViewMode;
use eframe::egui::{Pos2, Rect, Vec2};
use glam::{Mat4, Vec3, Vec4};

/// 3D camera movement mode (the three orbit variants).
///
/// Mirrors the three 3D arms of [`Camera`]; used by URL persistence, the
/// view-mode pills, and the compass overlay to talk about "which 3D
/// mode" without owning camera state.
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum CameraMode {
    /// Orbit around the planet core. Drag rotates the globe.
    #[default]
    PlanetOrbit,
    /// Orbit around the radar site, always facing it.
    SiteOrbit,
    /// Free look: first-person flying camera.
    FreeLook,
}

#[allow(dead_code)]
impl CameraMode {
    pub fn label(&self) -> &'static str {
        match self {
            CameraMode::PlanetOrbit => "Planet Orbit",
            CameraMode::SiteOrbit => "Site Orbit",
            CameraMode::FreeLook => "Free Look",
        }
    }

    pub fn next(&self) -> Self {
        match self {
            CameraMode::PlanetOrbit => CameraMode::SiteOrbit,
            CameraMode::SiteOrbit => CameraMode::FreeLook,
            CameraMode::FreeLook => CameraMode::PlanetOrbit,
        }
    }

    /// Key label for the mode (shown in UI).
    pub fn key_hint(&self) -> &'static str {
        match self {
            CameraMode::SiteOrbit => "2",
            CameraMode::PlanetOrbit => "3",
            CameraMode::FreeLook => "4",
        }
    }
}

// Distance clamp range (Earth radii).
// 1.001 allows very close zoom (~6.4 km above surface).
const MIN_DISTANCE: f32 = 1.001;
const MAX_DISTANCE: f32 = 20.0;

/// Default camera distance when viewing a radar site (~637 km above surface).
/// Provides a view comparable to the 2D flat view's ~500 km radius.
const DEFAULT_SITE_DISTANCE: f32 = 1.10;

/// Default vertical field-of-view (radians) for the 3D modes.
const DEFAULT_FOV_Y: f32 = std::f32::consts::FRAC_PI_4; // 45°

/// Default free-look movement speed (Earth radii per second).
const DEFAULT_FREE_SPEED: f32 = 0.5;

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

/// The flat 2D view plus the carried 3D seed.
///
/// 2D itself only needs pan/zoom, but the radar site (and fov/aspect) are
/// shared by every mode — the historical single-struct camera always
/// carried them. Keeping the [`Globe3DCommon`] seed on the 2D arm lets a
/// later switch to a 3D mode re-center on the correct site instead of the
/// equator. This is shared 3D *frustum/site* state, not orbit/free state,
/// so it isn't the kind of cross-mode leak S4 eliminates.
#[derive(Clone, Copy)]
pub struct Flat2D {
    pub view: Flat2DState,
    pub seed: Globe3DCommon,
}

// ── 3D shared state ─────────────────────────────────────────────────

/// State shared by all three 3D modes.
///
/// `fov_y`/`aspect` define the perspective frustum; `site_lat`/`site_lon`
/// track the radar site (the focus/reset/recenter target in every 3D
/// mode). Mode-specific state lives on the variant structs.
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

/// State for Planet Orbit mode: orbit around the planet core, drag rotates
/// the globe.
#[derive(Clone, Copy)]
pub struct PlanetOrbitState {
    pub common: Globe3DCommon,
    /// Latitude the camera is looking at (degrees, -90..90).
    pub center_lat: f32,
    /// Longitude the camera is looking at (degrees, -180..180).
    pub center_lon: f32,
    /// Distance from the camera to the globe center, in Earth radii.
    pub distance: f32,
    /// Camera tilt (pitch) in degrees. 0 = looking at globe center.
    pub tilt: f32,
    /// Camera rotation (yaw offset) in degrees. 0 = North up, positive = CW.
    pub rotation: f32,
}

/// State for Site Orbit mode: orbit around the radar site, always facing it.
#[derive(Clone, Copy)]
pub struct SiteOrbitState {
    pub common: Globe3DCommon,
    /// Distance from the camera to the globe center, in Earth radii.
    pub distance: f32,
    /// Bearing from site (degrees, 0=North, CW).
    pub orbit_bearing: f32,
    /// Elevation angle above horizon (degrees, 0=level, 90=directly above).
    pub orbit_elevation: f32,
    /// Camera tilt (pitch) in degrees.
    pub tilt: f32,
    /// Camera rotation (roll/yaw offset) in degrees.
    pub rotation: f32,
    /// Orbit pivot latitude (degrees) — carried so a switch back to planet
    /// orbit keeps the same look-at center.
    pub center_lat: f32,
    /// Orbit pivot longitude (degrees).
    pub center_lon: f32,
}

/// State for Free Look mode: first-person flying camera.
#[derive(Clone, Copy)]
pub struct FreeLookState {
    pub common: Globe3DCommon,
    /// Camera position in world space.
    pub free_pos: Vec3,
    /// Yaw angle in degrees (0 = looking along +Z, 90 = looking along +X).
    pub free_yaw: f32,
    /// Pitch angle in degrees (0 = level, positive = looking up).
    pub free_pitch: f32,
    /// Movement speed in Earth radii per second.
    pub free_speed: f32,
}

/// The camera state machine: exactly one of the flat 2D view or the three
/// 3D orbit modes is active at a time. Each variant owns only the fields
/// valid in that mode.
#[derive(Clone)]
pub enum Camera {
    Flat2D(Flat2D),
    PlanetOrbit(PlanetOrbitState),
    SiteOrbit(SiteOrbitState),
    FreeLook(FreeLookState),
}

impl Default for Camera {
    fn default() -> Self {
        // Matches the historical default: 2D view (ViewMode::default()),
        // with the camera seeded as if it were centered on the
        // equator/prime-meridian. Callers immediately `center_on` the
        // active site, so the seed center is rarely observed.
        Camera::Flat2D(Flat2D {
            view: Flat2DState::default(),
            seed: Globe3DCommon::centered_on(0.0, 0.0),
        })
    }
}

// ── Free-standing 3D math ───────────────────────────────────────────
//
// These helpers carry the exact orbit/free-look transforms so each
// `Camera` variant can produce identical matrices to the historical
// single-struct `GlobeCamera`. They are split out of the variants so the
// math is moved verbatim rather than re-derived per arm.

/// Convert geographic (lat°, lon°) to a point on the unit sphere.
fn geo_to_world(lat_deg: f64, lon_deg: f64) -> Vec3 {
    let lat = (lat_deg as f32).to_radians();
    let lon = (lon_deg as f32).to_radians();
    Vec3::new(lat.cos() * lon.sin(), lat.sin(), lat.cos() * lon.cos())
}

/// Rotation matrix that places `(center_lat, center_lon)` facing the camera.
fn globe_rotation_matrix(center_lat: f32, center_lon: f32) -> Mat4 {
    // Rotate world so that (center_lat, center_lon) ends up at +Z (facing camera).
    // 1. Rotate around Y by -lon → brings the target longitude to the prime meridian.
    //    After this, the target is at (0, sin(lat), cos(lat)).
    // 2. Rotate around X by +lat → brings (0, sin(lat), cos(lat)) to (0, 0, 1).
    let lat = center_lat.to_radians();
    let lon = center_lon.to_radians();
    Mat4::from_rotation_x(lat) * Mat4::from_rotation_y(-lon)
}

/// View matrix for Planet Orbit mode.
fn planet_orbit_view_matrix(s: &PlanetOrbitState) -> Mat4 {
    let eye = Vec3::new(0.0, 0.0, s.distance);
    let look_at = Mat4::look_at_rh(eye, Vec3::ZERO, Vec3::Y);
    let base = look_at * globe_rotation_matrix(s.center_lat, s.center_lon);

    // Apply tilt (pitch) and rotation (yaw) from right-drag.
    if s.tilt != 0.0 || s.rotation != 0.0 {
        let tilt_mat = Mat4::from_rotation_x(s.tilt.to_radians());
        let rot_mat = Mat4::from_rotation_z(s.rotation.to_radians());
        rot_mat * tilt_mat * base
    } else {
        base
    }
}

/// View matrix for Site Orbit mode — camera orbits around the radar site.
fn site_orbit_view_matrix(s: &SiteOrbitState) -> Mat4 {
    let site_pos = geo_to_world(s.common.site_lat as f64, s.common.site_lon as f64);
    let site_dist = s.distance - 1.0; // distance from the site surface
    let orbit_dist = site_dist.max(0.05);

    // Compute camera position by offsetting from the site along bearing/elevation
    let bearing_rad = s.orbit_bearing.to_radians();
    let elev_rad = s.orbit_elevation.to_radians();

    // Local coordinate frame at the site (on the sphere surface)
    let up = site_pos.normalize();
    // Handle pole degeneracy: if up ≈ ±Y, use Z as reference instead
    let ref_vec = if up.y.abs() > 0.99 { Vec3::Z } else { Vec3::Y };
    let east = ref_vec.cross(up).normalize();
    let north = up.cross(east).normalize();

    // Offset direction in the local tangent plane rotated by bearing, then elevated
    let horiz = north * bearing_rad.cos() + east * bearing_rad.sin();
    let offset_dir = (horiz * elev_rad.cos() + up * elev_rad.sin()).normalize();

    let eye = site_pos + offset_dir * orbit_dist;
    // Use radial up so the horizon stays level regardless of bearing.
    Mat4::look_at_rh(eye, site_pos, up)
}

/// View matrix for Free Look mode — first-person flying camera.
fn free_look_view_matrix(s: &FreeLookState) -> Mat4 {
    let (forward, _, _) = free_look_basis(s.free_yaw, s.free_pitch);
    let target = s.free_pos + forward;
    Mat4::look_at_rh(s.free_pos, target, Vec3::Y)
}

/// The forward / right / camera-up basis for a free-look yaw+pitch.
fn free_look_basis(free_yaw: f32, free_pitch: f32) -> (Vec3, Vec3, Vec3) {
    let yaw = free_yaw.to_radians();
    let pitch = free_pitch.to_radians();
    let fwd = Vec3::new(
        yaw.sin() * pitch.cos(),
        pitch.sin(),
        yaw.cos() * pitch.cos(),
    );
    let world_up = Vec3::Y;
    let right_dir = fwd.cross(world_up).normalize();
    let up_dir = right_dir.cross(fwd).normalize();
    (fwd, right_dir, up_dir)
}

/// Camera world position for Site Orbit mode.
fn site_orbit_camera_world_pos(s: &SiteOrbitState) -> Vec3 {
    let site_pos = geo_to_world(s.common.site_lat as f64, s.common.site_lon as f64);
    let site_dist = (s.distance - 1.0).max(0.05);
    let bearing_rad = s.orbit_bearing.to_radians();
    let elev_rad = s.orbit_elevation.to_radians();
    let up = site_pos.normalize();
    let ref_vec = if up.y.abs() > 0.99 { Vec3::Z } else { Vec3::Y };
    let east = ref_vec.cross(up).normalize();
    let north = up.cross(east).normalize();
    let horiz = north * bearing_rad.cos() + east * bearing_rad.sin();
    let offset_dir = (horiz * elev_rad.cos() + up * elev_rad.sin()).normalize();
    site_pos + offset_dir * site_dist
}

/// Camera world position for Planet Orbit mode.
fn planet_orbit_camera_world_pos(s: &PlanetOrbitState) -> Vec3 {
    // Camera sits at (0,0,distance) in view space; invert the globe rotation
    let inv_rot = globe_rotation_matrix(s.center_lat, s.center_lon).inverse();
    (inv_rot * Vec4::new(0.0, 0.0, s.distance, 1.0)).truncate()
}

/// Exponential (log-space) zoom step applied to a distance, with clamps.
/// Positive `delta` zooms in (closer). Shared by scroll-zoom and W/S keys.
fn zoom_distance(distance: f32, delta: f32) -> f32 {
    // Convert distance to log space, shift, convert back. This makes each
    // scroll tick a consistent percentage change.
    let log_dist = distance.ln();
    let log_min = MIN_DISTANCE.ln();
    let log_max = MAX_DISTANCE.ln();
    // Each scroll unit moves ~0.3% in log space (tuned for smooth feel)
    let new_log = log_dist - delta * 0.003;
    new_log.clamp(log_min, log_max).exp()
}

/// W/S keyboard zoom step (different sensitivity from scroll-zoom).
fn keyboard_zoom_distance(distance: f32, forward: f32, speed_mult: f32, dt: f32) -> f32 {
    let zoom_speed = 1.0 * speed_mult * dt;
    let log_dist = distance.ln();
    let new_log = log_dist - forward * zoom_speed;
    new_log.clamp(MIN_DISTANCE.ln(), MAX_DISTANCE.ln()).exp()
}

#[allow(dead_code)]
impl Camera {
    // ── Construction ────────────────────────────────────────────────

    /// Create a camera centered on the given geographic coordinates.
    ///
    /// Starts in the flat 2D view (matching the historical default view
    /// mode) but seeds the site so a later switch to a 3D mode is centered
    /// correctly. The 3D mode-specific state is materialized on demand by
    /// the `switch_to_*` transitions.
    pub fn centered_on(lat_deg: f64, lon_deg: f64) -> Self {
        Camera::Flat2D(Flat2D {
            view: Flat2DState::default(),
            seed: Globe3DCommon::centered_on(lat_deg, lon_deg),
        })
    }

    // ── Derived view mode ───────────────────────────────────────────

    /// The [`ViewMode`] derived from the active variant. The Flat2D
    /// variant is 2D; the three orbit variants are 3D. This is the single
    /// source of truth — there is no separately-stored toggle to keep in
    /// sync.
    pub fn view_mode(&self) -> ViewMode {
        match self {
            Camera::Flat2D(_) => ViewMode::Flat2D,
            _ => ViewMode::Globe3D,
        }
    }

    /// The 3D [`CameraMode`] of the active variant, or `None` in 2D.
    pub fn camera_mode(&self) -> Option<CameraMode> {
        match self {
            Camera::Flat2D(_) => None,
            Camera::PlanetOrbit(_) => Some(CameraMode::PlanetOrbit),
            Camera::SiteOrbit(_) => Some(CameraMode::SiteOrbit),
            Camera::FreeLook(_) => Some(CameraMode::FreeLook),
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
            _ => None,
        }
    }

    /// Mutable flat 2D view state, if active.
    pub fn flat_2d_mut(&mut self) -> Option<&mut Flat2DState> {
        match self {
            Camera::Flat2D(f) => Some(&mut f.view),
            _ => None,
        }
    }

    /// Shared 3D state (frustum + site). Live on the active 3D variant, or
    /// the carried seed in 2D — so the site survives a 2D excursion.
    fn common(&self) -> &Globe3DCommon {
        match self {
            Camera::Flat2D(f) => &f.seed,
            Camera::PlanetOrbit(s) => &s.common,
            Camera::SiteOrbit(s) => &s.common,
            Camera::FreeLook(s) => &s.common,
        }
    }

    fn common_mut(&mut self) -> &mut Globe3DCommon {
        match self {
            Camera::Flat2D(f) => &mut f.seed,
            Camera::PlanetOrbit(s) => &mut s.common,
            Camera::SiteOrbit(s) => &mut s.common,
            Camera::FreeLook(s) => &mut s.common,
        }
    }

    // ── Matrices ────────────────────────────────────────────────────

    /// View matrix (world → eye). Identity for the 2D variant (the 2D path
    /// never reads it; it uses [`MapProjection`](crate::geo::projection::MapProjection)).
    pub fn view_matrix(&self) -> Mat4 {
        match self {
            Camera::Flat2D(_) => Mat4::IDENTITY,
            Camera::PlanetOrbit(s) => planet_orbit_view_matrix(s),
            Camera::SiteOrbit(s) => site_orbit_view_matrix(s),
            Camera::FreeLook(s) => free_look_view_matrix(s),
        }
    }

    /// Perspective projection matrix.
    pub fn projection_matrix(&self) -> Mat4 {
        let (fov_y, aspect, effective_dist) = match self {
            // 2D never paints through this matrix; return a benign frustum.
            Camera::Flat2D(_) => (DEFAULT_FOV_Y, 1.0, DEFAULT_SITE_DISTANCE),
            Camera::PlanetOrbit(s) => (s.common.fov_y, s.common.aspect, s.distance),
            Camera::SiteOrbit(s) => (s.common.fov_y, s.common.aspect, s.distance),
            Camera::FreeLook(s) => (s.common.fov_y, s.common.aspect, s.free_pos.length()),
        };
        // Adjust near plane based on distance — when very close, use tighter near plane
        let near = if effective_dist < 1.1 { 0.0001 } else { 0.01 };
        Mat4::perspective_rh_gl(fov_y, aspect, near, 100.0)
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

    /// Camera position in world space. Meaningful only in 3D modes; the 2D
    /// variant returns the historical planet-orbit default.
    pub fn camera_world_pos(&self) -> Vec3 {
        match self {
            Camera::Flat2D(_) => Vec3::new(0.0, 0.0, DEFAULT_SITE_DISTANCE),
            Camera::PlanetOrbit(s) => planet_orbit_camera_world_pos(s),
            Camera::SiteOrbit(s) => site_orbit_camera_world_pos(s),
            Camera::FreeLook(s) => s.free_pos,
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

    /// Orbit the globe by screen-space delta (pixels).
    /// Planet Orbit: rotates the globe. Site Orbit: changes bearing/elevation.
    /// No-op in 2D and Free Look.
    pub fn orbit(&mut self, dx: f32, dy: f32, viewport_height: f32) {
        match self {
            Camera::PlanetOrbit(s) => {
                let sensitivity = s.common.fov_y / viewport_height;
                // Grab-and-drag: dragging right moves the globe right (center goes west).
                // Scale sensitivity by distance so close-up panning feels natural.
                let dist_scale = (s.distance - 1.0).max(0.01);
                let dlon = dx * sensitivity * (180.0 / std::f32::consts::PI) * dist_scale;
                let dlat = dy * sensitivity * (180.0 / std::f32::consts::PI) * dist_scale;

                s.center_lon -= dlon;
                s.center_lat += dlat;

                // Clamp latitude to avoid flipping
                s.center_lat = s.center_lat.clamp(-89.9, 89.9);
                // Wrap longitude
                if s.center_lon > 180.0 {
                    s.center_lon -= 360.0;
                }
                if s.center_lon < -180.0 {
                    s.center_lon += 360.0;
                }
            }
            Camera::SiteOrbit(s) => {
                let sensitivity = s.common.fov_y / viewport_height;
                // Grab-and-drag: dragging right orbits camera to the right (bearing decreases).
                let dbearing = -dx * sensitivity * (180.0 / std::f32::consts::PI);
                let delevation = -dy * sensitivity * (180.0 / std::f32::consts::PI);

                s.orbit_bearing = (s.orbit_bearing + dbearing) % 360.0;
                if s.orbit_bearing < 0.0 {
                    s.orbit_bearing += 360.0;
                }
                s.orbit_elevation = (s.orbit_elevation + delevation).clamp(5.0, 175.0);
            }
            // In free look, orbit doesn't apply — use free_look() instead. 2D: no-op.
            Camera::FreeLook(_) | Camera::Flat2D(_) => {}
        }
    }

    /// Adjust camera tilt (pitch) and rotation (yaw) by screen-space delta.
    /// Used by right-drag in orbit modes. No-op outside orbit modes.
    pub fn tilt_rotate(&mut self, dx: f32, dy: f32, viewport_height: f32) {
        let deg_per_rad = 180.0 / std::f32::consts::PI;
        let (fov_y, tilt, rotation) = match self {
            Camera::PlanetOrbit(s) => (s.common.fov_y, &mut s.tilt, &mut s.rotation),
            Camera::SiteOrbit(s) => (s.common.fov_y, &mut s.tilt, &mut s.rotation),
            Camera::FreeLook(_) | Camera::Flat2D(_) => return,
        };
        let sensitivity = fov_y / viewport_height;

        *rotation += dx * sensitivity * deg_per_rad;
        *tilt += dy * sensitivity * deg_per_rad;

        // Clamp tilt to avoid flipping
        *tilt = tilt.clamp(-89.0, 89.0);
        // Wrap rotation
        if *rotation > 180.0 {
            *rotation -= 360.0;
        }
        if *rotation < -180.0 {
            *rotation += 360.0;
        }
    }

    /// Adjust free look direction (yaw/pitch) by screen-space delta.
    /// Used by left-drag and right-drag in Free Look mode. No-op elsewhere.
    pub fn free_look(&mut self, dx: f32, dy: f32, viewport_height: f32) {
        let Camera::FreeLook(s) = self else {
            return;
        };
        let sensitivity = s.common.fov_y / viewport_height;
        let deg_per_rad = 180.0 / std::f32::consts::PI;

        s.free_yaw += dx * sensitivity * deg_per_rad;
        s.free_pitch -= dy * sensitivity * deg_per_rad;

        s.free_pitch = s.free_pitch.clamp(-89.0, 89.0);
        // Wrap yaw
        if s.free_yaw > 180.0 {
            s.free_yaw -= 360.0;
        }
        if s.free_yaw < -180.0 {
            s.free_yaw += 360.0;
        }
    }

    /// Move the free look camera by a directional vector relative to the camera.
    /// `forward` = along look direction, `right` = perpendicular, `up` = world up.
    /// `dt` is frame delta time in seconds. No-op outside Free Look.
    pub fn free_move(&mut self, forward: f32, right: f32, up: f32, dt: f32) {
        let Camera::FreeLook(s) = self else {
            return;
        };
        let (fwd, right_dir, up_dir) = free_look_basis(s.free_yaw, s.free_pitch);

        let velocity = s.free_speed * dt;
        s.free_pos += fwd * forward * velocity;
        s.free_pos += right_dir * right * velocity;
        s.free_pos += up_dir * up * velocity;
    }

    /// Pan the orbit pivot by screen-space delta (middle mouse drag).
    /// In orbit modes this shifts the center/pivot (same as [`Self::orbit`]).
    pub fn pan_pivot(&mut self, dx: f32, dy: f32, viewport_height: f32) {
        // Same as orbit for now — middle-drag shifts the center
        self.orbit(dx, dy, viewport_height);
    }

    /// Translate the free look camera sideways relative to the screen plane.
    /// Used by middle-drag in Free Look mode. No-op elsewhere.
    pub fn free_translate(&mut self, dx: f32, dy: f32, viewport_height: f32) {
        let Camera::FreeLook(s) = self else {
            return;
        };
        let (_, right_dir, up_dir) = free_look_basis(s.free_yaw, s.free_pitch);

        let sensitivity = s.common.fov_y / viewport_height;
        let dist_scale = (s.free_pos.length() - 1.0).max(0.01);
        let scale = sensitivity * dist_scale;

        s.free_pos -= right_dir * dx * scale;
        s.free_pos += up_dir * dy * scale;
    }

    /// Zoom by scroll delta. Positive = zoom in (closer).
    /// In Free Look, scroll adjusts movement speed. No-op in 2D (the 2D
    /// path handles its own zoom on `Flat2DState`).
    pub fn zoom(&mut self, delta: f32) {
        match self {
            Camera::FreeLook(s) => {
                // In free look, scroll adjusts movement speed
                let factor = 1.0 + delta * 0.003;
                s.free_speed = (s.free_speed * factor).clamp(0.01, 50.0);
            }
            Camera::PlanetOrbit(s) => {
                s.distance = zoom_distance(s.distance, delta);
            }
            Camera::SiteOrbit(s) => {
                s.distance = zoom_distance(s.distance, delta);
            }
            Camera::Flat2D(_) => {}
        }
    }

    /// Center the camera on the given lat/lon and reset the view.
    ///
    /// Updates the site for all 3D modes, and resets distance/orbit/free
    /// state to defaults derived from the new center — matching the
    /// historical `GlobeCamera::center_on`. In 2D this only records the new
    /// site (the 2D projection re-centers off `viz_state.center_lat/lon`);
    /// switching to a 3D mode later picks up the seeded site.
    pub fn center_on(&mut self, lat_deg: f64, lon_deg: f64) {
        let lat = lat_deg as f32;
        let lon = lon_deg as f32;
        match self {
            Camera::Flat2D(f) => {
                // Record the new site on the carried seed so a later switch
                // to a 3D mode centers on it. The 2D projection itself
                // centers via viz_state.center_lat/lon.
                f.seed.site_lat = lat;
                f.seed.site_lon = lon;
            }
            Camera::PlanetOrbit(s) => {
                s.common.site_lat = lat;
                s.common.site_lon = lon;
                s.center_lat = lat;
                s.center_lon = lon;
                s.distance = DEFAULT_SITE_DISTANCE;
                s.tilt = 0.0;
                s.rotation = 0.0;
            }
            Camera::SiteOrbit(s) => {
                s.common.site_lat = lat;
                s.common.site_lon = lon;
                s.center_lat = lat;
                s.center_lon = lon;
                s.distance = DEFAULT_SITE_DISTANCE;
                s.orbit_bearing = 180.0;
                s.orbit_elevation = 45.0;
                s.tilt = 0.0;
                s.rotation = 0.0;
            }
            Camera::FreeLook(s) => {
                s.common.site_lat = lat;
                s.common.site_lon = lon;
                // Initialize free look at a reasonable position
                s.free_pos = geo_to_world(lat_deg, lon_deg) * DEFAULT_SITE_DISTANCE;
                s.free_yaw = (-lon + 180.0) % 360.0 - 180.0;
                s.free_pitch = -lat;
            }
        }
    }

    /// Re-center on the site without changing distance or zoom level.
    pub fn recenter(&mut self) {
        match self {
            Camera::PlanetOrbit(s) => {
                s.center_lat = s.common.site_lat;
                s.center_lon = s.common.site_lon;
                s.tilt = 0.0;
                s.rotation = 0.0;
            }
            Camera::SiteOrbit(s) => {
                s.center_lat = s.common.site_lat;
                s.center_lon = s.common.site_lon;
                s.orbit_bearing = 180.0;
                s.orbit_elevation = 45.0;
                s.tilt = 0.0;
                s.rotation = 0.0;
            }
            Camera::FreeLook(_) | Camera::Flat2D(_) => {}
        }
    }

    /// Reset camera to a safe default for the current mode. R key handler.
    pub fn reset(&mut self) {
        match self {
            Camera::PlanetOrbit(s) => {
                s.center_lat = s.common.site_lat;
                s.center_lon = s.common.site_lon;
                s.distance = DEFAULT_SITE_DISTANCE;
                s.tilt = 0.0;
                s.rotation = 0.0;
            }
            Camera::SiteOrbit(s) => {
                s.orbit_bearing = 180.0;
                s.orbit_elevation = 45.0;
                s.distance = DEFAULT_SITE_DISTANCE;
                s.tilt = 0.0;
                s.rotation = 0.0;
            }
            Camera::FreeLook(s) => {
                // Reset to a default vantage point above the radar site
                let pos = geo_to_world(s.common.site_lat as f64, s.common.site_lon as f64)
                    * DEFAULT_SITE_DISTANCE;
                s.free_pos = pos;
                // Look toward the globe center
                let dir = -pos.normalize();
                s.free_yaw = dir.x.atan2(dir.z).to_degrees();
                s.free_pitch = dir.y.asin().to_degrees();
                s.free_speed = DEFAULT_FREE_SPEED;
            }
            Camera::Flat2D(_) => {}
        }
    }

    /// Focus camera on the radar site. F key handler.
    pub fn focus_site(&mut self) {
        match self {
            Camera::PlanetOrbit(s) => {
                s.center_lat = s.common.site_lat;
                s.center_lon = s.common.site_lon;
            }
            Camera::SiteOrbit(s) => {
                // Already orbiting the site; snap to looking north (camera south)
                s.orbit_bearing = 180.0;
            }
            Camera::FreeLook(s) => {
                // Move camera near the site and point toward it
                let site_pos = geo_to_world(s.common.site_lat as f64, s.common.site_lon as f64);
                s.free_pos = site_pos * 2.0;
                let dir = (site_pos - s.free_pos).normalize();
                s.free_yaw = dir.x.atan2(dir.z).to_degrees();
                s.free_pitch = dir.y.asin().to_degrees();
            }
            Camera::Flat2D(_) => {}
        }
    }

    /// Align camera so North is up. N key handler.
    pub fn align_north(&mut self) {
        match self {
            Camera::PlanetOrbit(s) => {
                s.rotation = 0.0;
            }
            Camera::SiteOrbit(s) => {
                s.rotation = 0.0;
                // Keep current bearing but remove tilt
                s.tilt = 0.0;
            }
            Camera::FreeLook(_) | Camera::Flat2D(_) => {}
        }
    }

    /// Level the horizon. L key handler.
    pub fn level_horizon(&mut self) {
        match self {
            Camera::PlanetOrbit(s) => {
                s.tilt = 0.0;
            }
            Camera::SiteOrbit(s) => {
                s.tilt = 0.0;
            }
            Camera::FreeLook(s) => {
                s.free_pitch = 0.0;
            }
            Camera::Flat2D(_) => {}
        }
    }

    /// Move pivot/center to a specific geographic point. Used for double-click.
    pub fn move_pivot_to(&mut self, lat_deg: f64, lon_deg: f64) {
        match self {
            Camera::PlanetOrbit(s) => {
                s.center_lat = lat_deg as f32;
                s.center_lon = lon_deg as f32;
            }
            Camera::SiteOrbit(s) => {
                // In site orbit, double-click moves the orbit pivot (the site)
                s.common.site_lat = lat_deg as f32;
                s.common.site_lon = lon_deg as f32;
            }
            Camera::FreeLook(s) => {
                // In free look, move to the clicked point
                let target = geo_to_world(lat_deg, lon_deg);
                // Position camera at current distance from the clicked point
                let dist = s.free_pos.length();
                s.free_pos = target * dist;
                let dir = (target - s.free_pos).normalize();
                s.free_yaw = dir.x.atan2(dir.z).to_degrees();
                s.free_pitch = dir.y.asin().to_degrees();
            }
            Camera::Flat2D(_) => {}
        }
    }

    /// Handle WASD/arrow key movement. Returns true if any movement occurred.
    /// `forward`: +1 = W/Up, -1 = S/Down
    /// `right`: +1 = D/Right, -1 = A/Left
    /// `up_down`: +1 = E, -1 = Q
    /// `speed_mult`: 2.0 for Shift, 0.25 for Ctrl, 1.0 otherwise.
    /// `dt`: frame delta time in seconds.
    pub fn keyboard_move(
        &mut self,
        forward: f32,
        right: f32,
        up_down: f32,
        speed_mult: f32,
        dt: f32,
    ) -> bool {
        if forward == 0.0 && right == 0.0 && up_down == 0.0 {
            return false;
        }

        let base_speed = 60.0; // degrees per second for orbit, or distance per second

        match self {
            Camera::PlanetOrbit(s) => {
                // WASD/arrows pan the globe (same as lat/lon drag)
                let speed = base_speed * speed_mult * dt;
                // W = camera looks further north → center_lat increases
                s.center_lat += forward * speed * 0.5;
                // D = camera looks further east → center_lon increases
                s.center_lon += right * speed * 0.5;

                s.center_lat = s.center_lat.clamp(-89.9, 89.9);
                if s.center_lon > 180.0 {
                    s.center_lon -= 360.0;
                }
                if s.center_lon < -180.0 {
                    s.center_lon += 360.0;
                }

                // W/S also zoom in Planet Orbit per the spec
                if forward != 0.0 {
                    s.distance = keyboard_zoom_distance(s.distance, forward, speed_mult, dt);
                }
            }
            Camera::SiteOrbit(s) => {
                // A/D rotate horizontally around site, W/S adjust distance
                let speed = base_speed * speed_mult * dt;
                s.orbit_bearing = (s.orbit_bearing + right * speed) % 360.0;
                if s.orbit_bearing < 0.0 {
                    s.orbit_bearing += 360.0;
                }

                // W/S adjust distance
                if forward != 0.0 {
                    s.distance = keyboard_zoom_distance(s.distance, forward, speed_mult, dt);
                }

                // Q/E roll the camera
                if up_down != 0.0 {
                    s.rotation += up_down * speed * 0.5;
                    if s.rotation > 180.0 {
                        s.rotation -= 360.0;
                    }
                    if s.rotation < -180.0 {
                        s.rotation += 360.0;
                    }
                }
            }
            Camera::FreeLook(_) => {
                // WASD = standard FPS movement
                self.free_move(forward, right, up_down, dt * speed_mult);
            }
            Camera::Flat2D(_) => return false,
        }
        true
    }

    /// Reset pivot to default (Home key). Site for orbit modes.
    pub fn reset_pivot(&mut self) {
        match self {
            Camera::PlanetOrbit(s) => {
                s.center_lat = s.common.site_lat;
                s.center_lon = s.common.site_lon;
            }
            Camera::SiteOrbit(_) => {
                // Reset orbit pivot to site location
                // (site_lat/site_lon already point to the site)
            }
            Camera::FreeLook(_) => {
                self.focus_site();
            }
            Camera::Flat2D(_) => {}
        }
    }

    /// Update the site position (for SiteOrbit mode) without moving the camera view.
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
    //
    // Each transition constructs the target variant from the current one,
    // preserving the shared 3D state (`Globe3DCommon`) where it exists and
    // deriving the new mode-specific fields. The historical `switch_mode`
    // logic for entering/leaving Free Look is preserved exactly.

    /// The shared 3D state to seed a new variant: the live 3D common in a
    /// 3D mode, or the carried 2D seed (so the site survives the switch).
    fn common_or_default(&self) -> Globe3DCommon {
        *self.common()
    }

    /// Switch to Planet Orbit mode, preserving reasonable state.
    pub fn switch_to_planet_orbit(&mut self) {
        if matches!(self, Camera::PlanetOrbit(_)) {
            return;
        }
        let common = self.common_or_default();
        // Leaving Free Look: derive orbit center/distance from the free
        // camera's world position (historical switch_mode behavior).
        let (center_lat, center_lon, distance) = match self {
            Camera::FreeLook(s) => {
                let distance = s.free_pos.length().clamp(MIN_DISTANCE, MAX_DISTANCE);
                let pos = s.free_pos.normalize();
                let lat = pos.y.asin().to_degrees();
                let lon = pos.x.atan2(pos.z).to_degrees();
                (lat, lon, distance)
            }
            Camera::SiteOrbit(s) => (s.center_lat, s.center_lon, s.distance),
            // From 2D: seed a fresh planet orbit centered on the site.
            _ => (common.site_lat, common.site_lon, DEFAULT_SITE_DISTANCE),
        };
        *self = Camera::PlanetOrbit(PlanetOrbitState {
            common,
            center_lat,
            center_lon,
            distance,
            tilt: 0.0,
            rotation: 0.0,
        });
    }

    /// Switch to Site Orbit mode, preserving reasonable state.
    pub fn switch_to_site_orbit(&mut self) {
        if matches!(self, Camera::SiteOrbit(_)) {
            return;
        }
        let common = self.common_or_default();
        // Leaving Free Look: derive orbit center/distance from the free
        // camera's world position (historical switch_mode behavior).
        let (center_lat, center_lon, distance, bearing, elevation) = match self {
            Camera::FreeLook(s) => {
                let distance = s.free_pos.length().clamp(MIN_DISTANCE, MAX_DISTANCE);
                let pos = s.free_pos.normalize();
                let lat = pos.y.asin().to_degrees();
                let lon = pos.x.atan2(pos.z).to_degrees();
                (lat, lon, distance, 180.0, 45.0)
            }
            Camera::PlanetOrbit(s) => (s.center_lat, s.center_lon, s.distance, 180.0, 45.0),
            // From 2D: seed a fresh site orbit centered on the site.
            _ => (
                common.site_lat,
                common.site_lon,
                DEFAULT_SITE_DISTANCE,
                180.0,
                45.0,
            ),
        };
        *self = Camera::SiteOrbit(SiteOrbitState {
            common,
            distance,
            orbit_bearing: bearing,
            orbit_elevation: elevation,
            tilt: 0.0,
            rotation: 0.0,
            center_lat,
            center_lon,
        });
    }

    /// Switch to Free Look mode, preserving reasonable state.
    pub fn switch_to_free_look(&mut self) {
        if matches!(self, Camera::FreeLook(_)) {
            return;
        }
        let common = self.common_or_default();
        // Entering Free Look from an orbit mode: initialize free look state
        // from the current orbit camera position and look direction
        // (historical switch_mode behavior). From 2D, seed from the site.
        let (free_pos, look_target) = match self {
            Camera::PlanetOrbit(s) => (
                planet_orbit_camera_world_pos(s),
                geo_to_world(s.center_lat as f64, s.center_lon as f64),
            ),
            Camera::SiteOrbit(s) => (
                site_orbit_camera_world_pos(s),
                geo_to_world(s.common.site_lat as f64, s.common.site_lon as f64),
            ),
            _ => {
                // From 2D: position above the site looking down at it.
                let site = geo_to_world(common.site_lat as f64, common.site_lon as f64);
                (site * DEFAULT_SITE_DISTANCE, site)
            }
        };
        let dir = (look_target - free_pos).normalize();
        let free_yaw = dir.x.atan2(dir.z).to_degrees();
        let free_pitch = dir.y.asin().to_degrees();
        *self = Camera::FreeLook(FreeLookState {
            common,
            free_pos,
            free_yaw,
            free_pitch,
            free_speed: DEFAULT_FREE_SPEED,
        });
    }

    /// Switch to the 3D mode matching the given [`CameraMode`].
    pub fn switch_to_3d(&mut self, mode: CameraMode) {
        match mode {
            CameraMode::PlanetOrbit => self.switch_to_planet_orbit(),
            CameraMode::SiteOrbit => self.switch_to_site_orbit(),
            CameraMode::FreeLook => self.switch_to_free_look(),
        }
    }

    /// Switch to the flat 2D view with the given 2D view state. Coming from
    /// a 3D mode the 2D pan/zoom resets to whatever `state` carries (the
    /// historical model stored 2D pan/zoom separately and never seeded it
    /// from the 3D camera); the radar site is preserved on the carried seed
    /// so a later switch back to 3D re-centers correctly.
    pub fn switch_to_flat_2d(&mut self, state: Flat2DState) {
        if !matches!(self, Camera::Flat2D(_)) {
            let seed = self.common_or_default();
            *self = Camera::Flat2D(Flat2D { view: state, seed });
        }
    }
}

/// Flattened camera values for URL persistence.
///
/// The historical single-struct camera serialized every field at once;
/// the enum only carries the active variant's fields, so this snapshot
/// fills the rest with the historical defaults so old/new share-links
/// round-trip. [`Camera::url_snapshot`] builds it; URL restore in
/// `main.rs` reconstructs a variant from the saved `vm`/`cm` + these
/// values.
#[derive(Clone, Copy)]
pub struct UrlCameraSnapshot {
    pub distance: f32,
    pub center_lat: f32,
    pub center_lon: f32,
    pub tilt: f32,
    pub rotation: f32,
    pub orbit_bearing: f32,
    pub orbit_elevation: f32,
    pub free_pos: [f32; 3],
    pub free_yaw: f32,
    pub free_pitch: f32,
    pub free_speed: f32,
}

impl Default for UrlCameraSnapshot {
    fn default() -> Self {
        // Mirrors the historical `GlobeCamera::default()` field values so a
        // link from a 2D session (no live 3D state) restores into the same
        // 3D defaults the old code produced.
        Self {
            distance: DEFAULT_SITE_DISTANCE,
            center_lat: 0.0,
            center_lon: 0.0,
            tilt: 0.0,
            rotation: 0.0,
            orbit_bearing: 180.0,
            orbit_elevation: 45.0,
            free_pos: [0.0, 0.0, DEFAULT_SITE_DISTANCE],
            free_yaw: 0.0,
            free_pitch: 0.0,
            free_speed: DEFAULT_FREE_SPEED,
        }
    }
}

#[allow(dead_code)]
impl Camera {
    /// Flatten the active variant's state for URL persistence, filling
    /// non-owned fields with historical defaults. The site center seeds
    /// `center_lat/lon` in 2D so a reloaded link re-centers correctly.
    pub fn url_snapshot(&self) -> UrlCameraSnapshot {
        let mut snap = UrlCameraSnapshot::default();
        match self {
            Camera::Flat2D(_) => {}
            Camera::PlanetOrbit(s) => {
                snap.distance = s.distance;
                snap.center_lat = s.center_lat;
                snap.center_lon = s.center_lon;
                snap.tilt = s.tilt;
                snap.rotation = s.rotation;
            }
            Camera::SiteOrbit(s) => {
                snap.distance = s.distance;
                snap.center_lat = s.center_lat;
                snap.center_lon = s.center_lon;
                snap.tilt = s.tilt;
                snap.rotation = s.rotation;
                snap.orbit_bearing = s.orbit_bearing;
                snap.orbit_elevation = s.orbit_elevation;
            }
            Camera::FreeLook(s) => {
                snap.free_pos = [s.free_pos.x, s.free_pos.y, s.free_pos.z];
                snap.free_yaw = s.free_yaw;
                snap.free_pitch = s.free_pitch;
                snap.free_speed = s.free_speed;
            }
        }
        snap
    }

    /// Reconstruct a 3D camera from a saved URL snapshot for the given
    /// [`CameraMode`], on a base camera that already has the site centered
    /// (via [`Camera::centered_on`]). Mirrors the historical URL restore
    /// that set each field directly on the single camera struct.
    pub fn restore_from_url(&mut self, mode: CameraMode, snap: &UrlCameraSnapshot) {
        let common = self.common_or_default();
        match mode {
            CameraMode::PlanetOrbit => {
                *self = Camera::PlanetOrbit(PlanetOrbitState {
                    common,
                    center_lat: snap.center_lat,
                    center_lon: snap.center_lon,
                    distance: snap.distance,
                    tilt: snap.tilt,
                    rotation: snap.rotation,
                });
            }
            CameraMode::SiteOrbit => {
                *self = Camera::SiteOrbit(SiteOrbitState {
                    common,
                    distance: snap.distance,
                    orbit_bearing: snap.orbit_bearing,
                    orbit_elevation: snap.orbit_elevation,
                    tilt: snap.tilt,
                    rotation: snap.rotation,
                    center_lat: snap.center_lat,
                    center_lon: snap.center_lon,
                });
            }
            CameraMode::FreeLook => {
                *self = Camera::FreeLook(FreeLookState {
                    common,
                    free_pos: Vec3::new(snap.free_pos[0], snap.free_pos[1], snap.free_pos[2]),
                    free_yaw: snap.free_yaw,
                    free_pitch: snap.free_pitch,
                    free_speed: snap.free_speed,
                });
            }
        }
    }
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
#[allow(dead_code)] // 3D overlays are not yet sharing the &dyn Projection call sites.
pub struct GlobeProjection<'a> {
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

    /// A planet-orbit camera centered on the continental US.
    fn planet() -> Camera {
        let mut c = Camera::centered_on(39.0, -98.0);
        c.switch_to_planet_orbit();
        c
    }

    /// A planet-orbit camera with its aspect set from a fixed screen rect.
    fn planet_with_rect() -> (Camera, Rect) {
        let rect = Rect::from_min_size(Pos2::new(0.0, 0.0), eframe::egui::Vec2::new(800.0, 600.0));
        let mut c = planet();
        c.set_aspect(rect);
        (c, rect)
    }

    #[wasm_bindgen_test]
    fn geo_to_world_unit_length() {
        let v = Camera::geo_to_world(39.0, -98.0);
        let len = (v.x * v.x + v.y * v.y + v.z * v.z).sqrt();
        assert!((len - 1.0).abs() < 1e-5);
    }

    #[wasm_bindgen_test]
    fn geo_to_world_north_pole_is_y_up() {
        let v = Camera::geo_to_world(90.0, 0.0);
        assert!(v.x.abs() < 1e-5);
        assert!((v.y - 1.0).abs() < 1e-5);
        assert!(v.z.abs() < 1e-5);
    }

    #[wasm_bindgen_test]
    fn geo_to_world_equator_prime_meridian_is_pos_z() {
        let v = Camera::geo_to_world(0.0, 0.0);
        assert!(v.x.abs() < 1e-5);
        assert!(v.y.abs() < 1e-5);
        assert!((v.z - 1.0).abs() < 1e-5);
    }

    #[wasm_bindgen_test]
    fn geo_to_world_round_trip_via_atan2() {
        // The screen_to_geo conversion uses asin(y) and atan2(x, z); the
        // inverse of geo_to_world should give the same lat/lon back.
        for (lat, lon) in [(0.0, 0.0), (39.0, -98.0), (-30.0, 120.0), (45.0, 45.0)] {
            let v = Camera::geo_to_world(lat, lon);
            let lat_back = v.y.asin().to_degrees() as f64;
            let lon_back = v.x.atan2(v.z).to_degrees() as f64;
            assert!((lat_back - lat).abs() < 1e-3, "lat {} -> {}", lat, lat_back);
            assert!((lon_back - lon).abs() < 1e-3, "lon {} -> {}", lon, lon_back);
        }
    }

    /// A zoom on a planet-orbit camera reads back its distance.
    fn planet_distance(c: &Camera) -> f32 {
        match c {
            Camera::PlanetOrbit(s) => s.distance,
            _ => panic!("expected planet orbit"),
        }
    }

    #[wasm_bindgen_test]
    fn zoom_in_decreases_distance() {
        let mut c = planet();
        let before = planet_distance(&c);
        c.zoom(100.0);
        assert!(planet_distance(&c) < before);
    }

    #[wasm_bindgen_test]
    fn zoom_out_increases_distance() {
        let mut c = planet();
        let before = planet_distance(&c);
        c.zoom(-100.0);
        assert!(planet_distance(&c) > before);
    }

    #[wasm_bindgen_test]
    fn zoom_clamps_to_min_max() {
        let mut c = planet();
        // Many ticks in one direction must not overshoot the bounds.
        for _ in 0..1000 {
            c.zoom(1000.0);
        }
        assert!(planet_distance(&c) >= 1.001);
        for _ in 0..2000 {
            c.zoom(-1000.0);
        }
        assert!(planet_distance(&c) <= 20.0);
    }

    #[wasm_bindgen_test]
    fn zoom_is_symmetric_in_log_space_away_from_clamps() {
        // Start from the middle of the log-distance range to avoid hitting
        // MIN_DISTANCE/MAX_DISTANCE clamps mid-test.
        let mut c = planet();
        let mid = (1.001_f32.ln() + 20.0_f32.ln()).exp().sqrt();
        if let Camera::PlanetOrbit(s) = &mut c {
            s.distance = mid;
        }
        let start = planet_distance(&c);
        c.zoom(50.0);
        c.zoom(-50.0);
        assert!(
            ((planet_distance(&c) - start) / start).abs() < 1e-4,
            "expected ~{}, got {}",
            start,
            planet_distance(&c)
        );
    }

    #[wasm_bindgen_test]
    fn center_on_updates_site_and_resets_view() {
        // center_on on a site-orbit camera updates the site and resets the
        // orbit angles to defaults.
        let mut c = Camera::centered_on(39.0, -98.0);
        c.switch_to_site_orbit();
        c.zoom(500.0);
        c.center_on(45.0, -100.0);
        let Camera::SiteOrbit(s) = &c else {
            panic!("expected site orbit");
        };
        assert!((s.common.site_lat - 45.0).abs() < 1e-5);
        assert!((s.common.site_lon - (-100.0)).abs() < 1e-5);
        // Distance and orbit angles get reset.
        assert!((s.orbit_bearing - 180.0).abs() < 1e-5);
        assert!((s.orbit_elevation - 45.0).abs() < 1e-5);
        assert!((s.distance - DEFAULT_SITE_DISTANCE).abs() < 1e-5);
    }

    #[wasm_bindgen_test]
    fn default_camera_is_2d() {
        let c = Camera::default();
        assert!(c.is_2d());
        assert_eq!(c.view_mode(), ViewMode::Flat2D);
        assert_eq!(c.camera_mode(), None);
    }

    #[wasm_bindgen_test]
    fn view_mode_derives_from_variant() {
        let mut c = Camera::centered_on(39.0, -98.0);
        assert_eq!(c.view_mode(), ViewMode::Flat2D);
        c.switch_to_planet_orbit();
        assert_eq!(c.view_mode(), ViewMode::Globe3D);
        assert_eq!(c.camera_mode(), Some(CameraMode::PlanetOrbit));
        c.switch_to_site_orbit();
        assert_eq!(c.view_mode(), ViewMode::Globe3D);
        assert_eq!(c.camera_mode(), Some(CameraMode::SiteOrbit));
        c.switch_to_free_look();
        assert_eq!(c.view_mode(), ViewMode::Globe3D);
        assert_eq!(c.camera_mode(), Some(CameraMode::FreeLook));
        c.switch_to_flat_2d(Flat2DState::default());
        assert_eq!(c.view_mode(), ViewMode::Flat2D);
    }

    #[wasm_bindgen_test]
    fn switch_to_free_look_drops_then_orbit_recovers_distance() {
        // Entering Free Look from planet orbit seeds free_pos from the orbit
        // camera position; leaving back to orbit recovers a comparable
        // distance. This pins the historical switch_mode round-trip.
        let mut c = planet();
        let d0 = planet_distance(&c);
        c.switch_to_free_look();
        let Camera::FreeLook(s) = &c else {
            panic!("expected free look");
        };
        // free_pos length ≈ original orbit distance (planet orbit camera sits
        // at `distance` from the globe center).
        assert!(
            (s.free_pos.length() - d0).abs() < 1e-3,
            "{}",
            s.free_pos.length()
        );
        c.switch_to_planet_orbit();
        // Distance recovered within the clamp range.
        let d1 = planet_distance(&c);
        assert!((d1 - d0).abs() < 1e-3, "d0={d0} d1={d1}");
    }

    #[wasm_bindgen_test]
    fn switch_to_same_mode_is_noop() {
        let mut c = planet();
        c.move_pivot_to(10.0, 20.0);
        let Camera::PlanetOrbit(before) = c.clone() else {
            panic!();
        };
        c.switch_to_planet_orbit(); // already planet → no reconstruction
        let Camera::PlanetOrbit(after) = &c else {
            panic!();
        };
        assert!((after.center_lat - before.center_lat).abs() < 1e-6);
        assert!((after.center_lon - before.center_lon).abs() < 1e-6);
    }

    #[wasm_bindgen_test]
    fn switch_preserves_site() {
        // The radar site (common state) survives every 3D transition.
        let mut c = Camera::centered_on(39.0, -98.0);
        c.switch_to_planet_orbit();
        c.switch_to_site_orbit();
        c.switch_to_free_look();
        let Camera::FreeLook(s) = &c else {
            panic!("expected free look");
        };
        assert!(
            (s.common.site_lat - 39.0).abs() < 1e-4,
            "{}",
            s.common.site_lat
        );
        assert!(
            (s.common.site_lon - (-98.0)).abs() < 1e-4,
            "{}",
            s.common.site_lon
        );
    }

    #[wasm_bindgen_test]
    fn free_look_state_not_leaked_into_orbit() {
        // Type-level guarantee: a planet-orbit variant has no free_pos field
        // to leak. We assert the variant type after a round-trip through free
        // look, which is the by-construction property S4 buys us.
        let mut c = planet();
        c.switch_to_free_look();
        c.free_move(1.0, 0.0, 0.0, 1.0); // perturb free_pos
        c.switch_to_planet_orbit();
        assert!(matches!(c, Camera::PlanetOrbit(_)));
    }

    /// The site center projects to screen and unprojects back to the same
    /// lat/lon — a full geo → screen → geo round-trip through the view and
    /// projection matrices.
    #[wasm_bindgen_test]
    fn site_center_geo_round_trips_through_screen() {
        let (c, rect) = planet_with_rect();
        let screen = c
            .geo_to_screen(39.0, -98.0, rect)
            .expect("site center must project (near side)");
        // The center should land near the middle of the screen.
        assert!((screen.x - rect.center().x).abs() < 1.0, "sx {}", screen.x);
        assert!((screen.y - rect.center().y).abs() < 1.0, "sy {}", screen.y);

        let (lat, lon) = c
            .screen_to_geo(screen, rect)
            .expect("center screen must hit the globe");
        assert!((lat - 39.0).abs() < 1e-2, "lat {}", lat);
        assert!((lon - (-98.0)).abs() < 1e-2, "lon {}", lon);
    }

    /// The screen rect's center unprojects to the near-side geographic point
    /// and projects straight back to that same screen pixel.
    #[wasm_bindgen_test]
    fn rect_center_screen_to_geo_and_back() {
        let (c, rect) = planet_with_rect();
        let center = rect.center();
        let (lat, lon) = c
            .screen_to_geo(center, rect)
            .expect("rect center hits the near side of the globe");
        // Near side of a camera centered on the site → the site itself.
        assert!((lat - 39.0).abs() < 1e-2, "lat {}", lat);
        assert!((lon - (-98.0)).abs() < 1e-2, "lon {}", lon);

        let back = c.geo_to_screen(lat, lon, rect).expect("must re-project");
        assert!((back.x - center.x).abs() < 1.0);
        assert!((back.y - center.y).abs() < 1.0);
    }

    /// A screen point well outside the projected globe disc misses the sphere,
    /// so `screen_to_geo` returns `None`.
    #[wasm_bindgen_test]
    fn screen_to_geo_misses_outside_disc() {
        let (c, rect) = planet_with_rect();
        // A point far off the right edge (NDC x ≈ 5) — well outside the globe
        // disc, so the unprojected ray misses the sphere.
        let off = Pos2::new(rect.max.x + 2000.0, rect.center().y);
        assert!(c.screen_to_geo(off, rect).is_none());
    }

    /// A geographic point on the far hemisphere is behind the globe and culled
    /// by `geo_to_screen` (the back-face test in `world_to_screen`).
    #[wasm_bindgen_test]
    fn geo_to_screen_culls_far_hemisphere() {
        let (c, rect) = planet_with_rect();
        // Antipode of the site (39,-98) is (-39, 82) — directly behind the globe.
        assert!(c.geo_to_screen(-39.0, 82.0, rect).is_none());
    }

    #[wasm_bindgen_test]
    fn flat_2d_zoom_pan_round_trip() {
        let mut c = Camera::default();
        if let Some(s) = c.flat_2d_mut() {
            s.zoom = 2.5;
            s.pan_offset = Vec2::new(10.0, -5.0);
        }
        let s = c.flat_2d().expect("flat 2d");
        assert!((s.zoom - 2.5).abs() < 1e-6);
        assert!((s.pan_offset.x - 10.0).abs() < 1e-6);
        assert!((s.pan_offset.y + 5.0).abs() < 1e-6);
    }

    #[wasm_bindgen_test]
    fn site_survives_round_trip_through_2d() {
        // Switching 3D → 2D → 3D must keep the radar site (the carried seed).
        let mut c = Camera::centered_on(39.0, -98.0);
        c.switch_to_site_orbit();
        c.switch_to_flat_2d(Flat2DState::default());
        assert!(c.is_2d());
        c.switch_to_planet_orbit();
        let Camera::PlanetOrbit(s) = &c else {
            panic!("expected planet orbit");
        };
        assert!((s.common.site_lat - 39.0).abs() < 1e-4);
        assert!((s.common.site_lon - (-98.0)).abs() < 1e-4);
        // A fresh planet orbit centers its look-at on the site.
        assert!((s.center_lat - 39.0).abs() < 1e-4);
        assert!((s.center_lon - (-98.0)).abs() < 1e-4);
    }

    #[wasm_bindgen_test]
    fn url_snapshot_round_trips_each_3d_mode() {
        // url_snapshot() → restore_from_url() preserves the variant-specific
        // fields, mirroring the historical share-link round-trip.
        for mode in [
            CameraMode::PlanetOrbit,
            CameraMode::SiteOrbit,
            CameraMode::FreeLook,
        ] {
            let mut c = Camera::centered_on(39.0, -98.0);
            c.switch_to_3d(mode);
            // Perturb the active mode's state so defaults can't mask a bug.
            c.zoom(50.0);
            c.orbit(7.0, -3.0, 600.0);
            c.free_look(5.0, 2.0, 600.0);
            c.free_move(0.5, 0.0, 0.0, 1.0);

            let snap = c.url_snapshot();
            let mut restored = Camera::centered_on(39.0, -98.0);
            restored.restore_from_url(mode, &snap);
            assert_eq!(restored.camera_mode(), Some(mode));

            // The view-projection matrix is the observable end product; equal
            // matrices ⇒ identical rendered geometry.
            let a = c.view_projection().to_cols_array();
            let b = restored.view_projection().to_cols_array();
            for (x, y) in a.iter().zip(b.iter()) {
                assert!((x - y).abs() < 1e-4, "vp mismatch for {mode:?}: {x} vs {y}");
            }
        }
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    // Re-declared helpers (the sibling `mod tests` ones are private).
    fn planet() -> Camera {
        let mut c = Camera::centered_on(39.0, -98.0);
        c.switch_to_planet_orbit();
        c
    }
    fn site() -> Camera {
        let mut c = Camera::centered_on(39.0, -98.0);
        c.switch_to_site_orbit();
        c
    }
    fn free() -> Camera {
        let mut c = Camera::centered_on(39.0, -98.0);
        c.switch_to_free_look();
        c
    }

    fn planet_state(c: &Camera) -> &PlanetOrbitState {
        match c {
            Camera::PlanetOrbit(s) => s,
            _ => panic!("expected planet orbit"),
        }
    }
    fn site_state(c: &Camera) -> &SiteOrbitState {
        match c {
            Camera::SiteOrbit(s) => s,
            _ => panic!("expected site orbit"),
        }
    }
    fn free_state(c: &Camera) -> &FreeLookState {
        match c {
            Camera::FreeLook(s) => s,
            _ => panic!("expected free look"),
        }
    }

    // ── CameraMode enum: label / next / key_hint / default ──────────────

    #[wasm_bindgen_test]
    fn camera_mode_labels() {
        assert_eq!(CameraMode::PlanetOrbit.label(), "Planet Orbit");
        assert_eq!(CameraMode::SiteOrbit.label(), "Site Orbit");
        assert_eq!(CameraMode::FreeLook.label(), "Free Look");
    }

    #[wasm_bindgen_test]
    fn camera_mode_next_cycles() {
        assert_eq!(CameraMode::PlanetOrbit.next(), CameraMode::SiteOrbit);
        assert_eq!(CameraMode::SiteOrbit.next(), CameraMode::FreeLook);
        assert_eq!(CameraMode::FreeLook.next(), CameraMode::PlanetOrbit);
        // Three nexts return to start.
        assert_eq!(
            CameraMode::PlanetOrbit.next().next().next(),
            CameraMode::PlanetOrbit
        );
    }

    #[wasm_bindgen_test]
    fn camera_mode_key_hints() {
        assert_eq!(CameraMode::SiteOrbit.key_hint(), "2");
        assert_eq!(CameraMode::PlanetOrbit.key_hint(), "3");
        assert_eq!(CameraMode::FreeLook.key_hint(), "4");
    }

    #[wasm_bindgen_test]
    fn camera_mode_default_is_planet_orbit() {
        assert_eq!(CameraMode::default(), CameraMode::PlanetOrbit);
    }

    // ── Accessors: is_2d / flat_2d / flat_2d_mut None paths ─────────────

    #[wasm_bindgen_test]
    fn is_2d_false_in_3d_modes() {
        assert!(!planet().is_2d());
        assert!(!site().is_2d());
        assert!(!free().is_2d());
        assert!(Camera::default().is_2d());
    }

    #[wasm_bindgen_test]
    fn flat_2d_accessors_none_in_3d() {
        let mut c = planet();
        assert!(c.flat_2d().is_none());
        assert!(c.flat_2d_mut().is_none());
    }

    #[wasm_bindgen_test]
    fn flat_2d_accessor_some_in_2d() {
        let c = Camera::default();
        let v = c.flat_2d().expect("2d view present");
        assert!((v.zoom - 1.0).abs() < 1e-6);
    }

    // ── Defaults of carried structs ─────────────────────────────────────

    #[wasm_bindgen_test]
    fn flat2dstate_default_values() {
        let d = Flat2DState::default();
        assert!((d.zoom - 1.0).abs() < 1e-6);
        assert!(d.pan_offset.x.abs() < 1e-6 && d.pan_offset.y.abs() < 1e-6);
    }

    #[wasm_bindgen_test]
    fn url_snapshot_default_matches_historical() {
        let d = UrlCameraSnapshot::default();
        assert!((d.distance - DEFAULT_SITE_DISTANCE).abs() < 1e-6);
        assert!(d.center_lat.abs() < 1e-6 && d.center_lon.abs() < 1e-6);
        assert!((d.orbit_bearing - 180.0).abs() < 1e-6);
        assert!((d.orbit_elevation - 45.0).abs() < 1e-6);
        assert!((d.free_pos[2] - DEFAULT_SITE_DISTANCE).abs() < 1e-6);
        assert!((d.free_speed - DEFAULT_FREE_SPEED).abs() < 1e-6);
    }

    // ── orbit(): longitude wrap, latitude clamp (Planet) ────────────────

    #[wasm_bindgen_test]
    fn orbit_planet_clamps_latitude() {
        let mut c = planet();
        // Huge positive dy pushes center_lat well past the pole; clamp at 89.9.
        c.orbit(0.0, 100000.0, 600.0);
        assert!((planet_state(&c).center_lat - 89.9).abs() < 1e-3);
        // Huge negative dy clamps at -89.9.
        c.orbit(0.0, -200000.0, 600.0);
        assert!((planet_state(&c).center_lat - (-89.9)).abs() < 1e-3);
    }

    #[wasm_bindgen_test]
    fn orbit_planet_wraps_longitude_into_range() {
        let mut c = planet();
        // Drive longitude far in both directions; result must stay in [-180,180]
        // after a single-step wrap each call.
        for _ in 0..50 {
            c.orbit(500.0, 0.0, 600.0);
            let lon = planet_state(&c).center_lon;
            assert!((-180.0..=180.0).contains(&lon), "lon out of range: {lon}");
        }
        for _ in 0..120 {
            c.orbit(-500.0, 0.0, 600.0);
            let lon = planet_state(&c).center_lon;
            assert!((-180.0..=180.0).contains(&lon), "lon out of range: {lon}");
        }
    }

    #[wasm_bindgen_test]
    fn orbit_site_wraps_bearing_and_clamps_elevation() {
        let mut c = site();
        // Drive bearing negative then positive; always normalized to [0,360).
        for _ in 0..40 {
            c.orbit(500.0, 0.0, 600.0);
            let b = site_state(&c).orbit_bearing;
            assert!((0.0..360.0).contains(&b), "bearing out of range: {b}");
        }
        // Elevation clamps to [5,175].
        c.orbit(0.0, 100000.0, 600.0);
        assert!(site_state(&c).orbit_elevation <= 175.0 + 1e-3);
        c.orbit(0.0, -200000.0, 600.0);
        assert!(site_state(&c).orbit_elevation >= 5.0 - 1e-3);
    }

    #[wasm_bindgen_test]
    fn orbit_noop_in_free_and_2d() {
        let mut f = free();
        let before = free_state(&f).free_pos;
        f.orbit(50.0, 50.0, 600.0);
        let after = free_state(&f).free_pos;
        assert!((before - after).length() < 1e-9);

        let mut two = Camera::default();
        two.orbit(50.0, 50.0, 600.0); // must not panic / change variant
        assert!(two.is_2d());
    }

    // ── tilt_rotate(): clamp + wrap + no-op ─────────────────────────────

    #[wasm_bindgen_test]
    fn tilt_rotate_clamps_tilt_and_wraps_rotation() {
        let mut c = planet();
        c.tilt_rotate(0.0, 1_000_000.0, 600.0); // huge pitch
        assert!(planet_state(&c).tilt <= 89.0 + 1e-3);
        c.tilt_rotate(0.0, -2_000_000.0, 600.0);
        assert!(planet_state(&c).tilt >= -89.0 - 1e-3);
        // Rotation stays wrapped within (-180,180] band after each call.
        let mut c2 = planet();
        for _ in 0..200 {
            c2.tilt_rotate(500.0, 0.0, 600.0);
            let r = planet_state(&c2).rotation;
            assert!((-180.0..=180.0).contains(&r), "rotation out of range: {r}");
        }
    }

    #[wasm_bindgen_test]
    fn tilt_rotate_noop_in_free_and_2d() {
        let mut f = free();
        let before = (free_state(&f).free_yaw, free_state(&f).free_pitch);
        f.tilt_rotate(40.0, 40.0, 600.0);
        let after = (free_state(&f).free_yaw, free_state(&f).free_pitch);
        assert!((before.0 - after.0).abs() < 1e-9 && (before.1 - after.1).abs() < 1e-9);

        let mut two = Camera::default();
        two.tilt_rotate(40.0, 40.0, 600.0);
        assert!(two.is_2d());
    }

    // ── free_look(): pitch clamp + yaw wrap + no-op ─────────────────────

    #[wasm_bindgen_test]
    fn free_look_clamps_pitch_and_wraps_yaw() {
        let mut c = free();
        // Large negative dy raises pitch toward +89 (pitch -= dy*...).
        c.free_look(0.0, -1_000_000.0, 600.0);
        assert!(free_state(&c).free_pitch <= 89.0 + 1e-3);
        c.free_look(0.0, 1_000_000.0, 600.0);
        assert!(free_state(&c).free_pitch >= -89.0 - 1e-3);
        // Yaw remains wrapped.
        for _ in 0..200 {
            c.free_look(500.0, 0.0, 600.0);
            let y = free_state(&c).free_yaw;
            assert!((-180.0..=180.0).contains(&y), "yaw out of range: {y}");
        }
    }

    #[wasm_bindgen_test]
    fn free_look_noop_in_orbit_and_2d() {
        let mut p = planet();
        let before = planet_state(&p).center_lat;
        p.free_look(40.0, 40.0, 600.0); // ignored in planet orbit
        assert!((planet_state(&p).center_lat - before).abs() < 1e-9);

        let mut two = Camera::default();
        two.free_look(40.0, 40.0, 600.0);
        assert!(two.is_2d());
    }

    // ── zoom(): free-look speed factor + clamp, 2D no-op ────────────────

    #[wasm_bindgen_test]
    fn zoom_freelook_scales_speed() {
        let mut c = free();
        // free_speed starts at DEFAULT_FREE_SPEED = 0.5; factor = 1 + 100*0.003 = 1.3
        c.zoom(100.0);
        assert!((free_state(&c).free_speed - 0.5 * 1.3).abs() < 1e-4);
    }

    #[wasm_bindgen_test]
    fn zoom_freelook_clamps_speed_range() {
        let mut c = free();
        for _ in 0..5000 {
            c.zoom(1000.0); // positive → speed up
        }
        assert!(free_state(&c).free_speed <= 50.0 + 1e-3);
        for _ in 0..10000 {
            c.zoom(-1000.0); // negative → slow down
        }
        assert!(free_state(&c).free_speed >= 0.01 - 1e-6);
    }

    #[wasm_bindgen_test]
    fn zoom_noop_in_2d() {
        let mut c = Camera::default();
        c.zoom(500.0);
        assert!(c.is_2d());
        // 2D zoom is handled on Flat2DState, not via Camera::zoom.
        assert!((c.flat_2d().unwrap().zoom - 1.0).abs() < 1e-6);
    }

    // ── recenter / reset / focus_site / align_north / level_horizon ─────

    #[wasm_bindgen_test]
    fn recenter_resets_orbit_angles_keeps_distance() {
        let mut c = planet();
        c.zoom(100.0); // change distance
        let dist = planet_state(&c).distance;
        c.tilt_rotate(30.0, 30.0, 600.0);
        c.move_pivot_to(10.0, 20.0);
        c.recenter();
        let s = planet_state(&c);
        // Center snaps back to site; tilt/rotation cleared; distance untouched.
        assert!((s.center_lat - 39.0).abs() < 1e-4);
        assert!((s.center_lon - (-98.0)).abs() < 1e-4);
        assert!(s.tilt.abs() < 1e-6 && s.rotation.abs() < 1e-6);
        assert!((s.distance - dist).abs() < 1e-6);
    }

    #[wasm_bindgen_test]
    fn reset_site_orbit_restores_defaults() {
        let mut c = site();
        c.zoom(200.0);
        c.orbit(40.0, 40.0, 600.0);
        c.reset();
        let s = site_state(&c);
        assert!((s.orbit_bearing - 180.0).abs() < 1e-5);
        assert!((s.orbit_elevation - 45.0).abs() < 1e-5);
        assert!((s.distance - DEFAULT_SITE_DISTANCE).abs() < 1e-5);
        assert!(s.tilt.abs() < 1e-6 && s.rotation.abs() < 1e-6);
    }

    #[wasm_bindgen_test]
    fn reset_freelook_restores_speed_and_points_at_center() {
        let mut c = free();
        c.zoom(100.0); // perturb speed
        c.free_move(2.0, 1.0, 0.0, 1.0); // perturb pos
        c.reset();
        let s = free_state(&c);
        assert!((s.free_speed - DEFAULT_FREE_SPEED).abs() < 1e-6);
        // free_pos is on the site ray at DEFAULT_SITE_DISTANCE from the origin.
        assert!((s.free_pos.length() - DEFAULT_SITE_DISTANCE).abs() < 1e-4);
    }

    #[wasm_bindgen_test]
    fn focus_site_planet_snaps_center_to_site() {
        let mut c = planet();
        c.move_pivot_to(10.0, 20.0);
        c.focus_site();
        let s = planet_state(&c);
        assert!((s.center_lat - s.common.site_lat).abs() < 1e-6);
        assert!((s.center_lon - s.common.site_lon).abs() < 1e-6);
    }

    #[wasm_bindgen_test]
    fn focus_site_site_orbit_sets_bearing_south() {
        let mut c = site();
        c.orbit(123.0, 0.0, 600.0); // move bearing off 180
        c.focus_site();
        assert!((site_state(&c).orbit_bearing - 180.0).abs() < 1e-6);
    }

    #[wasm_bindgen_test]
    fn align_north_clears_rotation() {
        let mut p = planet();
        p.tilt_rotate(40.0, 0.0, 600.0);
        p.align_north();
        assert!(planet_state(&p).rotation.abs() < 1e-6);

        let mut s = site();
        s.tilt_rotate(40.0, 40.0, 600.0);
        s.align_north();
        // Site orbit align_north clears both rotation and tilt.
        assert!(site_state(&s).rotation.abs() < 1e-6);
        assert!(site_state(&s).tilt.abs() < 1e-6);
    }

    #[wasm_bindgen_test]
    fn level_horizon_clears_tilt_or_pitch() {
        let mut p = planet();
        p.tilt_rotate(0.0, 40.0, 600.0);
        p.level_horizon();
        assert!(planet_state(&p).tilt.abs() < 1e-6);

        let mut f = free();
        f.free_look(0.0, -100.0, 600.0); // raise pitch
        f.level_horizon();
        assert!(free_state(&f).free_pitch.abs() < 1e-6);
    }

    // ── keyboard_move(): false on no input / 2d, true otherwise ─────────

    #[wasm_bindgen_test]
    fn keyboard_move_no_input_returns_false() {
        let mut c = planet();
        assert!(!c.keyboard_move(0.0, 0.0, 0.0, 1.0, 0.016));
    }

    #[wasm_bindgen_test]
    fn keyboard_move_2d_returns_false() {
        let mut c = Camera::default();
        assert!(!c.keyboard_move(1.0, 0.0, 0.0, 1.0, 0.016));
        assert!(c.is_2d());
    }

    #[wasm_bindgen_test]
    fn keyboard_move_planet_returns_true_and_moves() {
        let mut c = planet();
        let before = planet_state(&c).center_lat;
        assert!(c.keyboard_move(1.0, 0.0, 0.0, 1.0, 0.1));
        // forward>0 raises center_lat (before distance/zoom effects).
        assert!(planet_state(&c).center_lat > before);
    }

    #[wasm_bindgen_test]
    fn keyboard_move_site_wraps_bearing() {
        let mut c = site();
        // Large dt + right input drives bearing past 360; result normalized.
        for _ in 0..50 {
            assert!(c.keyboard_move(0.0, 1.0, 0.0, 4.0, 1.0));
            let b = site_state(&c).orbit_bearing;
            assert!((0.0..360.0).contains(&b), "bearing out of range: {b}");
        }
    }

    // ── set_site / set_aspect ───────────────────────────────────────────

    #[wasm_bindgen_test]
    fn set_site_updates_common_without_moving_view() {
        let mut c = site();
        let bearing_before = site_state(&c).orbit_bearing;
        c.set_site(12.0, 34.0);
        let s = site_state(&c);
        assert!((s.common.site_lat - 12.0).abs() < 1e-4);
        assert!((s.common.site_lon - 34.0).abs() < 1e-4);
        // View angle untouched.
        assert!((s.orbit_bearing - bearing_before).abs() < 1e-6);
    }

    #[wasm_bindgen_test]
    fn set_site_works_in_2d_via_seed() {
        // common_mut routes to the carried seed in 2D.
        let mut c = Camera::default();
        c.set_site(5.0, 6.0);
        c.switch_to_planet_orbit();
        let s = planet_state(&c);
        assert!((s.common.site_lat - 5.0).abs() < 1e-4);
        assert!((s.common.site_lon - 6.0).abs() < 1e-4);
    }

    #[wasm_bindgen_test]
    fn set_aspect_sets_ratio_and_guards_zero_height() {
        let mut c = planet();
        let rect = Rect::from_min_size(Pos2::new(0.0, 0.0), Vec2::new(800.0, 400.0));
        c.set_aspect(rect);
        // aspect = 800/400 = 2.0
        match &c {
            Camera::PlanetOrbit(s) => assert!((s.common.aspect - 2.0).abs() < 1e-5),
            _ => panic!("planet"),
        }
        // Zero-height rect leaves aspect unchanged (no div-by-zero).
        let bad = Rect::from_min_size(Pos2::new(0.0, 0.0), Vec2::new(800.0, 0.0));
        c.set_aspect(bad);
        match &c {
            Camera::PlanetOrbit(s) => assert!((s.common.aspect - 2.0).abs() < 1e-5),
            _ => panic!("planet"),
        }
    }

    // ── view_matrix / camera_world_pos 2D defaults ──────────────────────

    #[wasm_bindgen_test]
    fn view_matrix_identity_in_2d() {
        let c = Camera::default();
        let m = c.view_matrix();
        let id = Mat4::IDENTITY.to_cols_array();
        for (a, b) in m.to_cols_array().iter().zip(id.iter()) {
            assert!((a - b).abs() < 1e-9);
        }
    }

    #[wasm_bindgen_test]
    fn camera_world_pos_2d_default() {
        let c = Camera::default();
        let p = c.camera_world_pos();
        assert!(p.x.abs() < 1e-6 && p.y.abs() < 1e-6);
        assert!((p.z - DEFAULT_SITE_DISTANCE).abs() < 1e-6);
    }

    #[wasm_bindgen_test]
    fn camera_world_pos_freelook_is_free_pos() {
        let c = free();
        let s = free_state(&c);
        let p = c.camera_world_pos();
        assert!((p - s.free_pos).length() < 1e-6);
    }

    // ── static geo_to_world wrapper equals free fn ──────────────────────

    #[wasm_bindgen_test]
    fn static_geo_to_world_matches_inline_math() {
        // Spot-check: lat 0, lon 90 → +X axis.
        let v = Camera::geo_to_world(0.0, 90.0);
        assert!((v.x - 1.0).abs() < 1e-5);
        assert!(v.y.abs() < 1e-5);
        assert!(v.z.abs() < 1e-5);
    }

    // ── switch_to_* idempotency for site & free ─────────────────────────

    #[wasm_bindgen_test]
    fn switch_to_site_orbit_idempotent() {
        let mut c = site();
        c.orbit(33.0, 0.0, 600.0);
        let before = site_state(&c).orbit_bearing;
        c.switch_to_site_orbit(); // already site → no reconstruction
        assert!((site_state(&c).orbit_bearing - before).abs() < 1e-6);
    }

    #[wasm_bindgen_test]
    fn switch_to_free_look_idempotent() {
        let mut c = free();
        c.free_move(1.0, 0.0, 0.0, 1.0);
        let before = free_state(&c).free_pos;
        c.switch_to_free_look(); // already free → unchanged
        assert!((free_state(&c).free_pos - before).length() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn switch_to_3d_dispatches_each_mode() {
        let mut c = Camera::centered_on(39.0, -98.0);
        c.switch_to_3d(CameraMode::SiteOrbit);
        assert_eq!(c.camera_mode(), Some(CameraMode::SiteOrbit));
        c.switch_to_3d(CameraMode::FreeLook);
        assert_eq!(c.camera_mode(), Some(CameraMode::FreeLook));
        c.switch_to_3d(CameraMode::PlanetOrbit);
        assert_eq!(c.camera_mode(), Some(CameraMode::PlanetOrbit));
    }

    // ── move_pivot_to per-mode targets ──────────────────────────────────

    #[wasm_bindgen_test]
    fn move_pivot_to_planet_sets_center() {
        let mut c = planet();
        c.move_pivot_to(11.0, 22.0);
        let s = planet_state(&c);
        assert!((s.center_lat - 11.0).abs() < 1e-4);
        assert!((s.center_lon - 22.0).abs() < 1e-4);
        // Site (common) is NOT moved in planet orbit.
        assert!((s.common.site_lat - 39.0).abs() < 1e-4);
    }

    #[wasm_bindgen_test]
    fn move_pivot_to_site_moves_the_site() {
        let mut c = site();
        c.move_pivot_to(11.0, 22.0);
        let s = site_state(&c);
        // In site orbit, double-click moves the orbit pivot (the site itself).
        assert!((s.common.site_lat - 11.0).abs() < 1e-4);
        assert!((s.common.site_lon - 22.0).abs() < 1e-4);
    }
}
