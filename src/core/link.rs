//! The link: what you see (one-instrument §14).
//!
//! One rule replaces the old three-way split between the URL, localStorage
//! and nothing: **the link is what you see; preferences are how you like
//! it.** This module owns the link half — a pure codec between the query
//! string and [`LinkState`], and the boot decision [`decide_boot`] that says
//! how a session opens from it. The shell reads `location.search`, calls
//! [`parse`], applies the [`BootPlan`], and on each throttled persist writes
//! [`to_query`] with `history.replaceState`. Preferences are
//! [`crate::core::prefs`].
//!
//! | In the link | Key |
//! |---|---|
//! | site | `site` |
//! | playhead time (Unix seconds) | `t` |
//! | selection: point, range, pinned or not | `sel` |
//! | play mode (Frames / Rays) | `mode` |
//! | product | `product` |
//! | tilt (an angle, or Latest) | `tilt` |
//! | map center | `lat`, `lon` |
//! | 2D/3D, camera, map and timeline zoom, volume rendering and cutoff | `v` |
//! | storm cells on, at this envelope threshold (dBZ) | `cells` |
//! | developer flag | `dev` |
//!
//! `sel` is `now` (pinned to now), `last6f` / `last1800s` (a range ending now
//! of 6 frames / 1800 s), or `START~END` (a fixed range, Unix seconds).
//! Absent, the selection is a point at `t`. A pinned selection carries no
//! `t`: it opens at now whatever the link says.
//!
//! Retired keys: `ui=basic|advanced` is ignored (depth replaced the modes,
//! §11), and the `v` blob's `rt` flag is read only as a legacy spelling of
//! `sel=now`.

use base64::Engine as _;

use crate::core::{LoopBasis, LoopPreset, PlaybackMode, ViewState};

/// The selection a link carries.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum LinkSelection {
    /// A point at the link's time, detached from now.
    Point,
    /// A point pinned to now.
    PinnedNow,
    /// A fixed range, Unix seconds, ordered.
    Range { start: f64, end: f64 },
    /// A range ending now, sized by its basis.
    PinnedRange(LoopBasis),
}

impl LinkSelection {
    /// Attached to the live edge.
    pub(crate) fn is_pinned(self) -> bool {
        matches!(
            self,
            LinkSelection::PinnedNow | LinkSelection::PinnedRange(_)
        )
    }
}

/// The tilt a link carries.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum LinkTilt {
    /// The most recent cut, whatever its angle.
    Latest,
    /// A cut by its angle in degrees.
    Angle(f32),
}

/// Everything a link says about what is on screen. Every field is optional
/// on the way in: a link names only what it names.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct LinkState {
    pub site: Option<String>,
    pub time: Option<f64>,
    pub selection: Option<LinkSelection>,
    pub play_mode: Option<PlaybackMode>,
    pub product: Option<String>,
    pub tilt: Option<LinkTilt>,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub view: ViewState,
    /// `cells=<dbz>`: storm cells on at this envelope threshold. Absent says
    /// nothing (the preference stands); a bare `cells` means the default.
    pub cells: Option<f32>,
    /// `Some(true)` for `dev=true`; wins over the saved preference for the
    /// session without being saved.
    pub dev: Option<bool>,
}

