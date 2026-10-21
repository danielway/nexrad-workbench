//! Storm cell tracking across volumes: identity, motion and lineage.
//!
//! SCIT-style centroid tracking, fed one detection result per volume in time
//! order ([`Tracker::observe`]):
//!
//! 1. **Predict.** Each live track predicts where its cell is now from its
//!    last centroid and its motion (a weighted linear fit over its last
//!    [`TrackParams::fit_points`] centroids, recent ones weighted more). A
//!    track seen once borrows the mean motion of the others.
//! 2. **Match.** Every (track, cell) pair whose distance from the
//!    prediction is within the search radius
//!    (`base_radius_km + max_speed_kmh × gap`) is a candidate, with a cost
//!    that adds the distance (as a fraction of the radius), the change in
//!    area (log ratio) and the change in peak. Pairs are taken greedily,
//!    cheapest first; ties go to the older track, then the stronger cell, so
//!    the outcome is deterministic.
//! 3. **Coast and end.** A track with no match coasts (its position is
//!    advected along its motion) for [`TrackParams::coast_volumes`] volumes;
//!    one more miss ends it. A gap between volumes longer than
//!    [`TrackParams::max_gap_ms`] ends every track first: nothing is
//!    matched across a hole in the data.
//! 4. **Start.** A cell with no match starts a track with the next ID.
//!    IDs ([`track_label`]: A..Z, AA, AB, ...) are allocated once per
//!    tracker and never reused, so a label names one storm for the whole
//!    session.
//! 5. **Lineage.** A new track that starts within the search radius of a
//!    track matched in the same volume records `split_from`; a track whose
//!    nearest candidate cell was taken by another track records that track
//!    and, if it then ends, `merged_into`. Groups are compared by the set of
//!    tracks they contain (a lone cell counts as a group of one): one earlier
//!    set spread over two or more sets now is a split, two or more earlier
//!    sets meeting in one set now is a merge ([`LineageEvent`]).
//!
//! **Provisional input.** A live cut still filling gives a provisional
//! result. Observing it with `provisional = true` returns the frame the
//! final result would give for the cells seen so far (matched cells carry
//! their track, with the provisional centroid in the trail and motion) but
//! leaves the tracker untouched: no track starts, coasts, ends or records
//! lineage until the final result for that volume is observed. Cells that
//! would start a track have no ID yet.
//!
//! The tracker only moves forward: a volume at or before the last observed
//! one is refused. Keeping per-volume frames for scrubbing back is the
//! caller's job (the workbench's `core::storm_tracks`).
//!
//! Positions use the same flat projection as the detector (111 km per
//! degree of latitude, longitude scaled by the cosine of the latitude).
//! Motion directions are compass degrees *toward* which the storm moves.

use super::{StormCellsResult, KM_PER_DEG};
use serde::{Deserialize, Serialize};

/// A track's serial number; [`track_label`] gives its short name.
pub(crate) type TrackId = u32;

/// Below this motion confidence a forecast position is not worth drawing.
/// A two-volume track (0.25 at best) stays below it; three volumes on a
/// straight line clear it.
pub(crate) const MIN_FORECAST_CONFIDENCE: f64 = 0.4;

/// Short name for a track: 0 → "A", 25 → "Z", 26 → "AA", 27 → "AB", ...
/// (bijective base 26, like spreadsheet columns).
pub(crate) fn track_label(id: TrackId) -> String {
    let mut n = id as u64 + 1;
    let mut out = Vec::new();
    while n > 0 {
        let rem = ((n - 1) % 26) as u8;
        out.push(b'A' + rem);
        n = (n - 1) / 26;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

/// Tuning knobs for the tracker.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct TrackParams {
    /// Search radius around a prediction for a gap of zero, km.
    pub base_radius_km: f64,
    /// The radius grows by this speed times the time since the track was
    /// last seen.
    pub max_speed_kmh: f64,
    /// Volumes a track survives without a match before it ends.
    pub coast_volumes: u32,
    /// A longer gap between observed volumes ends every track.
    pub max_gap_ms: i64,
    /// Centroids used for the motion fit (the most recent ones).
    pub fit_points: usize,
    /// Centroids kept per track.
    pub history_len: usize,
    /// Weight of `|ln(area ratio)|` in the matching cost.
    pub area_weight: f64,
    /// Weight of the peak change (per 10 dB) in the matching cost.
    pub peak_weight: f64,
    /// A track moving slower than this, with a confident motion, is
    /// stationary (clutter, or a storm that is not going anywhere).
    pub stationary_kmh: f64,
    /// Motion confidence needed before a track can be called stationary.
    pub stationary_confidence: f64,
}

impl Default for TrackParams {
    fn default() -> Self {
        Self {
            base_radius_km: 15.0,
            max_speed_kmh: 60.0,
            coast_volumes: 2,
            max_gap_ms: 20 * 60 * 1000,
            fit_points: 5,
            history_len: 10,
            area_weight: 0.1,
            peak_weight: 1.0,
            stationary_kmh: 10.0,
            stationary_confidence: MIN_FORECAST_CONFIDENCE,
        }
    }
}

/// One observed centroid of a track.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct TrackPoint {
    pub time_ms: i64,
    pub lat: f64,
    pub lon: f64,
    pub peak_dbz: f32,
    pub area_km2: f32,
}

impl TrackPoint {
    fn pos(&self) -> (f64, f64) {
        (self.lat, self.lon)
    }
}

/// A motion vector with a confidence.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Motion {
    /// Eastward speed, km/h.
    pub east_kmh: f64,
    /// Northward speed, km/h.
    pub north_kmh: f64,
    /// 0..1: grows with the number of centroids in the fit (a quarter per
    /// step, full at five) and shrinks with the fit's residual.
    pub confidence: f64,
}

impl Motion {
    pub(crate) fn speed_kmh(&self) -> f64 {
        self.east_kmh.hypot(self.north_kmh)
    }

