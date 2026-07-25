use crate::nexrad::live::streaming_filter::StreamingFilter;
use nexrad_decode::messages::volume_coverage_pattern::{self, WaveformType};

/// Metadata describing a chunk's position within the volume scan.
///
/// Each chunk in a real-time NEXRAD volume has a sequence number (1-based).
/// Sequence 1 is always the Start chunk containing metadata (VCP, site info).
/// Subsequent sequences contain radar data, grouped by elevation sweep.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChunkMetadata {
    /// Sequence number of this chunk (1-based).
    sequence: usize,
    /// The 1-based elevation number this chunk belongs to, or None for the Start chunk.
    elevation_number: Option<usize>,
    /// The chunk's 0-based index within its sweep (e.g., 0..5 for super-res, 0..2 for standard).
    chunk_index_in_sweep: usize,
    /// Total number of chunks in this sweep (3 for standard, 6 for super-resolution).
    chunks_in_sweep: usize,
    /// Whether this is the first chunk in a sweep (inter-sweep gap applies before this chunk).
    is_first_in_sweep: bool,
    /// Whether this is the last chunk in a sweep.
    is_last_in_sweep: bool,
    /// Azimuth rotation rate for this elevation in degrees/second (from VCP).
    azimuth_rate_dps: f64,
    /// Elevation angle in degrees (from VCP).
    elevation_angle_deg: f64,
    /// Waveform type for this elevation (from VCP). `None` for the Start chunk.
    waveform_type: Option<WaveformType>,
    /// Whether this is the Start chunk (sequence 1, metadata-only).
    is_start_chunk: bool,
}

impl ChunkMetadata {
    /// Test-only constructor — estimation tests need metadata pairs without
    /// building a full VCP message.
    #[cfg(test)]
    pub(super) fn for_test(
        sequence: usize,
        elevation_number: Option<usize>,
        chunk_index_in_sweep: usize,
        chunks_in_sweep: usize,
        is_first_in_sweep: bool,
        azimuth_rate_dps: f64,
    ) -> Self {
        Self {
            sequence,
            elevation_number,
            chunk_index_in_sweep,
            chunks_in_sweep,
            is_first_in_sweep,
            is_last_in_sweep: chunk_index_in_sweep + 1 == chunks_in_sweep,
            azimuth_rate_dps,
            elevation_angle_deg: 0.5,
            waveform_type: None,
            is_start_chunk: sequence == 1,
        }
    }

    /// The sequence number of this chunk (1-based).
    pub fn sequence(&self) -> usize {
        self.sequence
    }

    /// The 1-based elevation number this chunk belongs to, or None for the Start chunk.
    pub fn elevation_number(&self) -> Option<usize> {
        self.elevation_number
    }

    /// The chunk's 0-based index within its sweep.
    pub fn chunk_index_in_sweep(&self) -> usize {
        self.chunk_index_in_sweep
    }

    /// Total number of chunks in this sweep (3 for standard, 6 for super-resolution).
    pub fn chunks_in_sweep(&self) -> usize {
        self.chunks_in_sweep
    }

    /// Whether this is the first chunk in a sweep.
    pub fn is_first_in_sweep(&self) -> bool {
        self.is_first_in_sweep
    }

    /// Whether this is the last chunk in a sweep.
    pub fn is_last_in_sweep(&self) -> bool {
        self.is_last_in_sweep
    }

    /// Azimuth rotation rate for this elevation in degrees/second.
    pub fn azimuth_rate_dps(&self) -> f64 {
        self.azimuth_rate_dps
    }

    /// Elevation angle in degrees.
    pub fn elevation_angle_deg(&self) -> f64 {
        self.elevation_angle_deg
    }

    /// Waveform type for this elevation. `None` for the Start chunk.
    pub fn waveform_type(&self) -> Option<WaveformType> {
        self.waveform_type
    }

    /// Whether this is the Start chunk (sequence 1, metadata-only).
    pub fn is_start_chunk(&self) -> bool {
        self.is_start_chunk
    }
}

