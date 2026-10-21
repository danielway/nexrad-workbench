//! A fixed tilt is an angle, not a cut number.
//!
//! A SAILS or MRLE volume comes back to its low angles mid-volume: VCP 212
//! with SAILS ×3 makes the 0.5° split pair four times. Each of those visits
//! is its own cut with its own elevation number (the number stays the cut's
//! identity in storage keys and the inspector), but a person who holds
//! "0.5°" means the angle. So every cut at that angle is a frame, in volume
//! order: the base cut and each repeat, in Frames mode, while stepping, on
//! the timeline's cells and in what the live stream fetches.
//!
//! The rule, in [`TiltFilter::cuts`]: a held tilt matches each cut of the
//! volume whose angle rounds to the same tenth of a degree as the held one
//! and that plays the same part in its split pair as the cut the pick chose
//! (the surveillance cut for reflectivity, the Doppler cut otherwise; see
//! `ladder::pick_cut`). That is one cut per visit, so the 0.5° of a
//! VCP 212 with SAILS ×3 is four frames a volume and of a VCP 35 one.
//!
//! The volume's own VCP pattern (from its header record) says each cut's
//! angle and waveform. Without one only the chosen cut number matches,
//! which is what a held tilt meant before.

use crate::core::ElevationSelection;
use crate::data::ExtractedVcp;

/// Whether two angles are the same tilt: they round to the same tenth of a
/// degree ("0.5°"), the precision every surface prints.
pub(crate) fn same_angle(a: f32, b: f32) -> bool {
    tenths(a) == tenths(b)
}

fn tenths(deg: f32) -> i32 {
    (deg * 10.0).round() as i32
}

/// A set of elevation numbers within one volume (1 to 63; a VCP holds at
/// most a few dozen cuts). `ALL` is every cut, whatever its number.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct CutSet(u64);

impl CutSet {
    pub(crate) const ALL: CutSet = CutSet(u64::MAX);
    pub(crate) const EMPTY: CutSet = CutSet(0);

    /// Just this cut.
    pub(crate) fn only(elevation_number: u8) -> Self {
        let mut s = Self::EMPTY;
        s.insert(elevation_number);
        s
    }

    fn insert(&mut self, elevation_number: u8) {
        if (1..64).contains(&elevation_number) {
            self.0 |= 1 << elevation_number;
        }
    }

    pub(crate) fn contains(self, elevation_number: u8) -> bool {
        self == Self::ALL
            || ((1..64).contains(&elevation_number) && self.0 & (1 << elevation_number) != 0)
    }

    /// How many cuts the set holds (0 for `ALL`, which has no count).
    pub(crate) fn len(self) -> usize {
        if self == Self::ALL {
            0
        } else {
            self.0.count_ones() as usize
        }
    }

    /// The numbers in the set, lowest first (none for `ALL`).
    #[cfg(test)]
    pub(crate) fn numbers(self) -> Vec<u8> {
        if self == Self::ALL {
            return Vec::new();
        }
        (1..64u8).filter(|&n| self.contains(n)).collect()
    }
}

/// The tilt a list of frames is built for: every cut (Latest), or one angle.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub(crate) enum TiltFilter {
    /// Latest: every cut is a frame.
    #[default]
    Every,
    /// A held angle. `anchor` is the cut the pick chose (its elevation
    /// number); it says which part of a split pair the angle means.
    Angle { anchor: u8, angle: f32 },
}

impl From<&ElevationSelection> for TiltFilter {
    fn from(selection: &ElevationSelection) -> Self {
        match selection {
            ElevationSelection::Latest => TiltFilter::Every,
            ElevationSelection::Fixed {
                elevation_number,
                angle,
            } => TiltFilter::Angle {
                anchor: *elevation_number,
                angle: *angle,
            },
        }
    }
}

impl TiltFilter {
    /// A held cut whose angle is not known: only that number matches.
    #[cfg(test)]
    pub(crate) fn number(n: u8) -> Self {
        TiltFilter::Angle {
            anchor: n,
            angle: f32::NAN,
        }
    }

    pub(crate) fn is_every(&self) -> bool {
        matches!(self, TiltFilter::Every)
    }

    /// The cut the pick chose; `None` for Latest.
    pub(crate) fn anchor(&self) -> Option<u8> {
        match self {
            TiltFilter::Every => None,
            TiltFilter::Angle { anchor, .. } => Some(*anchor),
        }
    }

