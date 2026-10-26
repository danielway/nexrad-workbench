//! What the 2D canvas says about each site on the map when more than one
//! is shown: which sites the caption names as behind the playhead, the
//! label a hovered radar shows, and which sites the probe reads and in what
//! order.
//!
//! The caption rule for one site stays in [`crate::core::canvas`]; this
//! module adds the words for the others. The shell gathers one plain fact
//! per site and paints what comes back.

use crate::core::canvas::{frame_disagrees, CaptionLine, CaptionRole};
use crate::core::session::{format_age, Clock};

/// The caption names at most this many sites.
pub(crate) const MAX_NAMED_SITES: usize = 2;

/// One site's frame, as the caption rule reads it.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct SiteFrame {
    pub id: String,
    /// The frame's midpoint and end, Unix seconds; `None` with nothing on
    /// screen.
    pub frame: Option<(f64, f64)>,
    /// One frame for this site ([`crate::core::canvas::frame_step_secs`]).
    pub frame_step_secs: f64,
    /// What the render loop knows about this site's frame: the caption
    /// rule reads it as it reads the focus's.
    pub context: crate::core::canvas::CaptionContext,
    /// This site's live stream.
    pub stream: crate::core::canvas::StreamFacts,
}

/// The sites whose frame disagrees with the playhead, and the playhead.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct SitesBehind {
    /// Each site's id and the time it shows (`None` with no frame yet),
    /// the newest frame first, at most [`MAX_NAMED_SITES`].
    pub sites: Vec<(String, Option<f64>)>,
    pub playhead: f64,
}

