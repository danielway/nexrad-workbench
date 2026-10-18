//! One session, four facts (one-instrument §2).
//!
//! "Live" used to mean seven things because each surface derived its own word
//! from whatever state was nearest to hand. [`SessionVm`] replaces that with
//! four independent facts, built once per frame, and gives every surface
//! exactly the facts it needs:
//!
//! | Fact        | Values                                                         |
//! |-------------|----------------------------------------------------------------|
//! | [`Stream`]  | off · connecting · receiving · waiting · stalled · failed      |
//! | [`Playhead`]| pinned to now · at a time (with its lag behind now)            |
//! | [`SelectionFact`] | a point · a fixed range · a range ending at now          |
//! | [`FrameFact`] | what is on screen: time, tilt, product, age, partial k of N  |
//!
//! The five surfaces are pure projections of those facts, and none of them
//! derives anything of its own:
//!
//! | Surface        | Reads                   | Field / method                    |
//! |----------------|-------------------------|-----------------------------------|
//! | selection chip | playhead + selection (+ stream for the lag) | [`SessionVm::selection_chip`] |
//! | now cap        | selection + stream      | [`SessionVm::now_cap`]            |
//! | age pill       | frame + selection       | [`SessionVm::age_pill`]           |
//! | activity chip  | stream                  | [`SessionVm::activity_chip`]      |
//! | canvas caption | frame + playhead        | [`SessionVm::canvas_caption`]     |
//!
//! The vocabulary (§2 "The vocabulary that follows"):
//!
//! | Fact                               | Word                          | Where |
//! |------------------------------------|-------------------------------|-------|
//! | playhead pinned to now             | `● LIVE`                      | chip, cap |
//! | playhead at a time, stream running | `At 11:30:49 · 5:33 behind`   | chip; cap says `REJOIN` |
//! | playhead at a time, no stream      | `At 11:30:49`                 | chip; cap says `GO LIVE` |
//! | a range loops                      | `⟲ 11:28 → 11:34`             | chip |
//! | a range ends at now                | `⟲ Last 6 frames → now`       | chip; cap says `LIVE` |
//! | stream health                      | `receiving` · `next 0.5° in ~0:40` · `stalled` | activity chip only |
//! | frame freshness                    | `2 min old`                   | age pill, only while pinned |
//!
//! Clock times are formatted by the shell (the zone is a browser fact), so
//! the surfaces that print a clock take a [`Clock`]; everything else is a
//! finished string here.

use crate::core::canvas::{
    caption_line, decide_canvas_caption, CanvasCaption, CaptionContext, CaptionLine,
};
use crate::core::live_status::LiveStatus;
use crate::core::selection_chip::ChipLabel;
use crate::core::transport::LiveStopPlacement;
use crate::core::{
    format_behind_short, format_lag, DisplayedSweep, FrameNow, Intent, LiveExitReason, LivePhase,
    LoopBasis, LoopMode, Selection, StreamActivity,
};
use crate::data::ScanKey;

// ───────────────────────────────────────────────────────────────────────────
// The four facts
// ───────────────────────────────────────────────────────────────────────────

/// What the channel to S3 is doing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Stream {
    /// No stream, and the last one (if any) ended on purpose.
    Off,
    /// Opening the connection / locating the live volume.
    Connecting,
    /// A chunk is arriving.
    Receiving,
    /// Between chunks, within the normal cadence.
    Waiting,
    /// Past the normal cadence with nothing arriving.
    Stalled,
    /// The stream ended on a connection error.
    Failed,
}

impl Stream {
    /// Classify the stream from the live state machine's activity, phase and
    /// last exit. A stream that died on a connection error reads as failed
    /// until the next one starts (which clears the exit reason).
    pub(crate) fn classify(
        activity: StreamActivity,
        phase: LivePhase,
        last_exit: Option<LiveExitReason>,
    ) -> Self {
        if phase == LivePhase::Error
            || (phase == LivePhase::Idle && last_exit == Some(LiveExitReason::ConnectionError))
        {
            return Stream::Failed;
        }
        match activity {
            StreamActivity::Off => Stream::Off,
            StreamActivity::Connecting => Stream::Connecting,
            StreamActivity::Receiving => Stream::Receiving,
            StreamActivity::Waiting => Stream::Waiting,
            StreamActivity::Stalled => Stream::Stalled,
        }
    }

    /// A stream session exists (it may be stalled, but it is still trying).
    pub(crate) fn is_running(self) -> bool {
        matches!(
            self,
            Stream::Connecting | Stream::Receiving | Stream::Waiting | Stream::Stalled
        )
    }

    /// Whether motion is honest: only while data is actually moving.
    pub(crate) fn is_animated(self) -> bool {
        matches!(self, Stream::Connecting | Stream::Receiving)
    }

    /// The headline word for the activity chip while the stream is the only
    /// thing going on. `None` when the stream has nothing healthy to report
    /// (off, stalled or failed are said by the stream line and failure tick).
    pub(crate) fn headline_word(self) -> Option<&'static str> {
        match self {
            Stream::Connecting => Some("Connecting"),
            Stream::Receiving => Some("Receiving"),
            Stream::Waiting => Some("Waiting"),
            Stream::Off | Stream::Stalled | Stream::Failed => None,
        }
    }
}

/// Where the user is.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Playhead {
    /// Riding the live edge.
    PinnedToNow,
    /// At a time; `lag_secs` is how far behind now it sits (never negative).
    AtTime { ts: f64, lag_secs: f64 },
}

/// What plays.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum SelectionFact {
    /// A single moment: the playhead (pinned or not is the [`Playhead`] fact).
    Point,
    /// A range with both ends fixed.
    FixedRange { start: f64, end: f64 },
    /// A range whose later end rides now.
    RangeToNow { basis: LoopBasis },
}

/// What is on screen.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct FrameFact {
    /// Representative (midpoint) collection time, Unix seconds.
    pub time_secs: f64,
    /// Collection end, Unix seconds — what the age is measured from.
    pub end_secs: f64,
    /// Tilt (elevation angle) in degrees.
    pub elevation_deg: f32,
    /// Product name as the sweep identity carries it.
    pub product: String,
    /// Seconds between the frame's end and now (never negative).
    pub age_secs: f64,
    /// `(chunks received, chunks expected)` when the frame is the cut the
    /// stream is still filling.
    pub partial: Option<(u32, u32)>,
}

/// The cut the live stream is filling, as the shell's live model reports it.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct LiveCut {
    pub scan_key: ScanKey,
    pub elevation_number: u8,
    /// Target angle when the VCP is known.
    pub elevation_angle: Option<f32>,
    pub chunks_received: u32,
    pub chunks_expected: Option<u32>,
}

// ───────────────────────────────────────────────────────────────────────────
// Surfaces
// ───────────────────────────────────────────────────────────────────────────

/// The cap on the timeline's now line.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum NowCap {
    /// The selection is pinned to now (a live point or a range ending at now).
    Live,
    /// A stream runs while the selection sits elsewhere.
    Rejoin,
    /// No stream.
    GoLive,
}

