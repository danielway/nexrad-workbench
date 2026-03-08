//! Orbital camera for 3D globe rendering.
//!
//! Uses a lat/lon-based model so North always stays up. Supports three
//! camera modes: planet orbit, site orbit, and free look.

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
    /// Free look: unconstrained pan and zoom.
    FreeLook,
}

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
}

/// Orbital camera looking at a unit-sphere globe centered at the origin.
/// Uses (center_lat, center_lon) so North is always up.
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
    /// Used in Free Look mode, or via Shift+drag in any mode.
    pub tilt: f32,
    /// Camera rotation (yaw offset) in degrees. 0 = North up, positive = CW.
    pub rotation: f32,
}

impl Default for GlobeCamera {
    fn default() -> Self {
        Self {
            center_lat: 0.0,
            center_lon: 0.0,
            distance: 3.0,
            fov_y: std::f32::consts::FRAC_PI_4, // 45°
            aspect: 1.0,
            mode: CameraMode::default(),
            site_lat: 0.0,
            site_lon: 0.0,
            orbit_bearing: 0.0,
            orbit_elevation: 45.0,
            tilt: 0.0,
            rotation: 0.0,
        }
    }
}

// Distance clamp range (Earth radii).
// 1.005 allows very close zoom (roughly matching 2D view detail levels).
const MIN_DISTANCE: f32 = 1.005;
const MAX_DISTANCE: f32 = 20.0;

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
            CameraMode::PlanetOrbit | CameraMode::FreeLook => {
                let eye = Vec3::new(0.0, 0.0, self.distance);
                let look_at = Mat4::look_at_rh(eye, Vec3::ZERO, Vec3::Y);
                let base = look_at * self.globe_rotation_matrix();

                // Apply tilt (pitch) and rotation (yaw) for Free Look.
                // These are also set via Shift+drag in any mode.
                if self.tilt != 0.0 || self.rotation != 0.0 {
                    let tilt_mat = Mat4::from_rotation_x(self.tilt.to_radians());
                    let rot_mat = Mat4::from_rotation_z(self.rotation.to_radians());
                    rot_mat * tilt_mat * base
                } else {
                    base
                }
            }
            CameraMode::SiteOrbit => self.site_orbit_view_matrix(),
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
        // Use geographic north as the up hint so the view orientation matches the compass.
        Mat4::look_at_rh(eye, site_pos, north)
    }

    /// Perspective projection matrix.
    pub fn projection_matrix(&self) -> Mat4 {
        // Adjust near plane based on distance — when very close, use tighter near plane
        let near = if self.distance < 1.1 { 0.0001 } else { 0.01 };
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
    fn camera_world_pos(&self) -> Vec3 {
        match self.mode {
            CameraMode::PlanetOrbit | CameraMode::FreeLook => {
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
    /// Behavior depends on the active camera mode.
    pub fn orbit(&mut self, dx: f32, dy: f32, viewport_height: f32) {
        let sensitivity = self.fov_y / viewport_height;

        match self.mode {
            CameraMode::PlanetOrbit | CameraMode::FreeLook => {
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
        }
    }

    /// Adjust camera tilt (pitch) and rotation (yaw) by screen-space delta.
    /// Available via Shift+drag in any mode, or primary drag in Free Look.
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

    /// Zoom by scroll delta. Positive = zoom in (closer).
    /// Uses exponential scaling so zooming feels consistent at all distances.
    pub fn zoom(&mut self, delta: f32) {
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
        self.distance = 3.0;
        self.orbit_bearing = 0.0;
        self.orbit_elevation = 45.0;
        self.tilt = 0.0;
        self.rotation = 0.0;
    }

    /// Re-center on the site without changing distance or zoom level.
    pub fn recenter(&mut self) {
        self.center_lat = self.site_lat;
        self.center_lon = self.site_lon;
        self.orbit_bearing = 0.0;
        self.orbit_elevation = 45.0;
        self.tilt = 0.0;
        self.rotation = 0.0;
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
}
