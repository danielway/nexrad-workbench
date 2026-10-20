//! The phone timeline's row (phone timeline v2): a row of volumes you can
//! touch, decided here and drawn by `ui::timeline::mini`.
//!
//! Each volume is one mark. At NOW ([`StripZoom::Cuts`]) it is a block as
//! wide as the volume's span less a [`BLOCK_GAP_PX`] gap: filled when the
//! volume is on the device (with a faint separator between its cuts), hollow
//! when it is not, a white outline on the one under the playhead. The live
//! volume grows from its start to the red now line, filled by the share of
//! its cuts received. Labels under the blocks are the volumes' start times
//! (`live` under the live one). At 3 H ([`StripZoom::Volumes`]) each volume
//! is a [`COLUMN_W_PX`] column as tall as the weather it held (a floor of
//! [`COLUMN_MIN_H`] so a quiet day reads as a row of short even columns),
//! filled on the device, white under the playhead, with hour labels. At
//! 24 H ([`StripZoom::Day`]) the storm hill is a smooth area through its
//! half-hour samples (a monotone cubic, so it never overshoots a sample or
//! dips below the floor), the stretch on the device drawn in slate, and a
//! 2 px white line at the playhead; labels at 06, 12, 18 and 00.
//!
//! At every stop: a red now line with a small `NOW` cap above it when now
//! is in the window, and one amber bracket over a selected range (its end
//! on the now line when the range is pinned to now). There is no ring, no
//! playhead line at NOW and 3 H, no ruler and no tab: the white mark is the
//! playhead, the labels are the ruler, the transport row's LIVE button is
//! the way back.
//!
//! While the frame under the playhead is not on the device yet, the row
//! says so on its block (a progress arc, [`Arc`]) and the map says so
//! with a dim and a pill ([`LoadingVm`]). While it downloads the arc is the
//! bytes received over the bytes expected (the response's size, else the
//! listing's), from the queued share to the decoding one; without a size
//! it is the stage (queued, downloading, decoding); with none known the
//! arc spins.
//!
//! All pure, in the row's own points (x along the lane, y from the row's
//! top): the shell paints rectangles, lines and text where this says.

use crate::core::acquisition::DownloadProgress;
use crate::core::phone::MiniWindow;
use crate::core::phone_strip::{activity_bars, StripZoom, DAY_BUCKET_SECS};
use crate::core::storm_activity::{ScanSizes, ViewScale, MIN_BAR_FRAC};
use crate::core::timeline_view::{FrameCellState, ScanContainer};

// ───────────────────────────────────────────────────────────────────────────
// Geometry (points from the row's top)
// ───────────────────────────────────────────────────────────────────────────

/// The row's height.
pub(crate) const ROW_H: f32 = 66.0;

/// The blocks' row at NOW.
pub(crate) const BLOCK_TOP: f32 = 22.0;
pub(crate) const BLOCK_H: f32 = 26.0;
/// The gap a block leaves before the next volume.
pub(crate) const BLOCK_GAP_PX: f32 = 3.0;
/// The top of a block's label.
pub(crate) const BLOCK_LABEL_TOP: f32 = 52.0;

/// The columns at 3 H: their width, the line they stand on, their floor
/// and how much taller the stormiest one is.
pub(crate) const COLUMN_W_PX: f32 = 11.0;
pub(crate) const COLUMN_BOTTOM: f32 = 48.0;
pub(crate) const COLUMN_MIN_H: f32 = 6.0;
pub(crate) const COLUMN_RANGE_H: f32 = 26.0;

/// The hill at 24 H: the line it stands on and its tallest.
pub(crate) const HILL_BOTTOM: f32 = 48.0;
pub(crate) const HILL_H: f32 = 36.0;

/// How far apart (points) the 24 H hill's curve is sampled.
pub(crate) const HILL_STEP_PX: f32 = 2.0;

/// The top of the hour labels at 3 H and 24 H.
pub(crate) const HOUR_LABEL_TOP: f32 = 50.0;

/// The now line's extent and the top of its `NOW` cap.
pub(crate) const NOW_TOP: f32 = 16.0;
pub(crate) const NOW_BOTTOM: f32 = 56.0;
pub(crate) const NOW_CAP_TOP: f32 = 8.0;

/// The loop bracket: its bar and how far its end ticks reach down.
pub(crate) const BRACKET_TOP: f32 = 14.0;
pub(crate) const BRACKET_H: f32 = 3.0;
pub(crate) const BRACKET_TICK_H: f32 = 9.0;

/// How far before a container's start a listed volume's start may fall and
/// still be that container (a volume's key time can lead its first sweep).
const KEY_SLACK_SECS: f64 = 30.0;

/// The space kept between two labels.
const LABEL_GAP_PX: f32 = 4.0;

/// The arc's share at each download stage.
pub(crate) const QUEUED_SHARE: f32 = 0.15;
pub(crate) const DOWNLOADING_SHARE: f32 = 0.45;
pub(crate) const DECODING_SHARE: f32 = 0.8;

// ───────────────────────────────────────────────────────────────────────────
// The view-model
// ───────────────────────────────────────────────────────────────────────────

/// How far the frame under the playhead has come.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Arc {
    /// A share of the circle, from the bytes received or the download's
    /// stage.
    Share(f32),
    /// Nothing known yet: the arc spins.
    Spinning,
}

/// The frame under the playhead is on its way: the map dims and a small
/// pill says what is coming.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LoadingVm {
    /// The volume's start (Unix seconds).
    pub start: f64,
    /// Its start in the zone, `23:49`.
    pub time: String,
    /// `Loading 23:49`.
    pub label: String,
    pub arc: Arc,
}

/// One volume at NOW.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct BlockVm {
    pub x0: f32,
    pub x1: f32,
    /// Filled: some of it is on the device.
    pub on_device: bool,
    /// Under the playhead: the white outline.
    pub selected: bool,
    /// The live volume: open at the now line, filled by `fill`.
    pub live: bool,
    /// The filled share of the block, from its left (1 on the device, 0
    /// hollow, the cuts received for the live volume).
    pub fill: f32,
    /// Separators between its cuts, as x.
    pub cuts: Vec<f32>,
    /// The progress arc while its frame loads.
    pub arc: Option<Arc>,
}

/// One volume at 3 H.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ColumnVm {
    pub x0: f32,
    pub x1: f32,
    pub height: f32,
    pub on_device: bool,
    pub selected: bool,
}

