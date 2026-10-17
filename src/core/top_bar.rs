//! The top bar (one-instrument §9): what am I looking at.
//!
//! Site, product and tilt are three pickers side by side; the frame time is
//! the primary readout, with its zone shown once and the age pill beside it.
//! Everything about what is *happening* lives in the transport and the
//! activity chip, so the bar carries no mode badge, stripe, status sentence,
//! errors chip, wordmark or version.
//!
//! [`TopBarVm::build`] projects this frame's state into the bar's words;
//! [`fold`] is the width rule; [`TopBarIntent`] is everything the bar can ask
//! for, applied by the dispatch shell (the tilt decision is
//! [`apply_tilt_pick`]). The egui layer (`ui::top_bar`, `ui::mobile::top_bar`)
//! only draws the view-model and emits the intents it carries.

use crate::core::diagnostics::{DiagnosticsIntent, VisibleAlert};
use crate::core::session::{AgeMarker, Clock, SessionVm};
use crate::core::{AlertsState, ElevationListEntry, ElevationSelection, Intent, RadarProduct};
use crate::geo::ViewMode;

pub(crate) use crate::core::ladder::{angle_label, apply_tilt_pick, tilt_rows, TiltPick, TiltRow};

// ───────────────────────────────────────────────────────────────────────────
// Intents
// ───────────────────────────────────────────────────────────────────────────

/// What the top bar (and the toast under it) can ask for.
#[derive(Clone, PartialEq, Debug)]
pub(crate) enum TopBarIntent {
    /// The site picker was clicked: open the site dialog.
    OpenSitePicker,
    /// A product was chosen.
    SetProduct(RadarProduct),
    /// A tilt was chosen.
    SetTilt(TiltPick),
    /// The zone label (or the frame clock) was clicked: flip local ↔ UTC.
    /// The interim home of the zone preference until Settings exists (§4).
    ToggleTimeZone,
    /// 2D or 3D.
    SetView(ViewMode),
    /// Show or hide the keyboard map.
    ToggleShortcutsHelp,
    /// Fold or open the panel (the rail stays).
    ToggleRightPanel,
    /// ⋯ › Settings: open the panel on its Settings tab.
    OpenSettings,
    /// The toast was clicked away.
    DismissToast,
}

// ───────────────────────────────────────────────────────────────────────────
// The view-model
// ───────────────────────────────────────────────────────────────────────────

/// Everything [`TopBarVm::build`] reads: plain data gathered by the shell.
pub(crate) struct TopBarInputs<'a> {
    pub session: &'a SessionVm,
    pub now_secs: f64,
    pub site_id: &'a str,
    pub product: RadarProduct,
    pub elevation_selection: &'a ElevationSelection,
    /// The volume's cuts, as the panel's elevation list builds them.
    pub elevations: &'a [ElevationListEntry],
    /// The chosen zone's abbreviation ("CDT", "UTC"); a browser fact.
    pub zone: &'a str,
    pub use_local_time: bool,
    pub view_mode: ViewMode,
    pub alerts: &'a AlertsState,
    /// Severity-sorted alerts intersecting the map ([`crate::core::diagnostics::DiagnosticsVm`]).
    pub visible_alerts: &'a [VisibleAlert],
    /// The 2D map has bounds this frame (false in 3D / before first paint).
    pub map_bounds_known: bool,
    pub worker_init_error: Option<&'a str>,
    /// The build's version and branch, for ⋯ › About.
    pub version: &'a str,
}

/// The site picker: the ID always, the city first to fold.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct SitePicker {
    pub id: String,
    /// "Oklahoma City, OK". `None` for a site the registry doesn't know.
    pub city: Option<String>,
}

#[derive(Clone, PartialEq, Debug)]
pub(crate) struct ProductRow {
    pub product: RadarProduct,
    /// "VEL", for the panel's segmented product row.
    pub code: &'static str,
    pub label: String,
    pub selected: bool,
}

#[derive(Clone, PartialEq, Debug)]
pub(crate) struct ProductPicker {
    /// "REF".
    pub code: &'static str,
    pub hover: &'static str,
    /// "Reflectivity · dBZ", under the panel's product row.
    pub name: String,
    pub rows: Vec<ProductRow>,
}

#[derive(Clone, PartialEq, Debug)]
pub(crate) struct TiltPicker {
    /// The mode and the cut it resolved to: "Latest 2.4°", or "0.5°".
    pub label: String,
    pub rows: Vec<TiltRow>,
}

