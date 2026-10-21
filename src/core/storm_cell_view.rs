//! What the storm cell overlay shows.
//!
//! Detection (when it runs, which result is current) is
//! [`crate::core::storm_cells`]; this module is the presentation half:
//!
//! - **Which cells draw.** 2D only. In Rays the canvas shows the previous
//!   sweep ahead of the sweep line and the current one behind it, and the
//!   cells belong to the current sweep, so a cell draws only once its whole
//!   footprint is behind the line ([`footprint_behind_sweep_line`]).
//! - **Grammar.** The outline is the footprint: one light stroke over a dark
//!   halo, intensity left out of the color. Groups get a fainter dashed
//!   outline. The peak dBZ sits in a chip at the outline's top-right; chips
//!   are placed strongest first and skipped when they would crowd one
//!   already placed or when the outline is too small on screen to carry one.
//!   Cells from a live sweep that is still arriving are provisional: drawn
//!   muted, with a hollow chip.
//!
//! Cell ids are per detection run, not tracks, so the chip shows no letter;
//! it arrives with tracking.
//!
//! Screen math takes the projection as a closure (`lat, lon -> [x, y]`) so
//! the module stays free of egui; text widths assume the monospace chip
//! font's 0.6 em advance (IBM Plex Mono).

use crate::core::canvas::{geo_to_polar, polar_in_prev_region};
use crate::core::{StormCell, StormCellsResult};
use crate::geo::ViewMode;

// ───────────────────────────────────────────────────────────────────────────
// Geometry: the Rays rule
// ───────────────────────────────────────────────────────────────────────────

/// Whether a footprint lies wholly behind the Rays sweep line, i.e. on
/// pixels of the sweep the cells were detected on. `sweep` is the canvas's
/// `(sweep azimuth, sweep start)`; `None` means the whole current sweep is
/// on screen. Every ring vertex and the centroid must be in the swept arc.
pub(crate) fn footprint_behind_sweep_line(
    ring: &[(f64, f64)],
    centroid: (f64, f64),
    radar: (f64, f64),
    sweep: Option<(f32, f32)>,
) -> bool {
    if sweep.is_none() {
        return true;
    }
    std::iter::once(&centroid)
        .chain(ring.iter())
        .all(|&(lat, lon)| {
            let (az, _) = geo_to_polar(lat, lon, radar.0, radar.1);
            !polar_in_prev_region(az as f32, sweep)
        })
}

// ───────────────────────────────────────────────────────────────────────────
// View-model
// ───────────────────────────────────────────────────────────────────────────

/// Everything the overlay's view-model is built from.
pub(crate) struct StormCellViewInputs<'a> {
    pub result: &'a StormCellsResult,
    /// The Layers checkbox.
    pub visible: bool,
    pub view_mode: ViewMode,
    /// Radar site `(lat, lon)`.
    pub radar: (f64, f64),
    /// The canvas's Rays sweep `(azimuth, start)`, `None` when the whole
    /// current sweep is on screen.
    pub sweep: Option<(f32, f32)>,
    /// The cells come from a live sweep that is still arriving.
    pub provisional: bool,
    /// Phone layout: thicker strokes, larger and sparser chips.
    pub phone: bool,
}

/// Stroke widths and chip type for the layout, in screen px.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct OverlayStyle {
    /// The light footprint stroke.
    pub outline_px: f32,
    /// The dark halo under it.
    pub halo_px: f32,
    /// The dashed group outline.
    pub group_px: f32,
    /// Chip font size (monospace).
    pub chip_font_px: f32,
    /// Minimum gap between two chips.
    pub chip_spacing_px: f32,
}

/// The style for the layout: 1.6 px outlines and 11 px chips on the
/// desktop, 2 px and 12 px with wider chip spacing on the phone.
pub(crate) fn overlay_style(phone: bool) -> OverlayStyle {
    if phone {
        OverlayStyle {
            outline_px: 2.0,
            halo_px: 4.5,
            group_px: 1.2,
            chip_font_px: 12.0,
            chip_spacing_px: 6.0,
        }
    } else {
        OverlayStyle {
            outline_px: 1.6,
            halo_px: 4.0,
            group_px: 1.0,
            chip_font_px: 11.0,
            chip_spacing_px: 2.0,
        }
    }
}

