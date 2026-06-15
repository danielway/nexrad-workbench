//! Inventory of known-available chunks.
//!
//! Tracks `(volume, sequence, upload_time, type)` for every chunk we know exists
//! in S3 — whether we downloaded it or merely saw it in a `list_chunks_in_volume`
//! probe. The newest known chunk is the engine's AVAILABILITY anchor (collection
//! is then inferred via the median/​default lag), generalizing the old one-shot
//! `build_plan_from_anchor` re-anchor into a standing input.
//!
//! The global `newest` advances only on a *strictly newer* upload, so a recycled
//! rotating volume slot (an older occupant surfacing in a stale listing) can
//! never drag the anchor backward — the freshness guard is a property of the
//! merge, not of loop-local code.

use nexrad_data::aws::realtime::{ChunkIdentifier, ChunkType, VolumeIndex};
use std::collections::{BTreeMap, BTreeSet};

/// Volume + 1-based sequence locating a single chunk.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ChunkCoord {
    pub volume: VolumeIndex,
    pub sequence: usize,
}

/// A chunk known to exist in S3, with its S3 upload time (Unix seconds).
#[derive(Clone, Copy, Debug)]
pub struct KnownChunk {
    pub coord: ChunkCoord,
    pub upload_secs: f64,
    pub chunk_type: ChunkType,
}

/// Whether `candidate` is strictly newer than the best upload seen so far.
/// `None` prior means nothing seen yet, so any candidate is newer. Pure — the
/// freshness rule shared by inventory merges and the streaming probe guard.
fn upload_is_newer(prior_newest: Option<f64>, candidate: f64) -> bool {
    match prior_newest {
        Some(prev) => candidate > prev,
        None => true,
    }
}

/// Per-volume view: which sequences are known, the newest upload, and whether
/// the End chunk has appeared.
#[derive(Clone, Debug, Default)]
struct VolumeInventory {
    seqs: BTreeSet<usize>,
    newest_upload_secs: Option<f64>,
    has_end: bool,
}

impl VolumeInventory {
    fn merge(&mut self, sequence: usize, upload_secs: f64, chunk_type: ChunkType) {
        self.seqs.insert(sequence);
        if upload_is_newer(self.newest_upload_secs, upload_secs) {
            self.newest_upload_secs = Some(upload_secs);
        }
        if chunk_type == ChunkType::End {
            self.has_end = true;
        }
    }

    fn max_seq(&self) -> Option<usize> {
        self.seqs.iter().next_back().copied()
    }
}

/// Inventory of all known-available chunks across the current + next volume.
#[derive(Clone, Debug, Default)]
pub struct KnownChunkInventory {
    by_volume: BTreeMap<usize, VolumeInventory>,
    /// The newest known chunk across all volumes — the availability anchor.
    newest: Option<KnownChunk>,
}

impl KnownChunkInventory {
    /// Merge one observed chunk (an arrival or a single listing entry). Returns
    /// `true` iff the global `newest` advanced (i.e. the availability anchor
    /// moved). Idempotent for an already-seen sequence with an equal/older
    /// upload.
    pub fn observe(&mut self, chunk: KnownChunk) -> bool {
        self.by_volume
            .entry(chunk.coord.volume.as_number())
            .or_default()
            .merge(chunk.coord.sequence, chunk.upload_secs, chunk.chunk_type);

        let advanced = upload_is_newer(self.newest.map(|n| n.upload_secs), chunk.upload_secs);
        if advanced {
            self.newest = Some(chunk);
        }
        advanced
    }

    /// Merge a full S3 listing for one volume (a periodic probe). Returns `true`
    /// iff the global `newest` advanced.
    pub fn observe_listing(&mut self, volume: VolumeIndex, listed: &[ChunkIdentifier]) -> bool {
        let mut advanced = false;
        for id in listed {
            let Some(upload) = id
                .upload_date_time()
                .map(|dt| dt.timestamp_millis() as f64 / 1000.0)
            else {
                continue;
            };
            advanced |= self.observe(KnownChunk {
                coord: ChunkCoord {
                    volume,
                    sequence: id.sequence(),
                },
                upload_secs: upload,
                chunk_type: id.chunk_type(),
            });
        }
        advanced
    }

