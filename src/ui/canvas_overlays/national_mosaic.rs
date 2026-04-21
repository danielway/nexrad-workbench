//! National radar mosaic overlay rendering.
//!
//! Paints the CONUS composite reflectivity texture as a warped mesh so it
//! remains correctly georegistered under the map's latitude-corrected
//! projection. Drawn before vector layers so borders sit on top.
//!
//! When an active site is showing data, the mosaic punches a circular hole
//! centered on the site's coverage so the two layers don't visually overlap.
//! Cells that cross the circle are tessellated into a dense sub-grid with
//! per-vertex alpha so the boundary follows the circle closely.

use crate::geo::MapProjection;
use crate::nexrad::NationalMosaic;
use eframe::egui::{self, Color32, Painter, Pos2, Shape};
use geo_types::Coord;

/// Zoom level above which the per-site radar dominates the viewport and the
/// mosaic is hidden to save draw time.
const MAX_DISPLAY_ZOOM: f32 = 4.0;

/// Grid resolution used to warp the mosaic rectangle through the projection.
/// A single quad visibly skews under the cos(lat) longitude correction when
/// the view is panned far from center; 10×10 keeps edges accurate.
const WARP_GRID: usize = 10;

/// Sub-grid resolution for cells that straddle the active-site cutout
/// circle. Higher values produce a smoother circle at the cost of extra
/// triangles; 32 keeps the boundary within ~1 base-cell/32 ≈ a few pixels
/// even when the straddling cell is large on screen.
const CUTOUT_SUBDIV: usize = 32;

/// Circular cutout region around the active radar site, in screen pixels.
#[derive(Clone, Copy)]
pub(crate) struct RadarCutout {
    pub center: Pos2,
    pub radius: f32,
}

pub(crate) fn draw_national_mosaic(
    painter: &Painter,
    projection: &MapProjection,
    mosaic: &NationalMosaic,
    zoom: f32,
    cutout: Option<RadarCutout>,
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

    // Precompute the base grid's lon/lat and screen positions so that the
    // per-cell decision path can reuse them and, when subdividing, bilinear-
    // interpolate through the actual projection (not screen-space).
    let stride = WARP_GRID + 1;
    let mut lons = Vec::with_capacity(stride);
    let mut lats = Vec::with_capacity(stride);
    for i in 0..=WARP_GRID {
        lons.push(min_lon + (max_lon - min_lon) * (i as f64 / WARP_GRID as f64));
    }
    for j in 0..=WARP_GRID {
        // Image origin is top-left (max_lat), so v=0 maps to max_lat.
        lats.push(max_lat - (max_lat - min_lat) * (j as f64 / WARP_GRID as f64));
    }
    let mut positions = Vec::with_capacity(stride * stride);
    for &lat in &lats {
        for &lon in &lons {
            positions.push(projection.geo_to_screen(Coord { x: lon, y: lat }));
        }
    }
    let idx = |i: usize, j: usize| j * stride + i;

    for j in 0..WARP_GRID {
        for i in 0..WARP_GRID {
            let p00 = positions[idx(i, j)];
            let p10 = positions[idx(i + 1, j)];
            let p01 = positions[idx(i, j + 1)];
            let p11 = positions[idx(i + 1, j + 1)];

            let fx0 = i as f32 / WARP_GRID as f32;
            let fx1 = (i + 1) as f32 / WARP_GRID as f32;
            let fy0 = j as f32 / WARP_GRID as f32;
            let fy1 = (j + 1) as f32 / WARP_GRID as f32;

            match cutout {
                None => emit_quad(&mut mesh, p00, p10, p01, p11, fx0, fx1, fy0, fy1, tint),
                Some(c) => match classify_cell(p00, p10, p01, p11, c) {
                    CellClass::Inside => continue,
                    CellClass::Outside => {
                        emit_quad(&mut mesh, p00, p10, p01, p11, fx0, fx1, fy0, fy1, tint)
                    }
                    CellClass::Straddle => emit_subdivided(
                        &mut mesh,
                        projection,
                        lons[i],
                        lons[i + 1],
                        lats[j],
                        lats[j + 1],
                        fx0,
                        fx1,
                        fy0,
                        fy1,
                        c,
                        tint,
                    ),
                },
            }
        }
    }

    painter.add(Shape::mesh(mesh));
}

enum CellClass {
    /// Every cell point is inside the cutout circle — skip.
    Inside,
    /// The cell is entirely outside the cutout circle — emit as-is.
    Outside,
    /// The cell crosses (or contains) the circle — subdivide.
    Straddle,
}

