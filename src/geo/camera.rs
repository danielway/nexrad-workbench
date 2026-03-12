//! Orbital camera for 3D globe rendering.
//!
//! Supports four camera modes: 2D top-down, planet orbit, site orbit, and free look.
//! Each mode has distinct mouse/keyboard controls following a consistent paradigm:
//! - Left mouse: primary navigation
//! - Right mouse: orientation adjustment
//! - Middle mouse / Shift+left: pan/translate
//! - Scroll: zoom or speed
//! - WASD / arrows: directional movement

use eframe::egui::{Pos2, Rect};
use glam::{Mat4, Vec3, Vec4};

/// Camera movement mode.
#[derive(Default, Clone, Copy, PartialEq, Eq)]
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

/// Orbital camera looking at a unit-sphere globe centered at the origin.
/// Uses (center_lat, center_lon) so North is always up in orbit modes.
#[derive(Clone)]
pub struct GlobeCamera {
    /// Latitude the camera is looking at (degrees, -90..90).
    pub center_lat: f32,

    /// Longitude the camera is looking at (degrees, -180..180).
    pub center_lon: f32,

    /// Distance from the camera to the globe center, in Earth radii.
    /// Must be > 1.0 (surface). Typical range 1.005 .. 20.0.
    pub distance: f32,

    /// Vertical field-of-view in radians.
    pub fov_y: f32,

    /// Viewport aspect ratio (width / height), updated each frame.
    pub aspect: f32,

    /// Active camera mode.
    pub mode: CameraMode,

    /// Site latitude for SiteOrbit mode (degrees).
    pub site_lat: f32,
    /// Site longitude for SiteOrbit mode (degrees).
    pub site_lon: f32,
    /// Bearing from site in SiteOrbit mode (degrees, 0=North, CW).
    pub orbit_bearing: f32,
    /// Elevation angle above horizon in SiteOrbit mode (degrees, 0=level, 90=directly above).
    pub orbit_elevation: f32,

    /// Camera tilt (pitch) in degrees. 0 = looking at globe center, positive = tilted up.
    /// Used in orbit modes via right-drag.
    pub tilt: f32,
    /// Camera rotation (yaw offset) in degrees. 0 = North up, positive = CW.
    pub rotation: f32,

    // ── Free Look state ──
    /// Camera position in world space (Free Look mode).
    pub free_pos: Vec3,
    /// Yaw angle in degrees (0 = looking along +Z, 90 = looking along +X).
    pub free_yaw: f32,
    /// Pitch angle in degrees (0 = level, positive = looking up).
    pub free_pitch: f32,
    /// Movement speed in Earth radii per second (Free Look mode).
    pub free_speed: f32,
}