/// Maps between real-time chunk sequence numbers and volume coverage pattern elevation numbers.
#[derive(Debug)]
pub struct ElevationChunkMapper {
    // Index is elevation number - 1, value is chunk range inclusive
    elevation_chunk_mappings: Vec<(usize, usize)>,
    // Metadata for every chunk, indexed by (sequence - 1)
    chunk_metadata: Vec<ChunkMetadata>,
}

impl ElevationChunkMapper {
    /// Create a new mapper from a volume coverage pattern.
    pub fn new(vcp: &volume_coverage_pattern::Message) -> Self {
        let mut elevation_chunk_mappings = Vec::new();
        let mut chunk_metadata = Vec::new();

        // Sequence 1 is the Start chunk (metadata-only)
        chunk_metadata.push(ChunkMetadata {
            sequence: 1,
            elevation_number: None,
            chunk_index_in_sweep: 0,
            chunks_in_sweep: 1,
            is_first_in_sweep: false,
            is_last_in_sweep: false,
            azimuth_rate_dps: 0.0,
            elevation_angle_deg: 0.0,
            waveform_type: None,
            is_start_chunk: true,
        });

        let mut total_chunk_count = 2;
        for (elev_idx, elevation) in vcp.elevations().iter().enumerate() {
            let elevation_chunk_count = if elevation.super_resolution_half_degree_azimuth() {
                6 // 720 radials / 120 chunks per chunk
            } else {
                3 // 360 radials / 120 chunks per chunk
            };

            let start_seq = total_chunk_count;
            let end_seq = total_chunk_count + elevation_chunk_count - 1;
            elevation_chunk_mappings.push((start_seq, end_seq));

            let azimuth_rate = elevation.azimuth_rate();
            let elevation_angle = elevation.elevation_angle();
            let waveform_type = elevation.waveform_type();

            for chunk_idx in 0..elevation_chunk_count {
                let seq = total_chunk_count + chunk_idx;
                chunk_metadata.push(ChunkMetadata {
                    sequence: seq,
                    elevation_number: Some(elev_idx + 1),
                    chunk_index_in_sweep: chunk_idx,
                    chunks_in_sweep: elevation_chunk_count,
                    is_first_in_sweep: chunk_idx == 0,
                    is_last_in_sweep: chunk_idx == elevation_chunk_count - 1,
                    azimuth_rate_dps: azimuth_rate,
                    elevation_angle_deg: elevation_angle,
                    waveform_type: Some(waveform_type),
                    is_start_chunk: false,
                });
            }

            total_chunk_count += elevation_chunk_count;
        }

        Self {
            elevation_chunk_mappings,
            chunk_metadata,
        }
    }

    /// Get the elevation number for a given sequence number. Returns None if the sequence number
    /// does not correspond to a radar scan described by the VCP.
    pub fn get_sequence_elevation_number(&self, sequence: usize) -> Option<usize> {
        // The first chunk is metadata, not a radar scan described by the VCP
        if sequence == 1 {
            return None;
        }

        self.elevation_chunk_mappings
            .iter()
            .position(|(start, end)| sequence >= *start && sequence <= *end)
            .map(|elevation_index| elevation_index + 1)
    }

    /// Returns the final sequence number for the volume.
    pub fn final_sequence(&self) -> usize {
        self.elevation_chunk_mappings
            .last()
            .map(|(_, end)| *end)
            .unwrap_or(0)
    }

    /// Get rich metadata for a specific chunk sequence number.
    ///
    /// Returns None if the sequence number is out of range.
    pub fn get_chunk_metadata(&self, sequence: usize) -> Option<&ChunkMetadata> {
        if sequence == 0 || sequence > self.chunk_metadata.len() {
            return None;
        }
        Some(&self.chunk_metadata[sequence - 1])
    }

    /// Get metadata for all chunks in the volume (including the Start chunk at index 0).
    pub fn all_chunk_metadata(&self) -> &[ChunkMetadata] {
        &self.chunk_metadata
    }

    /// Total number of chunks in the volume (including the Start chunk).
    pub fn total_chunks(&self) -> usize {
        self.chunk_metadata.len()
    }

