//! Pure geometry for the timeline's scan blocks.
//!
//! The Macro tier used to draw every scan as a fixed 2px tick anchored at its
//! start, discarding the end entirely. That made three separate things wrong at
//! once (all reported in the UX audit): a scan's on-screen extent said nothing
//! about its duration, a downloaded volume's intermediary elevations had
//! nowhere to appear, and the drawn indicator disagreed with the geometry the
//! range-selection logic uses.
//!
//! Drawing each scan across its real time span fixes all three — but it can
//! only be done safely with a plausibility cap on the block's right edge. See
//! [`block_extent`].

/// How much longer than its planned duration a volume may plausibly run before
/// we stop believing the archive's boundary.
///
/// Real volumes overrun modestly (dead time, AVSET early termination, mode
/// changes); 1.5x absorbs that while still catching an outage.
const OUTAGE_SLACK: f64 = 1.5;

/// Minimum painted width (px) for a scan block.
///
/// Equal to the Macro tier's old uniform tick width, so a block too short to
/// resolve degrades into precisely what was drawn before — no regression at the
/// dense end. Deliberately tiny: a floor makes the *visual* wider than the
/// truth, which is the opposite of the error the audit asked to fix, so the
/// overstatement is kept below pointer-targeting resolution rather than being
/// tuned for looks.
pub(crate) const MIN_BLOCK_W: f32 = 2.0;

/// A scan's time extent, split into what we actually hold and how far the
/// volume is believed to run.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct BlockExtent {
    /// Volume start (Unix seconds) — the block's left edge.
    pub start: f64,
    /// End of the data we actually have: the last downloaded sweep.
    pub data_end: f64,
    /// End of the volume as a whole, capped for plausibility.
    pub expected_end: f64,
}

impl BlockExtent {
    /// Whether part of the volume's expected span is not covered by data we
    /// hold — the solid/hollow split.
    pub(crate) fn is_partial(&self) -> bool {
        self.expected_end > self.data_end
    }
}

/// Resolve a scan's drawable extent, capping an implausible right edge.
///
/// **The cap is the load-bearing part.** `ArchiveListing::scan_boundaries`
/// derives every `ScanBoundary.end` as the *next file's timestamp*,
/// unconditionally — so the scan immediately before a six-hour radar outage
/// carries a six-hour boundary. A 2px tick hid that; a proportional block would
/// paint it as six solid hours of claimed coverage, which is strictly worse
/// than the tick it replaces. Capping at `plausible = vcp_estimate * slack`
/// turns outages back into what they are: empty track between blocks.
///
/// `vcp_estimated_secs` is the volume's planned duration
/// (`ExtractedVcp::estimated_volume_duration`); `fallback_secs` covers scans
/// with no VCP pattern. The cap never truncates real data — `end_time` (the
/// last downloaded sweep) is always honored, so a genuinely long volume keeps
/// its full extent.
pub(crate) fn block_extent(
    start: f64,
    end_time: f64,
    vcp_estimated_secs: Option<f64>,
    clamped_end: f64,
    fallback_secs: f64,
) -> BlockExtent {
    let planned = vcp_estimated_secs
        .filter(|d| d.is_finite() && *d > 0.0)
        .unwrap_or(fallback_secs)
        .max(1.0);
    let plausible = start + planned * OUTAGE_SLACK;

    // Observed data always wins over a forecast, so a volume that genuinely ran
    // long is never clipped.
    let data_end = end_time.max(start);
    let expected_end = clamped_end.min(plausible).max(data_end);

    BlockExtent {
        start,
        data_end,
        expected_end,
    }
}

/// Screen geometry for one scan block, in the strip's coordinate space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct BlockLayout {
    /// Left edge (screen x).
    pub x0: f32,
    /// Right edge (screen x), after the legibility floor.
    pub x1: f32,
    /// Where the solid (downloaded) portion ends; the remainder is hollow.
    pub data_x1: f32,
}

impl BlockLayout {
    /// Painted width in pixels.
    pub(crate) fn width(&self) -> f32 {
        self.x1 - self.x0
    }
}

/// Map a [`BlockExtent`] to screen x, culling blocks entirely off-track.
///
/// `ts_to_x` is the strip's shared coordinate mapping, passed in so this stays
/// free of any renderer types. The returned `x0`/`x1` are the *unclamped* edges
/// (beyond a [`MIN_BLOCK_W`] floor) precisely so they equal the geometry the
/// range-selection logic tests against — that agreement is the point, not a
/// side effect.
pub(crate) fn layout_block(
    extent: &BlockExtent,
    ts_to_x: impl Fn(f64) -> f32,
    track_left: f32,
    track_right: f32,
) -> Option<BlockLayout> {
    let x0 = ts_to_x(extent.start);
    let raw_x1 = ts_to_x(extent.expected_end);
    let x1 = raw_x1.max(x0 + MIN_BLOCK_W);

    if x1 < track_left || x0 > track_right {
        return None;
    }

    let data_x1 = ts_to_x(extent.data_end).clamp(x0, x1);
    Some(BlockLayout { x0, x1, data_x1 })
}

