//! Ranges on touch (phone timeline §4): a range is never drawn on the phone,
//! because a horizontal drag on the strip is a scrub. It is made from the
//! chip's sheet or from the long-press sheet, and the row shows it as one
//! amber bracket ([`crate::core::phone_row::Bracket`]); its ends are not
//! draggable.
//!
//! - **The long-press sheet** ([`press_sheet_vm`], [`row_intent`]): a
//!   long-press on the strip opens a small sheet about the volume under the
//!   finger, headed by the pressed time, tilt and product, with three rows:
//!   Loop from here (a range pinned to now, starting at that volume), Loop
//!   to here (the end fixed at that volume, for a loop pinned to now or a
//!   parked start) and Inspect this scan. A row that makes no sense is
//!   disabled with its reason. Both loops emit the selection intent every
//!   surface uses ([`Intent::SelectRange`]), so the one "ends at now" rule
//!   still pins an end within a frame of now and a new range starts paused.
//!
//! All pure; `ui::mobile::press_sheet` draws what these return.

use crate::core::phone::{sheet_rect, Insets, SheetRect};
use crate::core::transport::ends_at_now;
use crate::core::{ElevationSelection, Intent, PlaybackState, RadarProduct};

// ───────────────────────────────────────────────────────────────────────────
// The selection, as these decisions read it
// ───────────────────────────────────────────────────────────────────────────

/// The selection and the clock, as the sheet reads them.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct RangeFacts {
    /// The range's bounds, when one is selected.
    pub range: Option<(f64, f64)>,
    /// The range is pinned to now: its end rides the now line.
    pub pinned: bool,
    /// A start parked with no range yet (the desktop's I key), waiting for
    /// its end.
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

    /// The start Loop to here pairs its end with: a loop pinned to now's
    /// start, or a parked start. A fixed range already has its end.
    fn anchor_for_end(&self) -> Option<f64> {
        match self.range {
            Some((s, _)) if self.pinned => Some(s),
            Some(_) => None,
            None => self.parked,
        }
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
    /// The row's volume under the finger (start, end), if any: the block
    /// the sheet's loops start or end at.
    pub volume: Option<(f64, f64)>,
}

impl PressSheet {
    /// Where a loop from here starts: the volume's start, else the time.
    fn start_here(&self) -> f64 {
        self.volume.map_or(self.ts, |(s, _)| s)
    }

    /// Where a loop to here ends: the volume's end, else the time.
    fn end_here(&self) -> f64 {
        self.volume.map_or(self.ts, |(_, e)| e)
    }
}

/// One row of the sheet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PressRow {
    /// Loop from this volume to now, pinned to now.
    LoopFromHere,
    /// End the loop at this volume, making a fixed range.
    LoopToHere,
    /// Open the scan inspector for the scan under the finger.
    Inspect,
}

impl PressRow {
    /// The rows, top to bottom.
    pub(crate) const ALL: [PressRow; 3] = [
        PressRow::LoopFromHere,
        PressRow::LoopToHere,
        PressRow::Inspect,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            PressRow::LoopFromHere => "Loop from here",
            PressRow::LoopToHere => "Loop to here",
            PressRow::Inspect => "Inspect this scan",
        }
    }

    /// The quiet word at an enabled row's right.
    pub(crate) fn sub(self) -> Option<&'static str> {
        match self {
            PressRow::LoopFromHere => Some("to now"),
            _ => None,
        }
    }

    /// The row's glyph tone: the loops are amber, Inspect plain.
    pub(crate) fn tone(self) -> crate::core::phone_dock::ChipTone {
        use crate::core::phone_dock::ChipTone;
        match self {
            PressRow::LoopFromHere => ChipTone::Loop,
            PressRow::LoopToHere | PressRow::Inspect => ChipTone::Point,
        }
    }
}

/// Why Inspect is off: nothing is under the finger.
pub(crate) const NO_SCAN_HERE: &str = "no scan here";
/// Why Loop from here is off: this volume is now.
pub(crate) const ALREADY_NOW: &str = "this is now";
/// Why Loop to here is off: no loop pinned to now and no parked start.
pub(crate) const START_FIRST: &str = "start a loop first";
/// Why Loop to here is off: the loop starts after this volume.
pub(crate) const BEFORE_RANGE_START: &str = "before the loop's start";
/// Why a loop is off: it would span more than a selection may.
pub(crate) const TOO_LONG: &str = "longer than 7 days";