    /// Find the next sequence after `current` that matches an elevation
    /// predicate, scanning up through `final_sequence()`. Used by the
    /// filter-aware streaming path to skip chunks that the renderer won't
    /// display.
    ///
    /// `accept_end` is consulted only when the candidate is `final_sequence`:
    /// callers that want to keep volume-boundary signaling pass `true`, callers
    /// that are happy to synthesize the boundary pass `false`.
    pub fn next_matching_sequence_after(
        &self,
        current: usize,
        accept_end: bool,
        mut predicate: impl FnMut(Option<usize>) -> bool,
    ) -> Option<usize> {
        let final_seq = self.final_sequence();
        let start = current.saturating_add(1);
        for seq in start..=final_seq {
            let elev = self
                .get_chunk_metadata(seq)
                .and_then(|m| m.elevation_number());
            if predicate(elev) {
                return Some(seq);
            }
            if accept_end && seq == final_seq {
                return Some(seq);
            }
        }
        None
    }

    /// Whether any sequence strictly after `after_seq` carries a chunk the
    /// filter accepts. Used by the projector to decide whether to extend
    /// projection into the next volume — when the active filter has no
    /// remaining match in the current volume, the next download lands in
    /// the next volume's chunks, and projected times for that hop need to
    /// be available to the scheduler and UI.
    ///
    /// For [`StreamingFilter::All`] this returns `true` whenever any chunk
    /// remains (Start chunks are always accepted), so the projector won't
    /// extend — the streaming loop's next fetch naturally rolls over via
    /// the existing `try_fetch_volume_start` path without needing
    /// chained projection.
    pub fn has_remaining_match(&self, filter: StreamingFilter, after_seq: usize) -> bool {
        self.next_matching_sequence_after(after_seq, false, |elev| filter.accepts(elev))
            .is_some()
    }

