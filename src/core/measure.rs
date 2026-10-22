//! The distance tool's measurement: its two endpoints, snapping an endpoint
//! to a storm track or to "My location", following what an endpoint is
//! snapped to, and the readout.
//!
//! - **Placing.** A click places the next endpoint (start, then end, then a
//!   new start: [`crate::core::canvas::decide_distance_click`]). Dragging an
//!   endpoint marker moves it ([`grab`] says which one a press picks up).
//!   Where an endpoint lands, [`snap`] decides what it holds on to: the
//!   nearest lettered track's centroid within [`snap_radius_km`] (Tracks on),
//!   else the "My location" dot within it, else the map point itself. Only
//!   lettered tracks are in the tracks view, so a track seen once (no
//!   letter yet) never snaps.
//! - **Following.** Each frame, after the tracks sync, [`follow`] moves a
//!   snapped endpoint to its track's centroid on the shown volume (scrubbing
//!   or live), or to the current location fix. A coasting track keeps the
//!   snap (its position is advected). A track missing from a final (not
//!   provisional) volume has ended, or the volume is before it had a
//!   letter: the endpoint lets go and stays where the track was last seen,
//!   drawn muted with the letter ([`Anchor::Ended`]). Turning Tracks off
//!   lets every track endpoint go the same way; a site change does too
//!   ([`Measurement::release_tracks`]), since letters restart there.
//!   Dragging a snapped endpoint away unsnaps it.
//! - **Readout** ([`view`]): the distance, then, when one end follows a
//!   track and the other is fixed, the track's closest approach to it (time
//!   withheld while the motion's confidence is below the forecast arrow's
//!   threshold), and when both ends follow tracks, whether they converge or
//!   diverge and how fast.
//!
//! Positions are `(lat, lon)`. Nothing here touches egui or the browser.

use crate::core::canvas::{decide_distance_click, haversine_km, DistancePlacement};
use crate::core::storm_tracks::{TrackVm, TracksView};
use crate::storm_cells::track::{
    closest_approach, distance_km, pair_closest_approach, separation_rate_kmh, Approach, Motion,
    MIN_FORECAST_CONFIDENCE,
};

/// Snap radius around a target, in screen pixels: a little more than a
/// path dot and its halo on the desktop, a fingertip on the phone.
pub(crate) const SNAP_RADIUS_PX_DESKTOP: f64 = 16.0;
pub(crate) const SNAP_RADIUS_PX_PHONE: f64 = 24.0;

/// Closest approach beyond this many minutes is withheld: extrapolation
/// cannot see growth or decay that far out. Same horizon as the cell card.
pub(crate) const APPROACH_HORIZON_MIN: f64 = crate::core::storm_cell_view::APPROACH_HORIZON_MIN;

/// Below this rate (km/h) two tracks are "holding about the same distance".
pub(crate) const STEADY_RATE_KMH: f64 = 2.0;

/// The snap radius in km for the map's current scale.
pub(crate) fn snap_radius_km(phone: bool, km_per_px: f64) -> f64 {
    let px = if phone {
        SNAP_RADIUS_PX_PHONE
    } else {
        SNAP_RADIUS_PX_DESKTOP
    };
    if km_per_px.is_finite() && km_per_px > 0.0 {
        px * km_per_px
    } else {
        0.0
    }
}

/// What an endpoint holds on to.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Anchor {
    /// A fixed map point.
    Point,
    /// The "My location" fix; follows it.
    MyLocation,
    /// A lettered storm track; follows its centroid.
    Track(String),
    /// A track that ended (or was let go): fixed where it was last seen,
    /// drawn muted with its letter.
    Ended(String),
}

/// One end of the measurement.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Endpoint {
    pub lat: f64,
    pub lon: f64,
    pub anchor: Anchor,
}

impl Endpoint {
    fn pos(&self) -> (f64, f64) {
        (self.lat, self.lon)
    }

    fn point(lat: f64, lon: f64) -> Self {
        Self {
            lat,
            lon,
            anchor: Anchor::Point,
        }
    }
}

/// Which end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum End {
    Start,
    End,
}

/// The measurement on the map.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Measurement {
    pub start: Option<Endpoint>,
    pub end: Option<Endpoint>,
    /// The endpoint being dragged.
    pub dragging: Option<End>,
}

impl Measurement {
    fn slot(&mut self, end: End) -> &mut Option<Endpoint> {
        match end {
            End::Start => &mut self.start,
            End::End => &mut self.end,
        }
    }

    fn ends_mut(&mut self) -> impl Iterator<Item = &mut Endpoint> {
        self.start.iter_mut().chain(self.end.iter_mut())
    }