impl NowCap {
    pub(crate) fn label(self) -> &'static str {
        match self {
            NowCap::Live => "LIVE",
            NowCap::Rejoin => "REJOIN",
            NowCap::GoLive => "GO LIVE",
        }
    }
}

/// What the age pill marks the frame with.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum AgeMarker {
    /// Pinned point: the red dot.
    Pinned,
    /// A range ending at now: the loop mark.
    Loop,
}

/// The frame-freshness pill beside the top bar's frame time.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct AgePill {
    pub marker: AgeMarker,
    /// "2 min old".
    pub text: String,
}

/// Clock formatting in the user's chosen zone. Implemented by the shell,
/// which owns the zone (a browser fact).
pub(crate) trait Clock {
    /// `11:30:49`
    fn hms(&self, ts: f64) -> String;
    /// `11:30`
    fn hm(&self, ts: f64) -> String;
    /// `(year, month, day)` in the zone, for day-crossing ranges.
    fn day(&self, ts: f64) -> (i32, u32, u32);
}

/// The glyphs the chip prints. The design writes them as `●`, `⟲` and `→`;
/// the egui shell substitutes icon-font glyphs because its text font lacks
/// them.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ChipGlyphs {
    pub live: &'static str,
    pub loop_mark: &'static str,
    pub arrow: &'static str,
}

/// The design's own glyphs (the tests pin the table's words with them).
#[cfg(test)]
pub(crate) const DESIGN_GLYPHS: ChipGlyphs = ChipGlyphs {
    live: "●",
    loop_mark: "⟲",
    arrow: "→",
};

// ───────────────────────────────────────────────────────────────────────────
// The view-model
// ───────────────────────────────────────────────────────────────────────────

/// Everything [`SessionVm::build`] reads: plain data from existing state.
pub(crate) struct SessionInputs<'a> {
    pub now: FrameNow,
    /// This frame's stream measurements (activity, countdown, expected cut).
    pub live_status: &'a LiveStatus,
    pub live_phase: LivePhase,
    pub last_exit: Option<LiveExitReason>,
    pub selection: Selection,
    /// How a range loops (the chip menu's loop style).
    pub loop_mode: LoopMode,
    pub playback_position_secs: f64,
    /// What is on the GPU.
    pub displayed: Option<&'a DisplayedSweep>,
    /// The cut the live stream is filling, if any.
    pub live_cut: Option<LiveCut>,
    /// What the render loop knows about the frame and the playhead (one
    /// frame's length, a render in flight, downloads, the wanted tilt) — the
    /// caption rule reads it beside the frame and playhead facts.
    pub caption_context: CaptionContext,
}

/// One session, four facts, five surfaces. Rebuilt once per frame.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct SessionVm {
    // The four facts.
    pub stream: Stream,
    pub playhead: Playhead,
    pub selection: SelectionFact,
    pub frame: Option<FrameFact>,

    // The surfaces.
    /// What plays. Clock text via [`SessionVm::selection_chip_text`].
    pub selection_chip: ChipLabel,
    /// The loop style riding on the chip: `Some` only for a range that does
    /// not plainly Loop.
    pub loop_style: Option<LoopMode>,
    pub now_cap: NowCap,
    /// `None` while the selection is not pinned, or with no frame.
    pub age_pill: Option<AgePill>,
    /// The stream-health line. `None` with no stream to report.
    pub activity_chip: Option<String>,
    /// Hover sentence for the stream-health line.
    pub activity_hover: Option<&'static str>,
    /// The canvas caption, decided by the one caption rule
    /// ([`decide_canvas_caption`]). Words via
    /// [`SessionVm::canvas_caption_line`].
    pub canvas_caption: CanvasCaption,

    /// A stream runs and the selection is pinned to now: what the favicon,
    /// the tab title and the link's pinned selection read.
    pub live_now: bool,
}

impl Default for SessionVm {
    fn default() -> Self {
        Self {
            stream: Stream::Off,
            playhead: Playhead::AtTime {
                ts: 0.0,
                lag_secs: 0.0,
            },
            selection: SelectionFact::Point,
            frame: None,
            selection_chip: ChipLabel::At {
                ts: 0.0,
                behind_secs: None,
            },
            loop_style: None,
            now_cap: NowCap::GoLive,
            age_pill: None,
            activity_chip: None,
            activity_hover: None,
            canvas_caption: CanvasCaption::None,
            live_now: false,
        }
    }
}

impl SessionVm {
    pub(crate) fn build(inputs: SessionInputs<'_>) -> Self {
        let now = inputs.now.secs();
        let status = inputs.live_status;
        let stream = Stream::classify(status.activity, inputs.live_phase, inputs.last_exit);

        let pinned_point = matches!(inputs.selection, Selection::Point { live: true });
        let playhead = if pinned_point {
            Playhead::PinnedToNow
        } else {
            Playhead::AtTime {
                ts: inputs.playback_position_secs,
                lag_secs: (now - inputs.playback_position_secs).max(0.0),
            }
        };
        let selection = match inputs.selection {
            Selection::Point { .. } => SelectionFact::Point,
            Selection::Range(r) if r.live => SelectionFact::RangeToNow { basis: r.basis },
            Selection::Range(r) => match r.bounds {
                Some((start, end)) => SelectionFact::FixedRange { start, end },
                // A fixed range always has bounds; read it as its point.
                None => SelectionFact::Point,
            },
        };

        let frame = inputs.displayed.map(|d| FrameFact {
            time_secs: (d.start_time + d.end_time) / 2.0,
            end_secs: d.end_time,
            elevation_deg: d.elevation_deg,
            product: d.identity.product.clone(),
            age_secs: (now - d.end_time).max(0.0),
            partial: inputs.live_cut.as_ref().and_then(|cut| {
                (cut.scan_key == d.identity.scan_key
                    && cut.elevation_number == d.identity.elevation_number)
                    .then_some(())
                    .and(cut.chunks_expected)
                    .map(|n| (cut.chunks_received, n))
            }),
        });

        let selection_pinned =
            pinned_point || matches!(selection, SelectionFact::RangeToNow { .. });

        let selection_chip = chip_label(selection, playhead, stream);
        let loop_style = (!matches!(selection, SelectionFact::Point)
            && inputs.loop_mode != LoopMode::Loop)
            .then_some(inputs.loop_mode);
        let now_cap = if selection_pinned {
            NowCap::Live
        } else if stream.is_running() {
            NowCap::Rejoin
        } else {
            NowCap::GoLive
        };
        let age_pill = match (&frame, selection_pinned) {
            (Some(f), true) => Some(AgePill {
                marker: if pinned_point {
                    AgeMarker::Pinned
                } else {
                    AgeMarker::Loop
                },
                text: format_age(f.age_secs),
            }),
            _ => None,
        };
        let canvas_caption = decide_canvas_caption(
            frame.as_ref(),
            playhead,
            now,
            stream.is_running(),
            inputs.live_cut.as_ref().and_then(|c| c.elevation_angle),
            &inputs.caption_context,
        );
        let activity_chip = stream_line(stream, status, inputs.live_cut.as_ref(), now);
        let activity_hover = stream_hover(stream, status);
        let live_now = stream.is_running() && selection_pinned;

        Self {
            stream,
            playhead,
            selection,
            frame,
            selection_chip,
            loop_style,
            now_cap,
            age_pill,
            activity_chip,
            activity_hover,
            canvas_caption,
            live_now,
        }
    }