    /// Compass direction the storm moves toward, degrees in [0, 360).
    pub(crate) fn direction_deg(&self) -> f64 {
        self.east_kmh
            .atan2(self.north_kmh)
            .to_degrees()
            .rem_euclid(360.0)
    }
}

/// How a track ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum EndReason {
    /// Unmatched for longer than the coasting allowance.
    Lost,
    /// Unmatched while its cell was taken by this track.
    MergedInto(TrackId),
    /// The data had a gap longer than [`TrackParams::max_gap_ms`].
    Gap,
}

/// A live track inside the tracker.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Track {
    pub id: TrackId,
    /// Observed centroids, oldest first, at most
    /// [`TrackParams::history_len`].
    pub points: Vec<TrackPoint>,
    pub start_ms: i64,
    /// Volumes in which the track was matched (its age in volumes).
    pub volumes: u32,
    /// Consecutive volumes without a match; coasting while above zero.
    pub missed: u32,
    pub motion: Motion,
    /// The track this one split off from, if it started next to one.
    pub split_from: Option<TrackId>,
    /// While coasting: the track that took the cell this one would have
    /// matched.
    pub merging_into: Option<TrackId>,
}

impl Track {
    fn last(&self) -> &TrackPoint {
        // A track is created with one point and points are only trimmed
        // from the front, so it is never empty.
        &self.points[self.points.len() - 1]
    }

    /// Position predicted at `time_ms`: the last centroid advected along
    /// the motion. A one-volume track has no motion of its own and uses
    /// `field` (the mean motion of the other tracks) when there is one.
    fn predict(&self, time_ms: i64, field: Option<&Motion>) -> (f64, f64) {
        let last = self.last();
        let minutes = (time_ms - last.time_ms) as f64 / 60_000.0;
        let motion = match field {
            Some(field) if self.points.len() < 2 => field,
            _ => &self.motion,
        };
        advect(last.pos(), motion, minutes)
    }

    fn radius_km(&self, time_ms: i64, params: &TrackParams) -> f64 {
        let hours = (time_ms - self.last().time_ms).max(0) as f64 / 3_600_000.0;
        params.base_radius_km + params.max_speed_kmh * hours
    }

    fn refit(&mut self, params: &TrackParams) {
        let n = self.points.len().min(params.fit_points.max(2));
        self.motion = fit_motion(&self.points[self.points.len() - n..]);
    }

    fn snapshot(&self, time_ms: i64, cell_id: Option<u32>, params: &TrackParams) -> TrackSnapshot {
        let last = self.last();
        let coasting = cell_id.is_none();
        let (lat, lon) = if coasting {
            self.predict(time_ms, None)
        } else {
            last.pos()
        };
        let trail = self.points[self.points.len().saturating_sub(3)..]
            .iter()
            .map(|p| p.pos())
            .collect();
        let n = self.points.len();
        let peak_trend_db =
            (n >= 2).then(|| last.peak_dbz - self.points[n.saturating_sub(3)].peak_dbz);
        TrackSnapshot {
            id: self.id,
            cell_id,
            lat,
            lon,
            trail,
            motion: self.motion,
            start_ms: self.start_ms,
            last_seen_ms: last.time_ms,
            age_volumes: self.volumes,
            age_minutes: (last.time_ms - self.start_ms) as f64 / 60_000.0,
            peak_dbz: last.peak_dbz,
            peak_trend_db,
            stationary: self.motion.confidence >= params.stationary_confidence
                && self.motion.speed_kmh() < params.stationary_kmh,
            coasting,
            split_from: self.split_from,
        }
    }
}

/// A track as of one volume: what a view needs to draw and describe it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct TrackSnapshot {
    pub id: TrackId,
    /// The cell (index into that volume's result) the track is on; `None`
    /// while coasting.
    pub cell_id: Option<u32>,
    /// The cell's centroid, or the advected position while coasting.
    pub lat: f64,
    pub lon: f64,
    /// The last (up to) three observed centroids, oldest first; the last one
    /// is this volume's when the track is matched.
    pub trail: Vec<(f64, f64)>,
    pub motion: Motion,
    pub start_ms: i64,
    pub last_seen_ms: i64,
    /// Volumes the track was matched in, this one included.
    pub age_volumes: u32,
    /// Minutes from the first to the last observed centroid.
    pub age_minutes: f64,
    /// Peak at the last observed centroid.
    pub peak_dbz: f32,
    /// Peak change over the last three observed volumes (fewer for a young
    /// track), dB; `None` for a one-volume track.
    pub peak_trend_db: Option<f32>,
    pub stationary: bool,
    pub coasting: bool,
    pub split_from: Option<TrackId>,
}

impl TrackSnapshot {
    /// Where the track is forecast `minutes` from now: its position advected
    /// along its motion. Whether that is worth showing is the motion's
    /// confidence (see [`MIN_FORECAST_CONFIDENCE`]).
    pub(crate) fn forecast(&self, minutes: f64) -> (f64, f64) {
        advect((self.lat, self.lon), &self.motion, minutes)
    }
}

/// A track that ended at a volume.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct EndedTrack {
    pub id: TrackId,
    /// Time of its last observed centroid.
    pub end_ms: i64,
    pub reason: EndReason,
}

/// Split or merge of groups.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum LineageKind {
    Split,
    Merge,
}

/// A change in which tracks share a group, between two volumes. Each set
/// lists the tracks of one group (or a lone cell), ascending.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LineageEvent {
    pub time_ms: i64,
    pub kind: LineageKind,
    /// The sets in the previous volume.
    pub before: Vec<Vec<TrackId>>,
    /// The sets in this volume.
    pub after: Vec<Vec<TrackId>>,
}

/// What one observed volume gave: the track of every cell and the state of
/// every live track as of that volume.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Frame {
    pub time_ms: i64,
    /// Computed from a provisional result: nothing was committed.
    pub provisional: bool,
    /// The track of each cell, indexed like the result's cells. `None` only
    /// in a provisional frame, for a cell that would start a track.
    pub cell_tracks: Vec<Option<TrackId>>,
    /// Every track alive at this volume (matched and coasting), by id.
    pub tracks: Vec<TrackSnapshot>,
    /// Tracks that ended at this volume.
    pub ended: Vec<EndedTrack>,
    /// Group splits and merges seen at this volume.
    pub lineage: Vec<LineageEvent>,
}

