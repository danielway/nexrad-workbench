//! Pure timeline commit and cache-snapshot reconciliation.
//!
//! Worker chunk results are already confirmed persisted data, so they can be
//! merged into the in-memory timeline immediately. Cache loads are snapshots:
//! a snapshot dispatched at the current revision may replace the inventory,
//! while one dispatched before a synchronous commit is reconciled with the
//! newer inventory so it cannot erase that commit.

use crate::core::{ChunkIngestResult, IngestResult, RadarTimeline, Scan, Sweep};
use crate::data::{CachedSweep, ScanCompleteness};

/// Monotonic version of the in-memory timeline inventory.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct TimelineRevision(u64);

impl TimelineRevision {
    #[cfg(test)]
    pub(crate) fn value(self) -> u64 {
        self.0
    }

    #[cfg(test)]
    pub(crate) fn from_test_value(value: u64) -> Self {
        Self(value)
    }

    fn advance(&mut self) {
        self.0 = self.0.saturating_add(1);
    }

    fn is_after(self, other: Self) -> bool {
        self > other
    }
}

/// One worker-confirmed scan delta retained until cache snapshots dispatched
/// before it have completed.
#[derive(Clone)]
pub(crate) struct TimelineCommit {
    revision: TimelineRevision,
    scan: Scan,
}

/// How a cache snapshot was committed relative to its dispatch revision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TimelineSnapshotCommit {
    /// No synchronous commit occurred while the load was in flight.
    Replaced,
    /// The snapshot was stale and was unioned with newer in-memory data.
    Reconciled,
}

/// Decide whether a loaded snapshot is current enough to replace the timeline.
pub(crate) fn decide_timeline_snapshot_commit(
    current: TimelineRevision,
    dispatched_at: TimelineRevision,
) -> TimelineSnapshotCommit {
    if current == dispatched_at {
        TimelineSnapshotCommit::Replaced
    } else {
        TimelineSnapshotCommit::Reconciled
    }
}

/// Merge worker-confirmed scan, sweep, and VCP metadata synchronously.
pub(crate) fn commit_chunk_ingest(
    timeline: &mut RadarTimeline,
    revision: &mut TimelineRevision,
    commits: &mut Vec<TimelineCommit>,
    result: &ChunkIngestResult,
) {
    let incoming = scan_from_chunk(result);
    commit_scan(timeline, revision, commits, incoming);
}

/// Merge metadata from a completed archive ingest synchronously.
pub(crate) fn commit_archive_ingest(
    timeline: &mut RadarTimeline,
    revision: &mut TimelineRevision,
    commits: &mut Vec<TimelineCommit>,
    result: &IngestResult,
) {
    let incoming = scan_from_archive(result);
    commit_scan(timeline, revision, commits, incoming);
}

fn commit_scan(
    timeline: &mut RadarTimeline,
    revision: &mut TimelineRevision,
    commits: &mut Vec<TimelineCommit>,
    incoming: Scan,
) {
    merge_scan(&mut timeline.scans, incoming.clone());
    sort_timeline(timeline);
    revision.advance();
    commits.push(TimelineCommit {
        revision: *revision,
        scan: incoming,
    });
}

/// Commit a loaded cache snapshot, preserving commits made since dispatch.
pub(crate) fn commit_timeline_snapshot(
    timeline: &mut RadarTimeline,
    revision: &mut TimelineRevision,
    commits: &mut Vec<TimelineCommit>,
    dispatched_at: TimelineRevision,
    mut snapshot: RadarTimeline,
) -> TimelineSnapshotCommit {
    let decision = decide_timeline_snapshot_commit(*revision, dispatched_at);
    if decision == TimelineSnapshotCommit::Reconciled {
        for commit in commits
            .iter()
            .filter(|commit| commit.revision.is_after(dispatched_at))
        {
            merge_scan(&mut snapshot.scans, commit.scan.clone());
        }
    }
    sort_timeline(&mut snapshot);
    *timeline = snapshot;
    revision.advance();
    commits.retain(|commit| commit.revision.is_after(dispatched_at));
    decision
}

/// Apply an authoritative reset such as an explicit cache wipe.
pub(crate) fn reset_timeline(
    timeline: &mut RadarTimeline,
    revision: &mut TimelineRevision,
    commits: &mut Vec<TimelineCommit>,
) {
    *timeline = RadarTimeline::default();
    commits.clear();
    revision.advance();
}

