//! The phone's chrome around the row (phone timeline v2): the words and
//! states of the transport row and the top bar, so the egui layer only
//! draws them.
//!
//! - **The selection chip** ([`phone_chip`]): one form per state, short
//!   enough never to truncate: `Live`, `23:49`, `Last 4 frames`,
//!   `11:28 → 11:34`. Its tone is the state (red filled live, amber a
//!   loop, grey a point) and its outline is dashed while the frame under
//!   the playhead loads. The lag lives in the age pill, not here.
//! - **The LIVE button** ([`live_button`]): only while the selection is not
//!   pinned to now; it rejoins a running stream or starts one, first
//!   bringing now into view when it is off the window, as the strip's old
//!   now tab did.
//! - **The top bar's clock** ([`phone_clock`]): the frame on screen,
//!   `23:49:40`, with the date ahead of it only when the frame is not from
//!   today (`Oct 19 · 23:49:40`); while a frame loads, the time asked for
//!   (`23:49`). Shorter forms follow for a narrow bar.
//! - **The age pill** ([`phone_age`]): red `● LIVE` pinned to now,
//!   `20 min ago` at a point, amber `⟲ 13 min ago` during a loop.
//! - **The bell** ([`phone_bell`]): the alerts in view, with a dot when
//!   something failed; a tap opens the activity sheet while the dot shows,
//!   else the alerts.

use crate::core::selection_chip::ChipLabel;
use crate::core::session::{Clock, NowCap, Playhead, SelectionFact, SessionVm};
use crate::core::top_bar::{AlertsChip, FrameReadout};
use crate::core::Intent;

// ───────────────────────────────────────────────────────────────────────────
// The chip
// ───────────────────────────────────────────────────────────────────────────

/// The chip's tone: what is selected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ChipTone {
    /// Pinned to now: red, filled, with a white dot.
    Live,
    /// A range: amber outline.
    Loop,
    /// A point in time: grey outline.
    Point,
}

/// The phone's selection chip.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PhoneChip {
    pub tone: ChipTone,
    pub text: String,
    /// The frame under the playhead is on its way: a dashed outline.
    pub loading: bool,
}

/// The chip for a selection. `arrow` is the glyph between a range's ends
/// (the shell's font may lack `→`).
pub(crate) fn phone_chip(
    label: &ChipLabel,
    loading: bool,
    clock: &dyn Clock,
    arrow: &str,
) -> PhoneChip {
    let (tone, text) = match *label {
        ChipLabel::Live => (ChipTone::Live, "Live".to_string()),
        ChipLabel::At { ts, .. } => (ChipTone::Point, clock.hm(ts)),
        ChipLabel::RangeToNow { basis } => (ChipTone::Loop, format!("Last {}", basis.label())),
        ChipLabel::Range { start, end } => {
            let (a, b) = (clock.day(start), clock.day(end));
            let text = if a == b {
                format!("{} {arrow} {}", clock.hm(start), clock.hm(end))
            } else {
                format!(
                    "{}/{} {} {arrow} {}/{} {}",
                    a.1,
                    a.2,
                    clock.hm(start),
                    b.1,
                    b.2,
                    clock.hm(end)
                )
            };
            (ChipTone::Loop, text)
        }
    };
    PhoneChip {
        tone,
        text,
        loading,
    }
}

// ───────────────────────────────────────────────────────────────────────────
// The LIVE button
// ───────────────────────────────────────────────────────────────────────────

/// What a tap on the LIVE button does: rejoin a running stream or start
/// one; with now off the window (`edge`) it first brings now into view
/// (`CenterTimelineOnNow`, for the desktop timeline the session shares).
/// Nothing while already live.
pub(crate) fn cap_intents(cap: NowCap, edge: bool) -> Vec<Intent> {
    let attach = match cap {
        NowCap::Live => return Vec::new(),
        NowCap::Rejoin => Intent::ReturnToLive,
        NowCap::GoLive => Intent::GoLive,
    };
    if edge {
        vec![Intent::CenterTimelineOnNow, attach]
    } else {
        vec![attach]
    }
}

/// The LIVE button's intents, or `None` when it does not show (the
/// selection is pinned to now). `now_in_window`: the row shows now.
pub(crate) fn live_button(cap: NowCap, now_in_window: bool) -> Option<Vec<Intent>> {
    (cap != NowCap::Live).then(|| cap_intents(cap, !now_in_window))
}

