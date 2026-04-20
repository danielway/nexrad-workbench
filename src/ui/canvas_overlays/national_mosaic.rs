//! National radar mosaic overlay rendering.
//!
//! Paints the CONUS composite reflectivity texture as a warped mesh so it
//! remains correctly georegistered under the map's latitude-corrected
//! projection. Drawn before vector layers so borders sit on top.

use crate::geo::MapProjection;
use crate::nexrad::NationalMosaic;
use eframe::egui::{self, Color32, Painter, Shape};
use geo_types::Coord;

/// Zoom level above which the per-site radar dominates the viewport and the
/// mosaic is hidden to save draw time.
const MAX_DISPLAY_ZOOM: f32 = 4.0;

/// Grid resolution used to warp the mosaic rectangle through the projection.
/// A single quad visibly skews under the cos(lat) longitude correction when
/// the view is panned far from center; 10×10 keeps edges accurate.
const WARP_GRID: usize = 10;

pub(crate) fn draw_national_mosaic(
    painter: &Painter,
    projection: &MapProjection,
    mosaic: &NationalMosaic,
    zoom: f32,
) {
    if zoom > MAX_DISPLAY_ZOOM {
        return;
    }

    let texture = match mosaic.texture() {
        Some(t) => t,
        None => return,
    };

    let (min_lon, min_lat, max_lon, max_lat) = mosaic.bounds();
    if !projection.bbox_visible(min_lon, min_lat, max_lon, max_lat) {
        return;
    }

    // Semi-transparent so vector layers above remain legible. Unmultiplied
    // alpha because the PNG is already straight-alpha.
    let tint = Color32::from_rgba_unmultiplied(255, 255, 255, 180);
    let mut mesh = egui::Mesh::with_texture(texture.id());

    let stride = (WARP_GRID + 1) as u32;
    for j in 0..=WARP_GRID {
        for i in 0..=WARP_GRID {
            let fx = i as f64 / WARP_GRID as f64;
            let fy = j as f64 / WARP_GRID as f64;
            let lon = min_lon + (max_lon - min_lon) * fx;
            // Image origin is top-left (max_lat), so v=0 maps to max_lat.
            let lat = max_lat - (max_lat - min_lat) * fy;
            let pos = projection.geo_to_screen(Coord { x: lon, y: lat });
            mesh.vertices.push(egui::epaint::Vertex {
                pos,
                uv: egui::pos2(fx as f32, fy as f32),
                color: tint,
            });
        }
    }

    for j in 0..WARP_GRID {
        for i in 0..WARP_GRID {
            let a = j as u32 * stride + i as u32;
            let b = a + 1;
            let c = a + stride;
            let d = c + 1;
            mesh.indices.extend_from_slice(&[a, b, c, b, d, c]);
        }
    }

    painter.add(Shape::mesh(mesh));
}