/// Whether a live chunk result belongs to the active site and volume.
pub(crate) fn live_chunk_matches_scope(
    site_id: &str,
    active_volume: Option<&crate::data::ScanKey>,
    result: &ChunkIngestResult,
) -> bool {
    result.scan_key.site.0 == site_id
        && active_volume.is_none_or(|scan_key| {
            result.scan_key.site == scan_key.site
                && result.scan_key.scan_start >= scan_key.scan_start
        })
}

fn scan_from_chunk(result: &ChunkIngestResult) -> Scan {
    scan_from_persisted(
        &result.scan_key,
        &result.sweeps,
        result.vcp.as_ref(),
        result.chunk_max_time_secs,
    )
}

fn scan_from_archive(result: &IngestResult) -> Scan {
    scan_from_persisted(&result.scan_key, &result.sweeps, result.vcp.as_ref(), None)
}

fn scan_from_persisted(
    scan_key: &crate::data::ScanKey,
    cached_sweeps: &[CachedSweep],
    vcp: Option<&crate::data::ExtractedVcp>,
    additional_end_secs: Option<f64>,
) -> Scan {
    let key_timestamp = scan_key.scan_start.as_secs_f64();
    let sweeps: Vec<Sweep> = cached_sweeps.iter().map(sweep_from_cached).collect();
    let start_time = sweeps
        .iter()
        .map(|sweep| sweep.start_time)
        .fold(key_timestamp, f64::min);
    let end_time = sweeps
        .iter()
        .map(|sweep| sweep.end_time)
        .chain(additional_end_secs)
        .fold(key_timestamp, f64::max);
    let planned_sweep_count = vcp.map(|vcp| vcp.elevations.len() as u32);
    let cached_sweep_count = sweeps.len() as u32;

    Scan {
        start_time,
        end_time,
        key_timestamp,
        vcp: vcp.map(|vcp| vcp.number).unwrap_or(0),
        vcp_pattern: vcp.cloned(),
        sweeps,
        completeness: Some(ScanCompleteness::from_counts(
            vcp.is_some(),
            cached_sweep_count,
            planned_sweep_count,
        )),
        cached_sweep_count: Some(cached_sweep_count),
        planned_sweep_count,
    }
}

fn sweep_from_cached(cached: &CachedSweep) -> Sweep {
    Sweep {
        start_time: cached.start,
        end_time: cached.end,
        elevation: cached.elevation,
        elevation_number: cached.elevation_number,
        start_azimuth: cached.start_azimuth,
        radials: Vec::new(),
        cached_products: cached.cached_products.clone(),
    }
}

fn merge_scan(scans: &mut Vec<Scan>, incoming: Scan) {
    if let Some(existing) = scans
        .iter_mut()
        .find(|scan| scan.key_ms() == incoming.key_ms())
    {
        merge_scan_metadata(existing, incoming);
    } else {
        scans.push(incoming);
    }
}

fn merge_scan_metadata(existing: &mut Scan, incoming: Scan) {
    existing.start_time = existing.start_time.min(incoming.start_time);
    existing.end_time = existing.end_time.max(incoming.end_time);
    if incoming.vcp_pattern.is_some() {
        existing.vcp = incoming.vcp;
        existing.vcp_pattern = incoming.vcp_pattern;
    }
    existing.completeness = incoming.completeness.or(existing.completeness);
    existing.cached_sweep_count =
        max_option(existing.cached_sweep_count, incoming.cached_sweep_count);
    existing.planned_sweep_count =
        max_option(existing.planned_sweep_count, incoming.planned_sweep_count);

    for incoming_sweep in incoming.sweeps {
        if let Some(existing_sweep) = existing
            .sweeps
            .iter_mut()
            .find(|sweep| sweep.elevation_number == incoming_sweep.elevation_number)
        {
            merge_sweep(existing_sweep, incoming_sweep);
        } else {
            existing.sweeps.push(incoming_sweep);
        }
    }
    existing.sweeps.sort_by(|a, b| {
        a.elevation_number
            .cmp(&b.elevation_number)
            .then_with(|| a.start_time.total_cmp(&b.start_time))
    });

    let cached = existing.sweeps.len() as u32;
    existing.cached_sweep_count = Some(existing.cached_sweep_count.unwrap_or(0).max(cached));
    existing.completeness = Some(ScanCompleteness::from_counts(
        existing.vcp_pattern.is_some(),
        existing.cached_sweep_count.unwrap_or(cached),
        existing.planned_sweep_count,
    ));
}

