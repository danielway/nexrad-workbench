//! Compass rose overlay for the 3D globe view.
//!
//! Draws a small compass in the bottom-left corner that rotates to match
//! the globe camera's heading, so cardinal directions stay accurate as
//! the user orbits.

use crate::geo::GlobeCamera;
use eframe::egui::{self, Color32, Pos2, Rect, Stroke, Vec2};

pub(crate) fn draw_compass(ui: &mut egui::Ui, rect: &Rect, camera: &GlobeCamera) {
    let painter = ui.painter();
    let radius = 28.0f32;
    let margin = 16.0f32;
    let center = Pos2::new(
        rect.left() + margin + radius,
        rect.bottom() - margin - radius,
    );

    // Background circle
    painter.circle_filled(
        center,
        radius + 4.0,
        Color32::from_rgba_unmultiplied(15, 15, 25, 180),
    );
    painter.circle_stroke(
        center,
        radius + 4.0,
        Stroke::new(1.0, Color32::from_rgba_unmultiplied(80, 80, 100, 160)),
    );

    // Compute compass rotation to match the camera's on-screen orientation.
    // In SiteOrbit, orbit_bearing is where the camera IS, not where it looks.
    // The camera looks FROM the bearing TOWARD the site, so the viewing direction
    // is bearing + 180°. We add π to account for this.
    let rotation_rad = match camera.mode {
        crate::geo::camera::CameraMode::SiteOrbit => {
            std::f32::consts::PI - camera.orbit_bearing.to_radians()
        }
        _ => 0.0,
    } - camera.rotation.to_radians();

    // Cardinal directions
    let cardinals = [("N", 0.0), ("E", 90.0), ("S", 180.0), ("W", 270.0)];
    for (label, bearing_deg) in cardinals {
        let angle = (bearing_deg as f32).to_radians() + rotation_rad;
        // angle=0 → up (screen -Y), rotating CW
        let dir = Vec2::new(angle.sin(), -angle.cos());
        let label_pos = center + dir * (radius - 2.0);

        let (color, size) = if label == "N" {
            (Color32::from_rgb(255, 80, 80), 13.0)
        } else {
            (Color32::from_rgba_unmultiplied(180, 180, 200, 200), 11.0)
        };

        painter.text(
            label_pos,
            egui::Align2::CENTER_CENTER,
            label,
            egui::FontId::proportional(size),
            color,
        );
    }

    // Small tick marks for intercardinals
    for i in 0..8 {
        let angle = (i as f32 * 45.0).to_radians() + rotation_rad;
        if i % 2 == 0 {
            continue; // skip cardinals, already labeled
        }
        let dir = Vec2::new(angle.sin(), -angle.cos());
        let inner = center + dir * (radius - 8.0);
        let outer = center + dir * (radius - 2.0);
        painter.line_segment(
            [inner, outer],
            Stroke::new(1.0, Color32::from_rgba_unmultiplied(120, 120, 140, 140)),
        );
    }

    // Center dot
    painter.circle_filled(
        center,
        2.0,
        Color32::from_rgba_unmultiplied(150, 150, 170, 160),
    );
}