    /// Whether any endpoint follows something (a track or the location).
    pub(crate) fn follows_anything(&self) -> bool {
        self.start
            .iter()
            .chain(self.end.iter())
            .any(|e| matches!(e.anchor, Anchor::Track(_) | Anchor::MyLocation))
    }

    /// Let every track endpoint go where it stands (a site change: letters
    /// restart, so a letter would name another storm).
    pub(crate) fn release_tracks(&mut self) {
        for e in self.ends_mut() {
            if let Anchor::Track(label) = &e.anchor {
                e.anchor = Anchor::Ended(label.clone());
            }
        }
    }
}

/// What an endpoint can snap to this frame.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SnapTargets<'a> {
    /// The lettered tracks on the shown volume; `None` when Tracks (or
    /// storm cells) are off.
    pub tracks: Option<&'a TracksView>,
    /// The "My location" fix, when that layer is on and has one.
    pub my_location: Option<(f64, f64)>,
}

/// What a point within the snap radius lands on, nearest target first.
pub(crate) fn snap(lat: f64, lon: f64, radius_km: f64, targets: &SnapTargets<'_>) -> Endpoint {
    let p = (lat, lon);
    let mut best: Option<(f64, Endpoint)> = None;
    let mut consider = |d: f64, e: Endpoint| {
        if d <= radius_km && best.as_ref().is_none_or(|(bd, _)| d < *bd) {
            best = Some((d, e));
        }
    };
    for t in targets.tracks.iter().flat_map(|v| v.tracks.iter()) {
        consider(
            distance_km(p, (t.lat, t.lon)),
            Endpoint {
                lat: t.lat,
                lon: t.lon,
                anchor: Anchor::Track(t.label.clone()),
            },
        );
    }
    if let Some(me) = targets.my_location {
        consider(
            distance_km(p, me),
            Endpoint {
                lat: me.0,
                lon: me.1,
                anchor: Anchor::MyLocation,
            },
        );
    }
    best.map_or_else(|| Endpoint::point(lat, lon), |(_, e)| e)
}

/// The endpoint a press at `(lat, lon)` picks up, if one is within
/// `radius_km` (the nearer when both are).
pub(crate) fn grab(m: &Measurement, lat: f64, lon: f64, radius_km: f64) -> Option<End> {
    let p = (lat, lon);
    [(End::Start, &m.start), (End::End, &m.end)]
        .into_iter()
        .filter_map(|(end, e)| Some((end, distance_km(p, e.as_ref()?.pos()))))
        .filter(|&(_, d)| d <= radius_km)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(end, _)| end)
}

/// What the user did with the distance tool.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum MeasureIntent {
    /// A click: place the next endpoint. `km_per_px` is the map's scale
    /// there, for the snap radius.
    Place { lat: f64, lon: f64, km_per_px: f64 },
    /// A drag starts at `(lat, lon)`: pick up the endpoint under it, if any.
    DragStart { lat: f64, lon: f64, km_per_px: f64 },
    /// The dragged endpoint moves (unsnapped while it moves).
    DragTo { lat: f64, lon: f64 },
    /// The drag ends at `(lat, lon)`: the endpoint snaps there. A point
    /// that is not finite (the shell lost the pointer) only ends the drag.
    DragEnd { lat: f64, lon: f64, km_per_px: f64 },
}

/// Apply one intent. A point that is not finite is ignored (bar ending a
/// drag).
pub(crate) fn reduce(
    m: &mut Measurement,
    intent: MeasureIntent,
    targets: &SnapTargets<'_>,
    phone: bool,
) {
    let finite = |lat: f64, lon: f64| lat.is_finite() && lon.is_finite();
    match intent {
        MeasureIntent::Place {
            lat,
            lon,
            km_per_px,
        } if finite(lat, lon) => {
            let e = snap(lat, lon, snap_radius_km(phone, km_per_px), targets);
            match decide_distance_click(m.start.is_some(), m.end.is_some()) {
                DistancePlacement::Start => {
                    m.start = Some(e);
                    m.end = None;
                }
                DistancePlacement::End => m.end = Some(e),
            }
        }
        MeasureIntent::DragStart {
            lat,
            lon,
            km_per_px,
        } if finite(lat, lon) => {
            m.dragging = grab(m, lat, lon, snap_radius_km(phone, km_per_px));
        }
        MeasureIntent::DragTo { lat, lon } if finite(lat, lon) => {
            if let Some(end) = m.dragging {
                if let Some(e) = m.slot(end) {
                    *e = Endpoint::point(lat, lon);
                }
            }
        }
        MeasureIntent::DragEnd {
            lat,
            lon,
            km_per_px,
        } => {
            if let Some(end) = m.dragging.take() {
                if finite(lat, lon) {
                    let e = snap(lat, lon, snap_radius_km(phone, km_per_px), targets);
                    if let Some(slot) = m.slot(end) {
                        *slot = e;
                    }
                }
            }
        }
        _ => {}
    }
}

