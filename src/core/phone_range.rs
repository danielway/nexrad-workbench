//! Ranges on touch (phone timeline §4): a range is never drawn on the phone,
//! because a horizontal drag on the strip is a scrub. It is made from the
//! chip's presets or from the long-press sheet, and the row shows it as one
//! amber bracket ([`crate::core::phone_row::Bracket`]); its ends are not
//! draggable.
//!
//! - **The long-press sheet** ([`press_sheet_vm`], [`row_intent`]): a
//!   long-press on the strip opens a small sheet headed by the pressed time,
//!   tilt and product, with Inspect this scan, Start range here, End range
//!   here and Save event here. A row that makes no sense is disabled with
//!   its reason. Start and End emit the selection intents every surface
//!   uses ([`Intent::SelectRange`]), so the one "ends at now" rule pins an
//!   end within a frame of now and a new range starts paused; a start with
//!   no range yet is parked as the desktop's I key parks one
//!   ([`Intent::ParkRangeStart`]).
//!
//! All pure; `ui::mobile::press_sheet` draws what these return.

use crate::core::phone::{sheet_rect, Insets, SheetRect};
use crate::core::transport::ends_at_now;
use crate::core::{ElevationSelection, Intent, PlaybackState, RadarProduct};

// ───────────────────────────────────────────────────────────────────────────
// The selection, as these decisions read it
// ───────────────────────────────────────────────────────────────────────────

/// The selection and the clock, as the sheet and the handles read them.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct RangeFacts {
    /// The range's bounds, when one is selected.
    pub range: Option<(f64, f64)>,
    /// The range is pinned to now: its end rides the now line.
    pub pinned: bool,
    /// A start parked with no range yet (the desktop's I key, or Start range
    /// here), waiting for its end.
    pub parked: Option<f64>,
    pub now: f64,
    /// The frame spacing the "ends at now" rule reads.
    pub frame_secs: f64,
}

impl RangeFacts {
    /// Read the selection off the playback state.
    pub(crate) fn of(playback: &PlaybackState, now: f64) -> Self {
        let range = playback.playback_bounds();
        RangeFacts {
            range,
            pinned: playback.is_live_range(),
            parked: playback
                .drag
                .filter(|d| !d.in_progress && range.is_none())
                .map(|d| d.a)
                .filter(|a| a.is_finite()),
            now,
            frame_secs: playback.median_frame_spacing(),
        }
    }

    /// The range as it reads now: a pinned range ends at now, whatever its
    /// committed end (which trails now while the loop plays).
    pub(crate) fn effective(&self) -> Option<(f64, f64)> {
        self.range
            .map(|(s, e)| (s, if self.pinned { self.now.max(s) } else { e }))
    }

    /// The end a new range's start would pair with: the range's start, or a
    /// parked start.
    fn anchor_for_end(&self) -> Option<f64> {
        self.range.map(|(s, _)| s).or(self.parked)
    }
}

// ───────────────────────────────────────────────────────────────────────────
// The long-press sheet
// ───────────────────────────────────────────────────────────────────────────

/// What a long-press on the strip caught, kept while its sheet is open.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PressSheet {
    /// The time under the finger, snapped like a scrub and never past now.
    pub ts: f64,
    /// The scan under the finger, if any.
    pub scan_start: Option<f64>,
}

/// One row of the sheet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PressRow {
    /// Open the scan inspector for the scan under the finger.
    Inspect,
    /// Put the range's start here.
    StartRange,
    /// Put the range's end here.
    EndRange,
    /// Open the event dialog here, or for the range if one exists.
    SaveEvent,
}

impl PressRow {
    /// The rows, top to bottom: Inspect first, as the long-press used to
    /// open it directly.
    pub(crate) const ALL: [PressRow; 4] = [
        PressRow::Inspect,
        PressRow::StartRange,
        PressRow::EndRange,
        PressRow::SaveEvent,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            PressRow::Inspect => "Inspect this scan",
            PressRow::StartRange => "Start range here",
            PressRow::EndRange => "End range here",
            PressRow::SaveEvent => "Save event here",
        }
    }
}

