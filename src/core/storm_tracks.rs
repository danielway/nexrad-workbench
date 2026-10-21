//! Storm cell tracks for the workbench: the per-site store that feeds the
//! leaf tracker (`crate::storm_cells::track`) one detection result per
//! volume, in time order, and the view of the tracks on the shown volume.
//!
//! Detection results come from [`StormCells`]: its cache holds the shown
//! volume's cells and, while tracks are on, the cells of the volumes before
//! it ([`history_keys`] names them; `storm_cells::decide` asks a worker for
//! the ones not cached yet, after the shown volume). This module performs no
//! I/O: [`StormTracks::sync`] only reads that cache.
//!
//! # Scrubbing: when a volume is observed, and when it is looked up
//!
//! The timeline moves both ways, but the tracker only moves forward, so the
//! store observes each volume at most once and keeps the frame it gave
//! (cell → track, and every track's state as of that volume) by volume time.
//! For the shown volume at time `t`:
//!
//! 1. **Seen before:** `t` has a frame. Show it; nothing is re-observed, so
//!    scrubbing back and forth over the same volumes always shows the same
//!    IDs, trails and arrows.
//! 2. **Ahead of the last observed volume:** first observe, oldest first, the
//!    history volumes between the last observed one and `t` (at most
//!    [`HISTORY_VOLUMES`] before `t`), then `t`. Volumes the timeline skips
//!    are not waited for; the gap only widens the search radius, and a gap
//!    longer than the tracker's `max_gap_ms` ends every track. Until every
//!    history volume's cells are cached the shown volume has no tracks.
//! 3. **Before the first frame kept:** history cannot be prepended to a
//!    forward tracker, so the store restarts there: frames and live tracks
//!    are dropped and step 2 runs from `t`'s own history. The ID counter is
//!    kept, so the restarted tracks get new labels rather than reusing old
//!    ones; one label never names two storms in a site session.
//! 4. **Between kept frames but never observed** (a volume that joined the
//!    timeline after the playhead passed it): shown without tracks.
//!
//! A provisional (live, partial-cut) result is previewed against the tracker
//! and not stored; the final result for that volume is observed when it
//! arrives. A threshold change restarts the store (the cells differ), and
//! a site change resets it, ID counter included.

use std::collections::BTreeMap;

use crate::core::storm_cells::{
    has_reflectivity, lowest_reflectivity_cut, CutInput, StormCellKey, StormCells,
};
use crate::core::Scan;
use crate::data::{ScanKey, UnixMillis};
use crate::storm_cells::track::{
    bearing_distance, closest_approach, track_label, Approach, Frame, TrackId, TrackParams,
    Tracker, MIN_FORECAST_CONFIDENCE,
};

/// Volumes before the shown one that tracking asks to have detected, so a
/// freshly opened time has motion and a path: twelve volumes are about an
/// hour of a 5-minute VCP.
pub(crate) const HISTORY_VOLUMES: usize = 12;

/// Frames kept for scrubbing back (twice the detection cache).
pub(crate) const MAX_FRAMES: usize = 96;

/// The forecast arrow's lead time.
pub(crate) const FORECAST_MINUTES: f64 = 30.0;

/// The stored cuts of up to [`HISTORY_VOLUMES`] volumes before `displayed`,
/// oldest first: the input the tracker needs before it can observe the
/// displayed volume. `scans` is the timeline (sorted by start). Walking back
/// stops at a gap longer than the tracker's `max_gap_ms` (nothing would be
/// matched across it) and skips volumes whose lowest reflectivity cut is
/// not stored.
pub(crate) fn history_keys(
    scans: &[Scan],
    displayed: &ScanKey,
    threshold_dbz: f32,
) -> Vec<StormCellKey> {
    let max_gap_ms = TrackParams::default().max_gap_ms;
    let t = displayed.scan_start.0;
    let end = scans.partition_point(|s| s.key_ms() < t);
    let mut newer = t;
    let mut keys = Vec::new();
    for scan in scans[..end].iter().rev().take(HISTORY_VOLUMES) {
        let ms = scan.key_ms();
        if newer - ms > max_gap_ms {
            break;
        }
        newer = ms;
        let Some(cut) = lowest_reflectivity_cut(scan.vcp_pattern.as_ref(), &scan.sweeps) else {
            continue;
        };
        let stored = scan
            .sweeps
            .iter()
            .any(|s| s.elevation_number == cut && has_reflectivity(s));
        if stored {
            keys.push(StormCellKey {
                scan_key: ScanKey::new(displayed.site.clone(), UnixMillis(ms)),
                elevation_number: cut,
                input: CutInput::Stored,
                fallback: false,
                threshold_dbz,
            });
        }
    }
    keys.reverse();
    keys
}