// ───────────────────────────────────────────────────────────────────────────
// The top bar
// ───────────────────────────────────────────────────────────────────────────

/// The top bar's one clock.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PhoneClock {
    /// Longest first; the bar draws the first that fits.
    pub forms: Vec<String>,
    /// The zone, as a small suffix.
    pub zone: String,
    /// Something is on screen (or on its way); dimmed otherwise.
    pub bright: bool,
}

/// The clock: the frame on screen, dated only when not from today; while
/// a frame loads, the start of the volume asked for (`loading_time`).
pub(crate) fn phone_clock(readout: &FrameReadout, loading_time: Option<&str>) -> PhoneClock {
    let zone = readout.zone.clone();
    if let Some(t) = loading_time {
        return PhoneClock {
            forms: vec![t.to_string()],
            zone,
            bright: true,
        };
    }
    let short = |t: &str| {
        let b = t.as_bytes();
        if b.len() == 8 && b[2] == b':' && b[5] == b':' {
            t[..5].to_string()
        } else {
            t.to_string()
        }
    };
    let mut forms = match &readout.date {
        Some(d) => vec![
            format!("{d} \u{00B7} {}", readout.time),
            format!("{d} \u{00B7} {}", short(&readout.time)),
        ],
        None => vec![readout.time.clone()],
    };
    forms.dedup();
    PhoneClock {
        forms,
        zone,
        bright: readout.has_frame,
    }
}

/// The age pill's tone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AgeTone {
    /// Pinned to now: red, filled, `● LIVE`.
    Live,
    /// A loop: amber, `⟲ 13 min ago`.
    Loop,
    /// A point: grey outline, `20 min ago`.
    Point,
}

/// The age pill.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PhoneAge {
    pub tone: AgeTone,
    /// The words after the mark (the shell draws the dot or the loop
    /// mark): `LIVE`, `13 min ago`, `no frame yet`.
    pub text: String,
}

/// The pill: what the selection is and how old the frame on screen is.
pub(crate) fn phone_age(session: &SessionVm) -> PhoneAge {
    let age = session.frame.as_ref().map(|f| format_ago(f.age_secs));
    let none = || "no frame yet".to_string();
    match (session.selection, session.playhead) {
        (SelectionFact::Point, Playhead::PinnedToNow) => PhoneAge {
            tone: AgeTone::Live,
            text: "LIVE".to_string(),
        },
        (SelectionFact::Point, _) => PhoneAge {
            tone: AgeTone::Point,
            text: age.unwrap_or_else(none),
        },
        _ => PhoneAge {
            tone: AgeTone::Loop,
            text: age.unwrap_or_else(none),
        },
    }
}

/// An age in one unit, rounded: `28 s ago`, `20 min ago`, `3 h ago`,
/// `4 d ago`, `2 y ago`.
pub(crate) fn format_ago(age_secs: f64) -> String {
    let secs = age_secs.max(0.0);
    if secs.round() < 60.0 {
        return format!("{:.0} s ago", secs.round());
    }
    let m = (secs / 60.0).round();
    if m < 60.0 {
        return format!("{m:.0} min ago");
    }
    let h = (secs / 3600.0).round();
    if h < 24.0 {
        return format!("{h:.0} h ago");
    }
    let d = (secs / 86_400.0).round();
    if d < 365.0 {
        return format!("{d:.0} d ago");
    }
    format!("{:.0} y ago", (secs / (365.25 * 86_400.0)).round().max(1.0))
}

/// The bell's colour.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum BellTone {
    /// Nothing in view (or not known yet).
    Quiet,
    /// The alerts feed failed.
    Failed,
    /// Alerts in view: the most severe one's event, for its colour.
    Alert(String),
}

/// The bell in the top bar.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PhoneBell {
    pub tone: BellTone,
    /// The alerts in view, when there are any.
    pub count: Option<String>,
    /// Something failed (a download, an error not yet seen): the dot.
    pub dot: bool,
    pub hover: String,
    pub tap: Intent,
}