/// Move the snapped endpoints to what they follow on the shown volume.
/// `tracks_on` is storm cells and Tracks both on; `tracks` is the shown
/// volume's tracks view (`None` while it has none yet: held). Returns
/// whether anything changed.
pub(crate) fn follow(
    m: &mut Measurement,
    tracks_on: bool,
    tracks: Option<&TracksView>,
    my_location: Option<(f64, f64)>,
) -> bool {
    let before = m.clone();
    let dragging = m.dragging;
    for (end, e) in [(End::Start, &mut m.start), (End::End, &mut m.end)] {
        let Some(e) = e.as_mut() else { continue };
        if dragging == Some(end) {
            continue;
        }
        match &e.anchor {
            Anchor::Track(label) => {
                if !tracks_on {
                    e.anchor = Anchor::Ended(label.clone());
                    continue;
                }
                let Some(view) = tracks else { continue };
                match view.tracks.iter().find(|t| &t.label == label) {
                    Some(t) => (e.lat, e.lon) = (t.lat, t.lon),
                    // A live preview may not have matched it yet.
                    None if view.provisional => {}
                    None => e.anchor = Anchor::Ended(label.clone()),
                }
            }
            Anchor::MyLocation => match my_location {
                Some((lat, lon)) => (e.lat, e.lon) = (lat, lon),
                None => e.anchor = Anchor::Point,
            },
            Anchor::Point | Anchor::Ended(_) => {}
        }
    }
    *m != before
}

// ───────────────────────────────────────────────────────────────────────────
// View-model
// ───────────────────────────────────────────────────────────────────────────

/// One endpoint marker.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct EndVm {
    pub lat: f64,
    pub lon: f64,
    /// The track letter beside the marker.
    pub label: Option<String>,
    /// An ended track's endpoint: drawn muted.
    pub muted: bool,
}

/// The target a click (or drop) at the pointer would snap to, ringed.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SnapHintVm {
    pub lat: f64,
    pub lon: f64,
}

/// What the distance tool draws.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct MeasureVm {
    pub ends: Vec<EndVm>,
    /// The line, start to end, when both are placed.
    pub line: Option<((f64, f64), (f64, f64))>,
    /// The readout at the line's middle, one entry per line of text.
    pub readout: Vec<String>,
    pub snap_hint: Option<SnapHintVm>,
}

/// What [`view`] reads.
pub(crate) struct MeasureViewInputs<'a> {
    pub measurement: &'a Measurement,
    pub tool_active: bool,
    pub targets: SnapTargets<'a>,
    /// The pointer over the map, for the snap hint.
    pub hover: Option<(f64, f64)>,
    /// The snap radius at the current scale ([`snap_radius_km`]).
    pub snap_km: f64,
}

/// The measurement view-model.
pub(crate) fn view(i: &MeasureViewInputs<'_>) -> MeasureVm {
    let m = i.measurement;
    let mut vm = MeasureVm::default();
    for e in m.start.iter().chain(m.end.iter()) {
        let (label, muted) = match &e.anchor {
            Anchor::Track(l) => (Some(l.clone()), false),
            Anchor::Ended(l) => (Some(l.clone()), true),
            Anchor::Point | Anchor::MyLocation => (None, false),
        };
        vm.ends.push(EndVm {
            lat: e.lat,
            lon: e.lon,
            label,
            muted,
        });
    }
    if let (Some(a), Some(b)) = (&m.start, &m.end) {
        vm.line = Some((a.pos(), b.pos()));
        vm.readout = readout(a, b, i.targets.tracks);
    }
    // The hint previews where a click (or the drop) would snap.
    let placing = i.tool_active && (m.dragging.is_some() || grab_hover(i).is_none());
    if placing {
        if let Some((lat, lon)) = i.hover {
            let e = snap(lat, lon, i.snap_km, &i.targets);
            if e.anchor != Anchor::Point {
                vm.snap_hint = Some(SnapHintVm {
                    lat: e.lat,
                    lon: e.lon,
                });
            }
        }
    }
    vm
}

/// The endpoint the pointer would pick up (no hint over a grabbable end).
fn grab_hover(i: &MeasureViewInputs<'_>) -> Option<End> {
    let (lat, lon) = i.hover?;
    grab(i.measurement, lat, lon, i.snap_km)
}

