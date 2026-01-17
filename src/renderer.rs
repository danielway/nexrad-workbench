//! Placeholder renderer for generating test RGBA images.
//!
//! This module generates simple test patterns to validate the image pipeline.
//! It will be replaced with actual radar rendering logic later.

use eframe::egui::{self, ColorImage, TextureHandle, TextureOptions};

/// Width of the placeholder radar image
const IMAGE_WIDTH: usize = 512;
/// Height of the placeholder radar image
const IMAGE_HEIGHT: usize = 512;

/// Generates a placeholder radar image with a gradient pattern.
///
/// The pattern simulates a radar sweep with concentric rings and
/// a rotating gradient to give a radar-like appearance.
pub fn generate_placeholder_image() -> ColorImage {
    let mut pixels = vec![0u8; IMAGE_WIDTH * IMAGE_HEIGHT * 4];

    let center_x = IMAGE_WIDTH as f32 / 2.0;
    let center_y = IMAGE_HEIGHT as f32 / 2.0;
    let max_radius = center_x.min(center_y);

    for y in 0..IMAGE_HEIGHT {
        for x in 0..IMAGE_WIDTH {
            let dx = x as f32 - center_x;
            let dy = y as f32 - center_y;

            let distance = (dx * dx + dy * dy).sqrt();
            let angle = dy.atan2(dx);

            // Normalize distance to 0-1 range
            let norm_dist = (distance / max_radius).min(1.0);

            // Create radar-like concentric rings with angular variation
            let ring_pattern = ((norm_dist * 20.0).sin() * 0.5 + 0.5) * 255.0;
            let angular_pattern = ((angle * 8.0).sin() * 0.3 + 0.7) * 255.0;

            // Combine patterns with distance falloff
            let intensity = if norm_dist < 0.95 {
                (ring_pattern * angular_pattern / 255.0 * (1.0 - norm_dist * 0.5)) as u8
            } else {
                0
            };

            // Create a color based on intensity (simulating reflectivity palette)
            let (r, g, b) = intensity_to_color(intensity);

            let idx = (y * IMAGE_WIDTH + x) * 4;
            pixels[idx] = r;
            pixels[idx + 1] = g;
            pixels[idx + 2] = b;
            pixels[idx + 3] = 255; // Full alpha
        }
    }

    ColorImage::from_rgba_unmultiplied([IMAGE_WIDTH, IMAGE_HEIGHT], &pixels)
}

/// Converts an intensity value to a radar-like color.
///
/// This simulates a simplified reflectivity color palette.
fn intensity_to_color(intensity: u8) -> (u8, u8, u8) {
    match intensity {
        0..=30 => (0, 0, 0),                                 // No return
        31..=50 => (0, 50 + intensity, 50 + intensity),      // Light cyan
        51..=80 => (0, intensity * 2, 0),                    // Green
        81..=120 => (intensity, intensity, 0),               // Yellow
        121..=170 => (intensity + 50, 50, 0),                // Orange
        171..=210 => (200 + (intensity - 170), 0, 0),        // Red
        211..=255 => (255, 50, 255 - (255 - intensity) * 3), // Magenta/pink
    }
}

/// Creates an egui texture from the placeholder image.
pub fn create_placeholder_texture(ctx: &egui::Context) -> TextureHandle {
    let image = generate_placeholder_image();

    ctx.load_texture("placeholder_radar", image, TextureOptions::LINEAR)
}
