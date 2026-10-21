//! The tilt ladder (one-instrument §6, §8 and the answers page's "One
//! ladder, no modes"): one volume drawn by angle on the volume's own clock.
//!
//! Rows are the VCP's elevation angles, under a Latest row that follows the
//! newest cut. Each row is the volume's clock (0:00 to the volume's length),
//! so a bar sits where the radar makes that cut, and the thin connectors from
//! one bar to the next are the antenna's moves between elevations. Pinned to
//! now the ladder draws the volume being collected (received, filling,
//! expected, a red now line and a caret on the antenna's row); at a time it
//! draws the volume under the playhead (on device or in the archive, and the
//! white playhead line). That is the only state it has: the session's.
//!
//! [`tilt_rows`] is the one tilt list. The ladder's rows and the top bar's
//! tilt menu are both built by it, so the two can never list different cuts.
//! [`VolumeLadder::build`] projects one volume into the ladder; the egui
//! painter (`ui::ladder`) only draws it and emits the [`TiltPick`] or fetch
//! a row or bar carries.

use crate::core::projection::{
    scan_to_projection, ScanProjection, SweepProjection, SweepProjectionStatus,
};
use crate::core::session::{Clock, TiltRole};
use crate::core::{ElevationListEntry, ElevationSelection, Intent, RadarProduct};

// ───────────────────────────────────────────────────────────────────────────
// The one tilt list
// ───────────────────────────────────────────────────────────────────────────

/// A tilt chosen from a ladder row or the top bar's tilt menu.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum TiltPick {
    /// Follow the most recently completed sweep, whatever its angle.
    Latest,
    /// One cut of the volume.
    Fixed { elevation_number: u8, angle: f32 },
}

/// One row of the tilt list: Latest, or one elevation angle.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct TiltRow {
    /// "Latest", "0.5°".
    pub label: String,
    /// The angle; `None` for Latest.
    pub angle: Option<f32>,
    /// Quiet words: the waveforms at this angle and any repeats
    /// ("CS · CD · SAILS ×2").
    pub note: Option<String>,
    pub selected: bool,
    /// Whether the chosen product is recorded at this angle. A row is never
    /// disabled; one the product does not reach says so on hover.
    pub reaches_product: bool,
    /// What a click asks for. For an angle, the product picks the cut inside
    /// the row: reflectivity the surveillance cut of a split pair, the other
    /// products the Doppler cut.
    pub pick: TiltPick,
    pub hover: String,
}

/// The cuts that share one elevation angle, in volume order.
struct AngleGroup<'a> {
    angle: f32,
    cuts: Vec<&'a ElevationListEntry>,
}

/// Angles closer than this are one row (the VCP's commanded angles differ by
/// at least 0.3°; the encoder wobbles by hundredths).
const SAME_ANGLE_DEG: f32 = 0.05;

fn angle_groups(entries: &[ElevationListEntry]) -> Vec<AngleGroup<'_>> {
    let mut groups: Vec<AngleGroup<'_>> = Vec::new();
    for e in entries {
        match groups
            .iter_mut()
            .find(|g| (g.angle - e.angle).abs() < SAME_ANGLE_DEG)
        {
            Some(g) => g.cuts.push(e),
            None => groups.push(AngleGroup {
                angle: e.angle,
                cuts: vec![e],
            }),
        }
    }
    groups.sort_by(|a, b| a.angle.total_cmp(&b.angle));
    groups
}

/// "0.5°"
pub(crate) fn angle_label(deg: f32) -> String {
    format!("{deg:.1}\u{00B0}")
}

fn short_waveform(raw: &str) -> &'static str {
    match raw {
        "CS" => "CS",
        "CD" | "CDW" | "CDWO" => "CD",
        "B" | "Batch" => "B",
        "SP" | "SPP" => "SP",
        _ => "",
    }
}

/// Whether `product` is recorded at this cut. Empty `cached_products` means
/// "unknown" (a live VCP before any cut lands), which counts as yes.
fn records(e: &ElevationListEntry, product: RadarProduct) -> bool {
    e.cached_products.is_empty()
        || e.cached_products
            .iter()
            .any(|p| p == product.to_worker_string())
}

/// Whether the cut's waveform is the one for this product: surveillance
/// (CS) for reflectivity, Doppler for everything else.
fn suits(e: &ElevationListEntry, product: RadarProduct) -> bool {
    let surveillance = e.waveform == "CS";
    if product == RadarProduct::Reflectivity {
        surveillance
    } else {
        !surveillance
    }
}

/// The cut a click on this angle picks for `product`: one that records it
/// (known, not merely unknown), then one whose waveform suits it, then not
/// a SAILS/MRLE repeat; ties go to the first in the volume.
fn pick_cut<'a>(cuts: &[&'a ElevationListEntry], product: RadarProduct) -> &'a ElevationListEntry {
    let score = |e: &ElevationListEntry| {
        (
            !e.cached_products.is_empty() && records(e, product),
            suits(e, product),
            !(e.is_sails || e.is_mrle),
        )
    };
    let mut best = cuts[0];
    for &c in &cuts[1..] {
        if score(c) > score(best) {
            best = c;
        }
    }
    best
}

fn group_note(g: &AngleGroup<'_>) -> Option<String> {
    let mut words: Vec<String> = Vec::new();
    for c in &g.cuts {
        let w = short_waveform(&c.waveform);
        if !w.is_empty() && !words.iter().any(|x| x == w) {
            words.push(w.to_string());
        }
    }
    // A repeat is a whole split pair, so count the repeated surveillance
    // cuts when there are any.
    let count = |f: fn(&ElevationListEntry) -> bool| {
        let all: Vec<_> = g.cuts.iter().filter(|c| f(c)).collect();
        let cs = all.iter().filter(|c| c.waveform == "CS").count();
        if cs > 0 {
            cs
        } else {
            all.len()
        }
    };
    let sails = count(|c| c.is_sails);
    let mrle = count(|c| c.is_mrle);
    if sails > 0 {
        words.push(format!("SAILS \u{00D7}{sails}"));
    }
    if mrle > 0 {
        words.push(format!("MRLE \u{00D7}{mrle}"));
    }
    (!words.is_empty()).then(|| words.join(" \u{00B7} "))
}