/// The primary readout: the frame on screen, in the one clock (§4).
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct FrameReadout {
    /// "Oct 3" when the frame is not from today (in the chosen zone).
    pub date: Option<String>,
    /// The frame's date ("Oct 17"), today's with no frame: the phone's
    /// readout always shows it, and a tap on it opens "Go to a day".
    pub day: String,
    /// "11:21:57", or "--:--:--" with no frame.
    pub time: String,
    pub has_frame: bool,
    /// The zone, shown once here; every other clock inherits it.
    pub zone: String,
    pub zone_hover: String,
}

/// The mark at the head of the pill.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PillMark {
    /// Red dot: pinned to now.
    Pinned,
    /// Amber loop mark: a range ending at now.
    Loop,
    /// No mark ("no frame yet").
    None,
}

/// The pill beside the frame time.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct ReadoutPill {
    pub mark: PillMark,
    /// "2 min old", "no frame yet".
    pub text: String,
    pub hover: &'static str,
}

/// The alerts chip, colored after the most severe event in view.
#[derive(Clone, PartialEq, Debug)]
pub(crate) enum AlertsChip {
    /// 3D, or the map hasn't painted: nothing to count.
    Hidden,
    /// The first fetch is still out.
    Loading,
    /// No alerts in view (or the feed failed: `failed`, drawn with ⚠).
    Quiet {
        failed: bool,
        hover: String,
        click: Intent,
    },
    /// Alerts in view.
    Active {
        /// "Tornado Warning", "13 warnings · 11 watches".
        label: String,
        /// The phone bar's short form: the count.
        compact: String,
        /// Event of the most severe alert, for the chip's color.
        event: String,
        hover: String,
        click: Intent,
    },
}

/// The bar, ready to draw.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct TopBarVm {
    pub site: SitePicker,
    pub product: ProductPicker,
    pub tilt: TiltPicker,
    pub readout: FrameReadout,
    pub pill: Option<ReadoutPill>,
    pub alerts: AlertsChip,
    /// The decode worker failed to start: ⚠, the message and Retry.
    pub worker_error: Option<String>,
    /// 2D / 3D; the active one.
    pub view_mode: ViewMode,
    /// ⋯ › About.
    pub version: String,
}

impl TopBarVm {
    pub(crate) fn build(inputs: TopBarInputs<'_>, clock: &dyn Clock) -> Self {
        let session = inputs.session;
        let frame = session.frame.as_ref();

        let site = SitePicker {
            id: inputs.site_id.to_string(),
            city: crate::data::get_site(inputs.site_id).map(|s| match s.state {
                Some(st) => format!("{}, {st}", title_case(s.name)),
                None => title_case(s.name),
            }),
        };

        let product = ProductPicker {
            code: inputs.product.short_code(),
            hover: inputs.product.label(),
            name: format!(
                "{} \u{00B7} {}",
                inputs.product.label(),
                inputs.product.unit()
            ),
            rows: RadarProduct::all()
                .iter()
                .map(|&p| ProductRow {
                    product: p,
                    code: p.short_code(),
                    label: format!("{}  {}", p.short_code(), p.label()),
                    selected: p == inputs.product,
                })
                .collect(),
        };

        let tilt = tilt_picker(
            inputs.elevation_selection,
            inputs.elevations,
            inputs.product,
            frame.map(|f| f.elevation_deg),
        );

        let readout = match frame {
            Some(f) => FrameReadout {
                date: (clock.day(f.time_secs) != clock.day(inputs.now_secs))
                    .then(|| short_date(clock.day(f.time_secs))),
                day: short_date(clock.day(f.time_secs)),
                time: clock.hms(f.time_secs),
                has_frame: true,
                zone: inputs.zone.to_string(),
                zone_hover: zone_hover(inputs.zone, inputs.use_local_time),
            },
            None => FrameReadout {
                date: None,
                day: short_date(clock.day(inputs.now_secs)),
                time: "--:--:--".to_string(),
                has_frame: false,
                zone: inputs.zone.to_string(),
                zone_hover: zone_hover(inputs.zone, inputs.use_local_time),
            },
        };

        let pill = match (&session.age_pill, frame) {
            (Some(p), _) => Some(ReadoutPill {
                mark: match p.marker {
                    AgeMarker::Pinned => PillMark::Pinned,
                    AgeMarker::Loop => PillMark::Loop,
                },
                text: p.text.clone(),
                hover: match p.marker {
                    AgeMarker::Pinned => "Pinned to now: the newest frame's age",
                    AgeMarker::Loop => "Looping to now: the frame on screen's age",
                },
            }),
            (None, None) => Some(ReadoutPill {
                mark: PillMark::None,
                text: "no frame yet".to_string(),
                hover: "Nothing on screen yet",
            }),
            (None, Some(_)) => None,
        };

        TopBarVm {
            site,
            product,
            tilt,
            readout,
            pill,
            alerts: alerts_chip(
                inputs.alerts,
                inputs.visible_alerts,
                inputs.map_bounds_known,
            ),
            worker_error: inputs.worker_init_error.map(str::to_string),
            view_mode: inputs.view_mode,
            version: inputs.version.to_string(),
        }
    }
}

