//! Vertical color scale legend displayed on the right edge of the canvas.
//!
//! Builds a 1024-entry LUT matching the GPU shader's color mapping, then
//! paints it as a gradient bar on a scrim, captioned with the unit and named
//! in full on hover (one-instrument §7). The bar and its ticks set the
//! scrim's width, never the caption. Caption, hover and ticks are the core's
//! [`crate::core::canvas::legend_vm`].

use crate::ui::theme;
use eframe::egui::{self, Color32, Pos2, Rect, Stroke, StrokeKind, Vec2};

/// Trait wrapper for the registry. Z-order 20 sits above the radar
/// image (rendered at 0) and below interactive chrome.
pub(super) struct ColorScaleOverlay;

impl super::Overlay for ColorScaleOverlay {
    fn z_order(&self) -> i32 {
        20
    }

    fn draw(&self, ui: &mut egui::Ui, ctx: &super::OverlayContext) -> Option<egui::Rect> {
        draw_color_scale(ui, &ctx.rect, &ctx.state.viz_state.product);
        None
    }
}

fn draw_color_scale(ui: &mut egui::Ui, rect: &Rect, product: &crate::core::RadarProduct) {
    use crate::nexrad::color_table::{build_reflectivity_lut, product_value_range};
    use nexrad_render::Product;

    let product_nr = match product {
        crate::core::RadarProduct::Reflectivity => Product::Reflectivity,
        crate::core::RadarProduct::Velocity => Product::Velocity,
        crate::core::RadarProduct::SpectrumWidth => Product::SpectrumWidth,
        crate::core::RadarProduct::DifferentialReflectivity => Product::DifferentialReflectivity,
        crate::core::RadarProduct::CorrelationCoefficient => Product::CorrelationCoefficient,
        crate::core::RadarProduct::DifferentialPhase => Product::DifferentialPhase,
        crate::core::RadarProduct::ClutterFilterPower => Product::ClutterFilterPower,
    };

    let (min_val, max_val) = product_value_range(product_nr);

    // Build the LUT (1024 entries) — for reflectivity uses OKLab, others use crate scale
    let lut = if matches!(product, crate::core::RadarProduct::Reflectivity) {
        build_reflectivity_lut(min_val, max_val)
    } else {
        let color_scale = crate::nexrad::color_table::continuous_color_scale(product_nr);
        let lut_size = 1024usize;
        let mut data = Vec::with_capacity(lut_size * 4);
        for i in 0..lut_size {
            let t = i as f32 / (lut_size - 1) as f32;
            let value = min_val + t * (max_val - min_val);
            let color = color_scale.color(value);
            let rgba = color.to_rgba8();
            data.extend_from_slice(&rgba);
        }
        data
    };

    let vm = crate::core::canvas::legend_vm(*product, (min_val, max_val));

    let bar_width = 14.0f32;
    let margin = 12.0f32;
    let pad = 6.0f32;
    let title_h = 16.0f32;
    let tick_w = 30.0f32;
    let bar_height =
        (rect.height() - 2.0 * margin - title_h - 2.0 * pad - 20.0).clamp(100.0, 300.0);

    let painter = ui.painter();
    let title_font = egui::FontId::proportional(theme::CAPTION);
    let title = painter.layout_no_wrap(vm.caption.to_string(), title_font, theme::TEXT_DIM);

    // The scrim: the caption over a bar with its ticks to the left. The bar
    // sets the width; the caption is a unit, short enough to fit over it.
    let width = tick_w + 4.0 + bar_width + pad * 2.0;
    let scrim = Rect::from_min_size(
        Pos2::new(rect.right() - margin - width, rect.top() + margin),
        Vec2::new(width, pad + title_h + bar_height + pad + 4.0),
    );
    painter.rect(
        scrim,
        4.0,
        theme::map::scrim(),
        Stroke::new(1.0_f32, theme::map::scrim_edge()),
        StrokeKind::Inside,
    );
    ui.interact(scrim, ui.id().with("color_scale"), egui::Sense::hover())
        .on_hover_text(&vm.hover);
    painter.galley(
        Pos2::new(scrim.right() - pad - title.size().x, scrim.top() + pad),
        title,
        theme::TEXT_DIM,
    );

    let bar_left = scrim.right() - pad - bar_width;
    let bar_top = scrim.top() + pad + title_h + 4.0;
    let lut_size = 1024usize;

    // Draw the color bar as horizontal slices (bottom = low, top = high)
    let num_slices = bar_height as usize;
    for s in 0..num_slices {
        let frac = s as f32 / (num_slices - 1) as f32;
        let lut_idx = ((1.0 - frac) * (lut_size - 1) as f32) as usize; // flip: top=high
        let lut_idx = lut_idx.min(lut_size - 1);
        let r = lut[lut_idx * 4];
        let g = lut[lut_idx * 4 + 1];
        let b = lut[lut_idx * 4 + 2];
        let a = lut[lut_idx * 4 + 3];

        let y = bar_top + s as f32;
        let slice_rect = Rect::from_min_size(Pos2::new(bar_left, y), Vec2::new(bar_width, 1.5));
        painter.rect_filled(slice_rect, 0.0, Color32::from_rgba_unmultiplied(r, g, b, a));
    }

    let bar_rect = Rect::from_min_size(
        Pos2::new(bar_left, bar_top),
        Vec2::new(bar_width, bar_height),
    );
    painter.rect_stroke(
        bar_rect,
        0.0,
        Stroke::new(1.0_f32, theme::alpha(theme::TEXT_FAINT, 180)),
        StrokeKind::Outside,
    );

    let label_x = bar_left - 4.0;
    for (frac, label) in &vm.ticks {
        let y = bar_top + bar_height * (1.0 - frac);
        painter.line_segment(
            [Pos2::new(bar_left - 3.0, y), Pos2::new(bar_left, y)],
            Stroke::new(1.0_f32, theme::alpha(theme::TEXT_DIM, 200)),
        );
        painter.text(
            Pos2::new(label_x, y),
            egui::Align2::RIGHT_CENTER,
            label,
            egui::FontId::monospace(10.0),
            theme::TEXT_DIM,
        );
    }
}