/// Whether the pointer at `(lat, lon)` is over an endpoint it can drag,
/// for the shell's cursor.
pub(crate) fn can_grab(m: &Measurement, tool_active: bool, lat: f64, lon: f64, km: f64) -> bool {
    tool_active && (m.dragging.is_some() || grab(m, lat, lon, km).is_some())
}

/// The distance line: `12.3 km / 6.6 nm / 7.6 mi`.
pub(crate) fn distance_text(a: (f64, f64), b: (f64, f64)) -> String {
    let km = haversine_km(a.0, a.1, b.0, b.1);
    format!(
        "{:.1} km / {:.1} nm / {:.1} mi",
        km,
        km * 0.539957,
        km * 0.621371
    )
}

/// The readout for a finished measurement.
fn readout(a: &Endpoint, b: &Endpoint, tracks: Option<&TracksView>) -> Vec<String> {
    let mut lines = vec![distance_text(a.pos(), b.pos())];
    let live = |e: &Endpoint| match &e.anchor {
        Anchor::Track(l) => tracks?.tracks.iter().find(|t| &t.label == l),
        _ => None,
    };
    match (live(a), live(b)) {
        (Some(ta), Some(tb)) => lines.push(pair_text(ta, tb)),
        (Some(t), None) => lines.push(approach_text(t, b.pos())),
        (None, Some(t)) => lines.push(approach_text(t, a.pos())),
        (None, None) => {}
    }
    for e in [a, b] {
        if let Anchor::Ended(l) = &e.anchor {
            lines.push(format!(
                "Track {l} ended: point stays where it was last seen"
            ));
        }
    }
    lines
}

/// A track's motion as a vector.
fn motion(t: &TrackVm) -> Motion {
    Motion::toward(t.direction_deg, t.speed_kmh, t.confidence)
}

/// Whether the track's motion is confident enough to forecast from (the
/// forecast arrow's threshold).
fn confident(t: &TrackVm) -> bool {
    t.confidence >= MIN_FORECAST_CONFIDENCE
}

/// Kilometres, one decimal under 10 km.
fn km_text(km: f64) -> String {
    if km < 10.0 {
        format!("{km:.1} km")
    } else {
        format!("{km:.0} km")
    }
}

/// `high` / `medium` confidence (only shown once it is confident).
fn confidence_word(c: f64) -> &'static str {
    if c >= 0.75 {
        "high"
    } else {
        "medium"
    }
}

/// A track to a fixed point: `Closest approach 4.2 km in 18 min · high
/// confidence`, or why not.
pub(crate) fn approach_text(t: &TrackVm, point: (f64, f64)) -> String {
    if t.stationary {
        return format!("{} is stationary", t.label);
    }
    if !confident(t) {
        return "Closest approach: motion not known yet".to_string();
    }
    let a: Approach = closest_approach((t.lat, t.lon), &motion(t), point);
    if a.minutes < 1.0 {
        "Moving away: closest approach has passed".to_string()
    } else if a.minutes > APPROACH_HORIZON_MIN {
        "Closest approach more than an hour out".to_string()
    } else {
        format!(
            "Closest approach {} in {:.0} min \u{00B7} {} confidence",
            km_text(a.distance_km),
            a.minutes,
            confidence_word(t.confidence)
        )
    }
}

