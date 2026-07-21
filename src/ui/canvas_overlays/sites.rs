//! NEXRAD site marker overlay.
//!
//! Renders colored dots and optional labels for all 156+ NEXRAD sites
//! within the visible map bounds. The active site is drawn larger and
//! highlighted; off-screen sites are culled for performance.
//!
//! Works against any [`Projection`] — 2D culls via `visible_bounds`,
//! 3D falls back to projecting every site and skipping the ones the
//! projector reports as not visible.

use crate::data::{get_site, NEXRAD_SITES};
use crate::geo::{text_with_halo, GeoPass, Projection};
use crate::state::GeoLayerVisibility;
use eframe::egui::{self, Painter, Stroke, Vec2};

use super::super::colors::sites as site_colors;

pub(crate) fn render_nexrad_sites(
    painter: &Painter,
    projection: &dyn Projection,
    current_site_id: &str,
    visibility: &GeoLayerVisibility,
    pass: GeoPass,
) {
    let current_site_id_upper = current_site_id.to_uppercase();
    let bounds = projection.visible_bounds();

    if visibility.nexrad_sites {
        for site in NEXRAD_SITES.iter() {
            if site.id == current_site_id_upper {
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

            let Some(screen_pos) = projection.geo_to_screen(site.lat, site.lon) else {
                continue;
            };

            match pass {
                GeoPass::Lines => {
                    painter.circle_filled(screen_pos, 4.0, site_colors::OTHER);
                    painter.circle_stroke(
                        screen_pos,
                        4.0,
                        Stroke::new(1.0_f32, site_colors::OTHER_STROKE),
                    );
                }
                GeoPass::Labels => {
                    if visibility.labels {
                        text_with_halo(
                            painter,
                            screen_pos + Vec2::new(6.0, -2.0),
                            egui::Align2::LEFT_CENTER,
                            site.id,
                            egui::FontId::proportional(10.0),
                            site_colors::LABEL,
                        );
                    }
                }
            }
        }
    }

    if let Some(site) = get_site(&current_site_id_upper) {
        let Some(screen_pos) = projection.geo_to_screen(site.lat, site.lon) else {
            return;
        };

        match pass {
            GeoPass::Lines => {
                painter.circle_filled(screen_pos, 6.0, site_colors::CURRENT);
                painter.circle_stroke(
                    screen_pos,
                    6.0,
                    Stroke::new(1.5_f32, site_colors::CURRENT_STROKE),
                );
            }
            GeoPass::Labels => {
                text_with_halo(
                    painter,
                    screen_pos + Vec2::new(8.0, -2.0),
                    egui::Align2::LEFT_CENTER,
                    site.id,
                    egui::FontId::proportional(11.0),
                    site_colors::CURRENT_LABEL,
                );
            }
        }
    }
}