    /// The cuts of a volume (described by its VCP `plan`) that are frames
    /// at this tilt. See the module docs for the rule.
    pub(crate) fn cuts(&self, plan: Option<&ExtractedVcp>) -> CutSet {
        let TiltFilter::Angle { anchor, angle } = *self else {
            return CutSet::ALL;
        };
        let elevations = plan.map_or(&[][..], |p| &p.elevations[..]);
        let at_angle = |e: &crate::data::ExtractedVcpElevation| same_angle(e.angle, angle);
        let surveillance = |e: &crate::data::ExtractedVcpElevation| e.waveform == "CS";
        // The part of a split pair the held cut plays: the anchor's own when
        // it is at the angle in this volume, else the first cut at the angle
        // (the VCP renumbered its cuts).
        let role = elevations
            .get(anchor.saturating_sub(1) as usize)
            .filter(|e| at_angle(e))
            .or_else(|| elevations.iter().find(|e| at_angle(e)))
            .map(surveillance);
        let Some(role) = role else {
            return CutSet::only(anchor);
        };
        let mut set = CutSet::EMPTY;
        for (i, e) in elevations.iter().enumerate() {
            if at_angle(e) && surveillance(e) == role {
                set.insert((i + 1) as u8);
            }
        }
        set
    }

    /// Whether cut `elevation_number` of a volume with this `plan` is a
    /// frame at this tilt.
    pub(crate) fn matches(&self, plan: Option<&ExtractedVcp>, elevation_number: u8) -> bool {
        self.cuts(plan).contains(elevation_number)
    }
}

