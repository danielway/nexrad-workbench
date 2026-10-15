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

/// A distinguishing swatch for a saved event in the events list, by its ID.
pub(crate) fn event_swatch(id: u64) -> Color32 {
    const PALETTE: &[Color32] = &[
        Color32::from_rgb(255, 200, 80),
        Color32::from_rgb(120, 220, 160),
        Color32::from_rgb(160, 180, 255),
        Color32::from_rgb(255, 150, 150),
        Color32::from_rgb(200, 160, 255),
        Color32::from_rgb(255, 180, 120),
    ];
    PALETTE[(id % PALETTE.len() as u64) as usize]
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

    #[wasm_bindgen_test]
    fn event_swatch_cycles() {
        assert!(event_swatch(0) == event_swatch(6));
        assert!(event_swatch(0) != event_swatch(1));
    }
}