    /// What clicking the now cap does: stop a pinned stream, rejoin a
    /// background one, or start one.
    pub(crate) fn now_cap_intent(&self) -> Intent {
        match self.now_cap {
            NowCap::Live if self.stream.is_running() => {
                Intent::StopLive(LiveStopPlacement::LiveEdge)
            }
            // Instant re-pin — the stream never stopped.
            NowCap::Rejoin => Intent::ReturnToLive,
            NowCap::Live | NowCap::GoLive => Intent::GoLive,
        }
    }

    /// What the off-screen "jump to now" chip does after centering on now:
    /// attach to the stream (rejoin or start), or nothing when already live.
    pub(crate) fn now_edge_chip_intent(&self) -> Option<Intent> {
        match self.now_cap {
            NowCap::Rejoin => Some(Intent::ReturnToLive),
            NowCap::GoLive => Some(Intent::GoLive),
            NowCap::Live if self.stream.is_running() => None,
            NowCap::Live => Some(Intent::GoLive),
        }
    }

    /// The selection chip's words. `compact` (the phone bar) is as short
    /// as the words still read: a point drops its seconds and keeps the lag
    /// in short words (`11:30 · 5:33 behind`, `15:12 · 20 h behind`), so a
    /// detached phone still says how far behind it is, and a range to now
    /// drops "Last" (`⟲ 4 frames → now`, `⟲ 30 min → now`). A loop style
    /// other than Loop rides on the end: `⟲ 11:28 → 11:34 · Ping-pong`.
    pub(crate) fn selection_chip_text(
        &self,
        clock: &dyn Clock,
        glyphs: &ChipGlyphs,
        compact: bool,
    ) -> String {
        let text = chip_text(&self.selection_chip, clock, glyphs, compact);
        match self.loop_style {
            Some(style) => format!("{text} · {}", style.label()),
            None => text,
        }
    }

    /// The canvas caption, segment by segment (§7 forms), or `None` when
    /// the frame and the playhead agree.
    pub(crate) fn canvas_caption_line(&self, clock: &dyn Clock) -> Option<CaptionLine> {
        caption_line(
            &self.canvas_caption,
            self.playhead == Playhead::PinnedToNow,
            clock,
        )
    }

