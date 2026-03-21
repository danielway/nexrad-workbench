use crate::data::{get_site, NEXRAD_SITES};
use crate::geo::MapProjection;
use crate::state::GeoLayerVisibility;
use eframe::egui::{self, Painter, Stroke, Vec2};
use geo_types::Coord;

use super::super::colors::sites as site_colors;

pub(crate) fn render_nexrad_sites(
    painter: &Painter,
    projection: &MapProjection,
    current_site_id: &str,
    visibility: &GeoLayerVisibility,
) {
    let current_site_id_upper = current_site_id.to_uppercase();
    let (min_lon, min_lat, max_lon, max_lat) = projection.visible_bounds();

    if visibility.nexrad_sites {
        for site in NEXRAD_SITES.iter() {
            if site.id == current_site_id_upper {
                continue;
            }

            let padding = 2.0;
            if site.lat < min_lat - padding
                || site.lat > max_lat + padding
                || site.lon < min_lon - padding
                || site.lon > max_lon + padding
            {
                continue;
            }

            let screen_pos = projection.geo_to_screen(Coord {
                x: site.lon,
                y: site.lat,
            });

            painter.circle_filled(screen_pos, 4.0, site_colors::OTHER);
            painter.circle_stroke(screen_pos, 4.0, Stroke::new(1.0, site_colors::OTHER_STROKE));

            if visibility.labels {
                painter.text(
                    screen_pos + Vec2::new(6.0, -2.0),
                    egui::Align2::LEFT_CENTER,
                    site.id,
                    egui::FontId::proportional(10.0),
                    site_colors::LABEL,
                );
            }
        }
    }

    if let Some(site) = get_site(&current_site_id_upper) {
        let screen_pos = projection.geo_to_screen(Coord {
            x: site.lon,
            y: site.lat,
        });

        painter.circle_filled(screen_pos, 6.0, site_colors::CURRENT);
        painter.circle_stroke(
            screen_pos,
            6.0,
            Stroke::new(1.5, site_colors::CURRENT_STROKE),
        );

        painter.text(
            screen_pos + Vec2::new(8.0, -2.0),
            egui::Align2::LEFT_CENTER,
            site.id,
            egui::FontId::proportional(11.0),
            site_colors::CURRENT_LABEL,
        );
    }
}
