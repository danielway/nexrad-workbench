//! Worker outcome vocabulary — the pure result types produced by the decode
//! worker and cache loader shells.
//!
//! These are plain data (buffers, keys, timings) with no channels, JsValue,
//! or I/O handles, so core reducers can consume them headlessly. The wire
//! deserialization (`*Msg` structs) and the channel machinery that produces
//! them stay in `crate::nexrad`.

use crate::core::ScanMetadata;

/// How much work the decode-worker pool is carrying right now.
///
/// Each field counts jobs that have been posted to a worker but whose result
/// message has not yet come back to the main thread. That interval covers
/// `postMessage` transit, time queued inside the worker's single-threaded
/// event loop, and the decode itself — the main thread cannot distinguish
/// them without a progress wire message, and this type deliberately does not
/// pretend to. "Processing" means *submitted and unreturned*, nothing finer.
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct WorkerLoad {
    /// Archive volume ingests (split → decompress → decode → store).
    pub ingest: usize,
    /// Live per-chunk ingests.
    pub chunk_ingest: usize,
    /// Archive sweep renders.
    pub render: usize,
    /// Live sweep renders.
    pub render_live: usize,
    /// Whole-volume packs for the 3-D ray marcher.
    pub volume: usize,
    /// Requests posted before the worker signalled `ready`, still parked in
    /// its local queue.
    pub queued_pre_ready: usize,
}

impl WorkerLoad {
    /// Total outstanding jobs across every bucket.
    #[allow(dead_code)] // Read by the activity view-model; pinned by tests meanwhile.
    pub(crate) fn total(&self) -> usize {
        self.ingest
            + self.chunk_ingest
            + self.render
            + self.render_live
            + self.volume
            + self.queued_pre_ready
    }

    /// Field-wise sum, for folding a pool's per-worker loads into one figure.
    pub(crate) fn merge(a: Self, b: Self) -> Self {
        Self {
            ingest: a.ingest + b.ingest,
            chunk_ingest: a.chunk_ingest + b.chunk_ingest,
            render: a.render + b.render,
            render_live: a.render_live + b.render_live,
            volume: a.volume + b.volume,
            queued_pre_ready: a.queued_pre_ready + b.queued_pre_ready,
        }
    }
}

/// Context for an ingest request.
pub(crate) struct IngestContext {
    /// Typed identifier for the volume being ingested. Built once at
    /// dispatch (in `WorkerPool::ingest`) so the worker round-trip
    /// doesn't re-parse the storage-key string — the JS side and the
    /// Rust side ultimately produce the same key from the same inputs.
    pub scan_key: crate::data::ScanKey,
    /// Volume scan start (Unix seconds, sub-second precision). Same value
    /// `scan_key` was built from; kept for diagnostics.
    pub timestamp_secs: f64,
    pub fetch_latency_ms: f64,
}

/// Successful ingest result from the worker.
pub(crate) struct IngestResult {
    pub context: IngestContext,
    /// Typed identifier for the ingested scan, parsed from the storage-key
    /// string the worker actually wrote under (derived from the decoded
    /// volume-header time). Falls back to `context.scan_key` only if that
    /// string is unparseable.
    pub scan_key: crate::data::ScanKey,
    /// Number of records stored in IDB.
    pub records_stored: u32,
    /// Unique elevation numbers found across all records.
    pub elevation_numbers: Vec<u8>,
    /// Per-sweep metadata extracted from radials during ingest.
    pub sweeps: Vec<crate::data::CachedSweep>,
    /// Full extracted VCP pattern (from Message Type 5).
    /// Available for direct VCP inspection; primary propagation is via IDB metadata.
    #[allow(dead_code)]
    // Doc above: kept for direct VCP inspection; IDB metadata is the live path.
    pub vcp: Option<crate::data::ExtractedVcp>,
    /// Total time in worker (ms).
    pub total_ms: f64,
    /// Sub-phase timing: record splitting.
    pub split_ms: f64,
    /// Sub-phase timing: decompression.
    pub decompress_ms: f64,
    /// Sub-phase timing: decoding records.
    pub decode_ms: f64,
    /// Sub-phase timing: sweep extraction.
    pub extract_ms: f64,
    /// Sub-phase timing: IDB store.
    pub store_ms: f64,
    /// Sub-phase timing: index update.
    pub index_ms: f64,
}

/// Context for a per-chunk ingest request (real-time streaming).
pub(crate) struct ChunkIngestContext {
    /// Typed identifier for the in-progress volume. Built once at dispatch
    /// from `(site_id, timestamp_secs)`; every chunk of the volume shares
    /// the same value. Consumers should read this rather than re-parsing
    /// the storage-key string the worker emits on the response.
    pub scan_key: crate::data::ScanKey,
    /// Volume scan start (Unix seconds, sub-second precision). Same value
    /// `scan_key` was built from; kept for diagnostics and lag math.
    pub timestamp_secs: f64,
    pub chunk_index: u32,
}

