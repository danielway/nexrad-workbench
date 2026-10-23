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
//! # Scrubbing, playback and late results
//!
//! The timeline moves both ways and detection results arrive in any order
//! (a loop can outrun detection, history lands after the shown volume),
//! but the tracker only moves forward. So the store keeps, per volume
//! time, the frame the tracker gave (cell → track, every track's state)
//! and the tracker right after it. For the shown volume at time `t`:
//!
//! 1. **Seen before:** `t` has a frame for this cut. Show it; nothing is
//!    observed again, so scrubbing back and forth, or a loop wrapping to
//!    its start, always shows the same IDs, trails and arrows.
//! 2. **Not seen yet:** observe `t` from the tracker after the nearest
//!    frame before it (or the run's start), then observe every later frame
//!    again, in time order, from its cached cells, replacing it. A volume
//!    whose result arrives late (after the playhead moved on and later
//!    volumes were tracked) is therefore tracked like any other, and letters
//!    already shown stay with their storms.
//! 3. **History:** the [`HISTORY_VOLUMES`] volumes before `t` that
//!    [`history_keys`] names are observed the same way as their cells land
//!    (`storm_cells::decide` asks for them nearest first). The shown volume
//!    does not wait for them: it is tracked from whatever is cached, and
//!    its tracks gain motion and path as history fills in.
//! 4. **Far before every frame** (more than the tracker's `max_gap_ms`
//!    before the first one): nothing links the two, so the store starts a
//!    new run there. The ID counter is kept, so the new run's tracks get
//!    new labels.
//!
//! **Letters.** The tracker's IDs are internal; the letters shown are
//! handed out here, from a counter that only moves forward. A track gets
//! one once it has been seen in two cuts (two volumes on one site; on the
//! scene, two cuts of the radar that shapes it), or at once when it split
//! off a lettered track; a track seen once has none and uses none up. Each
//! lettered track's centroids remember its letter, so when a replay
//! observes a track again (even one that now becomes lettered a volume
//! earlier) it takes the same letter back instead of a new one.
//!
//! A frame whose cells have left the detection cache cannot be observed
//! again; a replay drops it, and it is observed again when shown. A
//! provisional (live, partial-cut) result is previewed from the tracker
//! before it and not stored; the final result for that volume is observed
//! when it arrives. A threshold change restarts the store (the cells
//! differ), and a site change resets it, ID counter included.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::core::storm_cells::{
    has_reflectivity, lowest_reflectivity_cut, CellKey, CutInput, SceneCellKey, StormCellKey,
    StormCells,
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
/// oldest first: the volumes the tracker should see before the displayed
/// one. `scans` is the timeline (sorted by start). Walking back
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

/// The scene keys before `shown` that tracking asks to have detected,
/// oldest first: at every volume start (on any of its sites) before the
/// shown scene's time, the scene of each site's newest stored volume
/// started by then, from [`history_keys`] per site plus the site's shown
/// cut (`scans[i]` is the timeline of `shown.sites[i]`). A site with no
/// such volume yet is left out of that scene, as a site with nothing
/// displayed is left out of the shown one. At most [`HISTORY_VOLUMES`]
/// scenes.
pub(crate) fn scene_history_keys(scans: &[&[Scan]], shown: &SceneCellKey) -> Vec<SceneCellKey> {
    // Each site's stored volumes before its shown one, and the shown one
    // itself once stored (a scene before the shown one can hold it).
    let per_site: Vec<Vec<StormCellKey>> = shown
        .sites
        .iter()
        .zip(scans)
        .map(|(k, s)| {
            let mut keys = history_keys(s, &k.scan_key, shown.threshold_dbz);
            keys.extend((k.input == CutInput::Stored).then(|| k.clone()));
            keys
        })
        .collect();
    if per_site.len() != shown.sites.len() {
        return Vec::new();
    }
    let t = shown.time_ms();
    let starts: BTreeSet<i64> = per_site
        .iter()
        .flatten()
        .map(|k| k.scan_key.scan_start.0)
        .filter(|&ms| ms < t)
        .collect();
    let mut keys = Vec::new();
    for &at in starts.iter().rev() {
        let sites: Vec<StormCellKey> = per_site
            .iter()
            .filter_map(|site| {
                site.iter()
                    .rev()
                    .find(|k| k.scan_key.scan_start.0 <= at)
                    .cloned()
            })
            .collect();
        keys.push(SceneCellKey {
            sites,
            threshold_dbz: shown.threshold_dbz,
        });
        if keys.len() == HISTORY_VOLUMES {
            break;
        }
    }
    keys.reverse();
    keys
}

#[derive(Clone, Debug)]
struct StoredFrame<K> {
    key: K,
    frame: Frame,
    /// The tracker right after this volume: where a volume inserted after
    /// this one starts from.
    after: Tracker,
    /// The letters of this volume's lettered tracks.
    letters: Letters,
}

