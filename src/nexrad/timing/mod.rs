// Parts of the forked surface (e.g. `project_full_scan_timing`, the `ChunkProjection`
// accessors, `ChunkTimingStats::get_statistics`) aren't currently wired into this
// crate but are kept intact to preserve the upstream shape for easy diffing and
// eventual contribution back to `nexrad-data`.
#![allow(dead_code, unused_imports)]

//! Local fork of the real-time chunk timing prediction logic from
//! `nexrad_data::aws::realtime`. Kept here so we can iterate on the physics
//! model, statistics blending, and projection shape without cutting a new
//! `nexrad-data` release each time. Intended to be contributed back upstream
//! once stabilised.
//!
//! Public surface mirrors the upstream module: `estimate_chunk_availability_time`,
//! `project_scan_timing`, `project_full_scan_timing`, `ChunkTimingStats`,
//! `ChunkCharacteristics`, `ElevationChunkMapper`, `ChunkMetadata`,
//! `ChunkTimingModel`, `ScanTimingProjection`, `ChunkProjection`.
//!
//! Types that are NOT forked (still sourced from `nexrad_data::aws::realtime`):
//! `ChunkIdentifier`, `ChunkType`, `VolumeIndex` — these are plumbing types
//! used by the forked code but aren't part of the timing logic itself.

mod chunk_timing_model;
mod chunk_timing_stats;
mod elevation_chunk_mapper;
mod estimate_next_chunk_time;
mod scan_timing_projection;

pub use chunk_timing_model::{ChunkTimingModel, IntervalCase, PhysicsBreakdown};
pub use chunk_timing_stats::{BucketStats, ChunkCharacteristics, ChunkTimingStats};
pub use elevation_chunk_mapper::{ChunkMetadata, ElevationChunkMapper};
pub use estimate_next_chunk_time::{
    estimate_chunk_availability_time, estimate_chunk_processing_diagnostics,
    estimate_chunk_processing_time, EstimatedChunkProcessing, SchedulerPath,
};
pub use scan_timing_projection::{
    project_full_scan_timing, project_scan_timing, AnchorSource, ChunkProjection,
    ScanTimingProjection,
};