/// The one tilt list: Latest, then one row per elevation angle of the
/// volume (`entries` in volume order, as the elevation roster builds them).
pub(crate) fn tilt_rows(
    selection: &ElevationSelection,
    entries: &[ElevationListEntry],
    product: RadarProduct,
) -> Vec<TiltRow> {
    let mut rows = vec![TiltRow {
        label: "Latest".to_string(),
        angle: None,
        note: None,
        selected: selection.is_auto(),
        reaches_product: true,
        pick: TiltPick::Latest,
        hover: "Follow the newest cut, whatever its angle".to_string(),
    }];
    let selected_number = selection.elevation_number();
    if entries.is_empty() {
        // Before any volume is known: the default lowest cut.
        rows.push(TiltRow {
            label: angle_label(0.5),
            angle: Some(0.5),
            note: None,
            selected: selected_number == Some(1),
            reaches_product: true,
            pick: TiltPick::Fixed {
                elevation_number: 1,
                angle: 0.5,
            },
            hover: "The lowest cut".to_string(),
        });
        return rows;
    }
    rows.extend(angle_groups(entries).into_iter().map(|g| {
        let cut = pick_cut(&g.cuts, product);
        let reaches = g.cuts.iter().any(|c| records(c, product));
        let selected =
            selected_number.is_some_and(|n| g.cuts.iter().any(|c| c.elevation_number == n));
        let hover = if reaches {
            format!(
                "{} \u{00B7} beam {:.1} km up at 100 km",
                angle_label(g.angle),
                crate::core::volume_plan::beam_height_km(100.0, g.angle)
            )
        } else {
            format!(
                "{} is not recorded at {}",
                product.label(),
                angle_label(g.angle)
            )
        };
        TiltRow {
            label: angle_label(g.angle),
            angle: Some(g.angle),
            note: group_note(&g),
            selected,
            reaches_product: reaches,
            pick: TiltPick::Fixed {
                elevation_number: cut.elevation_number,
                angle: cut.angle,
            },
            hover,
        }
    }));
    rows
}

/// Apply a tilt pick to the selection. Choosing Latest remembers the fixed
/// cut it left so a later fixed pick can restore it.
pub(crate) fn apply_tilt_pick(
    selection: &mut ElevationSelection,
    last_fixed: &mut Option<(u8, f32)>,
    pick: TiltPick,
) {
    match pick {
        TiltPick::Latest => {
            if let ElevationSelection::Fixed {
                elevation_number,
                angle,
            } = selection
            {
                *last_fixed = Some((*elevation_number, *angle));
            }
            *selection = ElevationSelection::Latest;
        }
        TiltPick::Fixed {
            elevation_number,
            angle,
        } => {
            *selection = ElevationSelection::Fixed {
                elevation_number,
                angle,
            };
        }
    }
}

/// After a product change, the cut a fixed angle should now draw: the same
/// row, re-picked for the new product (the surveillance cut of a split pair
/// for reflectivity, the Doppler cut otherwise). `None` keeps the selection.
pub(crate) fn repick_for_product(
    selection: &ElevationSelection,
    entries: &[ElevationListEntry],
    product: RadarProduct,
) -> Option<TiltPick> {
    let n = selection.elevation_number()?;
    let group = angle_groups(entries)
        .into_iter()
        .find(|g| g.cuts.iter().any(|c| c.elevation_number == n))?;
    let cut = pick_cut(&group.cuts, product);
    (cut.elevation_number != n).then_some(TiltPick::Fixed {
        elevation_number: cut.elevation_number,
        angle: cut.angle,
    })
}

/// E (down) and Shift+E (up) through the rows, Latest included, wrapping.
pub(crate) fn step_tilt(rows: &[TiltRow], down: bool) -> Option<TiltPick> {
    if rows.is_empty() {
        return None;
    }
    let i = rows.iter().position(|r| r.selected).unwrap_or(0);
    let n = rows.len();
    let next = if down { (i + 1) % n } else { (i + n - 1) % n };
    Some(rows[next].pick)
}

// ───────────────────────────────────────────────────────────────────────────
// The ladder
// ───────────────────────────────────────────────────────────────────────────

/// How a cut's bar is drawn.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum BarMark {
    /// Live: received (solid slate).
    Received,
    /// Live: being received now (periwinkle, filled to `progress`).
    Filling { progress: f32 },
    /// Live: still to come (dashed grey).
    Expected,
    /// At a time: on this device (solid slate).
    OnDevice,
    /// Listed in the archive, not on this device (hollow; a click fetches).
    InArchive,
}

/// One cut on its row, placed on the volume clock.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct LadderBar {
    pub elevation_number: u8,
    /// Seconds from the volume's start.
    pub start: f64,
    pub end: f64,
    pub mark: BarMark,
    /// The cut on screen: a white outline.
    pub on_screen: bool,
    /// The bounds are projected, not observed (tooltips carry a tilde).
    pub estimated: bool,
    pub hover: String,
    /// A hollow bar's click: fetch this one cut.
    pub fetch: Option<Intent>,
}

#[derive(Clone, PartialEq, Debug)]
pub(crate) struct LadderRow {
    pub tilt: TiltRow,
    /// The quiet hint the Latest row prints where its bars would be.
    pub hint: Option<&'static str>,
    pub bars: Vec<LadderBar>,
}

/// Where an antenna move sits relative to now.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum MoveState {
    /// Already made (or any move at a time).
    Done,
    /// Happening now: the antenna is between these rows.
    Active,
    /// Still to come (drawn faint).
    Ahead,
}

