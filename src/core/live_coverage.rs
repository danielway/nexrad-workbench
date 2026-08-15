//! Pure coverage accounting for a live volume's sweep chunks.

use std::collections::{BTreeSet, HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SweepChunk {
    pub elevation: u8,
    pub sequence: u32,
    pub index_in_sweep: u8,
    pub chunks_in_sweep: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChunkDecision {
    Duplicate,
    Partial,
    ReadyToCommit(u8),
}

#[derive(Debug, Default)]
pub(crate) struct LiveCoverage {
    received: HashMap<u8, BTreeSet<u32>>,
    expected: HashMap<u8, BTreeSet<u32>>,
    durable: HashSet<u8>,
}

impl LiveCoverage {
    pub(crate) fn accept(&mut self, chunk: SweepChunk) -> ChunkDecision {
        if self.durable.contains(&chunk.elevation) {
            return ChunkDecision::Duplicate;
        }
        let first = chunk
            .sequence
            .saturating_sub(u32::from(chunk.index_in_sweep));
        let expected = self.expected.entry(chunk.elevation).or_default();
        expected.extend(first..first + u32::from(chunk.chunks_in_sweep));
        let received = self.received.entry(chunk.elevation).or_default();
        if !received.insert(chunk.sequence) {
            return ChunkDecision::Duplicate;
        }
        if received == expected {
            ChunkDecision::ReadyToCommit(chunk.elevation)
        } else {
            ChunkDecision::Partial
        }
    }

    pub(crate) fn commit_succeeded(&mut self, elevation: u8) {
        self.durable.insert(elevation);
    }

    #[cfg(test)]
    pub(crate) fn expected_sequences(&self, elevation: u8) -> Vec<u32> {
        self.expected
            .get(&elevation)
            .map(|sequences| sequences.iter().copied().collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(sequence: u32, index: u8) -> SweepChunk {
        SweepChunk {
            elevation: 2,
            sequence,
            index_in_sweep: index,
            chunks_in_sweep: 3,
        }
    }

    #[test]
    fn gap_prevents_terminal_completion_until_filled() {
        let mut coverage = LiveCoverage::default();
        assert_eq!(coverage.accept(chunk(5, 0)), ChunkDecision::Partial);
        assert_eq!(coverage.accept(chunk(7, 2)), ChunkDecision::Partial);
        assert_eq!(
            coverage.accept(chunk(6, 1)),
            ChunkDecision::ReadyToCommit(2)
        );
    }

    #[test]
    fn duplicate_is_idempotent_and_commit_is_acknowledged() {
        let mut coverage = LiveCoverage::default();
        assert_eq!(coverage.accept(chunk(5, 0)), ChunkDecision::Partial);
        assert_eq!(coverage.accept(chunk(5, 0)), ChunkDecision::Duplicate);
        assert_eq!(coverage.accept(chunk(6, 1)), ChunkDecision::Partial);
        assert_eq!(
            coverage.accept(chunk(7, 2)),
            ChunkDecision::ReadyToCommit(2)
        );
        coverage.commit_succeeded(2);
        assert_eq!(coverage.accept(chunk(7, 2)), ChunkDecision::Duplicate);
    }

    #[test]
    fn elevations_have_independent_coverage() {
        let mut coverage = LiveCoverage::default();
        assert_eq!(coverage.accept(chunk(5, 0)), ChunkDecision::Partial);
        assert_eq!(
            coverage.accept(SweepChunk {
                elevation: 1,
                sequence: 2,
                index_in_sweep: 0,
                chunks_in_sweep: 3
            }),
            ChunkDecision::Partial
        );
        assert_eq!(coverage.expected_sequences(2), vec![5, 6, 7]);
    }
}