/// Gap (px) to leave between adjacent scan blocks.
///
/// Archive scan boundaries are contiguous — each block ends exactly where the
/// next begins — so without a gap a run of volumes paints as one solid bar and
/// the scan cadence, which is the whole reason to draw per-scan blocks,
/// disappears. Collapses to zero once blocks are too narrow to spare it, where
/// the density read matters more than the separation.
pub(crate) fn cell_gap_px(block_px: f32) -> f32 {
    if block_px >= 6.0 {
        1.0
    } else {
        0.0
    }
}

/// Whether a block is wide enough to draw per-sweep boundary hairlines.
///
/// Answers the audit's "all the intermediary elevations vanish": once a block
/// can spare a couple of pixels per sweep, the volume's internal structure is
/// drawn inside it. Below that the lines would be mush and the solid fill
/// carries the meaning instead.
pub(crate) fn should_draw_sweep_texture(block_px: f32, sweep_count: usize) -> bool {
    if sweep_count < 2 {
        return false;
    }
    block_px >= 6.0 && block_px / sweep_count as f32 >= 2.5
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    const START: f64 = 1_000_000.0;
    const VCP: Option<f64> = Some(300.0);
    const FALLBACK: f64 = 300.0;

    // ---- block_extent: the outage cap --------------------------------------

    #[wasm_bindgen_test]
    fn caps_an_outage_inflated_boundary() {
        // THE regression test. ScanBoundary.end is the next file's timestamp,
        // unconditionally — so the scan before a 6-hour outage claims 6 hours.
        // Proportional blocks must not paint that as coverage.
        let six_hours_later = START + 6.0 * 3600.0;
        let e = block_extent(START, START + 280.0, VCP, six_hours_later, FALLBACK);
        assert!(approx(e.expected_end, START + 300.0 * OUTAGE_SLACK));
        assert!(e.expected_end < six_hours_later);
    }

    #[wasm_bindgen_test]
    fn a_normal_boundary_is_left_alone() {
        // A next-volume start one cadence later is entirely plausible and must
        // survive untouched — the cap is for outages, not for trimming.
        let next = START + 300.0;
        let e = block_extent(START, START + 295.0, VCP, next, FALLBACK);
        assert!(approx(e.expected_end, next));
    }

    #[wasm_bindgen_test]
    fn never_truncates_real_data() {
        // A volume that genuinely ran past the cap (AVSET off, slow mode) keeps
        // its full extent: observed data outranks a forecast.
        let long_end = START + 900.0;
        let e = block_extent(START, long_end, VCP, long_end, FALLBACK);
        assert!(approx(e.expected_end, long_end));
        assert!(approx(e.data_end, long_end));
    }

    #[wasm_bindgen_test]
    fn falls_back_when_no_vcp_pattern_is_known() {
        let e = block_extent(START, START + 100.0, None, START + 99_999.0, FALLBACK);
        assert!(approx(e.expected_end, START + FALLBACK * OUTAGE_SLACK));
    }

    #[wasm_bindgen_test]
    fn a_degenerate_vcp_estimate_falls_back_too() {
        for bad in [Some(0.0), Some(-5.0), Some(f64::NAN)] {
            let e = block_extent(START, START + 100.0, bad, START + 99_999.0, FALLBACK);
            assert!(approx(e.expected_end, START + FALLBACK * OUTAGE_SLACK));
        }
    }

    #[wasm_bindgen_test]
    fn partial_downloads_split_solid_from_hollow() {
        // Audit item: "the volume scan time range only represents the
        // downloaded data, not the full volume's time." Both are now carried.
        let e = block_extent(START, START + 120.0, VCP, START + 300.0, FALLBACK);
        assert!(e.is_partial());
        assert!(approx(e.data_end, START + 120.0));
        assert!(approx(e.expected_end, START + 300.0));
    }

    #[wasm_bindgen_test]
    fn a_fully_covered_volume_is_not_partial() {
        let e = block_extent(START, START + 300.0, VCP, START + 300.0, FALLBACK);
        assert!(!e.is_partial());
    }

    #[wasm_bindgen_test]
    fn data_end_never_precedes_the_start() {
        // Defensive: a malformed scan with end < start must not invert.
        let e = block_extent(START, START - 500.0, VCP, START + 300.0, FALLBACK);
        assert!(e.data_end >= e.start);
        assert!(e.expected_end >= e.data_end);
    }

    // ---- layout_block ------------------------------------------------------

    /// 1 px per second, track origin at x=0.
    fn unit_ts_to_x(ts: f64) -> f32 {
        (ts - START) as f32
    }

    #[wasm_bindgen_test]
    fn layout_matches_the_selection_geometry_exactly() {
        // The executable contract for the audit's "frame indicators are
        // narrower than the logic that determines whether they are included in
        // the range selection": the painted edges ARE ts_to_x of the same
        // bounds `intersecting_indices` tests against.
        let e = block_extent(START, START + 280.0, VCP, START + 300.0, FALLBACK);
        let l = layout_block(&e, unit_ts_to_x, -1e6, 1e6).unwrap();
        assert!((l.x0 - unit_ts_to_x(e.start)).abs() < 1e-4);
        assert!((l.x1 - unit_ts_to_x(e.expected_end)).abs() < 1e-4);
    }

    #[wasm_bindgen_test]
    fn the_solid_portion_stops_at_the_data_end() {
        let e = block_extent(START, START + 120.0, VCP, START + 300.0, FALLBACK);
        let l = layout_block(&e, unit_ts_to_x, -1e6, 1e6).unwrap();
        assert!((l.data_x1 - 120.0).abs() < 1e-4);
        assert!(l.data_x1 < l.x1);
    }

    #[wasm_bindgen_test]
    fn a_sub_pixel_block_floors_to_the_old_tick_width() {
        // Zero regression at the dense end: what used to be a 2px tick is still
        // 2px, so no scan silently vanishes.
        let e = BlockExtent {
            start: START,
            data_end: START,
            expected_end: START + 0.1,
        };
        let l = layout_block(&e, unit_ts_to_x, -1e6, 1e6).unwrap();
        assert!((l.width() - MIN_BLOCK_W).abs() < 1e-4);
    }

    #[wasm_bindgen_test]
    fn the_floor_never_overstates_by_more_than_two_pixels() {
        // A floor makes the visual WIDER than the truth — the opposite of the
        // error being fixed — so the overstatement must stay bounded.
        for secs in [0.0_f64, 0.5, 1.0, 1.9, 5.0] {
            let e = BlockExtent {
                start: START,
                data_end: START,
                expected_end: START + secs,
            };
            let l = layout_block(&e, unit_ts_to_x, -1e6, 1e6).unwrap();
            let truth = secs as f32;
            assert!(l.width() - truth <= MIN_BLOCK_W + 1e-4);
            assert!(l.width() >= truth - 1e-4);
        }
    }

    #[wasm_bindgen_test]
    fn data_x1_is_clamped_inside_the_block() {
        // A data end beyond the capped expected end (possible after the cap
        // trims an outage boundary) must not paint past the block.
        let e = BlockExtent {
            start: START,
            data_end: START + 9999.0,
            expected_end: START + 10.0,
        };
        let l = layout_block(&e, unit_ts_to_x, -1e6, 1e6).unwrap();
        assert!(l.data_x1 <= l.x1);
        assert!(l.data_x1 >= l.x0);
    }

    #[wasm_bindgen_test]
    fn offscreen_blocks_are_culled() {
        let e = block_extent(START, START + 300.0, VCP, START + 300.0, FALLBACK);
        // Track entirely to the right of the block.
        assert!(layout_block(&e, unit_ts_to_x, 5000.0, 6000.0).is_none());
        // Track entirely to the left.
        assert!(layout_block(&e, unit_ts_to_x, -6000.0, -5000.0).is_none());
        // Overlapping — kept.
        assert!(layout_block(&e, unit_ts_to_x, 100.0, 200.0).is_some());
    }

    // ---- cell_gap_px / should_draw_sweep_texture ---------------------------

    #[wasm_bindgen_test]
    fn the_gap_collapses_on_narrow_blocks() {
        // Boundaries are contiguous, so without a gap a run of volumes reads as
        // one solid bar and the cadence is lost — but a gap wider than the
        // block itself would erase the block.
        assert!(cell_gap_px(20.0) > 0.0);
        assert!(cell_gap_px(6.0) > 0.0);
        assert!(cell_gap_px(5.9) == 0.0);
        assert!(cell_gap_px(2.0) == 0.0);
    }

    #[wasm_bindgen_test]
    fn sweep_texture_needs_room_per_sweep() {
        // 14 sweeps need >= 35px; below that the hairlines would be mush.
        assert!(!should_draw_sweep_texture(30.0, 14));
        assert!(should_draw_sweep_texture(40.0, 14));
        // Wide block, few sweeps — comfortably drawn.
        assert!(should_draw_sweep_texture(40.0, 5));
    }

    #[wasm_bindgen_test]
    fn sweep_texture_is_skipped_when_there_is_no_structure_to_show() {
        // Zero or one sweep has no internal boundaries worth drawing.
        assert!(!should_draw_sweep_texture(500.0, 0));
        assert!(!should_draw_sweep_texture(500.0, 1));
    }

    #[wasm_bindgen_test]
    fn sweep_texture_needs_a_minimum_block_width_regardless_of_count() {
        // Two sweeps in a 5px block passes the per-sweep test (2.5px each) but
        // the block is still too small to read as structure.
        assert!(!should_draw_sweep_texture(5.0, 2));
        assert!(should_draw_sweep_texture(6.0, 2));
    }
}