/// Two tracks: `Converging at 12 km/h · closest 3.1 km in 25 min`,
/// `Diverging at 8 km/h`, `Holding about the same distance`, or why not.
pub(crate) fn pair_text(a: &TrackVm, b: &TrackVm) -> String {
    if !confident(a) || !confident(b) {
        return format!("{} and {}: motion not known yet", a.label, b.label);
    }
    let (pa, pb) = ((a.lat, a.lon), (b.lat, b.lon));
    let (ma, mb) = (motion(a), motion(b));
    let rate = separation_rate_kmh(pa, &ma, pb, &mb);
    if rate.abs() < STEADY_RATE_KMH {
        return "Holding about the same distance".to_string();
    }
    if rate > 0.0 {
        return format!("Diverging at {rate:.0} km/h");
    }
    let p = pair_closest_approach(pa, &ma, pb, &mb);
    if p.minutes >= 1.0 && p.minutes <= APPROACH_HORIZON_MIN {
        format!(
            "Converging at {:.0} km/h \u{00B7} closest {} in {:.0} min",
            -rate,
            km_text(p.distance_km),
            p.minutes
        )
    } else {
        format!("Converging at {:.0} km/h", -rate)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storm_cells::track::displace;
    use wasm_bindgen_test::wasm_bindgen_test;

    const ORIGIN: (f64, f64) = (41.5, -94.0);
    /// 1 km per pixel: the desktop snap radius is 16 km.
    const KM_PER_PX: f64 = 1.0;

    fn at(east: f64, north: f64) -> (f64, f64) {
        displace(ORIGIN, east, north)
    }

    fn track(label: &str, pos: (f64, f64), dir: f64, speed: f64, conf: f64) -> TrackVm {
        TrackVm {
            id: 0,
            label: label.to_string(),
            cell_id: Some(0),
            lat: pos.0,
            lon: pos.1,
            trail: vec![pos],
            split_origin: None,
            forecast: None,
            speed_kmh: speed,
            direction_deg: dir,
            confidence: conf,
            age_volumes: 5,
            age_minutes: 20.0,
            peak_trend_db: None,
            stationary: false,
            coasting: false,
            from_reference: None,
            approach: None,
        }
    }

    fn tracks(ts: Vec<TrackVm>) -> TracksView {
        TracksView {
            provisional: false,
            tracks: ts,
        }
    }

    fn place(m: &mut Measurement, p: (f64, f64), targets: &SnapTargets<'_>) {
        reduce(
            m,
            MeasureIntent::Place {
                lat: p.0,
                lon: p.1,
                km_per_px: KM_PER_PX,
            },
            targets,
            false,
        );
    }

    #[wasm_bindgen_test]
    fn snap_radius_scales_with_the_map_and_is_larger_on_the_phone() {
        assert_eq!(snap_radius_km(false, 0.5), 8.0);
        assert_eq!(snap_radius_km(true, 0.5), 12.0);
        assert_eq!(snap_radius_km(false, f64::NAN), 0.0);
    }

    #[wasm_bindgen_test]
    fn a_click_near_a_lettered_track_snaps_to_its_centroid() {
        let v = tracks(vec![track("B", at(30.0, 0.0), 90.0, 40.0, 0.9)]);
        let targets = SnapTargets {
            tracks: Some(&v),
            my_location: None,
        };
        let mut m = Measurement::default();
        place(&mut m, at(0.0, 0.0), &targets);
        assert_eq!(m.start.as_ref().unwrap().anchor, Anchor::Point);
        // 10 km off the centroid, inside the 16 km radius.
        place(&mut m, at(30.0, 10.0), &targets);
        let end = m.end.as_ref().unwrap();
        assert_eq!(end.anchor, Anchor::Track("B".into()));
        assert_eq!((end.lat, end.lon), at(30.0, 0.0));
        // The phone's radius is wider: 20 km off snaps there, not here.
        let far = at(30.0, 20.0);
        assert_eq!(snap(far.0, far.1, 16.0, &targets).anchor, Anchor::Point);
        assert_eq!(
            snap(far.0, far.1, snap_radius_km(true, KM_PER_PX), &targets).anchor,
            Anchor::Track("B".into())
        );
    }

    #[wasm_bindgen_test]
    fn nothing_snaps_with_tracks_off_or_on_an_unlettered_track() {
        // Tracks off: the shell passes no view.
        let mut m = Measurement::default();
        place(&mut m, at(30.0, 1.0), &SnapTargets::default());
        assert_eq!(m.start.as_ref().unwrap().anchor, Anchor::Point);
        // A track seen once has no letter and is not in the view; only the
        // lettered one is a target, and it is out of reach.
        let v = tracks(vec![track("A", at(80.0, 0.0), 90.0, 40.0, 0.9)]);
        let targets = SnapTargets {
            tracks: Some(&v),
            my_location: None,
        };
        let e = snap(at(30.0, 1.0).0, at(30.0, 1.0).1, 16.0, &targets);
        assert_eq!(e.anchor, Anchor::Point);
    }

    #[wasm_bindgen_test]
    fn the_nearest_target_wins_and_my_location_snaps_too() {
        let v = tracks(vec![
            track("A", at(10.0, 0.0), 90.0, 40.0, 0.9),
            track("B", at(0.0, 6.0), 90.0, 40.0, 0.9),
        ]);
        let targets = SnapTargets {
            tracks: Some(&v),
            my_location: Some(at(0.0, -3.0)),
        };
        let p = at(0.0, 0.0);
        assert_eq!(snap(p.0, p.1, 16.0, &targets).anchor, Anchor::MyLocation);
        let p = at(0.0, 5.0);
        assert_eq!(
            snap(p.0, p.1, 16.0, &targets).anchor,
            Anchor::Track("B".into())
        );
    }

    #[wasm_bindgen_test]
    fn a_snapped_endpoint_follows_its_track_across_volumes() {
        let mut m = Measurement::default();
        let v1 = tracks(vec![track("C", at(0.0, 0.0), 90.0, 36.0, 0.9)]);
        let t1 = SnapTargets {
            tracks: Some(&v1),
            my_location: None,
        };
        place(&mut m, at(-40.0, 0.0), &t1);
        place(&mut m, at(1.0, 1.0), &t1);
        assert_eq!(m.end.as_ref().unwrap().anchor, Anchor::Track("C".into()));
        // Next volume: C moved 3 km east; the end follows, the start stays.
        let v2 = tracks(vec![track("C", at(3.0, 0.0), 90.0, 36.0, 0.9)]);
        assert!(follow(&mut m, true, Some(&v2), None));
        assert_eq!(m.end.as_ref().map(|e| (e.lat, e.lon)), Some(at(3.0, 0.0)));
        assert_eq!(
            m.start.as_ref().map(|e| (e.lat, e.lon)),
            Some(at(-40.0, 0.0))
        );
        // And the next: coasting keeps the snap.
        let mut c = track("C", at(6.0, 0.0), 90.0, 36.0, 0.9);
        c.coasting = true;
        c.cell_id = None;
        let v3 = tracks(vec![c]);
        follow(&mut m, true, Some(&v3), None);
        let end = m.end.as_ref().unwrap();
        assert_eq!(
            (end.anchor.clone(), (end.lat, end.lon)),
            (Anchor::Track("C".into()), at(6.0, 0.0))
        );
        // No view yet (cells loading): held, nothing changes.
        assert!(!follow(&mut m, true, None, None));
        // Scrubbing back follows back.
        follow(&mut m, true, Some(&v1), None);
        assert_eq!(m.end.as_ref().map(|e| (e.lat, e.lon)), Some(at(0.0, 0.0)));
    }

    #[wasm_bindgen_test]
    fn an_ended_track_lets_go_where_it_was_last_seen() {
        let mut m = Measurement::default();
        let v1 = tracks(vec![track("D", at(0.0, 0.0), 90.0, 36.0, 0.9)]);
        let t1 = SnapTargets {
            tracks: Some(&v1),
            my_location: None,
        };
        place(&mut m, at(0.0, 0.0), &t1);
        // A provisional (live) preview without D holds the snap.
        let mut preview = tracks(vec![]);
        preview.provisional = true;
        assert!(!follow(&mut m, true, Some(&preview), None));
        // A final volume without D: it ended.
        follow(&mut m, true, Some(&tracks(vec![])), None);
        let s = m.start.as_ref().unwrap();
        assert_eq!(s.anchor, Anchor::Ended("D".into()));
        assert_eq!((s.lat, s.lon), at(0.0, 0.0));
        // It stays put even if a later view has a D again.
        follow(&mut m, true, Some(&v1), None);
        assert_eq!(m.start.as_ref().unwrap().anchor, Anchor::Ended("D".into()));
        let vm = view(&MeasureViewInputs {
            measurement: &m,
            tool_active: true,
            targets: SnapTargets::default(),
            hover: None,
            snap_km: 16.0,
        });
        assert_eq!(
            vm.ends,
            vec![EndVm {
                lat: at(0.0, 0.0).0,
                lon: at(0.0, 0.0).1,
                label: Some("D".into()),
                muted: true,
            }]
        );
    }

    #[wasm_bindgen_test]
    fn tracks_off_or_a_site_change_lets_go_too() {
        let v = tracks(vec![track("E", at(0.0, 0.0), 90.0, 36.0, 0.9)]);
        let targets = SnapTargets {
            tracks: Some(&v),
            my_location: None,
        };
        let mut m = Measurement::default();
        place(&mut m, at(0.0, 0.0), &targets);
        let mut m2 = m.clone();
        follow(&mut m, false, None, None);
        assert_eq!(m.start.as_ref().unwrap().anchor, Anchor::Ended("E".into()));
        m2.release_tracks();
        assert_eq!(m2.start.as_ref().unwrap().anchor, Anchor::Ended("E".into()));
        assert!(!m2.follows_anything());
    }

    #[wasm_bindgen_test]
    fn my_location_endpoint_follows_the_fix_and_lets_go_without_one() {
        let me = at(0.0, 0.0);
        let targets = SnapTargets {
            tracks: None,
            my_location: Some(me),
        };
        let mut m = Measurement::default();
        place(&mut m, at(1.0, 1.0), &targets);
        assert_eq!(m.start.as_ref().unwrap().anchor, Anchor::MyLocation);
        follow(&mut m, false, None, Some(at(0.5, 0.0)));
        assert_eq!(m.start.as_ref().map(|e| (e.lat, e.lon)), Some(at(0.5, 0.0)));
        follow(&mut m, false, None, None);
        assert_eq!(m.start.as_ref().unwrap().anchor, Anchor::Point);
    }

    #[wasm_bindgen_test]
    fn dragging_a_snapped_end_away_unsnaps_and_dropping_on_a_track_snaps() {
        let v = tracks(vec![
            track("F", at(0.0, 0.0), 90.0, 36.0, 0.9),
            track("G", at(60.0, 0.0), 90.0, 36.0, 0.9),
        ]);
        let targets = SnapTargets {
            tracks: Some(&v),
            my_location: None,
        };
        let mut m = Measurement::default();
        place(&mut m, at(-50.0, 0.0), &targets);
        place(&mut m, at(1.0, 0.0), &targets);
        let drag = |m: &mut Measurement, i| reduce(m, i, &targets, false);
        // A press away from both ends picks nothing up (the map pans).
        let p = at(-20.0, 30.0);
        drag(
            &mut m,
            MeasureIntent::DragStart {
                lat: p.0,
                lon: p.1,
                km_per_px: KM_PER_PX,
            },
        );
        assert_eq!(m.dragging, None);
        // Pick up the F end and move it: unsnapped while it moves, and the
        // track does not pull it back.
        let p = at(2.0, 2.0);
        drag(
            &mut m,
            MeasureIntent::DragStart {
                lat: p.0,
                lon: p.1,
                km_per_px: KM_PER_PX,
            },
        );
        assert_eq!(m.dragging, Some(End::End));
        let p = at(30.0, 30.0);
        drag(&mut m, MeasureIntent::DragTo { lat: p.0, lon: p.1 });
        follow(&mut m, true, Some(&v), None);
        assert_eq!(m.end.as_ref().unwrap(), &Endpoint::point(p.0, p.1));
        // Dropped far from every track: a plain point.
        drag(
            &mut m,
            MeasureIntent::DragEnd {
                lat: p.0,
                lon: p.1,
                km_per_px: KM_PER_PX,
            },
        );
        assert_eq!(m.dragging, None);
        assert_eq!(m.end.as_ref().unwrap().anchor, Anchor::Point);
        // Dragged onto G: snaps to G.
        drag(
            &mut m,
            MeasureIntent::DragStart {
                lat: p.0,
                lon: p.1,
                km_per_px: KM_PER_PX,
            },
        );
        let p = at(58.0, 3.0);
        drag(
            &mut m,
            MeasureIntent::DragEnd {
                lat: p.0,
                lon: p.1,
                km_per_px: KM_PER_PX,
            },
        );
        assert_eq!(m.end.as_ref().unwrap().anchor, Anchor::Track("G".into()));
        // A drop the shell could not place still ends the drag.
        drag(
            &mut m,
            MeasureIntent::DragStart {
                lat: p.0,
                lon: p.1,
                km_per_px: KM_PER_PX,
            },
        );
        assert_eq!(m.dragging, Some(End::End));
        drag(
            &mut m,
            MeasureIntent::DragEnd {
                lat: f64::NAN,
                lon: f64::NAN,
                km_per_px: KM_PER_PX,
            },
        );
        assert_eq!(m.dragging, None);
        assert_eq!(m.end.as_ref().unwrap().anchor, Anchor::Track("G".into()));
    }

    #[wasm_bindgen_test]
    fn closest_approach_text_to_a_fixed_point() {
        // 20 km west and 4.2 km north of the point, moving east at 40 km/h.
        let point = at(0.0, 0.0);
        let t = track("B", at(-20.0, 4.2), 90.0, 40.0, 0.9);
        assert_eq!(
            approach_text(&t, point),
            "Closest approach 4.2 km in 30 min \u{00B7} high confidence"
        );
        let mid = TrackVm {
            confidence: 0.5,
            ..t.clone()
        };
        assert!(approach_text(&mid, point).ends_with("medium confidence"));
        // Low confidence: no time.
        let young = TrackVm {
            confidence: 0.25,
            ..t.clone()
        };
        assert_eq!(
            approach_text(&young, point),
            "Closest approach: motion not known yet"
        );
        // Moving away.
        let away = TrackVm {
            direction_deg: 270.0,
            ..t.clone()
        };
        assert_eq!(
            approach_text(&away, point),
            "Moving away: closest approach has passed"
        );
        // Too far out to say.
        let slow = TrackVm {
            speed_kmh: 10.0,
            ..t.clone()
        };
        assert_eq!(
            approach_text(&slow, point),
            "Closest approach more than an hour out"
        );
        let still = TrackVm {
            stationary: true,
            ..t
        };
        assert_eq!(approach_text(&still, point), "B is stationary");
    }

    #[wasm_bindgen_test]
    fn the_readout_of_a_track_to_a_point_measurement() {
        let v = tracks(vec![track("B", at(-20.0, 4.2), 90.0, 40.0, 0.9)]);
        let targets = SnapTargets {
            tracks: Some(&v),
            my_location: None,
        };
        let mut m = Measurement::default();
        place(&mut m, at(-20.0, 4.0), &targets);
        place(&mut m, at(0.0, 0.0), &targets);
        let vm = view(&MeasureViewInputs {
            measurement: &m,
            tool_active: true,
            targets,
            hover: None,
            snap_km: 16.0,
        });
        assert_eq!(vm.readout.len(), 2, "{:?}", vm.readout);
        assert!(vm.readout[0].starts_with("20.") && vm.readout[0].contains(" km / "));
        assert_eq!(
            vm.readout[1],
            "Closest approach 4.2 km in 30 min \u{00B7} high confidence"
        );
        assert_eq!(vm.ends[0].label.as_deref(), Some("B"));
        assert!(!vm.ends[0].muted);
        assert_eq!(vm.ends[1].label, None);
        // After the track ends: no approach, a note instead.
        follow(&mut m, true, Some(&tracks(vec![])), None);
        let vm = view(&MeasureViewInputs {
            measurement: &m,
            tool_active: true,
            targets: SnapTargets::default(),
            hover: None,
            snap_km: 16.0,
        });
        assert_eq!(
            vm.readout[1..],
            ["Track B ended: point stays where it was last seen".to_string()]
        );
    }

    #[wasm_bindgen_test]
    fn the_readout_of_a_track_to_track_measurement() {
        // A still, B 20 km east closing at 30 km/h on A's line... offset 3
        // km north so they pass rather than meet.
        let a = track("A", at(0.0, 0.0), 90.0, 0.0, 0.9);
        let b = track("B", at(20.0, 3.0), 270.0, 30.0, 0.9);
        assert_eq!(
            pair_text(&a, &b),
            "Converging at 30 km/h \u{00B7} closest 3.0 km in 40 min"
        );
        let parting = TrackVm {
            direction_deg: 90.0,
            ..b.clone()
        };
        assert_eq!(pair_text(&a, &parting), "Diverging at 30 km/h");
        let along = TrackVm {
            direction_deg: 90.0,
            speed_kmh: 30.0,
            ..a.clone()
        };
        assert_eq!(
            pair_text(&along, &parting),
            "Holding about the same distance"
        );
        let young = TrackVm {
            confidence: 0.2,
            ..b.clone()
        };
        assert_eq!(pair_text(&a, &young), "A and B: motion not known yet");
        // Through the view.
        let v = tracks(vec![a.clone(), b.clone()]);
        let targets = SnapTargets {
            tracks: Some(&v),
            my_location: None,
        };
        let mut m = Measurement::default();
        place(&mut m, at(1.0, 0.0), &targets);
        place(&mut m, at(19.0, 3.0), &targets);
        let vm = view(&MeasureViewInputs {
            measurement: &m,
            tool_active: true,
            targets,
            hover: None,
            snap_km: 16.0,
        });
        assert_eq!(vm.readout[1], pair_text(&a, &b));
        assert_eq!(
            vm.ends
                .iter()
                .map(|e| e.label.clone().unwrap())
                .collect::<Vec<_>>(),
            ["A", "B"]
        );
    }

    #[wasm_bindgen_test]
    fn the_snap_hint_previews_the_target_under_the_pointer() {
        let v = tracks(vec![track("H", at(0.0, 0.0), 90.0, 36.0, 0.9)]);
        let targets = SnapTargets {
            tracks: Some(&v),
            my_location: None,
        };
        let m = Measurement::default();
        let inputs = |hover, tool_active| MeasureViewInputs {
            measurement: &m,
            tool_active,
            targets,
            hover: Some(hover),
            snap_km: 16.0,
        };
        let near = at(3.0, 3.0);
        assert_eq!(
            view(&inputs(near, true)).snap_hint,
            Some(SnapHintVm {
                lat: at(0.0, 0.0).0,
                lon: at(0.0, 0.0).1
            })
        );
        assert_eq!(view(&inputs(near, false)).snap_hint, None, "tool off");
        assert_eq!(view(&inputs(at(40.0, 0.0), true)).snap_hint, None);
        // Over a placed endpoint the press drags it: no hint, a grab cursor.
        let mut placed = Measurement::default();
        place(&mut placed, at(0.0, 0.0), &targets);
        let i = MeasureViewInputs {
            measurement: &placed,
            ..inputs(near, true)
        };
        assert_eq!(view(&i).snap_hint, None);
        assert!(can_grab(&placed, true, near.0, near.1, 16.0));
        assert!(!can_grab(&placed, false, near.0, near.1, 16.0));
    }
}