impl Frame {
    /// The track on cell `cell_id`, if any.
    #[cfg_attr(not(test), allow(dead_code))] // Part of the leaf API; tests read it.
    pub(crate) fn track_of_cell(&self, cell_id: u32) -> Option<&TrackSnapshot> {
        let id = (*self.cell_tracks.get(cell_id as usize)?)?;
        self.track(id)
    }

    #[cfg_attr(not(test), allow(dead_code))] // Part of the leaf API; tests read it.
    pub(crate) fn track(&self, id: TrackId) -> Option<&TrackSnapshot> {
        self.tracks.iter().find(|t| t.id == id)
    }
}

/// The tracker's state: live tracks, the ID counter, and the previous
/// volume's group sets. Serializable so a session can be saved or moved.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Tracker {
    pub params: TrackParams,
    tracks: Vec<Track>,
    next_id: TrackId,
    last_time_ms: Option<i64>,
    /// The previous volume's group sets (see [`LineageEvent`]).
    units: Vec<Vec<TrackId>>,
}

impl Tracker {
    /// Time of the last committed (final) volume.
    pub(crate) fn last_time_ms(&self) -> Option<i64> {
        self.last_time_ms
    }

    /// The live tracks (matched or coasting at the last volume).
    #[cfg_attr(not(test), allow(dead_code))] // Part of the leaf API; tests read it.
    pub(crate) fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    /// The id the next new track gets.
    #[cfg_attr(not(test), allow(dead_code))] // Part of the leaf API; tests read it.
    pub(crate) fn next_id(&self) -> TrackId {
        self.next_id
    }

    /// Drop every track and start over from no history, keeping the ID
    /// counter so no label is reused.
    pub(crate) fn restart(&mut self) {
        let (params, next_id) = (self.params.clone(), self.next_id);
        *self = Self {
            params,
            next_id,
            ..Self::default()
        };
    }

    /// Feed the result of the volume at `time_ms`. Returns `None` (and
    /// changes nothing) when the volume is not after the last committed one.
    /// A provisional result changes nothing either; see the module docs.
    pub(crate) fn observe(
        &mut self,
        time_ms: i64,
        result: &StormCellsResult,
        provisional: bool,
    ) -> Option<Frame> {
        if self.last_time_ms.is_some_and(|last| time_ms <= last) {
            return None;
        }
        if provisional {
            let mut scratch = self.clone();
            return Some(scratch.step(time_ms, result, true));
        }
        Some(self.step(time_ms, result, false))
    }