fn merge_sweep(existing: &mut Sweep, mut incoming: Sweep) {
    for product in std::mem::take(&mut existing.cached_products) {
        if !incoming.cached_products.contains(&product) {
            incoming.cached_products.push(product);
        }
    }
    if incoming.radials.is_empty() {
        incoming.radials = std::mem::take(&mut existing.radials);
    }
    *existing = incoming;
}

fn max_option(a: Option<u32>, b: Option<u32>) -> Option<u32> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}

fn sort_timeline(timeline: &mut RadarTimeline) {
    timeline.scans.sort_by(|a, b| {
        a.start_time
            .total_cmp(&b.start_time)
            .then_with(|| a.key_ms().cmp(&b.key_ms()))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::playback_manager::{resolve_desired_display, DesiredDisplay};
    use crate::core::{ChunkIngestContext, ElevationSelection, IngestContext, RadarProduct};
    use crate::data::{ExtractedVcp, ExtractedVcpElevation, ScanKey};
    use wasm_bindgen_test::wasm_bindgen_test;

    fn cached(elevation_number: u8, product: &str) -> CachedSweep {
        CachedSweep {
            start: 1_000.0 + f64::from(elevation_number),
            end: 1_010.0 + f64::from(elevation_number),
            elevation: f32::from(elevation_number) * 0.5,
            elevation_number,
            start_azimuth: 10.0,
            cached_products: vec![product.to_string()],
        }
    }

    fn vcp(number: u16, count: usize) -> ExtractedVcp {
        ExtractedVcp {
            number,
            elevations: (0..count)
                .map(|index| ExtractedVcpElevation {
                    angle: index as f32 * 0.5,
                    waveform: "CS".to_string(),
                    prf_number: 1,
                    is_sails: false,
                    is_mrle: false,
                    is_base_tilt: index == 0,
                    azimuth_rate: Some(20.0),
                })
                .collect(),
        }
    }

    fn chunk(sweeps: Vec<CachedSweep>, vcp: Option<ExtractedVcp>) -> ChunkIngestResult {
        let scan_key = ScanKey::from_secs("KDMX", 1_000);
        ChunkIngestResult {
            context: ChunkIngestContext {
                scan_key: scan_key.clone(),
                timestamp_secs: 1_000.0,
                chunk_index: 0,
            },
            scan_key,
            elevations_completed: sweeps.iter().map(|s| s.elevation_number).collect(),
            sweeps_stored: sweeps.len() as u32,
            is_end: false,
            sweeps,
            vcp,
            total_ms: 1.0,
            current_elevation: None,
            current_elevation_radials: None,
            last_radial_azimuth: None,
            last_radial_time_secs: None,
            volume_header_time_secs: Some(1_000.0),
            chunk_min_time_secs: None,
            chunk_max_time_secs: None,
            chunk_elev_spans: Vec::new(),
            chunk_elev_az_ranges: Vec::new(),
        }
    }

    fn archive(sweeps: Vec<CachedSweep>, vcp: Option<ExtractedVcp>) -> IngestResult {
        let scan_key = ScanKey::from_secs("KDMX", 1_003);
        IngestResult {
            context: IngestContext {
                scan_key: scan_key.clone(),
                timestamp_secs: 1_000.0,
                fetch_latency_ms: 1.0,
            },
            scan_key,
            records_stored: 1,
            elevation_numbers: sweeps.iter().map(|s| s.elevation_number).collect(),
            sweeps,
            vcp,
            total_ms: 1.0,
            split_ms: 0.1,
            decompress_ms: 0.1,
            decode_ms: 0.1,
            extract_ms: 0.1,
            store_ms: 0.1,
            index_ms: 0.1,
        }
    }

    #[wasm_bindgen_test]
    fn chunk_commits_advance_revision_and_preserve_prior_sweeps() {
        let mut timeline = RadarTimeline::default();
        let mut revision = TimelineRevision::default();
        let mut commits = Vec::new();

        commit_chunk_ingest(
            &mut timeline,
            &mut revision,
            &mut commits,
            &chunk(vec![cached(1, "reflectivity")], Some(vcp(215, 2))),
        );
        commit_chunk_ingest(
            &mut timeline,
            &mut revision,
            &mut commits,
            &chunk(vec![cached(2, "velocity")], None),
        );

        assert_eq!(revision.value(), 2);
        assert_eq!(timeline.scans.len(), 1);
        assert_eq!(timeline.scans[0].vcp, 215);
        assert_eq!(
            timeline.scans[0]
                .sweeps
                .iter()
                .map(|s| s.elevation_number)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
    }

    #[wasm_bindgen_test]
    fn repeated_sweep_commit_unions_cached_products() {
        let mut timeline = RadarTimeline::default();
        let mut revision = TimelineRevision::default();
        let mut commits = Vec::new();
        commit_chunk_ingest(
            &mut timeline,
            &mut revision,
            &mut commits,
            &chunk(vec![cached(1, "reflectivity")], None),
        );
        commit_chunk_ingest(
            &mut timeline,
            &mut revision,
            &mut commits,
            &chunk(vec![cached(1, "velocity")], None),
        );

        let products = &timeline.scans[0].sweeps[0].cached_products;
        assert!(products.iter().any(|p| p == "reflectivity"));
        assert!(products.iter().any(|p| p == "velocity"));
    }

    #[wasm_bindgen_test]
    fn snapshot_decision_is_based_on_dispatch_revision() {
        let dispatched = TimelineRevision::default();
        assert_eq!(
            decide_timeline_snapshot_commit(dispatched, dispatched),
            TimelineSnapshotCommit::Replaced
        );
        let mut current = dispatched;
        current.advance();
        assert_eq!(
            decide_timeline_snapshot_commit(current, dispatched),
            TimelineSnapshotCommit::Reconciled
        );
    }

    #[wasm_bindgen_test]
    fn stale_snapshot_reconciles_without_erasing_newer_chunk() {
        let mut timeline = RadarTimeline::default();
        let mut revision = TimelineRevision::default();
        let mut commits = Vec::new();
        commit_chunk_ingest(
            &mut timeline,
            &mut revision,
            &mut commits,
            &chunk(vec![cached(1, "reflectivity")], Some(vcp(215, 2))),
        );
        let dispatched = revision;
        commit_chunk_ingest(
            &mut timeline,
            &mut revision,
            &mut commits,
            &chunk(vec![cached(2, "velocity")], None),
        );

        let mut snapshot = RadarTimeline::default();
        let mut snapshot_revision = TimelineRevision::default();
        let mut snapshot_commits = Vec::new();
        commit_chunk_ingest(
            &mut snapshot,
            &mut snapshot_revision,
            &mut snapshot_commits,
            &chunk(vec![cached(1, "reflectivity")], Some(vcp(215, 2))),
        );
        let decision = commit_timeline_snapshot(
            &mut timeline,
            &mut revision,
            &mut commits,
            dispatched,
            snapshot,
        );

        assert_eq!(decision, TimelineSnapshotCommit::Reconciled);
        assert_eq!(revision.value(), 3);
        assert_eq!(timeline.scans.len(), 1);
        assert_eq!(
            timeline.scans[0]
                .sweeps
                .iter()
                .map(|s| s.elevation_number)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
    }

    #[wasm_bindgen_test]
    fn archive_ingest_is_immediate_and_survives_older_snapshot() {
        let mut timeline = RadarTimeline::default();
        let mut revision = TimelineRevision::default();
        let mut commits = Vec::new();
        let dispatched_before_ingest = revision;

        commit_archive_ingest(
            &mut timeline,
            &mut revision,
            &mut commits,
            &archive(vec![cached(2, "reflectivity")], Some(vcp(215, 2))),
        );

        assert_eq!(revision.value(), 1);
        assert_eq!(timeline.scans.len(), 1);
        assert_eq!(timeline.scans[0].key_ms(), 1_003_000);
        assert_eq!(timeline.scans[0].sweeps[0].elevation_number, 2);

        let decision = commit_timeline_snapshot(
            &mut timeline,
            &mut revision,
            &mut commits,
            dispatched_before_ingest,
            RadarTimeline::default(),
        );

        assert_eq!(decision, TimelineSnapshotCommit::Reconciled);
        assert_eq!(timeline.scans.len(), 1);
        assert_eq!(timeline.scans[0].sweeps[0].elevation_number, 2);
    }

    #[wasm_bindgen_test]
    fn current_snapshot_replaces_prior_inventory() {
        let mut timeline = RadarTimeline::default();
        let mut revision = TimelineRevision::default();
        let mut commits = Vec::new();
        commit_chunk_ingest(
            &mut timeline,
            &mut revision,
            &mut commits,
            &chunk(vec![cached(1, "reflectivity")], None),
        );
        let dispatched = revision;

        let decision = commit_timeline_snapshot(
            &mut timeline,
            &mut revision,
            &mut commits,
            dispatched,
            RadarTimeline::default(),
        );

        assert_eq!(decision, TimelineSnapshotCommit::Replaced);
        assert!(timeline.scans.is_empty());
        assert_eq!(revision.value(), 2);
    }

    #[wasm_bindgen_test]
    fn latest_handoff_is_live_while_collecting_and_newest_cached_while_waiting() {
        let mut timeline = RadarTimeline::default();
        let mut revision = TimelineRevision::default();
        let mut commits = Vec::new();

        let a = chunk(vec![cached(1, "reflectivity")], Some(vcp(215, 2)));
        commit_chunk_ingest(&mut timeline, &mut revision, &mut commits, &a);
        let stale_snapshot = timeline.clone();
        let stale_dispatched_at = revision;

        let collecting_b = resolve_desired_display(
            "KDMX",
            1_020.0,
            &ElevationSelection::Latest,
            RadarProduct::Reflectivity,
            &timeline,
            900.0,
            Some((2, 1_000_000)),
        );
        assert_eq!(
            collecting_b,
            DesiredDisplay::LivePartial {
                elevation_number: 2
            }
        );

        let b = chunk(
            vec![cached(1, "reflectivity"), cached(2, "reflectivity")],
            None,
        );
        commit_chunk_ingest(&mut timeline, &mut revision, &mut commits, &b);

        let waiting = resolve_desired_display(
            "KDMX",
            1_020.0,
            &ElevationSelection::Latest,
            RadarProduct::Reflectivity,
            &timeline,
            900.0,
            None,
        );
        match waiting {
            DesiredDisplay::Cached(identity) => assert_eq!(identity.elevation_number, 2),
            other => panic!("expected newest cached sweep B, got {other:?}"),
        }

        let decision = commit_timeline_snapshot(
            &mut timeline,
            &mut revision,
            &mut commits,
            stale_dispatched_at,
            stale_snapshot,
        );
        assert_eq!(decision, TimelineSnapshotCommit::Reconciled);
        let after_stale_load = resolve_desired_display(
            "KDMX",
            1_020.0,
            &ElevationSelection::Latest,
            RadarProduct::Reflectivity,
            &timeline,
            900.0,
            None,
        );
        match after_stale_load {
            DesiredDisplay::Cached(identity) => assert_eq!(identity.elevation_number, 2),
            other => panic!("stale load rolled back newest sweep: {other:?}"),
        }
    }

    #[wasm_bindgen_test]
    fn stale_snapshot_preserves_only_post_dispatch_commits() {
        let mut timeline = RadarTimeline::default();
        let mut revision = TimelineRevision::default();
        let mut commits = Vec::new();
        commit_chunk_ingest(
            &mut timeline,
            &mut revision,
            &mut commits,
            &chunk(vec![cached(1, "reflectivity")], None),
        );
        let dispatched = revision;
        commit_chunk_ingest(
            &mut timeline,
            &mut revision,
            &mut commits,
            &chunk(vec![cached(2, "reflectivity")], None),
        );

        let decision = commit_timeline_snapshot(
            &mut timeline,
            &mut revision,
            &mut commits,
            dispatched,
            RadarTimeline::default(),
        );

        assert_eq!(decision, TimelineSnapshotCommit::Reconciled);
        assert_eq!(timeline.scans.len(), 1);
        assert_eq!(timeline.scans[0].sweeps.len(), 1);
        assert_eq!(timeline.scans[0].sweeps[0].elevation_number, 2);
    }

    #[wasm_bindgen_test]
    fn authoritative_reset_discards_inventory_and_commit_journal() {
        let mut timeline = RadarTimeline::default();
        let mut revision = TimelineRevision::default();
        let mut commits = Vec::new();
        commit_chunk_ingest(
            &mut timeline,
            &mut revision,
            &mut commits,
            &chunk(vec![cached(1, "reflectivity")], None),
        );

        reset_timeline(&mut timeline, &mut revision, &mut commits);

        assert!(timeline.scans.is_empty());
        assert!(commits.is_empty());
        assert_eq!(revision.value(), 2);
    }

    #[wasm_bindgen_test]
    fn live_chunk_scope_rejects_stale_site_and_volume() {
        let result = chunk(vec![cached(1, "reflectivity")], None);
        assert!(live_chunk_matches_scope("KDMX", None, &result));
        assert!(live_chunk_matches_scope(
            "KDMX",
            Some(&result.scan_key),
            &result
        ));
        assert!(!live_chunk_matches_scope("KTLX", None, &result));
        assert!(!live_chunk_matches_scope(
            "KDMX",
            Some(&ScanKey::from_secs("KDMX", 2_000)),
            &result
        ));
        assert!(live_chunk_matches_scope(
            "KDMX",
            Some(&ScanKey::from_secs("KDMX", 500)),
            &result
        ));
    }
}