    /// Sequences in the volume that match the predicate, restricted to the
    /// range `[lower, upper]` inclusive. Used by the filter-aware backfill
    /// path to determine which already-uploaded chunks to download in
    /// parallel.
    pub fn matching_sequences_in_range(
        &self,
        lower: usize,
        upper: usize,
        mut predicate: impl FnMut(Option<usize>) -> bool,
    ) -> Vec<usize> {
        let lower = lower.max(1);
        let upper = upper.min(self.final_sequence());
        let mut out = Vec::new();
        for seq in lower..=upper {
            let elev = self
                .get_chunk_metadata(seq)
                .and_then(|m| m.elevation_number());
            if predicate(elev) {
                out.push(seq);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_vcp::{build_vcp, TestElevation};
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// A 2-elevation VCP: elev 1 super-res (6 chunks), elev 2 standard (3).
    /// Sequence layout: 1=Start, 2..=7 elev1, 8..=10 elev2; final=10.
    fn mapper_2elev() -> ElevationChunkMapper {
        let vcp = build_vcp(&[
            TestElevation {
                super_res: true,
                elevation_angle_raw: 0,
                azimuth_rate_raw: 1 << 14, // 22.5 dps
                waveform_raw: 1,           // CS
                channel_raw: 0,
            },
            TestElevation::standard_cs(0, 1 << 14),
        ]);
        ElevationChunkMapper::new(&vcp)
    }

    #[wasm_bindgen_test]
    fn new_builds_the_full_sequence_layout() {
        let m = mapper_2elev();
        // Start chunk at seq 1 + 6 + 3 data chunks = 10 total.
        assert_eq!(m.total_chunks(), 10);
        assert_eq!(m.final_sequence(), 10);

        // Start chunk.
        let start = m.get_chunk_metadata(1).unwrap();
        assert!(start.is_start_chunk());
        assert_eq!(start.elevation_number(), None);
        assert!(!start.is_first_in_sweep());

        // Elevation 1 occupies seqs 2..=7 (6 super-res chunks).
        for (idx, seq) in (2..=7).enumerate() {
            let md = m.get_chunk_metadata(seq).unwrap();
            assert_eq!(md.elevation_number(), Some(1));
            assert_eq!(md.chunks_in_sweep(), 6);
            assert_eq!(md.chunk_index_in_sweep(), idx);
            assert_eq!(md.is_first_in_sweep(), idx == 0);
            assert_eq!(md.is_last_in_sweep(), idx == 5);
            assert!(!md.is_start_chunk());
        }

        // Elevation 2 occupies seqs 8..=10 (3 standard chunks).
        for (idx, seq) in (8..=10).enumerate() {
            let md = m.get_chunk_metadata(seq).unwrap();
            assert_eq!(md.elevation_number(), Some(2));
            assert_eq!(md.chunks_in_sweep(), 3);
            assert_eq!(md.chunk_index_in_sweep(), idx);
            assert_eq!(md.is_first_in_sweep(), idx == 0);
            assert_eq!(md.is_last_in_sweep(), idx == 2);
        }
    }

    #[wasm_bindgen_test]
    fn get_sequence_elevation_number_boundaries() {
        let m = mapper_2elev();
        // Start chunk maps to no elevation.
        assert_eq!(m.get_sequence_elevation_number(1), None);
        // First/last seq of each elevation range.
        assert_eq!(m.get_sequence_elevation_number(2), Some(1));
        assert_eq!(m.get_sequence_elevation_number(7), Some(1));
        assert_eq!(m.get_sequence_elevation_number(8), Some(2));
        assert_eq!(m.get_sequence_elevation_number(10), Some(2));
        // Past the final sequence → None.
        assert_eq!(m.get_sequence_elevation_number(11), None);
    }

    #[wasm_bindgen_test]
    fn get_chunk_metadata_off_by_one_guards() {
        let m = mapper_2elev();
        // seq 0 is invalid (1-based sequencing).
        assert!(m.get_chunk_metadata(0).is_none());
        // seq == len is the last valid chunk.
        assert!(m.get_chunk_metadata(10).is_some());
        // seq > len → None.
        assert!(m.get_chunk_metadata(11).is_none());
    }

    #[wasm_bindgen_test]
    fn next_matching_after_accepts_all_none_subset() {
        let m = mapper_2elev();
        // Accept all: from seq 1, the next match is seq 2.
        assert_eq!(m.next_matching_sequence_after(1, false, |_| true), Some(2));
        // Accept none: never matches; with accept_end=false → None (drives the
        // synthetic volume end in streaming_state).
        assert_eq!(m.next_matching_sequence_after(1, false, |_| false), None);
        // Subset: only elevation 2 (seqs 8..=10). From seq 1 the next is 8.
        let elev2 = |e: Option<usize>| e == Some(2);
        assert_eq!(m.next_matching_sequence_after(1, false, elev2), Some(8));
        // From inside elevation 2 (seq 8) the next elev-2 match is seq 9.
        assert_eq!(m.next_matching_sequence_after(8, false, elev2), Some(9));
        // After the last elev-2 chunk there is no further match.
        assert_eq!(m.next_matching_sequence_after(10, false, elev2), None);
    }

    #[wasm_bindgen_test]
    fn next_matching_after_accept_end_at_final_sequence() {
        let m = mapper_2elev();
        // A predicate that rejects everything: with accept_end=false there is
        // no match even at the final sequence.
        assert_eq!(m.next_matching_sequence_after(9, false, |_| false), None);
        // With accept_end=true the final sequence (10) is returned even though
        // the predicate rejects it — volume-boundary signaling.
        assert_eq!(m.next_matching_sequence_after(9, true, |_| false), Some(10));
        // saturating_add: starting at usize::MAX doesn't panic and yields None.
        assert_eq!(
            m.next_matching_sequence_after(usize::MAX, true, |_| true),
            None
        );
    }

    #[wasm_bindgen_test]
    fn has_remaining_match_honors_filter() {
        let m = mapper_2elev();
        // `All` always has a remaining match while chunks remain (so the
        // projector never extends into the next volume for All).
        assert!(m.has_remaining_match(StreamingFilter::All, 1));
        assert!(m.has_remaining_match(StreamingFilter::All, 9));
        // After the final sequence, nothing remains even for All.
        assert!(!m.has_remaining_match(StreamingFilter::All, 10));
        // Filtering to elevation 2: a remaining match exists before seq 10 but
        // not at/after it.
        assert!(m.has_remaining_match(StreamingFilter::Elevation(2), 1));
        assert!(!m.has_remaining_match(StreamingFilter::Elevation(2), 10));
        // Filtering to elevation 1: no remaining match once we're past seq 7.
        assert!(!m.has_remaining_match(StreamingFilter::Elevation(1), 7));
    }

    #[wasm_bindgen_test]
    fn matching_sequences_in_range_clamps_and_excludes_start() {
        let m = mapper_2elev();
        // Accept-all over an over-wide range clamps to [1, final]. The Start
        // chunk (seq 1, elev None) is included only if the predicate accepts
        // None — here accept-all does, so seq 1 appears.
        let all = m.matching_sequences_in_range(0, 999, |_| true);
        assert_eq!(all, (1..=10).collect::<Vec<_>>());

        // Excluding the Start chunk: a predicate that requires Some(elev).
        let data_only = m.matching_sequences_in_range(0, 999, |e| e.is_some());
        assert_eq!(data_only, (2..=10).collect::<Vec<_>>());

        // Subset filter within an inner range.
        let elev2 = m.matching_sequences_in_range(3, 9, |e| e == Some(2));
        assert_eq!(elev2, vec![8, 9]);

        // Empty when lower > upper after clamping.
        assert!(m.matching_sequences_in_range(9, 3, |_| true).is_empty());
    }

    #[wasm_bindgen_test]
    fn empty_vcp_has_zero_final_sequence() {
        let vcp = build_vcp(&[]);
        let m = ElevationChunkMapper::new(&vcp);
        // Only the Start chunk exists; no elevation ranges.
        assert_eq!(m.final_sequence(), 0);
        assert_eq!(m.total_chunks(), 1);
        assert_eq!(m.get_sequence_elevation_number(1), None);
        // next_matching scans 2..=0 (empty) → None regardless of accept_end.
        assert_eq!(m.next_matching_sequence_after(1, true, |_| true), None);
        // range scan clamps upper to final (0) → lower(1) > upper(0) → empty.
        assert!(m.matching_sequences_in_range(1, 5, |_| true).is_empty());
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::super::test_vcp::{build_vcp, TestElevation};
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// A single standard (3-chunk) elevation VCP.
    /// Layout: 1=Start, 2..=4 elev1; final=4, total=4.
    /// Elevation angle raw 1<<11 decodes to 11.25 deg; azimuth raw 1<<13 to 11.25 dps.
    fn mapper_1elev_standard() -> ElevationChunkMapper {
        let vcp = build_vcp(&[TestElevation {
            super_res: false,
            elevation_angle_raw: 1 << 11, // 11.25 deg
            azimuth_rate_raw: 1 << 13,    // 11.25 dps
            waveform_raw: 1,              // CS
            channel_raw: 0,
        }]);
        ElevationChunkMapper::new(&vcp)
    }

    /// Re-built 2-elev mix (elev1 super-res CS, elev2 standard CS) for tests
    /// that need real VCP-derived metadata fields. Same layout as the sibling
    /// module's `mapper_2elev`: 1=Start, 2..=7 elev1, 8..=10 elev2; final=10.
    fn mapper_2elev() -> ElevationChunkMapper {
        let vcp = build_vcp(&[
            TestElevation {
                super_res: true,
                elevation_angle_raw: 1 << 11, // 11.25 deg
                azimuth_rate_raw: 1 << 14,    // 22.5 dps
                waveform_raw: 1,              // CS
                channel_raw: 0,
            },
            TestElevation::standard_cs(0, 1 << 13), // 0 deg, 11.25 dps
        ]);
        ElevationChunkMapper::new(&vcp)
    }

    // ── ChunkMetadata::for_test constructor + getter coverage ────────────────

    #[wasm_bindgen_test]
    fn for_test_derives_is_last_and_is_start() {
        // chunk_index_in_sweep + 1 == chunks_in_sweep → is_last_in_sweep true.
        let last = ChunkMetadata::for_test(8, Some(2), 2, 3, false, 22.5);
        assert!(last.is_last_in_sweep());
        assert!(!last.is_first_in_sweep());
        assert!(!last.is_start_chunk()); // sequence != 1

        // Not the last index → is_last_in_sweep false.
        let mid = ChunkMetadata::for_test(7, Some(2), 1, 3, false, 22.5);
        assert!(!mid.is_last_in_sweep());

        // sequence == 1 → is_start_chunk true (derived in for_test).
        let start = ChunkMetadata::for_test(1, None, 0, 1, false, 0.0);
        assert!(start.is_start_chunk());
        // index 0, chunks_in_sweep 1 → 0+1==1 → is_last_in_sweep also true.
        assert!(start.is_last_in_sweep());
    }

    #[wasm_bindgen_test]
    fn for_test_getters_round_trip_inputs() {
        let md = ChunkMetadata::for_test(5, Some(3), 2, 6, true, -33.75);
        assert_eq!(md.sequence(), 5);
        assert_eq!(md.elevation_number(), Some(3));
        assert_eq!(md.chunk_index_in_sweep(), 2);
        assert_eq!(md.chunks_in_sweep(), 6);
        assert!(md.is_first_in_sweep());
        assert!((md.azimuth_rate_dps() - (-33.75)).abs() < 1e-9);
        // for_test hard-codes these two fields.
        assert!((md.elevation_angle_deg() - 0.5).abs() < 1e-9);
        assert_eq!(md.waveform_type(), None);
    }

    #[wasm_bindgen_test]
    fn chunk_metadata_is_copy_and_eq() {
        let a = ChunkMetadata::for_test(2, Some(1), 0, 6, true, 22.5);
        let b = a; // Copy — `a` still usable below.
        assert_eq!(a, b);
        let c = ChunkMetadata::for_test(3, Some(1), 1, 6, false, 22.5);
        assert_ne!(a, c);
        // Debug renders something non-empty.
        assert!(!format!("{a:?}").is_empty());
    }

    // ── Real-VCP metadata field propagation ──────────────────────────────────

    #[wasm_bindgen_test]
    fn start_chunk_metadata_fields_are_zeroed() {
        let m = mapper_2elev();
        let start = m.get_chunk_metadata(1).unwrap();
        assert!(start.is_start_chunk());
        assert_eq!(start.elevation_number(), None);
        assert_eq!(start.chunk_index_in_sweep(), 0);
        assert_eq!(start.chunks_in_sweep(), 1);
        assert!(!start.is_last_in_sweep());
        assert!((start.azimuth_rate_dps() - 0.0).abs() < 1e-9);
        assert!((start.elevation_angle_deg() - 0.0).abs() < 1e-9);
        assert_eq!(start.waveform_type(), None);
    }

    #[wasm_bindgen_test]
    fn data_chunk_metadata_carries_vcp_fields() {
        let m = mapper_2elev();
        // Elevation 1: super-res, 11.25 deg angle, 22.5 dps, CS waveform.
        let e1 = m.get_chunk_metadata(2).unwrap();
        assert_eq!(e1.elevation_number(), Some(1));
        assert!((e1.elevation_angle_deg() - 11.25).abs() < 1e-9);
        assert!((e1.azimuth_rate_dps() - 22.5).abs() < 1e-9);
        assert_eq!(e1.waveform_type(), Some(WaveformType::CS));

        // Elevation 2: standard, 0 deg angle, 11.25 dps, CS waveform.
        let e2 = m.get_chunk_metadata(8).unwrap();
        assert_eq!(e2.elevation_number(), Some(2));
        assert!((e2.elevation_angle_deg() - 0.0).abs() < 1e-9);
        assert!((e2.azimuth_rate_dps() - 11.25).abs() < 1e-9);
        assert_eq!(e2.waveform_type(), Some(WaveformType::CS));
    }

    #[wasm_bindgen_test]
    fn all_chunk_metadata_matches_total_and_indexing() {
        let m = mapper_2elev();
        let all = m.all_chunk_metadata();
        assert_eq!(all.len(), m.total_chunks());
        assert_eq!(all.len(), 10);
        // Index 0 is the Start chunk (sequence 1).
        assert!(all[0].is_start_chunk());
        assert_eq!(all[0].sequence(), 1);
        // Sequence is index + 1 throughout, and `get_chunk_metadata` agrees.
        for (i, md) in all.iter().enumerate() {
            assert_eq!(md.sequence(), i + 1);
            assert_eq!(m.get_chunk_metadata(i + 1).unwrap(), md);
        }
    }

    // ── Single-elevation standard layout ─────────────────────────────────────

    #[wasm_bindgen_test]
    fn single_standard_elevation_layout() {
        let m = mapper_1elev_standard();
        // Start chunk + 3 standard data chunks.
        assert_eq!(m.total_chunks(), 4);
        assert_eq!(m.final_sequence(), 4);
        assert_eq!(m.get_sequence_elevation_number(1), None); // start chunk
                                                              // seqs 2..=4 all belong to elevation 1.
        for seq in 2..=4 {
            assert_eq!(m.get_sequence_elevation_number(seq), Some(1));
            let md = m.get_chunk_metadata(seq).unwrap();
            assert_eq!(md.chunks_in_sweep(), 3);
        }
        assert!(m.get_chunk_metadata(2).unwrap().is_first_in_sweep());
        assert!(m.get_chunk_metadata(4).unwrap().is_last_in_sweep());
        assert!(!m.get_chunk_metadata(4).unwrap().is_first_in_sweep());
        // Past the final sequence → no elevation.
        assert_eq!(m.get_sequence_elevation_number(5), None);
    }

    // ── next_matching_sequence_after additional branches ─────────────────────

    #[wasm_bindgen_test]
    fn next_matching_after_at_or_past_final_returns_none() {
        let m = mapper_2elev(); // final = 10
                                // Starting exactly at final: range 11..=10 is empty → None.
        assert_eq!(m.next_matching_sequence_after(10, true, |_| true), None);
        // Starting past final → None.
        assert_eq!(m.next_matching_sequence_after(20, true, |_| true), None);
        // accept_end only fires when the candidate IS final; here the loop never
        // reaches final because start > final.
        assert_eq!(m.next_matching_sequence_after(10, true, |_| false), None);
    }

    #[wasm_bindgen_test]
    fn next_matching_after_predicate_wins_over_accept_end() {
        let m = mapper_2elev();
        // From seq 8, the next elev-2 match is 9 — returned before reaching the
        // accept_end branch at final (10).
        let elev2 = |e: Option<usize>| e == Some(2);
        assert_eq!(m.next_matching_sequence_after(8, true, elev2), Some(9));
        // From seq 9, the predicate matches 10 directly (it's elev 2), so the
        // accept_end branch is not what returns it — but the value is still 10.
        assert_eq!(m.next_matching_sequence_after(9, true, elev2), Some(10));
    }

    // ── matching_sequences_in_range / has_remaining_match extra branches ──────

    #[wasm_bindgen_test]
    fn matching_range_lower_clamps_to_one_and_filters_nonexistent() {
        let m = mapper_2elev();
        // lower < 1 clamps up to 1; upper exactly at final is inclusive.
        let r = m.matching_sequences_in_range(0, 10, |e| e == Some(1));
        assert_eq!(r, (2..=7).collect::<Vec<_>>());
        // An elevation that does not exist in this VCP yields no sequences.
        assert!(m
            .matching_sequences_in_range(1, 10, |e| e == Some(99))
            .is_empty());
        // upper clamps down to final (10): asking for 50 still stops at 10.
        let tail = m.matching_sequences_in_range(8, 50, |_| true);
        assert_eq!(tail, vec![8, 9, 10]);
    }

    #[wasm_bindgen_test]
    fn has_remaining_match_finds_existing_elevation_chunks() {
        let m = mapper_2elev(); // elevations 1 and 2 exist
                                // Elevation 1 has matches while before its last chunk (seq 7)...
        assert!(m.has_remaining_match(StreamingFilter::Elevation(1), 1));
        // ...and querying after_seq=6 still finds seq 7.
        assert!(m.has_remaining_match(StreamingFilter::Elevation(1), 6));
        // Note: a filter for a nonexistent elevation still reports remaining
        // matches because non-elevation chunks (elev == None, e.g. Start/End)
        // are accepted by every elevation filter — see StreamingFilter::accepts.
    }
}
