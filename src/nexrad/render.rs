//! Custom NEXRAD rendering using egui's Painter API.
//!
//! Since nexrad-render requires Cairo (not WASM-compatible), this module
//! provides a custom rendering implementation that works in the browser.

use crate::geo::MapProjection;
use eframe::egui::{Color32, Painter, Pos2, Stroke};
use geo_types::Coord;
use std::f64::consts::PI;

/// NWS standard reflectivity color palette.
///
/// Maps dBZ values to colors following the National Weather Service
/// standard color scheme for reflectivity data.
pub struct ReflectivityPalette {
    /// Color lookup table indexed by dBZ value + offset
    colors: Vec<Color32>,
    /// Minimum dBZ value in the palette
    min_dbz: f32,
    /// Maximum dBZ value in the palette
    max_dbz: f32,
}

impl Default for ReflectivityPalette {
    fn default() -> Self {
        Self::nws_standard()
    }
}

impl ReflectivityPalette {
    /// Creates the NWS standard reflectivity palette.
    ///
    /// Color scheme:
    /// - < 5 dBZ: Transparent (no precipitation)
    /// - 5-15 dBZ: Light cyan/green (light precipitation)
    /// - 15-30 dBZ: Green to yellow (moderate rain)
    /// - 30-45 dBZ: Yellow to red (heavy rain)
    /// - 45-60 dBZ: Red to magenta (severe)
    /// - > 60 dBZ: Magenta to white (extreme)
    pub fn nws_standard() -> Self {
        let mut colors = Vec::with_capacity(80);

        for dbz in -5..75 {
            let color = match dbz {
                d if d < 5 => Color32::TRANSPARENT,
                d if d < 10 => Color32::from_rgba_unmultiplied(0, 236, 236, 180), // Light cyan
                d if d < 15 => Color32::from_rgba_unmultiplied(1, 160, 246, 180), // Cyan
                d if d < 20 => Color32::from_rgba_unmultiplied(0, 0, 246, 180),   // Blue
                d if d < 25 => Color32::from_rgba_unmultiplied(0, 255, 0, 180),   // Light green
                d if d < 30 => Color32::from_rgba_unmultiplied(0, 200, 0, 180),   // Green
                d if d < 35 => Color32::from_rgba_unmultiplied(0, 144, 0, 180),   // Dark green
                d if d < 40 => Color32::from_rgba_unmultiplied(255, 255, 0, 200), // Yellow
                d if d < 45 => Color32::from_rgba_unmultiplied(231, 192, 0, 200), // Gold
                d if d < 50 => Color32::from_rgba_unmultiplied(255, 144, 0, 200), // Orange
                d if d < 55 => Color32::from_rgba_unmultiplied(255, 0, 0, 220),   // Red
                d if d < 60 => Color32::from_rgba_unmultiplied(214, 0, 0, 220),   // Dark red
                d if d < 65 => Color32::from_rgba_unmultiplied(192, 0, 0, 220),   // Maroon
                d if d < 70 => Color32::from_rgba_unmultiplied(255, 0, 255, 240), // Magenta
                _ => Color32::from_rgba_unmultiplied(255, 255, 255, 255),         // White
            };
            colors.push(color);
        }

        Self {
            colors,
            min_dbz: -5.0,
            max_dbz: 75.0,
        }
    }

    /// Gets the color for a given dBZ value.
    pub fn get_color(&self, dbz: f32) -> Color32 {
        if dbz < self.min_dbz || dbz > self.max_dbz {
            return Color32::TRANSPARENT;
        }

        let index = ((dbz - self.min_dbz) as usize).min(self.colors.len() - 1);
        self.colors[index]
    }
}

/// Decoded sweep data ready for rendering.
pub struct DecodedSweep {
    /// Elevation angle in degrees
    pub elevation: f32,
    /// Radials in this sweep
    pub radials: Vec<DecodedRadial>,
}

/// A single radial of decoded data.
pub struct DecodedRadial {
    /// Azimuth angle in degrees (0 = North, clockwise)
    pub azimuth: f32,
    /// Azimuth spacing (half-width) in degrees
    pub azimuth_spacing: f32,
    /// Distance to first gate in km
    pub first_gate_km: f32,
    /// Gate spacing in km
    pub gate_spacing_km: f32,
    /// Reflectivity values for each gate (dBZ)
    pub reflectivity: Vec<f32>,
}

/// Renders a decoded sweep to the canvas.
///
/// Each radial is rendered as a series of wedge-shaped gates,
/// colored according to the reflectivity palette.
pub fn render_sweep(
    painter: &Painter,
    projection: &MapProjection,
    sweep: &DecodedSweep,
    radar_lat: f64,
    radar_lon: f64,
    palette: &ReflectivityPalette,
    max_range_km: f32,
) {
    for radial in &sweep.radials {
        render_radial(
            painter,
            projection,
            radial,
            radar_lat,
            radar_lon,
            palette,
            max_range_km,
        );
    }
}

