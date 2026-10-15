//! Radar sweep overlay: range rings, radials, the Rays sweep marks and the
//! edge ring (one-instrument §7, "Quiet canvas").
//!
//! Paints [`crate::core::canvas::CanvasOverlayVm`] and nothing else: rings
//! every 50 km to the data's real range (hover a 100 km ring for its range),
//! a faint white start line, a solid white data edge, a dashed red now line
//! where the antenna probably is, and one edge ring at the data's range,
//! split where the new sweep meets the previous one. No text prints on the
//! map; hovering the edge ring reads the edge's time and age.

use crate::core::canvas::{CanvasOverlayVm, RingGeometry};
use crate::geo::MapProjection;
use crate::ui::theme;
use eframe::egui::{self, Painter, Pos2, Rect, Shape, Stroke, StrokeKind, Vec2};
use geo_types::Coord;
use std::f32::consts::PI;

/// Screen geometry of the site: its center and pixels per km.
pub(crate) struct SiteScreen {
    pub center: Pos2,
    pub px_per_km: f32,
}

impl SiteScreen {
    pub(crate) fn new(projection: &MapProjection, lat: f64, lon: f64) -> Self {
        let center = projection.geo_to_screen(Coord { x: lon, y: lat });
        // 100 km east of the site, through the same equirectangular math as
        // the GPU disc and the mosaic cutout.
        let lon_range = crate::core::canvas::cutout_lon_range_deg(lat, 100.0);
        let edge = projection.geo_to_screen(Coord {
            x: lon + lon_range,
            y: lat,
        });
        Self {
            center,
            px_per_km: (edge.x - center.x).abs() / 100.0,
        }
    }

    fn radius(&self, km: f64) -> f32 {
        km as f32 * self.px_per_km
    }

    fn at(&self, radius: f32, az_deg: f32) -> Pos2 {
        let a = (az_deg - 90.0) * PI / 180.0;
        Pos2::new(
            self.center.x + radius * a.cos(),
            self.center.y + radius * a.sin(),
        )
    }
}

/// Everything the painter needs beyond the view-model.
pub(crate) struct SweepPaint<'a> {
    pub site: &'a SiteScreen,
    pub rect: Rect,
    pub hover: Option<Pos2>,
    /// Data-age desaturation is running (for the faded wedge's tooltip).
    pub desaturating: bool,
    pub now_secs: f64,
    pub clock: &'a dyn crate::core::session::Clock,
}

pub(crate) fn render_radar_sweep(painter: &Painter, vm: &CanvasOverlayVm, p: &SweepPaint<'_>) {
    let site = p.site;
    draw_rings(painter, site, &vm.rings);

    let data_r = site.radius(vm.rings.data_km);
    let Some(marks) = vm.sweep.as_ref() else {
        draw_edge_ring_whole(painter, site, data_r);
        draw_hover(painter, vm, p, None);
        return;
    };

    // The edge ring: the new sweep's arc in white, the rest faint.
    draw_edge_ring_split(
        painter,
        site,
        data_r,
        marks.start_az,
        marks.edge_az,
        marks.stale,
    );

    // Start line, faint white; the data edge, solid white (both "what you
    // see"); held positions between sweeps go grey.
    let (start_color, edge_color) = if marks.stale {
        (
            theme::radar::sweep_start_line_stale(),
            theme::radar::sweep_line_stale(),
        )
    } else {
        (theme::radar::sweep_start_line(), theme::radar::SWEEP_LINE)
    };
    painter.line_segment(
        [site.center, site.at(data_r, marks.start_az)],
        Stroke::new(1.5_f32, start_color),
    );

    // Chunk boundaries of the live cut: hairlines.
    for az in &marks.chunk_az {
        painter.line_segment(
            [site.center, site.at(data_r, *az)],
            Stroke::new(1.0_f32, theme::radar::chunk_line()),
        );
    }

    painter.line_segment(
        [site.center, site.at(data_r, marks.edge_az)],
        Stroke::new(2.0_f32, edge_color),
    );

    // The now line: projected, so dashed, and red because red is now.
    if let Some(now_az) = marks.now_az {
        painter.extend(Shape::dashed_line(
            &[site.center, site.at(data_r, now_az)],
            Stroke::new(2.0_f32, theme::NOW),
            7.0,
            5.0,
        ));
    }

    // Row 2's fallback: one label at the data edge, time only, inside the
    // canvas.
    if let Some(t) = vm.edge_label {
        let galley = painter.layout_no_wrap(p.clock.hms(t), theme::readout_font(), theme::SEEN);
        let size = galley.size() + Vec2::new(8.0, 4.0);
        let (x, y) = crate::core::canvas::edge_label_pos(
            (site.center.x, site.center.y),
            data_r,
            marks.edge_az,
            (size.x, size.y),
            (p.rect.min.x, p.rect.min.y, p.rect.max.x, p.rect.max.y),
        );
        let r = Rect::from_min_size(Pos2::new(x, y), size);
        scrim(painter, r);
        painter.galley(r.min + Vec2::new(4.0, 2.0), galley, theme::SEEN);
    }

    draw_hover(painter, vm, p, Some(marks));
}