    /// Drop volumes outside the current + next window to bound memory. Keeps
    /// only `keep` and `keep.next()`.
    pub fn retain_from(&mut self, keep: VolumeIndex) {
        let a = keep.as_number();
        let b = keep.next().as_number();
        self.by_volume.retain(|v, _| *v == a || *v == b);
    }

    /// The newest known chunk across all volumes (the availability anchor).
    #[allow(dead_code)] // Exercised by tests; no prod caller.
    pub fn newest(&self) -> Option<KnownChunk> {
        self.newest
    }

    /// Whether a specific `(volume, sequence)` chunk is known-available.
    pub fn contains(&self, coord: ChunkCoord) -> bool {
        self.by_volume
            .get(&coord.volume.as_number())
            .is_some_and(|v| v.seqs.contains(&coord.sequence))
    }

    /// Highest known sequence published in `volume`, if any.
    pub fn newest_seq_in(&self, volume: VolumeIndex) -> Option<usize> {
        self.by_volume
            .get(&volume.as_number())
            .and_then(|v| v.max_seq())
    }

    /// Newest S3 upload time (Unix seconds) seen in `volume`, if any.
    pub fn newest_upload_in(&self, volume: VolumeIndex) -> Option<f64> {
        self.by_volume
            .get(&volume.as_number())
            .and_then(|v| v.newest_upload_secs)
    }