/// One sliver of the 24 H hill: a quad from `x0` to `x1` standing on the
/// hill's line, `h0` tall at its left and `h1` at its right. Slivers are at
/// most [`HILL_STEP_PX`] wide and never overlap, so the shell fills each as
/// a convex quad and the row of them is the smooth area.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct HillPiece {
    pub x0: f32,
    pub x1: f32,
    pub h0: f32,
    pub h1: f32,
    /// Slate (on the device) rather than the storm blue.
    pub on_device: bool,
}

/// A label under the row.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RowLabel {
    /// Its left edge.
    pub x: f32,
    pub top: f32,
    pub text: String,
    /// The selected block's label reads brighter.
    pub bright: bool,
}

/// The amber bracket over a selected range.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Bracket {
    pub x0: f32,
    pub x1: f32,
    /// Each end's tick, when that end is on the lane.
    pub start_tick: bool,
    pub end_tick: bool,
}

/// The row, ready to paint.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct RowVm {
    pub blocks: Vec<BlockVm>,
    pub columns: Vec<ColumnVm>,
    pub hill: Vec<HillPiece>,
    /// The 24 H stop's white line at the playhead.
    pub playhead_x: Option<f32>,
    pub labels: Vec<RowLabel>,
    pub now_x: Option<f32>,
    pub bracket: Option<Bracket>,
    pub loading: Option<LoadingVm>,
    /// The start of the volume under the playhead: what the chip names for
    /// a point, so it reads as the block's label does.
    pub selected_start: Option<f64>,
}

/// The download spans the acquisition reports, in whole seconds.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Downloads<'a> {
    pub queued: &'a [(i64, i64)],
    pub downloading: &'a [(i64, i64)],
    /// Downloaded, being decoded and stored.
    pub decoding: &'a [(i64, i64)],
    /// The bytes each download in flight has received, by its scan start.
    pub bytes: &'a [(i64, DownloadProgress)],
}

/// Everything the row reads.
pub(crate) struct RowInputs<'a> {
    pub stop: StripZoom,
    pub window: MiniWindow,
    /// The lane's left and right x.
    pub lane: (f32, f32),
    pub now: f64,
    pub playhead: f64,
    /// Pinned to now (a live point, or a range ending at now).
    pub pinned: bool,
    /// The displayed scan's time, when a frame is on screen.
    pub active_ts: Option<f64>,
    /// The strip's volumes (the next volume's ghost is skipped).
    pub containers: &'a [ScanContainer],
    /// The day's listing: volumes with no container, and the weather they
    /// held.
    pub sizes: &'a ScanSizes,
    /// The selected range as drawn (its end on now when pinned).
    pub range: Option<(f64, f64)>,
    pub downloads: Downloads<'a>,
    /// The zone, seconds east of UTC.
    pub tz_offset_secs: i64,
    /// A label's width in points.
    pub measure: &'a dyn Fn(&str) -> f32,
}

// ───────────────────────────────────────────────────────────────────────────
// Volumes
// ───────────────────────────────────────────────────────────────────────────

/// One volume as the row reads it.
#[derive(Clone, Debug)]
struct Vol<'a> {
    start: f64,
    /// Its end; the live volume's is its expected end.
    end: f64,
    on_device: bool,
    live: bool,
    container: Option<&'a ScanContainer>,
}

impl Vol<'_> {
    fn holds(&self, ts: f64) -> bool {
        ts >= self.start - 1.0 && ts < self.end.max(self.start + 1.0)
    }

    /// Where its mark ends: the live volume's at now.
    fn shown_end(&self, now: f64) -> f64 {
        if self.live {
            now.max(self.start)
        } else {
            self.end.max(self.start)
        }
    }
}

/// The strip's volumes and the listed ones no container covers, oldest
/// first. Nothing past now.
fn volumes<'a>(containers: &'a [ScanContainer], sizes: &ScanSizes, now: f64) -> Vec<Vol<'a>> {
    let mut out: Vec<Vol<'a>> = containers
        .iter()
        .filter(|c| !c.is_projected && c.start_secs <= now)
        .map(|c| Vol {
            start: c.start_secs,
            end: c.end_secs.max(c.start_secs),
            on_device: !c.is_available,
            live: c.is_live,
            container: Some(c),
        })
        .collect();
    let covered = |ts: f64| {
        containers.iter().any(|c| {
            !c.is_projected
                && ts >= c.start_secs - KEY_SLACK_SECS
                && ts < c.end_secs.max(c.start_secs + 1.0)
        })
    };
    out.extend(
        sizes
            .scans
            .iter()
            .filter(|s| s.start <= now && !covered(s.start))
            .map(|s| Vol {
                start: s.start,
                end: s.end.max(s.start),
                on_device: false,
                live: false,
                container: None,
            }),
    );
    out.sort_by(|a, b| a.start.total_cmp(&b.start));
    out
}

