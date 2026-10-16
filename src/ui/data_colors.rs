//! Colors that encode data categories, not chrome.
//!
//! These answer "which kind of thing is this" for map data (mPING report
//! categories, storm-cell intensity) and tell saved events apart in a list.
//! They sit outside [`crate::ui::theme`]'s color grammar on purpose: the
//! grammar governs chrome, and these are legends for data. The radar color
//! tables themselves live in `crate::nexrad::render::color_table`.

use crate::mping::ReportCategory;
use eframe::egui::Color32;

/// Fill color for an mPING report's marker, by category.
pub(crate) fn mping_fill(category: ReportCategory) -> Color32 {
    match category {
        ReportCategory::RainSnow => Color32::from_rgb(120, 200, 255),
        ReportCategory::Hail => Color32::from_rgb(255, 230, 80),
        ReportCategory::WindDamage => Color32::from_rgb(255, 160, 60),
        ReportCategory::Tornado => Color32::from_rgb(255, 80, 80),
        ReportCategory::Flood => Color32::from_rgb(80, 120, 220),
        ReportCategory::ReducedVisibility => Color32::from_rgb(180, 180, 180),
        ReportCategory::Other => Color32::from_rgb(220, 220, 220),
    }
}

/// The national mosaic's palette (NWS base reflectivity, which the MRMS WMS
/// layer is drawn in), for the legend on its stamp: the color of the 5 dBZ
/// band starting at `dbz`.
pub(crate) fn mosaic_reflectivity(dbz: f32) -> Color32 {
    const BANDS: [(f32, Color32); 15] = [
        (5.0, Color32::from_rgb(4, 233, 231)),
        (10.0, Color32::from_rgb(1, 159, 244)),
        (15.0, Color32::from_rgb(3, 0, 244)),
        (20.0, Color32::from_rgb(2, 253, 2)),
        (25.0, Color32::from_rgb(1, 197, 1)),
        (30.0, Color32::from_rgb(0, 142, 0)),
        (35.0, Color32::from_rgb(253, 248, 2)),
        (40.0, Color32::from_rgb(229, 188, 0)),
        (45.0, Color32::from_rgb(253, 149, 0)),
        (50.0, Color32::from_rgb(253, 0, 0)),
        (55.0, Color32::from_rgb(212, 0, 0)),
        (60.0, Color32::from_rgb(188, 0, 0)),
        (65.0, Color32::from_rgb(248, 0, 253)),
        (70.0, Color32::from_rgb(152, 84, 198)),
        (75.0, Color32::from_rgb(253, 253, 253)),
    ];
    BANDS
        .iter()
        .rev()
        .find(|(lo, _)| dbz >= *lo)
        .map_or(BANDS[0].1, |(_, c)| *c)
}

/// Storm-cell outline color by its maximum reflectivity (dBZ).
pub(crate) fn storm_cell(max_dbz: f32) -> Color32 {
    if max_dbz >= 60.0 {
        Color32::from_rgb(255, 50, 50) // Severe
    } else if max_dbz >= 50.0 {
        Color32::from_rgb(255, 150, 50) // Strong
    } else {
        Color32::from_rgb(255, 220, 80) // Moderate
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn mping_categories_are_distinct() {
        use ReportCategory::*;
        let all = [
            RainSnow,
            Hail,
            WindDamage,
            Tornado,
            Flood,
            ReducedVisibility,
            Other,
        ];
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert!(mping_fill(*a) != mping_fill(*b));
            }
        }
    }

    #[wasm_bindgen_test]
    fn storm_cell_thresholds() {
        assert!(storm_cell(65.0) == storm_cell(60.0));
        assert!(storm_cell(59.9) == storm_cell(50.0));
        assert!(storm_cell(49.9) == storm_cell(10.0));
        assert!(storm_cell(60.0) != storm_cell(50.0));
        assert!(storm_cell(50.0) != storm_cell(49.0));
    }
}