/// Successful per-chunk ingest result from the worker.
pub(crate) struct ChunkIngestResult {
    pub context: ChunkIngestContext,
    /// Typed identifier for the in-progress volume. Same value as
    /// `context.scan_key`; surfaced separately for ergonomic access.
    pub scan_key: crate::data::ScanKey,
    /// Elevation numbers that became complete with this chunk.
    pub elevations_completed: Vec<u8>,
    /// Number of sweep blobs written to IDB.
    pub sweeps_stored: u32,
    /// Whether this was the final chunk in the volume.
    pub is_end: bool,
    /// Per-sweep metadata for all completed elevations so far.
    pub sweeps: Vec<crate::data::CachedSweep>,
    /// VCP pattern if extracted.
    pub vcp: Option<crate::data::ExtractedVcp>,
    /// Total processing time in worker (ms).
    pub total_ms: f64,
    /// Elevation number currently being accumulated (partial sweep in progress).
    pub current_elevation: Option<u8>,
    /// Number of radials received so far for the current in-progress elevation.
    pub current_elevation_radials: Option<u32>,
    /// Last radial's azimuth angle in degrees (for sweep line extrapolation).
    pub last_radial_azimuth: Option<f32>,
    /// Timestamp of the last radial in Unix seconds (for sweep line extrapolation).
    pub last_radial_time_secs: Option<f64>,
    /// Volume header date/time in Unix seconds (authoritative scan start time).
    pub volume_header_time_secs: Option<f64>,
    /// Earliest radial collection time (Unix seconds) observed in this chunk.
    #[allow(dead_code)]
    // Pairs with chunk_max_time_secs; consumed by the timing debug UI later.
    pub chunk_min_time_secs: Option<f64>,
    /// Latest radial collection time (Unix seconds) observed in this chunk.
    /// Paired with the chunk's S3 upload time yields the per-chunk
    /// availability lag (AVAILABILITY − ACTUAL collection).
    pub chunk_max_time_secs: Option<f64>,
    /// Per-elevation time spans within this chunk:
    /// (elevation_number, start_secs, end_secs, radial_count).
    pub chunk_elev_spans: Vec<(u8, f64, f64, u32)>,
    /// Per-elevation azimuth ranges within this chunk:
    /// (elevation_number, first_azimuth, last_azimuth).
    pub chunk_elev_az_ranges: Vec<(u8, f32, f32)>,
}

/// Context for a render/decode request.
pub(crate) struct RenderContext {
    /// Typed identifier for the scan being rendered. The wire-protocol
    /// uses `to_storage_key()` once at dispatch.
    pub scan_key: crate::data::ScanKey,
    /// Elevation number being rendered.
    pub elevation_number: u8,
}

/// Decoded radar sweep data from the worker (raw data for GPU rendering).
pub(crate) struct DecodeResult {
    pub context: RenderContext,
    /// Sorted azimuth angles in degrees.
    pub azimuths: Vec<f32>,
    /// Flat row-major raw gate values (azimuth_count * gate_count).
    /// Raw u8/u16 values cast to f32. Sentinels: 0=below threshold, 1=range folded.
    pub gate_values: Vec<f32>,
    pub azimuth_count: u32,
    pub gate_count: u32,
    pub first_gate_range_km: f64,
    pub gate_interval_km: f64,
    pub max_range_km: f64,
    pub product: String,
    pub radial_count: u32,
    pub fetch_ms: f64,
    /// Sub-phase timing: deserialization.
    pub deser_ms: f64,
    /// Sub-phase timing: marshalling data for transfer.
    pub marshal_ms: f64,
    /// Total render time in worker (ms).
    pub total_ms: f64,
    /// Scale factor for decoding raw values: physical = (raw - offset) / scale.
    pub scale: f32,
    /// Offset for decoding raw values.
    pub offset: f32,
    /// Mean elevation angle across all radials in the sweep.
    pub mean_elevation: f32,
    /// Sweep start timestamp (Unix seconds).
    pub sweep_start_secs: f64,
    /// Sweep end timestamp (Unix seconds).
    pub sweep_end_secs: f64,
    /// Per-radial collection timestamps in Unix seconds (parallel to azimuths).
    pub radial_times: Vec<f64>,
    /// Median angular spacing between adjacent sorted radials, in degrees.
    /// Used by the shader's search threshold instead of deriving from azimuth_count.
    pub azimuth_spacing_deg: f32,
}

/// Per-sweep metadata for the volume ray marcher.
pub struct VolumeSweepMeta {
    pub elevation_deg: f32,
    pub azimuth_count: u32,
    pub gate_count: u32,
    pub first_gate_km: f32,
    pub gate_interval_km: f32,
    pub max_range_km: f32,
    pub data_offset: u32,
    pub scale: f32,
    pub offset: f32,
}