/// An outline whose projected extent is below this draws no chip.
pub(crate) const CHIP_MIN_OUTLINE_PX: f32 = 14.0;
/// Advance of one monospace glyph, in em (IBM Plex Mono: 600/1000).
const MONO_ADVANCE_EM: f32 = 0.6;
/// Horizontal padding inside a chip, each side.
const CHIP_PAD_X: f32 = 4.0;
/// Chip height beyond the font size.
const CHIP_PAD_Y: f32 = 5.0;
/// Gap between the outline's right edge and its chip.
const CHIP_GAP_X: f32 = 3.0;

/// One cell's footprint on screen.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CellOutlineVm {
    pub id: u32,
    /// Closed ring in screen px (first point not repeated).
    pub ring: Vec<[f32; 2]>,
}

/// A peak dBZ chip.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ChipVm {
    pub cell_id: u32,
    /// Top-left corner in screen px.
    pub min: [f32; 2],
    pub size: [f32; 2],
    pub text: String,
    /// Provisional cell: outline only, no fill.
    pub hollow: bool,
}

/// What the overlay draws this frame. Empty when off, in 3D, or with no
/// cells.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StormCellsVm {
    pub style: OverlayStyle,
    /// Group outlines (dashed), closed rings in screen px.
    pub groups: Vec<Vec<[f32; 2]>>,
    pub cells: Vec<CellOutlineVm>,
    pub chips: Vec<ChipVm>,
    /// Cells come from a live sweep still arriving: drawn muted.
    pub provisional: bool,
}

impl StormCellsVm {
    fn empty(phone: bool) -> Self {
        Self {
            style: overlay_style(phone),
            groups: Vec::new(),
            cells: Vec::new(),
            chips: Vec::new(),
            provisional: false,
        }
    }

    /// Nothing to draw.
    pub(crate) fn is_empty(&self) -> bool {
        self.cells.is_empty() && self.groups.is_empty()
    }
}

/// The ids of the cells that draw: on, 2D, and behind the sweep line.
pub(crate) fn shown_cells(i: &StormCellViewInputs<'_>) -> Vec<u32> {
    if !i.visible || i.view_mode != ViewMode::Flat2D {
        return Vec::new();
    }
    i.result
        .cells
        .iter()
        .filter(|c| footprint_behind_sweep_line(&c.outline, (c.lat, c.lon), i.radar, i.sweep))
        .map(|c| c.id)
        .collect()
}

/// Build the overlay's view-model. `project` maps `(lat, lon)` to screen px.
pub(crate) fn build(
    i: &StormCellViewInputs<'_>,
    project: impl Fn(f64, f64) -> [f32; 2],
) -> StormCellsVm {
    let mut vm = StormCellsVm::empty(i.phone);
    let shown = shown_cells(i);
    if shown.is_empty() {
        return vm;
    }
    vm.provisional = i.provisional;
    let style = vm.style;

    let ring_px = |ring: &[(f64, f64)]| -> Vec<[f32; 2]> {
        let open = match ring.split_last() {
            Some((last, rest)) if !rest.is_empty() && Some(last) == rest.first() => rest,
            _ => ring,
        };
        open.iter().map(|&(lat, lon)| project(lat, lon)).collect()
    };

    // Groups that have at least one member on screen, each wholly behind
    // the sweep line like its cells.
    for g in &i.result.groups {
        let member_shown = g.cell_ids.iter().any(|id| shown.contains(id));
        if member_shown
            && g.outline.len() >= 3
            && footprint_behind_sweep_line(&g.outline, (g.lat, g.lon), i.radar, i.sweep)
        {
            vm.groups.push(ring_px(&g.outline));
        }
    }

    // Shown cells are in result order: strongest first.
    let mut placed: Vec<([f32; 2], [f32; 2])> = Vec::new();
    for &id in &shown {
        let Some(cell) = i.result.cells.get(id as usize) else {
            continue;
        };
        let ring = ring_px(&cell.outline);
        if ring.len() < 3 {
            continue;
        }
        if let Some(chip) = place_chip(cell, &ring, &style, &placed) {
            placed.push((chip.min, chip.size));
            vm.chips.push(ChipVm {
                hollow: i.provisional,
                ..chip
            });
        }
        vm.cells.push(CellOutlineVm { id, ring });
    }
    vm
}

/// The chip text for a cell: its peak, whole dBZ.
pub(crate) fn chip_text(cell: &StormCell) -> String {
    format!("{:.0}", cell.peak_dbz)
}

/// Size of a chip carrying `text` at the style's font.
fn chip_size(text: &str, style: &OverlayStyle) -> [f32; 2] {
    let glyphs = text.chars().count() as f32;
    [
        glyphs * MONO_ADVANCE_EM * style.chip_font_px + 2.0 * CHIP_PAD_X,
        style.chip_font_px + CHIP_PAD_Y,
    ]
}