    fn step(&mut self, time_ms: i64, result: &StormCellsResult, provisional: bool) -> Frame {
        let params = self.params.clone();
        let mut frame = Frame {
            time_ms,
            provisional,
            ..Frame::default()
        };

        // A hole in the data: nothing matches across it.
        if self
            .last_time_ms
            .is_some_and(|last| time_ms - last > params.max_gap_ms)
        {
            for t in self.tracks.drain(..) {
                frame.ended.push(EndedTrack {
                    id: t.id,
                    end_ms: t.last().time_ms,
                    reason: EndReason::Gap,
                });
            }
            self.units.clear();
        }

        // Candidate pairs within each track's search radius.
        let field = motion_field(&self.tracks);
        let predictions: Vec<((f64, f64), f64)> = self
            .tracks
            .iter()
            .map(|t| {
                (
                    t.predict(time_ms, field.as_ref()),
                    t.radius_km(time_ms, &params),
                )
            })
            .collect();
        let mut candidates: Vec<(f64, usize, usize, f64)> = Vec::new();
        for (ti, (track, &(pred, radius))) in self.tracks.iter().zip(&predictions).enumerate() {
            let last = track.last();
            for (ci, cell) in result.cells.iter().enumerate() {
                let d = distance_km(pred, (cell.lat, cell.lon));
                if d > radius {
                    continue;
                }
                let area_ratio = (cell.area_km2.max(0.1) / last.area_km2.max(0.1)) as f64;
                let cost = d / radius
                    + params.area_weight * area_ratio.ln().abs()
                    + params.peak_weight * ((cell.peak_dbz - last.peak_dbz).abs() as f64 / 10.0);
                candidates.push((cost, ti, ci, d));
            }
        }
        candidates.sort_by(|a, b| {
            a.0.total_cmp(&b.0)
                .then(self.tracks[a.1].id.cmp(&self.tracks[b.1].id))
                .then(a.2.cmp(&b.2))
        });
        let mut track_cell: Vec<Option<usize>> = vec![None; self.tracks.len()];
        let mut cell_track: Vec<Option<usize>> = vec![None; result.cells.len()];
        for &(_, ti, ci, _) in &candidates {
            if track_cell[ti].is_none() && cell_track[ci].is_none() {
                track_cell[ti] = Some(ci);
                cell_track[ci] = Some(ti);
            }
        }

        // Unmatched tracks: whose cell did they lose to (nearest taken
        // candidate)?
        let mut lost_to: Vec<Option<TrackId>> = vec![None; self.tracks.len()];
        for (ti, lost) in lost_to.iter_mut().enumerate() {
            if track_cell[ti].is_some() {
                continue;
            }
            *lost = candidates
                .iter()
                .filter(|c| c.1 == ti)
                .filter_map(|c| cell_track[c.2].map(|other| (c.3, self.tracks[other].id)))
                .min_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)))
                .map(|(_, id)| id);
        }

        // Update matched tracks; coast or end the rest.
        let old_ids: Vec<TrackId> = self.tracks.iter().map(|t| t.id).collect();
        let mut kept: Vec<Track> = Vec::with_capacity(self.tracks.len());
        let mut kept_cell: Vec<Option<u32>> = Vec::with_capacity(self.tracks.len());
        for (ti, mut track) in std::mem::take(&mut self.tracks).into_iter().enumerate() {
            match track_cell[ti] {
                Some(ci) => {
                    let cell = &result.cells[ci];
                    track.points.push(TrackPoint {
                        time_ms,
                        lat: cell.lat,
                        lon: cell.lon,
                        peak_dbz: cell.peak_dbz,
                        area_km2: cell.area_km2,
                    });
                    if track.points.len() > params.history_len.max(3) {
                        track.points.remove(0);
                    }
                    track.volumes += 1;
                    track.missed = 0;
                    track.merging_into = None;
                    track.refit(&params);
                    kept.push(track);
                    kept_cell.push(Some(ci as u32));
                }
                None => {
                    track.missed += 1;
                    if lost_to[ti].is_some() {
                        track.merging_into = lost_to[ti];
                    }
                    if track.missed > params.coast_volumes {
                        frame.ended.push(EndedTrack {
                            id: track.id,
                            end_ms: track.last().time_ms,
                            reason: track
                                .merging_into
                                .map_or(EndReason::Lost, EndReason::MergedInto),
                        });
                    } else {
                        kept.push(track);
                        kept_cell.push(None);
                    }
                }
            }
        }

        // Unmatched cells start tracks (final results only).
        let mut cell_tracks: Vec<Option<TrackId>> = vec![None; result.cells.len()];
        for (track, cell) in kept.iter().zip(&kept_cell) {
            if let Some(ci) = cell {
                cell_tracks[*ci as usize] = Some(track.id);
            }
        }
        if !provisional {
            for (ci, cell) in result.cells.iter().enumerate() {
                if cell_tracks[ci].is_some() {
                    continue;
                }
                let pos = (cell.lat, cell.lon);
                // Next to a track matched in this volume: a split.
                let split_from = predictions
                    .iter()
                    .enumerate()
                    .filter(|(ti, _)| track_cell[*ti].is_some())
                    .map(|(ti, &(pred, radius))| (distance_km(pred, pos), radius, ti))
                    .filter(|&(d, radius, _)| d <= radius)
                    .min_by(|a, b| a.0.total_cmp(&b.0).then(a.2.cmp(&b.2)))
                    .map(|(_, _, ti)| old_ids[ti]);
                let id = self.next_id;
                self.next_id += 1;
                kept.push(Track {
                    id,
                    points: vec![TrackPoint {
                        time_ms,
                        lat: cell.lat,
                        lon: cell.lon,
                        peak_dbz: cell.peak_dbz,
                        area_km2: cell.area_km2,
                    }],
                    start_ms: time_ms,
                    volumes: 1,
                    missed: 0,
                    motion: Motion::default(),
                    split_from,
                    merging_into: None,
                });
                kept_cell.push(Some(ci as u32));
                cell_tracks[ci] = Some(id);
            }
        }

        // Group lineage, from the sets of tracks sharing a group.
        let units = units_of(result, &cell_tracks);
        if !provisional {
            frame.lineage = lineage(time_ms, &self.units, &units);
        }

        let mut order: Vec<usize> = (0..kept.len()).collect();
        order.sort_by_key(|&i| kept[i].id);
        frame.tracks = order
            .iter()
            .map(|&i| kept[i].snapshot(time_ms, kept_cell[i], &params))
            .collect();
        frame.cell_tracks = cell_tracks;
        frame.ended.sort_by_key(|e| e.id);

        self.tracks = kept;
        self.units = units;
        self.last_time_ms = Some(time_ms);
        frame
    }
}

/// The confidence-weighted mean motion of the tracks that have one: the
/// first guess for a track seen only once (SCIT's default motion).
fn motion_field(tracks: &[Track]) -> Option<Motion> {
    let (mut e, mut n, mut w) = (0.0, 0.0, 0.0);
    for t in tracks.iter().filter(|t| t.points.len() >= 2) {
        let c = t.motion.confidence;
        e += c * t.motion.east_kmh;
        n += c * t.motion.north_kmh;
        w += c;
    }
    (w > 0.0).then(|| Motion {
        east_kmh: e / w,
        north_kmh: n / w,
        confidence: 0.0,
    })
}

/// The sets of tracks sharing a group (or a lone cell), ascending.
fn units_of(result: &StormCellsResult, cell_tracks: &[Option<TrackId>]) -> Vec<Vec<TrackId>> {
    let mut units: Vec<Vec<TrackId>> = Vec::new();
    for group in &result.groups {
        let mut ids: Vec<TrackId> = group
            .cell_ids
            .iter()
            .filter_map(|&c| cell_tracks.get(c as usize).copied().flatten())
            .collect();
        ids.sort_unstable();
        if !ids.is_empty() {
            units.push(ids);
        }
    }
    for (ci, cell) in result.cells.iter().enumerate() {
        if cell.group_id.is_none() {
            if let Some(id) = cell_tracks[ci] {
                units.push(vec![id]);
            }
        }
    }
    units.sort();
    units
}

/// Splits and merges between the previous volume's group sets and this
/// one's. Only tracks present in both volumes count.
fn lineage(time_ms: i64, before: &[Vec<TrackId>], after: &[Vec<TrackId>]) -> Vec<LineageEvent> {
    let shares = |a: &[TrackId], b: &[TrackId]| a.iter().any(|t| b.contains(t));
    let mut events = Vec::new();
    for prev in before {
        let parts: Vec<Vec<TrackId>> = after.iter().filter(|u| shares(prev, u)).cloned().collect();
        if parts.len() >= 2 {
            events.push(LineageEvent {
                time_ms,
                kind: LineageKind::Split,
                before: vec![prev.clone()],
                after: parts,
            });
        }
    }
    for now in after {
        let parts: Vec<Vec<TrackId>> = before.iter().filter(|u| shares(now, u)).cloned().collect();
        if parts.len() >= 2 {
            events.push(LineageEvent {
                time_ms,
                kind: LineageKind::Merge,
                before: parts,
                after: vec![now.clone()],
            });
        }
    }
    events
}