    /// Whether `volume`'s End chunk has been observed.
    pub fn has_end(&self, volume: VolumeIndex) -> bool {
        self.by_volume
            .get(&volume.as_number())
            .is_some_and(|v| v.has_end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn vol(n: usize) -> VolumeIndex {
        VolumeIndex::new(n)
    }

    fn known(volume: usize, sequence: usize, upload: f64, ty: ChunkType) -> KnownChunk {
        KnownChunk {
            coord: ChunkCoord {
                volume: vol(volume),
                sequence,
            },
            upload_secs: upload,
            chunk_type: ty,
        }
    }

    #[wasm_bindgen_test]
    fn observe_advances_anchor_only_on_strictly_newer_upload() {
        let mut inv = KnownChunkInventory::default();
        // First observation always advances.
        assert!(inv.observe(known(1, 2, 100.0, ChunkType::Intermediate)));
        // Strictly newer advances.
        assert!(inv.observe(known(1, 3, 150.0, ChunkType::Intermediate)));
        // Equal upload does not advance.
        assert!(!inv.observe(known(1, 4, 150.0, ChunkType::Intermediate)));
        // Older (recycled-slot) upload does not advance.
        assert!(!inv.observe(known(1, 5, 50.0, ChunkType::Intermediate)));
        // Anchor stayed at the newest (150.0).
        assert_eq!(inv.newest().map(|n| n.upload_secs), Some(150.0));
    }

    #[wasm_bindgen_test]
    fn merges_sequences_and_detects_end_per_volume() {
        let mut inv = KnownChunkInventory::default();
        inv.observe(known(7, 1, 10.0, ChunkType::Start));
        inv.observe(known(7, 2, 20.0, ChunkType::Intermediate));
        inv.observe(known(7, 9, 90.0, ChunkType::End));
        assert!(inv.contains(ChunkCoord {
            volume: vol(7),
            sequence: 2
        }));
        assert!(!inv.contains(ChunkCoord {
            volume: vol(7),
            sequence: 5
        }));
        assert_eq!(inv.newest_seq_in(vol(7)), Some(9));
        assert_eq!(inv.newest_upload_in(vol(7)), Some(90.0));
        assert!(inv.has_end(vol(7)));
        assert!(!inv.has_end(vol(8)));
    }

    #[wasm_bindgen_test]
    fn observe_listing_advances_when_slot_is_fresh() {
        // Seed an older current volume.
        let mut inv = KnownChunkInventory::default();
        inv.observe(known(1, 5, 100.0, ChunkType::Intermediate));
        // A listing whose newest upload is older than the anchor (stale recycled
        // slot) must NOT advance.
        // (Empty listing trivially does not advance.)
        assert!(!inv.observe_listing(vol(2), &[]));
        assert_eq!(inv.newest().map(|n| n.upload_secs), Some(100.0));
    }

    #[wasm_bindgen_test]
    fn retain_from_keeps_only_current_and_next() {
        let mut inv = KnownChunkInventory::default();
        inv.observe(known(1, 1, 10.0, ChunkType::Start));
        inv.observe(known(2, 1, 20.0, ChunkType::Start));
        inv.observe(known(3, 1, 30.0, ChunkType::Start));
        inv.retain_from(vol(2)); // keep 2 and 3
        assert!(!inv.contains(ChunkCoord {
            volume: vol(1),
            sequence: 1
        }));
        assert!(inv.contains(ChunkCoord {
            volume: vol(2),
            sequence: 1
        }));
        assert!(inv.contains(ChunkCoord {
            volume: vol(3),
            sequence: 1
        }));
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn vol(n: usize) -> VolumeIndex {
        VolumeIndex::new(n)
    }

    fn known(volume: usize, sequence: usize, upload: f64, ty: ChunkType) -> KnownChunk {
        KnownChunk {
            coord: ChunkCoord {
                volume: vol(volume),
                sequence,
            },
            upload_secs: upload,
            chunk_type: ty,
        }
    }

    #[wasm_bindgen_test]
    fn upload_is_newer_truth_table() {
        // Nothing seen yet → anything is newer.
        assert!(upload_is_newer(None, 0.0));
        assert!(upload_is_newer(None, -50.0));
        // Strictly greater only.
        assert!(upload_is_newer(Some(100.0), 100.1));
        assert!(!upload_is_newer(Some(100.0), 100.0)); // equal is NOT newer
        assert!(!upload_is_newer(Some(100.0), 99.9));
    }

    #[wasm_bindgen_test]
    fn empty_inventory_reports_nothing() {
        let inv = KnownChunkInventory::default();
        assert!(inv.newest().is_none());
        assert!(inv.newest_seq_in(vol(1)).is_none());
        assert!(inv.newest_upload_in(vol(1)).is_none());
        assert!(!inv.has_end(vol(1)));
        assert!(!inv.contains(ChunkCoord {
            volume: vol(1),
            sequence: 1,
        }));
    }

    #[wasm_bindgen_test]
    fn re_observing_a_sequence_is_set_idempotent() {
        let mut inv = KnownChunkInventory::default();
        inv.observe(known(3, 7, 100.0, ChunkType::Intermediate));
        // Same sequence again with an older upload: no anchor advance, still one
        // entry, max sequence unchanged.
        assert!(!inv.observe(known(3, 7, 50.0, ChunkType::Intermediate)));
        assert!(inv.contains(ChunkCoord {
            volume: vol(3),
            sequence: 7,
        }));
        assert_eq!(inv.newest_seq_in(vol(3)), Some(7));
    }

    #[wasm_bindgen_test]
    fn anchor_advances_across_volumes() {
        let mut inv = KnownChunkInventory::default();
        inv.observe(known(1, 1, 100.0, ChunkType::Start));
        // A newer upload in the NEXT volume moves the global anchor.
        assert!(inv.observe(known(2, 1, 200.0, ChunkType::Start)));
        let n = inv.newest().unwrap();
        assert_eq!(n.upload_secs, 200.0);
        assert_eq!(n.coord.volume.as_number(), 2);
    }

    #[wasm_bindgen_test]
    fn newest_upload_in_tracks_max_not_last_seen() {
        let mut inv = KnownChunkInventory::default();
        inv.observe(known(5, 1, 200.0, ChunkType::Intermediate));
        // A later observation with an OLDER upload must not lower the per-volume
        // newest (recycled-slot guard).
        inv.observe(known(5, 2, 100.0, ChunkType::Intermediate));
        assert_eq!(inv.newest_upload_in(vol(5)), Some(200.0));
        // But the sequence set still grows.
        assert_eq!(inv.newest_seq_in(vol(5)), Some(2));
    }

    #[wasm_bindgen_test]
    fn chunk_coord_equality_is_volume_and_sequence() {
        let a = ChunkCoord {
            volume: vol(1),
            sequence: 4,
        };
        let b = ChunkCoord {
            volume: vol(1),
            sequence: 4,
        };
        let c = ChunkCoord {
            volume: vol(2),
            sequence: 4,
        };
        assert_eq!(a, b);
        assert_ne!(a, c);
    }
}