/// Classify a cell against the cutout circle. Uses farthest-corner and
/// AABB-closest-point tests, which are exact for convex cells: straddling
/// cells fall into `Straddle`, including the case where the circle lies
/// entirely inside a single cell.
fn classify_cell(p00: Pos2, p10: Pos2, p01: Pos2, p11: Pos2, c: RadarCutout) -> CellClass {
    let d00 = (p00 - c.center).length();
    let d10 = (p10 - c.center).length();
    let d01 = (p01 - c.center).length();
    let d11 = (p11 - c.center).length();

    if d00.max(d10).max(d01).max(d11) <= c.radius {
        return CellClass::Inside;
    }

    let minx = p00.x.min(p10.x).min(p01.x).min(p11.x);
    let maxx = p00.x.max(p10.x).max(p01.x).max(p11.x);
    let miny = p00.y.min(p10.y).min(p01.y).min(p11.y);
    let maxy = p00.y.max(p10.y).max(p01.y).max(p11.y);
    let cx = c.center.x.clamp(minx, maxx);
    let cy = c.center.y.clamp(miny, maxy);
    let closest_dist = ((cx - c.center.x).powi(2) + (cy - c.center.y).powi(2)).sqrt();
    if closest_dist >= c.radius {
        return CellClass::Outside;
    }

    CellClass::Straddle
}

#[allow(clippy::too_many_arguments)]
fn emit_quad(
    mesh: &mut egui::Mesh,
    p00: Pos2,
    p10: Pos2,
    p01: Pos2,
    p11: Pos2,
    fx0: f32,
    fx1: f32,
    fy0: f32,
    fy1: f32,
    tint: Color32,
) {
    let base = mesh.vertices.len() as u32;
    mesh.vertices.push(egui::epaint::Vertex {
        pos: p00,
        uv: egui::pos2(fx0, fy0),
        color: tint,
    });
    mesh.vertices.push(egui::epaint::Vertex {
        pos: p10,
        uv: egui::pos2(fx1, fy0),
        color: tint,
    });
    mesh.vertices.push(egui::epaint::Vertex {
        pos: p01,
        uv: egui::pos2(fx0, fy1),
        color: tint,
    });
    mesh.vertices.push(egui::epaint::Vertex {
        pos: p11,
        uv: egui::pos2(fx1, fy1),
        color: tint,
    });
    mesh.indices
        .extend_from_slice(&[base, base + 1, base + 2, base + 1, base + 3, base + 2]);
}

/// Tessellate a straddling cell into `CUTOUT_SUBDIV × CUTOUT_SUBDIV` sub-
/// quads, each carrying per-vertex alpha set to 0 inside the circle and the
/// full tint alpha outside. Sub-cells whose corners are all inside the
/// circle are dropped entirely to save fill.
#[allow(clippy::too_many_arguments)]
fn emit_subdivided(
    mesh: &mut egui::Mesh,
    projection: &MapProjection,
    lon0: f64,
    lon1: f64,
    lat0: f64,
    lat1: f64,
    fx0: f32,
    fx1: f32,
    fy0: f32,
    fy1: f32,
    cutout: RadarCutout,
    tint: Color32,
) {
    let base = mesh.vertices.len() as u32;
    let sub_stride = (CUTOUT_SUBDIV + 1) as u32;

    for sj in 0..=CUTOUT_SUBDIV {
        for si in 0..=CUTOUT_SUBDIV {
            let tx = si as f64 / CUTOUT_SUBDIV as f64;
            let ty = sj as f64 / CUTOUT_SUBDIV as f64;
            let lon = lon0 + (lon1 - lon0) * tx;
            let lat = lat0 + (lat1 - lat0) * ty;
            let pos = projection.geo_to_screen(Coord { x: lon, y: lat });
            let uv = egui::pos2(fx0 + (fx1 - fx0) * tx as f32, fy0 + (fy1 - fy0) * ty as f32);
            let alpha = if (pos - cutout.center).length() <= cutout.radius {
                0
            } else {
                tint.a()
            };
            let color = Color32::from_rgba_unmultiplied(tint.r(), tint.g(), tint.b(), alpha);
            mesh.vertices.push(egui::epaint::Vertex { pos, uv, color });
        }
    }

    for sj in 0..CUTOUT_SUBDIV {
        for si in 0..CUTOUT_SUBDIV {
            let a = base + sj as u32 * sub_stride + si as u32;
            let b = a + 1;
            let c = a + sub_stride;
            let d = c + 1;
            // If every corner of this sub-quad is inside the hole, drop the
            // triangles. Otherwise emit both; the linear interpolation of
            // alpha between inside (0) and outside (tint.a()) vertices
            // produces a ~1 sub-cell-wide anti-aliased edge along the circle.
            if mesh.vertices[a as usize].color.a() == 0
                && mesh.vertices[b as usize].color.a() == 0
                && mesh.vertices[c as usize].color.a() == 0
                && mesh.vertices[d as usize].color.a() == 0
            {
                continue;
            }
            mesh.indices.extend_from_slice(&[a, b, c, b, d, c]);
        }
    }
}