impl LinkState {
    /// A link that names nothing: opening with it is opening with no link.
    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.site.is_none() && self.time.is_none() && self.selection.is_none()
    }

    /// The selection, resolving the legacy `v.rt` flag and the default (a
    /// point at `t`).
    pub(crate) fn effective_selection(&self) -> Option<LinkSelection> {
        self.selection
            .or_else(|| (self.view.rt == Some(true)).then_some(LinkSelection::PinnedNow))
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Codec
// ───────────────────────────────────────────────────────────────────────────

/// Parse a query string (with or without the leading `?`). Unknown keys,
/// including the retired `ui`, are ignored; malformed values are dropped
/// one by one rather than failing the link.
pub(crate) fn parse(query: &str) -> LinkState {
    let mut link = LinkState::default();
    let query = query.trim_start_matches('?');
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        match key {
            "site" if !value.is_empty() => link.site = Some(value.to_uppercase()),
            "t" => link.time = value.parse().ok().filter(|t: &f64| t.is_finite()),
            "sel" => link.selection = parse_selection(value),
            "mode" => {
                link.play_mode = match value {
                    "frames" => Some(PlaybackMode::Macro),
                    "rays" => Some(PlaybackMode::Micro),
                    _ => None,
                }
            }
            "product" if !value.is_empty() => link.product = Some(value.to_string()),
            "tilt" => link.tilt = parse_tilt(value),
            "lat" => link.lat = value.parse().ok().filter(|v: &f64| v.is_finite()),
            "lon" => link.lon = value.parse().ok().filter(|v: &f64| v.is_finite()),
            "v" => {
                if let Some(view) = decode_view(value) {
                    link.view = view;
                }
            }
            "cells" => {
                link.cells = if value.is_empty() {
                    Some(crate::core::storm_cells::THRESHOLD_DEFAULT_DBZ)
                } else {
                    value.parse::<f32>().ok().filter(|v| v.is_finite())
                }
            }
            "dev" => {
                link.dev = match value {
                    "true" => Some(true),
                    "false" => Some(false),
                    _ => None,
                }
            }
            _ => {}
        }
    }
    link
}

/// Write a link as a query string with its leading `?`. A pinned selection
/// writes no `t`; `dev` is written only when on, so ordinary links stay
/// clean.
pub(crate) fn to_query(link: &LinkState) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(site) = &link.site {
        parts.push(format!("site={site}"));
    }
    let pinned = link.selection.is_some_and(LinkSelection::is_pinned);
    if let (Some(t), false) = (link.time, pinned) {
        parts.push(format!("t={t:.0}"));
    }
    if let Some(sel) = link.selection.and_then(selection_value) {
        parts.push(format!("sel={sel}"));
    }
    if let Some(mode) = link.play_mode {
        parts.push(format!(
            "mode={}",
            match mode {
                PlaybackMode::Macro => "frames",
                PlaybackMode::Micro => "rays",
            }
        ));
    }
    if let Some(product) = &link.product {
        parts.push(format!("product={product}"));
    }
    if let Some(tilt) = link.tilt {
        parts.push(format!(
            "tilt={}",
            match tilt {
                LinkTilt::Latest => "latest".to_string(),
                LinkTilt::Angle(a) => format_angle(a),
            }
        ));
    }
    if let Some(lat) = link.lat {
        parts.push(format!("lat={lat:.4}"));
    }
    if let Some(lon) = link.lon {
        parts.push(format!("lon={lon:.4}"));
    }
    if link.view != ViewState::default() {
        parts.push(format!("v={}", encode_view(&link.view)));
    }
    if let Some(dbz) = link.cells {
        parts.push(format!("cells={}", format_angle(dbz)));
    }
    if link.dev == Some(true) {
        parts.push("dev=true".to_string());
    }
    format!("?{}", parts.join("&"))
}

fn parse_selection(value: &str) -> Option<LinkSelection> {
    if value == "now" {
        return Some(LinkSelection::PinnedNow);
    }
    if let Some(rest) = value.strip_prefix("last") {
        if let Some(n) = rest.strip_suffix('f') {
            return n
                .parse::<u32>()
                .ok()
                .filter(|n| *n > 0)
                .map(|n| LinkSelection::PinnedRange(LoopBasis::FrameCount(n)));
        }
        if let Some(secs) = rest.strip_suffix('s') {
            return secs
                .parse::<f64>()
                .ok()
                .filter(|s| s.is_finite() && *s > 1.0)
                .map(|s| LinkSelection::PinnedRange(LoopBasis::Duration(s)));
        }
        return None;
    }
    let (a, b) = value.split_once('~')?;
    let (a, b) = (a.parse::<f64>().ok()?, b.parse::<f64>().ok()?);
    let (start, end) = (a.min(b), a.max(b));
    (start.is_finite() && end.is_finite() && end - start > 1.0)
        .then_some(LinkSelection::Range { start, end })
}

