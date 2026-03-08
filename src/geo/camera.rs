//! Orbital camera for 3D globe rendering.
//!
//! Provides view/projection matrices, geographic ↔ screen coordinate
//! conversion, and orbit/zoom controls for navigating a unit-sphere Earth.

use eframe::egui::{Pos2, Rect};
use glam::{Mat4, Quat, Vec3, Vec4};

/// Orbital camera looking at a unit-sphere globe centered at the origin.
#[derive(Clone)]
pub struct GlobeCamera {
    /// Rotation applied to the globe (which lat/lon faces the camera).
    /// Identity = (0°N, 0°E) faces the camera.
    pub rotation: Quat,

    /// Distance from the camera to the globe center, in Earth radii.
    /// Must be > 1.0 (surface). Typical range 1.5 .. 20.0.
    pub distance: f32,

    /// Vertical field-of-view in radians.
    pub fov_y: f32,

    /// Viewport aspect ratio (width / height), updated each frame.
    pub aspect: f32,
}

impl Default for GlobeCamera {
    fn default() -> Self {
        Self {
            rotation: Quat::IDENTITY,
            distance: 3.0,
            fov_y: std::f32::consts::FRAC_PI_4, // 45°
            aspect: 1.0,
        }
    }
}

// Distance clamp range (Earth radii).
const MIN_DISTANCE: f32 = 1.15;
const MAX_DISTANCE: f32 = 20.0;

impl GlobeCamera {
    /// Create a camera centered on the given geographic coordinates.
    pub fn centered_on(lat_deg: f64, lon_deg: f64) -> Self {
        let mut cam = Self::default();
        cam.center_on(lat_deg, lon_deg);
        cam
    }

    // ── Matrices ────────────────────────────────────────────────

    /// View matrix (world → eye).
    pub fn view_matrix(&self) -> Mat4 {
        // Camera sits at (0, 0, distance) looking toward origin.
        // Globe is rotated so the desired lat/lon faces the camera.
        let eye = Vec3::new(0.0, 0.0, self.distance);
        let look_at = Mat4::look_at_rh(eye, Vec3::ZERO, Vec3::Y);
        // Apply inverse of globe rotation to the view (rotate world, not camera).
        look_at * Mat4::from_quat(self.rotation.inverse())
    }

    /// Perspective projection matrix.
    pub fn projection_matrix(&self) -> Mat4 {
        Mat4::perspective_rh_gl(self.fov_y, self.aspect, 0.01, 100.0)
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

    /// Project a 3D world position to screen coordinates.
    /// Returns `None` if the point is on the far side of the globe.
    pub fn world_to_screen(&self, pos: Vec3, screen_rect: Rect) -> Option<Pos2> {
        // Back-face test: point must face the camera.
        let cam_pos = self.rotation * Vec3::new(0.0, 0.0, self.distance);
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
    /// `dx` rotates around Y (longitude), `dy` around the camera's X (latitude).
    pub fn orbit(&mut self, dx: f32, dy: f32, viewport_height: f32) {
        let sensitivity = self.fov_y / viewport_height;
        let angle_x = -dx * sensitivity;
        let angle_y = -dy * sensitivity;

        // Rotate around world Y for longitude change
        let rot_y = Quat::from_rotation_y(angle_x);
        // Rotate around camera-local X for latitude change
        let right = self.rotation * Vec3::X;
        let rot_x = Quat::from_axis_angle(right, angle_y);

        self.rotation = (rot_x * rot_y * self.rotation).normalize();
    }

    /// Zoom by scroll delta. Positive = zoom in (closer).
    pub fn zoom(&mut self, delta: f32) {
        let factor = 1.0 - delta * 0.001;
        self.distance = (self.distance * factor).clamp(MIN_DISTANCE, MAX_DISTANCE);
    }

    /// Rotate the globe so that the given lat/lon faces the camera, and reset distance.
    pub fn center_on(&mut self, lat_deg: f64, lon_deg: f64) {
        // We need a rotation that maps geo_to_world(lat, lon) to (0, 0, 1) — facing the camera.
        let target = Self::geo_to_world(lat_deg, lon_deg);
        let forward = Vec3::Z; // default camera look direction
        self.rotation = Quat::from_rotation_arc(target, forward);
        self.distance = 3.0;
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