/// All-elevation packed volume data for ray-march rendering.
pub(crate) struct VolumeData {
    /// Packed raw gate values (all sweeps concatenated).
    /// Byte width per value is determined by `word_size`.
    pub buffer: Vec<u8>,
    /// Bytes per gate value: 1 (R8UI) when all sweeps are u8, 2 (R16UI) otherwise.
    pub word_size: u8,
    /// Per-sweep metadata in ascending elevation-*angle* order, one entry per
    /// distinct angle. Guaranteed by `core::volume_plan::plan_volume_sweeps`,
    /// which the worker packer runs before concatenating gate data — the ray
    /// marcher's bracket search depends on it.
    pub sweeps: Vec<VolumeSweepMeta>,
    pub product: String,
    pub total_ms: f64,
}

/// Result of a cache load operation.
#[derive(Debug, Clone)]
pub(crate) enum CacheLoadResult {
    /// Successfully loaded metadata for a site
    Success {
        site_id: String,
        metadata: Vec<ScanMetadata>,
        /// Total cache size across all sites (in bytes)
        total_cache_size: u64,
    },
    /// Cache load failed with an error
    Error(String),
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use crate::data::{ScanKey, UnixMillis};
    use wasm_bindgen_test::wasm_bindgen_test;

    fn sample_metadata() -> ScanMetadata {
        ScanMetadata {
            key: ScanKey::new("KDMX", UnixMillis(1_700_000_000_000)),
            file_name: "KDMX20230101_000000_V06".to_string(),
            file_size: 4096,
            end_timestamp: Some(1_700_000_300),
            vcp: None,
            completeness: None,
            cached_sweep_count: Some(7),
            planned_sweep_count: Some(14),
            sweeps: None,
        }
    }

    fn load(ingest: usize, render: usize, volume: usize) -> WorkerLoad {
        WorkerLoad {
            ingest,
            render,
            volume,
            ..WorkerLoad::default()
        }
    }

    // ── WorkerLoad ──────────────────────────────────────────────────────────

    /// `merge` sums each bucket independently — no bucket bleeds into another,
    /// so a pool's ingest count stays an ingest count.
    #[wasm_bindgen_test]
    fn worker_load_merge_sums_each_bucket() {
        let a = WorkerLoad {
            ingest: 1,
            chunk_ingest: 2,
            render: 3,
            render_live: 4,
            volume: 5,
            queued_pre_ready: 6,
        };
        let b = WorkerLoad {
            ingest: 10,
            chunk_ingest: 20,
            render: 30,
            render_live: 40,
            volume: 50,
            queued_pre_ready: 60,
        };
        let merged = WorkerLoad::merge(a, b);
        assert_eq!(merged.ingest, 11);
        assert_eq!(merged.chunk_ingest, 22);
        assert_eq!(merged.render, 33);
        assert_eq!(merged.render_live, 44);
        assert_eq!(merged.volume, 55);
        assert_eq!(merged.queued_pre_ready, 66);
    }

    /// `total` counts every bucket, including the pre-ready queue — a request
    /// parked before the worker booted is still outstanding work.
    #[wasm_bindgen_test]
    fn worker_load_total_is_the_field_sum() {
        assert_eq!(WorkerLoad::default().total(), 0);
        assert_eq!(load(2, 3, 1).total(), 6);
        let with_queue = WorkerLoad {
            queued_pre_ready: 4,
            ..load(1, 1, 1)
        };
        assert_eq!(with_queue.total(), 7);
    }

    /// Merging with the identity leaves a load unchanged.
    #[wasm_bindgen_test]
    fn worker_load_merge_with_default_is_identity() {
        let a = load(3, 4, 5);
        assert_eq!(WorkerLoad::merge(a, WorkerLoad::default()), a);
        assert_eq!(WorkerLoad::merge(WorkerLoad::default(), a), a);
    }

    #[wasm_bindgen_test]
    fn success_result_clone_preserves_fields() {
        let original = CacheLoadResult::Success {
            site_id: "KTLX".to_string(),
            metadata: vec![sample_metadata(), sample_metadata()],
            total_cache_size: 100,
        };
        let cloned = original.clone();
        match cloned {
            CacheLoadResult::Success {
                site_id,
                metadata,
                total_cache_size,
            } => {
                assert_eq!(site_id, "KTLX");
                assert_eq!(metadata.len(), 2);
                assert_eq!(total_cache_size, 100);
            }
            other => panic!("expected Success, got {:?}", other),
        }
    }

    #[wasm_bindgen_test]
    fn error_result_clone_preserves_message() {
        let original = CacheLoadResult::Error("io failure".to_string());
        let cloned = original.clone();
        match cloned {
            CacheLoadResult::Error(msg) => assert_eq!(msg, "io failure"),
            other => panic!("expected Error, got {:?}", other),
        }
    }

    #[wasm_bindgen_test]
    fn result_debug_is_non_empty() {
        let s = format!("{:?}", CacheLoadResult::Error("x".to_string()));
        assert!(s.contains("Error"));
        let s2 = format!(
            "{:?}",
            CacheLoadResult::Success {
                site_id: "KDMX".to_string(),
                metadata: Vec::new(),
                total_cache_size: 5,
            }
        );
        assert!(s2.contains("Success"));
    }
}