/// Which of `others` (every site but the focus) the caption names: those
/// more than one frame from `target`, newest frame first (the disc on top
/// reads first), a site with no frame last, two at most. `None` when every
/// site agrees with the playhead.
pub(crate) fn sites_behind(others: &[SiteFrame], target: f64) -> Option<SitesBehind> {
    let mut behind: Vec<(String, Option<f64>)> = others
        .iter()
        .filter(|s| match s.frame {
            Some((mid, end)) => frame_disagrees(mid, end, target, s.frame_step_secs),
            None => true,
        })
        .map(|s| (s.id.clone(), s.frame.map(|(mid, _)| mid)))
        .collect();
    if behind.is_empty() {
        return None;
    }
    // Stable, so equal times keep list order.
    behind.sort_by(|a, b| match (a.1, b.1) {
        (Some(x), Some(y)) => y.total_cmp(&x),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
    behind.truncate(MAX_NAMED_SITES);
    Some(SitesBehind {
        sites: behind,
        playhead: target,
    })
}

/// The caption's words for the sites behind, in the mock's form:
/// `KJAX showing 02:57:12 · KMLB 02:56:33 · playhead 03:00:00`. A site with
/// no frame reads `KJAX no frame yet`.
pub(crate) fn sites_behind_line(
    behind: &SitesBehind,
    pinned: bool,
    clock: &dyn Clock,
) -> CaptionLine {
    let mut segs: Vec<(String, CaptionRole)> = Vec::new();
    for (i, (id, showing)) in behind.sites.iter().enumerate() {
        if i > 0 {
            segs.push((" · ".to_string(), CaptionRole::Words));
        }
        match showing {
            Some(ts) => {
                let lead = if i == 0 {
                    format!("{id} showing ")
                } else {
                    format!("{id} ")
                };
                segs.push((lead, CaptionRole::Words));
                segs.push((clock.hms(*ts), CaptionRole::Frame));
            }
            None => segs.push((format!("{id} no frame yet"), CaptionRole::Words)),
        }
    }
    segs.push((" · playhead ".to_string(), CaptionRole::Words));
    segs.push((clock.hms(behind.playhead), CaptionRole::Playhead { pinned }));
    CaptionLine {
        segments: segs,
        ..CaptionLine::default()
    }
}

/// The label a hovered radar shows: its id, its frame's time and age
/// (`KMLB · 02:56:33 · 3 min old`), or `KMLB · no frame yet`.
pub(crate) fn radar_hover_label(
    id: &str,
    frame_time: Option<f64>,
    age_secs: Option<f64>,
    clock: &dyn Clock,
) -> String {
    match frame_time {
        None => format!("{id} · no frame yet"),
        Some(ts) => match age_secs {
            Some(age) => format!("{id} · {} · {}", clock.hms(ts), format_age(age)),
            None => format!("{id} · {}", clock.hms(ts)),
        },
    }
}

/// One site's reading under the probe.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct ProbeReading {
    pub id: String,
    pub azimuth_deg: f64,
    pub range_km: f64,
    /// The reach of the frame on screen; `None` with no frame.
    pub max_range_km: Option<f64>,
    /// The product value at the cursor, physical units.
    pub value: Option<f32>,
}

impl ProbeReading {
    /// Whether the cursor lies on this site's disc.
    fn under_cursor(&self) -> bool {
        self.max_range_km.is_some_and(|r| self.range_km <= r)
    }
}

/// Which readings the probe lists, as indices into `readings` (given in
/// list order): the sites whose disc is under the cursor, the focus first
/// and the rest in list order.
pub(crate) fn probe_order(readings: &[ProbeReading], focus: usize) -> Vec<usize> {
    let rest = (0..readings.len()).filter(|&i| i != focus);
    std::iter::once(focus)
        .chain(rest)
        .filter(|&i| readings.get(i).is_some_and(ProbeReading::under_cursor))
        .collect()
}

/// One site's probe line: `KTBW  REF 52.0 dBZ  Az 123.4° 45.6 km`, or
/// `KTBW  no echo  Az ...` with nothing above threshold there.
pub(crate) fn probe_site_line(reading: &ProbeReading, code: &str, unit: &str) -> String {
    let value = match reading.value {
        Some(v) if unit.is_empty() => format!("{code} {v:.3}"),
        Some(v) => format!("{code} {v:.1} {unit}"),
        None => "no echo".to_string(),
    };
    format!(
        "{}  {value}  Az {:.1}\u{00B0} {:.1} km",
        reading.id, reading.azimuth_deg, reading.range_km
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

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
        fn day(&self, _ts: f64) -> (i32, u32, u32) {
            (2024, 10, 10)
        }
    }

    /// 03:00:00 UTC on 10 October 2024, the Milton playhead.
    const PLAYHEAD: f64 = 1_728_529_200.0;

    fn hms(h: i64, m: i64, s: i64) -> f64 {
        1_728_518_400.0 + (h * 3600 + m * 60 + s) as f64
    }

    /// A site whose 30 s sweep has its midpoint at `mid`.
    fn site(id: &str, mid: Option<f64>) -> SiteFrame {
        SiteFrame {
            id: id.to_string(),
            frame: mid.map(|m| (m, m + 15.0)),
            frame_step_secs: 60.0,
            context: Default::default(),
            stream: Default::default(),
        }
    }

    fn text(line: &CaptionLine) -> String {
        line.segments.iter().map(|(s, _)| s.as_str()).collect()
    }

    #[wasm_bindgen_test]
    fn the_caption_names_the_sites_behind_newest_first_in_the_mocks_words() {
        let others = [
            site("KMLB", Some(hms(2, 56, 33))),
            site("KJAX", Some(hms(2, 57, 12))),
        ];
        let behind = sites_behind(&others, PLAYHEAD).unwrap();
        assert_eq!(
            text(&sites_behind_line(&behind, false, &Utc)),
            "KJAX showing 02:57:12 · KMLB 02:56:33 · playhead 03:00:00"
        );
    }

    #[wasm_bindgen_test]
    fn sites_with_the_playhead_are_not_named() {
        let others = [
            site("KMLB", Some(PLAYHEAD - 20.0)),
            site("KJAX", Some(hms(2, 57, 12))),
        ];
        let behind = sites_behind(&others, PLAYHEAD).unwrap();
        assert_eq!(
            behind.sites,
            vec![("KJAX".to_string(), Some(hms(2, 57, 12)))]
        );
        let all_with_it = [site("KMLB", Some(PLAYHEAD - 20.0))];
        assert_eq!(sites_behind(&all_with_it, PLAYHEAD), None);
        assert_eq!(sites_behind(&[], PLAYHEAD), None);
    }

    #[wasm_bindgen_test]
    fn at_most_two_sites_are_named_and_a_blank_disc_reads_last() {
        let others = [
            site("KAMX", None),
            site("KMLB", Some(hms(2, 56, 33))),
            site("KJAX", Some(hms(2, 57, 12))),
        ];
        let behind = sites_behind(&others, PLAYHEAD).unwrap();
        assert_eq!(behind.sites.len(), MAX_NAMED_SITES);
        assert_eq!(behind.sites[0].0, "KJAX");
        assert_eq!(behind.sites[1].0, "KMLB");
        let blank = sites_behind(&[site("KAMX", None)], PLAYHEAD).unwrap();
        assert_eq!(
            text(&sites_behind_line(&blank, true, &Utc)),
            "KAMX no frame yet · playhead 03:00:00"
        );
    }

    #[wasm_bindgen_test]
    fn the_playhead_takes_its_pinned_colour() {
        let behind = sites_behind(&[site("KJAX", Some(hms(2, 50, 0)))], PLAYHEAD).unwrap();
        let line = sites_behind_line(&behind, true, &Utc);
        assert_eq!(
            line.segments.last().unwrap().1,
            CaptionRole::Playhead { pinned: true }
        );
        assert_eq!(line.segments[1].1, CaptionRole::Frame);
        assert_eq!(line.action, None);
    }

    #[wasm_bindgen_test]
    fn a_hovered_radar_reads_its_frame_time_and_age() {
        assert_eq!(
            radar_hover_label("KMLB", Some(hms(2, 56, 33)), Some(180.0), &Utc),
            "KMLB · 02:56:33 · 3 min old"
        );
        assert_eq!(
            radar_hover_label("KMLB", Some(hms(2, 56, 33)), None, &Utc),
            "KMLB · 02:56:33"
        );
        assert_eq!(
            radar_hover_label("KJAX", None, None, &Utc),
            "KJAX · no frame yet"
        );
    }

    fn reading(id: &str, range_km: f64, value: Option<f32>) -> ProbeReading {
        ProbeReading {
            id: id.to_string(),
            azimuth_deg: 90.0,
            range_km,
            max_range_km: Some(230.0),
            value,
        }
    }

    #[wasm_bindgen_test]
    fn the_probe_lists_the_discs_under_the_cursor_focus_first() {
        let readings = [
            reading("KMLB", 100.0, Some(47.0)),
            reading("KJAX", 300.0, None),
            reading("KTBW", 40.0, Some(52.0)),
        ];
        // KTBW is the focus; KJAX's disc does not reach the cursor.
        assert_eq!(probe_order(&readings, 2), vec![2, 0]);
        // A site with no frame has no disc.
        let mut blank = readings.clone();
        blank[0].max_range_km = None;
        assert_eq!(probe_order(&blank, 2), vec![2]);
        // The focus off its disc is left out too.
        assert_eq!(probe_order(&readings, 1), vec![0, 2]);
    }

    #[wasm_bindgen_test]
    fn a_probe_line_names_the_site_and_its_value() {
        let r = reading("KTBW", 45.64, Some(52.0));
        assert_eq!(
            probe_site_line(&r, "REF", "dBZ"),
            "KTBW  REF 52.0 dBZ  Az 90.0° 45.6 km"
        );
        let quiet = reading("KMLB", 10.0, None);
        assert_eq!(
            probe_site_line(&quiet, "REF", "dBZ"),
            "KMLB  no echo  Az 90.0° 10.0 km"
        );
        let unitless = reading("KMLB", 10.0, Some(0.95));
        assert_eq!(
            probe_site_line(&unitless, "CC", ""),
            "KMLB  CC 0.950  Az 90.0° 10.0 km"
        );
    }
}