/// The cuts a fetch scoped to `wanted` cut numbers must keep, given the
/// volume's VCP: each wanted cut and every other visit of its angle (the
/// SAILS/MRLE repeats are frames at that tilt too). Empty `wanted` (a whole
/// volume) stays empty.
pub(crate) fn scope_with_repeats(wanted: &[u8], plan: Option<&ExtractedVcp>) -> Vec<u8> {
    let mut out: Vec<u8> = wanted.to_vec();
    if let Some(p) = plan {
        for &anchor in wanted {
            let Some(e) = p.elevations.get(anchor.saturating_sub(1) as usize) else {
                continue;
            };
            let set = TiltFilter::Angle {
                anchor,
                angle: e.angle,
            }
            .cuts(plan);
            out.extend((1..=p.elevations.len() as u8).filter(|&n| set.contains(n)));
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// The selection as a cut number to compare with the cut the antenna is
/// collecting: `None` for Latest, the collecting cut itself when it is at
/// the held angle (a SAILS/MRLE repeat counts as the tilt), else the
/// chosen cut.
pub(crate) fn selected_cut_against(
    selection: &ElevationSelection,
    plan: Option<&ExtractedVcp>,
    collecting: Option<u8>,
) -> Option<u8> {
    let filter = TiltFilter::from(selection);
    let anchor = filter.anchor()?;
    match collecting {
        Some(c) if filter.matches(plan, c) => Some(c),
        _ => Some(anchor),
    }
}

#[cfg(test)]
pub(crate) mod fixtures {
    use crate::data::{ExtractedVcp, ExtractedVcpElevation};

    fn cut(angle: f32, waveform: &str, sails: bool) -> ExtractedVcpElevation {
        ExtractedVcpElevation {
            angle,
            waveform: waveform.to_string(),
            prf_number: 1,
            is_sails: sails,
            is_mrle: false,
            is_base_tilt: false,
            azimuth_rate: Some(20.0),
        }
    }

    /// VCP 212 with SAILS ×3: the 0.5° split pair (CS then CD) comes back
    /// three times among the upper cuts. 0.5° CS is cuts 1, 9, 14 and 19;
    /// 0.5° CD is 2, 10, 15 and 20.
    pub(crate) fn vcp_212_sails_x3() -> ExtractedVcp {
        let mut e = vec![
            cut(0.5, "CS", false),
            cut(0.5, "CDW", false),
            cut(0.9, "CS", false),
            cut(0.9, "CDW", false),
            cut(1.3, "CS", false),
            cut(1.3, "CDW", false),
            cut(1.8, "B", false),
            cut(2.4, "B", false),
        ];
        e.push(cut(0.5, "CS", true)); // 9
        e.push(cut(0.5, "CDW", true)); // 10
        e.extend([
            cut(3.1, "B", false),
            cut(4.0, "B", false),
            cut(5.1, "B", false),
        ]);
        e.push(cut(0.5, "CS", true)); // 14
        e.push(cut(0.5, "CDW", true)); // 15
        e.extend([
            cut(6.4, "CDWO", false),
            cut(8.0, "CDWO", false),
            cut(10.0, "CDWO", false),
        ]);
        e.push(cut(0.5, "CS", true)); // 19
        e.push(cut(0.5, "CDW", true)); // 20
        e.extend([
            cut(12.5, "CDWO", false),
            cut(15.6, "CDWO", false),
            cut(19.5, "CDWO", false),
        ]);
        ExtractedVcp {
            number: 212,
            elevations: e,
        }
    }

    /// Each cut lasts 12 s, back to back from the volume's start.
    pub(crate) const CUT: f64 = 12.0;

    pub(crate) fn volume(start: f64, vcp: &ExtractedVcp) -> crate::core::Scan {
        let sweeps: Vec<crate::core::Sweep> = vcp
            .elevations
            .iter()
            .enumerate()
            .map(|(i, e)| crate::core::Sweep {
                start_time: start + CUT * i as f64,
                end_time: start + CUT * (i + 1) as f64,
                elevation: e.angle,
                elevation_number: (i + 1) as u8,
                start_azimuth: 0.0,
                radials: Vec::new(),
                cached_products: vec!["reflectivity".to_string(), "velocity".to_string()],
            })
            .collect();
        crate::core::Scan {
            start_time: start,
            end_time: start + CUT * sweeps.len() as f64,
            key_timestamp: start,
            vcp: vcp.number,
            vcp_pattern: Some(vcp.clone()),
            sweeps,
            completeness: Some(crate::data::ScanCompleteness::Complete),
            cached_sweep_count: None,
            planned_sweep_count: None,
        }
    }

    /// VCP 35 (clear air): one 0.5° split pair, no repeats.
    pub(crate) fn vcp_35() -> ExtractedVcp {
        ExtractedVcp {
            number: 35,
            elevations: vec![
                cut(0.5, "CS", false),
                cut(0.5, "CDW", false),
                cut(0.9, "CS", false),
                cut(0.9, "CDW", false),
                cut(1.3, "B", false),
                cut(1.8, "B", false),
                cut(2.4, "B", false),
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn held(n: u8, angle: f32) -> TiltFilter {
        TiltFilter::Angle { anchor: n, angle }
    }

    #[wasm_bindgen_test]
    fn sails_x3_makes_four_frames_at_the_held_angle() {
        let v = vcp_212_sails_x3();
        assert_eq!(held(1, 0.5).cuts(Some(&v)).numbers(), vec![1, 9, 14, 19]);
        // The Doppler half of the pair (velocity) repeats too.
        assert_eq!(held(2, 0.5).cuts(Some(&v)).numbers(), vec![2, 10, 15, 20]);
        // An angle SAILS does not revisit is one frame.
        assert_eq!(held(3, 0.9).cuts(Some(&v)).numbers(), vec![3]);
        assert_eq!(held(7, 1.8).cuts(Some(&v)).numbers(), vec![7]);
    }

    #[wasm_bindgen_test]
    fn vcp_35_makes_one_frame() {
        let v = vcp_35();
        assert_eq!(held(1, 0.5).cuts(Some(&v)).numbers(), vec![1]);
        assert_eq!(held(2, 0.5).cuts(Some(&v)).numbers(), vec![2]);
    }

    #[wasm_bindgen_test]
    fn angles_match_to_the_tenth() {
        assert!(same_angle(0.48, 0.5));
        assert!(same_angle(0.53, 0.5));
        assert!(!same_angle(0.9, 0.5));
        let v = vcp_212_sails_x3();
        // A held angle read off the encoder still finds every visit.
        assert_eq!(held(1, 0.48).cuts(Some(&v)).numbers(), vec![1, 9, 14, 19]);
    }

    #[wasm_bindgen_test]
    fn without_a_plan_only_the_chosen_cut_matches() {
        assert_eq!(held(1, 0.5).cuts(None).numbers(), vec![1]);
        assert!(held(1, 0.5).matches(None, 1));
        assert!(!held(1, 0.5).matches(None, 9));
    }

    #[wasm_bindgen_test]
    fn a_renumbered_volume_matches_by_angle() {
        // The anchor (cut 3, 0.9° in the old VCP) is 0.5° here: the angle
        // wins, and the first 0.5° cut's part of the pair is used.
        let v = vcp_212_sails_x3();
        assert_eq!(held(5, 0.9).cuts(Some(&v)).numbers(), vec![3]);
        // An angle the volume lacks falls back to the number.
        assert_eq!(held(4, 0.7).cuts(Some(&v)).numbers(), vec![4]);
    }

    #[wasm_bindgen_test]
    fn a_scoped_fetch_keeps_every_visit_of_its_cut() {
        let v = vcp_212_sails_x3();
        assert_eq!(scope_with_repeats(&[1], Some(&v)), vec![1, 9, 14, 19]);
        assert_eq!(scope_with_repeats(&[2], Some(&v)), vec![2, 10, 15, 20]);
        assert_eq!(scope_with_repeats(&[3], Some(&v)), vec![3]);
        assert_eq!(scope_with_repeats(&[1], Some(&vcp_35())), vec![1]);
        assert_eq!(scope_with_repeats(&[1], None), vec![1]);
        assert!(scope_with_repeats(&[], Some(&v)).is_empty());
    }

    #[wasm_bindgen_test]
    fn every_matches_every_cut() {
        let v = vcp_212_sails_x3();
        assert!(TiltFilter::Every.matches(Some(&v), 22));
        assert!(TiltFilter::Every.matches(None, 200));
        assert_eq!(TiltFilter::Every.cuts(None), CutSet::ALL);
    }

    #[wasm_bindgen_test]
    fn collecting_a_repeat_counts_as_the_tilt() {
        let v = vcp_212_sails_x3();
        let sel = ElevationSelection::Fixed {
            elevation_number: 1,
            angle: 0.5,
        };
        assert_eq!(selected_cut_against(&sel, Some(&v), Some(14)), Some(14));
        assert_eq!(selected_cut_against(&sel, Some(&v), Some(15)), Some(1));
        assert_eq!(selected_cut_against(&sel, Some(&v), None), Some(1));
        assert_eq!(
            selected_cut_against(&ElevationSelection::Latest, Some(&v), Some(14)),
            None
        );
    }
}

/// The held angle through every surface that lists frames: the frame list
/// playback loops over, stepping, the resolver, the desktop strip's cells
/// and the ladder.
#[cfg(test)]
mod sails_frames {
    use super::fixtures::*;
    use super::*;
    use crate::core::{RadarProduct, RadarTimeline};
    use crate::data::ExtractedVcp;
    use wasm_bindgen_test::wasm_bindgen_test;

    struct PlainClock;
    impl crate::core::session::Clock for PlainClock {
        fn hms(&self, ts: f64) -> String {
            format!("{ts:.0}")
        }
        fn hm(&self, ts: f64) -> String {
            format!("{ts:.0}")
        }
        fn day(&self, _ts: f64) -> (i32, u32, u32) {
            (2026, 1, 1)
        }
    }

    fn timeline(vcp: &ExtractedVcp) -> RadarTimeline {
        RadarTimeline {
            scans: vec![volume(0.0, vcp), volume(300.0, vcp)],
        }
    }

    const HELD_05: ElevationSelection = ElevationSelection::Fixed {
        elevation_number: 1,
        angle: 0.5,
    };

    /// End of cut `n` in the volume starting at `start`.
    fn end_of(start: f64, n: u8) -> f64 {
        start + CUT * n as f64
    }

    #[wasm_bindgen_test]
    fn sails_x3_loops_four_frames_a_volume_and_vcp_35_one() {
        let tl = timeline(&vcp_212_sails_x3());
        let frames = tl.matching_sweep_end_times((&HELD_05).into(), "reflectivity", None);
        let want: Vec<f64> = [0.0, 300.0]
            .iter()
            .flat_map(|&v| [1, 9, 14, 19].map(|n| end_of(v, n)))
            .collect();
        assert_eq!(frames, want);

        let tl35 = timeline(&vcp_35());
        let frames = tl35.matching_sweep_end_times((&HELD_05).into(), "reflectivity", None);
        assert_eq!(frames, vec![end_of(0.0, 1), end_of(300.0, 1)]);
    }

    #[wasm_bindgen_test]
    fn stepping_lands_on_each_repeat() {
        use crate::core::transport::{step_target, ScanFrames, StepDir};
        let tl = timeline(&vcp_212_sails_x3());
        let index = ScanFrames {
            scans: &tl,
            tilt: (&HELD_05).into(),
            volumes: false,
        };
        let mut pos = end_of(0.0, 1);
        let mut landed = Vec::new();
        for _ in 0..4 {
            pos = step_target(&index, pos, StepDir::Forward, None, 30.0, 10_000.0).unwrap();
            landed.push(pos);
        }
        assert_eq!(
            landed,
            vec![
                end_of(0.0, 9),
                end_of(0.0, 14),
                end_of(0.0, 19),
                end_of(300.0, 1)
            ]
        );
        let back = step_target(&index, pos, StepDir::Back, None, 30.0, 10_000.0);
        assert_eq!(back, Some(end_of(0.0, 19)));
    }

    #[wasm_bindgen_test]
    fn the_canvas_shows_the_newest_visit() {
        use crate::core::playback_manager::resolve_active_sweep_target;
        let tl = timeline(&vcp_212_sails_x3());
        let at = |t: f64| {
            resolve_active_sweep_target("KTLX", t, &HELD_05, RadarProduct::Reflectivity, &tl, 900.0)
                .map(|id| id.elevation_number)
        };
        assert_eq!(at(end_of(0.0, 1)), Some(1));
        assert_eq!(at(end_of(0.0, 9)), Some(9));
        assert_eq!(at(end_of(0.0, 16)), Some(14));
        assert_eq!(at(end_of(0.0, 19)), Some(19));
        // Velocity holds the Doppler half of each visit.
        let vel = ElevationSelection::Fixed {
            elevation_number: 2,
            angle: 0.5,
        };
        let id = resolve_active_sweep_target(
            "KTLX",
            end_of(0.0, 16),
            &vel,
            RadarProduct::Velocity,
            &tl,
            900.0,
        );
        assert_eq!(id.map(|i| i.elevation_number), Some(15));
    }

    #[wasm_bindgen_test]
    fn the_strip_draws_a_cell_per_visit_and_rings_the_one_on_screen() {
        use crate::core::timeline_view::{FrameJoinInputs, TimelineView};
        let tl = timeline(&vcp_212_sails_x3());
        let view = TimelineView::build(&tl, &[], None, None);
        let join = |active: Option<(f64, u8)>| FrameJoinInputs {
            queued: &[],
            in_flight: &[],
            failed: &[],
            product: "reflectivity",
            tilt: (&HELD_05).into(),
            active,
            prev_active: None,
        };
        for (n, on) in [(1u8, 0usize), (9, 1), (14, 2), (19, 3)] {
            let cs = view.frame_containers_in_range(0.0, 600.0, join(Some((300.0, n))));
            assert_eq!(cs.len(), 2);
            let numbers: Vec<u8> = cs[1].cells.iter().map(|c| c.elevation_number).collect();
            assert_eq!(numbers, vec![1, 9, 14, 19]);
            let ringed: Vec<usize> = cs[1]
                .cells
                .iter()
                .enumerate()
                .filter(|(_, c)| c.is_active)
                .map(|(i, _)| i)
                .collect();
            assert_eq!(ringed, vec![on], "cut {n}");
            assert!(cs[0].cells.iter().all(|c| !c.is_active));
        }
        let tl35 = timeline(&vcp_35());
        let view = TimelineView::build(&tl35, &[], None, None);
        let cs = view.frame_containers_in_range(0.0, 600.0, join(None));
        assert!(cs.iter().all(|c| c.cells.len() == 1));
    }

    #[wasm_bindgen_test]
    fn the_ladder_row_counts_the_repeats_and_outlines_each_one() {
        use crate::core::ladder::{
            tilt_rows, LadderClock, LadderInputs, OnScreenCut, VolumeLadder,
        };
        use crate::core::projection::scan_to_projection;
        let vcp = vcp_212_sails_x3();
        let entries = crate::core::build_elevation_list_from_vcp(&vcp);
        let rows = tilt_rows(&HELD_05, &entries, RadarProduct::Reflectivity);
        assert_eq!(rows[1].label, "0.5\u{00B0}");
        assert_eq!(
            rows[1].note.as_deref(),
            Some("CS \u{00B7} CD \u{00B7} SAILS \u{00D7}3")
        );
        assert_eq!(rows[1].repeats.as_deref(), Some("SAILS \u{00D7}3"));
        assert_eq!(rows[2].repeats, None);
        assert!(rows[1].selected);

        let scan = volume(0.0, &vcp);
        let projection = scan_to_projection(&scan);
        for n in [1u8, 9, 14, 19] {
            let ladder = VolumeLadder::build(
                LadderInputs {
                    entries: &entries,
                    selection: &HELD_05,
                    product: RadarProduct::Reflectivity,
                    volume: Some(&projection),
                    clock: LadderClock::AtTime {
                        playhead_secs: end_of(0.0, n),
                    },
                    antenna_azimuth: None,
                    on_screen: Some(OnScreenCut {
                        elevation_number: n,
                        start_secs: end_of(0.0, n) - CUT,
                    }),
                    role: crate::core::session::TiltRole::Picks,
                    fetching: false,
                },
                &PlainClock,
            );
            let outlined: Vec<(usize, u8)> = ladder
                .rows
                .iter()
                .enumerate()
                .flat_map(|(r, row)| {
                    row.bars
                        .iter()
                        .filter(|b| b.on_screen)
                        .map(move |b| (r, b.elevation_number))
                })
                .collect();
            assert_eq!(outlined, vec![(1, n)], "cut {n}");
        }
    }
}