/// Letter numbers ([`track_label`]) by tracker ID, for one volume.
type Letters = BTreeMap<TrackId, u32>;

/// A centroid's exact position: detection is deterministic, so the same
/// cell observed again (on a replay) has the same bits.
fn point_key((lat, lon): (f64, f64)) -> (u64, u64) {
    (lat.to_bits(), lon.to_bits())
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
    /// The shown volume, or `history` volumes before it, were observed,
    /// and `replayed` later frames observed again after them.
    Observed { history: usize, replayed: usize },
    /// The shown result is provisional: previewed, nothing stored (bar
    /// `history` volumes before it that were observed).
    Previewed { history: usize },
    /// The shown volume could not be tracked.
    Untracked,
}

/// The track store: one for the focus site, one for the scene of several
/// sites (keyed by [`SceneCellKey`]).
#[derive(Clone, Debug)]
pub(crate) struct StormTracks<K = StormCellKey> {
    /// The tracker a run starts from: no tracks, and the ID counter the
    /// run's first volume starts at.
    run_start: Tracker,
    /// Final frames by volume time.
    frames: BTreeMap<i64, StoredFrame<K>>,
    /// The threshold the stored frames were detected at.
    threshold_dbz: Option<f32>,
    /// The shown volume's key, frame and letters.
    current: Option<(K, Frame, Letters)>,
    /// The next letter to hand out. Only moves forward (a site change
    /// resets it), so a letter names one storm.
    next_letter: u32,
    /// The letter each lettered track's centroids carry, so a track
    /// observed again on a replay keeps its letter.
    letter_of_point: HashMap<(u64, u64), u32>,
    /// The last [`StormTracks::sync`] outcome.
    pub last_sync: Option<TrackSync>,
    /// Volumes observed so far (replays included), for the debug log.
    observations: u64,
}

impl<K: CellKey> Default for StormTracks<K> {
    fn default() -> Self {
        let mut run_start = Tracker::default();
        run_start.params.coast_ms = K::COAST_MS;
        Self {
            run_start,
            frames: BTreeMap::new(),
            threshold_dbz: None,
            current: None,
            next_letter: 0,
            letter_of_point: HashMap::new(),
            last_sync: None,
            observations: 0,
        }
    }
}