/// The volume under the playhead: the newest that holds it; pinned, the
/// newest that has started (the live edge can run past the last volume's
/// end).
fn selected_index(vols: &[Vol<'_>], playhead: f64, pinned: bool) -> Option<usize> {
    let holding = vols
        .iter()
        .enumerate()
        .filter(|(_, v)| v.holds(playhead))
        .max_by(|a, b| a.1.start.total_cmp(&b.1.start))
        .map(|(i, _)| i);
    holding.or_else(|| {
        pinned
            .then(|| vols.iter().rposition(|v| v.start <= playhead + 1.0))
            .flatten()
    })
}

/// The live volume's filled share: its cuts received over its cuts expected
/// (a cut coming in counts its chunks), or the time gone when its cuts
/// aren't known.
fn live_fill(v: &Vol<'_>, now: f64) -> f32 {
    let cells = v.container.map_or(&[][..], |c| &c.cells[..]);
    let share = if cells.is_empty() {
        let span = v.end - v.start;
        if span > 0.0 {
            ((now - v.start) / span) as f32
        } else {
            1.0
        }
    } else {
        let got: f32 = cells
            .iter()
            .map(|c| match c.state {
                FrameCellState::Projected => 0.0,
                FrameCellState::InFlight => c
                    .chunks
                    .as_ref()
                    .filter(|p| p.chunks_expected > 0)
                    .map_or(0.5, |p| p.chunks_received as f32 / p.chunks_expected as f32),
                _ => 1.0,
            })
            .sum();
        got / cells.len() as f32
    };
    share.clamp(0.0, 1.0)
}

/// The arc's share while a download is under way: the bytes received over
/// the bytes expected, from [`DOWNLOADING_SHARE`] (nothing yet) to
/// [`DECODING_SHARE`] (all of it, decoding next), or the stage's
/// [`DOWNLOADING_SHARE`] when the size isn't known, so the arc only ever
/// moves forward (queued, then downloading, then decoding).
pub(crate) fn downloading_share(progress: Option<DownloadProgress>, listed: Option<u64>) -> f32 {
    match progress.and_then(|p| p.fraction(listed)) {
        Some(f) => DOWNLOADING_SHARE + (DECODING_SHARE - DOWNLOADING_SHARE) * f,
        None => DOWNLOADING_SHARE,
    }
}

/// How far the volume's download has come, from the acquisition's spans and
/// the bytes received (over the response's size, else the listing's).
fn download_arc(v: &Vol<'_>, d: &Downloads<'_>, sizes: &ScanSizes) -> Arc {
    let mine = |s: f64| s >= v.start - KEY_SLACK_SECS && s < v.end.max(v.start + 1.0);
    let hits = |spans: &[(i64, i64)]| {
        spans.iter().any(|&(s, e)| {
            let (s, e) = (s as f64, e as f64);
            s < v.end.max(v.start + 1.0) && e.max(s + 1.0) > v.start - KEY_SLACK_SECS
        })
    };
    if hits(d.decoding) {
        Arc::Share(DECODING_SHARE)
    } else if hits(d.downloading) {
        let progress = d
            .bytes
            .iter()
            .find(|(k, _)| mine(*k as f64))
            .map(|(_, p)| *p);
        let listed = sizes
            .scans
            .iter()
            .find(|s| (s.start - v.start).abs() <= KEY_SLACK_SECS)
            .map(|s| u64::from(s.bytes));
        Arc::Share(downloading_share(progress, listed))
    } else if hits(d.queued) {
        Arc::Share(QUEUED_SHARE)
    } else {
        Arc::Spinning
    }
}

/// `HH:MM` in the zone.
fn hm(ts: f64, tz_offset_secs: i64) -> String {
    let secs = (ts + tz_offset_secs as f64).rem_euclid(86_400.0) as i64;
    format!("{:02}:{:02}", secs / 3600, (secs % 3600) / 60)
}

/// What the map and the selected block say while the playhead's frame is
/// on its way: only when the playhead sits on its own (not pinned to now),
/// its volume is not on the device, and the frame on screen is from
/// another volume. `None` otherwise.
fn loading_vm(i: &RowInputs<'_>, vols: &[Vol<'_>], selected: Option<usize>) -> Option<LoadingVm> {
    if i.pinned {
        return None;
    }
    let v = &vols[selected?];
    if v.on_device || i.active_ts.is_some_and(|ts| v.holds(ts)) {
        return None;
    }
    let time = hm(v.start, i.tz_offset_secs);
    Some(LoadingVm {
        start: v.start,
        label: format!("Loading {time}"),
        time,
        arc: download_arc(v, &i.downloads, i.sizes),
    })
}

// ───────────────────────────────────────────────────────────────────────────
// The row
// ───────────────────────────────────────────────────────────────────────────

/// Build the row at its stop.
pub(crate) fn row_vm(i: &RowInputs<'_>) -> RowVm {
    let (left, right) = i.lane;
    let w = i.window;
    let px_per_sec = ((right - left) as f64 / w.span().max(1.0)) as f32;
    let to_x = |t: f64| left + ((t - w.start) as f32) * px_per_sec;
    let vols = volumes(i.containers, i.sizes, i.now);
    let selected = selected_index(&vols, i.playhead, i.pinned);
    let loading = loading_vm(i, &vols, selected);
    let in_lane = |x0: f32, x1: f32| x1 >= left && x0 <= right;

    let now_x = Some(to_x(i.now)).filter(|x| *x >= left && *x <= right);
    let mut out = RowVm {
        now_x,
        loading: loading.clone(),
        selected_start: selected.map(|k| vols[k].start),
        ..RowVm::default()
    };

    match i.stop {
        StripZoom::Cuts => {
            let mut labels: Vec<(bool, RowLabel)> = Vec::new();
            for (k, v) in vols.iter().enumerate() {
                let x0 = to_x(v.start);
                let x1 = if v.live {
                    to_x(i.now)
                } else {
                    to_x(v.end) - BLOCK_GAP_PX
                }
                .max(x0 + 2.0);
                if !in_lane(x0, x1) {
                    continue;
                }
                let is_sel = selected == Some(k);
                let fill = if v.live {
                    live_fill(v, i.now)
                } else if v.on_device {
                    1.0
                } else {
                    0.0
                };
                let cuts = if v.on_device && !v.live {
                    cut_separators(v, &to_x, x0, x1)
                } else {
                    Vec::new()
                };
                out.blocks.push(BlockVm {
                    x0,
                    x1,
                    on_device: v.on_device,
                    selected: is_sel,
                    live: v.live,
                    fill,
                    cuts,
                    arc: loading.as_ref().filter(|_| is_sel).map(|l| l.arc),
                });
                if x0 < left {
                    // Cut by the lane's edge: its label would name a block
                    // mostly out of view and crowd the next one's.
                    continue;
                }
                let text = if v.live {
                    "live".to_string()
                } else {
                    hm(v.start, i.tz_offset_secs)
                };
                labels.push((
                    is_sel,
                    RowLabel {
                        x: x0.max(left) + 2.0,
                        top: BLOCK_LABEL_TOP,
                        text,
                        bright: is_sel,
                    },
                ));
            }
            out.labels = place_labels(labels, (left, right), None, i.measure);
        }
        StripZoom::Volumes => {
            let scale = ViewScale::from_levels(
                i.sizes
                    .scans
                    .iter()
                    .filter(|s| s.end >= w.start && s.start <= w.end)
                    .map(|s| s.level),
            );
            for (k, v) in vols.iter().enumerate() {
                let end = v.shown_end(i.now);
                let (a, b) = (to_x(v.start), to_x(end));
                let width = COLUMN_W_PX.min((b - a - 2.0).max(2.0));
                let mid = (a + b) / 2.0;
                let (x0, x1) = (mid - width / 2.0, mid + width / 2.0);
                if !in_lane(x0, x1) {
                    continue;
                }
                let share = i
                    .sizes
                    .at((v.start + end) / 2.0)
                    .map_or(0.0, |s| {
                        (scale.height_frac(s.level) - MIN_BAR_FRAC) / (1.0 - MIN_BAR_FRAC)
                    })
                    .clamp(0.0, 1.0);
                out.columns.push(ColumnVm {
                    x0,
                    x1,
                    height: COLUMN_MIN_H + share * COLUMN_RANGE_H,
                    on_device: v.on_device,
                    selected: selected == Some(k),
                });
            }
            out.labels = hour_labels(i, &to_x, 1, &|h| format!("{h:02}:00"));
        }
        StripZoom::Day => {
            out.hill = hill(i, &vols, &to_x);
            out.playhead_x = Some(to_x(i.playhead)).filter(|x| *x >= left && *x <= right);
            out.labels = hour_labels(i, &to_x, 6, &|h| format!("{h:02}"));
        }
    }

    out.bracket = i.range.and_then(|(s, e)| {
        // From the start of the block the range's first frame is in, so the
        // bracket covers the blocks it plays.
        let s = vols
            .iter()
            .filter(|v| v.holds(s))
            .map(|v| v.start)
            .fold(s, f64::min);
        let (xs, xe) = (to_x(s), to_x(e.max(s)));
        let (x0, x1) = (xs.max(left), xe.min(right));
        (x1 - x0 >= 1.0).then_some(Bracket {
            x0,
            x1,
            start_tick: xs >= left,
            end_tick: xe <= right,
        })
    });
    out
}

