//! NEXRAD site marker overlay.
//!
//! Renders colored dots and optional labels for all 156+ NEXRAD sites
//! within the visible map bounds. The sites on the map are drawn larger and
//! highlighted, each with its id label; off-screen sites are culled for performance. Which labels
//! print is the core's ([`crate::core::map_chrome::select_site_labels`]):
//! a label gives way to a nearby city label or site label and never prints
//! under the map's chrome; the dot always stays.
//!
//! Works against any [`Projection`] — 2D culls via `visible_bounds`,
//! 3D falls back to projecting every site and skipping the ones the
//! projector reports as not visible.

use crate::core::map_chrome::{select_site_labels, SiteLabelCandidate};
use crate::data::{get_site, NEXRAD_SITES};
use crate::geo::GeoLayerVisibility;
use crate::geo::{paint_galley_with_halo, GeoLabelClass, GeoPass, Projection, ScreenBounds};
use eframe::egui::{self, Painter, Pos2, Stroke, Vec2};

use crate::ui::theme;

/// What the site labels keep clear of this frame (Labels pass only).
pub(crate) struct SiteLabelRoom<'a> {
    /// The map labels painted this frame.
    pub map_labels: &'a [(GeoLabelClass, ScreenBounds)],
    /// The chrome's rectangles.
    pub keep_out: &'a [ScreenBounds],
    /// The canvas.
    pub viewport: ScreenBounds,
}

/// A site on screen: its id, dot position, and whether it is on the map.
struct OnScreen {
    id: &'static str,
    pos: Pos2,
    current: bool,
}

fn sites_on_screen(
    projection: &dyn Projection,
    current_upper: &[String],
    visibility: &GeoLayerVisibility,
) -> Vec<OnScreen> {
    let bounds = projection.visible_bounds();
    let mut out = Vec::new();
    if visibility.nexrad_sites {
        for site in NEXRAD_SITES.iter() {
            if current_upper.iter().any(|c| c == site.id) {
                continue;
            }

            // 2D cull: skip sites outside the bbox + padding. 3D has no
            // bbox; project everything and let the visibility check fall
            // through.
            if let Some((min_lon, min_lat, max_lon, max_lat)) = bounds {
                let padding = 2.0;
                if site.lat < min_lat - padding
                    || site.lat > max_lat + padding
                    || site.lon < min_lon - padding
                    || site.lon > max_lon + padding
                {
                    continue;
                }
            }

            if let Some(pos) = projection.geo_to_screen(site.lat, site.lon) {
                out.push(OnScreen {
                    id: site.id,
                    pos,
                    current: false,
                });
            }
        }
    }
    for site in current_upper.iter().filter_map(|id| get_site(id)) {
        if let Some(pos) = projection.geo_to_screen(site.lat, site.lon) {
            out.push(OnScreen {
                id: site.id,
                pos,
                current: true,
            });
        }
    }
    out
}

pub(crate) fn render_nexrad_sites(
    painter: &Painter,
    projection: &dyn Projection,
    current_site_ids: &[&str],
    visibility: &GeoLayerVisibility,
    pass: GeoPass,
    room: Option<&SiteLabelRoom>,
) {
    let current_upper: Vec<String> = current_site_ids.iter().map(|s| s.to_uppercase()).collect();
    let sites = sites_on_screen(projection, &current_upper, visibility);

    match pass {
        GeoPass::Lines => {
            for s in &sites {
                if s.current {
                    painter.circle_filled(s.pos, 6.0, theme::map::SITE_CURRENT);
                    painter.circle_stroke(
                        s.pos,
                        6.0,
                        Stroke::new(1.5_f32, theme::map::SITE_CURRENT_STROKE),
                    );
                } else {
                    painter.circle_filled(s.pos, 4.0, theme::map::SITE);
                    painter.circle_stroke(
                        s.pos,
                        4.0,
                        Stroke::new(1.0_f32, theme::map::SITE_STROKE),
                    );
                }
            }
        }
        GeoPass::Labels => {
            // Other sites' labels follow the labels toggle; the id of each
            // site on the map always prints.
            let labels: Vec<_> = sites
                .iter()
                .filter(|s| s.current || visibility.labels)
                .map(|s| {
                    let (offset, size, color) = if s.current {
                        (8.0, 11.0, theme::map::SITE_CURRENT)
                    } else {
                        (6.0, 10.0, theme::map::LABEL)
                    };
                    let font = egui::FontId::proportional(size);
                    let galley = painter.layout_no_wrap(s.id.to_string(), font, color);
                    let at = s.pos + Vec2::new(offset, -2.0 - galley.size().y / 2.0);
                    (s, at, galley, color)
                })
                .collect();
            let candidates: Vec<SiteLabelCandidate> = labels
                .iter()
                .map(|(s, at, galley, _)| SiteLabelCandidate {
                    bounds: ScreenBounds {
                        min_x: at.x,
                        min_y: at.y,
                        max_x: at.x + galley.size().x,
                        max_y: at.y + galley.size().y,
                    },
                    current: s.current,
                })
                .collect();
            let shown = match room {
                Some(room) => {
                    select_site_labels(&candidates, room.map_labels, room.keep_out, room.viewport)
                }
                None => vec![true; candidates.len()],
            };
            for ((_, at, galley, color), show) in labels.into_iter().zip(shown) {
                if show {
                    paint_galley_with_halo(painter, at, &galley, color, egui::Color32::BLACK);
                }
            }
        }
    }
}