#[derive(Clone, Debug)]
struct StoredFrame {
    key: StormCellKey,
    frame: Frame,
}

/// What [`StormTracks::sync`] did, for logs and tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TrackSync {
    /// Tracks are off.
    Off,
    /// No cells are shown.
    NoCells,
    /// The shown volume was observed before; its frame is shown.
    LookedUp,
    /// The shown volume (and `history` volumes before it) were observed.
    Observed { history: usize },
    /// The shown result is provisional: previewed, nothing stored.
    Previewed { history: usize },
    /// A history volume's cells are not cached yet.
    Waiting,
    /// The shown volume cannot be tracked (see the module docs, case 4).
    Untracked,
}

/// The per-site track store.
#[derive(Clone, Debug, Default)]
pub(crate) struct StormTracks {
    tracker: Tracker,
    /// Final frames by volume time.
    frames: BTreeMap<i64, StoredFrame>,
    /// The threshold the stored frames were detected at.
    threshold_dbz: Option<f32>,
    /// The shown volume's key and frame.
    current: Option<(StormCellKey, Frame)>,
    /// The last [`StormTracks::sync`] outcome.
    pub last_sync: Option<TrackSync>,
}

impl StormTracks {
    /// Forget everything, ID counter included. For a site change.
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }

    /// Drop the frames and live tracks, keeping the ID counter.
    fn restart(&mut self) {
        self.tracker.restart();
        self.frames.clear();
        self.current = None;
    }

    /// The frame for the shown volume, if it is tracked.
    pub(crate) fn current(&self) -> Option<&Frame> {
        self.current.as_ref().map(|(_, f)| f)
    }

    /// Volume times with a stored frame, oldest first.
    #[cfg(test)]
    pub(crate) fn observed_times(&self) -> Vec<i64> {
        self.frames.keys().copied().collect()
    }

    /// Bring the tracks in line with the shown cells. `enabled` is "Storm
    /// cells" and "Tracks" both on; `history` comes from [`history_keys`]
    /// for the shown volume.
    pub(crate) fn sync(
        &mut self,
        enabled: bool,
        cells: &StormCells,
        history: &[StormCellKey],
    ) -> TrackSync {
        let outcome = self.sync_inner(enabled, cells, history);
        self.last_sync = Some(outcome);
        outcome
    }

    fn sync_inner(
        &mut self,
        enabled: bool,
        cells: &StormCells,
        history: &[StormCellKey],
    ) -> TrackSync {
        if !enabled {
            self.current = None;
            return TrackSync::Off;
        }
        let Some(key) = cells.computed_for.clone() else {
            self.current = None;
            return TrackSync::NoCells;
        };
        if self.threshold_dbz != Some(key.threshold_dbz) {
            self.restart();
            self.threshold_dbz = Some(key.threshold_dbz);
        }
        let t = key.scan_key.scan_start.0;
        if let (Some((k, _)), Some(outcome)) = (&self.current, self.last_sync) {
            if k == &key {
                // Nothing changed since the last frame.
                return outcome;
            }
        }
        if let Some(stored) = self.frames.get(&t) {
            if stored.key.same_cut(&key) {
                self.current = Some((key, stored.frame.clone()));
                return TrackSync::LookedUp;
            }
            self.current = None;
            return TrackSync::Untracked;
        }
        if let Some(last) = self.tracker.last_time_ms() {
            if t < last {
                let first = self.frames.keys().next().copied();
                if first.is_none_or(|first| t < first) {
                    self.restart();
                } else {
                    self.current = None;
                    return TrackSync::Untracked;
                }
            }
        }

        // History between the last observed volume and this one.
        let last = self.tracker.last_time_ms();
        let mut chain = Vec::new();
        for h in history {
            let ht = h.scan_key.scan_start.0;
            if ht >= t || last.is_some_and(|l| ht <= l) {
                continue;
            }
            match cells.cached_result(h) {
                Some(result) => chain.push((h, ht, result)),
                None => {
                    self.current = None;
                    return TrackSync::Waiting;
                }
            }
        }
        let history_count = chain.len();
        for (h, ht, result) in chain {
            if let Some(frame) = self.tracker.observe(ht, result, false) {
                self.store(ht, h.clone(), frame);
            }
        }

        let provisional = cells.is_provisional();
        let Some(frame) = self.tracker.observe(t, &cells.result, provisional) else {
            self.current = None;
            return TrackSync::Untracked;
        };
        if !provisional {
            self.store(t, key.clone(), frame.clone());
        }
        self.current = Some((key, frame));
        if provisional {
            TrackSync::Previewed {
                history: history_count,
            }
        } else {
            TrackSync::Observed {
                history: history_count,
            }
        }
    }

    fn store(&mut self, t: i64, key: StormCellKey, frame: Frame) {
        self.frames.insert(t, StoredFrame { key, frame });
        while self.frames.len() > MAX_FRAMES {
            self.frames.pop_first();
        }
    }

    /// The tracks on the shown volume. `reference` is a point (the user's
    /// location) to compute closest approach to.
    pub(crate) fn view(&self, reference: Option<(f64, f64)>) -> Option<TracksView> {
        let frame = self.current()?;
        let tracks = frame
            .tracks
            .iter()
            .map(|s| {
                let confident = s.motion.confidence >= MIN_FORECAST_CONFIDENCE;
                TrackVm {
                    id: s.id,
                    label: track_label(s.id),
                    cell_id: s.cell_id,
                    lat: s.lat,
                    lon: s.lon,
                    trail: s.trail.clone(),
                    split_origin: s.split_origin,
                    forecast: confident.then(|| s.forecast(FORECAST_MINUTES)),
                    speed_kmh: s.motion.speed_kmh(),
                    direction_deg: s.motion.direction_deg(),
                    confidence: s.motion.confidence,
                    age_volumes: s.age_volumes,
                    age_minutes: s.age_minutes,
                    peak_trend_db: s.peak_trend_db,
                    stationary: s.stationary,
                    coasting: s.coasting,
                    from_reference: reference.map(|r| bearing_distance(r, (s.lat, s.lon))),
                    approach: reference
                        .filter(|_| confident)
                        .map(|r| closest_approach((s.lat, s.lon), &s.motion, r)),
                }
            })
            .collect();
        Some(TracksView {
            provisional: frame.provisional,
            tracks,
        })
    }
}