impl<K: CellKey> StormTracks<K> {
    /// Forget everything, ID counter included. For a site change.
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }

    /// The tracker after the newest frame (or the run's start).
    fn latest(&self) -> &Tracker {
        self.frames
            .values()
            .next_back()
            .map_or(&self.run_start, |f| &f.after)
    }

    /// The tracker to observe a volume at `t` from: the one after the
    /// nearest frame before `t`, or the run's start.
    fn state_before(&self, t: i64) -> Tracker {
        self.frames
            .range(..t)
            .next_back()
            .map_or_else(|| self.run_start.clone(), |(_, f)| f.after.clone())
    }

    /// Drop the frames and live tracks, keeping the ID counter.
    fn restart(&mut self) {
        let mut start = self.latest().clone();
        start.restart();
        self.run_start = start;
        self.frames.clear();
        self.current = None;
        self.letter_of_point.clear();
    }

    /// The letters before the volume at `t`: those of the nearest frame
    /// before it.
    fn letters_before(&self, t: i64) -> Letters {
        self.frames
            .range(..t)
            .next_back()
            .map(|(_, f)| f.letters.clone())
            .unwrap_or_default()
    }

    /// Letter the tracks of `frame`, given the letters of the volume before
    /// (`prev`). A track keeps its letter; a track without one gets one
    /// once it has been seen twice, or at once when it split off a lettered
    /// track. It takes back the letter its centroids already carry (a
    /// replay), else the next one when `allocate` (final results only); a
    /// track seen once has none and uses none up.
    ///
    /// `points` is every centroid each track has in the volumes being
    /// observed (later ones included), newest last: on a replay a track
    /// can become lettered earlier than before, and should take back the
    /// letter its later centroids carry rather than a new one.
    fn letter(
        &mut self,
        frame: &Frame,
        prev: &Letters,
        points: &HashMap<TrackId, Vec<(f64, f64)>>,
        allocate: bool,
    ) -> Letters {
        let mut out = Letters::new();
        let mut used = BTreeSet::new();
        for s in &frame.tracks {
            if let Some(&l) = prev.get(&s.id) {
                out.insert(s.id, l);
                used.insert(l);
            }
        }
        for s in &frame.tracks {
            if out.contains_key(&s.id) {
                continue;
            }
            let split_off_lettered = s
                .split_from
                .is_some_and(|p| out.contains_key(&p) || prev.contains_key(&p));
            if s.age_volumes < 2 && !split_off_lettered {
                continue;
            }
            let carried = points
                .get(&s.id)
                .map_or(&s.trail, |p| p)
                .iter()
                .rev()
                .filter_map(|&p| self.letter_of_point.get(&point_key(p)).copied())
                .find(|l| !used.contains(l));
            let l = match carried {
                Some(l) => l,
                None if allocate => {
                    self.next_letter += 1;
                    self.next_letter - 1
                }
                None => continue,
            };
            out.insert(s.id, l);
            used.insert(l);
        }
        if allocate {
            for s in &frame.tracks {
                if let Some(&l) = out.get(&s.id) {
                    for &p in &s.trail {
                        self.letter_of_point.insert(point_key(p), l);
                    }
                }
            }
        }
        out
    }

    /// Whether the volume of `key` has a frame for that cut.
    fn framed(&self, key: &K) -> bool {
        self.frames
            .get(&key.time_ms())
            .is_some_and(|f| f.key.same_cut(key))
    }

    /// The newest letter handed out so far on this site, if any. For the
    /// debug log: how quickly letters are used up measures how stable the
    /// tracking is.
    pub(crate) fn newest_label(&self) -> Option<String> {
        self.next_letter.checked_sub(1).map(track_label)
    }

    /// Volumes observed so far on this site, replays included.
    pub(crate) fn observations(&self) -> u64 {
        self.observations
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
        cells: &StormCells<K>,
        history: &[K],
    ) -> TrackSync {
        let outcome = self.sync_inner(enabled, cells, history);
        self.last_sync = Some(outcome);
        outcome
    }

    fn sync_inner(&mut self, enabled: bool, cells: &StormCells<K>, history: &[K]) -> TrackSync {
        if !enabled {
            self.current = None;
            return TrackSync::Off;
        }
        let Some(key) = cells.computed_for.clone() else {
            self.current = None;
            return TrackSync::NoCells;
        };
        if self.threshold_dbz != Some(key.threshold_dbz()) {
            self.restart();
            self.threshold_dbz = Some(key.threshold_dbz());
        }
        let t = key.time_ms();
        // Far before every frame: nothing links the two, start a new run.
        let max_gap_ms = self.run_start.params.max_gap_ms;
        if self
            .frames
            .keys()
            .next()
            .is_some_and(|&first| t + max_gap_ms < first)
        {
            self.restart();
        }

        // History volumes whose cells have landed but are not tracked yet.
        let pending: Vec<K> = history
            .iter()
            .filter(|h| h.time_ms() < t)
            .filter(|h| !self.framed(h) && cells.cached_result(h).is_some())
            .cloned()
            .collect();
        if pending.is_empty() {
            if let (Some((k, _, _)), Some(outcome)) = (&self.current, self.last_sync) {
                if k == &key {
                    // Nothing changed since the last frame.
                    return outcome;
                }
            }
        }

        let provisional = cells.is_provisional();
        let mut new: Vec<(i64, K)> = pending.iter().map(|h| (h.time_ms(), h.clone())).collect();
        if !provisional && !self.framed(&key) {
            new.push((t, key.clone()));
        }
        let observed = new.len();
        let replayed = self.insert(new, cells);

        if provisional {
            let mut tracker = self.state_before(t);
            let Some(frame) = tracker.observe(t, &cells.result, true) else {
                self.current = None;
                return TrackSync::Untracked;
            };
            let prev = self.letters_before(t);
            let letters = self.letter(&frame, &prev, &HashMap::new(), false);
            self.current = Some((key, frame, letters));
            return TrackSync::Previewed {
                history: pending.len(),
            };
        }
        let Some(stored) = self.frames.get(&t).filter(|f| f.key.same_cut(&key)) else {
            self.current = None;
            return TrackSync::Untracked;
        };
        self.current = Some((key, stored.frame.clone(), stored.letters.clone()));
        if observed == 0 {
            TrackSync::LookedUp
        } else {
            TrackSync::Observed {
                history: pending.len(),
                replayed,
            }
        }
    }

    /// Observe the `new` volumes and, after the earliest of them, observe
    /// every later frame again in time order (from cached cells; a frame
    /// whose cells were evicted is dropped, to be observed again when it is
    /// shown). Returns the number of later frames observed again.
    fn insert(&mut self, new: Vec<(i64, K)>, cells: &StormCells<K>) -> usize {
        let Some(x0) = new.iter().map(|(t, _)| *t).min() else {
            return 0;
        };
        let mut tracker = self.state_before(x0);
        let mut letters = self.letters_before(x0);
        let mut seq: BTreeMap<i64, K> = self
            .frames
            .split_off(&x0)
            .into_iter()
            .map(|(t, f)| (t, f.key))
            .collect();
        let mut replayed = seq.len();
        for (t, key) in new {
            if seq.insert(t, key).is_some() {
                replayed -= 1;
            }
        }
        let mut observed = Vec::new();
        let mut points: HashMap<TrackId, Vec<(f64, f64)>> = HashMap::new();
        for (t, key) in seq {
            let Some(result) = cells.cached_result(&key) else {
                continue;
            };
            if let Some(frame) = tracker.observe(t, result, false) {
                self.observations += 1;
                for s in frame.tracks.iter().filter(|s| s.cell_id.is_some()) {
                    points.entry(s.id).or_default().push((s.lat, s.lon));
                }
                observed.push((t, key, frame, tracker.clone()));
            }
        }
        for (t, key, frame, after) in observed {
            letters = self.letter(&frame, &letters, &points, true);
            self.frames.insert(
                t,
                StoredFrame {
                    key,
                    frame,
                    after,
                    letters: letters.clone(),
                },
            );
        }
        while self.frames.len() > MAX_FRAMES {
            self.frames.pop_first();
        }
        replayed
    }

    /// The tracks on the shown volume. `reference` is a point (the user's
    /// location) to compute closest approach to.
    pub(crate) fn view(&self, reference: Option<(f64, f64)>) -> Option<TracksView> {
        let (_, frame, letters) = self.current.as_ref()?;
        let tracks = frame
            .tracks
            .iter()
            .filter_map(|s| Some((s, *letters.get(&s.id)?)))
            .map(|(s, letter)| {
                let confident = s.motion.confidence >= MIN_FORECAST_CONFIDENCE;
                TrackVm {
                    id: s.id,
                    label: track_label(letter),
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
            sites: Vec::new(),
            observed_ms: None,
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

    fn observed(history: usize, replayed: usize) -> TrackSync {
        TrackSync::Observed { history, replayed }
    }

    #[wasm_bindgen_test]
    fn a_freshly_opened_volume_is_tracked_at_once_and_gains_motion_as_history_lands() {
        let (mut tracks, mut cells) = (StormTracks::default(), StormCells::default());
        // No history cached yet: tracked anyway, but seen once, so no
        // letter yet (and none used up).
        assert_eq!(show(&mut tracks, &mut cells, 5), observed(0, 0));
        assert!(tracks.view(None).unwrap().for_cell(0).is_none());
        assert_eq!(tracks.newest_label(), None);
        // Part of the history lands: observed before 5, and 5 again.
        cache(&mut cells, [1, 2, 3]);
        assert_eq!(tracks.sync(true, &cells, &history(5)), observed(3, 1));
        assert_eq!(
            tracks.view(None).unwrap().for_cell(0).unwrap().age_volumes,
            4
        );
        // The rest: the same storm, the same letter, the whole path.
        cache(&mut cells, [4]);
        assert_eq!(tracks.sync(true, &cells, &history(5)), observed(1, 1));
        // Nothing new: nothing observed.
        assert_eq!(
            tracks.sync(true, &cells, &history(5)),
            observed(1, 1),
            "the outcome is kept, not redone"
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
        assert_eq!(tracks.newest_label().as_deref(), Some("A"));
    }

    #[wasm_bindgen_test]
    fn scrubbing_back_and_forth_shows_the_same_tracks() {
        let (mut tracks, mut cells) = (StormTracks::default(), StormCells::default());
        // 0 and 1 are not detected, so going back to 4 finds no history to
        // add.
        cache(&mut cells, 2..=8);
        assert_eq!(show(&mut tracks, &mut cells, 6), observed(4, 0));
        let at6 = tracks.view(None).unwrap();
        assert!(at6.for_cell(1).is_none(), "the second storm is new at 6");
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
        assert_eq!(show(&mut tracks, &mut cells, 8), observed(1, 0));
        assert_eq!(label_of_cell(&tracks, 0), "A");
        assert_eq!(label_of_cell(&tracks, 1), "B");
        assert_eq!(show(&mut tracks, &mut cells, 7), TrackSync::LookedUp);
        assert_eq!(tracks.observed_times(), (2..=8).map(ms).collect::<Vec<_>>());
    }

    #[wasm_bindgen_test]
    fn a_volume_just_before_the_first_frame_joins_the_run() {
        let (mut tracks, mut cells) = (StormTracks::default(), StormCells::default());
        cache(&mut cells, 0..=8);
        show(&mut tracks, &mut cells, 8);
        assert_eq!(tracks.observed_times(), (4..=8).map(ms).collect::<Vec<_>>());
        // Volume 1 (and its history, 0) come before every frame but within
        // the tracker's gap: observed, and 4..8 again after them.
        assert_eq!(show(&mut tracks, &mut cells, 1), observed(1, 5));
        assert_eq!(label_of_cell(&tracks, 0), "A");
        let mut times = vec![ms(0), ms(1)];
        times.extend((4..=8).map(ms));
        assert_eq!(tracks.observed_times(), times);
        assert_eq!(show(&mut tracks, &mut cells, 8), TrackSync::LookedUp);
        assert_eq!(label_of_cell(&tracks, 0), "A");
        assert_eq!(label_of_cell(&tracks, 1), "B");
    }

    #[wasm_bindgen_test]
    fn a_volume_far_before_the_first_frame_starts_a_new_run_with_new_labels() {
        let (mut tracks, mut cells) = (StormTracks::default(), StormCells::default());
        cache(&mut cells, 0..=20);
        show(&mut tracks, &mut cells, 20);
        assert_eq!(label_of_cell(&tracks, 0), "A");
        assert_eq!(label_of_cell(&tracks, 1), "B");
        // Volume 2 is an hour before every frame.
        assert_eq!(show(&mut tracks, &mut cells, 2), observed(2, 0));
        assert_eq!(label_of_cell(&tracks, 0), "C");
        assert_eq!(tracks.observed_times(), vec![ms(0), ms(1), ms(2)]);
    }

    #[wasm_bindgen_test]
    fn a_late_volume_between_frames_is_tracked_and_later_frames_replayed() {
        let (mut tracks, mut cells) = (StormTracks::default(), StormCells::default());
        // Volumes 2 and 4..6 tracked; 3's cells had not landed then.
        for v in [2, 4, 5] {
            cells.seed(key(v), result(v), true, false);
        }
        cells.seed(key(6), result(6), true, true);
        let sparse = vec![key(2), key(3), key(4), key(5)];
        assert_eq!(tracks.sync(true, &cells, &sparse), observed(3, 0));
        let a6 = tracks.view(None).unwrap().for_cell(0).unwrap().clone();
        assert_eq!((a6.label.as_str(), a6.age_volumes), ("A", 4));
        // 3 is shown and its cells land late (with 0 and 1 as history).
        cache(&mut cells, [0, 1]);
        assert_eq!(show(&mut tracks, &mut cells, 3), observed(2, 4));
        let a3 = tracks.view(None).unwrap().for_cell(0).unwrap().clone();
        assert_eq!((a3.label.as_str(), a3.age_volumes), ("A", 4));
        assert_eq!(tracks.observed_times(), (0..=6).map(ms).collect::<Vec<_>>());
        // The later frames were observed again: 6 now has the longer path.
        assert_eq!(show(&mut tracks, &mut cells, 6), TrackSync::LookedUp);
        let a6 = tracks.view(None).unwrap().for_cell(0).unwrap().clone();
        assert_eq!((a6.label.as_str(), a6.age_volumes), ("A", 7));
        assert!(tracks.view(None).unwrap().for_cell(1).is_none());
        assert_eq!(tracks.newest_label().as_deref(), Some("A"));
    }

    #[wasm_bindgen_test]
    fn a_loop_that_outruns_detection_ends_with_every_volume_tracked() {
        let (mut tracks, mut cells) = (StormTracks::default(), StormCells::default());
        // Pass one: the playhead runs 0..=8, but 3 and 5 are shown before
        // their cells land, so they are never seen by the store.
        let skipped = [3, 5];
        for v in 0..=8 {
            if skipped.contains(&v) {
                continue;
            }
            show(&mut tracks, &mut cells, v);
        }
        let after_pass_one = tracks.newest_label();
        assert_eq!(after_pass_one.as_deref(), Some("B"));
        // The wrap back to 0 looks up; nothing restarts.
        assert_eq!(show(&mut tracks, &mut cells, 0), TrackSync::LookedUp);
        assert_eq!(tracks.newest_label(), after_pass_one);
        assert!(
            tracks.view(None).unwrap().for_cell(0).is_none(),
            "seen once at 0"
        );
        // Pass two: every volume shown again, 3 and 5 now detected.
        for v in 1..=8 {
            let outcome = show(&mut tracks, &mut cells, v);
            assert!(
                matches!(outcome, TrackSync::LookedUp | TrackSync::Observed { .. }),
                "{v}: {outcome:?}"
            );
            assert_eq!(label_of_cell(&tracks, 0), "A", "volume {v}");
        }
        assert_eq!(tracks.observed_times(), (0..=8).map(ms).collect::<Vec<_>>());
        // A third pass changes nothing: every volume is looked up.
        for v in 0..=8 {
            assert_eq!(show(&mut tracks, &mut cells, v), TrackSync::LookedUp);
        }
        assert_eq!(tracks.newest_label(), after_pass_one);
        assert_eq!(label_of_cell(&tracks, 1), "B");
    }

    /// Show `vol` with these cells, history: every earlier volume given.
    fn show_cells(
        tracks: &mut StormTracks,
        cells: &mut StormCells,
        vol: i64,
        list: Vec<StormCell>,
        history: &[i64],
    ) -> TrackSync {
        let r = StormCellsResult {
            cells: list,
            ..StormCellsResult::default()
        };
        cells.seed(key(vol), r, true, true);
        let keys: Vec<StormCellKey> = history.iter().map(|&v| key(v)).collect();
        tracks.sync(true, cells, &keys)
    }

    fn label_opt(tracks: &StormTracks, cell_id: u32) -> Option<String> {
        tracks
            .view(None)
            .and_then(|v| v.for_cell(cell_id).map(|t| t.label.clone()))
    }

    #[wasm_bindgen_test]
    fn a_cell_seen_once_never_gets_a_letter_or_uses_one_up() {
        let (mut tracks, mut cells) = (StormTracks::default(), StormCells::default());
        for v in 0..6 {
            // The storm, plus a flash 40 km further along each volume.
            let list = vec![
                cell(0, v as f64 * 3.0, 0.0),
                cell(1, -100.0 + 40.0 * v as f64, 60.0),
            ];
            show_cells(&mut tracks, &mut cells, v, list, &[]);
            assert_eq!(label_opt(&tracks, 1), None, "volume {v}");
        }
        assert_eq!(label_opt(&tracks, 0).as_deref(), Some("A"));
        assert_eq!(tracks.newest_label().as_deref(), Some("A"));
    }

    #[wasm_bindgen_test]
    fn a_track_lettered_on_its_second_volume_keeps_that_letter() {
        let (mut tracks, mut cells) = (StormTracks::default(), StormCells::default());
        let at = |v: i64| vec![cell(0, v as f64 * 3.0, 0.0)];
        show_cells(&mut tracks, &mut cells, 0, at(0), &[]);
        assert_eq!(label_opt(&tracks, 0), None);
        for v in 1..5 {
            show_cells(&mut tracks, &mut cells, v, at(v), &[]);
            assert_eq!(label_opt(&tracks, 0).as_deref(), Some("A"), "volume {v}");
        }
        // Scrubbing back: the second volume still says A, the first nothing.
        show_cells(&mut tracks, &mut cells, 1, at(1), &[]);
        assert_eq!(label_opt(&tracks, 0).as_deref(), Some("A"));
        show_cells(&mut tracks, &mut cells, 0, at(0), &[]);
        assert_eq!(label_opt(&tracks, 0), None);
        assert_eq!(tracks.newest_label().as_deref(), Some("A"));
    }

    #[wasm_bindgen_test]
    fn a_late_volume_letters_a_track_it_makes_seen_twice() {
        let (mut tracks, mut cells) = (StormTracks::default(), StormCells::default());
        let a = |v: i64| cell(0, v as f64 * 3.0, 0.0);
        let b = |v: i64| cell(1, v as f64 * 3.0, 50.0);
        show_cells(&mut tracks, &mut cells, 0, vec![a(0)], &[]);
        show_cells(&mut tracks, &mut cells, 1, vec![a(1)], &[]);
        // B first seen at 3 (2's cells have not landed): no letter.
        show_cells(&mut tracks, &mut cells, 3, vec![a(3), b(3)], &[0, 1, 2]);
        assert_eq!(label_opt(&tracks, 0).as_deref(), Some("A"));
        assert_eq!(label_opt(&tracks, 1), None);
        // 2 lands late with B in it: B is now seen twice, at 3 it is B.
        cells.seed(
            key(2),
            StormCellsResult {
                cells: vec![a(2), b(2)],
                ..StormCellsResult::default()
            },
            true,
            false,
        );
        show_cells(&mut tracks, &mut cells, 3, vec![a(3), b(3)], &[0, 1, 2]);
        assert_eq!(label_opt(&tracks, 0).as_deref(), Some("A"));
        assert_eq!(label_opt(&tracks, 1).as_deref(), Some("B"));
    }

    #[wasm_bindgen_test]
    fn a_split_off_a_lettered_storm_gets_a_letter_at_once() {
        let (mut tracks, mut cells) = (StormTracks::default(), StormCells::default());
        for v in 0..3 {
            show_cells(
                &mut tracks,
                &mut cells,
                v,
                vec![cell(0, v as f64 * 3.0, 0.0)],
                &[],
            );
        }
        // Volume 3: the storm splits; the piece 8 km south is new.
        let list = vec![cell(0, 9.0, 0.0), cell(1, 9.0, -8.0)];
        show_cells(&mut tracks, &mut cells, 3, list, &[]);
        assert_eq!(label_opt(&tracks, 0).as_deref(), Some("A"));
        let child = tracks.view(None).unwrap().for_cell(1).cloned().unwrap();
        assert_eq!((child.label.as_str(), child.age_volumes), ("B", 1));
        assert!(child.split_origin.is_some());
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
        assert_eq!(show(&mut tracks, &mut cells, 7), observed(0, 0));
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
        assert_eq!(label_of_cell(&tracks, 0), "A");
        // Another threshold: other cells, so a restart (labels go on).
        cells.seed(key_at(6, CutInput::Stored, 45.0), result(6), true, true);
        let history45: Vec<StormCellKey> =
            (2..6).map(|v| key_at(v, CutInput::Stored, 45.0)).collect();
        for (v, k) in (2..6).zip(&history45) {
            cells.seed(k.clone(), result(v), true, false);
        }
        assert_eq!(tracks.sync(true, &cells, &history45), observed(4, 0));
        assert_eq!(label_of_cell(&tracks, 0), "B");
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
        assert_eq!(tracks.sync(true, &cells, &history(2)), observed(2, 0));
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

    // ---- the scene --------------------------------------------------------

    fn site_key(site: &str, at_ms: i64) -> StormCellKey {
        StormCellKey {
            scan_key: ScanKey::new(site, UnixMillis(at_ms)),
            ..key(0)
        }
    }

    /// The scene at volume `vol`: KAAA's volume and KBBB's, started a
    /// minute before it.
    fn scene_key(vol: i64) -> SceneCellKey {
        SceneCellKey {
            sites: vec![
                site_key("KAAA", ms(vol)),
                site_key("KBBB", ms(vol) - 60_000),
            ],
            threshold_dbz: 35.0,
        }
    }

    #[wasm_bindgen_test]
    fn the_scene_is_tracked_with_one_letter_pool() {
        let mut tracks: StormTracks<SceneCellKey> = StormTracks::default();
        let mut cells: StormCells<SceneCellKey> = StormCells::default();
        for vol in 0..4 {
            cells.seed(scene_key(vol), result(vol), true, true);
            let history: Vec<SceneCellKey> = (0..vol).map(scene_key).collect();
            tracks.sync(true, &cells, &history);
        }
        let view = tracks.view(None).unwrap();
        let a = view.for_cell(0).unwrap();
        assert_eq!(a.label, "A");
        assert_eq!(a.age_volumes, 4);
        assert!((a.speed_kmh - 36.0).abs() < 0.5, "{}", a.speed_kmh);
        assert_eq!(tracks.observed_times(), (0..4).map(ms).collect::<Vec<_>>());
    }

    /// `scan(vol)` started `offset_ms` later.
    fn scan_at(vol: i64, offset_ms: i64) -> Scan {
        let mut s = scan(vol, &[1]);
        s.start_time += offset_ms as f64 / 1000.0;
        s.end_time += offset_ms as f64 / 1000.0;
        s.key_timestamp += offset_ms as f64 / 1000.0;
        s
    }

    #[wasm_bindgen_test]
    fn scene_history_steps_through_every_sites_volume_starts() {
        // KAAA at 0, 5, ... min; KBBB two minutes after each.
        let a: Vec<Scan> = (0..=5).map(|v| scan_at(v, 0)).collect();
        let b: Vec<Scan> = (0..=4).map(|v| scan_at(v, 120_000)).collect();
        let shown = SceneCellKey {
            sites: vec![site_key("KAAA", ms(5)), site_key("KBBB", ms(4) + 120_000)],
            threshold_dbz: 35.0,
        };
        let keys = scene_history_keys(&[&a, &b], &shown);
        // Back from KBBB's 4 (the shown cut) to KAAA's 0, which has no
        // KBBB volume before it and so is KAAA's alone.
        assert_eq!(keys.len(), 10);
        let starts = |k: &SceneCellKey| -> Vec<i64> {
            k.sites.iter().map(|s| s.scan_key.scan_start.0).collect()
        };
        assert_eq!(starts(&keys[0]), vec![ms(0)]);
        assert_eq!(starts(&keys[1]), vec![ms(0), ms(0) + 120_000]);
        assert_eq!(starts(&keys[2]), vec![ms(1), ms(0) + 120_000]);
        assert_eq!(starts(&keys[9]), vec![ms(4), ms(4) + 120_000]);
        assert!(keys.windows(2).all(|w| w[0].time_ms() < w[1].time_ms()));
        assert!(keys.iter().all(|k| k.time_ms() < shown.time_ms()));
        // Never more than the history length.
        let a: Vec<Scan> = (0..=20).map(|v| scan_at(v, 0)).collect();
        let b: Vec<Scan> = (0..=20).map(|v| scan_at(v, 120_000)).collect();
        let shown = SceneCellKey {
            sites: vec![site_key("KAAA", ms(20)), site_key("KBBB", ms(19) + 120_000)],
            threshold_dbz: 35.0,
        };
        assert_eq!(scene_history_keys(&[&a, &b], &shown).len(), HISTORY_VOLUMES);
    }

    #[wasm_bindgen_test]
    fn the_scene_store_coasts_thirteen_minutes_and_the_site_store_by_volumes() {
        let mut scene: StormTracks<SceneCellKey> = StormTracks::default();
        assert_eq!(scene.run_start.params.coast_ms, 13 * 60_000);
        scene.reset();
        assert_eq!(scene.run_start.params.coast_ms, 13 * 60_000);
        let site: StormTracks = StormTracks::default();
        assert_eq!(site.run_start.params.coast_ms, 0);
        assert_eq!(site.run_start.params, TrackParams::default());
    }

    /// A three-site scene: KAAA cuts every 4.5 min and shapes the storm;
    /// KBBB and KCCC cut 1.5 and 3 min after each KAAA cut. Scene `f` is
    /// at `T0 + f * 1.5 min`.
    fn three_site_scene(f: i64) -> SceneCellKey {
        let step = 90_000;
        let newest = |offset: i64| {
            let k = (f - offset).div_euclid(3);
            T0_MS + (3 * k + offset) * step
        };
        let mut sites = vec![site_key("KAAA", newest(0))];
        if f >= 1 {
            sites.push(site_key("KBBB", newest(1)));
        }
        if f >= 2 {
            sites.push(site_key("KCCC", newest(2)));
        }
        SceneCellKey {
            sites,
            threshold_dbz: 35.0,
        }
    }

    /// The scene's cells: the storm (36 km/h east, as KAAA's cut has it,
    /// gone from KAAA's third cut) and a still storm 60 km north.
    fn three_site_result(f: i64) -> StormCellsResult {
        let cut = f / 3;
        let cut_ms = T0_MS + cut * 270_000;
        let mut cells = Vec::new();
        if cut != 2 {
            cells.push(cell(0, 2.7 * cut as f64, 0.0));
        }
        cells.push(cell(cells.len() as u32, 0.0, 60.0));
        for c in &mut cells {
            c.observed_ms = Some(cut_ms);
        }
        StormCellsResult {
            cells,
            ..StormCellsResult::default()
        }
    }

    /// The storm's label in scene `f`, if it has one.
    fn storm_label(tracks: &StormTracks<SceneCellKey>, f: i64) -> Option<String> {
        if f / 3 == 2 {
            return None;
        }
        tracks
            .view(None)
            .and_then(|v| v.for_cell(0).map(|t| t.label.clone()))
    }

    #[wasm_bindgen_test]
    fn a_scene_storm_keeps_its_letter_between_its_radars_cuts() {
        let frames = 13i64;
        let history = |f: i64| -> Vec<SceneCellKey> { (0..f).map(three_site_scene).collect() };
        // In order.
        let mut tracks: StormTracks<SceneCellKey> = StormTracks::default();
        let mut cells: StormCells<SceneCellKey> = StormCells::default();
        let mut in_order = Vec::new();
        for f in 0..frames {
            cells.seed(three_site_scene(f), three_site_result(f), true, true);
            tracks.sync(true, &cells, &history(f));
            in_order.push(storm_label(&tracks, f));
        }
        // Lettered once its radar has cut twice (scene 3), and the same
        // letter after the cut it was missing from.
        assert_eq!(in_order[..3], [None, None, None]);
        for f in [3, 4, 5, 9, 10, 11, 12] {
            assert_eq!(in_order[f].as_deref(), Some("A"), "scene {f}");
        }
        let a = tracks.view(None).unwrap().for_cell(0).cloned().unwrap();
        assert_eq!(a.age_volumes, 4, "one point per KAAA cut");
        assert!((a.speed_kmh - 36.0).abs() < 0.5, "{}", a.speed_kmh);

        // Out of order: the last scene first, then the KAAA scenes as
        // history, then the rest.
        let mut tracks: StormTracks<SceneCellKey> = StormTracks::default();
        let mut cells: StormCells<SceneCellKey> = StormCells::default();
        for f in [0, 3, 6, 9] {
            cells.seed(three_site_scene(f), three_site_result(f), true, false);
        }
        cells.seed(three_site_scene(12), three_site_result(12), true, true);
        tracks.sync(true, &cells, &history(12));
        for f in (1..12).filter(|f| f % 3 != 0) {
            cells.seed(three_site_scene(f), three_site_result(f), true, false);
        }
        tracks.sync(true, &cells, &history(12));
        for f in 0..frames {
            cells.seed(three_site_scene(f), three_site_result(f), true, true);
            tracks.sync(true, &cells, &history(f));
            assert_eq!(storm_label(&tracks, f), in_order[f as usize], "scene {f}");
        }
        assert_eq!(
            tracks.observed_times(),
            (0..frames).map(|f| T0_MS + f * 90_000).collect::<Vec<_>>()
        );
        assert_eq!(tracks.newest_label().as_deref(), Some("B"));
    }
}