/// Why Inspect is off: nothing is under the finger.
pub(crate) const NO_SCAN_HERE: &str = "No scan here";
/// Why Start is off: the range ends before this time.
pub(crate) const AFTER_RANGE_END: &str = "After the range's end";
/// Why End is off: nothing to end.
pub(crate) const START_FIRST: &str = "Start a range first";
/// Why End is off: the start is after this time.
pub(crate) const BEFORE_RANGE_START: &str = "Before the range's start";
/// Why Start or End is off: the range would span more than a selection may.
pub(crate) const TOO_LONG: &str = "Longer than 7 days";

/// The intent a row emits, or why it is disabled. Start and End move one end
/// of the range and keep the other; End within a frame of now pins the range
/// ([`ends_at_now`]). Start with no range parks a start for End to pair with.
pub(crate) fn row_intent(
    row: PressRow,
    press: &PressSheet,
    f: &RangeFacts,
) -> Result<Intent, &'static str> {
    let ts = press.ts.min(f.now);
    match row {
        PressRow::Inspect => press
            .scan_start
            .map(Intent::InspectScan)
            .ok_or(NO_SCAN_HERE),
        PressRow::StartRange => match f.effective() {
            None => Ok(Intent::ParkRangeStart(ts)),
            Some((_, end)) if ts >= end - 1.0 => Err(AFTER_RANGE_END),
            Some((_, end)) if !crate::core::selection_span_allowed(ts, end) => Err(TOO_LONG),
            Some((_, end)) => Ok(Intent::SelectRange {
                start: ts,
                end,
                to_live: f.pinned,
            }),
        },
        PressRow::EndRange => {
            let start = f.anchor_for_end().ok_or(START_FIRST)?;
            if ts <= start + 1.0 {
                return Err(BEFORE_RANGE_START);
            }
            if !crate::core::selection_span_allowed(start, ts) {
                return Err(TOO_LONG);
            }
            let pins = ends_at_now(ts, f.now, f.frame_secs);
            Ok(Intent::SelectRange {
                start,
                end: if pins { f.now } else { ts },
                to_live: pins,
            })
        }
        PressRow::SaveEvent => Ok(if f.range.is_some() {
            Intent::SaveSelectionAsEvent
        } else {
            Intent::SaveEventAt(ts)
        }),
    }
}

/// One row as the sheet draws it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PressRowVm {
    pub row: PressRow,
    pub label: &'static str,
    /// Why the row is disabled; `None` when it can be tapped.
    pub disabled: Option<&'static str>,
}

/// The sheet: its heading and its rows.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PressSheetVm {
    /// `16:42:17 · 0.5° · reflectivity`.
    pub heading: String,
    pub rows: Vec<PressRowVm>,
}

/// The sheet's heading: the pressed time in the chosen zone
/// (`tz_offset_secs` east of UTC), the tilt on screen and the product.
pub(crate) fn press_heading(
    ts: f64,
    tz_offset_secs: i64,
    tilt: &ElevationSelection,
    product: RadarProduct,
) -> String {
    let secs = (ts.floor() as i64 + tz_offset_secs).rem_euclid(86_400);
    let time = format!(
        "{:02}:{:02}:{:02}",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    );
    let tilt = match tilt {
        ElevationSelection::Fixed { angle, .. } => format!("{angle:.1}\u{00B0}"),
        ElevationSelection::Latest => "latest tilt".to_string(),
    };
    format!(
        "{time} \u{00B7} {tilt} \u{00B7} {}",
        product.label().to_lowercase()
    )
}

/// The sheet for `press` over the selection `f`, under `heading`.
pub(crate) fn press_sheet_vm(press: &PressSheet, f: &RangeFacts, heading: String) -> PressSheetVm {
    PressSheetVm {
        heading,
        rows: PressRow::ALL
            .iter()
            .map(|&row| PressRowVm {
                row,
                label: row.label(),
                disabled: row_intent(row, press, f).err(),
            })
            .collect(),
    }
}

/// The sheet's portrait height: the header and four rows.
pub(crate) const PRESS_SHEET_H: f32 = 268.0;

