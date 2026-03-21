use eframe::egui::{self, Color32, Pos2, Rect, Stroke, StrokeKind, Vec2};

pub(crate) fn draw_color_scale(
    ui: &mut egui::Ui,
    rect: &Rect,
    product: &crate::state::RadarProduct,
) {
    use crate::nexrad::color_table::{build_reflectivity_lut, product_value_range};
    use nexrad_render::Product;

    let product_nr = match product {
        crate::state::RadarProduct::Reflectivity => Product::Reflectivity,
        crate::state::RadarProduct::Velocity => Product::Velocity,
        crate::state::RadarProduct::SpectrumWidth => Product::SpectrumWidth,
        crate::state::RadarProduct::DifferentialReflectivity => Product::DifferentialReflectivity,
        crate::state::RadarProduct::CorrelationCoefficient => Product::CorrelationCoefficient,
        crate::state::RadarProduct::DifferentialPhase => Product::DifferentialPhase,
        crate::state::RadarProduct::ClutterFilterPower => Product::ClutterFilterPower,
    };

    let (min_val, max_val) = product_value_range(product_nr);

    // Build the LUT (1024 entries) — for reflectivity uses OKLab, others use crate scale
    let lut = if matches!(product, crate::state::RadarProduct::Reflectivity) {
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

    let bar_width = 16.0f32;
    let margin = 14.0f32;
    let top_margin = 20.0f32;
    let bottom_margin = 20.0f32;
    let bar_height = (rect.height() - top_margin - bottom_margin).clamp(100.0, 320.0);

    let bar_left = rect.right() - margin - bar_width;
    let bar_top = rect.top() + top_margin;

    let painter = ui.painter();
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

    // Outline
    let bar_rect = Rect::from_min_size(
        Pos2::new(bar_left, bar_top),
        Vec2::new(bar_width, bar_height),
    );
    painter.rect_stroke(
        bar_rect,
        0.0,
        Stroke::new(1.0, Color32::from_rgba_unmultiplied(120, 120, 130, 180)),
        StrokeKind::Outside,
    );

    // Tick labels
    let range = max_val - min_val;
    let tick_step = if range > 200.0 {
        60.0
    } else if range > 60.0 {
        10.0
    } else if range > 10.0 {
        5.0
    } else if range > 2.0 {
        1.0
    } else {
        0.2
    };

    let label_x = bar_left - 4.0;
    let mut val = (min_val / tick_step).ceil() * tick_step;
    while val <= max_val {
        let frac = (val - min_val) / range;
        let y = bar_top + bar_height * (1.0 - frac);

        // Tick line
        painter.line_segment(
            [Pos2::new(bar_left - 3.0, y), Pos2::new(bar_left, y)],
            Stroke::new(1.0, Color32::from_rgba_unmultiplied(180, 180, 190, 200)),
        );

        // Label
        let label = if tick_step < 1.0 {
            format!("{:.1}", val)
        } else {
            format!("{:.0}", val)
        };
        painter.text(
            Pos2::new(label_x, y),
            egui::Align2::RIGHT_CENTER,
            label,
            egui::FontId::monospace(10.0),
            Color32::from_rgba_unmultiplied(180, 180, 190, 220),
        );
        val += tick_step;
    }

    // Unit label at top
    painter.text(
        Pos2::new(bar_left + bar_width * 0.5, bar_top - 6.0),
        egui::Align2::CENTER_BOTTOM,
        product.unit(),
        egui::FontId::monospace(10.0),
        Color32::from_rgba_unmultiplied(160, 160, 170, 200),
    );
}