/// Weighted least-squares motion over `points` (oldest first): weights rise
/// linearly toward the most recent centroid. See [`Motion::confidence`].
fn fit_motion(points: &[TrackPoint]) -> Motion {
    let n = points.len();
    if n < 2 {
        return Motion::default();
    }
    let origin = points[n - 1];
    let samples: Vec<(f64, f64, f64, f64)> = points
        .iter()
        .enumerate()
        .map(|(k, p)| {
            let (x, y) = offset_km(origin.pos(), p.pos());
            let t = (p.time_ms - origin.time_ms) as f64 / 3_600_000.0;
            ((k + 1) as f64, t, x, y)
        })
        .collect();
    let sw: f64 = samples.iter().map(|s| s.0).sum();
    let mean =
        |f: fn(&(f64, f64, f64, f64)) -> f64| samples.iter().map(|s| s.0 * f(s)).sum::<f64>() / sw;
    let (tm, xm, ym) = (mean(|s| s.1), mean(|s| s.2), mean(|s| s.3));
    let stt: f64 = samples.iter().map(|s| s.0 * (s.1 - tm).powi(2)).sum();
    if stt <= 0.0 {
        return Motion::default();
    }
    let vx = samples
        .iter()
        .map(|s| s.0 * (s.1 - tm) * (s.2 - xm))
        .sum::<f64>()
        / stt;
    let vy = samples
        .iter()
        .map(|s| s.0 * (s.1 - tm) * (s.3 - ym))
        .sum::<f64>()
        / stt;
    let rss: f64 = samples
        .iter()
        .map(|s| {
            let rx = s.2 - (xm + vx * (s.1 - tm));
            let ry = s.3 - (ym + vy * (s.1 - tm));
            s.0 * (rx * rx + ry * ry)
        })
        .sum();
    let rms_km = (rss / sw).sqrt();
    let length = ((n - 1) as f64 / 4.0).min(1.0);
    let fit = 1.0 / (1.0 + (rms_km / FIT_SCALE_KM).powi(2));
    Motion {
        east_kmh: vx,
        north_kmh: vy,
        confidence: length * fit,
    }
}

/// Residual (rms, km) at which the fit factor of the confidence halves.
const FIT_SCALE_KM: f64 = 3.0;

// --- Geometry -------------------------------------------------------------

/// East and north offset (km) of `to` from `from`, both (lat, lon).
pub(crate) fn offset_km(from: (f64, f64), to: (f64, f64)) -> (f64, f64) {
    let lat_mid = ((from.0 + to.0) * 0.5).to_radians();
    let dlon = (to.1 - from.1 + 180.0).rem_euclid(360.0) - 180.0;
    (
        dlon * KM_PER_DEG * lat_mid.cos(),
        (to.0 - from.0) * KM_PER_DEG,
    )
}

/// Flat distance in km between two (lat, lon) points.
pub(crate) fn distance_km(a: (f64, f64), b: (f64, f64)) -> f64 {
    let (x, y) = offset_km(a, b);
    x.hypot(y)
}

/// Compass bearing (degrees, [0, 360)) and distance (km) from `from` to
/// `to`.
pub(crate) fn bearing_distance(from: (f64, f64), to: (f64, f64)) -> (f64, f64) {
    let (x, y) = offset_km(from, to);
    (x.atan2(y).to_degrees().rem_euclid(360.0), x.hypot(y))
}

/// `pos` moved along `motion` for `minutes`.
pub(crate) fn advect(pos: (f64, f64), motion: &Motion, minutes: f64) -> (f64, f64) {
    let hours = minutes / 60.0;
    displace(pos, motion.east_kmh * hours, motion.north_kmh * hours)
}

/// `pos` moved `east_km` east and `north_km` north.
pub(crate) fn displace(pos: (f64, f64), east_km: f64, north_km: f64) -> (f64, f64) {
    let lat = pos.0 + north_km / KM_PER_DEG;
    let cos = ((pos.0 + lat) * 0.5).to_radians().cos().max(1e-6);
    (lat, pos.1 + east_km / (KM_PER_DEG * cos))
}

/// When and how close a point moving from `pos` along `motion` passes a
/// fixed `point`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Approach {
    /// Minutes from now to the closest approach; 0 when it is moving away
    /// (or not moving), so the closest point is now.
    pub minutes: f64,
    /// Distance at closest approach, km.
    pub distance_km: f64,
}