/// Place a cell's chip at its outline's top-right, or `None` when the
/// outline is too small on screen or the chip would crowd one already
/// placed.
fn place_chip(
    cell: &StormCell,
    ring: &[[f32; 2]],
    style: &OverlayStyle,
    placed: &[([f32; 2], [f32; 2])],
) -> Option<ChipVm> {
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for p in ring {
        min_x = min_x.min(p[0]);
        min_y = min_y.min(p[1]);
        max_x = max_x.max(p[0]);
        max_y = max_y.max(p[1]);
    }
    if (max_x - min_x).max(max_y - min_y) < CHIP_MIN_OUTLINE_PX {
        return None;
    }
    let text = chip_text(cell);
    let size = chip_size(&text, style);
    let min = [max_x + CHIP_GAP_X, min_y - size[1] * 0.4];
    let gap = style.chip_spacing_px;
    let crowds = placed.iter().any(|(m, s)| {
        min[0] < m[0] + s[0] + gap
            && m[0] < min[0] + size[0] + gap
            && min[1] < m[1] + s[1] + gap
            && m[1] < min[1] + size[1] + gap
    });
    (!crowds).then_some(ChipVm {
        cell_id: cell.id,
        min,
        size,
        text,
        hollow: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storm_cells::StormGroup;
    use wasm_bindgen_test::wasm_bindgen_test;

    const RADAR: (f64, f64) = (41.73, -93.72);

    /// A square footprint of half-side `h` degrees around `(lat, lon)`.
    fn square(lat: f64, lon: f64, h: f64) -> Vec<(f64, f64)> {
        vec![
            (lat - h, lon - h),
            (lat - h, lon + h),
            (lat + h, lon + h),
            (lat + h, lon - h),
            (lat - h, lon - h),
        ]
    }

    fn cell(id: u32, lat: f64, lon: f64, h: f64, peak: f32) -> StormCell {
        StormCell {
            id,
            group_id: None,
            level_dbz: 35.0,
            peak_dbz: peak,
            mean_dbz: peak - 12.0,
            area_km2: (2.0 * h * 111.0).powi(2) as f32,
            gate_count: 100,
            lat,
            lon,
            bounds: (lat - h, lon - h, lat + h, lon + h),
            bearing_from_radar_deg: 239.4,
            range_from_radar_km: 98.2,
            orientation_deg: 0.0,
            elongation: 1.0,
            outline: square(lat, lon, h),
        }
    }

    /// Two cells NE of the radar, strongest first, in one group.
    fn result() -> StormCellsResult {
        let mut a = cell(0, 42.2, -93.2, 0.1, 62.0);
        let mut b = cell(1, 42.2, -92.6, 0.1, 55.0);
        a.group_id = Some(0);
        b.group_id = Some(0);
        b.level_dbz = 50.0;
        StormCellsResult {
            cells: vec![a, b],
            groups: vec![StormGroup {
                id: 0,
                level_dbz: 35.0,
                peak_dbz: 62.0,
                area_km2: 900.0,
                cell_ids: vec![0, 1],
                lat: 42.2,
                lon: -92.9,
                outline: square(42.2, -92.9, 0.45),
            }],
            ..StormCellsResult::default()
        }
    }

    fn inputs(r: &StormCellsResult) -> StormCellViewInputs<'_> {
        StormCellViewInputs {
            result: r,
            visible: true,
            view_mode: ViewMode::Flat2D,
            radar: RADAR,
            sweep: None,
            provisional: false,
            phone: false,
        }
    }

    /// 100 px per degree, north up, origin at (43, -94).
    fn project(lat: f64, lon: f64) -> [f32; 2] {
        [((lon + 94.0) * 100.0) as f32, ((43.0 - lat) * 100.0) as f32]
    }

    // ── Rays rule ──────────────────────────────────────────────────────────

    #[wasm_bindgen_test]
    fn rays_draws_a_cell_only_behind_the_sweep_line() {
        let r = result();
        let mut i = inputs(&r);
        // Cell 0 spans azimuths of about 29° to 52°, cell 1 about 53° to 68°.
        i.sweep = Some((20.0, 0.0));
        assert!(shown_cells(&i).is_empty(), "line has not reached them");
        i.sweep = Some((60.0, 0.0));
        assert_eq!(shown_cells(&i), vec![0], "only the nearer-north cell");
        i.sweep = Some((90.0, 0.0));
        assert_eq!(shown_cells(&i), vec![0, 1]);
        // The sweep started inside cell 0: part of its footprint is
        // previous-sweep pixels ahead of the line.
        i.sweep = Some((90.0, 50.0));
        assert!(!shown_cells(&i).contains(&0));
        // Wrapping through north: started at 350°, now at 60°.
        i.sweep = Some((60.0, 350.0));
        assert_eq!(shown_cells(&i), vec![0]);
        // No sweep line: everything on screen is the current sweep.
        i.sweep = None;
        assert_eq!(shown_cells(&i), vec![0, 1]);
    }

    #[wasm_bindgen_test]
    fn nothing_draws_off_or_in_3d() {
        let r = result();
        let mut i = inputs(&r);
        i.view_mode = ViewMode::Globe3D;
        assert!(build(&i, project).is_empty());
        i.view_mode = ViewMode::Flat2D;
        i.visible = false;
        assert!(build(&i, project).is_empty());
        assert!(build(&inputs(&StormCellsResult::default()), project).is_empty());
    }

    // ── View-model ─────────────────────────────────────────────────────────

    #[wasm_bindgen_test]
    fn outlines_and_groups_project_to_screen() {
        let r = result();
        let vm = build(&inputs(&r), project);
        assert_eq!(vm.cells.len(), 2);
        assert_eq!(vm.groups.len(), 1);
        // The closing point is dropped: four corners.
        assert_eq!(vm.cells[0].ring.len(), 4);
        assert_eq!(vm.cells[0].ring[0], project(42.1, -93.3));
        assert_eq!(vm.groups[0].len(), 4);
        assert_eq!(vm.style, overlay_style(false));
        assert!(!vm.provisional);
    }

    #[wasm_bindgen_test]
    fn phone_strokes_are_heavier() {
        let r = result();
        let mut i = inputs(&r);
        i.phone = true;
        i.provisional = true;
        let vm = build(&i, project);
        assert_eq!(vm.style, overlay_style(true));
        assert!(vm.style.outline_px > overlay_style(false).outline_px);
        assert_eq!(vm.style.chip_font_px, 12.0);
        assert!(vm.style.chip_spacing_px > overlay_style(false).chip_spacing_px);
        assert!(vm.provisional);
        assert!(
            vm.chips.iter().all(|c| c.hollow),
            "provisional chips are hollow"
        );
    }

    #[wasm_bindgen_test]
    fn chips_sit_at_the_outline_top_right() {
        let r = result();
        let vm = build(&inputs(&r), project);
        assert_eq!(vm.chips.len(), 2);
        let chip = &vm.chips[0];
        assert_eq!((chip.cell_id, chip.text.as_str()), (0, "62"));
        assert!(!chip.hollow);
        // Right of cell 0's east edge (-93.1), straddling its north edge
        // (42.3).
        let ne = project(42.3, -93.1);
        assert!(chip.min[0] > ne[0] && chip.min[0] < ne[0] + 6.0);
        assert!(chip.min[1] < ne[1] && chip.min[1] + chip.size[1] > ne[1]);
        // Two mono glyphs at 11 px plus padding; 16 px tall.
        assert!((chip.size[0] - (2.0 * 0.6 * 11.0 + 8.0)).abs() < 1e-4);
        assert_eq!(chip.size[1], 16.0);
    }

    #[wasm_bindgen_test]
    fn chips_go_strongest_first_and_skip_overlaps() {
        let mut r = result();
        // Move cell 1 next to cell 0 so their chips would collide.
        r.cells[1] = cell(1, 42.2, -93.15, 0.1, 55.0);
        let vm = build(&inputs(&r), project);
        assert_eq!(vm.cells.len(), 2, "both outlines still draw");
        assert_eq!(vm.chips.len(), 1);
        assert_eq!(vm.chips[0].cell_id, 0, "the stronger cell keeps its chip");
    }

    #[wasm_bindgen_test]
    fn small_outlines_carry_no_chip() {
        let r = result();
        // 20 px per degree: cells are 4 px across.
        let vm = build(&inputs(&r), |lat, lon| {
            [((lon + 94.0) * 20.0) as f32, ((43.0 - lat) * 20.0) as f32]
        });
        assert_eq!(vm.cells.len(), 2);
        assert!(vm.chips.is_empty());
    }

    #[wasm_bindgen_test]
    fn group_outline_draws_only_wholly_behind_the_line() {
        let r = result();
        let mut i = inputs(&r);
        // The group's envelope spans past both cells; with the line short
        // of the far cell the group does not draw.
        i.sweep = Some((60.0, 0.0));
        let vm = build(&i, project);
        assert_eq!(vm.cells.len(), 1);
        assert!(vm.groups.is_empty());
    }
}