/// A block's cut separators: at each cut's start after the first, kept
/// clear of the block's ends and of each other.
fn cut_separators(v: &Vol<'_>, to_x: &dyn Fn(f64) -> f32, x0: f32, x1: f32) -> Vec<f32> {
    let Some(c) = v.container else {
        return Vec::new();
    };
    let mut starts: Vec<f64> = c
        .cells
        .iter()
        .filter(|cell| cell.state != FrameCellState::Projected)
        .map(|cell| cell.start_secs)
        .collect();
    starts.sort_by(f64::total_cmp);
    let mut out: Vec<f32> = Vec::new();
    for s in starts.into_iter().skip(1) {
        let x = to_x(s);
        if x > x0 + 3.0 && x < x1 - 3.0 && out.last().is_none_or(|l| x - l >= 3.0) {
            out.push(x);
        }
    }
    out
}

/// Labels at every `every`-th hour of the zone, centred on their hour, as
/// many as fit: inside the lane, clear of each other and of the now line.
fn hour_labels(
    i: &RowInputs<'_>,
    to_x: &dyn Fn(f64) -> f32,
    every: i64,
    text: &dyn Fn(i64) -> String,
) -> Vec<RowLabel> {
    let off = i.tz_offset_secs as f64;
    let step = 3600.0 * every as f64;
    let mut t = ((i.window.start + off) / step).ceil() * step - off;
    let mut labels = Vec::new();
    while t <= i.window.end {
        let hour = ((t + off).rem_euclid(86_400.0) / 3600.0).round() as i64 % 24;
        let s = text(hour);
        let width = (i.measure)(&s);
        labels.push((
            false,
            RowLabel {
                x: to_x(t) - width / 2.0,
                top: HOUR_LABEL_TOP,
                text: s,
                bright: false,
            },
        ));
        t += step;
    }
    let now_x = Some(to_x(i.now)).filter(|x| *x >= i.lane.0 && *x <= i.lane.1);
    place_labels(labels, i.lane, now_x, i.measure)
}

/// The labels that fit, left to right: each inside the lane, clear of the
/// now line and of the labels already placed. Preferred labels (the
/// selected block's) are placed first.
fn place_labels(
    labels: Vec<(bool, RowLabel)>,
    (left, right): (f32, f32),
    now_x: Option<f32>,
    measure: &dyn Fn(&str) -> f32,
) -> Vec<RowLabel> {
    let mut order: Vec<(bool, RowLabel, f32)> = labels
        .into_iter()
        .map(|(p, l)| {
            let w = measure(&l.text);
            (p, l, w)
        })
        .collect();
    // Preferred first, then left to right (a stable sort keeps the order).
    order.sort_by_key(|(p, _, _)| !*p);
    let mut placed: Vec<(f32, f32, RowLabel)> = Vec::new();
    for (_, l, w) in order {
        let (a, b) = (l.x, l.x + w);
        let fits = a >= left
            && b <= right
            && now_x.is_none_or(|n| b + LABEL_GAP_PX <= n - 1.0 || a >= n + 1.0 + LABEL_GAP_PX)
            && placed
                .iter()
                .all(|(pa, pb, _)| b + LABEL_GAP_PX <= *pa || a >= *pb + LABEL_GAP_PX);
        if fits {
            placed.push((a, b, l));
        }
    }
    placed.sort_by(|a, b| a.0.total_cmp(&b.0));
    placed.into_iter().map(|(_, _, l)| l).collect()
}

/// A monotone cubic through `knots` (x ascending, at least one), evaluated
/// at `x`: Fritsch–Carlson tangents (PCHIP), so between two knots it stays
/// between their values and never overshoots. Flat past either end.
pub(crate) fn monotone_cubic(knots: &[(f32, f32)], x: f32) -> f32 {
    let n = knots.len();
    if n == 0 {
        return 0.0;
    }
    if n == 1 || x <= knots[0].0 {
        return knots[0].1;
    }
    if x >= knots[n - 1].0 {
        return knots[n - 1].1;
    }
    let k = knots.partition_point(|p| p.0 <= x).clamp(1, n - 1) - 1;
    let h = |j: usize| knots[j + 1].0 - knots[j].0;
    let d = |j: usize| (knots[j + 1].1 - knots[j].1) / h(j);
    let tangent = |j: usize| -> f32 {
        if j == 0 {
            return d(0);
        }
        if j == n - 1 {
            return d(n - 2);
        }
        let (d0, d1) = (d(j - 1), d(j));
        if d0 * d1 <= 0.0 {
            return 0.0;
        }
        let (h0, h1) = (h(j - 1), h(j));
        let (w1, w2) = (2.0 * h1 + h0, h1 + 2.0 * h0);
        (w1 + w2) / (w1 / d0 + w2 / d1)
    };
    let (x0, y0) = knots[k];
    let (x1, y1) = knots[k + 1];
    let hk = x1 - x0;
    // End tangents are clamped like the interior ones: never against the
    // secant, never more than three times it.
    let clamp = |m: f32, dk: f32| {
        if m * dk <= 0.0 {
            0.0
        } else if m.abs() > 3.0 * dk.abs() {
            3.0 * dk
        } else {
            m
        }
    };
    let dk = d(k);
    let (m0, m1) = (clamp(tangent(k), dk), clamp(tangent(k + 1), dk));
    let t = (x - x0) / hk;
    let (t2, t3) = (t * t, t * t * t);
    let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
    let h10 = t3 - 2.0 * t2 + t;
    let h01 = -2.0 * t3 + 3.0 * t2;
    let h11 = t3 - t2;
    h00 * y0 + h10 * hk * m0 + h01 * y1 + h11 * hk * m1
}

