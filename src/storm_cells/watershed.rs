//! Cell footprints: share each envelope among its cells with a marker-based
//! watershed.
//!
//! Segmentation finds a cell as the component at the level where it became
//! distinct from its neighbours (45-55 dBZ in a squall line): the core, tight
//! and jagged. What a reader wants drawn is the storm's share of the
//! envelope, the echo at or above the user's threshold that belongs to it.
//!
//! Each cell's core is a seed (a basin). The basins grow outward over the
//! envelope's gates in descending order of their *path minimum*: the lowest
//! filtered value on the best path from the basin to the gate. A gate goes
//! to the first basin that reaches it, so every envelope gate ends up with
//! the core it is joined to by the strongest echo, and two cores in one
//! envelope split along the weakest line between them (the saddle). Gates
//! are 8-connected with azimuth wrap where radials are adjacent, the same
//! rule as the labeling, so a basin is one connected piece of its envelope.
//!
//! The queue is a bucket queue on [`BUCKET_DB`] steps above the envelope
//! level, first-in first-out within a bucket, so a flat stretch between two
//! cores is shared evenly and the run is linear in the envelope's size.

use std::collections::VecDeque;

use super::components::{for_each_neighbour, Pixel};

/// Resolution of the flooding order. NEXRAD reflectivity comes in 0.5 dB
/// steps, so finer buckets would order nothing more.
const BUCKET_DB: f32 = 0.5;

/// Basin label of a gate not (yet) claimed.
const UNCLAIMED: u32 = u32::MAX;

/// Reusable scratch for [`Watershed::partition`]: one label per grid gate.
pub(super) struct Watershed<'a> {
    grid: &'a [f32],
    az_adjacent: &'a [bool],
    gate_count: usize,
    label: Vec<u32>,
    buckets: Vec<VecDeque<usize>>,
}

impl<'a> Watershed<'a> {
    pub(super) fn new(grid: &'a [f32], az_adjacent: &'a [bool], gate_count: usize) -> Self {
        debug_assert_eq!(grid.len(), az_adjacent.len() * gate_count);
        Self {
            grid,
            az_adjacent,
            gate_count,
            label: vec![UNCLAIMED; grid.len()],
            buckets: Vec::new(),
        }
    }