impl Default for GlobeCamera {
    fn default() -> Self {
        Self {
            center_lat: 0.0,
            center_lon: 0.0,
            distance: DEFAULT_SITE_DISTANCE,
            fov_y: std::f32::consts::FRAC_PI_4, // 45°
            aspect: 1.0,
            mode: CameraMode::default(),
            site_lat: 0.0,
            site_lon: 0.0,
            orbit_bearing: 180.0,
            orbit_elevation: 45.0,
            tilt: 0.0,
            rotation: 0.0,
            free_pos: Vec3::new(0.0, 0.0, DEFAULT_SITE_DISTANCE),
            free_yaw: 0.0,
            free_pitch: 0.0,
            free_speed: 0.5,
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

#[allow(dead_code)]
impl GlobeCamera {
    /// Create a camera centered on the given geographic coordinates.
    pub fn centered_on(lat_deg: f64, lon_deg: f64) -> Self {
        let mut cam = Self::default();
        cam.center_on(lat_deg, lon_deg);
        cam
    }

    // ── Matrices ────────────────────────────────────────────────

    /// Build the rotation matrix that places `(center_lat, center_lon)` facing the camera.
    fn globe_rotation_matrix(&self) -> Mat4 {
        // Rotate world so that (center_lat, center_lon) ends up at +Z (facing camera).
        // 1. Rotate around Y by -lon → brings the target longitude to the prime meridian.
        //    After this, the target is at (0, sin(lat), cos(lat)).
        // 2. Rotate around X by +lat → brings (0, sin(lat), cos(lat)) to (0, 0, 1).
        let lat = self.center_lat.to_radians();
        let lon = self.center_lon.to_radians();
        Mat4::from_rotation_x(lat) * Mat4::from_rotation_y(-lon)
    }

    /// View matrix (world → eye).
    pub fn view_matrix(&self) -> Mat4 {
        match self.mode {
            CameraMode::PlanetOrbit => {
                let eye = Vec3::new(0.0, 0.0, self.distance);
                let look_at = Mat4::look_at_rh(eye, Vec3::ZERO, Vec3::Y);
                let base = look_at * self.globe_rotation_matrix();

                // Apply tilt (pitch) and rotation (yaw) from right-drag.
                if self.tilt != 0.0 || self.rotation != 0.0 {
                    let tilt_mat = Mat4::from_rotation_x(self.tilt.to_radians());
                    let rot_mat = Mat4::from_rotation_z(self.rotation.to_radians());
                    rot_mat * tilt_mat * base
                } else {
                    base
                }
            }
            CameraMode::SiteOrbit => self.site_orbit_view_matrix(),
            CameraMode::FreeLook => self.free_look_view_matrix(),
        }
    }

    /// View matrix for SiteOrbit mode — camera orbits around the radar site.
    fn site_orbit_view_matrix(&self) -> Mat4 {
        let site_pos = Self::geo_to_world(self.site_lat as f64, self.site_lon as f64);
        let site_dist = self.distance - 1.0; // distance from the site surface
        let orbit_dist = site_dist.max(0.05);

        // Compute camera position by offsetting from the site along bearing/elevation
        let bearing_rad = self.orbit_bearing.to_radians();
        let elev_rad = self.orbit_elevation.to_radians();

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

    /// View matrix for FreeLook mode — first-person flying camera.
    fn free_look_view_matrix(&self) -> Mat4 {
        let yaw = self.free_yaw.to_radians();
        let pitch = self.free_pitch.to_radians();

        // Forward direction from yaw and pitch
        let forward = Vec3::new(
            yaw.sin() * pitch.cos(),
            pitch.sin(),
            yaw.cos() * pitch.cos(),
        );

        let target = self.free_pos + forward;
        Mat4::look_at_rh(self.free_pos, target, Vec3::Y)
    }

    /// Perspective projection matrix.
    pub fn projection_matrix(&self) -> Mat4 {
        // Adjust near plane based on distance — when very close, use tighter near plane
        let effective_dist = match self.mode {
            CameraMode::FreeLook => self.free_pos.length(),
            _ => self.distance,
        };
        let near = if effective_dist < 1.1 { 0.0001 } else { 0.01 };
        Mat4::perspective_rh_gl(self.fov_y, self.aspect, near, 100.0)
    }

    /// Combined view-projection matrix.
    pub fn view_projection(&self) -> Mat4 {
        self.projection_matrix() * self.view_matrix()
    }

    // ── Coordinate conversions ──────────────────────────────────

    /// Convert geographic (lat°, lon°) to a point on the unit sphere.
    pub fn geo_to_world(lat_deg: f64, lon_deg: f64) -> Vec3 {
        let lat = (lat_deg as f32).to_radians();
        let lon = (lon_deg as f32).to_radians();
        Vec3::new(lat.cos() * lon.sin(), lat.sin(), lat.cos() * lon.cos())
    }

    /// Camera position in world space.
    pub fn camera_world_pos(&self) -> Vec3 {
        match self.mode {
            CameraMode::PlanetOrbit => {
                // Camera sits at (0,0,distance) in view space; invert the globe rotation
                let inv_rot = self.globe_rotation_matrix().inverse();
                (inv_rot * Vec4::new(0.0, 0.0, self.distance, 1.0)).truncate()
            }
            CameraMode::SiteOrbit => {
                let site_pos = Self::geo_to_world(self.site_lat as f64, self.site_lon as f64);
                let site_dist = (self.distance - 1.0).max(0.05);
                let bearing_rad = self.orbit_bearing.to_radians();
                let elev_rad = self.orbit_elevation.to_radians();
                let up = site_pos.normalize();
                let ref_vec = if up.y.abs() > 0.99 { Vec3::Z } else { Vec3::Y };
                let east = ref_vec.cross(up).normalize();
                let north = up.cross(east).normalize();
                let horiz = north * bearing_rad.cos() + east * bearing_rad.sin();
                let offset_dir = (horiz * elev_rad.cos() + up * elev_rad.sin()).normalize();
                site_pos + offset_dir * site_dist
            }
            CameraMode::FreeLook => self.free_pos,
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
        self.world_to_screen(Self::geo_to_world(lat_deg, lon_deg), screen_rect)
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

    // ── Controls ────────────────────────────────────────────────

    /// Orbit the globe by screen-space delta (pixels).
    /// Planet Orbit: rotates the globe. Site Orbit: changes bearing/elevation.
    pub fn orbit(&mut self, dx: f32, dy: f32, viewport_height: f32) {
        let sensitivity = self.fov_y / viewport_height;

        match self.mode {
            CameraMode::PlanetOrbit => {
                // Grab-and-drag: dragging right moves the globe right (center goes west).
                // Scale sensitivity by distance so close-up panning feels natural.
                let dist_scale = (self.distance - 1.0).max(0.01);
                let dlon = dx * sensitivity * (180.0 / std::f32::consts::PI) * dist_scale;
                let dlat = dy * sensitivity * (180.0 / std::f32::consts::PI) * dist_scale;

                self.center_lon -= dlon;
                self.center_lat += dlat;

                // Clamp latitude to avoid flipping
                self.center_lat = self.center_lat.clamp(-89.9, 89.9);
                // Wrap longitude
                if self.center_lon > 180.0 {
                    self.center_lon -= 360.0;
                }
                if self.center_lon < -180.0 {
                    self.center_lon += 360.0;
                }
            }
            CameraMode::SiteOrbit => {
                // Grab-and-drag: dragging right orbits camera to the right (bearing decreases).
                let dbearing = -dx * sensitivity * (180.0 / std::f32::consts::PI);
                let delevation = -dy * sensitivity * (180.0 / std::f32::consts::PI);

                self.orbit_bearing = (self.orbit_bearing + dbearing) % 360.0;
                if self.orbit_bearing < 0.0 {
                    self.orbit_bearing += 360.0;
                }
                self.orbit_elevation = (self.orbit_elevation + delevation).clamp(5.0, 175.0);
            }
            CameraMode::FreeLook => {
                // In free look, orbit doesn't apply — use look() instead
            }
        }
    }

    /// Adjust camera tilt (pitch) and rotation (yaw) by screen-space delta.
    /// Used by right-drag in orbit modes.
    pub fn tilt_rotate(&mut self, dx: f32, dy: f32, viewport_height: f32) {
        let sensitivity = self.fov_y / viewport_height;
        let deg_per_rad = 180.0 / std::f32::consts::PI;

        self.rotation += dx * sensitivity * deg_per_rad;
        self.tilt += dy * sensitivity * deg_per_rad;

        // Clamp tilt to avoid flipping
        self.tilt = self.tilt.clamp(-89.0, 89.0);
        // Wrap rotation
        if self.rotation > 180.0 {
            self.rotation -= 360.0;
        }
        if self.rotation < -180.0 {
            self.rotation += 360.0;
        }
    }

    /// Adjust free look direction (yaw/pitch) by screen-space delta.
    /// Used by left-drag in Free Look mode and right-drag (orientation) in Free Look.
    pub fn free_look(&mut self, dx: f32, dy: f32, viewport_height: f32) {
        let sensitivity = self.fov_y / viewport_height;
        let deg_per_rad = 180.0 / std::f32::consts::PI;

        self.free_yaw += dx * sensitivity * deg_per_rad;
        self.free_pitch -= dy * sensitivity * deg_per_rad;

        self.free_pitch = self.free_pitch.clamp(-89.0, 89.0);
        // Wrap yaw
        if self.free_yaw > 180.0 {
            self.free_yaw -= 360.0;
        }
        if self.free_yaw < -180.0 {
            self.free_yaw += 360.0;
        }
    }

    /// Move the free look camera by a directional vector relative to the camera.
    /// `forward` = along look direction, `right` = perpendicular, `up` = world up.
    /// `dt` is frame delta time in seconds.
    pub fn free_move(&mut self, forward: f32, right: f32, up: f32, dt: f32) {
        let yaw = self.free_yaw.to_radians();
        let pitch = self.free_pitch.to_radians();

        let fwd = Vec3::new(
            yaw.sin() * pitch.cos(),
            pitch.sin(),
            yaw.cos() * pitch.cos(),
        );
        let world_up = Vec3::Y;
        let right_dir = fwd.cross(world_up).normalize();
        // Camera-relative up (perpendicular to forward and right)
        let up_dir = right_dir.cross(fwd).normalize();

        let velocity = self.free_speed * dt;
        self.free_pos += fwd * forward * velocity;
        self.free_pos += right_dir * right * velocity;
        self.free_pos += up_dir * up * velocity;
    }

    /// Pan the orbit pivot by screen-space delta (middle mouse drag).
    /// In Planet Orbit, this shifts the center lat/lon.
    /// In Site Orbit, this shifts the orbit pivot slightly.
    pub fn pan_pivot(&mut self, dx: f32, dy: f32, viewport_height: f32) {
        // Same as orbit for now — middle-drag shifts the center
        self.orbit(dx, dy, viewport_height);
    }

    /// Translate the free look camera sideways relative to the screen plane.
    /// Used by middle-drag in Free Look mode.
    pub fn free_translate(&mut self, dx: f32, dy: f32, viewport_height: f32) {
        let yaw = self.free_yaw.to_radians();
        let pitch = self.free_pitch.to_radians();

        let fwd = Vec3::new(
            yaw.sin() * pitch.cos(),
            pitch.sin(),
            yaw.cos() * pitch.cos(),
        );
        let world_up = Vec3::Y;
        let right_dir = fwd.cross(world_up).normalize();
        let up_dir = right_dir.cross(fwd).normalize();

        let sensitivity = self.fov_y / viewport_height;
        let dist_scale = (self.free_pos.length() - 1.0).max(0.01);
        let scale = sensitivity * dist_scale;

        self.free_pos -= right_dir * dx * scale;
        self.free_pos += up_dir * dy * scale;
    }

    /// Zoom by scroll delta. Positive = zoom in (closer).
    /// Uses exponential scaling so zooming feels consistent at all distances.
    pub fn zoom(&mut self, delta: f32) {
        if self.mode == CameraMode::FreeLook {
            // In free look, scroll adjusts movement speed
            let factor = 1.0 + delta * 0.003;
            self.free_speed = (self.free_speed * factor).clamp(0.01, 50.0);
            return;
        }

        // Convert distance to log space, shift, convert back.
        // This makes each scroll tick a consistent percentage change.
        let log_dist = self.distance.ln();
        let log_min = MIN_DISTANCE.ln();
        let log_max = MAX_DISTANCE.ln();

        // Each scroll unit moves ~0.3% in log space (tuned for smooth feel)
        let new_log = log_dist - delta * 0.003;
        self.distance = new_log.clamp(log_min, log_max).exp();
    }

    /// Rotate the globe so that the given lat/lon faces the camera, and reset distance.
    pub fn center_on(&mut self, lat_deg: f64, lon_deg: f64) {
        self.center_lat = lat_deg as f32;
        self.center_lon = lon_deg as f32;
        self.site_lat = lat_deg as f32;
        self.site_lon = lon_deg as f32;
        self.distance = DEFAULT_SITE_DISTANCE;
        self.orbit_bearing = 180.0;
        self.orbit_elevation = 45.0;
        self.tilt = 0.0;
        self.rotation = 0.0;
        // Initialize free look at a reasonable position
        let pos = Self::geo_to_world(lat_deg, lon_deg) * DEFAULT_SITE_DISTANCE;
        self.free_pos = pos;
        self.free_yaw = (-lon_deg as f32 + 180.0) % 360.0 - 180.0;
        self.free_pitch = -(lat_deg as f32);
    }

    /// Re-center on the site without changing distance or zoom level.
    pub fn recenter(&mut self) {
        self.center_lat = self.site_lat;
        self.center_lon = self.site_lon;
        self.orbit_bearing = 180.0;
        self.orbit_elevation = 45.0;
        self.tilt = 0.0;
        self.rotation = 0.0;
    }

    /// Reset camera to a safe default for the current mode.
    /// R key handler.
    pub fn reset(&mut self) {
        match self.mode {
            CameraMode::PlanetOrbit => {
                self.center_lat = self.site_lat;
                self.center_lon = self.site_lon;
                self.distance = DEFAULT_SITE_DISTANCE;
                self.tilt = 0.0;
                self.rotation = 0.0;
            }
            CameraMode::SiteOrbit => {
                self.orbit_bearing = 180.0;
                self.orbit_elevation = 45.0;
                self.distance = DEFAULT_SITE_DISTANCE;
                self.tilt = 0.0;
                self.rotation = 0.0;
            }
            CameraMode::FreeLook => {
                // Reset to a default vantage point above the radar site
                let pos = Self::geo_to_world(self.site_lat as f64, self.site_lon as f64)
                    * DEFAULT_SITE_DISTANCE;
                self.free_pos = pos;
                // Look toward the globe center
                let dir = -pos.normalize();
                self.free_yaw = dir.x.atan2(dir.z).to_degrees();
                self.free_pitch = dir.y.asin().to_degrees();
                self.free_speed = 0.5;
            }
        }
    }

    /// Focus camera on the radar site. F key handler.
    pub fn focus_site(&mut self) {
        match self.mode {
            CameraMode::PlanetOrbit => {
                self.center_lat = self.site_lat;
                self.center_lon = self.site_lon;
            }
            CameraMode::SiteOrbit => {
                // Already orbiting the site; snap to looking north (camera south)
                self.orbit_bearing = 180.0;
            }
            CameraMode::FreeLook => {
                // Move camera near the site and point toward it
                let site_pos = Self::geo_to_world(self.site_lat as f64, self.site_lon as f64);
                self.free_pos = site_pos * 2.0;
                let dir = (site_pos - self.free_pos).normalize();
                self.free_yaw = dir.x.atan2(dir.z).to_degrees();
                self.free_pitch = dir.y.asin().to_degrees();
            }
        }
    }

    /// Align camera so North is up. N key handler.
    pub fn align_north(&mut self) {
        self.rotation = 0.0;
        if self.mode == CameraMode::SiteOrbit {
            // Keep current bearing but remove tilt
            self.tilt = 0.0;
        }
    }

    /// Level the horizon. L key handler.
    pub fn level_horizon(&mut self) {
        self.tilt = 0.0;
        if self.mode == CameraMode::FreeLook {
            self.free_pitch = 0.0;
        }
    }

    /// Move pivot/center to a specific geographic point. Used for double-click.
    pub fn move_pivot_to(&mut self, lat_deg: f64, lon_deg: f64) {
        match self.mode {
            CameraMode::PlanetOrbit => {
                self.center_lat = lat_deg as f32;
                self.center_lon = lon_deg as f32;
            }
            CameraMode::SiteOrbit => {
                // In site orbit, double-click moves the orbit pivot
                self.site_lat = lat_deg as f32;
                self.site_lon = lon_deg as f32;
            }
            CameraMode::FreeLook => {
                // In free look, move to the clicked point
                let target = Self::geo_to_world(lat_deg, lon_deg);
                // Position camera at current distance from the clicked point
                let dist = self.free_pos.length();
                self.free_pos = target * dist;
                let dir = (target - self.free_pos).normalize();
                self.free_yaw = dir.x.atan2(dir.z).to_degrees();
                self.free_pitch = dir.y.asin().to_degrees();
            }
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

        match self.mode {
            CameraMode::PlanetOrbit => {
                // WASD/arrows pan the globe (same as lat/lon drag)
                let speed = base_speed * speed_mult * dt;
                // W = camera looks further north → center_lat increases
                self.center_lat += forward * speed * 0.5;
                // D = camera looks further east → center_lon increases
                self.center_lon += right * speed * 0.5;

                self.center_lat = self.center_lat.clamp(-89.9, 89.9);
                if self.center_lon > 180.0 {
                    self.center_lon -= 360.0;
                }
                if self.center_lon < -180.0 {
                    self.center_lon += 360.0;
                }

                // W/S also zoom in Planet Orbit per the spec
                if forward != 0.0 {
                    let zoom_speed = 1.0 * speed_mult * dt;
                    let log_dist = self.distance.ln();
                    let new_log = log_dist - forward * zoom_speed;
                    self.distance = new_log.clamp(MIN_DISTANCE.ln(), MAX_DISTANCE.ln()).exp();
                }
            }
            CameraMode::SiteOrbit => {
                // A/D rotate horizontally around site, W/S adjust distance
                let speed = base_speed * speed_mult * dt;
                self.orbit_bearing = (self.orbit_bearing + right * speed) % 360.0;
                if self.orbit_bearing < 0.0 {
                    self.orbit_bearing += 360.0;
                }

                // W/S adjust distance
                if forward != 0.0 {
                    let zoom_speed = 1.0 * speed_mult * dt;
                    let log_dist = self.distance.ln();
                    let new_log = log_dist - forward * zoom_speed;
                    self.distance = new_log.clamp(MIN_DISTANCE.ln(), MAX_DISTANCE.ln()).exp();
                }

                // Q/E roll the camera
                if up_down != 0.0 {
                    self.rotation += up_down * speed * 0.5;
                    if self.rotation > 180.0 {
                        self.rotation -= 360.0;
                    }
                    if self.rotation < -180.0 {
                        self.rotation += 360.0;
                    }
                }
            }
            CameraMode::FreeLook => {
                // WASD = standard FPS movement
                self.free_move(forward, right, up_down, dt * speed_mult);
            }
        }
        true
    }

    /// Reset pivot to default (Home key). Earth center for planet orbit, site for site orbit.
    pub fn reset_pivot(&mut self) {
        match self.mode {
            CameraMode::PlanetOrbit => {
                self.center_lat = self.site_lat;
                self.center_lon = self.site_lon;
            }
            CameraMode::SiteOrbit => {
                // Reset orbit pivot to site location
                // (site_lat/site_lon already point to the site)
            }
            CameraMode::FreeLook => {
                self.focus_site();
            }
        }
    }

    /// Update the site position (for SiteOrbit mode) without moving the camera view.
    pub fn set_site(&mut self, lat_deg: f64, lon_deg: f64) {
        self.site_lat = lat_deg as f32;
        self.site_lon = lon_deg as f32;
    }

    /// Update aspect ratio from the current viewport.
    pub fn set_aspect(&mut self, screen_rect: Rect) {
        let w = screen_rect.width();
        let h = screen_rect.height();
        if h > 0.0 {
            self.aspect = w / h;
        }
    }

    /// Switch to a specific camera mode, preserving reasonable state.
    pub fn switch_mode(&mut self, new_mode: CameraMode) {
        if self.mode == new_mode {
            return;
        }

        // When entering Free Look from an orbit mode, initialize free look state
        // from the current orbit camera position and look direction.
        if new_mode == CameraMode::FreeLook {
            let cam_pos = self.camera_world_pos();
            self.free_pos = cam_pos;

            // Look direction: toward the orbit center (globe center for planet, site for site)
            let look_target = match self.mode {
                CameraMode::SiteOrbit => {
                    Self::geo_to_world(self.site_lat as f64, self.site_lon as f64)
                }
                _ => Self::geo_to_world(self.center_lat as f64, self.center_lon as f64),
            };
            let dir = (look_target - cam_pos).normalize();
            self.free_yaw = dir.x.atan2(dir.z).to_degrees();
            self.free_pitch = dir.y.asin().to_degrees();
        }

        // When leaving Free Look, set orbit parameters from current free look position
        if self.mode == CameraMode::FreeLook && new_mode != CameraMode::FreeLook {
            self.distance = self.free_pos.length().clamp(MIN_DISTANCE, MAX_DISTANCE);

            // Convert position to lat/lon for orbit center
            let pos = self.free_pos.normalize();
            let lat = pos.y.asin().to_degrees();
            let lon = pos.x.atan2(pos.z).to_degrees();
            self.center_lat = lat;
            self.center_lon = lon;
            self.tilt = 0.0;
            self.rotation = 0.0;
        }

        self.mode = new_mode;
    }
}