fn draw_rings(painter: &Painter, site: &SiteScreen, rings: &RingGeometry) {
    for (km, major) in &rings.rings {
        let (width, color) = if *major {
            (1.0_f32, theme::map::ring_major())
        } else {
            (1.0_f32, theme::map::ring())
        };
        painter.circle_stroke(site.center, site.radius(*km), Stroke::new(width, color));
    }

    // Radials every 30°, to the data's range.
    let data_r = site.radius(rings.data_km);
    for i in 0..12 {
        painter.line_segment(
            [site.center, site.at(data_r, i as f32 * 30.0)],
            Stroke::new(0.5_f32, theme::map::radial()),
        );
    }

    painter.circle_filled(site.center, 4.0, theme::map::CENTER_MARKER);
    painter.circle_stroke(
        site.center,
        4.0,
        Stroke::new(1.0_f32, theme::map::CENTER_MARKER_STROKE),
    );
}

const EDGE_RING_WIDTH: f32 = 3.0;

fn draw_edge_ring_whole(painter: &Painter, site: &SiteScreen, radius: f32) {
    painter.circle_stroke(
        site.center,
        radius,
        Stroke::new(1.5_f32, theme::map::ring_major()),
    );
}

fn draw_edge_ring_split(
    painter: &Painter,
    site: &SiteScreen,
    radius: f32,
    start_az: f32,
    edge_az: f32,
    stale: bool,
) {
    let swept = (edge_az - start_az).rem_euclid(360.0);
    let (new_color, old_color) = if stale {
        (theme::radar::edge_ring_old(), theme::radar::edge_ring_old())
    } else {
        (theme::radar::edge_ring_new(), theme::radar::edge_ring_old())
    };
    arc(painter, site, radius, start_az, swept, new_color);
    arc(painter, site, radius, edge_az, 360.0 - swept, old_color);
}

fn arc(
    painter: &Painter,
    site: &SiteScreen,
    radius: f32,
    from_az: f32,
    span_deg: f32,
    color: egui::Color32,
) {
    if span_deg <= 0.0 {
        return;
    }
    let steps = ((span_deg / 2.0).ceil() as usize).max(1);
    let points: Vec<Pos2> = (0..=steps)
        .map(|i| site.at(radius, from_az + span_deg * i as f32 / steps as f32))
        .collect();
    painter.add(Shape::line(points, Stroke::new(EDGE_RING_WIDTH, color)));
}

/// Hover states over the site's circles: a 100 km ring names its range, the
/// edge ring reads the edge's time and age, the faded wedge says what it is.
fn draw_hover(
    painter: &Painter,
    vm: &CanvasOverlayVm,
    p: &SweepPaint<'_>,
    marks: Option<&crate::core::canvas::SweepMarks>,
) {
    let Some(pos) = p.hover else {
        return;
    };
    if !p.rect.contains(pos) {
        return;
    }
    let site = p.site;
    let d = pos - site.center;
    let dist = d.length();
    let text = match crate::core::canvas::hovered_ring_km(&vm.rings, dist, site.px_per_km) {
        Some(km) if (km - vm.rings.data_km).abs() < 1e-6 => match marks.and_then(|m| m.edge_time) {
            Some(t) => Some(crate::core::canvas::edge_hover_text(t, p.now_secs, p.clock)),
            None => Some(crate::core::canvas::ring_label(km)),
        },
        Some(km) => Some(crate::core::canvas::ring_label(km)),
        None => marks
            .and_then(|m| {
                let az = (d.x.atan2(-d.y).to_degrees() + 360.0) % 360.0;
                crate::core::canvas::faded_wedge_hint(
                    p.desaturating && !m.stale,
                    Some((m.edge_az, m.start_az)),
                    az,
                    (dist / site.px_per_km.max(1e-6)) as f64,
                    vm.rings.data_km,
                )
            })
            .map(str::to_string),
    };
    let Some(text) = text else {
        return;
    };
    let galley = painter.layout_no_wrap(text, theme::readout_font(), theme::TEXT);
    let size = galley.size() + Vec2::new(10.0, 6.0);
    let mut min = pos + Vec2::new(12.0, 12.0);
    min.x = min.x.min(p.rect.max.x - size.x);
    min.y = min.y.min(p.rect.max.y - size.y);
    let r = Rect::from_min_size(min, size);
    scrim(painter, r);
    painter.galley(r.min + Vec2::new(5.0, 3.0), galley, theme::TEXT);
}

fn scrim(painter: &Painter, r: Rect) {
    painter.rect(
        r,
        4.0,
        theme::map::scrim(),
        Stroke::new(1.0_f32, theme::map::scrim_edge()),
        StrokeKind::Inside,
    );
}