/// The bell, from the alerts chip and the activity's failures (`warning`,
/// the ⚠ tick's hover, when there is one). The dot takes the tap: the
/// activity sheet is where the failures are; otherwise the alerts.
pub(crate) fn phone_bell(alerts: &AlertsChip, warning: Option<&str>) -> PhoneBell {
    let sheet = Intent::SetActivitySheetOpen(true);
    let (tone, count, hover, click) = match alerts {
        AlertsChip::Hidden => (BellTone::Quiet, None, String::new(), None),
        AlertsChip::Loading => (
            BellTone::Quiet,
            None,
            "Loading NWS alerts\u{2026}".to_string(),
            None,
        ),
        AlertsChip::Quiet {
            failed,
            hover,
            click,
        } => (
            if *failed {
                BellTone::Failed
            } else {
                BellTone::Quiet
            },
            None,
            hover.clone(),
            Some(click.clone()),
        ),
        AlertsChip::Active {
            compact,
            event,
            hover,
            click,
            ..
        } => (
            BellTone::Alert(event.clone()),
            Some(compact.clone()),
            hover.clone(),
            Some(click.clone()),
        ),
    };
    match warning {
        Some(w) => PhoneBell {
            tone,
            count,
            dot: true,
            hover: w.to_string(),
            tap: sheet,
        },
        None => PhoneBell {
            tone,
            count,
            dot: false,
            hover: if hover.is_empty() {
                "Activity".to_string()
            } else {
                hover
            },
            tap: click.unwrap_or(sheet),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::session::FrameFact;
    use crate::core::LoopBasis;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// UTC, `HH:MM` and `HH:MM:SS`, days from the epoch.
    struct Utc;
    impl Clock for Utc {
        fn hms(&self, ts: f64) -> String {
            let s = ts.rem_euclid(86_400.0) as i64;
            format!("{:02}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
        }
        fn hm(&self, ts: f64) -> String {
            let s = ts.rem_euclid(86_400.0) as i64;
            format!("{:02}:{:02}", s / 3600, (s % 3600) / 60)
        }
        fn day(&self, ts: f64) -> (i32, u32, u32) {
            (1970, 1, 1 + (ts / 86_400.0).floor() as u32)
        }
    }

    const T: f64 = 23.0 * 3600.0 + 49.0 * 60.0 + 40.0;

    #[wasm_bindgen_test]
    fn the_chip_has_one_short_form_per_state() {
        let chip = |l: ChipLabel| phone_chip(&l, false, &Utc, "→");
        assert_eq!(chip(ChipLabel::Live).text, "Live");
        assert_eq!(chip(ChipLabel::Live).tone, ChipTone::Live);
        let at = chip(ChipLabel::At {
            ts: T,
            behind_secs: Some(1_200.0),
        });
        assert_eq!(at.text, "23:49", "no lag, no seconds");
        assert_eq!(at.tone, ChipTone::Point);
        let last = chip(ChipLabel::RangeToNow {
            basis: LoopBasis::FrameCount(4),
        });
        assert_eq!(
            (last.text.as_str(), last.tone),
            ("Last 4 frames", ChipTone::Loop)
        );
        let r = chip(ChipLabel::Range {
            start: 11.0 * 3600.0 + 28.0 * 60.0,
            end: 11.0 * 3600.0 + 34.0 * 60.0,
        });
        assert_eq!((r.text.as_str(), r.tone), ("11:28 → 11:34", ChipTone::Loop));
        // Across midnight, each end carries its day.
        let r = chip(ChipLabel::Range {
            start: T,
            end: 86_400.0 + 600.0,
        });
        assert_eq!(r.text, "1/1 23:49 → 1/2 00:10");
    }

    #[wasm_bindgen_test]
    fn the_chip_is_dashed_while_its_frame_loads() {
        let c = phone_chip(
            &ChipLabel::At {
                ts: T,
                behind_secs: None,
            },
            true,
            &Utc,
            "→",
        );
        assert!(c.loading);
    }

    #[wasm_bindgen_test]
    fn live_shows_only_off_now_and_rejoins_or_starts() {
        assert_eq!(live_button(NowCap::Live, true), None);
        assert!(matches!(
            live_button(NowCap::Rejoin, true).unwrap()[..],
            [Intent::ReturnToLive]
        ));
        assert!(matches!(
            live_button(NowCap::GoLive, true).unwrap()[..],
            [Intent::GoLive]
        ));
        // Now off the window: bring it into view first.
        assert!(matches!(
            live_button(NowCap::Rejoin, false).unwrap()[..],
            [Intent::CenterTimelineOnNow, Intent::ReturnToLive]
        ));
        assert!(cap_intents(NowCap::Live, false).is_empty());
    }

    fn readout(date: Option<&str>, has_frame: bool) -> FrameReadout {
        FrameReadout {
            date: date.map(str::to_string),
            day: "Oct 20".to_string(),
            time: "23:49:40".to_string(),
            has_frame,
            zone: "UTC".to_string(),
            zone_hover: String::new(),
        }
    }

    #[wasm_bindgen_test]
    fn one_clock_dated_only_when_not_today_and_the_asked_time_while_loading() {
        let c = phone_clock(&readout(None, true), None);
        assert_eq!(c.forms, vec!["23:49:40"]);
        assert_eq!(c.zone, "UTC");
        assert!(c.bright);
        let c = phone_clock(&readout(Some("Oct 19"), true), None);
        assert_eq!(c.forms, vec!["Oct 19 · 23:49:40", "Oct 19 · 23:49"]);
        let c = phone_clock(&readout(Some("Oct 19"), true), Some("23:42"));
        assert_eq!(c.forms, vec!["23:42"]);
        assert!(!phone_clock(&readout(None, false), None).bright);
    }

    fn session(selection: SelectionFact, playhead: Playhead, age: Option<f64>) -> SessionVm {
        SessionVm {
            selection,
            playhead,
            frame: age.map(|a| FrameFact {
                time_secs: 0.0,
                end_secs: 0.0,
                elevation_deg: 0.5,
                product: "reflectivity".to_string(),
                age_secs: a,
                partial: None,
            }),
            ..SessionVm::default()
        }
    }

    #[wasm_bindgen_test]
    fn the_age_pill_says_live_the_age_or_the_loops_age() {
        let live = phone_age(&session(
            SelectionFact::Point,
            Playhead::PinnedToNow,
            Some(30.0),
        ));
        assert_eq!((live.tone, live.text.as_str()), (AgeTone::Live, "LIVE"));
        let at = Playhead::AtTime {
            ts: 0.0,
            lag_secs: 1_200.0,
        };
        let point = phone_age(&session(SelectionFact::Point, at, Some(1_190.0)));
        assert_eq!(
            (point.tone, point.text.as_str()),
            (AgeTone::Point, "20 min ago")
        );
        let lp = phone_age(&session(
            SelectionFact::RangeToNow {
                basis: LoopBasis::FrameCount(4),
            },
            at,
            Some(780.0),
        ));
        assert_eq!((lp.tone, lp.text.as_str()), (AgeTone::Loop, "13 min ago"));
        let empty = phone_age(&session(SelectionFact::Point, at, None));
        assert_eq!(empty.text, "no frame yet");
    }

    #[wasm_bindgen_test]
    fn ages_round_to_one_unit() {
        assert_eq!(format_ago(28.4), "28 s ago");
        assert_eq!(format_ago(59.6), "1 min ago");
        assert_eq!(format_ago(20.0 * 60.0), "20 min ago");
        assert_eq!(format_ago(3.0 * 3600.0), "3 h ago");
        assert_eq!(format_ago(4.0 * 86_400.0), "4 d ago");
        assert_eq!(format_ago(800.0 * 86_400.0), "2 y ago");
    }

    #[wasm_bindgen_test]
    fn the_bell_dots_failures_and_their_tap_opens_the_activity_sheet() {
        let click = Intent::SetActivitySheetOpen(false);
        let active = AlertsChip::Active {
            label: "Tornado Warning".to_string(),
            compact: "3".to_string(),
            event: "Tornado Warning".to_string(),
            hover: "3 alerts".to_string(),
            click: click.clone(),
        };
        let b = phone_bell(&active, None);
        assert_eq!(b.tone, BellTone::Alert("Tornado Warning".to_string()));
        assert_eq!(b.count.as_deref(), Some("3"));
        assert!(!b.dot);
        assert_eq!(b.tap, click);
        let b = phone_bell(&active, Some("Some downloads failed"));
        assert!(b.dot);
        assert_eq!(b.tap, Intent::SetActivitySheetOpen(true));
        assert_eq!(b.hover, "Some downloads failed");
        // With no alerts to show, the bell still opens the activity sheet.
        let b = phone_bell(&AlertsChip::Hidden, None);
        assert_eq!(b.tap, Intent::SetActivitySheetOpen(true));
        assert_eq!(b.count, None);
    }
}