    /// The canvas caption's words as one string.
    #[cfg(test)]
    pub(crate) fn canvas_caption_text(&self, clock: &dyn Clock) -> Option<String> {
        self.canvas_caption_line(clock).map(|l| l.text())
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Projections
// ───────────────────────────────────────────────────────────────────────────

fn chip_label(selection: SelectionFact, playhead: Playhead, stream: Stream) -> ChipLabel {
    match (selection, playhead) {
        (SelectionFact::RangeToNow { basis }, _) => ChipLabel::RangeToNow { basis },
        (SelectionFact::FixedRange { start, end }, _) => ChipLabel::Range { start, end },
        (SelectionFact::Point, Playhead::PinnedToNow) => ChipLabel::Live,
        (SelectionFact::Point, Playhead::AtTime { ts, lag_secs }) => ChipLabel::At {
            ts,
            // The lag is only news while a stream keeps now moving away.
            behind_secs: stream.is_running().then_some(lag_secs),
        },
    }
}

fn chip_text(label: &ChipLabel, clock: &dyn Clock, g: &ChipGlyphs, compact: bool) -> String {
    match *label {
        ChipLabel::Live => format!("{} LIVE", g.live),
        ChipLabel::At {
            ts,
            behind_secs: Some(lag),
        } if compact => format!("{} · {} behind", clock.hm(ts), format_behind_short(lag)),
        ChipLabel::At { ts, .. } if compact => format!("At {}", clock.hm(ts)),
        ChipLabel::At {
            ts,
            behind_secs: Some(lag),
        } => format!("At {} · {} behind", clock.hms(ts), format_lag(lag)),
        ChipLabel::At {
            ts,
            behind_secs: None,
        } => format!("At {}", clock.hms(ts)),
        ChipLabel::Range { start, end } => {
            let (a, b) = (clock.day(start), clock.day(end));
            if a == b {
                format!(
                    "{} {} {} {}",
                    g.loop_mark,
                    clock.hm(start),
                    g.arrow,
                    clock.hm(end)
                )
            } else {
                format!(
                    "{} {}/{} {} {} {}/{} {}",
                    g.loop_mark,
                    a.1,
                    a.2,
                    clock.hm(start),
                    g.arrow,
                    b.1,
                    b.2,
                    clock.hm(end)
                )
            }
        }
        // The phone bar drops "Last": the count and "→ now" already say
        // it is the newest frames. The unit stays, since a bare "4 → now"
        // could be minutes as well as frames.
        ChipLabel::RangeToNow { basis } if compact => {
            format!("{} {} {} now", g.loop_mark, basis.label(), g.arrow)
        }
        ChipLabel::RangeToNow { basis } => {
            format!("{} Last {} {} now", g.loop_mark, basis.label(), g.arrow)
        }
    }
}

/// Frame age in one unit, rounded (§4 "age"): `28 s old`, `2 min old`,
/// `3 h old`, `4 d old`, `13 y old`.
pub(crate) fn format_age(age_secs: f64) -> String {
    let secs = age_secs.max(0.0);
    let s = secs.round();
    if s < 60.0 {
        return format!("{s:.0} s old");
    }
    let m = (secs / 60.0).round();
    if m < 60.0 {
        return format!("{m:.0} min old");
    }
    let h = (secs / 3600.0).round();
    if h < 24.0 {
        return format!("{h:.0} h old");
    }
    let d = (secs / 86_400.0).round();
    if d < 365.0 {
        return format!("{d:.0} d old");
    }
    format!("{:.0} y old", (secs / (365.25 * 86_400.0)).round().max(1.0))
}

/// A projected countdown: `~4:46`.
fn countdown(secs: f64) -> String {
    format!("~{}", format_lag(secs.max(0.0).ceil()))
}

/// The stream-health words for the activity chip.
fn stream_line(
    stream: Stream,
    status: &LiveStatus,
    cut: Option<&LiveCut>,
    now: f64,
) -> Option<String> {
    match stream {
        Stream::Off => None,
        Stream::Connecting => Some(format!(
            "connecting… {}s",
            status.phase_elapsed_secs.max(0.0) as i64
        )),
        Stream::Receiving => Some(match cut {
            Some(cut) => {
                let mut s = "receiving".to_string();
                if let Some(angle) = cut.elevation_angle {
                    s.push_str(&format!(" {angle:.1}°"));
                }
                if let Some(n) = cut.chunks_expected {
                    s.push_str(&format!(" · {} of {n}", cut.chunks_received.min(n)));
                }
                s
            }
            None => "receiving".to_string(),
        }),
        Stream::Waiting => Some(match (status.expected_sweep, status.countdown_secs) {
            (Some(e), Some(s)) => format!("next {} in {}", e.label(), countdown(s)),
            (None, Some(s)) => format!("next sweep in {}", countdown(s)),
            (Some(e), None) => format!("waiting for {}", e.label()),
            (None, None) => "waiting for next sweep".to_string(),
        }),
        Stream::Stalled => Some(match status.expected_sweep {
            Some(e) => format!(
                "stalled · {} is {} late",
                e.label(),
                format_lag((now - e.available_at_secs).max(0.0))
            ),
            None => format!(
                "stalled · no data for {}",
                format_lag(status.phase_elapsed_secs)
            ),
        }),
        Stream::Failed => Some("stream failed".to_string()),
    }
}

fn stream_hover(stream: Stream, status: &LiveStatus) -> Option<&'static str> {
    match stream {
        Stream::Off => None,
        Stream::Connecting => Some("Connecting to the live feed"),
        Stream::Receiving => Some("Receiving live data now"),
        Stream::Waiting => Some("Waiting for the next expected sweep"),
        Stream::Stalled => Some(if status.expected_sweep.is_some() {
            "Expected sweep is late — change elevation or stop streaming"
        } else {
            "No data has arrived for a while"
        }),
        Stream::Failed => Some("The live stream lost its connection — GO LIVE to try again"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::canvas::CaptionRole;
    use crate::core::domain::playback::RangeSelection;
    use crate::core::{ExpectedSweep, SweepIdentity};
    use crate::data::UnixMillis;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// 2025-10-15 00:00:00 UTC.
    const DAY: f64 = 1_760_486_400.0;

    fn t(h: u32, m: u32, s: u32) -> f64 {
        DAY + (h * 3600 + m * 60 + s) as f64
    }

    /// UTC clock with plain arithmetic, so the tests read the design's
    /// own times.
    struct Utc;

    impl Clock for Utc {
        fn hms(&self, ts: f64) -> String {
            let s = (ts.floor() as i64).rem_euclid(86_400);
            format!("{:02}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
        }
        fn hm(&self, ts: f64) -> String {
            let s = (ts.floor() as i64).rem_euclid(86_400);
            format!("{:02}:{:02}", s / 3600, (s / 60) % 60)
        }
        fn day(&self, ts: f64) -> (i32, u32, u32) {
            // Days since the test epoch, enough to tell days apart.
            let d = ((ts - DAY) / 86_400.0).floor() as i64;
            (2025, 10, (15 + d) as u32)
        }
    }

    fn key() -> ScanKey {
        ScanKey::new("KTLX", UnixMillis(1_760_520_000_000))
    }

    /// A frame on screen whose collection ends at `end`, 20 s long.
    fn frame(end: f64, elevation_number: u8, elevation_deg: f32) -> DisplayedSweep {
        DisplayedSweep {
            identity: SweepIdentity::new(key(), elevation_number, "reflectivity"),
            start_time: end - 20.0,
            end_time: end,
            elevation_deg,
        }
    }

    fn expected(angle: f32, available_at_secs: f64) -> ExpectedSweep {
        ExpectedSweep {
            elevation_number: Some(1),
            elevation_angle: Some(angle),
            available_at_secs,
        }
    }

    fn waiting(countdown: f64, exp: Option<ExpectedSweep>) -> LiveStatus {
        LiveStatus {
            activity: StreamActivity::Waiting,
            countdown_secs: Some(countdown),
            expected_sweep: exp,
            ..LiveStatus::default()
        }
    }

    fn receiving() -> LiveStatus {
        LiveStatus {
            activity: StreamActivity::Receiving,
            ..LiveStatus::default()
        }
    }

    fn cut(elevation_number: u8, angle: f32, k: u32, n: u32) -> LiveCut {
        LiveCut {
            scan_key: key(),
            elevation_number,
            elevation_angle: Some(angle),
            chunks_received: k,
            chunks_expected: Some(n),
        }
    }

    const PINNED: Selection = Selection::Point { live: true };
    const POINT: Selection = Selection::Point { live: false };

    fn last_6_frames(bounds: (f64, f64)) -> Selection {
        Selection::Range(RangeSelection {
            bounds: Some(bounds),
            live: true,
            basis: LoopBasis::FrameCount(6),
        })
    }

    fn fixed_range(start: f64, end: f64) -> Selection {
        Selection::Range(RangeSelection {
            bounds: Some((start, end)),
            live: false,
            basis: LoopBasis::Duration(end - start),
        })
    }

    /// A scenario: everything off, the playhead at `now`.
    struct S<'a> {
        now: f64,
        status: LiveStatus,
        phase: LivePhase,
        last_exit: Option<LiveExitReason>,
        selection: Selection,
        loop_mode: LoopMode,
        playhead: f64,
        displayed: Option<&'a DisplayedSweep>,
        live_cut: Option<LiveCut>,
        caption: CaptionContext,
    }

    impl<'a> S<'a> {
        fn at(now: f64) -> Self {
            Self {
                now,
                status: LiveStatus::default(),
                phase: LivePhase::Idle,
                last_exit: None,
                selection: POINT,
                loop_mode: LoopMode::Loop,
                playhead: now,
                displayed: None,
                live_cut: None,
                caption: CaptionContext::default(),
            }
        }

        fn streaming(mut self, status: LiveStatus) -> Self {
            self.phase = match status.activity {
                StreamActivity::Off => LivePhase::Idle,
                StreamActivity::Connecting => LivePhase::AcquiringLock,
                StreamActivity::Receiving => LivePhase::Streaming,
                StreamActivity::Waiting | StreamActivity::Stalled => LivePhase::WaitingForChunk,
            };
            self.status = status;
            self
        }

        fn vm(&self) -> SessionVm {
            SessionVm::build(SessionInputs {
                now: FrameNow(self.now),
                live_status: &self.status,
                live_phase: self.phase,
                last_exit: self.last_exit,
                selection: self.selection,
                loop_mode: self.loop_mode,
                playback_position_secs: self.playhead,
                displayed: self.displayed,
                live_cut: self.live_cut.clone(),
                caption_context: self.caption,
            })
        }
    }

    fn chip(vm: &SessionVm) -> String {
        vm.selection_chip_text(&Utc, &DESIGN_GLYPHS, false)
    }

    fn pill(vm: &SessionVm) -> Option<String> {
        vm.age_pill.as_ref().map(|p| p.text.clone())
    }

    // ── The vocabulary table (§2), row by row ──────────────────────────────

    #[wasm_bindgen_test]
    fn row_pinned_to_now_reads_live_on_chip_and_cap_and_dots_the_pill() {
        let f = frame(t(11, 22, 0), 1, 0.5);
        let mut s = S::at(t(11, 24, 0)).streaming(receiving());
        s.selection = PINNED;
        s.displayed = Some(&f);
        let vm = s.vm();
        assert_eq!(vm.playhead, Playhead::PinnedToNow);
        assert_eq!(chip(&vm), "● LIVE");
        assert_eq!(vm.now_cap.label(), "LIVE");
        assert_eq!(vm.age_pill.as_ref().unwrap().marker, AgeMarker::Pinned);
    }

    /// "Never means that a stream is running, that the frame is fresh": a
    /// pinned chip stays LIVE through a stall, and the pill still counts.
    #[wasm_bindgen_test]
    fn row_pinned_to_now_does_not_mean_the_stream_is_healthy() {
        let f = frame(t(11, 0, 0), 1, 0.5);
        let mut s = S::at(t(11, 30, 0)).streaming(LiveStatus {
            activity: StreamActivity::Stalled,
            expected_sweep: Some(expected(2.4, t(11, 29, 18))),
            ..LiveStatus::default()
        });
        s.selection = PINNED;
        s.displayed = Some(&f);
        let vm = s.vm();
        assert_eq!(chip(&vm), "● LIVE");
        assert_eq!(vm.now_cap, NowCap::Live);
        assert_eq!(pill(&vm).as_deref(), Some("30 min old"));
        assert_eq!(
            vm.activity_chip.as_deref(),
            Some("stalled · 2.4° is 0:42 late")
        );
    }

    #[wasm_bindgen_test]
    fn row_at_a_time_with_a_stream_reads_lag_and_rejoin() {
        let mut s = S::at(t(11, 36, 22)).streaming(waiting(40.0, None));
        s.playhead = t(11, 30, 49);
        let vm = s.vm();
        assert_eq!(chip(&vm), "At 11:30:49 · 5:33 behind");
        assert_eq!(vm.now_cap.label(), "REJOIN");
        assert_eq!(
            vm.playhead,
            Playhead::AtTime {
                ts: t(11, 30, 49),
                lag_secs: 333.0
            }
        );
    }

    /// "Never means IDLE": no stream is a time with or without a frame.
    #[wasm_bindgen_test]
    fn row_at_a_time_without_a_stream_reads_the_time_and_go_live() {
        let mut s = S::at(t(12, 0, 0));
        s.playhead = t(11, 30, 49);
        let vm = s.vm();
        assert_eq!(chip(&vm), "At 11:30:49");
        assert_eq!(vm.now_cap.label(), "GO LIVE");
        assert_eq!(vm.activity_chip, None);
        assert_eq!(vm.age_pill, None);
    }

    /// The table leaves the cap of a fixed range unstated; it follows the
    /// stream like any selection that is not pinned.
    #[wasm_bindgen_test]
    fn row_a_range_loops_reads_its_bounds_and_cap_follows_the_stream() {
        let mut s = S::at(t(12, 0, 0));
        s.selection = fixed_range(t(11, 28, 0), t(11, 34, 0));
        s.playhead = t(11, 30, 0);
        let vm = s.vm();
        assert_eq!(chip(&vm), "⟲ 11:28 → 11:34");
        assert_eq!(vm.now_cap, NowCap::GoLive);
        assert_eq!(vm.age_pill, None);

        let mut s = S::at(t(12, 0, 0)).streaming(receiving());
        s.selection = fixed_range(t(11, 28, 0), t(11, 34, 0));
        assert_eq!(s.vm().now_cap, NowCap::Rejoin);
    }

    /// "Never means LIVE about the frame": the frame may be 26 min old and
    /// the pill says so, with the loop mark.
    #[wasm_bindgen_test]
    fn row_a_range_ending_at_now_reads_last_frames_and_keeps_the_cap_live() {
        let f = frame(t(11, 1, 24), 1, 0.5);
        let mut s = S::at(t(11, 27, 24)).streaming(waiting(65.0, None));
        s.selection = last_6_frames((t(11, 1, 0), t(11, 27, 24)));
        s.playhead = t(11, 1, 14);
        s.displayed = Some(&f);
        let vm = s.vm();
        assert_eq!(chip(&vm), "⟲ Last 6 frames → now");
        assert_eq!(vm.now_cap, NowCap::Live);
        let p = vm.age_pill.unwrap();
        assert_eq!(p.marker, AgeMarker::Loop);
        assert_eq!(p.text, "26 min old");
    }

    #[wasm_bindgen_test]
    fn row_stream_health_words_receiving_next_and_stalled() {
        let now = t(11, 45, 0);
        let mut s = S::at(now).streaming(receiving());
        assert_eq!(s.vm().activity_chip.as_deref(), Some("receiving"));
        s.live_cut = Some(cut(3, 2.4, 3, 6));
        assert_eq!(
            s.vm().activity_chip.as_deref(),
            Some("receiving 2.4° · 3 of 6")
        );

        let s = S::at(now).streaming(waiting(39.2, Some(expected(0.5, now + 40.0))));
        assert_eq!(s.vm().activity_chip.as_deref(), Some("next 0.5° in ~0:40"));

        let s = S::at(now).streaming(LiveStatus {
            activity: StreamActivity::Stalled,
            expected_sweep: Some(expected(2.4, now - 42.0)),
            ..LiveStatus::default()
        });
        assert_eq!(
            s.vm().activity_chip.as_deref(),
            Some("stalled · 2.4° is 0:42 late")
        );
    }

    /// "Never means anything about where the playhead is": the same stream
    /// prints the same line pinned, detached or looping.
    #[wasm_bindgen_test]
    fn row_stream_health_is_independent_of_the_playhead() {
        let now = t(11, 45, 0);
        let lines: Vec<_> = [PINNED, POINT, last_6_frames((now - 600.0, now))]
            .into_iter()
            .map(|selection| {
                let mut s = S::at(now).streaming(waiting(40.0, Some(expected(0.5, now + 40.0))));
                s.selection = selection;
                s.playhead = now - 300.0;
                s.vm().activity_chip
            })
            .collect();
        assert!(lines.iter().all(|l| l == &lines[0]));
    }

    /// "Only while a selection is pinned": at a time, the timestamp carries
    /// the meaning and the pill is gone, however old the frame.
    #[wasm_bindgen_test]
    fn row_frame_freshness_only_while_pinned() {
        let f = frame(t(11, 22, 0), 1, 0.5);
        let mut s = S::at(t(11, 24, 0)).streaming(receiving());
        s.displayed = Some(&f);
        s.selection = PINNED;
        assert_eq!(pill(&s.vm()).as_deref(), Some("2 min old"));
        s.selection = POINT;
        s.playhead = t(11, 22, 0);
        assert_eq!(pill(&s.vm()), None);
        assert_eq!(s.vm().frame.unwrap().age_secs, 120.0);
    }

    #[wasm_bindgen_test]
    fn age_is_one_unit_rounded() {
        assert_eq!(format_age(0.0), "0 s old");
        assert_eq!(format_age(28.4), "28 s old");
        assert_eq!(format_age(59.4), "59 s old");
        assert_eq!(format_age(59.6), "1 min old");
        assert_eq!(format_age(120.0), "2 min old");
        assert_eq!(format_age(26.0 * 60.0), "26 min old");
        assert_eq!(format_age(3.0 * 3600.0), "3 h old");
        assert_eq!(format_age(3.0 * 86_400.0), "3 d old");
        assert_eq!(format_age(13.0 * 365.25 * 86_400.0), "13 y old");
        // Clock skew never reads as negative.
        assert_eq!(format_age(-5.0), "0 s old");
    }

    // ── The five moments (§2), every surface at once ───────────────────────

    /// A · Pinned, Frames, 0.5°, waiting (inventory 05).
    #[wasm_bindgen_test]
    fn moment_a_pinned_frames_waiting() {
        let f = frame(t(11, 22, 7), 1, 0.5);
        let now = t(11, 24, 7);
        let mut s = S::at(now).streaming(waiting(300.0, Some(expected(0.5, now + 300.0))));
        s.selection = PINNED;
        s.displayed = Some(&f);
        let vm = s.vm();
        assert_eq!(Utc.hms(vm.frame.as_ref().unwrap().time_secs), "11:21:57");
        assert_eq!(pill(&vm).as_deref(), Some("2 min old"));
        assert_eq!(chip(&vm), "● LIVE");
        assert_eq!(vm.now_cap.label(), "LIVE");
        assert_eq!(vm.activity_chip.as_deref(), Some("next 0.5° in ~5:00"));
        assert_eq!(vm.canvas_caption_text(&Utc), None);
        assert!(vm.live_now);
    }

    /// B · Pinned, Rays, Latest (inventory 59): the frame is the cut being
    /// filled, so it carries partial progress.
    #[wasm_bindgen_test]
    fn moment_b_pinned_rays_receiving_a_partial_cut() {
        let f = frame(t(11, 45, 48), 3, 2.4);
        let now = t(11, 46, 5);
        let mut s = S::at(now).streaming(receiving());
        s.selection = PINNED;
        s.displayed = Some(&f);
        s.live_cut = Some(cut(3, 2.4, 3, 6));
        let vm = s.vm();
        let fr = vm.frame.as_ref().unwrap();
        assert_eq!(Utc.hms(fr.time_secs), "11:45:38");
        assert_eq!(fr.partial, Some((3, 6)));
        assert_eq!(pill(&vm).as_deref(), Some("17 s old"));
        assert_eq!(chip(&vm), "● LIVE");
        assert_eq!(vm.now_cap.label(), "LIVE");
        assert_eq!(vm.activity_chip.as_deref(), Some("receiving 2.4° · 3 of 6"));
        assert_eq!(vm.canvas_caption_text(&Utc), None);
    }

    /// C · Freeze (inventory 60): a detach, nothing more. The stream keeps
    /// receiving and the cap offers REJOIN.
    #[wasm_bindgen_test]
    fn moment_c_freeze_is_a_detach() {
        let f = frame(t(11, 46, 3), 4, 3.1);
        let mut s = S::at(t(11, 46, 17)).streaming(receiving());
        s.playhead = t(11, 46, 4);
        s.displayed = Some(&f);
        s.live_cut = Some(cut(4, 3.1, 1, 6));
        let vm = s.vm();
        assert_eq!(Utc.hms(vm.frame.as_ref().unwrap().time_secs), "11:45:53");
        assert_eq!(pill(&vm), None);
        assert_eq!(chip(&vm), "At 11:46:04 · 0:13 behind");
        assert_eq!(vm.now_cap.label(), "REJOIN");
        assert_eq!(vm.activity_chip.as_deref(), Some("receiving 3.1° · 1 of 6"));
        assert_eq!(vm.canvas_caption_text(&Utc), None);
        assert!(!vm.live_now);
    }

    /// D · Detached into a gap (inventory 36): the caption names both clocks
    /// and what is coming, in the §7 form.
    #[wasm_bindgen_test]
    fn moment_d_detached_into_a_gap_captions_both_clocks() {
        let f = frame(t(11, 29, 1), 1, 0.5);
        let mut s = S::at(t(11, 36, 22)).streaming(waiting(120.0, None));
        s.playhead = t(11, 30, 49);
        s.displayed = Some(&f);
        // SAILS revisits 0.5° every ~90 s; a download covers the playhead.
        s.caption = CaptionContext {
            frame_step_secs: 90.0,
            fetch_covers_playhead: true,
            wanted_tilt_deg: Some(0.5),
            ..CaptionContext::default()
        };
        let vm = s.vm();
        assert_eq!(Utc.hms(vm.frame.as_ref().unwrap().time_secs), "11:28:51");
        assert_eq!(pill(&vm), None);
        assert_eq!(chip(&vm), "At 11:30:49 · 5:33 behind");
        assert_eq!(vm.now_cap.label(), "REJOIN");
        assert_eq!(
            vm.canvas_caption_text(&Utc).as_deref(),
            Some("showing 11:28:51 · playhead 11:30:49 · 0.5° at 11:30 is downloading")
        );
    }

    /// E · Last 6 frames → now (inventory 16).
    #[wasm_bindgen_test]
    fn moment_e_last_six_frames_to_now() {
        let f = frame(t(11, 1, 24), 1, 0.5);
        let now = t(11, 27, 24);
        let mut s = S::at(now).streaming(waiting(65.0, Some(expected(0.5, now + 65.0))));
        s.selection = last_6_frames((t(10, 57, 0), now));
        s.playhead = t(11, 1, 14);
        s.displayed = Some(&f);
        let vm = s.vm();
        assert_eq!(Utc.hms(vm.frame.as_ref().unwrap().time_secs), "11:01:14");
        assert_eq!(pill(&vm).as_deref(), Some("26 min old"));
        assert_eq!(vm.age_pill.as_ref().unwrap().marker, AgeMarker::Loop);
        assert_eq!(chip(&vm), "⟲ Last 6 frames → now");
        assert_eq!(vm.now_cap.label(), "LIVE");
        assert_eq!(vm.activity_chip.as_deref(), Some("next 0.5° in ~1:05"));
        assert_eq!(vm.canvas_caption_text(&Utc), None);
        assert!(vm.live_now);
    }

    // ── Facts and edges ────────────────────────────────────────────────────

    #[wasm_bindgen_test]
    fn stream_fails_on_a_connection_error_until_the_next_start() {
        let mut s = S::at(t(12, 0, 0));
        s.last_exit = Some(LiveExitReason::ConnectionError);
        let vm = s.vm();
        assert_eq!(vm.stream, Stream::Failed);
        assert!(!vm.stream.is_running());
        assert_eq!(vm.activity_chip.as_deref(), Some("stream failed"));
        assert_eq!(vm.now_cap, NowCap::GoLive);

        // The Error phase reads as failed, not stalled.
        s.last_exit = None;
        s.phase = LivePhase::Error;
        s.status.activity = StreamActivity::Stalled;
        assert_eq!(s.vm().stream, Stream::Failed);

        // A user stop is just off.
        let mut s = S::at(t(12, 0, 0));
        s.last_exit = Some(LiveExitReason::UserStopped);
        assert_eq!(s.vm().stream, Stream::Off);
        assert_eq!(s.vm().activity_chip, None);
    }

    #[wasm_bindgen_test]
    fn connecting_and_waiting_fallback_words() {
        let now = t(12, 0, 0);
        let s = S::at(now).streaming(LiveStatus {
            activity: StreamActivity::Connecting,
            phase_elapsed_secs: 5.9,
            ..LiveStatus::default()
        });
        assert_eq!(s.vm().activity_chip.as_deref(), Some("connecting… 5s"));

        let s = S::at(now).streaming(LiveStatus {
            activity: StreamActivity::Waiting,
            ..LiveStatus::default()
        });
        assert_eq!(
            s.vm().activity_chip.as_deref(),
            Some("waiting for next sweep")
        );
        let s = S::at(now).streaming(LiveStatus {
            activity: StreamActivity::Waiting,
            expected_sweep: Some(expected(2.4, now + 10.0)),
            ..LiveStatus::default()
        });
        assert_eq!(s.vm().activity_chip.as_deref(), Some("waiting for 2.4°"));
        let s = S::at(now).streaming(waiting(-3.0, None));
        assert_eq!(s.vm().activity_chip.as_deref(), Some("next sweep in ~0:00"));

        // A connect that never completes stalls with the silence's length.
        let s = S::at(now).streaming(LiveStatus {
            activity: StreamActivity::Stalled,
            phase_elapsed_secs: 120.0,
            ..LiveStatus::default()
        });
        assert_eq!(
            s.vm().activity_chip.as_deref(),
            Some("stalled · no data for 2:00")
        );
    }

    #[wasm_bindgen_test]
    fn only_moving_data_animates() {
        assert!(Stream::Receiving.is_animated());
        assert!(Stream::Connecting.is_animated());
        for s in [
            Stream::Off,
            Stream::Waiting,
            Stream::Stalled,
            Stream::Failed,
        ] {
            assert!(!s.is_animated(), "{s:?}");
        }
    }

    #[wasm_bindgen_test]
    fn partial_only_when_the_frame_is_the_cut_being_filled() {
        let f = frame(t(11, 45, 48), 2, 1.5);
        let mut s = S::at(t(11, 46, 0)).streaming(receiving());
        s.displayed = Some(&f);
        s.live_cut = Some(cut(3, 2.4, 3, 6));
        assert_eq!(s.vm().frame.unwrap().partial, None);
    }

    #[wasm_bindgen_test]
    fn compact_chip_drops_seconds_and_keeps_the_lag() {
        let mut s = S::at(t(11, 36, 22)).streaming(receiving());
        s.playhead = t(11, 30, 49);
        assert_eq!(
            s.vm().selection_chip_text(&Utc, &DESIGN_GLYPHS, true),
            "11:30 · 5:33 behind"
        );
        s.playhead = t(11, 10, 0);
        assert_eq!(
            s.vm().selection_chip_text(&Utc, &DESIGN_GLYPHS, true),
            "11:10 · 26 min behind"
        );
    }

    #[wasm_bindgen_test]
    fn compact_chip_without_a_stream_has_no_lag() {
        let mut s = S::at(t(11, 36, 22));
        s.playhead = t(11, 30, 49);
        assert_eq!(
            s.vm().selection_chip_text(&Utc, &DESIGN_GLYPHS, true),
            "At 11:30"
        );
    }

    /// Every compact shape the phone bar can show, each as short as it
    /// reads.
    #[wasm_bindgen_test]
    fn compact_chip_labels_for_every_selection_shape() {
        let compact = |s: &S| s.vm().selection_chip_text(&Utc, &DESIGN_GLYPHS, true);
        let mut s = S::at(t(12, 0, 0));
        s.selection = PINNED;
        assert_eq!(compact(&s), "● LIVE");

        s.selection = fixed_range(t(11, 28, 0), t(11, 34, 0));
        assert_eq!(compact(&s), "⟲ 11:28 → 11:34");

        for (n, text) in [(2, "⟲ 2 frames → now"), (4, "⟲ 4 frames → now")] {
            s.selection = Selection::Range(RangeSelection {
                bounds: Some((t(11, 40, 0), t(12, 0, 0))),
                live: true,
                basis: LoopBasis::FrameCount(n),
            });
            assert_eq!(compact(&s), text);
        }
        for (secs, text) in [(1800.0, "⟲ 30 min → now"), (7200.0, "⟲ 2 h → now")] {
            s.selection = Selection::Range(RangeSelection {
                bounds: Some((t(12, 0, 0) - secs, t(12, 0, 0))),
                live: true,
                basis: LoopBasis::Duration(secs),
            });
            assert_eq!(compact(&s), text);
        }
        // The desktop keeps "Last".
        assert_eq!(chip(&s.vm()), "⟲ Last 2 h → now");

        // A loop style still rides on the end.
        s.loop_mode = LoopMode::PingPong;
        assert_eq!(compact(&s), "⟲ 2 h → now · Ping-pong");
    }

    #[wasm_bindgen_test]
    fn range_across_midnight_carries_dates() {
        let mut s = S::at(DAY + 86_400.0 * 2.0);
        s.selection = fixed_range(t(23, 10, 0), t(23, 10, 0) + 3600.0);
        assert_eq!(chip(&s.vm()), "⟲ 10/15 23:10 → 10/16 00:10");
    }

    #[wasm_bindgen_test]
    fn lag_never_goes_negative() {
        let mut s = S::at(t(12, 0, 0)).streaming(receiving());
        s.playhead = t(12, 0, 5);
        assert_eq!(chip(&s.vm()), "At 12:00:05 · 0:00 behind");
    }

    // ── The caption rule (§7) ───────────────────────────────────────────

    fn tilt(step: f64) -> CaptionContext {
        CaptionContext {
            frame_step_secs: step,
            wanted_tilt_deg: Some(0.5),
            ..CaptionContext::default()
        }
    }

    #[wasm_bindgen_test]
    fn a_blank_map_always_says_why() {
        let mut s = S::at(t(12, 0, 0));
        s.playhead = t(11, 21, 49);
        s.caption = CaptionContext {
            fetch_covers_playhead: true,
            ..tilt(300.0)
        };
        assert_eq!(
            s.vm().canvas_caption_text(&Utc).as_deref(),
            Some("no frame yet · downloading 11:21:49")
        );

        // Nothing coming, but the archive lists the scan: offer to fetch it.
        s.playhead = t(11, 30, 0);
        s.caption = CaptionContext {
            fetchable_scan_start: Some(t(11, 28, 0) as i64),
            ..tilt(300.0)
        };
        let vm = s.vm();
        let line = vm.canvas_caption_line(&Utc).unwrap();
        assert_eq!(line.text(), "no 0.5° data at 11:30 · fetch");
        assert_eq!(
            line.action,
            Some(Intent::FetchScan {
                scan_start: t(11, 28, 0) as i64,
                elevation_filter: None,
            })
        );
        assert!(line
            .segments
            .iter()
            .any(|(w, r)| w == "fetch" && *r == CaptionRole::Action));

        // Nothing listed either.
        s.caption = tilt(300.0);
        assert_eq!(
            s.vm().canvas_caption_text(&Utc).as_deref(),
            Some("no 0.5° data at 11:30")
        );

        // Pinned with a stream: the frame is on its way.
        let mut s = S::at(t(12, 0, 0)).streaming(waiting(30.0, None));
        s.selection = PINNED;
        s.live_cut = Some(cut(5, 4.0, 1, 6));
        s.caption = tilt(300.0);
        assert_eq!(
            s.vm().canvas_caption_text(&Utc).as_deref(),
            Some("no frame yet · waiting for 0.5° (antenna on 4.0°)")
        );
    }

    #[wasm_bindgen_test]
    fn within_one_frame_there_is_no_caption() {
        let f = frame(t(11, 0, 0), 1, 0.5);
        let mut s = S::at(t(12, 0, 0));
        s.displayed = Some(&f);
        s.caption = tilt(300.0);
        // Inside the frame, and up to one step past its end.
        for playhead in [t(10, 59, 50), t(11, 0, 0), t(11, 5, 0)] {
            s.playhead = playhead;
            assert_eq!(s.vm().canvas_caption, CanvasCaption::None, "{playhead}");
        }
        // One second past one frame: the clocks disagree.
        s.playhead = t(11, 5, 1);
        assert_eq!(
            s.vm().canvas_caption_text(&Utc).as_deref(),
            Some("showing 10:59:50 · playhead 11:05:01 · no 0.5° at 11:05")
        );
    }

    /// The old rule let a 15-minute lookback count as coverage, so a
    /// 9-minute gap went unsaid (§inv 7.4). One frame is the yardstick now.
    #[wasm_bindgen_test]
    fn a_nine_minute_gap_inside_the_lookback_is_said() {
        let f = frame(t(11, 8, 18), 1, 0.5);
        let mut s = S::at(t(12, 0, 0));
        s.playhead = t(11, 17, 43);
        s.displayed = Some(&f);
        s.caption = tilt(300.0);
        assert!(matches!(
            s.vm().canvas_caption,
            CanvasCaption::Discrepancy { .. }
        ));
    }

    /// Pinned or not: a pinned frame later than one step says what the stream
    /// is waiting for.
    #[wasm_bindgen_test]
    fn pinned_to_now_captions_a_late_frame() {
        let f = frame(t(11, 10, 0), 1, 0.5);
        let mut s = S::at(t(11, 20, 0)).streaming(waiting(30.0, None));
        s.selection = PINNED;
        s.displayed = Some(&f);
        s.live_cut = Some(cut(6, 4.0, 2, 6));
        s.caption = tilt(300.0);
        let vm = s.vm();
        assert_eq!(
            vm.canvas_caption_text(&Utc).as_deref(),
            Some("showing 11:09:50 · playhead 11:20:00 · waiting for 0.5° (antenna on 4.0°)")
        );
        // The playhead's time is now: its segment says so for the shell's red.
        let line = vm.canvas_caption_line(&Utc).unwrap();
        assert!(line
            .segments
            .iter()
            .any(|(_, r)| *r == CaptionRole::Playhead { pinned: true }));
        assert!(line.segments.iter().any(|(_, r)| *r == CaptionRole::Frame));
    }

    #[wasm_bindgen_test]
    fn a_frame_after_the_playhead_is_said_too() {
        let f = frame(t(11, 30, 0), 1, 0.5);
        let mut s = S::at(t(12, 0, 0));
        s.playhead = t(11, 0, 0);
        s.displayed = Some(&f);
        s.caption = CaptionContext {
            wanted_tilt_deg: None,
            ..tilt(300.0)
        };
        assert_eq!(
            s.vm().canvas_caption_text(&Utc).as_deref(),
            Some("showing 11:29:50 · playhead 11:00:00 · no data at 11:00")
        );
    }

    #[wasm_bindgen_test]
    fn a_render_in_flight_is_not_a_disagreement() {
        let f = frame(t(11, 0, 0), 1, 0.5);
        let mut s = S::at(t(12, 0, 0));
        s.playhead = t(11, 30, 0);
        s.displayed = Some(&f);
        s.caption = CaptionContext {
            render_pending: true,
            ..tilt(300.0)
        };
        assert_eq!(s.vm().canvas_caption, CanvasCaption::None);
        s.displayed = None;
        assert_eq!(s.vm().canvas_caption, CanvasCaption::None);
    }

    #[wasm_bindgen_test]
    fn volume_rendering_pinned_says_why_it_waits() {
        let mut s = S::at(t(12, 0, 0)).streaming(receiving());
        s.selection = PINNED;
        s.caption = CaptionContext {
            volume_waiting: true,
            ..tilt(300.0)
        };
        assert_eq!(
            s.vm().canvas_caption_text(&Utc).as_deref(),
            Some("waiting for the volume to complete · volume rendering needs every tilt")
        );
    }

    /// Live now needs a running stream and a pinned selection.
    #[wasm_bindgen_test]
    fn live_now_truth_table() {
        let now = t(12, 0, 0);
        for (stream, selection, want) in [
            (true, PINNED, true),
            (true, last_6_frames((now - 600.0, now)), true),
            (true, POINT, false),
            (false, PINNED, false),
            (false, POINT, false),
        ] {
            let mut s = S::at(now);
            if stream {
                s = s.streaming(receiving());
            }
            s.selection = selection;
            assert_eq!(s.vm().live_now, want, "{stream} {selection:?}");
        }
    }

    #[wasm_bindgen_test]
    fn cap_and_edge_chip_intents() {
        let now = t(12, 0, 0);
        let mut s = S::at(now).streaming(receiving());
        s.selection = PINNED;
        assert_eq!(
            s.vm().now_cap_intent(),
            Intent::StopLive(LiveStopPlacement::LiveEdge)
        );
        assert_eq!(s.vm().now_edge_chip_intent(), None);

        s.selection = POINT;
        assert_eq!(s.vm().now_cap_intent(), Intent::ReturnToLive);
        assert_eq!(s.vm().now_edge_chip_intent(), Some(Intent::ReturnToLive));

        let s = S::at(now);
        assert_eq!(s.vm().now_cap_intent(), Intent::GoLive);
        assert_eq!(s.vm().now_edge_chip_intent(), Some(Intent::GoLive));
    }

    #[wasm_bindgen_test]
    fn headline_word_only_for_healthy_streams() {
        assert_eq!(Stream::Connecting.headline_word(), Some("Connecting"));
        assert_eq!(Stream::Receiving.headline_word(), Some("Receiving"));
        assert_eq!(Stream::Waiting.headline_word(), Some("Waiting"));
        for s in [Stream::Off, Stream::Stalled, Stream::Failed] {
            assert_eq!(s.headline_word(), None);
        }
    }

    #[wasm_bindgen_test]
    fn hover_names_the_late_sweep_when_known() {
        let now = t(12, 0, 0);
        let s = S::at(now).streaming(LiveStatus {
            activity: StreamActivity::Stalled,
            expected_sweep: Some(expected(2.4, now - 60.0)),
            ..LiveStatus::default()
        });
        assert_eq!(
            s.vm().activity_hover,
            Some("Expected sweep is late — change elevation or stop streaming")
        );
        assert_eq!(S::at(now).vm().activity_hover, None);
    }

    #[wasm_bindgen_test]
    fn the_loop_style_rides_on_a_range_chip_unless_it_is_plain_loop() {
        let mut s = S::at(1_000.0);
        s.selection = fixed_range(400.0, 760.0);
        assert_eq!(s.vm().loop_style, None);
        s.loop_mode = LoopMode::PingPong;
        let vm = s.vm();
        assert_eq!(vm.loop_style, Some(LoopMode::PingPong));
        assert!(chip(&vm).ends_with(" · Ping-pong"), "{}", chip(&vm));
        // A point has no loop to style.
        s.selection = POINT;
        assert_eq!(s.vm().loop_style, None);
    }
}
