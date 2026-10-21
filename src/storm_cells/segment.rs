//! Nested-threshold segmentation (the SCIT idea).
//!
//! One threshold cannot separate storms: at a low level a whole line or rain
//! shield is one connected blob, at a high level every weak storm vanishes.
//! Instead, components are labeled at a ladder of levels `L0, L0+5, …` and
//! arranged in the containment tree (a component at level k+1 lies inside
//! exactly one component at level k). A component that contains two or more
//! *qualifying* components one rung up is a group and those children are the
//! candidates; one that contains one or none is a cell.
//!
//! A child qualifies when it is big enough (area and gate floors, area being
//! range-scaled because it is summed from gate footprints) and *prominent*:
//! its peak stands at least `prominence_dbz` (3 dB) above its own level.
//! The prominence rule is what drops stratiform local maxima, which barely
//! poke above the next level.

use super::components::{Labeler, Pixel};
use super::DetectionParams;

/// One connected component at one ladder level.
#[derive(Debug)]
pub(super) struct Component {
    /// Index into the ladder (0 = envelope).
    pub level_idx: usize,
    pub pixels: Vec<Pixel>,
    /// Maximum filtered value inside.
    pub peak: f32,
    /// Qualifying components one level up that lie inside this one.
    pub children: Vec<usize>,
}

/// The containment forest plus what it resolved to.
#[derive(Debug)]
pub(super) struct Segmentation {
    /// Threshold of each ladder level, ascending.
    pub levels: Vec<f32>,
    pub components: Vec<Component>,
    /// Envelope components that survived the size floors, with the
    /// components each resolved to (cells). An entry with two or more cells
    /// is a group.
    pub envelopes: Vec<(usize, Vec<usize>)>,
}

/// Ladder of levels: `envelope, envelope + step, …` up to `top` inclusive.
/// Always contains at least the envelope level.
pub(super) fn ladder(params: &DetectionParams) -> Vec<f32> {
    let step = params.level_step_dbz.max(0.5);
    let mut levels = vec![params.threshold_dbz];
    let mut next = params.threshold_dbz + step;
    while next <= params.top_level_dbz + 1e-3 && levels.len() < 32 {
        levels.push(next);
        next += step;
    }
    levels
}

/// Label every ladder level over `filtered` and resolve the tree into cells.
///
/// `gate_area_km2[g]` is the ground footprint of one gate at gate index `g`.
pub(super) fn segment(
    filtered: &[f32],
    az_adjacent: &[bool],
    gate_count: usize,
    gate_area_km2: &[f64],
    params: &DetectionParams,
) -> Segmentation {
    let levels = ladder(params);
    let mut labeler = Labeler::new(filtered, az_adjacent, gate_count);
    let mut components: Vec<Component> = Vec::new();

    let measure = |pixels: &[Pixel]| -> (f32, f64) {
        let mut peak = f32::NEG_INFINITY;
        let mut area = 0.0;
        for &(a, g) in pixels {
            let v = filtered[a as usize * gate_count + g as usize];
            if v > peak {
                peak = v;
            }
            area += gate_area_km2[g as usize];
        }
        (peak, area)
    };
    let big_enough = |pixels: &[Pixel], area: f64| {
        pixels.len() as u32 >= params.min_gate_count && area >= params.min_area_km2 as f64
    };

    // Level 0: the envelope, a full scan.
    let mut live: Vec<usize> = Vec::new();
    for seed in 0..filtered.len() {
        if let Some(pixels) = labeler.fill(seed, levels[0], 1) {
            let (peak, area_km2) = measure(&pixels);
            if !big_enough(&pixels, area_km2) {
                continue;
            }
            live.push(components.len());
            components.push(Component {
                level_idx: 0,
                pixels,
                peak,
                children: Vec::new(),
            });
        }
    }
    let envelope_ids = live.clone();

    // Higher levels: only descend into live (envelope or qualifying)
    // components, seeding from their own gates, so containment is known by
    // construction and each level touches fewer gates than the last.
    for (k, &level) in levels.iter().enumerate().skip(1) {
        let stamp = (k + 1) as u8;
        let mut next_live = Vec::new();
        for &parent in &live {
            let mut found = Vec::new();
            // Index loop: `labeler` is borrowed mutably inside.
            for p in 0..components[parent].pixels.len() {
                let (a, g) = components[parent].pixels[p];
                let seed = a as usize * gate_count + g as usize;
                let Some(pixels) = labeler.fill(seed, level, stamp) else {
                    continue;
                };
                let (peak, area_km2) = measure(&pixels);
                if !big_enough(&pixels, area_km2) || peak < level + params.prominence_dbz {
                    continue;
                }
                found.push(Component {
                    level_idx: k,
                    pixels,
                    peak,
                    children: Vec::new(),
                });
            }
            for c in found {
                let id = components.len();
                components.push(c);
                components[parent].children.push(id);
                next_live.push(id);
            }
        }
        if next_live.is_empty() {
            break;
        }
        live = next_live;
    }

    let core_min = params.threshold_dbz + params.core_margin_dbz;
    let envelopes = envelope_ids
        .into_iter()
        .map(|e| (e, resolve(&components, e, core_min)))
        .filter(|(_, cells)| !cells.is_empty())
        .collect();

    Segmentation {
        levels,
        components,
        envelopes,
    }
}