/// Consecutive half-hours of the 24 H hill: its x extent and a knot at each
/// bucket's middle.
struct HillRun {
    x0: f32,
    x1: f32,
    knots: Vec<(f32, f32)>,
}

/// The 24 H hill: a smooth area through the half-hour samples in the storm
/// blue, the stretch the device holds in slate. Consecutive half-hours make
/// one run, a monotone cubic through each bucket's middle (flat at the
/// run's ends); stretches the listing has no sizes for draw at the floor
/// where a volume is; elsewhere nothing. Sampled every [`HILL_STEP_PX`]
/// across the lane and at every edge (a run's, a held stretch's, a
/// volume's), as slivers left to right.
fn hill(i: &RowInputs<'_>, vols: &[Vol<'_>], to_x: &dyn Fn(f64) -> f32) -> Vec<HillPiece> {
    let floor = HILL_H * MIN_BAR_FRAC;
    let (left, right) = i.lane;
    if right <= left {
        return Vec::new();
    }
    // Runs of consecutive half-hours.
    let mut runs: Vec<HillRun> = Vec::new();
    let mut last_start: Option<f64> = None;
    for b in activity_bars(StripZoom::Day, i.window, i.sizes) {
        let (a, e) = (to_x(b.start), to_x(b.end.max(b.start)));
        let knot = ((a + e) / 2.0, HILL_H * b.frac);
        let joined = last_start.is_some_and(|s| b.start - s <= 1.5 * DAY_BUCKET_SECS);
        last_start = Some(b.start);
        match runs.last_mut() {
            Some(run) if joined => {
                run.x1 = run.x1.max(e);
                if knot.0 > run.knots.last().map_or(f32::MIN, |k| k.0) + 1e-3 {
                    run.knots.push(knot);
                }
            }
            _ => runs.push(HillRun {
                x0: a,
                x1: e,
                knots: vec![knot],
            }),
        }
    }
    // The held stretches, merged when closer than two points.
    let mut held: Vec<(f32, f32)> = Vec::new();
    for v in vols.iter().filter(|v| v.on_device) {
        let (a, b) = (
            to_x(v.start),
            to_x(v.shown_end(i.now)).max(to_x(v.start) + 1.0),
        );
        match held.last_mut() {
            Some(last) if a - last.1 <= 2.0 => last.1 = last.1.max(b),
            _ => held.push((a, b)),
        }
    }
    let spans: Vec<(f32, f32)> = vols
        .iter()
        .map(|v| (to_x(v.start), to_x(v.shown_end(i.now))))
        .collect();
    // Where to sample: every step and every edge, inside the lane.
    let mut xs: Vec<f32> = Vec::new();
    let mut x = left;
    while x < right {
        xs.push(x);
        x += HILL_STEP_PX;
    }
    xs.push(right);
    for r in &runs {
        xs.extend([r.x0, r.x1]);
    }
    for &(a, b) in held.iter().chain(&spans) {
        xs.extend([a, b]);
    }
    xs.retain(|x| *x >= left && *x <= right);
    xs.sort_by(f32::total_cmp);
    xs.dedup_by(|b, a| *b - *a < 0.05);
    let mut out = Vec::new();
    for w in xs.windows(2) {
        let (x0, x1) = (w[0], w[1]);
        let mid = (x0 + x1) / 2.0;
        let (h0, h1) = if let Some(run) = runs.iter().find(|r| mid >= r.x0 && mid <= r.x1) {
            let at = |x: f32| monotone_cubic(&run.knots, x).max(floor);
            (at(x0), at(x1))
        } else if spans.iter().any(|&(a, b)| mid >= a && mid <= b) {
            (floor, floor)
        } else {
            continue;
        };
        out.push(HillPiece {
            x0,
            x1,
            h0,
            h1,
            on_device: held.iter().any(|&(a, b)| mid >= a && mid <= b),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::storm_activity::ScanActivity;
    use crate::core::timeline_view::{CellChunkProgress, FrameCell};
    use wasm_bindgen_test::wasm_bindgen_test;

    fn cell(start: f64, end: f64, state: FrameCellState) -> FrameCell {
        FrameCell {
            start_secs: start,
            end_secs: end,
            elevation_number: 1,
            elevation_angle: 0.5,
            state,
            chunks: None,
            is_active: false,
            is_prev_active: false,
        }
    }

    fn held(start: f64, end: f64, cuts: &[f64]) -> ScanContainer {
        ScanContainer {
            start_secs: start,
            end_secs: end,
            key_secs: start,
            vcp: 212,
            cells: cuts
                .iter()
                .map(|&s| cell(s, s + 30.0, FrameCellState::Cached))
                .collect(),
            sweep_spans: Vec::new(),
            is_available: false,
            is_live: false,
            is_projected: false,
        }
    }

    fn not_held(start: f64, end: f64) -> ScanContainer {
        ScanContainer {
            is_available: true,
            cells: vec![cell(start, end, FrameCellState::Available)],
            ..held(start, end, &[])
        }
    }

    /// Volumes at 0, 420, 840 (not held but the first), and the live one at
    /// 1260 with two of four cuts in; now 1600.
    fn strip() -> Vec<ScanContainer> {
        let mut live = held(1260.0, 1680.0, &[]);
        live.is_live = true;
        live.cells = vec![
            cell(1260.0, 1290.0, FrameCellState::Cached),
            cell(1380.0, 1410.0, FrameCellState::Cached),
            cell(1500.0, 1530.0, FrameCellState::Projected),
            cell(1620.0, 1650.0, FrameCellState::Projected),
        ];
        let ghost = ScanContainer {
            is_projected: true,
            ..held(1680.0, 2100.0, &[1680.0])
        };
        vec![
            held(0.0, 420.0, &[0.0, 140.0, 280.0]),
            not_held(420.0, 840.0),
            not_held(840.0, 1260.0),
            live,
            ghost,
        ]
    }

    const NOW: f64 = 1600.0;

    fn measure(s: &str) -> f32 {
        6.0 * s.chars().count() as f32
    }

    fn inputs<'a>(
        stop: StripZoom,
        containers: &'a [ScanContainer],
        sizes: &'a ScanSizes,
        playhead: f64,
        pinned: bool,
    ) -> RowInputs<'a> {
        RowInputs {
            stop,
            // 1 point per 5 s across 0..1700 on a 340-point lane.
            window: MiniWindow {
                start: -100.0,
                end: 1_700.0,
            },
            lane: (0.0, 360.0),
            now: NOW,
            playhead,
            pinned,
            active_ts: Some(1260.0),
            containers,
            sizes,
            range: None,
            downloads: Downloads::default(),
            tz_offset_secs: 0,
            measure: &measure,
        }
    }

    fn x(t: f64) -> f32 {
        ((t + 100.0) / 5.0) as f32
    }

    #[wasm_bindgen_test]
    fn now_is_one_block_per_volume_filled_on_the_device_hollow_elsewhere() {
        let cs = strip();
        let sz = ScanSizes::default();
        let row = row_vm(&inputs(StripZoom::Cuts, &cs, &sz, NOW, true));
        assert_eq!(row.blocks.len(), 4, "the ghost is not a block");
        let b = &row.blocks;
        assert_eq!((b[0].x0, b[0].x1), (x(0.0), x(420.0) - BLOCK_GAP_PX));
        assert!(b[0].on_device && b[0].fill == 1.0);
        assert!(!b[1].on_device && b[1].fill == 0.0);
        assert!(!b[2].on_device);
        // The live block grows to the now line, half its cuts in.
        assert!(b[3].live && b[3].on_device);
        assert_eq!(b[3].x1, x(NOW));
        assert_eq!(b[3].fill, 0.5);
        // Pinned, the live block is the one looked at.
        assert_eq!(
            b.iter().map(|b| b.selected).collect::<Vec<_>>(),
            vec![false, false, false, true]
        );
        assert_eq!(row.now_x, Some(x(NOW)));
    }

    #[wasm_bindgen_test]
    fn a_held_block_shows_a_separator_between_its_cuts() {
        let cs = strip();
        let sz = ScanSizes::default();
        let row = row_vm(&inputs(StripZoom::Cuts, &cs, &sz, NOW, true));
        assert_eq!(row.blocks[0].cuts, vec![x(140.0), x(280.0)]);
        assert!(row.blocks[1].cuts.is_empty(), "nothing to separate");
        assert!(row.blocks[3].cuts.is_empty(), "the live one fills instead");
    }

    #[wasm_bindgen_test]
    fn labels_are_start_times_and_live_and_give_way_to_each_other() {
        let cs = strip();
        let sz = ScanSizes::default();
        let row = row_vm(&inputs(StripZoom::Cuts, &cs, &sz, NOW, true));
        let words: Vec<_> = row.labels.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(words, vec!["00:00", "00:07", "00:14", "live"]);
        assert!(row.labels[3].bright && !row.labels[0].bright);
        assert_eq!(row.labels[0].x, x(0.0) + 2.0);
        // A block cut by the lane's left edge draws, unlabelled.
        let mut i = inputs(StripZoom::Cuts, &cs, &sz, NOW, true);
        i.window.start = 100.0;
        let row = row_vm(&i);
        assert!(row.blocks[0].x0 < i.lane.0);
        let words: Vec<_> = row.labels.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(words, vec!["00:07", "00:14", "live"]);
        // Crowded, the selected one's label stays and the others give way.
        let crowd = |s: &str| 40.0 * s.len() as f32;
        let mut i = inputs(StripZoom::Cuts, &cs, &sz, 500.0, false);
        i.measure = &crowd;
        let row = row_vm(&i);
        assert!(row.labels.iter().any(|l| l.text == "00:07"));
        for w in row.labels.windows(2) {
            assert!(w[0].x + crowd(&w[0].text) <= w[1].x);
        }
    }

    #[wasm_bindgen_test]
    fn a_tapped_hollow_block_loads_with_an_arc_and_a_pill() {
        let cs = strip();
        let sz = ScanSizes::default();
        let row = row_vm(&inputs(StripZoom::Cuts, &cs, &sz, 430.0, false));
        let sel = row.blocks.iter().find(|b| b.selected).unwrap();
        assert_eq!(sel.x0, x(420.0));
        assert_eq!(sel.arc, Some(Arc::Spinning), "nothing known: it spins");
        let l = row.loading.unwrap();
        assert_eq!(l.label, "Loading 00:07");
        assert_eq!(l.start, 420.0);
        assert_eq!(row.selected_start, Some(420.0));
        // Only the selected block carries the arc.
        assert_eq!(row.blocks.iter().filter(|b| b.arc.is_some()).count(), 1);
    }

    #[wasm_bindgen_test]
    fn the_arc_follows_the_download_stage() {
        let cs = strip();
        let sz = ScanSizes::default();
        let span = [(420_i64, 840_i64)];
        for (d, share) in [
            (
                Downloads {
                    queued: &span,
                    ..Downloads::default()
                },
                QUEUED_SHARE,
            ),
            (
                Downloads {
                    downloading: &span,
                    queued: &span,
                    ..Downloads::default()
                },
                DOWNLOADING_SHARE,
            ),
            (
                Downloads {
                    decoding: &span,
                    ..Downloads::default()
                },
                DECODING_SHARE,
            ),
        ] {
            let mut i = inputs(StripZoom::Cuts, &cs, &sz, 430.0, false);
            i.downloads = d;
            assert_eq!(row_vm(&i).loading.unwrap().arc, Arc::Share(share));
        }
        // Another volume's download is not this one's.
        let other = [(840_i64, 1260_i64)];
        let mut i = inputs(StripZoom::Cuts, &cs, &sz, 430.0, false);
        i.downloads = Downloads {
            downloading: &other,
            ..Downloads::default()
        };
        assert_eq!(row_vm(&i).loading.unwrap().arc, Arc::Spinning);
    }

    #[wasm_bindgen_test]
    fn the_downloading_share_runs_from_downloading_to_decoding_by_bytes() {
        let at = |received, total| Some(DownloadProgress { received, total });
        assert_eq!(downloading_share(at(0, Some(100)), None), DOWNLOADING_SHARE);
        assert_eq!(downloading_share(at(100, Some(100)), None), DECODING_SHARE);
        let half = downloading_share(at(50, Some(100)), None);
        assert!((half - (DOWNLOADING_SHARE + DECODING_SHARE) / 2.0).abs() < 1e-6);
        // No response size: the listing's.
        assert_eq!(downloading_share(at(25, None), Some(25)), DECODING_SHARE);
        // No size anywhere, or no bytes yet: the stage.
        assert_eq!(downloading_share(at(25, None), None), DOWNLOADING_SHARE);
        assert_eq!(downloading_share(None, Some(100)), DOWNLOADING_SHARE);
    }

    #[wasm_bindgen_test]
    fn the_arc_follows_the_bytes_received() {
        let cs = strip();
        let span = [(420_i64, 840_i64)];
        // The listing says 4 MB; the response has said nothing of its size.
        let sz = ScanSizes {
            scans: vec![ScanActivity {
                start: 420.0,
                end: 840.0,
                bytes: 4_000_000,
                level: 0.5,
            }],
            known_days: vec![0],
        };
        let mut last = 0.0;
        for received in [0_u64, 1_000_000, 2_000_000, 4_000_000] {
            let bytes = [(
                425_i64,
                DownloadProgress {
                    received,
                    total: None,
                },
            )];
            let mut i = inputs(StripZoom::Cuts, &cs, &sz, 430.0, false);
            i.downloads = Downloads {
                downloading: &span,
                bytes: &bytes,
                ..Downloads::default()
            };
            let Arc::Share(share) = row_vm(&i).loading.unwrap().arc else {
                panic!("a share while downloading");
            };
            assert!(share >= last, "the arc only advances");
            last = share;
        }
        assert_eq!(last, DECODING_SHARE, "all of it: decoding next");
        // Another download's bytes are not this one's.
        let bytes = [(
            900_i64,
            DownloadProgress {
                received: 4_000_000,
                total: Some(4_000_000),
            },
        )];
        let mut i = inputs(StripZoom::Cuts, &cs, &sz, 430.0, false);
        i.downloads = Downloads {
            downloading: &span,
            bytes: &bytes,
            ..Downloads::default()
        };
        assert_eq!(
            row_vm(&i).loading.unwrap().arc,
            Arc::Share(DOWNLOADING_SHARE),
            "no bytes reported: the stage"
        );
    }

    #[wasm_bindgen_test]
    fn nothing_loads_when_pinned_held_or_already_on_screen() {
        let cs = strip();
        let sz = ScanSizes::default();
        // Pinned to now.
        assert!(row_vm(&inputs(StripZoom::Cuts, &cs, &sz, 430.0, true))
            .loading
            .is_none());
        // A held volume: its frame is a render away, not a download.
        assert!(row_vm(&inputs(StripZoom::Cuts, &cs, &sz, 150.0, false))
            .loading
            .is_none());
        // The frame on screen is already this volume's.
        let mut i = inputs(StripZoom::Cuts, &cs, &sz, 430.0, false);
        i.active_ts = Some(425.0);
        assert!(row_vm(&i).loading.is_none());
        // Loading is the same at every stop.
        for stop in [StripZoom::Volumes, StripZoom::Day] {
            assert!(row_vm(&inputs(stop, &cs, &sz, 430.0, false))
                .loading
                .is_some());
        }
    }

    #[wasm_bindgen_test]
    fn the_live_fill_counts_a_cut_coming_in_by_its_chunks() {
        let mut cs = strip();
        cs[3].cells[2].state = FrameCellState::InFlight;
        cs[3].cells[2].chunks = Some(CellChunkProgress {
            chunks_expected: 4,
            chunks_received: 2,
            partial_radials: 0,
        });
        let sz = ScanSizes::default();
        let row = row_vm(&inputs(StripZoom::Cuts, &cs, &sz, NOW, true));
        assert_eq!(row.blocks[3].fill, 2.5 / 4.0);
    }

    fn scan(start: f64, level: f32) -> ScanActivity {
        ScanActivity {
            start,
            end: start + 420.0,
            bytes: 1,
            level,
        }
    }

    #[wasm_bindgen_test]
    fn listed_volumes_with_no_container_are_hollow_blocks() {
        let cs = vec![held(840.0, 1260.0, &[840.0])];
        let sz = ScanSizes {
            scans: vec![scan(0.0, 0.1), scan(420.0, 0.1), scan(840.0, 0.1)],
            known_days: vec![0],
        };
        let row = row_vm(&inputs(StripZoom::Cuts, &cs, &sz, 900.0, false));
        assert_eq!(
            row.blocks.iter().map(|b| b.on_device).collect::<Vec<_>>(),
            vec![false, false, true]
        );
    }

    #[wasm_bindgen_test]
    fn three_hours_is_columns_as_tall_as_the_weather_with_a_floor() {
        let cs = vec![held(840.0, 1260.0, &[840.0])];
        let stormy = ScanSizes {
            scans: vec![scan(0.0, 0.1), scan(420.0, 0.9), scan(840.0, 0.1)],
            known_days: vec![0],
        };
        let row = row_vm(&inputs(StripZoom::Volumes, &cs, &stormy, 900.0, false));
        let c = &row.columns;
        assert_eq!(c.len(), 3);
        assert!(c.iter().all(|c| c.x1 - c.x0 <= COLUMN_W_PX));
        assert_eq!(c[0].height, COLUMN_MIN_H);
        assert_eq!(c[1].height, COLUMN_MIN_H + COLUMN_RANGE_H);
        assert!(c[2].on_device && c[2].selected && !c[0].on_device);
        // A quiet day: short even columns.
        let quiet = ScanSizes {
            scans: vec![scan(0.0, 0.11), scan(420.0, 0.12), scan(840.0, 0.1)],
            known_days: vec![0],
        };
        let row = row_vm(&inputs(StripZoom::Volumes, &cs, &quiet, 900.0, false));
        for c in &row.columns {
            assert!(c.height < COLUMN_MIN_H + 0.2 * COLUMN_RANGE_H, "{c:?}");
        }
        assert!(row.blocks.is_empty() && row.hill.is_empty());
    }

    #[wasm_bindgen_test]
    fn hour_labels_sit_on_the_hours_clear_of_the_now_line() {
        let cs = strip();
        let sz = ScanSizes::default();
        let mut i = inputs(StripZoom::Volumes, &cs, &sz, NOW, true);
        i.window = MiniWindow {
            start: -3_600.0,
            end: 10_800.0,
        };
        i.now = 3_650.0;
        let row = row_vm(&i);
        let words: Vec<_> = row.labels.iter().map(|l| l.text.as_str()).collect();
        // 01:00 sits on the now line and gives way.
        assert_eq!(words, vec!["00:00", "02:00"]);
        // In the zone: two hours east.
        i.tz_offset_secs = 7_200;
        let row = row_vm(&i);
        assert_eq!(row.labels[0].text, "02:00");
    }

    #[wasm_bindgen_test]
    fn the_day_hill_draws_the_held_stretch_in_slate_and_the_playhead_white() {
        let cs = vec![
            held(1800.0, 2220.0, &[1800.0]),
            held(2220.0, 2640.0, &[2220.0]),
        ];
        let sz = ScanSizes {
            scans: (0..12).map(|k| scan(k as f64 * 420.0, 0.5)).collect(),
            known_days: vec![0],
        };
        let mut i = inputs(StripZoom::Day, &cs, &sz, 2_000.0, false);
        i.window = MiniWindow {
            start: 0.0,
            end: 5_400.0,
        };
        i.lane = (0.0, 540.0);
        i.now = 5_000.0;
        let row = row_vm(&i);
        let slate: Vec<_> = row.hill.iter().filter(|p| p.on_device).collect();
        assert!(!slate.is_empty());
        let (a, b) = (
            slate.iter().map(|p| p.x0).fold(f32::MAX, f32::min),
            slate.iter().map(|p| p.x1).fold(f32::MIN, f32::max),
        );
        assert_eq!((a, b), (180.0, 264.0));
        assert!(row.hill.iter().all(|p| p.h0 > 0.0 && p.h1 > 0.0));
        assert_eq!(row.playhead_x, Some(200.0));
        // Slivers, left to right, touching and never overlapping.
        for w in row.hill.windows(2) {
            assert!(w[0].x1 <= w[1].x0 + 1e-4);
        }
        assert!(row.hill.iter().all(|p| p.x1 - p.x0 <= HILL_STEP_PX + 1e-4));
    }

    #[wasm_bindgen_test]
    fn the_day_hill_is_a_smooth_curve_through_its_half_hours() {
        let cs = Vec::new();
        // A storm rising and falling over six half-hours.
        let levels = [0.1_f32, 0.3, 0.9, 0.5, 0.2, 0.1];
        let sz = ScanSizes {
            scans: (0..36)
                .map(|k| scan(k as f64 * 300.0, levels[k / 6]))
                .collect(),
            known_days: vec![0],
        };
        let mut i = inputs(StripZoom::Day, &cs, &sz, 10_000.0, false);
        i.window = MiniWindow {
            start: 0.0,
            end: 10_800.0,
        };
        i.lane = (0.0, 360.0);
        i.now = 20_000.0;
        let row = row_vm(&i);
        assert!(row.hill.len() >= 170, "dense: {}", row.hill.len());
        // Continuous: each sliver starts where the last ended.
        for w in row.hill.windows(2) {
            assert!((w[0].x1 - w[1].x0).abs() < 1e-4);
            assert!((w[0].h1 - w[1].h0).abs() < 1e-3, "{:?}", w);
        }
        // Never below the floor, never above the tallest sample.
        let floor = HILL_H * MIN_BAR_FRAC;
        for p in &row.hill {
            for h in [p.h0, p.h1] {
                assert!(h >= floor - 1e-4 && h <= HILL_H + 1e-4, "{h}");
            }
        }
        // No step: neighbours differ by a little, not a bucket's jump.
        let max_jump = row
            .hill
            .iter()
            .map(|p| (p.h1 - p.h0).abs())
            .fold(0.0_f32, f32::max);
        assert!(max_jump < 4.0, "{max_jump}");
        // Peaks at the stormiest half-hour's middle (4_500 s, x = 150).
        let peak = row
            .hill
            .iter()
            .max_by(|a, b| a.h0.total_cmp(&b.h0))
            .unwrap();
        assert!((peak.x0 - 150.0).abs() <= HILL_STEP_PX, "{}", peak.x0);
    }

    #[wasm_bindgen_test]
    fn the_monotone_cubic_never_overshoots_its_knots() {
        let knots = [
            (0.0, 0.0),
            (10.0, 10.0),
            (20.0, 10.0),
            (30.0, 0.0),
            (40.0, 5.0),
        ];
        for k in &knots {
            assert!((monotone_cubic(&knots, k.0) - k.1).abs() < 1e-4);
        }
        let mut x = 0.0;
        while x <= 40.0 {
            let y = monotone_cubic(&knots, x);
            let seg = knots
                .windows(2)
                .find(|w| x >= w[0].0 && x <= w[1].0)
                .unwrap();
            let (lo, hi) = (seg[0].1.min(seg[1].1), seg[0].1.max(seg[1].1));
            assert!(y >= lo - 1e-4 && y <= hi + 1e-4, "{x}: {y}");
            x += 0.25;
        }
        assert_eq!(monotone_cubic(&knots, -5.0), 0.0);
        assert_eq!(monotone_cubic(&knots, 50.0), 5.0);
        assert_eq!(monotone_cubic(&[(3.0, 2.0)], 9.0), 2.0);
    }

    #[wasm_bindgen_test]
    fn the_day_labels_quarter_the_day() {
        let cs = Vec::new();
        let sz = ScanSizes::default();
        let mut i = inputs(StripZoom::Day, &cs, &sz, 0.0, false);
        i.window = MiniWindow {
            start: 0.0,
            end: 86_400.0,
        };
        i.lane = (0.0, 480.0);
        i.now = 200_000.0;
        let row = row_vm(&i);
        let words: Vec<_> = row.labels.iter().map(|l| l.text.as_str()).collect();
        // 00 at the left edge does not fit; nor does the next midnight.
        assert_eq!(words, vec!["06", "12", "18"]);
        assert_eq!(row.now_x, None, "now is off the window");
    }

    #[wasm_bindgen_test]
    fn a_range_is_one_bracket_ending_at_the_now_line_when_pinned() {
        let cs = strip();
        let sz = ScanSizes::default();
        for stop in StripZoom::ALL {
            let mut i = inputs(stop, &cs, &sz, 500.0, true);
            i.range = Some((420.0, NOW));
            let b = row_vm(&i).bracket.unwrap();
            assert_eq!((b.x0, b.x1), (x(420.0), x(NOW)));
            assert!(b.start_tick && b.end_tick);
        }
        // A range whose first frame is into a volume starts at its block.
        let mut i = inputs(StripZoom::Cuts, &cs, &sz, 500.0, true);
        i.range = Some((440.0, NOW));
        assert_eq!(row_vm(&i).bracket.unwrap().x0, x(420.0));
        // A range starting off the lane: no tick at its cut end.
        let mut i = inputs(StripZoom::Cuts, &cs, &sz, 500.0, false);
        i.range = Some((-2_000.0, 600.0));
        let b = row_vm(&i).bracket.unwrap();
        assert_eq!(b.x0, 0.0);
        assert!(!b.start_tick && b.end_tick);
        // No range, no bracket.
        assert!(row_vm(&inputs(StripZoom::Cuts, &cs, &sz, 500.0, false))
            .bracket
            .is_none());
    }
}