    /// Split `envelope` (a connected component of gates `>= floor`) among
    /// `seeds` (disjoint connected components inside it, one per cell).
    /// Returns one pixel list per seed, in seed order; together they are
    /// exactly the envelope's gates. A single seed gets the whole envelope.
    pub(super) fn partition(
        &mut self,
        envelope: &[Pixel],
        seeds: &[&[Pixel]],
        floor: f32,
    ) -> Vec<Vec<Pixel>> {
        let gate_count = self.gate_count;
        let flat = |&(a, g): &Pixel| a as usize * gate_count + g as usize;
        if seeds.len() <= 1 {
            return seeds.iter().map(|_| envelope.to_vec()).collect();
        }

        let peak = envelope
            .iter()
            .map(|p| self.grid[flat(p)])
            .fold(floor, f32::max);
        let bucket_of = |v: f32| (((v - floor) / BUCKET_DB).max(0.0)) as usize;
        let top = bucket_of(peak);
        if self.buckets.len() <= top {
            self.buckets.resize_with(top + 1, VecDeque::new);
        }

        for (basin, seed) in seeds.iter().enumerate() {
            for p in seed.iter() {
                let i = flat(p);
                self.label[i] = basin as u32;
                self.buckets[bucket_of(self.grid[i])].push_back(i);
            }
        }

        // Priorities never rise (a gate's bucket is capped by the bucket it
        // was reached from), so one sweep from the top bucket down suffices.
        let (grid, label, buckets) = (self.grid, &mut self.label, &mut self.buckets);
        for b in (0..=top).rev() {
            while let Some(i) = buckets[b].pop_front() {
                let basin = label[i];
                for_each_neighbour(i, gate_count, self.az_adjacent, |n| {
                    // Below the floor (or NaN, which compares false) is
                    // outside the envelope.
                    let in_envelope = grid[n] >= floor;
                    if label[n] != UNCLAIMED || !in_envelope {
                        return;
                    }
                    label[n] = basin;
                    buckets[bucket_of(grid[n]).min(b)].push_back(n);
                });
            }
        }

        let mut out: Vec<Vec<Pixel>> = vec![Vec::new(); seeds.len()];
        for p in envelope {
            let i = flat(p);
            let basin = label[i];
            // Every envelope gate is connected to a seed, so it is claimed.
            debug_assert_ne!(basin, UNCLAIMED);
            if let Some(dst) = out.get_mut(basin as usize) {
                dst.push(*p);
            }
            label[i] = UNCLAIMED;
        }
        out
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    const N: f32 = f32::NAN;

    /// Grid of `rows` (one per radial), with every radial adjacent to the
    /// next (and the last to the first) when `wrap`.
    fn grid(rows: &[&[f32]], wrap: bool) -> (Vec<f32>, Vec<bool>, usize) {
        let gate_count = rows[0].len();
        let g = rows.iter().flat_map(|r| r.iter().copied()).collect();
        let mut adj = vec![true; rows.len()];
        if !wrap {
            *adj.last_mut().unwrap() = false;
        }
        (g, adj, gate_count)
    }

    fn at_or_above(g: &[f32], gate_count: usize, level: f32) -> Vec<Pixel> {
        (0..g.len())
            .filter(|&i| g[i] >= level)
            .map(|i| ((i / gate_count) as u16, (i % gate_count) as u16))
            .collect()
    }

    fn sorted(mut v: Vec<Pixel>) -> Vec<Pixel> {
        v.sort();
        v
    }

    #[wasm_bindgen_test]
    fn two_cores_split_along_the_saddle() {
        // Two 60 dBZ cores in one 35 dBZ envelope. The saddle (38) is nearer
        // the right core, so the left basin takes more of the bridge.
        let row: &[f32] = &[
            36.0, 45.0, 60.0, 45.0, 42.0, 40.0, 38.0, 44.0, 60.0, 44.0, 36.0,
        ];
        let (g, adj, gc) = grid(&[row, row, row], false);
        let env = at_or_above(&g, gc, 35.0);
        let left: Vec<Pixel> = (0..3).map(|a| (a, 2)).collect();
        let right: Vec<Pixel> = (0..3).map(|a| (a, 8)).collect();
        let mut ws = Watershed::new(&g, &adj, gc);
        let parts = ws.partition(&env, &[&left, &right], 35.0);

        let gates = |p: &[Pixel]| {
            let mut v: Vec<u16> = p.iter().map(|&(_, g)| g).collect();
            v.sort();
            v.dedup();
            v
        };
        assert_eq!(gates(&parts[0]), vec![0, 1, 2, 3, 4, 5]);
        assert_eq!(gates(&parts[1]), vec![6, 7, 8, 9, 10]);
        // A partition: every envelope gate exactly once.
        assert_eq!(parts[0].len() + parts[1].len(), env.len());
        let mut all = parts.concat();
        all.sort();
        assert_eq!(all, sorted(env));
    }

    #[wasm_bindgen_test]
    fn gate_goes_to_the_core_it_is_joined_to_by_stronger_echo() {
        // Gate (0, 2) is two steps from the left core but joined to it only
        // through 36; the right core reaches it in five steps through 50.
        let a: &[f32] = &[60.0, 36.0, 40.0, 36.0, 36.0, 36.0, 60.0];
        let b: &[f32] = &[N, N, 50.0, 50.0, 50.0, 50.0, 50.0];
        let (g, adj, gc) = grid(&[a, b], false);
        let env = at_or_above(&g, gc, 35.0);
        let mut ws = Watershed::new(&g, &adj, gc);
        let parts = ws.partition(&env, &[&[(0, 0)], &[(0, 6)]], 35.0);
        assert_eq!(sorted(parts[0].clone()), vec![(0, 0), (0, 1)]);
        assert!(parts[1].contains(&(0, 2)));
    }

    #[wasm_bindgen_test]
    fn lone_seed_gets_the_whole_envelope() {
        let row: &[f32] = &[36.0, 45.0, 60.0, 45.0, 36.0];
        let (g, adj, gc) = grid(&[row, row], false);
        let env = at_or_above(&g, gc, 35.0);
        let core = [(0u16, 2u16), (1, 2)];
        let mut ws = Watershed::new(&g, &adj, gc);
        let parts = ws.partition(&env, &[&core], 35.0);
        assert_eq!(parts, vec![env]);
    }

    #[wasm_bindgen_test]
    fn basins_flood_across_the_azimuth_seam() {
        // 8 radials: cores on radials 7 and 1, the saddle on radial 0, so
        // the envelope (radials 6, 7, 0, 1, 2) straddles the seam.
        let core: &[f32] = &[60.0, 60.0];
        let shoulder: &[f32] = &[45.0, 45.0];
        let saddle: &[f32] = &[40.0, 40.0];
        let gap: &[f32] = &[N, N];
        let rows = [saddle, core, shoulder, gap, gap, gap, shoulder, core];
        let (g, adj, gc) = grid(&rows, true);
        let env = at_or_above(&g, gc, 35.0);
        let radials = |p: &[Pixel]| {
            let mut v: Vec<u16> = p.iter().map(|&(a, _)| a).collect();
            v.sort();
            v.dedup();
            v
        };
        let west: Vec<Pixel> = vec![(7, 0), (7, 1)];
        let east: Vec<Pixel> = vec![(1, 0), (1, 1)];
        let mut ws = Watershed::new(&g, &adj, gc);
        let parts = ws.partition(&env, &[&west, &east], 35.0);
        // The west core reaches the saddle across the seam (it is seeded
        // first, so it wins the tie).
        assert_eq!(radials(&parts[0]), vec![0, 6, 7]);
        assert_eq!(radials(&parts[1]), vec![1, 2]);

        // Cut the seam: radial 0 is then reachable only from the east.
        let mut cut = adj.clone();
        cut[7] = false;
        let mut ws = Watershed::new(&g, &cut, gc);
        let parts = ws.partition(&env, &[&west, &east], 35.0);
        assert_eq!(radials(&parts[0]), vec![6, 7]);
        assert_eq!(radials(&parts[1]), vec![0, 1, 2]);
    }

    #[wasm_bindgen_test]
    fn scratch_labels_are_reset_between_envelopes() {
        let row: &[f32] = &[50.0, 40.0, 50.0, N, 50.0, 40.0, 50.0];
        let (g, adj, gc) = grid(&[row], false);
        let mut ws = Watershed::new(&g, &adj, gc);
        let first = ws.partition(&[(0, 0), (0, 1), (0, 2)], &[&[(0, 0)], &[(0, 2)]], 35.0);
        let second = ws.partition(&[(0, 4), (0, 5), (0, 6)], &[&[(0, 4)], &[(0, 6)]], 35.0);
        assert_eq!(first[0].len() + first[1].len(), 3);
        assert_eq!(second[0].len() + second[1].len(), 3);
        assert!(ws.label.iter().all(|l| *l == UNCLAIMED));
    }
}