/// Resolve component `i` into the components that are cells.
///
/// - two or more qualifying children: a group; the cells are the union of
///   resolving each child.
/// - exactly one: resolve the child; if that yields a single cell, the chain
///   is single all the way down and `i` itself is the cell (a lone storm's
///   outline is its widest component); otherwise pass the child's cells up.
/// - none: `i` is a cell when its peak reaches the core minimum.
fn resolve(components: &[Component], i: usize, core_min: f32) -> Vec<usize> {
    let c = &components[i];
    let leaf = || {
        if c.peak >= core_min {
            vec![i]
        } else {
            Vec::new()
        }
    };
    match c.children.as_slice() {
        [] => leaf(),
        [only] => {
            let below = resolve(components, *only, core_min);
            if below.len() <= 1 {
                if below.is_empty() {
                    leaf()
                } else {
                    vec![i]
                }
            } else {
                below
            }
        }
        many => many
            .iter()
            .flat_map(|&child| resolve(components, child, core_min))
            .collect(),
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    const N: f32 = f32::NAN;

    fn params() -> DetectionParams {
        DetectionParams {
            min_area_km2: 0.0,
            min_gate_count: 1,
            ..DetectionParams::default()
        }
    }

    /// Run `segment` on rows (one row per radial) with unit gate area and
    /// no azimuth wrap.
    fn run(rows: &[&[f32]], p: &DetectionParams) -> Segmentation {
        let gate_count = rows[0].len();
        let grid: Vec<f32> = rows.iter().flat_map(|r| r.iter().copied()).collect();
        let adj = vec![false; rows.len()];
        let area = vec![1.0; gate_count];
        segment(&grid, &adj, gate_count, &area, p)
    }

    fn cell_levels(s: &Segmentation) -> Vec<Vec<f32>> {
        s.envelopes
            .iter()
            .map(|(_, cells)| {
                cells
                    .iter()
                    .map(|&c| s.levels[s.components[c].level_idx])
                    .collect()
            })
            .collect()
    }

    #[wasm_bindgen_test]
    fn ladder_runs_from_envelope_to_top_in_five_db_steps() {
        let p = DetectionParams::default();
        assert_eq!(ladder(&p), vec![35.0, 40.0, 45.0, 50.0, 55.0, 60.0, 65.0]);
        let high = DetectionParams {
            threshold_dbz: 70.0,
            ..p
        };
        assert_eq!(ladder(&high), vec![70.0]);
    }

    #[wasm_bindgen_test]
    fn lone_storm_keeps_its_envelope_outline() {
        // One storm: 36 edge, 52 core (single chain 35→40→45→50).
        let s = run(&[&[36.0, 44.0, 52.0, 44.0, 36.0]], &params());
        assert_eq!(cell_levels(&s), vec![vec![35.0]]);
        let (env, cells) = &s.envelopes[0];
        assert_eq!(cells, &vec![*env]);
    }

    #[wasm_bindgen_test]
    fn two_cores_in_one_envelope_make_a_group() {
        // Two 58 dBZ cores joined by a 42 dBZ bridge: one envelope, split at 45.
        let s = run(
            &[&[38.0, 50.0, 58.0, 50.0, 42.0, 50.0, 58.0, 50.0, 38.0]],
            &params(),
        );
        assert_eq!(s.envelopes.len(), 1);
        assert_eq!(cell_levels(&s), vec![vec![45.0, 45.0]]);
    }

    #[wasm_bindgen_test]
    fn weak_echo_without_a_core_is_dropped() {
        // Peak 43 < envelope + 10 → no cell.
        let s = run(&[&[36.0, 43.0, 36.0]], &params());
        assert!(s.envelopes.is_empty());
    }

    #[wasm_bindgen_test]
    fn bump_without_prominence_does_not_split() {
        // A 47 bump next to a 60 core: at 45 both are separate, but the bump
        // peaks only 2 dB above 45 (< 3 dB prominence) so it does not
        // qualify → one lone cell.
        let s = run(&[&[38.0, 47.0, 42.0, 50.0, 60.0, 50.0, 38.0]], &params());
        assert_eq!(cell_levels(&s), vec![vec![35.0]]);
    }

    #[wasm_bindgen_test]
    fn bump_with_prominence_splits() {
        // Same shape, but the bump reaches 48 (3 dB above 45) → two cells.
        let s = run(&[&[38.0, 48.0, 42.0, 50.0, 60.0, 50.0, 38.0]], &params());
        assert_eq!(cell_levels(&s), vec![vec![45.0, 45.0]]);
    }

    #[wasm_bindgen_test]
    fn small_children_do_not_qualify() {
        // Two cores of one gate each with a floor of 2 gates → neither
        // qualifies; the envelope is a single cell.
        let p = DetectionParams {
            min_gate_count: 2,
            ..params()
        };
        let s = run(&[&[38.0, 58.0, 42.0, 58.0, 38.0]], &p);
        assert_eq!(cell_levels(&s), vec![vec![35.0]]);
    }

    #[wasm_bindgen_test]
    fn nested_split_reports_cells_at_the_level_they_separate() {
        // Envelope → at 45 two parts; the right part splits again at 55.
        let row: &[f32] = &[
            50.0, 62.0, 50.0, 40.0, 52.0, 64.0, 52.0, 58.0, 66.0, 58.0, 36.0,
        ];
        let s = run(&[row], &params());
        let mut levels = cell_levels(&s).remove(0);
        levels.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert_eq!(levels, vec![45.0, 55.0, 55.0]);
    }

    #[wasm_bindgen_test]
    fn background_is_never_labeled() {
        let s = run(&[&[N, N, N]], &params());
        assert!(s.components.is_empty());
    }
}