/// The intent a row emits, or why it is disabled. Loop from here pins a
/// range from the pressed volume's start to now; Loop to here keeps the
/// loop's start (or a parked one) and ends at the pressed volume's end, a
/// fixed range unless that end is within a frame of now ([`ends_at_now`]).
pub(crate) fn row_intent(
    row: PressRow,
    press: &PressSheet,
    f: &RangeFacts,
) -> Result<Intent, &'static str> {
    match row {
        PressRow::Inspect => press
            .scan_start
            .map(Intent::InspectScan)
            .ok_or(NO_SCAN_HERE),
        PressRow::LoopFromHere => {
            let start = press.start_here().min(f.now);
            if start >= f.now - 1.0 {
                return Err(ALREADY_NOW);
            }
            if !crate::core::selection_span_allowed(start, f.now) {
                return Err(TOO_LONG);
            }
            Ok(Intent::SelectRange {
                start,
                end: f.now,
                to_live: true,
            })
        }
        PressRow::LoopToHere => {
            let start = f.anchor_for_end().ok_or(START_FIRST)?;
            let end = press.end_here().min(f.now);
            if end <= start + 1.0 {
                return Err(BEFORE_RANGE_START);
            }
            if !crate::core::selection_span_allowed(start, end) {
                return Err(TOO_LONG);
            }
            let pins = ends_at_now(end, f.now, f.frame_secs);
            Ok(Intent::SelectRange {
                start,
                end: if pins { f.now } else { end },
                to_live: pins,
            })
        }
    }
}

/// One row as the sheet draws it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PressRowVm {
    pub row: PressRow,
    pub label: &'static str,
    /// The quiet word at the row's right: its reason when disabled.
    pub sub: Option<&'static str>,
    pub tone: crate::core::phone_dock::ChipTone,
    /// Whether the row can be tapped.
    pub enabled: bool,
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
            .map(|&row| {
                let why = row_intent(row, press, f).err();
                PressRowVm {
                    row,
                    label: row.label(),
                    sub: why.or(row.sub()),
                    tone: row.tone(),
                    enabled: why.is_none(),
                }
            })
            .collect(),
    }
}

/// A phone action sheet's portrait height for `rows` rows under its header.
pub(crate) fn action_sheet_h(rows: usize) -> f32 {
    SHEET_HEAD_H + SHEET_ROW_H * rows as f32
}

/// An action sheet row's height: a comfortable thumb target.
pub(crate) const SHEET_ROW_H: f32 = 50.0;
/// The header, the frame's margins and the bottom gap.
const SHEET_HEAD_H: f32 = 76.0;