/// The antenna's move from the end of one cut to the start of the next.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct LadderMove {
    pub from_row: usize,
    pub to_row: usize,
    /// Seconds from the volume's start.
    pub start: f64,
    pub end: f64,
    pub state: MoveState,
}

/// The row the antenna is on (live only).
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Caret {
    OnRow(usize),
    /// Moving between two rows.
    Between {
        from: usize,
        to: usize,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum LineKind {
    /// Red: now, while pinned to the volume being collected.
    Now,
    /// White: the playhead, at a time.
    Playhead,
}

#[derive(Clone, PartialEq, Debug)]
pub(crate) struct LadderLine {
    /// Seconds from the volume's start.
    pub at: f64,
    pub kind: LineKind,
    /// "now · 2:15" under the axis.
    pub label: Option<String>,
}

/// The ladder, ready to draw.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct VolumeLadder {
    /// "VCP 212 Precipitation · 23 cuts · ~7:05".
    pub summary: Option<String>,
    /// "on 1.3° · az 148° · next volume in ~4:50", "moving to 1.8° · ~0:02",
    /// "volume of 11:08:00 · 5 of 23 cuts on device".
    pub status: Option<String>,
    /// The volume clock's length in seconds (0 with no volume).
    pub length_secs: f64,
    /// The axis end label: "7:05" (with a tilde while projected).
    pub length_label: String,
    /// `rows[0]` is Latest; the rest are angles, lowest first.
    pub rows: Vec<LadderRow>,
    /// Rows take clicks (false while the volume draws every tilt: no row
    /// is selected and none picks).
    pub picks: bool,
    pub moves: Vec<LadderMove>,
    pub caret: Option<Caret>,
    pub line: Option<LadderLine>,
    /// Why there are no bars, when there are none.
    pub empty: Option<&'static str>,
}

/// Which clock the volume is read on.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum LadderClock {
    /// Pinned to the volume being collected: `now` is the red line.
    Live { now_secs: f64 },
    /// At a time: the playhead is the white line.
    AtTime { playhead_secs: f64 },
}

/// The cut on screen, for the white outline.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct OnScreenCut {
    pub elevation_number: u8,
    pub start_secs: f64,
}

/// Everything [`VolumeLadder::build`] reads: plain data gathered per frame.
pub(crate) struct LadderInputs<'a> {
    /// The volume's cuts in volume order (the elevation roster).
    pub entries: &'a [ElevationListEntry],
    pub selection: &'a ElevationSelection,
    pub product: RadarProduct,
    /// The volume to draw (see [`ladder_volume`]); `None` before any.
    pub volume: Option<&'a ScanProjection>,
    pub clock: LadderClock,
    /// The antenna's estimated azimuth (live only).
    pub antenna_azimuth: Option<f32>,
    pub on_screen: Option<OnScreenCut>,
    /// What the rows do: pick a tilt, or stand down while the volume draws
    /// every tilt (then every cut on the device is on screen).
    pub role: TiltRole,
    /// A download or ingest covers the playhead (the status line says the
    /// rest of a partial volume is on its way).
    pub fetching: bool,
}

/// What a row says on hover while the volume draws every tilt.
pub(crate) const DRAWS_ALL_HOVER: &str =
    "Drawn as part of the volume \u{00B7} volume rendering draws every tilt";

/// What the Latest row prints while the volume draws every tilt.
const DRAWS_ALL_HINT: &str = "volume rendering draws every tilt";

/// "VCP 212 Precipitation": the number with the pattern's name when the VCP
/// table knows it, else the number alone.
pub(crate) fn vcp_label(vcp: u16) -> String {
    match crate::data::vcp::get_vcp_definition(vcp) {
        Some(def) => format!("VCP {vcp} {}", def.name),
        None => format!("VCP {vcp}"),
    }
}