/// Where the sheet sits: the phone sheet's place, in portrait only as tall
/// as its rows.
pub(crate) fn press_sheet_rect(
    width: f32,
    height: f32,
    landscape: bool,
    insets: Insets,
) -> SheetRect {
    let r = sheet_rect(width, height, landscape, insets);
    if landscape {
        return r;
    }
    let h = PRESS_SHEET_H.min(r.h);
    SheetRect {
        y: r.y + r.h - h,
        h,
        ..r
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    const NOW: f64 = 100_000.0;
    const FRAME: f64 = 60.0;

    fn facts(range: Option<(f64, f64)>, pinned: bool) -> RangeFacts {
        RangeFacts {
            range,
            pinned,
            parked: None,
            now: NOW,
            frame_secs: FRAME,
        }
    }

    fn press(ts: f64) -> PressSheet {
        PressSheet {
            ts,
            scan_start: Some(ts - 30.0),
        }
    }

    fn disabled(vm: &PressSheetVm, row: PressRow) -> Option<&'static str> {
        vm.rows.iter().find(|r| r.row == row).unwrap().disabled
    }

    // ── the sheet ─────────────────────────────────────────────────────

    #[wasm_bindgen_test]
    fn the_sheet_lists_inspect_first_then_start_end_and_save() {
        let vm = press_sheet_vm(&press(NOW - 600.0), &facts(None, false), String::new());
        let rows: Vec<_> = vm.rows.iter().map(|r| r.label).collect();
        assert_eq!(
            rows,
            [
                "Inspect this scan",
                "Start range here",
                "End range here",
                "Save event here"
            ]
        );
    }

    #[wasm_bindgen_test]
    fn the_heading_names_the_time_tilt_and_product() {
        // 16:42:17 UTC.
        let ts = 16.0 * 3600.0 + 42.0 * 60.0 + 17.4;
        let tilt = ElevationSelection::Fixed {
            elevation_number: 1,
            angle: 0.5,
        };
        assert_eq!(
            press_heading(ts, 0, &tilt, RadarProduct::Reflectivity),
            "16:42:17 \u{00B7} 0.5\u{00B0} \u{00B7} reflectivity"
        );
        // Five hours west, the latest tilt, another product.
        assert_eq!(
            press_heading(
                ts,
                -5 * 3600,
                &ElevationSelection::Latest,
                RadarProduct::Velocity
            ),
            "11:42:17 \u{00B7} latest tilt \u{00B7} velocity"
        );
    }

    #[wasm_bindgen_test]
    fn with_no_scan_under_the_finger_inspect_is_disabled() {
        let p = PressSheet {
            ts: NOW - 600.0,
            scan_start: None,
        };
        let vm = press_sheet_vm(&p, &facts(None, false), String::new());
        assert_eq!(disabled(&vm, PressRow::Inspect), Some(NO_SCAN_HERE));
        let p = press(NOW - 600.0);
        assert_eq!(
            row_intent(PressRow::Inspect, &p, &facts(None, false)),
            Ok(Intent::InspectScan(NOW - 630.0))
        );
    }

    #[wasm_bindgen_test]
    fn with_no_range_start_parks_and_end_asks_for_a_start() {
        let f = facts(None, false);
        let p = press(NOW - 600.0);
        assert_eq!(
            row_intent(PressRow::StartRange, &p, &f),
            Ok(Intent::ParkRangeStart(NOW - 600.0))
        );
        assert_eq!(row_intent(PressRow::EndRange, &p, &f), Err(START_FIRST));
        // Save opens the event dialog at the pressed time.
        assert_eq!(
            row_intent(PressRow::SaveEvent, &p, &f),
            Ok(Intent::SaveEventAt(NOW - 600.0))
        );
    }

    #[wasm_bindgen_test]
    fn end_pairs_with_a_parked_start_and_refuses_one_before_it() {
        let f = RangeFacts {
            parked: Some(NOW - 3_000.0),
            ..facts(None, false)
        };
        assert_eq!(
            row_intent(PressRow::EndRange, &press(NOW - 1_200.0), &f),
            Ok(Intent::SelectRange {
                start: NOW - 3_000.0,
                end: NOW - 1_200.0,
                to_live: false
            })
        );
        assert_eq!(
            row_intent(PressRow::EndRange, &press(NOW - 3_600.0), &f),
            Err(BEFORE_RANGE_START)
        );
        // Starting again moves the parked start.
        assert_eq!(
            row_intent(PressRow::StartRange, &press(NOW - 3_600.0), &f),
            Ok(Intent::ParkRangeStart(NOW - 3_600.0))
        );
    }

    #[wasm_bindgen_test]
    fn end_within_a_frame_of_now_pins_the_range() {
        let f = facts(Some((NOW - 3_000.0, NOW - 2_000.0)), false);
        assert_eq!(
            row_intent(PressRow::EndRange, &press(NOW - FRAME / 2.0), &f),
            Ok(Intent::SelectRange {
                start: NOW - 3_000.0,
                end: NOW,
                to_live: true
            })
        );
        // Two frames back is a fixed end.
        assert_eq!(
            row_intent(PressRow::EndRange, &press(NOW - 2.0 * FRAME), &f),
            Ok(Intent::SelectRange {
                start: NOW - 3_000.0,
                end: NOW - 2.0 * FRAME,
                to_live: false
            })
        );
    }

    #[wasm_bindgen_test]
    fn start_moves_a_ranges_start_and_keeps_its_end_or_its_pin() {
        let fixed = facts(Some((NOW - 3_000.0, NOW - 2_000.0)), false);
        assert_eq!(
            row_intent(PressRow::StartRange, &press(NOW - 4_000.0), &fixed),
            Ok(Intent::SelectRange {
                start: NOW - 4_000.0,
                end: NOW - 2_000.0,
                to_live: false
            })
        );
        let vm = press_sheet_vm(&press(NOW - 1_000.0), &fixed, String::new());
        assert_eq!(disabled(&vm, PressRow::StartRange), Some(AFTER_RANGE_END));
        // A pinned range's end is now, and stays pinned.
        let pinned = facts(Some((NOW - 600.0, NOW - 20.0)), true);
        assert_eq!(
            row_intent(PressRow::StartRange, &press(NOW - 1_000.0), &pinned),
            Ok(Intent::SelectRange {
                start: NOW - 1_000.0,
                end: NOW,
                to_live: true
            })
        );
    }

    #[wasm_bindgen_test]
    fn a_range_longer_than_a_selection_may_is_refused_with_its_reason() {
        let f = facts(Some((NOW - 3_000.0, NOW - 2_000.0)), false);
        let far = press(NOW - 9.0 * 86_400.0);
        assert_eq!(row_intent(PressRow::StartRange, &far, &f), Err(TOO_LONG));
        let f = facts(Some((NOW - 9.0 * 86_400.0, NOW - 8.9 * 86_400.0)), false);
        assert_eq!(
            row_intent(PressRow::EndRange, &press(NOW - 600.0), &f),
            Err(TOO_LONG)
        );
    }

    #[wasm_bindgen_test]
    fn save_uses_the_range_when_one_exists() {
        let f = facts(Some((NOW - 3_000.0, NOW - 2_000.0)), false);
        assert_eq!(
            row_intent(PressRow::SaveEvent, &press(NOW - 600.0), &f),
            Ok(Intent::SaveSelectionAsEvent)
        );
        let vm = press_sheet_vm(&press(NOW - 600.0), &f, String::new());
        assert_eq!(disabled(&vm, PressRow::SaveEvent), None);
    }

    #[wasm_bindgen_test]
    fn the_sheet_sits_at_the_bottom_as_tall_as_its_rows() {
        let r = press_sheet_rect(390.0, 844.0, false, (47.0, 0.0, 34.0, 0.0));
        assert_eq!(r.h, PRESS_SHEET_H);
        assert!((r.y + r.h - (844.0 - 34.0)).abs() < 0.01);
        let l = press_sheet_rect(844.0, 390.0, true, (0.0, 0.0, 0.0, 0.0));
        assert_eq!(l, sheet_rect(844.0, 390.0, true, (0.0, 0.0, 0.0, 0.0)));
    }

    // ── handles ───────────────────────────────────────────────────────

    #[wasm_bindgen_test]
    fn facts_read_a_parked_start_only_without_a_range() {
        let mut ps = PlaybackState::default();
        ps.drag = Some(crate::core::TimeSelection {
            a: 500.0,
            b: 500.0,
            in_progress: false,
        });
        let f = RangeFacts::of(&ps, 1_000.0);
        assert_eq!(f.parked, Some(500.0));
        assert_eq!(f.range, None);
        assert!(ps.select_range(100.0, 400.0, false));
        let f = RangeFacts::of(&ps, 1_000.0);
        assert_eq!(f.range, Some((100.0, 400.0)));
        assert_eq!(f.parked, None);
    }
}