/// One track on the shown volume, ready to draw and describe.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TrackVm {
    pub id: TrackId,
    /// "A", "B", ..., "AA".
    pub label: String,
    /// The cell it is on; `None` while coasting.
    pub cell_id: Option<u32>,
    /// Current (or, coasting, advected) position.
    pub lat: f64,
    pub lon: f64,
    /// The storm's path: its observed centroids (up to 24), oldest first.
    pub trail: Vec<(f64, f64)>,
    /// The parent's centroid when this track split off: the path's join.
    pub split_origin: Option<(f64, f64)>,
    /// Position in [`FORECAST_MINUTES`], when the motion is confident
    /// enough to draw it.
    pub forecast: Option<(f64, f64)>,
    pub speed_kmh: f64,
    /// Compass direction of motion (toward).
    pub direction_deg: f64,
    /// Motion confidence, 0..1.
    pub confidence: f64,
    pub age_volumes: u32,
    pub age_minutes: f64,
    /// Peak change over the last three volumes, dB.
    pub peak_trend_db: Option<f32>,
    pub stationary: bool,
    pub coasting: bool,
    /// Bearing (degrees) and distance (km) from the reference point.
    pub from_reference: Option<(f64, f64)>,
    /// Closest approach to the reference point, when the motion is
    /// confident.
    pub approach: Option<Approach>,
}