fn zone_hover(zone: &str, use_local: bool) -> String {
    if use_local {
        format!("Times in {zone} — click for UTC")
    } else {
        "Times in UTC — click for local time".to_string()
    }
}

fn tilt_picker(
    selection: &ElevationSelection,
    entries: &[ElevationListEntry],
    product: RadarProduct,
    frame_deg: Option<f32>,
) -> TiltPicker {
    let label = match selection {
        ElevationSelection::Latest => match frame_deg {
            Some(d) => format!("Latest {}", angle_label(d)),
            None => "Latest".to_string(),
        },
        ElevationSelection::Fixed { angle, .. } => angle_label(*angle),
    };

    let rows = tilt_rows(selection, entries, product);
    TiltPicker { label, rows }
}

fn alerts_chip(alerts: &AlertsState, visible: &[VisibleAlert], bounds_known: bool) -> AlertsChip {
    let has_loaded = alerts.last_success_ms > 0.0;
    let error = alerts.last_error.as_deref();
    if !has_loaded && error.is_none() {
        return AlertsChip::Loading;
    }
    if !bounds_known {
        return AlertsChip::Hidden;
    }
    let refresh = Intent::Diagnostics(DiagnosticsIntent::RefreshAlerts);
    let Some(top) = visible.first() else {
        return AlertsChip::Quiet {
            failed: error.is_some(),
            hover: match error {
                Some(e) => format!("NWS alerts: {e} — click to retry"),
                None => format!(
                    "No active alerts in view ({} active nationwide)",
                    alerts.alerts.len()
                ),
            },
            click: refresh,
        };
    };

    let (label, hover, click) = if visible.len() == 1 {
        (
            top.event.clone(),
            format!("{} — click for details", top.event),
            Intent::Diagnostics(DiagnosticsIntent::SelectAlert(top.id.clone())),
        )
    } else {
        let warnings = visible.iter().filter(|a| a.is_warning).count();
        let others = visible.len() - warnings;
        let mut parts = Vec::new();
        if warnings > 0 {
            parts.push(plural(warnings, "warning", "warnings"));
        }
        if others > 0 {
            parts.push(plural(others, "watch", "watches"));
        }
        let mut hover = String::from("Click to view alerts in this area:\n");
        for a in visible.iter().take(6) {
            hover.push_str(&format!(
                "\n  \u{2022} [{}] {}",
                a.severity.label(),
                a.event
            ));
        }
        if visible.len() > 6 {
            hover.push_str(&format!("\n  \u{2026} and {} more", visible.len() - 6));
        }
        (
            parts.join(" \u{00B7} "),
            hover,
            Intent::Diagnostics(DiagnosticsIntent::OpenAlertList),
        )
    };
    AlertsChip::Active {
        label,
        compact: visible.len().to_string(),
        event: top.event.clone(),
        hover,
        click,
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// "Oct 3"
fn short_date((_, month, day): (i32, u32, u32)) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let m = MONTHS[(month.clamp(1, 12) - 1) as usize];
    format!("{m} {day}")
}

/// "OKLAHOMA CITY" → "Oklahoma City".
fn title_case(s: &str) -> String {
    s.split(' ')
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(first) => {
                    first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase()
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

// ───────────────────────────────────────────────────────────────────────────
// The width rule
// ───────────────────────────────────────────────────────────────────────────

/// Measured widths (px) of the bar's pieces, including their spacing. The
/// shell measures text; the decision is here.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct TopBarWidths {
    /// Everything that never folds: panel toggles, site ID, product and tilt
    /// pickers, frame time and pill, alerts chip, ⋯, worker error.
    pub fixed: f32,
    pub city: f32,
    pub zone: f32,
    /// Help button and 2D/3D toggle together.
    pub help_and_views: f32,
}

/// What stays inline this frame. Whatever is `false` is folded: the city and
/// the zone simply go (the zone is still in the readout's hover), help and
/// 2D/3D move into ⋯.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct TopBarFold {
    pub city: bool,
    pub zone: bool,
    pub help_and_views: bool,
}

/// Breathing room kept free so the clusters never touch.
const FOLD_MARGIN: f32 = 12.0;

/// The width rule (§9): as the bar narrows, the city folds first, then the
/// zone, then the help and 2D/3D buttons go into ⋯. The fixed cluster has
/// nothing variable in it, so it never clips.
pub(crate) fn fold(available: f32, w: &TopBarWidths) -> TopBarFold {
    let budget = available - FOLD_MARGIN;
    let mut f = TopBarFold {
        city: true,
        zone: true,
        help_and_views: true,
    };
    let used = |f: &TopBarFold| {
        w.fixed
            + if f.city { w.city } else { 0.0 }
            + if f.zone { w.zone } else { 0.0 }
            + if f.help_and_views {
                w.help_and_views
            } else {
                0.0
            }
    };
    if used(&f) > budget {
        f.city = false;
    }
    if used(&f) > budget {
        f.zone = false;
    }
    if used(&f) > budget {
        f.help_and_views = false;
    }
    f
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alerts::AlertSeverity;
    use crate::core::domain::playback::RangeSelection;
    use crate::core::live_status::LiveStatus;
    use crate::core::session::SessionInputs;
    use crate::core::{DisplayedSweep, FrameNow, LivePhase, LoopBasis, Selection, SweepIdentity};
    use crate::data::{ScanKey, UnixMillis};
    use wasm_bindgen_test::wasm_bindgen_test;

    /// 2025-10-15 00:00:00 UTC.
    const DAY: f64 = 1_760_486_400.0;

    fn t(h: u32, m: u32, s: u32) -> f64 {
        DAY + (h * 3600 + m * 60 + s) as f64
    }

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
            let d = ((ts - DAY) / 86_400.0).floor() as i64;
            (2025, 10, (15 + d) as u32)
        }
    }

    fn displayed(end: f64, deg: f32) -> DisplayedSweep {
        DisplayedSweep {
            identity: SweepIdentity::new(
                ScanKey::new("KTLX", UnixMillis(1_760_520_000_000)),
                1,
                "reflectivity",
            ),
            start_time: end - 20.0,
            end_time: end,
            elevation_deg: deg,
        }
    }

    fn session(selection: Selection, frame: Option<&DisplayedSweep>, now: f64) -> SessionVm {
        SessionVm::build(SessionInputs {
            now: FrameNow(now),
            live_status: &LiveStatus::default(),
            live_phase: LivePhase::Idle,
            last_exit: None,
            selection,
            playback_position_secs: now - 600.0,
            displayed: frame,
            live_cut: None,
            caption_context: crate::core::canvas::CaptionContext::default(),
            loop_mode: crate::core::LoopMode::Loop,
        })
    }

    struct Fixture {
        session: SessionVm,
        now: f64,
        selection: ElevationSelection,
        elevations: Vec<ElevationListEntry>,
        alerts: AlertsState,
        visible: Vec<VisibleAlert>,
        bounds: bool,
    }

    impl Fixture {
        fn new(session: SessionVm, now: f64) -> Self {
            Fixture {
                session,
                now,
                selection: ElevationSelection::default(),
                elevations: Vec::new(),
                alerts: AlertsState {
                    last_success_ms: 1.0,
                    ..AlertsState::default()
                },
                visible: Vec::new(),
                bounds: true,
            }
        }

        fn build(&self) -> TopBarVm {
            TopBarVm::build(
                TopBarInputs {
                    session: &self.session,
                    now_secs: self.now,
                    site_id: "KTLX",
                    product: RadarProduct::Reflectivity,
                    elevation_selection: &self.selection,
                    elevations: &self.elevations,
                    zone: "UTC",
                    use_local_time: false,
                    view_mode: ViewMode::Flat2D,
                    alerts: &self.alerts,
                    visible_alerts: &self.visible,
                    map_bounds_known: self.bounds,
                    worker_init_error: None,
                    version: "main (abc123)",
                },
                &Utc,
            )
        }
    }

    fn pinned(now: f64, frame: &DisplayedSweep) -> Fixture {
        Fixture::new(
            session(Selection::Point { live: true }, Some(frame), now),
            now,
        )
    }

    fn entry(n: u8, angle: f32, products: &[&str]) -> ElevationListEntry {
        ElevationListEntry {
            elevation_number: n,
            angle,
            waveform: String::new(),
            is_sails: false,
            is_mrle: false,
            cached_products: products.iter().map(|s| s.to_string()).collect(),
        }
    }

    // ── The readout in five states (§9) ────────────────────────────────

    #[wasm_bindgen_test]
    fn pinned_to_now_shows_the_frame_time_zone_and_red_dot_age() {
        let now = t(11, 24, 0);
        let f = displayed(t(11, 22, 7), 0.5);
        let vm = pinned(now, &f).build();
        assert_eq!(vm.readout.time, "11:21:57");
        assert_eq!(vm.readout.zone, "UTC");
        assert_eq!(vm.readout.date, None);
        let pill = vm.pill.unwrap();
        assert_eq!(pill.mark, PillMark::Pinned);
        assert_eq!(pill.text, "2 min old");
    }

    #[wasm_bindgen_test]
    fn a_loop_to_now_marks_the_pill_with_the_loop() {
        let now = t(11, 27, 0);
        let f = displayed(t(11, 1, 24), 0.5);
        let sel = Selection::Range(RangeSelection {
            bounds: Some((now - 1_800.0, now)),
            live: true,
            basis: LoopBasis::FrameCount(6),
        });
        let vm = Fixture::new(session(sel, Some(&f), now), now).build();
        assert_eq!(vm.readout.time, "11:01:14");
        let pill = vm.pill.unwrap();
        assert_eq!(pill.mark, PillMark::Loop);
        assert_eq!(pill.text, "26 min old");
    }

    #[wasm_bindgen_test]
    fn at_a_time_the_frame_time_stands_alone() {
        let now = t(12, 0, 0);
        let f = displayed(t(11, 29, 1), 0.5);
        let vm = Fixture::new(
            session(Selection::Point { live: false }, Some(&f), now),
            now,
        )
        .build();
        assert_eq!(vm.readout.time, "11:28:51");
        assert_eq!(vm.readout.date, None);
        assert!(vm.pill.is_none());
    }

    #[wasm_bindgen_test]
    fn another_day_puts_the_date_before_the_time() {
        let now = t(12, 0, 0);
        let f = displayed(t(21, 14, 18) - 12.0 * 86_400.0, 0.5);
        let vm = Fixture::new(
            session(Selection::Point { live: false }, Some(&f), now),
            now,
        )
        .build();
        assert_eq!(vm.readout.date.as_deref(), Some("Oct 3"));
        assert_eq!(vm.readout.day, "Oct 3");
        assert_eq!(vm.readout.time, "21:14:08");
    }

    #[wasm_bindgen_test]
    fn no_frame_yet_is_a_pill_that_says_so() {
        let now = t(12, 0, 0);
        let vm = Fixture::new(session(Selection::Point { live: true }, None, now), now).build();
        assert!(!vm.readout.has_frame);
        assert_eq!(vm.readout.time, "--:--:--");
        let pill = vm.pill.unwrap();
        assert_eq!(pill.mark, PillMark::None);
        assert_eq!(pill.text, "no frame yet");
    }

    #[wasm_bindgen_test]
    fn the_zone_hover_names_what_a_click_switches_to() {
        assert_eq!(zone_hover("CDT", true), "Times in CDT — click for UTC");
        assert_eq!(
            zone_hover("UTC", false),
            "Times in UTC — click for local time"
        );
    }

    // ── Pickers ─────────────────────────────────────────────────────────

    #[wasm_bindgen_test]
    fn the_site_picker_carries_the_id_and_a_readable_city() {
        let now = t(12, 0, 0);
        let f = displayed(now, 0.5);
        let vm = pinned(now, &f).build();
        assert_eq!(vm.site.id, "KTLX");
        assert_eq!(vm.site.city.as_deref(), Some("Oklahoma City, OK"));
    }

    #[wasm_bindgen_test]
    fn the_product_picker_lists_every_product_and_marks_the_chosen_one() {
        let now = t(12, 0, 0);
        let f = displayed(now, 0.5);
        let vm = pinned(now, &f).build();
        assert_eq!(vm.product.code, "REF");
        assert_eq!(vm.product.rows.len(), RadarProduct::all().len());
        assert_eq!(
            vm.product.rows.iter().filter(|r| r.selected).count(),
            1,
            "exactly one product is chosen"
        );
        assert_eq!(vm.product.rows[1].label, "VEL  Velocity");
        assert_eq!(vm.product.rows[1].code, "VEL");
        assert_eq!(vm.product.name, "Reflectivity · dBZ");
    }

    #[wasm_bindgen_test]
    fn tilt_latest_shows_the_mode_and_the_cut_it_resolved_to() {
        let now = t(12, 0, 0);
        let f = displayed(now, 2.4);
        let mut fx = pinned(now, &f);
        fx.selection = ElevationSelection::Latest;
        let vm = fx.build();
        assert_eq!(vm.tilt.label, "Latest 2.4°");
        assert!(vm.tilt.rows[0].selected);
        assert_eq!(vm.tilt.rows[0].pick, TiltPick::Latest);
    }

    #[wasm_bindgen_test]
    fn tilt_fixed_shows_its_angle_and_rows_follow_the_volume() {
        let now = t(12, 0, 0);
        let f = displayed(now, 0.5);
        let mut fx = pinned(now, &f);
        fx.selection = ElevationSelection::Fixed {
            elevation_number: 2,
            angle: 0.9,
        };
        let mut sails = entry(3, 0.5, &[]);
        sails.is_sails = true;
        fx.elevations = vec![
            entry(1, 0.5, &["reflectivity"]),
            entry(2, 0.9, &["velocity"]),
            sails,
        ];
        let vm = fx.build();
        assert_eq!(vm.tilt.label, "0.9°");
        // The top bar lists the ladder's rows: one per angle, the SAILS
        // repeat joined to its angle.
        let labels: Vec<_> = vm.tilt.rows.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels, ["Latest", "0.5°", "0.9°"]);
        assert!(vm.tilt.rows[2].selected);
        assert!(!vm.tilt.rows[0].selected);
        // Reflectivity is not recorded at 0.9°: the row says so, never disabled.
        assert!(vm.tilt.rows[1].reaches_product);
        assert!(!vm.tilt.rows[2].reaches_product);
        assert_eq!(vm.tilt.rows[1].note.as_deref(), Some("SAILS ×1"));
    }

    #[wasm_bindgen_test]
    fn tilt_rows_before_any_volume_offer_the_default_cut() {
        let now = t(12, 0, 0);
        let vm = Fixture::new(session(Selection::Point { live: true }, None, now), now).build();
        assert_eq!(vm.tilt.label, "0.5°");
        assert_eq!(vm.tilt.rows.len(), 2);
        assert!(vm.tilt.rows[1].selected);
    }

    #[wasm_bindgen_test]
    fn picking_latest_remembers_the_fixed_cut_it_left() {
        let mut sel = ElevationSelection::Fixed {
            elevation_number: 4,
            angle: 1.8,
        };
        let mut last = None;
        apply_tilt_pick(&mut sel, &mut last, TiltPick::Latest);
        assert!(sel == ElevationSelection::Latest);
        assert_eq!(last, Some((4, 1.8)));
        // Latest → Latest keeps the memory.
        apply_tilt_pick(&mut sel, &mut last, TiltPick::Latest);
        assert_eq!(last, Some((4, 1.8)));
        apply_tilt_pick(
            &mut sel,
            &mut last,
            TiltPick::Fixed {
                elevation_number: 1,
                angle: 0.5,
            },
        );
        assert!(
            sel == ElevationSelection::Fixed {
                elevation_number: 1,
                angle: 0.5
            }
        );
    }

    // ── Alerts chip ─────────────────────────────────────────────────────

    fn alert(id: &str, event: &str, warning: bool) -> VisibleAlert {
        VisibleAlert {
            id: id.into(),
            event: event.into(),
            area_desc: String::new(),
            severity: AlertSeverity::Severe,
            is_warning: warning,
            expires_secs: None,
        }
    }

    #[wasm_bindgen_test]
    fn alerts_chip_counts_warnings_and_watches_and_colors_by_the_first() {
        let now = t(12, 0, 0);
        let f = displayed(now, 0.5);
        let mut fx = pinned(now, &f);
        fx.visible = vec![
            alert("a", "Tornado Warning", true),
            alert("b", "Severe Thunderstorm Warning", true),
            alert("c", "Tornado Watch", false),
        ];
        match fx.build().alerts {
            AlertsChip::Active {
                label,
                compact,
                event,
                click,
                ..
            } => {
                assert_eq!(label, "2 warnings · 1 watch");
                assert_eq!(compact, "3");
                assert_eq!(event, "Tornado Warning");
                assert_eq!(click, Intent::Diagnostics(DiagnosticsIntent::OpenAlertList));
            }
            other => panic!("expected an active chip, got {other:?}"),
        }
    }

    #[wasm_bindgen_test]
    fn one_alert_names_its_event_and_opens_its_detail() {
        let now = t(12, 0, 0);
        let f = displayed(now, 0.5);
        let mut fx = pinned(now, &f);
        fx.visible = vec![alert("a", "Flood Advisory", false)];
        match fx.build().alerts {
            AlertsChip::Active { label, click, .. } => {
                assert_eq!(label, "Flood Advisory");
                assert_eq!(
                    click,
                    Intent::Diagnostics(DiagnosticsIntent::SelectAlert("a".into()))
                );
            }
            other => panic!("expected an active chip, got {other:?}"),
        }
    }

    #[wasm_bindgen_test]
    fn alerts_chip_is_loading_then_quiet_then_hidden_without_bounds() {
        let now = t(12, 0, 0);
        let f = displayed(now, 0.5);
        let mut fx = pinned(now, &f);
        fx.alerts.last_success_ms = 0.0;
        assert_eq!(fx.build().alerts, AlertsChip::Loading);
        fx.alerts.last_error = Some("HTTP 503".into());
        match fx.build().alerts {
            AlertsChip::Quiet { failed, hover, .. } => {
                assert!(failed);
                assert!(hover.contains("HTTP 503"));
            }
            other => panic!("expected quiet, got {other:?}"),
        }
        fx.bounds = false;
        assert_eq!(fx.build().alerts, AlertsChip::Hidden);
    }

    // ── The width rule ──────────────────────────────────────────────────

    const W: TopBarWidths = TopBarWidths {
        fixed: 700.0,
        city: 140.0,
        zone: 40.0,
        help_and_views: 110.0,
    };

    fn all() -> f32 {
        W.fixed + W.city + W.zone + W.help_and_views + FOLD_MARGIN
    }

    #[wasm_bindgen_test]
    fn a_wide_bar_keeps_everything_inline() {
        let f = fold(all(), &W);
        assert!(f.city && f.zone && f.help_and_views);
    }

    #[wasm_bindgen_test]
    fn the_city_folds_first_then_the_zone_then_help_and_views() {
        let f = fold(all() - 1.0, &W);
        assert_eq!(
            f,
            TopBarFold {
                city: false,
                zone: true,
                help_and_views: true
            }
        );
        let f = fold(all() - W.city - 1.0, &W);
        assert_eq!(
            f,
            TopBarFold {
                city: false,
                zone: false,
                help_and_views: true
            }
        );
        let f = fold(all() - W.city - W.zone - 1.0, &W);
        assert_eq!(
            f,
            TopBarFold {
                city: false,
                zone: false,
                help_and_views: false
            }
        );
        // Narrower than the fixed cluster: everything that can fold has.
        assert_eq!(fold(100.0, &W), f);
    }

    #[wasm_bindgen_test]
    fn folding_is_monotonic_as_the_bar_narrows() {
        let rank = |f: TopBarFold| f.city as u8 + f.zone as u8 + f.help_and_views as u8;
        let mut prev = rank(fold(2_000.0, &W));
        let mut w = 2_000.0;
        while w > 0.0 {
            let r = rank(fold(w, &W));
            assert!(r <= prev, "nothing reappears as the bar narrows ({w})");
            prev = r;
            w -= 5.0;
        }
    }

    #[wasm_bindgen_test]
    fn title_case_reads_registry_names() {
        assert_eq!(title_case("OKLAHOMA CITY"), "Oklahoma City");
        assert_eq!(title_case("DES  MOINES"), "Des Moines");
        assert_eq!(short_date((2025, 1, 9)), "Jan 9");
    }
}