fn selection_value(sel: LinkSelection) -> Option<String> {
    match sel {
        LinkSelection::Point => None,
        LinkSelection::PinnedNow => Some("now".to_string()),
        LinkSelection::PinnedRange(LoopBasis::FrameCount(n)) => Some(format!("last{n}f")),
        LinkSelection::PinnedRange(LoopBasis::Duration(s)) => Some(format!("last{s:.0}s")),
        LinkSelection::Range { start, end } => Some(format!("{start:.0}~{end:.0}")),
    }
}

fn parse_tilt(value: &str) -> Option<LinkTilt> {
    if value == "latest" {
        return Some(LinkTilt::Latest);
    }
    value
        .parse::<f32>()
        .ok()
        .filter(|a| a.is_finite() && (-1.0..=90.0).contains(a))
        .map(LinkTilt::Angle)
}

/// `0.5`, `19.5`, `1` — at most two decimals, no trailing zeros (tilt
/// angles and storm cell thresholds).
fn format_angle(a: f32) -> String {
    let s = format!("{a:.2}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    s.to_string()
}

fn encode_view(view: &ViewState) -> String {
    let json = serde_json::to_vec(view).unwrap_or_default();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json)
}

fn decode_view(value: &str) -> Option<ViewState> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(value)
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

// ───────────────────────────────────────────────────────────────────────────
// Boot
// ───────────────────────────────────────────────────────────────────────────

/// Where the session opens in time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum BootStart {
    /// Pinned to now: start the stream once a site is known.
    PinnedNow,
    /// A range ending now: start the stream and loop it.
    PinnedRange(LoopPreset),
    /// Detached, the playhead at this time.
    At(f64),
    /// Detached on a fixed range, the playhead inside it.
    Range { start: f64, end: f64, playhead: f64 },
}

impl BootStart {
    /// Opens attached to the live edge.
    pub(crate) fn is_pinned(self) -> bool {
        matches!(self, BootStart::PinnedNow | BootStart::PinnedRange(_))
    }
}

/// How a session opens: the decision [`decide_boot`] makes from the link and
/// the preferred site.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct BootPlan {
    /// The site to open at. `None` opens the first-visit picker.
    pub site: Option<String>,
    /// The site came from the link (rather than the preference).
    pub site_from_link: bool,
    pub start: BootStart,
}