/// The tracks on the shown volume.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TracksView {
    /// From a provisional (live) result.
    pub provisional: bool,
    /// By track id.
    pub tracks: Vec<TrackVm>,
}

impl TracksView {
    /// The track on cell `cell_id`.
    pub(crate) fn for_cell(&self, cell_id: u32) -> Option<&TrackVm> {
        self.tracks.iter().find(|t| t.cell_id == Some(cell_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{StormCell, StormCellsResult, Sweep};
    use crate::data::{ExtractedVcp, ExtractedVcpElevation};
    use crate::storm_cells::track::displace;
    use wasm_bindgen_test::wasm_bindgen_test;

    const T0_MS: i64 = 1_716_321_600_000;
    const VOL_MS: i64 = 5 * 60 * 1000;
    const ORIGIN: (f64, f64) = (41.5, -94.5);

    fn ms(vol: i64) -> i64 {
        T0_MS + vol * VOL_MS
    }

    fn key_at(vol: i64, input: CutInput, threshold_dbz: f32) -> StormCellKey {
        StormCellKey {
            scan_key: ScanKey::new("KDMX", UnixMillis(ms(vol))),
            elevation_number: 1,
            input,
            fallback: false,
            threshold_dbz,
        }
    }

    fn key(vol: i64) -> StormCellKey {
        key_at(vol, CutInput::Stored, 35.0)
    }

    fn cell(id: u32, east_km: f64, north_km: f64) -> StormCell {
        let (lat, lon) = displace(ORIGIN, east_km, north_km);
        StormCell {
            id,
            group_id: None,
            level_dbz: 35.0,
            peak_dbz: 55.0,
            mean_dbz: 45.0,
            area_km2: 100.0,
            gate_count: 100,
            lat,
            lon,
            bounds: (lat, lon, lat, lon),
            bearing_from_radar_deg: 0.0,
            range_from_radar_km: 50.0,
            orientation_deg: 0.0,
            elongation: 1.0,
            outline: Vec::new(),
            core_outline: Vec::new(),
        }
    }

    /// One storm moving east at 36 km/h (3 km per volume), plus, from
    /// volume 6 on, a second one 60 km north.
    fn result(vol: i64) -> StormCellsResult {
        let mut cells = vec![cell(0, vol as f64 * 3.0, 0.0)];
        if vol >= 6 {
            cells.push(cell(1, 0.0, 60.0));
        }
        StormCellsResult {
            cells,
            ..StormCellsResult::default()
        }
    }

    /// History keys for `vol` in these tests: the four volumes before it
    /// (not before 0). The real rule is [`history_keys`].
    fn history(vol: i64) -> Vec<StormCellKey> {
        ((vol - 4).max(0)..vol).map(key).collect()
    }

    /// Cache volumes `vols` (not shown).
    fn cache(cells: &mut StormCells, vols: impl IntoIterator<Item = i64>) {
        for v in vols {
            cells.seed(key(v), result(v), true, false);
        }
    }

    /// Show volume `vol` (cached) and sync.
    fn show(tracks: &mut StormTracks, cells: &mut StormCells, vol: i64) -> TrackSync {
        cells.seed(key(vol), result(vol), true, true);
        tracks.sync(true, cells, &history(vol))
    }

    fn label_of_cell(tracks: &StormTracks, cell_id: u32) -> String {
        tracks
            .view(None)
            .unwrap()
            .for_cell(cell_id)
            .unwrap()
            .label
            .clone()
    }

    #[wasm_bindgen_test]
    fn a_freshly_opened_volume_waits_for_its_history_then_has_motion() {
        let (mut tracks, mut cells) = (StormTracks::default(), StormCells::default());
        assert_eq!(show(&mut tracks, &mut cells, 5), TrackSync::Waiting);
        assert!(tracks.view(None).is_none());
        cache(&mut cells, [1, 2, 3]);
        assert_eq!(show(&mut tracks, &mut cells, 5), TrackSync::Waiting);
        cache(&mut cells, [4]);
        assert_eq!(
            show(&mut tracks, &mut cells, 5),
            TrackSync::Observed { history: 4 }
        );
        let view = tracks.view(None).unwrap();
        assert!(!view.provisional);
        let a = view.for_cell(0).unwrap();
        assert_eq!(a.label, "A");
        assert_eq!(a.age_volumes, 5);
        assert_eq!(a.trail.len(), 5, "the whole path so far");
        assert!((a.speed_kmh - 36.0).abs() < 0.5);
        assert!((a.direction_deg - 90.0).abs() < 1.0);
        let (lat, lon) = a.forecast.expect("confident after five volumes");
        let (dx, _) = crate::storm_cells::track::offset_km((a.lat, a.lon), (lat, lon));
        assert!((dx - 18.0).abs() < 0.3, "{dx}");
        assert_eq!(tracks.observed_times(), (1..=5).map(ms).collect::<Vec<_>>());
    }

    #[wasm_bindgen_test]
    fn scrubbing_back_and_forth_shows_the_same_tracks() {
        let (mut tracks, mut cells) = (StormTracks::default(), StormCells::default());
        cache(&mut cells, 0..=8);
        assert_eq!(
            show(&mut tracks, &mut cells, 6),
            TrackSync::Observed { history: 4 }
        );
        let at6 = tracks.view(None).unwrap();
        assert_eq!(label_of_cell(&tracks, 1), "B");
        // Back two volumes: looked up, not re-observed.
        assert_eq!(show(&mut tracks, &mut cells, 4), TrackSync::LookedUp);
        let at4 = tracks.view(None).unwrap();
        assert_eq!(at4.for_cell(0).unwrap().label, "A");
        assert_eq!(at4.for_cell(0).unwrap().age_volumes, 3);
        assert!(at4.for_cell(1).is_none());
        // Forward again: the same frame as before.
        assert_eq!(show(&mut tracks, &mut cells, 6), TrackSync::LookedUp);
        assert_eq!(tracks.view(None).unwrap(), at6);
        // On past it: observed once, IDs carry on.
        assert_eq!(
            show(&mut tracks, &mut cells, 8),
            TrackSync::Observed { history: 1 }
        );
        assert_eq!(label_of_cell(&tracks, 0), "A");
        assert_eq!(label_of_cell(&tracks, 1), "B");
        assert_eq!(show(&mut tracks, &mut cells, 7), TrackSync::LookedUp);
        assert_eq!(tracks.observed_times(), (2..=8).map(ms).collect::<Vec<_>>());
    }

    #[wasm_bindgen_test]
    fn a_volume_before_the_first_frame_restarts_with_new_labels() {
        let (mut tracks, mut cells) = (StormTracks::default(), StormCells::default());
        cache(&mut cells, 0..=8);
        show(&mut tracks, &mut cells, 8);
        assert_eq!(label_of_cell(&tracks, 0), "A");
        assert_eq!(label_of_cell(&tracks, 1), "B");
        // Volume 1 was never observed and is older than every frame.
        assert_eq!(
            show(&mut tracks, &mut cells, 1),
            TrackSync::Observed { history: 1 }
        );
        assert_eq!(label_of_cell(&tracks, 0), "C");
        assert_eq!(tracks.observed_times(), vec![ms(0), ms(1)]);
    }

    #[wasm_bindgen_test]
    fn a_volume_that_joined_between_frames_is_untracked() {
        let (mut tracks, mut cells) = (StormTracks::default(), StormCells::default());
        cache(&mut cells, 0..=8);
        // Observe 2 and 4..6 only: 3 was not in the timeline then.
        cells.seed(key(6), result(6), true, true);
        let sparse = vec![key(2), key(4), key(5)];
        assert_eq!(
            tracks.sync(true, &cells, &sparse),
            TrackSync::Observed { history: 3 }
        );
        assert_eq!(show(&mut tracks, &mut cells, 3), TrackSync::Untracked);
        assert!(tracks.view(None).is_none());
    }

    #[wasm_bindgen_test]
    fn a_live_volume_is_previewed_until_its_final_result() {
        let (mut tracks, mut cells) = (StormTracks::default(), StormCells::default());
        cache(&mut cells, 0..=6);
        show(&mut tracks, &mut cells, 6);
        // Volume 7 streaming: a provisional result.
        let live = key_at(7, CutInput::Live { radials: 300 }, 35.0);
        cells.seed(live.clone(), result(7), false, true);
        assert!(cells.is_provisional());
        assert_eq!(
            tracks.sync(true, &cells, &history(7)),
            TrackSync::Previewed { history: 0 }
        );
        let view = tracks.view(None).unwrap();
        assert!(view.provisional);
        assert_eq!(view.for_cell(0).unwrap().label, "A");
        assert_eq!(view.for_cell(0).unwrap().age_volumes, 6);
        assert!(!tracks.observed_times().contains(&ms(7)));
        // The same key next frame does not preview again.
        assert_eq!(
            tracks.sync(true, &cells, &history(7)),
            TrackSync::Previewed { history: 0 }
        );
        // The cut completes: stored result, observed for good.
        assert_eq!(
            show(&mut tracks, &mut cells, 7),
            TrackSync::Observed { history: 0 }
        );
        assert!(!tracks.view(None).unwrap().provisional);
        assert!(tracks.observed_times().contains(&ms(7)));
        // A live key for the same cut that finished counts as the same cut.
        let done = key_at(7, CutInput::Live { radials: 720 }, 35.0);
        cells.seed(done, result(7), true, true);
        assert_eq!(tracks.sync(true, &cells, &history(7)), TrackSync::LookedUp);
    }

    #[wasm_bindgen_test]
    fn threshold_change_restarts_and_site_change_resets() {
        let (mut tracks, mut cells) = (StormTracks::default(), StormCells::default());
        cache(&mut cells, 0..=6);
        show(&mut tracks, &mut cells, 6);
        assert_eq!(label_of_cell(&tracks, 1), "B");
        // Another threshold: other cells, so a restart (labels go on).
        cells.seed(key_at(6, CutInput::Stored, 45.0), result(6), true, true);
        let history45: Vec<StormCellKey> =
            (2..6).map(|v| key_at(v, CutInput::Stored, 45.0)).collect();
        for (v, k) in (2..6).zip(&history45) {
            cells.seed(k.clone(), result(v), true, false);
        }
        assert_eq!(
            tracks.sync(true, &cells, &history45),
            TrackSync::Observed { history: 4 }
        );
        assert_eq!(label_of_cell(&tracks, 0), "C");
        // New site: everything forgotten, labels from A again.
        tracks.reset();
        let mut cells = StormCells::default();
        cache(&mut cells, 0..=6);
        show(&mut tracks, &mut cells, 6);
        assert_eq!(label_of_cell(&tracks, 0), "A");
    }

    #[wasm_bindgen_test]
    fn off_or_nothing_shown_means_no_view() {
        let (mut tracks, mut cells) = (StormTracks::default(), StormCells::default());
        assert_eq!(tracks.sync(true, &cells, &[]), TrackSync::NoCells);
        cache(&mut cells, 0..=2);
        cells.seed(key(2), result(2), true, true);
        assert_eq!(tracks.sync(false, &cells, &history(2)), TrackSync::Off);
        assert!(tracks.view(None).is_none());
        assert_eq!(
            tracks.sync(true, &cells, &history(2)),
            TrackSync::Observed { history: 2 }
        );
        assert!(tracks.view(None).is_some());
    }

    #[wasm_bindgen_test]
    fn the_view_gives_closest_approach_to_a_reference_point() {
        let (mut tracks, mut cells) = (StormTracks::default(), StormCells::default());
        cache(&mut cells, 0..=4);
        show(&mut tracks, &mut cells, 4);
        // The storm is at 12 km east, moving east at 36 km/h; I am 30 km
        // further east and 4 km north.
        let me = displace(ORIGIN, 42.0, 4.0);
        let view = tracks.view(Some(me)).unwrap();
        let a = view.for_cell(0).unwrap();
        let approach = a.approach.expect("confident motion");
        assert!((approach.minutes - 50.0).abs() < 1.0, "{approach:?}");
        assert!((approach.distance_km - 4.0).abs() < 0.2, "{approach:?}");
        let (bearing, dist) = a.from_reference.unwrap();
        assert!((bearing - 262.4).abs() < 1.0 && (dist - 30.27).abs() < 0.3);
        // A young track has no confident motion: no approach, no arrow.
        let (mut tracks, mut cells) = (StormTracks::default(), StormCells::default());
        cache(&mut cells, [0]);
        cells.seed(key(1), result(1), true, true);
        tracks.sync(true, &cells, &[key(0)]);
        let a = tracks.view(Some(me)).unwrap().tracks[0].clone();
        assert!(a.approach.is_none() && a.forecast.is_none());
        assert!(a.from_reference.is_some());
    }

    fn scan(vol: i64, stored_cuts: &[u8]) -> Scan {
        let angles = [0.5_f32, 0.5, 0.9];
        Scan {
            start_time: ms(vol) as f64 / 1000.0,
            end_time: ms(vol) as f64 / 1000.0 + 280.0,
            key_timestamp: ms(vol) as f64 / 1000.0,
            vcp: 215,
            vcp_pattern: Some(ExtractedVcp {
                number: 215,
                elevations: angles
                    .iter()
                    .map(|&angle| ExtractedVcpElevation {
                        angle,
                        waveform: "CS".into(),
                        prf_number: 1,
                        is_sails: false,
                        is_mrle: false,
                        is_base_tilt: false,
                        azimuth_rate: None,
                    })
                    .collect(),
            }),
            sweeps: stored_cuts
                .iter()
                .map(|&e| Sweep {
                    start_time: 0.0,
                    end_time: 0.0,
                    elevation: angles[e as usize - 1],
                    elevation_number: e,
                    start_azimuth: 0.0,
                    radials: Vec::new(),
                    cached_products: vec!["reflectivity".into()],
                })
                .collect(),
            completeness: None,
            cached_sweep_count: None,
            planned_sweep_count: None,
        }
    }

    #[wasm_bindgen_test]
    fn history_is_the_previous_stored_lowest_cuts_up_to_a_gap() {
        let displayed = ScanKey::new("KDMX", UnixMillis(ms(10)));
        // 0 .. 9, five minutes apart; volume 7 holds only cut 3.
        let mut scans: Vec<Scan> = (0..10).map(|v| scan(v, &[1, 2, 3])).collect();
        scans[7] = scan(7, &[3]);
        scans.push(scan(10, &[1]));
        scans.push(scan(11, &[1]));
        let keys = history_keys(&scans, &displayed, 35.0);
        let vols: Vec<i64> = keys
            .iter()
            .map(|k| (k.scan_key.scan_start.0 - T0_MS) / VOL_MS)
            .collect();
        // Every volume back (0..9), 7 skipped: its lowest cut is not stored.
        assert_eq!(vols, vec![0, 1, 2, 3, 4, 5, 6, 8, 9]);
        assert!(keys.iter().all(|k| k.elevation_number == 1
            && k.input == CutInput::Stored
            && !k.fallback
            && k.threshold_dbz == 35.0
            && k.scan_key.site.0 == "KDMX"));
        // A 30-minute hole before volume 8 stops the walk there.
        let mut gappy: Vec<Scan> = (0..3).map(|v| scan(v, &[1])).collect();
        gappy.extend((9..12).map(|v| scan(v, &[1])));
        let displayed = ScanKey::new("KDMX", UnixMillis(ms(11)));
        let vols: Vec<i64> = history_keys(&gappy, &displayed, 35.0)
            .iter()
            .map(|k| (k.scan_key.scan_start.0 - T0_MS) / VOL_MS)
            .collect();
        assert_eq!(vols, vec![9, 10]);
        // At most twelve volumes (an hour) back, nearest included.
        let long: Vec<Scan> = (0..20).map(|v| scan(v, &[1])).collect();
        let displayed = ScanKey::new("KDMX", UnixMillis(ms(20)));
        let vols: Vec<i64> = history_keys(&long, &displayed, 35.0)
            .iter()
            .map(|k| (k.scan_key.scan_start.0 - T0_MS) / VOL_MS)
            .collect();
        assert_eq!(vols, (8..20).collect::<Vec<_>>());
        assert_eq!(HISTORY_VOLUMES, 12);
        // Nothing before the first volume.
        let displayed = ScanKey::new("KDMX", UnixMillis(ms(0)));
        assert!(history_keys(&scans, &displayed, 35.0).is_empty());
    }
}