/// Where an action sheet of `rows` rows sits: the phone sheet's place, in
/// portrait only as tall as its rows.
pub(crate) fn action_sheet_rect(
    width: f32,
    height: f32,
    landscape: bool,
    insets: Insets,
    rows: usize,
) -> SheetRect {
    let r = sheet_rect(width, height, landscape, insets);
    if landscape {
        return r;
    }
    let h = action_sheet_h(rows).min(r.h);
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

    /// A press 100 s into a 7-minute volume.
    fn press(volume_start: f64) -> PressSheet {
        PressSheet {
            ts: volume_start + 100.0,
            scan_start: Some(volume_start),
            volume: Some((volume_start, volume_start + 420.0)),
        }
    }

    fn row(vm: &PressSheetVm, row: PressRow) -> &PressRowVm {
        vm.rows.iter().find(|r| r.row == row).unwrap()
    }

    // ── the sheet ─────────────────────────────────────────────────────

    #[wasm_bindgen_test]
    fn the_sheet_has_three_rows_about_the_volume() {
        let vm = press_sheet_vm(&press(NOW - 1_800.0), &facts(None, false), String::new());
        let rows: Vec<_> = vm.rows.iter().map(|r| r.label).collect();
        assert_eq!(
            rows,
            ["Loop from here", "Loop to here", "Inspect this scan"]
        );
        let from = row(&vm, PressRow::LoopFromHere);
        assert_eq!((from.sub, from.enabled), (Some("to now"), true));
        assert_eq!(from.tone, crate::core::phone_dock::ChipTone::Loop);
        // No loop to end: Loop to here says so.
        let to = row(&vm, PressRow::LoopToHere);
        assert_eq!((to.sub, to.enabled), (Some(START_FIRST), false));
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
            scan_start: None,
            ..press(NOW - 600.0)
        };
        let vm = press_sheet_vm(&p, &facts(None, false), String::new());
        assert!(!row(&vm, PressRow::Inspect).enabled);
        assert_eq!(
            row_intent(PressRow::Inspect, &press(NOW - 600.0), &facts(None, false)),
            Ok(Intent::InspectScan(NOW - 600.0))
        );
    }

    #[wasm_bindgen_test]
    fn loop_from_here_pins_a_range_from_the_volumes_start_to_now() {
        // Whatever is selected, it starts at the block, not the finger.
        for f in [
            facts(None, false),
            facts(Some((NOW - 3_000.0, NOW - 2_000.0)), false),
            facts(Some((NOW - 600.0, NOW - 20.0)), true),
        ] {
            assert_eq!(
                row_intent(PressRow::LoopFromHere, &press(NOW - 1_800.0), &f),
                Ok(Intent::SelectRange {
                    start: NOW - 1_800.0,
                    end: NOW,
                    to_live: true
                })
            );
        }
        // With no volume known, it starts at the time pressed.
        let p = PressSheet {
            volume: None,
            ..press(NOW - 1_800.0)
        };
        assert_eq!(
            row_intent(PressRow::LoopFromHere, &p, &facts(None, false)),
            Ok(Intent::SelectRange {
                start: NOW - 1_700.0,
                end: NOW,
                to_live: true
            })
        );
    }

    #[wasm_bindgen_test]
    fn loop_from_here_is_off_on_a_volume_starting_now_or_too_far_back() {
        let f = facts(None, false);
        let p = PressSheet {
            ts: NOW,
            scan_start: None,
            volume: Some((NOW + 10.0, NOW + 430.0)),
        };
        assert_eq!(row_intent(PressRow::LoopFromHere, &p, &f), Err(ALREADY_NOW));
        let far = press(NOW - 9.0 * 86_400.0);
        assert_eq!(row_intent(PressRow::LoopFromHere, &far, &f), Err(TOO_LONG));
    }

    #[wasm_bindgen_test]
    fn loop_to_here_ends_a_pinned_loop_at_the_volumes_end() {
        let pinned = facts(Some((NOW - 3_000.0, NOW - 20.0)), true);
        assert_eq!(
            row_intent(PressRow::LoopToHere, &press(NOW - 1_800.0), &pinned),
            Ok(Intent::SelectRange {
                start: NOW - 3_000.0,
                end: NOW - 1_380.0,
                to_live: false
            })
        );
        // Before the loop's start: nothing to end.
        assert_eq!(
            row_intent(PressRow::LoopToHere, &press(NOW - 4_000.0), &pinned),
            Err(BEFORE_RANGE_START)
        );
        // On the live volume the end is now, and the loop stays pinned.
        assert_eq!(
            row_intent(PressRow::LoopToHere, &press(NOW - 100.0), &pinned),
            Ok(Intent::SelectRange {
                start: NOW - 3_000.0,
                end: NOW,
                to_live: true
            })
        );
    }

    #[wasm_bindgen_test]
    fn loop_to_here_pairs_with_a_parked_start_but_not_a_fixed_range() {
        let parked = RangeFacts {
            parked: Some(NOW - 3_000.0),
            ..facts(None, false)
        };
        assert_eq!(
            row_intent(PressRow::LoopToHere, &press(NOW - 1_800.0), &parked),
            Ok(Intent::SelectRange {
                start: NOW - 3_000.0,
                end: NOW - 1_380.0,
                to_live: false
            })
        );
        let fixed = facts(Some((NOW - 3_000.0, NOW - 2_000.0)), false);
        assert_eq!(
            row_intent(PressRow::LoopToHere, &press(NOW - 1_800.0), &fixed),
            Err(START_FIRST)
        );
    }

    #[wasm_bindgen_test]
    fn loop_to_here_refuses_a_loop_longer_than_a_selection_may() {
        let f = facts(Some((NOW - 9.0 * 86_400.0, NOW - 20.0)), true);
        assert_eq!(
            row_intent(PressRow::LoopToHere, &press(NOW - 1_800.0), &f),
            Err(TOO_LONG)
        );
    }

    #[wasm_bindgen_test]
    fn an_action_sheet_sits_at_the_bottom_as_tall_as_its_rows() {
        let r = action_sheet_rect(390.0, 844.0, false, (47.0, 0.0, 34.0, 0.0), 3);
        assert_eq!(r.h, action_sheet_h(3));
        assert!((r.y + r.h - (844.0 - 34.0)).abs() < 0.01);
        assert!(action_sheet_h(5) > action_sheet_h(3));
        let l = action_sheet_rect(844.0, 390.0, true, (0.0, 0.0, 0.0, 0.0), 3);
        assert_eq!(l, sheet_rect(844.0, 390.0, true, (0.0, 0.0, 0.0, 0.0)));
    }

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