/// "7:05"
pub(crate) fn mmss(secs: f64) -> String {
    let s = secs.max(0.0).round() as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

/// One cut resolved onto the volume clock.
struct PlacedCut {
    elevation_number: u8,
    angle: f32,
    waveform: String,
    row: usize,
    start: f64,
    end: f64,
    estimated: bool,
    status: Option<SweepProjectionStatus>,
    progress: Option<f32>,
}

/// Nominal per-cut durations summing to `length`: the VCP pattern's
/// azimuth-rate weights when its cut count matches, else even.
fn nominal_durations(volume: &ScanProjection, count: usize, length: f64) -> Vec<f64> {
    if let Some(p) = volume.vcp_pattern.as_ref() {
        if p.elevations.len() == count {
            return p.sweep_durations(length);
        }
    }
    if count == 0 {
        Vec::new()
    } else {
        vec![length / count as f64; count]
    }
}

impl VolumeLadder {
    pub(crate) fn build(inputs: LadderInputs<'_>, clock: &dyn Clock) -> Self {
        let mut tilt = tilt_rows(inputs.selection, inputs.entries, inputs.product);
        let draws_all = inputs.role.draws_all();
        if draws_all {
            // No row is the one on screen: every cut is.
            for t in &mut tilt {
                t.selected = false;
                if t.reaches_product {
                    t.hover = DRAWS_ALL_HOVER.to_string();
                }
            }
        }
        let live = matches!(inputs.clock, LadderClock::Live { .. });
        let latest_hint = if draws_all {
            DRAWS_ALL_HINT
        } else if live {
            "follows the newest cut"
        } else {
            "the newest cut at the playhead"
        };
        let mut rows: Vec<LadderRow> = tilt
            .into_iter()
            .enumerate()
            .map(|(i, t)| LadderRow {
                tilt: t,
                hint: (i == 0).then_some(latest_hint),
                bars: Vec::new(),
            })
            .collect();
        let picks = !draws_all;

        let Some(volume) = inputs.volume.filter(|_| !inputs.entries.is_empty()) else {
            return VolumeLadder {
                summary: None,
                status: None,
                length_secs: 0.0,
                length_label: String::new(),
                rows,
                picks,
                moves: Vec::new(),
                caret: None,
                line: None,
                empty: Some(if live {
                    "Waiting for the volume\u{2019}s first cut"
                } else {
                    "No volume at the playhead"
                }),
            };
        };

        let v0 = volume.volume_start;
        let row_of = |angle: f32| {
            rows.iter()
                .position(|r| {
                    r.tilt
                        .angle
                        .is_some_and(|a| (a - angle).abs() < SAME_ANGLE_DEG)
                })
                .unwrap_or(1)
        };

        // Resolve every roster cut to its sweep, when the volume has one.
        let found: Vec<Option<&SweepProjection>> = inputs
            .entries
            .iter()
            .map(|e| {
                volume
                    .sweeps
                    .iter()
                    .find(|s| s.elevation_number == e.elevation_number)
            })
            .collect();
        let complete = found.iter().all(Option::is_some);
        let span = (volume.volume_end - v0).max(0.0);
        let estimated_total = volume
            .vcp_pattern
            .as_ref()
            .and_then(|p| p.estimated_volume_duration());
        let length = if complete {
            span
        } else {
            span.max(estimated_total.unwrap_or(span))
        }
        .max(
            found
                .iter()
                .flatten()
                .map(|s| s.collection_end_secs - v0)
                .fold(0.0, f64::max),
        );
        let nominal = nominal_durations(volume, inputs.entries.len(), length);

        // Missing cuts fill the gap between their known neighbours, keeping
        // their nominal proportions.
        let mut bounds: Vec<(f64, f64)> = vec![(0.0, 0.0); inputs.entries.len()];
        let mut i = 0;
        while i < inputs.entries.len() {
            if let Some(s) = found[i] {
                bounds[i] = (s.collection_start_secs - v0, s.collection_end_secs - v0);
                i += 1;
                continue;
            }
            let run_start = i;
            while i < inputs.entries.len() && found[i].is_none() {
                i += 1;
            }
            let gap_start = if run_start == 0 {
                0.0
            } else {
                bounds[run_start - 1].1
            };
            let gap_end = found
                .get(i)
                .copied()
                .flatten()
                .map_or(length, |s| s.collection_start_secs - v0);
            let want: f64 = nominal[run_start..i].iter().sum();
            let scale = if gap_end > gap_start && want > 0.0 {
                (gap_end - gap_start) / want
            } else {
                1.0
            };
            let mut t = gap_start;
            for k in run_start..i {
                let d = nominal[k] * scale;
                bounds[k] = (t, t + d);
                t += d;
            }
        }

        let mut cuts: Vec<PlacedCut> = inputs
            .entries
            .iter()
            .zip(&found)
            .zip(&bounds)
            .map(|((e, s), &(start, end))| PlacedCut {
                elevation_number: e.elevation_number,
                angle: e.angle,
                waveform: e.waveform.clone(),
                row: row_of(e.angle),
                start,
                end,
                estimated: s.is_none_or(|s| !s.is_observed()),
                status: s.map(|s| s.status),
                progress: s.filter(|s| s.is_in_progress()).map(|s| {
                    if s.chunks_in_sweep > 0 {
                        (s.chunks_received as f32 / s.chunks_in_sweep as f32).clamp(0.0, 1.0)
                    } else {
                        0.0
                    }
                }),
            })
            .collect();
        cuts.sort_by(|a, b| a.start.total_cmp(&b.start));

        let now_rel = match inputs.clock {
            LadderClock::Live { now_secs } => now_secs - v0,
            LadderClock::AtTime { playhead_secs } => playhead_secs - v0,
        };

        // Bars.
        for c in &cuts {
            let mark = match (live, c.status) {
                (true, Some(SweepProjectionStatus::CollectedByUs)) => BarMark::Received,
                (true, Some(SweepProjectionStatus::InProgress)) => {
                    let by_time = if c.end > c.start {
                        ((now_rel - c.start) / (c.end - c.start)).clamp(0.0, 1.0) as f32
                    } else {
                        0.0
                    };
                    BarMark::Filling {
                        progress: c.progress.unwrap_or(0.0).max(by_time),
                    }
                }
                (true, Some(SweepProjectionStatus::AvailableNotCollected)) => BarMark::InArchive,
                (true, _) => BarMark::Expected,
                (false, Some(SweepProjectionStatus::CollectedByUs)) => BarMark::OnDevice,
                (false, _) => BarMark::InArchive,
            };
            let tilde = if c.estimated { "~" } else { "" };
            let mut hover = format!(
                "{}{} \u{00B7} {tilde}{}\u{2013}{tilde}{} \u{00B7} beam {:.1} km up at 100 km",
                angle_label(c.angle),
                match short_waveform(&c.waveform) {
                    "" => String::new(),
                    w => format!(" {w}"),
                },
                clock.hms(v0 + c.start),
                clock.hms(v0 + c.end),
                crate::core::volume_plan::beam_height_km(100.0, c.angle),
            );
            match mark {
                BarMark::InArchive => hover.push_str(" \u{00B7} in the archive, click to fetch"),
                BarMark::Expected => hover.push_str(" \u{00B7} still to come"),
                BarMark::Filling { .. } => hover.push_str(" \u{00B7} receiving"),
                _ => {}
            }
            // One cut is on screen, or every cut on the device is.
            let on_screen = if draws_all {
                c.status == Some(SweepProjectionStatus::CollectedByUs)
            } else {
                inputs.on_screen.is_some_and(|o| {
                    o.elevation_number == c.elevation_number
                        && o.start_secs >= v0 - 1.0
                        && o.start_secs <= v0 + length + 1.0
                })
            };
            rows[c.row].bars.push(LadderBar {
                elevation_number: c.elevation_number,
                start: c.start,
                end: c.end,
                mark,
                on_screen,
                estimated: c.estimated,
                hover,
                fetch: (mark == BarMark::InArchive).then(|| Intent::FetchScan {
                    scan_start: v0.round() as i64,
                    elevation_filter: Some(c.elevation_number),
                }),
            });
        }

        // Moves between consecutive cuts.
        let moves: Vec<LadderMove> = cuts
            .windows(2)
            .map(|w| {
                let (start, end) = (w[0].end, w[1].start.max(w[0].end));
                let state = if !live || now_rel >= end {
                    MoveState::Done
                } else if now_rel >= start {
                    MoveState::Active
                } else {
                    MoveState::Ahead
                };
                LadderMove {
                    from_row: w[0].row,
                    to_row: w[1].row,
                    start,
                    end,
                    state,
                }
            })
            .collect();

        // The caret and the status line.
        let filling = live
            .then(|| {
                cuts.iter()
                    .find(|c| c.status == Some(SweepProjectionStatus::InProgress))
            })
            .flatten();
        let active_move = live
            .then(|| {
                moves
                    .iter()
                    .zip(cuts.iter().skip(1))
                    .find(|(m, _)| m.state == MoveState::Active)
            })
            .flatten();
        let caret = match (filling, active_move) {
            (Some(c), _) => Some(Caret::OnRow(c.row)),
            (None, Some((m, _))) => Some(Caret::Between {
                from: m.from_row,
                to: m.to_row,
            }),
            _ => None,
        };
        let next_volume = format!("next volume in ~{}", mmss(length - now_rel));
        let status = if live {
            Some(match (filling, active_move) {
                (Some(c), _) => {
                    let mut s = format!("on {}", angle_label(c.angle));
                    if let Some(az) = inputs.antenna_azimuth {
                        s.push_str(&format!(" \u{00B7} az {:.0}\u{00B0}", az));
                    }
                    format!("{s} \u{00B7} {next_volume}")
                }
                (None, Some((m, to))) => format!(
                    "moving to {} \u{00B7} ~{}",
                    angle_label(to.angle),
                    mmss(m.end - now_rel)
                ),
                (None, None) => next_volume,
            })
        } else {
            let on_device = cuts
                .iter()
                .filter(|c| c.status == Some(SweepProjectionStatus::CollectedByUs))
                .count();
            let mut s = format!(
                "volume of {} \u{00B7} {on_device} of {} cuts on device",
                clock.hms(v0),
                cuts.len()
            );
            if draws_all && on_device < cuts.len() && inputs.fetching {
                s.push_str(" \u{00B7} fetching the rest");
            }
            Some(s)
        };

        let projected = live || cuts.iter().any(|c| c.estimated);
        let length_label = format!("{}{}", if projected { "~" } else { "" }, mmss(length));
        let summary = Some(if volume.vcp_number > 0 {
            format!(
                "{} \u{00B7} {} cuts \u{00B7} {length_label}",
                vcp_label(volume.vcp_number),
                cuts.len()
            )
        } else {
            format!("{} cuts \u{00B7} {length_label}", cuts.len())
        });

        let line = if live {
            Some(LadderLine {
                at: now_rel.clamp(0.0, length),
                kind: LineKind::Now,
                label: Some(format!("now \u{00B7} {}", mmss(now_rel.clamp(0.0, length)))),
            })
        } else {
            (0.0..=length).contains(&now_rel).then_some(LadderLine {
                at: now_rel,
                kind: LineKind::Playhead,
                label: None,
            })
        };

        VolumeLadder {
            summary,
            status,
            length_secs: length,
            length_label,
            rows,
            picks,
            moves,
            caret,
            line,
            empty: None,
        }
    }
}

/// The volume the ladder draws and the clock it is read on. Pinned to now
/// with a volume being collected, that volume on the live clock; otherwise
/// the volume under the playhead (a settled scan, else the live volume when
/// the playhead sits inside it) on the playhead's clock.
pub(crate) fn ladder_volume(
    view: &crate::core::TimelineView<'_>,
    pinned: bool,
    now_secs: f64,
    playhead_secs: f64,
) -> (Option<ScanProjection>, LadderClock) {
    if pinned {
        if let Some(v) = view.live_volume() {
            return (Some(v.clone()), LadderClock::Live { now_secs });
        }
    }
    let at = LadderClock::AtTime { playhead_secs };
    if let Some(scan) = view.settled_scan_at(playhead_secs) {
        return (Some(scan_to_projection(scan)), at);
    }
    let live = view
        .live_volume()
        .filter(|v| playhead_secs >= v.volume_start && playhead_secs <= v.volume_end)
        .cloned();
    (live, at)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::projection::{ProjectionScanRole, SweepTimingProvenance};
    use wasm_bindgen_test::wasm_bindgen_test;

    struct Utc;
    impl Clock for Utc {
        fn hms(&self, ts: f64) -> String {
            let s = ts.rem_euclid(86_400.0) as u64;
            format!("{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
        }
        fn hm(&self, ts: f64) -> String {
            self.hms(ts)[..5].to_string()
        }
        fn day(&self, _ts: f64) -> (i32, u32, u32) {
            (2026, 10, 15)
        }
    }

    fn entry(n: u8, angle: f32, wf: &str, products: &[&str]) -> ElevationListEntry {
        ElevationListEntry {
            elevation_number: n,
            angle,
            waveform: wf.to_string(),
            is_sails: false,
            is_mrle: false,
            cached_products: products.iter().map(|p| p.to_string()).collect(),
        }
    }

    /// A small split-cut volume: 0.5° CS/CD, 1.3° CS/CD, 2.4° B.
    fn roster() -> Vec<ElevationListEntry> {
        vec![
            entry(1, 0.5, "CS", &[]),
            entry(2, 0.5, "CDW", &[]),
            entry(3, 1.3, "CS", &[]),
            entry(4, 1.3, "CDW", &[]),
            entry(5, 2.4, "B", &[]),
        ]
    }

    const V0: f64 = 40_080.0; // 11:08:00 UTC

    fn sweep(
        n: u8,
        angle: f32,
        start: f64,
        end: f64,
        status: SweepProjectionStatus,
    ) -> SweepProjection {
        SweepProjection {
            elevation_number: n,
            elevation_angle: angle,
            scan_role: ProjectionScanRole::CurrentInProgress,
            status,
            timing: if status == SweepProjectionStatus::CollectedByUs {
                SweepTimingProvenance::Observed
            } else {
                SweepTimingProvenance::Estimated
            },
            collection_start_secs: V0 + start,
            collection_end_secs: V0 + end,
            chunks_in_sweep: 6,
            chunks_received: if status == SweepProjectionStatus::InProgress {
                3
            } else {
                0
            },
            radials_received: 0,
            azimuth_rate_dps: 0.0,
            chunks: Vec::new(),
        }
    }

    fn volume(sweeps: Vec<SweepProjection>, end: f64) -> ScanProjection {
        ScanProjection {
            vcp_number: 212,
            vcp_pattern: None,
            roster: Default::default(),
            in_progress_elevation: None,
            in_progress_radials: None,
            volume_start: V0,
            volume_end: V0 + end,
            sweeps,
            extrapolation: None,
            next_scan_ghost: None,
        }
    }

    use SweepProjectionStatus::{CollectedByUs as Got, FutureExpected as Ahead, InProgress as Now};

    /// The live volume 2:15 in: 0.5° both cuts and 1.3° CS received, 1.3° CD
    /// filling, 2.4° to come. Cuts are 40 s with 2 s moves.
    fn live_volume() -> ScanProjection {
        volume(
            vec![
                sweep(1, 0.5, 0.0, 40.0, Got),
                sweep(2, 0.5, 42.0, 82.0, Got),
                sweep(3, 1.3, 84.0, 124.0, Got),
                sweep(4, 1.3, 126.0, 166.0, Now),
                sweep(5, 2.4, 168.0, 208.0, Ahead),
            ],
            210.0,
        )
    }

    fn build(
        entries: &[ElevationListEntry],
        selection: &ElevationSelection,
        product: RadarProduct,
        volume: Option<&ScanProjection>,
        clock: LadderClock,
        on_screen: Option<OnScreenCut>,
    ) -> VolumeLadder {
        VolumeLadder::build(
            LadderInputs {
                entries,
                selection,
                product,
                volume,
                clock,
                antenna_azimuth: Some(148.2),
                on_screen,
                role: TiltRole::Picks,
                fetching: false,
            },
            &Utc,
        )
    }

    /// The ladder while the volume draws every tilt.
    fn draws_all(
        entries: &[ElevationListEntry],
        volume: Option<&ScanProjection>,
        fetching: bool,
    ) -> VolumeLadder {
        VolumeLadder::build(
            LadderInputs {
                entries,
                selection: &FIXED_05,
                product: RadarProduct::Reflectivity,
                volume,
                clock: LadderClock::AtTime {
                    playhead_secs: 1_000_050.0,
                },
                antenna_azimuth: None,
                on_screen: None,
                role: TiltRole::DrawsAll,
                fetching,
            },
            &Utc,
        )
    }

    const FIXED_05: ElevationSelection = ElevationSelection::Fixed {
        elevation_number: 1,
        angle: 0.5,
    };

    // ── The one tilt list ───────────────────────────────────────────────

    #[wasm_bindgen_test]
    fn rows_are_latest_then_one_per_angle_lowest_first() {
        let rows = tilt_rows(&FIXED_05, &roster(), RadarProduct::Reflectivity);
        let labels: Vec<_> = rows.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels, ["Latest", "0.5°", "1.3°", "2.4°"]);
        assert!(rows[1].selected && !rows[0].selected);
        assert_eq!(rows[1].note.as_deref(), Some("CS · CD"));
        assert_eq!(rows[3].note.as_deref(), Some("B"));
    }

    #[wasm_bindgen_test]
    fn the_product_picks_the_cut_inside_a_split_row() {
        let rows = tilt_rows(&FIXED_05, &roster(), RadarProduct::Reflectivity);
        assert_eq!(
            rows[2].pick,
            TiltPick::Fixed {
                elevation_number: 3,
                angle: 1.3
            }
        );
        let rows = tilt_rows(&FIXED_05, &roster(), RadarProduct::Velocity);
        assert_eq!(
            rows[2].pick,
            TiltPick::Fixed {
                elevation_number: 4,
                angle: 1.3
            }
        );
        // The selected row stays selected whichever of its cuts is chosen.
        let on_doppler = ElevationSelection::Fixed {
            elevation_number: 2,
            angle: 0.5,
        };
        assert!(tilt_rows(&on_doppler, &roster(), RadarProduct::Velocity)[1].selected);
    }

    #[wasm_bindgen_test]
    fn no_row_is_disabled_but_an_unreached_angle_says_so() {
        let entries = vec![
            entry(1, 0.5, "CS", &["reflectivity"]),
            entry(2, 1.3, "CDW", &["reflectivity", "velocity"]),
        ];
        let rows = tilt_rows(&FIXED_05, &entries, RadarProduct::Velocity);
        assert!(!rows[1].reaches_product);
        assert!(rows[1].hover.contains("not recorded"));
        assert!(rows[2].reaches_product);
    }

    #[wasm_bindgen_test]
    fn sails_repeats_join_their_angle_and_are_counted() {
        let mut entries = roster();
        let mut s1 = entry(6, 0.5, "CS", &[]);
        s1.is_sails = true;
        let mut s2 = entry(7, 0.5, "CDW", &[]);
        s2.is_sails = true;
        entries.extend([s1, s2]);
        let rows = tilt_rows(&FIXED_05, &entries, RadarProduct::Reflectivity);
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[1].note.as_deref(), Some("CS · CD · SAILS ×1"));
        // The base cut, not the repeat, is what a click on the row picks.
        assert_eq!(
            rows[1].pick,
            TiltPick::Fixed {
                elevation_number: 1,
                angle: 0.5
            }
        );
    }

    #[wasm_bindgen_test]
    fn before_any_volume_the_list_offers_the_default_cut() {
        let rows = tilt_rows(&FIXED_05, &[], RadarProduct::Reflectivity);
        assert_eq!(rows.len(), 2);
        assert!(rows[1].selected);
    }

    #[wasm_bindgen_test]
    fn a_product_change_repicks_inside_the_same_row() {
        assert_eq!(
            repick_for_product(&FIXED_05, &roster(), RadarProduct::Velocity),
            Some(TiltPick::Fixed {
                elevation_number: 2,
                angle: 0.5
            })
        );
        assert_eq!(
            repick_for_product(&FIXED_05, &roster(), RadarProduct::Reflectivity),
            None
        );
        assert_eq!(
            repick_for_product(
                &ElevationSelection::Latest,
                &roster(),
                RadarProduct::Velocity
            ),
            None
        );
    }

    #[wasm_bindgen_test]
    fn e_steps_through_the_rows_with_latest_and_wraps() {
        let rows = tilt_rows(&FIXED_05, &roster(), RadarProduct::Reflectivity);
        assert_eq!(
            step_tilt(&rows, true),
            Some(TiltPick::Fixed {
                elevation_number: 3,
                angle: 1.3
            })
        );
        assert_eq!(step_tilt(&rows, false), Some(TiltPick::Latest));
        let rows = tilt_rows(
            &ElevationSelection::Latest,
            &roster(),
            RadarProduct::Reflectivity,
        );
        assert_eq!(
            step_tilt(&rows, false),
            Some(TiltPick::Fixed {
                elevation_number: 5,
                angle: 2.4
            })
        );
    }

    #[wasm_bindgen_test]
    fn picking_latest_remembers_the_fixed_cut_it_left() {
        let mut sel = FIXED_05;
        let mut last = None;
        apply_tilt_pick(&mut sel, &mut last, TiltPick::Latest);
        assert!(sel == ElevationSelection::Latest);
        assert_eq!(last, Some((1, 0.5)));
    }

    // ── Live, fixed tilt ────────────────────────────────────────────────

    #[wasm_bindgen_test]
    fn live_fixed_tilt_draws_received_filling_expected_and_now() {
        let v = live_volume();
        let l = build(
            &roster(),
            &FIXED_05,
            RadarProduct::Reflectivity,
            Some(&v),
            LadderClock::Live {
                now_secs: V0 + 135.0,
            },
            Some(OnScreenCut {
                elevation_number: 1,
                start_secs: V0,
            }),
        );
        assert_eq!(
            l.summary.as_deref(),
            Some("VCP 212 Precip Fast · 5 cuts · ~3:30")
        );
        assert_eq!(
            l.status.as_deref(),
            Some("on 1.3° · az 148° · next volume in ~1:15")
        );
        assert_eq!(l.caret, Some(Caret::OnRow(2)));
        let line = l.line.as_ref().unwrap();
        assert_eq!(line.kind, LineKind::Now);
        assert_eq!(line.label.as_deref(), Some("now · 2:15"));

        let marks: Vec<Vec<BarMark>> = l
            .rows
            .iter()
            .map(|r| r.bars.iter().map(|b| b.mark).collect())
            .collect();
        assert!(marks[0].is_empty(), "Latest prints a hint, not bars");
        assert_eq!(l.rows[0].hint, Some("follows the newest cut"));
        assert_eq!(marks[1], [BarMark::Received, BarMark::Received]);
        assert_eq!(marks[2][0], BarMark::Received);
        // Filled by what arrived (3 of 6 chunks), which leads the clock (9 s
        // of a 40 s cut).
        assert_eq!(marks[2][1], BarMark::Filling { progress: 0.5 });
        assert_eq!(marks[3], [BarMark::Expected]);

        // The 0.5° surveillance cut on screen is outlined, far left of now.
        assert!(l.rows[1].bars[0].on_screen && !l.rows[1].bars[1].on_screen);
        assert!(l.rows[1].bars[0].end < line.at);
    }

    #[wasm_bindgen_test]
    fn bars_sit_where_the_radar_makes_each_cut() {
        let v = live_volume();
        let l = build(
            &roster(),
            &FIXED_05,
            RadarProduct::Reflectivity,
            Some(&v),
            LadderClock::Live {
                now_secs: V0 + 135.0,
            },
            None,
        );
        assert_eq!(l.length_secs, 210.0);
        let b = &l.rows[2].bars;
        assert_eq!((b[0].start, b[0].end), (84.0, 124.0));
        assert_eq!((b[1].start, b[1].end), (126.0, 166.0));
        assert_eq!(l.rows[3].bars[0].start, 168.0);
        // Four moves join the five cuts in volume order, rows and gaps.
        assert_eq!(l.moves.len(), 4);
        assert_eq!((l.moves[1].from_row, l.moves[1].to_row), (1, 2));
        assert_eq!((l.moves[1].start, l.moves[1].end), (82.0, 84.0));
        assert_eq!(l.moves[1].state, MoveState::Done);
        assert_eq!(l.moves[3].state, MoveState::Ahead);
    }

    // ── Live, antenna moving ────────────────────────────────────────────

    #[wasm_bindgen_test]
    fn live_moving_puts_the_caret_between_rows_and_says_where_to() {
        let mut v = live_volume();
        v.sweeps[3].status = Got; // 1.3° CD done, 2.4° not started
        let l = build(
            &roster(),
            &ElevationSelection::Latest,
            RadarProduct::Reflectivity,
            Some(&v),
            LadderClock::Live {
                now_secs: V0 + 167.0,
            },
            None,
        );
        assert_eq!(l.caret, Some(Caret::Between { from: 2, to: 3 }));
        assert_eq!(l.status.as_deref(), Some("moving to 2.4° · ~0:01"));
        assert_eq!(l.moves[3].state, MoveState::Active);
        assert!(l.rows[0].tilt.selected);
    }

    #[wasm_bindgen_test]
    fn after_the_last_cut_the_header_counts_down_to_the_next_volume() {
        let mut v = live_volume();
        for s in &mut v.sweeps {
            s.status = Got;
        }
        let l = build(
            &roster(),
            &FIXED_05,
            RadarProduct::Reflectivity,
            Some(&v),
            LadderClock::Live {
                now_secs: V0 + 209.0,
            },
            None,
        );
        assert_eq!(l.caret, None);
        assert_eq!(l.status.as_deref(), Some("next volume in ~0:01"));
    }

    // ── The volume draws every tilt ──────────────────────────────────

    #[wasm_bindgen_test]
    fn draws_all_rows_stand_down_and_every_cut_on_device_is_on_screen() {
        let v = volume(
            vec![
                sweep(1, 0.5, 0.0, 40.0, Got),
                sweep(4, 1.3, 126.0, 166.0, Got),
            ],
            166.0,
        );
        let l = draws_all(&roster(), Some(&v), false);
        assert!(!l.picks);
        assert!(l.rows.iter().all(|r| !r.tilt.selected));
        assert_eq!(l.rows[0].hint, Some(DRAWS_ALL_HINT));
        assert_eq!(l.rows[1].tilt.hover, DRAWS_ALL_HOVER);
        // The cuts on the device are drawn; the hollow ones are not.
        for row in &l.rows {
            for b in &row.bars {
                assert_eq!(b.on_screen, b.mark == BarMark::OnDevice, "{}", b.hover);
            }
        }
        assert!(l
            .status
            .as_deref()
            .unwrap()
            .ends_with("2 of 5 cuts on device"));
        // With a fetch under way the status says the rest is coming.
        let l = draws_all(&roster(), Some(&v), true);
        assert!(l
            .status
            .as_deref()
            .unwrap()
            .ends_with("2 of 5 cuts on device · fetching the rest"));
        // A whole volume has nothing to fetch.
        let mut v = live_volume();
        for s in &mut v.sweeps {
            s.status = Got;
        }
        let l = draws_all(&roster(), Some(&v), true);
        assert!(l
            .status
            .as_deref()
            .unwrap()
            .ends_with("5 of 5 cuts on device"));
    }

    #[wasm_bindgen_test]
    fn picks_keeps_the_selection_and_the_one_cut_on_screen() {
        let v = volume(vec![sweep(1, 0.5, 0.0, 40.0, Got)], 40.0);
        let l = build(
            &roster(),
            &FIXED_05,
            RadarProduct::Reflectivity,
            Some(&v),
            LadderClock::AtTime {
                playhead_secs: V0 + 10.0,
            },
            Some(OnScreenCut {
                elevation_number: 1,
                start_secs: V0,
            }),
        );
        assert!(l.picks);
        assert!(l.rows[1].tilt.selected);
        assert!(l.rows[1].bars[0].on_screen);
    }

    // ── Archive ─────────────────────────────────────────────────────────

    #[wasm_bindgen_test]
    fn archive_draws_on_device_and_hollow_cuts_and_the_playhead() {
        // Two of five cuts on device; the rest are placed by the VCP between
        // them and are fetchable.
        let v = volume(
            vec![
                sweep(1, 0.5, 0.0, 40.0, Got),
                sweep(4, 1.3, 126.0, 166.0, Got),
            ],
            166.0,
        );
        let l = build(
            &roster(),
            &FIXED_05,
            RadarProduct::Reflectivity,
            Some(&v),
            LadderClock::AtTime {
                playhead_secs: V0 + 30.0,
            },
            None,
        );
        assert_eq!(
            l.status.as_deref(),
            Some("volume of 11:08:00 · 2 of 5 cuts on device")
        );
        assert_eq!(l.caret, None);
        let line = l.line.as_ref().unwrap();
        assert_eq!((line.kind, line.at), (LineKind::Playhead, 30.0));
        assert!(line.label.is_none());

        let r05 = &l.rows[1].bars;
        assert_eq!(r05[0].mark, BarMark::OnDevice);
        assert_eq!(r05[1].mark, BarMark::InArchive);
        assert_eq!(
            r05[1].fetch,
            Some(Intent::FetchScan {
                scan_start: V0 as i64,
                elevation_filter: Some(2)
            })
        );
        // The two missing cuts between 0:40 and 2:06 share that gap.
        assert_eq!(r05[1].start, 40.0);
        let r13 = &l.rows[2].bars;
        assert!((r13[0].end - 126.0).abs() < 1e-9);
        assert!(r05[1].estimated && !r05[0].estimated);
        // Every move is drawn as made: there is no antenna at a time.
        assert!(l.moves.iter().all(|m| m.state == MoveState::Done));
        assert_eq!(l.rows[0].hint, Some("the newest cut at the playhead"));
    }

    #[wasm_bindgen_test]
    fn no_volume_keeps_the_rows_and_says_why() {
        let l = build(
            &roster(),
            &FIXED_05,
            RadarProduct::Reflectivity,
            None,
            LadderClock::AtTime { playhead_secs: V0 },
            None,
        );
        assert_eq!(l.rows.len(), 4);
        assert!(l.rows.iter().all(|r| r.bars.is_empty()));
        assert_eq!(l.empty, Some("No volume at the playhead"));
        assert!(l.line.is_none() && l.summary.is_none());
    }

    #[wasm_bindgen_test]
    fn the_vcp_is_named_when_the_table_knows_it() {
        assert_eq!(vcp_label(212), "VCP 212 Precip Fast");
        assert_eq!(vcp_label(215), "VCP 215 Precipitation");
        assert_eq!(vcp_label(35), "VCP 35 Clear Air");
        assert_eq!(vcp_label(9999), "VCP 9999");
    }

    #[wasm_bindgen_test]
    fn mmss_reads_the_volume_clock() {
        assert_eq!(mmss(425.0), "7:05");
        assert_eq!(mmss(0.4), "0:00");
        assert_eq!(mmss(-3.0), "0:00");
    }
}