/// Decide how a session opens.
///
/// - No link: pinned to now at the preferred site, as before (the picker
///   first when there is none).
/// - A pinned selection opens pinned, whatever time the link carries.
/// - A time (or a fixed range) opens there, detached.
/// - A site alone opens pinned to now at that site.
///
/// `known_site` validates a site id; an unknown one in the link falls back
/// to the preferred site rather than opening on a radar that does not exist.
pub(crate) fn decide_boot(
    link: &LinkState,
    preferred_site: Option<&str>,
    known_site: impl Fn(&str) -> bool,
) -> BootPlan {
    let link_site = link.site.as_deref().filter(|s| known_site(s));
    let (site, site_from_link) = match (link_site, preferred_site) {
        (Some(s), _) => (Some(s.to_string()), true),
        (None, Some(p)) if known_site(p) => (Some(p.to_string()), false),
        _ => (None, false),
    };

    let start = match link.effective_selection() {
        Some(LinkSelection::PinnedNow) => BootStart::PinnedNow,
        Some(LinkSelection::PinnedRange(basis)) => BootStart::PinnedRange(match basis {
            LoopBasis::FrameCount(n) => LoopPreset::LastFrames(n),
            LoopBasis::Duration(s) => LoopPreset::LastDuration(s),
        }),
        Some(LinkSelection::Range { start, end }) => BootStart::Range {
            start,
            end,
            playhead: link.time.unwrap_or(start).clamp(start, end),
        },
        Some(LinkSelection::Point) | None => match link.time {
            Some(t) => BootStart::At(t),
            None => BootStart::PinnedNow,
        },
    };

    BootPlan {
        site,
        site_from_link,
        start,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn known(s: &str) -> bool {
        matches!(s, "KTLX" | "KDMX" | "KOAX")
    }

    fn full() -> LinkState {
        LinkState {
            site: Some("KTLX".into()),
            time: Some(1_700_000_000.0),
            selection: Some(LinkSelection::Range {
                start: 1_699_999_000.0,
                end: 1_700_000_500.0,
            }),
            play_mode: Some(PlaybackMode::Micro),
            product: Some("VEL".into()),
            tilt: Some(LinkTilt::Angle(2.4)),
            lat: Some(35.3331),
            lon: Some(-97.2778),
            view: ViewState {
                vm: Some(1),
                cd: Some(2.5),
                ct: Some(30.0),
                v3d: Some(true),
                vdc: Some(12.5),
                ..Default::default()
            },
            cells: Some(40.0),
            dev: Some(true),
        }
    }

    #[wasm_bindgen_test]
    fn storm_cells_travel_as_their_threshold() {
        let link = LinkState {
            cells: Some(35.0),
            ..Default::default()
        };
        let q = to_query(&link);
        assert_eq!(q, "?cells=35");
        assert_eq!(parse(&q).cells, Some(35.0));
        assert_eq!(parse("?cells=42.5").cells, Some(42.5));
        // Off is the absence of the key.
        assert!(!to_query(&LinkState::default()).contains("cells"));
        assert_eq!(parse("?site=KDMX").cells, None);
        // A bare key means on at the default; junk is dropped.
        assert_eq!(
            parse("?cells").cells,
            Some(crate::core::storm_cells::THRESHOLD_DEFAULT_DBZ)
        );
        assert_eq!(parse("?cells=lots").cells, None);
        assert_eq!(parse("?cells=NaN").cells, None);
    }

    // ── Codec ──────────────────────────────────────────────────────────────

    #[wasm_bindgen_test]
    fn round_trips_every_field() {
        let link = full();
        assert_eq!(parse(&to_query(&link)), link);
    }

    #[wasm_bindgen_test]
    fn round_trips_each_selection() {
        for sel in [
            LinkSelection::PinnedNow,
            LinkSelection::PinnedRange(LoopBasis::FrameCount(6)),
            LinkSelection::PinnedRange(LoopBasis::Duration(1800.0)),
            LinkSelection::Range {
                start: 100.0,
                end: 900.0,
            },
        ] {
            let link = LinkState {
                site: Some("KDMX".into()),
                selection: Some(sel),
                ..Default::default()
            };
            assert_eq!(parse(&to_query(&link)).selection, Some(sel), "{sel:?}");
        }
    }

    #[wasm_bindgen_test]
    fn a_point_is_the_absence_of_sel() {
        let link = LinkState {
            time: Some(500.0),
            selection: Some(LinkSelection::Point),
            ..Default::default()
        };
        let q = to_query(&link);
        assert!(!q.contains("sel="), "{q}");
        assert!(q.contains("t=500"), "{q}");
    }

    #[wasm_bindgen_test]
    fn a_pinned_selection_writes_no_time() {
        let link = LinkState {
            site: Some("KTLX".into()),
            time: Some(1_700_000_000.0),
            selection: Some(LinkSelection::PinnedNow),
            ..Default::default()
        };
        let q = to_query(&link);
        assert!(!q.contains("t="), "{q}");
        assert!(q.contains("sel=now"), "{q}");
    }

    #[wasm_bindgen_test]
    fn ui_is_retired_and_ignored() {
        for q in ["?ui=basic", "?ui=advanced", "?site=KTLX&ui=basic"] {
            let link = parse(q);
            let without = parse(&q.replace("ui=basic", "").replace("ui=advanced", ""));
            assert_eq!(link, without, "{q}");
        }
        assert!(parse("?ui=basic").is_empty());
    }

    #[wasm_bindgen_test]
    fn dev_flag_is_written_only_when_on() {
        let mut link = LinkState::default();
        link.dev = Some(false);
        assert!(!to_query(&link).contains("dev"));
        link.dev = Some(true);
        assert!(to_query(&link).contains("dev=true"));
        assert_eq!(parse("?dev=true").dev, Some(true));
        assert_eq!(parse("?dev=false").dev, Some(false));
        assert_eq!(parse("?dev=1").dev, None);
    }

    #[wasm_bindgen_test]
    fn tilt_spellings() {
        assert_eq!(parse("?tilt=latest").tilt, Some(LinkTilt::Latest));
        assert_eq!(parse("?tilt=0.5").tilt, Some(LinkTilt::Angle(0.5)));
        assert_eq!(parse("?tilt=abc").tilt, None);
        assert_eq!(parse("?tilt=400").tilt, None);
        assert_eq!(format_angle(0.5), "0.5");
        assert_eq!(format_angle(19.5), "19.5");
        assert_eq!(format_angle(1.0), "1");
        assert_eq!(format_angle(2.42), "2.42");
    }

    #[wasm_bindgen_test]
    fn malformed_values_drop_one_by_one() {
        let link = parse("?site=ktlx&t=soon&sel=whenever&mode=fast&v=!!!&lat=x");
        assert_eq!(link.site.as_deref(), Some("KTLX"));
        assert_eq!(link.time, None);
        assert_eq!(link.selection, None);
        assert_eq!(link.play_mode, None);
        assert_eq!(link.view, ViewState::default());
        assert_eq!(link.lat, None);
    }

    #[wasm_bindgen_test]
    fn degenerate_ranges_are_rejected() {
        assert_eq!(parse("?sel=100~100").selection, None);
        assert_eq!(parse("?sel=last0f").selection, None);
        assert_eq!(parse("?sel=last0s").selection, None);
        // Reversed endpoints are ordered.
        assert_eq!(
            parse("?sel=900~100").selection,
            Some(LinkSelection::Range {
                start: 100.0,
                end: 900.0
            })
        );
    }

    #[wasm_bindgen_test]
    fn legacy_links_still_open() {
        // The pre-split URL: site, t, product, lat/lon, and `v` with `rt`.
        let view = ViewState {
            rt: Some(true),
            mz: Some(1.0),
            ..Default::default()
        };
        let q = format!(
            "?site=KTLX&t=1700000000&product=REF&lat=35.3&lon=-97.3&v={}",
            encode_view(&view)
        );
        let link = parse(&q);
        assert_eq!(link.effective_selection(), Some(LinkSelection::PinnedNow));
        assert_eq!(link.product.as_deref(), Some("REF"));
        assert_eq!(link.view.mz, Some(1.0));
    }

    #[wasm_bindgen_test]
    fn default_view_writes_no_v() {
        let link = LinkState {
            site: Some("KTLX".into()),
            ..Default::default()
        };
        assert_eq!(to_query(&link), "?site=KTLX");
    }

    // ── Boot ───────────────────────────────────────────────────────────────

    #[wasm_bindgen_test]
    fn no_link_opens_pinned_live_at_the_preferred_site() {
        let plan = decide_boot(&parse(""), Some("KDMX"), known);
        assert_eq!(plan.site.as_deref(), Some("KDMX"));
        assert!(!plan.site_from_link);
        assert_eq!(plan.start, BootStart::PinnedNow);
    }

    #[wasm_bindgen_test]
    fn no_link_and_no_preference_asks_for_a_site_then_pins() {
        let plan = decide_boot(&parse(""), None, known);
        assert_eq!(plan.site, None);
        assert_eq!(plan.start, BootStart::PinnedNow);
    }

    #[wasm_bindgen_test]
    fn a_time_opens_there_detached() {
        let plan = decide_boot(&parse("?site=KTLX&t=1700000000"), Some("KDMX"), known);
        assert_eq!(plan.site.as_deref(), Some("KTLX"));
        assert!(plan.site_from_link);
        assert_eq!(plan.start, BootStart::At(1_700_000_000.0));
        assert!(!plan.start.is_pinned());
    }

    #[wasm_bindgen_test]
    fn a_pinned_selection_opens_pinned_whatever_the_time() {
        let plan = decide_boot(&parse("?site=KTLX&t=1700000000&sel=now"), None, known);
        assert_eq!(plan.start, BootStart::PinnedNow);
        let plan = decide_boot(&parse("?site=KTLX&t=1700000000&sel=last6f"), None, known);
        assert_eq!(
            plan.start,
            BootStart::PinnedRange(LoopPreset::LastFrames(6))
        );
        let plan = decide_boot(&parse("?sel=last3600s"), Some("KOAX"), known);
        assert_eq!(
            plan.start,
            BootStart::PinnedRange(LoopPreset::LastDuration(3600.0))
        );
        assert!(plan.start.is_pinned());
    }

    #[wasm_bindgen_test]
    fn a_fixed_range_opens_detached_with_the_playhead_inside() {
        let plan = decide_boot(&parse("?site=KTLX&sel=1000~2000&t=1500"), None, known);
        assert_eq!(
            plan.start,
            BootStart::Range {
                start: 1000.0,
                end: 2000.0,
                playhead: 1500.0
            }
        );
        // A time outside the range is clamped; no time starts at the start.
        let plan = decide_boot(&parse("?sel=1000~2000&t=5000"), None, known);
        assert!(matches!(plan.start, BootStart::Range { playhead, .. } if playhead == 2000.0));
        let plan = decide_boot(&parse("?sel=1000~2000"), None, known);
        assert!(matches!(plan.start, BootStart::Range { playhead, .. } if playhead == 1000.0));
    }

    #[wasm_bindgen_test]
    fn a_site_alone_opens_pinned_there() {
        let plan = decide_boot(&parse("?site=KOAX"), Some("KDMX"), known);
        assert_eq!(plan.site.as_deref(), Some("KOAX"));
        assert_eq!(plan.start, BootStart::PinnedNow);
    }

    #[wasm_bindgen_test]
    fn an_unknown_site_falls_back_to_the_preference() {
        let plan = decide_boot(&parse("?site=ZZZZ"), Some("KDMX"), known);
        assert_eq!(plan.site.as_deref(), Some("KDMX"));
        assert!(!plan.site_from_link);
        let plan = decide_boot(&parse("?site=ZZZZ"), None, known);
        assert_eq!(plan.site, None);
    }

    #[wasm_bindgen_test]
    fn ui_basic_changes_nothing_about_the_boot() {
        assert_eq!(
            decide_boot(&parse("?ui=basic"), Some("KDMX"), known),
            decide_boot(&parse(""), Some("KDMX"), known)
        );
    }

    #[wasm_bindgen_test]
    fn legacy_rt_opens_pinned_and_legacy_t_opens_detached() {
        let pinned = ViewState {
            rt: Some(true),
            ..Default::default()
        };
        let q = format!("?site=KTLX&t=1700000000&v={}", encode_view(&pinned));
        assert_eq!(
            decide_boot(&parse(&q), None, known).start,
            BootStart::PinnedNow
        );
        let q = format!(
            "?site=KTLX&t=1700000000&v={}",
            encode_view(&ViewState::default())
        );
        assert_eq!(
            decide_boot(&parse(&q), None, known).start,
            BootStart::At(1_700_000_000.0)
        );
    }
}