/// Closest approach of a point moving at constant velocity to a fixed
/// point, in the flat projection around the fixed point.
pub(crate) fn closest_approach(pos: (f64, f64), motion: &Motion, point: (f64, f64)) -> Approach {
    let (rx, ry) = offset_km(point, pos);
    let (vx, vy) = (motion.east_kmh, motion.north_kmh);
    let v2 = vx * vx + vy * vy;
    let hours = if v2 > 1e-9 {
        (-(rx * vx + ry * vy) / v2).max(0.0)
    } else {
        0.0
    };
    Approach {
        minutes: hours * 60.0,
        distance_km: (rx + vx * hours).hypot(ry + vy * hours),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storm_cells::{StormCell, StormGroup};
    use wasm_bindgen_test::wasm_bindgen_test;

    const T0: i64 = 1_716_321_600_000;
    const VOL_MS: i64 = 5 * 60 * 1000;
    const ORIGIN: (f64, f64) = (41.5, -94.5);

    /// A synthetic cell `east_km`/`north_km` from [`ORIGIN`].
    struct C {
        east: f64,
        north: f64,
        peak: f32,
        area: f32,
        group: Option<u32>,
    }

    fn c(east: f64, north: f64) -> C {
        C {
            east,
            north,
            peak: 55.0,
            area: 100.0,
            group: None,
        }
    }

    impl C {
        fn peak(self, peak: f32) -> Self {
            Self { peak, ..self }
        }
        fn group(self, g: u32) -> Self {
            Self {
                group: Some(g),
                ..self
            }
        }
    }

    /// A result with cells in the order given (ids are the indices).
    fn result(cells: Vec<C>) -> StormCellsResult {
        let mut r = StormCellsResult::default();
        for (i, cell) in cells.iter().enumerate() {
            let (lat, lon) = displace(ORIGIN, cell.east, cell.north);
            r.cells.push(StormCell {
                id: i as u32,
                group_id: cell.group,
                level_dbz: 35.0,
                peak_dbz: cell.peak,
                mean_dbz: 45.0,
                area_km2: cell.area,
                gate_count: 100,
                lat,
                lon,
                bounds: (lat, lon, lat, lon),
                bearing_from_radar_deg: 0.0,
                range_from_radar_km: 50.0,
                orientation_deg: 0.0,
                elongation: 1.0,
                outline: Vec::new(),
            });
            if let Some(g) = cell.group {
                while r.groups.len() <= g as usize {
                    r.groups.push(StormGroup {
                        id: r.groups.len() as u32,
                        level_dbz: 35.0,
                        peak_dbz: 0.0,
                        area_km2: 0.0,
                        cell_ids: Vec::new(),
                        lat,
                        lon,
                        outline: Vec::new(),
                    });
                }
                r.groups[g as usize].cell_ids.push(i as u32);
            }
        }
        r
    }

    fn t(vol: i64) -> i64 {
        T0 + vol * VOL_MS
    }

    /// km per 5-minute volume at `kmh`.
    fn step_km(kmh: f64) -> f64 {
        kmh / 12.0
    }

    fn ids(frame: &Frame) -> Vec<Option<TrackId>> {
        frame.cell_tracks.clone()
    }

    #[wasm_bindgen_test]
    fn labels_run_a_to_z_then_two_letters() {
        let l = |n| track_label(n);
        assert_eq!(l(0), "A");
        assert_eq!(l(1), "B");
        assert_eq!(l(25), "Z");
        assert_eq!(l(26), "AA");
        assert_eq!(l(27), "AB");
        assert_eq!(l(51), "AZ");
        assert_eq!(l(52), "BA");
        assert_eq!(l(701), "ZZ");
        assert_eq!(l(702), "AAA");
    }

    #[wasm_bindgen_test]
    fn straight_line_motion_keeps_one_id_and_fits_its_velocity() {
        let mut tr = Tracker::default();
        let mut last = None;
        for v in 0..6 {
            let x = v as f64 * step_km(40.0);
            let f = tr.observe(t(v), &result(vec![c(x, 0.0)]), false).unwrap();
            assert_eq!(ids(&f), vec![Some(0)], "volume {v}");
            let s = f.track(0).unwrap();
            assert_eq!(s.age_volumes, v as u32 + 1);
            assert_eq!(s.trail.len(), (v as usize + 1).min(3));
            match v {
                0 => assert_eq!(s.motion.confidence, 0.0),
                1 => assert!(s.motion.confidence < MIN_FORECAST_CONFIDENCE),
                _ => assert!(s.motion.confidence >= MIN_FORECAST_CONFIDENCE),
            }
            last = Some(s.clone());
        }
        let s = last.unwrap();
        assert!((s.motion.speed_kmh() - 40.0).abs() < 0.5, "{:?}", s.motion);
        assert!((s.motion.direction_deg() - 90.0).abs() < 1.0);
        assert!(s.motion.confidence > 0.99);
        assert!(!s.stationary && !s.coasting);
        assert_eq!(s.age_minutes, 25.0);
        // +30 min at 40 km/h: 20 km further east.
        let (dx, dy) = offset_km((s.lat, s.lon), s.forecast(30.0));
        assert!((dx - 20.0).abs() < 0.2 && dy.abs() < 0.2, "{dx} {dy}");
        assert_eq!(tr.next_id(), 1);
    }

    #[wasm_bindgen_test]
    fn ids_follow_storms_when_the_strength_order_swaps() {
        let mut tr = Tracker::default();
        // Two storms 30 km apart moving NE; the weaker becomes the stronger.
        let f0 = tr
            .observe(
                t(0),
                &result(vec![c(0.0, 0.0).peak(60.0), c(30.0, 0.0).peak(50.0)]),
                false,
            )
            .unwrap();
        assert_eq!(ids(&f0), vec![Some(0), Some(1)]);
        let d = step_km(50.0) / 2f64.sqrt();
        let f1 = tr
            .observe(
                t(1),
                &result(vec![c(30.0 + d, d).peak(56.0), c(d, d).peak(54.0)]),
                false,
            )
            .unwrap();
        assert_eq!(ids(&f1), vec![Some(1), Some(0)]);
    }

    #[wasm_bindgen_test]
    fn a_young_track_is_predicted_with_the_others_motion() {
        let mut tr = Tracker::default();
        // A storm far away moves 10 km north per volume.
        tr.observe(t(0), &result(vec![c(100.0, 0.0)]), false);
        tr.observe(t(1), &result(vec![c(100.0, 10.0)]), false);
        // A new storm appears; next volume two candidates: one 9 km north
        // (moved with the flow) and one 8 km south (stood still and drifted).
        tr.observe(t(2), &result(vec![c(100.0, 20.0), c(0.0, 0.0)]), false);
        let f = tr
            .observe(
                t(3),
                &result(vec![c(100.0, 30.0), c(0.0, 9.0), c(0.0, -8.0)]),
                false,
            )
            .unwrap();
        assert_eq!(ids(&f), vec![Some(0), Some(1), Some(2)]);
    }

    #[wasm_bindgen_test]
    fn a_new_cell_next_to_a_track_records_the_split() {
        let mut tr = Tracker::default();
        tr.observe(t(0), &result(vec![c(0.0, 0.0)]), false);
        // The storm splits: two cores 8 km apart in one group.
        let f = tr
            .observe(
                t(1),
                &result(vec![c(1.0, 3.0).group(0), c(1.0, -5.0).peak(50.0).group(0)]),
                false,
            )
            .unwrap();
        assert_eq!(ids(&f), vec![Some(0), Some(1)]);
        assert_eq!(f.track(1).unwrap().split_from, Some(0));
        assert_eq!(f.track(0).unwrap().split_from, None);
        // A new set with a new track is not a group split.
        assert!(f.lineage.is_empty());
        // Next volume they separate into lone cells: a group split.
        let f = tr
            .observe(
                t(2),
                &result(vec![c(2.0, 6.0), c(2.0, -8.0).peak(50.0)]),
                false,
            )
            .unwrap();
        assert_eq!(ids(&f), vec![Some(0), Some(1)]);
        assert_eq!(
            f.lineage,
            vec![LineageEvent {
                time_ms: t(2),
                kind: LineageKind::Split,
                before: vec![vec![0, 1]],
                after: vec![vec![0], vec![1]],
            }]
        );
    }

    #[wasm_bindgen_test]
    fn merging_storms_end_one_track_into_the_other() {
        let mut tr = Tracker::default();
        tr.observe(
            t(0),
            &result(vec![c(0.0, 0.0).peak(60.0), c(12.0, 0.0)]),
            false,
        );
        // Lone cells join one group.
        let f = tr
            .observe(
                t(1),
                &result(vec![c(1.0, 0.0).peak(60.0).group(0), c(10.0, 0.0).group(0)]),
                false,
            )
            .unwrap();
        assert_eq!(ids(&f), vec![Some(0), Some(1)]);
        assert_eq!(f.lineage.len(), 1);
        assert_eq!(f.lineage[0].kind, LineageKind::Merge);
        assert_eq!(f.lineage[0].before, vec![vec![0], vec![1]]);
        assert_eq!(f.lineage[0].after, vec![vec![0, 1]]);
        // Then one cell between them: A keeps it (cheaper), B coasts.
        let f = tr
            .observe(t(2), &result(vec![c(4.0, 0.0).peak(61.0)]), false)
            .unwrap();
        assert_eq!(ids(&f), vec![Some(0)]);
        let b = f.track(1).unwrap();
        assert!(b.coasting && b.cell_id.is_none());
        assert_eq!(tr.tracks()[1].merging_into, Some(0));
        tr.observe(t(3), &result(vec![c(5.0, 0.0).peak(61.0)]), false);
        let f = tr
            .observe(t(4), &result(vec![c(6.0, 0.0).peak(61.0)]), false)
            .unwrap();
        assert_eq!(
            f.ended,
            vec![EndedTrack {
                id: 1,
                end_ms: t(1),
                reason: EndReason::MergedInto(0),
            }]
        );
        assert!(f.track(1).is_none());
    }

    #[wasm_bindgen_test]
    fn a_vanished_cell_coasts_two_volumes_then_ends_and_a_neighbour_is_new() {
        let mut tr = Tracker::default();
        let s = step_km(36.0); // 3 km per volume, east
        for v in 0..3 {
            tr.observe(t(v), &result(vec![c(v as f64 * s, 0.0)]), false);
        }
        // A disappears; a new cell pops up 40 km north (outside the radius).
        let f = tr
            .observe(t(3), &result(vec![c(0.0, 40.0)]), false)
            .unwrap();
        assert_eq!(ids(&f), vec![Some(1)]);
        let a = f.track(0).unwrap();
        assert!(a.coasting);
        // Advected along its motion: one more step east.
        let (dx, dy) = offset_km(ORIGIN, (a.lat, a.lon));
        assert!((dx - 3.0 * s).abs() < 0.2 && dy.abs() < 0.2, "{dx} {dy}");
        assert_eq!(f.track(1).unwrap().split_from, None);
        let f = tr
            .observe(t(4), &result(vec![c(0.0, 40.0)]), false)
            .unwrap();
        assert!(f.track(0).unwrap().coasting && f.ended.is_empty());
        let f = tr
            .observe(t(5), &result(vec![c(0.0, 40.0)]), false)
            .unwrap();
        assert_eq!(
            f.ended,
            vec![EndedTrack {
                id: 0,
                end_ms: t(2),
                reason: EndReason::Lost
            }]
        );
        assert_eq!(ids(&f), vec![Some(1)]);
    }

    #[wasm_bindgen_test]
    fn a_coasting_track_picks_its_storm_up_again() {
        let mut tr = Tracker::default();
        let s = step_km(48.0);
        for v in 0..3 {
            tr.observe(t(v), &result(vec![c(v as f64 * s, 0.0)]), false);
        }
        tr.observe(t(3), &StormCellsResult::default(), false);
        let f = tr
            .observe(t(4), &result(vec![c(4.0 * s + 2.0, 1.0)]), false)
            .unwrap();
        assert_eq!(ids(&f), vec![Some(0)]);
        let a = f.track(0).unwrap();
        assert!(!a.coasting);
        assert_eq!(a.age_volumes, 4);
    }

    #[wasm_bindgen_test]
    fn provisional_results_preview_without_committing() {
        let mut tr = Tracker::default();
        let s = step_km(60.0);
        tr.observe(t(0), &result(vec![c(0.0, 0.0)]), false);
        tr.observe(t(1), &result(vec![c(s, 0.0)]), false);
        let before = tr.clone();
        // Part of the next cut: A moved on, and a new cell far away.
        let partial = result(vec![c(2.0 * s, 0.0), c(-60.0, 0.0)]);
        let p = tr.observe(t(2), &partial, true).unwrap();
        assert!(p.provisional);
        assert_eq!(ids(&p), vec![Some(0), None]);
        assert_eq!(p.track(0).unwrap().trail.len(), 3);
        assert!(p.lineage.is_empty() && p.ended.is_empty());
        assert_eq!(tr, before, "nothing committed");
        // A second provisional for the same volume is still allowed.
        assert!(tr.observe(t(2), &partial, true).is_some());
        // The final result commits.
        let f = tr.observe(t(2), &partial, false).unwrap();
        assert!(!f.provisional);
        assert_eq!(ids(&f), vec![Some(0), Some(1)]);
        assert_eq!(f.track(0), p.track(0));
        assert_eq!(tr.last_time_ms(), Some(t(2)));
    }

    #[wasm_bindgen_test]
    fn provisional_misses_do_not_end_tracks() {
        let mut tr = Tracker::default();
        for v in 0..3 {
            tr.observe(t(v), &result(vec![c(v as f64, 0.0)]), false);
        }
        tr.observe(t(3), &StormCellsResult::default(), false);
        tr.observe(t(4), &StormCellsResult::default(), false);
        // A third miss would end A, but only a final result can.
        let p = tr
            .observe(t(5), &StormCellsResult::default(), true)
            .unwrap();
        assert_eq!(p.ended.len(), 1);
        assert_eq!(tr.tracks().len(), 1);
    }

    #[wasm_bindgen_test]
    fn volumes_at_or_before_the_last_are_refused() {
        let mut tr = Tracker::default();
        tr.observe(t(1), &result(vec![c(0.0, 0.0)]), false);
        let before = tr.clone();
        assert!(tr
            .observe(t(1), &result(vec![c(0.0, 0.0)]), false)
            .is_none());
        assert!(tr
            .observe(t(0), &result(vec![c(0.0, 0.0)]), false)
            .is_none());
        assert!(tr.observe(t(0), &result(vec![c(0.0, 0.0)]), true).is_none());
        assert_eq!(tr, before);
    }

    #[wasm_bindgen_test]
    fn a_long_gap_ends_every_track_and_new_ids_follow() {
        let mut tr = Tracker::default();
        tr.observe(t(0), &result(vec![c(0.0, 0.0), c(50.0, 0.0)]), false);
        let f = tr
            .observe(t(5), &result(vec![c(0.0, 0.0), c(50.0, 0.0)]), false)
            .unwrap();
        assert_eq!(ids(&f), vec![Some(2), Some(3)]);
        assert_eq!(f.ended.len(), 2);
        assert!(f.ended.iter().all(|e| e.reason == EndReason::Gap));
    }

    #[wasm_bindgen_test]
    fn a_still_echo_is_stationary_and_its_trend_tracks_the_peak() {
        let mut tr = Tracker::default();
        let mut f = Frame::default();
        for (v, peak) in [50.0, 52.0, 55.0, 58.0].into_iter().enumerate() {
            let jitter = if v % 2 == 0 { 0.2 } else { -0.2 };
            f = tr
                .observe(t(v as i64), &result(vec![c(jitter, 0.0).peak(peak)]), false)
                .unwrap();
        }
        let s = f.track(0).unwrap();
        assert!(s.stationary, "{:?}", s.motion);
        assert_eq!(s.peak_trend_db, Some(6.0)); // 58 - 52 over three volumes
        assert_eq!(s.peak_dbz, 58.0);
    }

    #[wasm_bindgen_test]
    fn ids_continue_past_z_and_are_never_reused() {
        let mut tr = Tracker::default();
        let cells: Vec<C> = (0..30).map(|i| c(i as f64 * 40.0, 0.0)).collect();
        let f = tr.observe(t(0), &result(cells), false).unwrap();
        let labels: Vec<String> = f
            .cell_tracks
            .iter()
            .map(|id| track_label(id.unwrap()))
            .collect();
        assert_eq!(labels[25], "Z");
        assert_eq!(labels[26], "AA");
        assert_eq!(labels[29], "AD");
        // A restart keeps the counter.
        tr.restart();
        let f = tr.observe(t(1), &result(vec![c(0.0, 0.0)]), false).unwrap();
        assert_eq!(ids(&f), vec![Some(30)]);
    }

    #[wasm_bindgen_test]
    fn the_same_sequence_gives_the_same_frames() {
        let run = || {
            let mut tr = Tracker::default();
            (0..5)
                .map(|v| {
                    let x = v as f64 * 3.0;
                    let cells = vec![c(x, 0.0), c(x + 9.0, 1.0).peak(52.0), c(x, 30.0)];
                    tr.observe(t(v), &result(cells), false).unwrap()
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(run(), run());
    }

    #[wasm_bindgen_test]
    fn tracker_state_round_trips_through_serde() {
        let mut tr = Tracker::default();
        tr.observe(t(0), &result(vec![c(0.0, 0.0)]), false);
        tr.observe(t(1), &result(vec![c(3.0, 0.0)]), false);
        let json = serde_json::to_string(&tr).unwrap();
        let back: Tracker = serde_json::from_str(&json).unwrap();
        assert_eq!(back, tr);
    }

    #[wasm_bindgen_test]
    fn closest_approach_of_a_passing_storm() {
        let me = ORIGIN;
        // 20 km west and 5 km north of me, moving east at 40 km/h.
        let storm = displace(me, -20.0, 5.0);
        let east = Motion {
            east_kmh: 40.0,
            north_kmh: 0.0,
            confidence: 1.0,
        };
        let a = closest_approach(storm, &east, me);
        assert!((a.minutes - 30.0).abs() < 0.2, "{a:?}");
        assert!((a.distance_km - 5.0).abs() < 0.05, "{a:?}");
        // Moving away: closest is now.
        let west = Motion {
            east_kmh: -40.0,
            ..east
        };
        let a = closest_approach(storm, &west, me);
        assert_eq!(a.minutes, 0.0);
        assert!((a.distance_km - 20.0f64.hypot(5.0)).abs() < 0.05);
        // Not moving: closest is now.
        let a = closest_approach(storm, &Motion::default(), me);
        assert_eq!(a.minutes, 0.0);
    }

    #[wasm_bindgen_test]
    fn bearing_and_distance_are_compass_and_km() {
        let (b, d) = bearing_distance(ORIGIN, displace(ORIGIN, 10.0, 10.0));
        assert!((b - 45.0).abs() < 0.1 && (d - 200f64.sqrt()).abs() < 0.05);
        let (b, _) = bearing_distance(ORIGIN, displace(ORIGIN, -1.0, 0.0));
        assert!((b - 270.0).abs() < 0.1);
    }
}