/// Renders a single radial as a series of gate wedges.
fn render_radial(
    painter: &Painter,
    projection: &MapProjection,
    radial: &DecodedRadial,
    radar_lat: f64,
    radar_lon: f64,
    palette: &ReflectivityPalette,
    max_range_km: f32,
) {
    let azimuth_rad = (radial.azimuth as f64 - 90.0) * PI / 180.0;
    let half_width_rad = (radial.azimuth_spacing as f64) * PI / 180.0;

    let left_az = azimuth_rad - half_width_rad;
    let right_az = azimuth_rad + half_width_rad;

    for (idx, &dbz) in radial.reflectivity.iter().enumerate() {
        let color = palette.get_color(dbz);
        if color.a() == 0 {
            continue; // Skip transparent gates
        }

        let inner_km = radial.first_gate_km + (idx as f32) * radial.gate_spacing_km;
        let outer_km = inner_km + radial.gate_spacing_km;

        if outer_km > max_range_km {
            break; // Beyond max range
        }

        // Convert gate corners to lat/lon then to screen coordinates
        let points = gate_to_screen_points(
            projection,
            radar_lat,
            radar_lon,
            inner_km as f64,
            outer_km as f64,
            left_az,
            right_az,
        );

        // Draw the gate as a filled convex polygon
        painter.add(eframe::egui::Shape::convex_polygon(
            points,
            color,
            Stroke::NONE,
        ));
    }
}

/// Converts a gate wedge to screen coordinates.
///
/// A gate is a wedge shape defined by inner/outer range and left/right azimuth.
/// Returns the four corner points in screen space.
fn gate_to_screen_points(
    projection: &MapProjection,
    radar_lat: f64,
    radar_lon: f64,
    inner_km: f64,
    outer_km: f64,
    left_az: f64,
    right_az: f64,
) -> Vec<Pos2> {
    // Convert km to degrees (approximate, 1 degree ≈ 111 km)
    let km_to_deg = 1.0 / 111.0;

    // Calculate the four corners of the gate wedge
    let corners = [
        (inner_km, left_az),  // Inner-left
        (inner_km, right_az), // Inner-right
        (outer_km, right_az), // Outer-right
        (outer_km, left_az),  // Outer-left
    ];

    corners
        .iter()
        .map(|(range_km, az)| {
            let dx = range_km * az.cos() * km_to_deg;
            let dy = range_km * az.sin() * km_to_deg;

            // Account for latitude correction on longitude
            let lat_correction = radar_lat.to_radians().cos();
            let lon = radar_lon + dx / lat_correction;
            let lat = radar_lat + dy;

            projection.geo_to_screen(Coord { x: lon, y: lat })
        })
        .collect()
}

/// Decodes raw NEXRAD data into a renderable sweep structure.
///
/// This handles decompression, decoding, and extraction of reflectivity data.
pub fn decode_sweep_from_data(data: &[u8]) -> Result<DecodedSweep, String> {
    use nexrad::prelude::GateValue;

    // Use the nexrad crate's load function which handles decompression and decoding
    let volume = nexrad::load(data).map_err(|e| format!("Failed to load NEXRAD data: {}", e))?;

    // Get the first (lowest elevation) sweep
    let sweep = volume
        .sweeps()
        .iter()
        .next()
        .ok_or_else(|| "No sweeps in volume".to_string())?;

    // Get elevation angle from first radial (sweep only has elevation_number, not angle)
    let elevation = sweep
        .radials()
        .iter()
        .next()
        .map(|r| r.elevation_angle_degrees())
        .unwrap_or(0.5);

    let mut radials = Vec::new();

    for radial in sweep.radials() {
        let azimuth = radial.azimuth_angle_degrees();
        // Default azimuth spacing (typically 0.5 or 1.0 degrees)
        let azimuth_spacing = 0.5;

        // Try to get reflectivity data
        if let Some(ref_data) = radial.reflectivity() {
            let first_gate_km = ref_data.first_gate_range_km() as f32;
            let gate_spacing_km = ref_data.gate_interval_km() as f32;

            // Convert gate values to dBZ
            let reflectivity: Vec<f32> = ref_data
                .values()
                .iter()
                .map(|val| match val {
                    GateValue::Value(v) => *v as f32,
                    _ => -999.0,
                })
                .collect();

            radials.push(DecodedRadial {
                azimuth,
                azimuth_spacing,
                first_gate_km,
                gate_spacing_km,
                reflectivity,
            });
        }
    }

    if radials.is_empty() {
        return Err("No reflectivity data found".to_string());
    }

    Ok(DecodedSweep { elevation, radials })
}
