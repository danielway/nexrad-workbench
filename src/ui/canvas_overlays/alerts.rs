//! NWS alerts canvas overlay.
//!
//! Draws each alert whose bounding box intersects the visible area as a
//! translucent fill plus an event-type-colored outline. Only runs in 2D flat
//! mode.
//!
//! The outline is drawn **segment by segment** rather than as one stroked path:
//! simplified zone geometry contains near-180° "needle" vertices, and a mitered
//! closed path turns those into long screen-spanning spikes (a miter length
//! grows as `edge / sin(angle/2)`). Independent line segments have no joins, so
//! they cannot spike — the same approach the geo boundary renderer uses.

use crate::alerts::{bbox_intersects, Alert};
use crate::geo::MapProjection;
use eframe::egui::epaint::{Vertex, WHITE_UV};
use eframe::egui::{Color32, Mesh, Painter, Pos2, Shape, Stroke};
use geo_types::Coord;

/// Skip outline segments shorter than this (screen px²): duplicate/degenerate
/// vertices left by simplification, which add nothing and risk artifacts.
const MIN_SEG_SQ: f32 = 0.5;

/// Render alert fills + outlines on top of the radar view.
///
/// `show_warnings` / `show_other` gate the two product classes independently so
/// the layers panel can toggle warnings and watches/advisories separately.
pub(crate) fn render_alerts(
    painter: &Painter,
    projection: &MapProjection,
    alerts: &[Alert],
    show_warnings: bool,
    show_other: bool,
) {
    let bounds = projection.visible_bounds();

    let mut ordered: Vec<&Alert> = alerts
        .iter()
        .filter(|a| {
            if a.is_warning() {
                show_warnings
            } else {
                show_other
            }
        })
        .filter(|a| bbox_intersects(a, bounds))
        .collect();
    // Warnings paint on top of watches; within a class, higher severity wins
    // (false < true, so non-warnings sort first and draw underneath).
    ordered.sort_by_key(|a| (a.is_warning(), a.severity.rank()));

    // Slightly wider black halo so warning strokes stay legible over bright
    // radar fills. Only warnings get it — watches stay subdued.
    let halo = Stroke::new(4.5, Color32::BLACK);

    for alert in ordered {
        let warning = alert.is_warning();
        let (r, g, b) = alert.color();

        // Translucent fill (under the outline): warnings more opaque so the
        // urgent areas read first.
        let fill_alpha = if warning { 55 } else { 25 };
        draw_fill(
            painter,
            projection,
            bounds,
            &alert.fill_triangles,
            Color32::from_rgba_unmultiplied(r, g, b, fill_alpha),
        );

        // Outline, segment by segment (no miter joins → no spikes).
        let (width, alpha) = if warning { (2.5, 220) } else { (1.5, 110) };
        let stroke = Stroke::new(width, Color32::from_rgba_unmultiplied(r, g, b, alpha));
        for polygon in &alert.geometry.polygons {
            for ring in polygon {
                let pts: Vec<Pos2> = ring
                    .iter()
                    .map(|&(lon, lat)| projection.geo_to_screen(Coord { x: lon, y: lat }))
                    .collect();
                let n = pts.len();
                if n < 2 {
                    continue;
                }
                for i in 0..n {
                    let p1 = pts[i];
                    let p2 = pts[(i + 1) % n]; // (i+1)%n closes the ring
                    let (dx, dy) = (p2.x - p1.x, p2.y - p1.y);
                    if dx * dx + dy * dy < MIN_SEG_SQ {
                        continue;
                    }
                    if warning {
                        painter.line_segment([p1, p2], halo);
                    }
                    painter.line_segment([p1, p2], stroke);
                }
            }
        }
    }
}

/// Paint a translucent fill from cached geo-space triangles, skipping triangles
/// entirely outside the visible bounds. egui clips the rest to the canvas.
fn draw_fill(
    painter: &Painter,
    projection: &MapProjection,
    bounds: (f64, f64, f64, f64),
    triangles: &[[(f64, f64); 3]],
    color: Color32,
) {
    if triangles.is_empty() {
        return;
    }
    let (min_lon, min_lat, max_lon, max_lat) = bounds;
    let mut mesh = Mesh::default();
    for tri in triangles {
        let xs = [tri[0].0, tri[1].0, tri[2].0];
        let ys = [tri[0].1, tri[1].1, tri[2].1];
        if xs.iter().all(|&x| x < min_lon)
            || xs.iter().all(|&x| x > max_lon)
            || ys.iter().all(|&y| y < min_lat)
            || ys.iter().all(|&y| y > max_lat)
        {
            continue; // triangle fully off one edge of the view
        }
        let base = mesh.vertices.len() as u32;
        for &(lon, lat) in tri {
            mesh.vertices.push(Vertex {
                pos: projection.geo_to_screen(Coord { x: lon, y: lat }),
                uv: WHITE_UV,
                color,
            });
        }
        mesh.indices.extend_from_slice(&[base, base + 1, base + 2]);
    }
    if !mesh.is_empty() {
        painter.add(Shape::mesh(mesh));
    }
}
