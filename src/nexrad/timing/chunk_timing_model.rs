use super::ChunkMetadata;
use nexrad_decode::messages::volume_coverage_pattern::WaveformType;

/// Which case [`ChunkTimingModel::estimate_chunk_interval_secs`] selected.
///
/// Recorded on the per-chunk diagnostic so we can tell whether a prediction
/// error came from intra-sweep rotation rate, inter-sweep transition, or the
/// fixed inter-volume gap — three completely different fixes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntervalCase {
    /// Pure rotation rate: same elevation, no transition.
    IntraSweep,
    /// First chunk of a new sweep within the same volume — base gap +
    /// elevation slew + waveform penalty + chunk duration.
    InterSweep,
    /// First chunk of a new volume — fixed `INTER_VOLUME_GAP_SECS`.
    InterVolume,
}

impl IntervalCase {
    pub fn short(&self) -> &'static str {
        match self {
            IntervalCase::IntraSweep => "intra",
            IntervalCase::InterSweep => "inter_sweep",
            IntervalCase::InterVolume => "inter_volume",
        }
    }
}

/// Decomposition of a single physics-model interval prediction.
///
/// `total_secs` is what the legacy `estimate_chunk_interval_secs` returns;
/// the other fields expose the components that fed it so prediction error
/// can be attributed to a specific knob.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PhysicsBreakdown {
    pub case: IntervalCase,
    pub total_secs: f64,
    /// Per-chunk rotation duration (sweep_duration / chunks_in_sweep).
    /// `None` for InterVolume (no chunk-duration term applies) or when the
    /// azimuth rate wasn't available and a fallback was used.
    pub chunk_duration_secs: Option<f64>,
    /// Inter-sweep gap base (0.7s) + elevation slew. `None` outside InterSweep.
    pub inter_sweep_gap_secs: Option<f64>,
    /// Waveform-transition penalty applied within an inter-sweep gap. `None`
    /// outside InterSweep (or when no penalty applies).
    pub waveform_penalty_secs: Option<f64>,
}

/// Sweep duration bias correction in seconds.
///
/// Sweeps consistently finish ~0.67s before a full 360-degree rotation would predict,
/// because the last radial at ~359.5 degrees means the sweep is slightly short of a
/// full circle. Derived from analysis of 801 sweep observations across 59 volumes.
const SWEEP_DURATION_BIAS_SECS: f64 = 0.67;

/// Base overhead for inter-sweep transitions in seconds.
///
/// This represents the minimum time for mode switching between sweeps when the
/// antenna doesn't need to change elevation (e.g., CS to CDW at same angle).
const INTER_SWEEP_BASE_GAP_SECS: f64 = 0.7;

/// Seconds per degree of elevation change during inter-sweep transitions.
///
/// Represents the antenna slew rate during transitions between sweeps at different
/// elevation angles. Combined with the base gap: gap = 0.7 + (|delta_elev| * 0.08).
const INTER_SWEEP_ELEVATION_RATE_SECS_PER_DEG: f64 = 0.08;

/// Inter-volume gap in seconds.
///
/// Time between the last radial of one volume and the first radial of the next.
/// Includes antenna return to starting elevation plus initialization overhead.
/// Derived from analysis: mean ~8.5s, range 7-10s.
const INTER_VOLUME_GAP_SECS: f64 = 8.5;

/// Gap in seconds between the Start chunk's upload and the first intermediate
/// chunk's upload, within the same volume.
///
/// The Start chunk is metadata-only and is published almost immediately; the first
/// intermediate chunk lags by ~1–2s in practice (observed across three VCP 212
/// volumes). This is distinct from `INTER_VOLUME_GAP_SECS`, which measures the gap
/// between the *End* of one volume and the *Start* of the next.
const START_TO_FIRST_INTERMEDIATE_GAP_SECS: f64 = 1.5;

/// Physics-based timing model for predicting chunk and sweep timing from VCP parameters.
///
/// All predictions are derived from analysis of 59 archive volumes across 12 NEXRAD sites,
/// 8 VCP types, and diverse meteorological scenarios. The azimuth rate from the VCP is the
/// dominant predictor, achieving a mean absolute error of 0.33s for sweep duration prediction.
pub struct ChunkTimingModel;

impl ChunkTimingModel {
    /// Predicted sweep duration in seconds based on azimuth rotation rate.
    ///
    /// Formula: `(360 / azimuth_rate_dps) - 0.67`
    ///
    /// Returns `None` if `azimuth_rate_dps` is zero or negative.
    pub fn sweep_duration_secs(azimuth_rate_dps: f64) -> Option<f64> {
        if azimuth_rate_dps <= 0.0 {
            return None;
        }
        Some((360.0 / azimuth_rate_dps) - SWEEP_DURATION_BIAS_SECS)
    }

    /// Predicted duration for a single chunk within a sweep.
    ///
    /// Chunks divide a sweep evenly: `sweep_duration / chunks_in_sweep`.
    ///
    /// Returns `None` if `azimuth_rate_dps` is zero or negative, or `chunks_in_sweep` is zero.
    pub fn chunk_duration_secs(azimuth_rate_dps: f64, chunks_in_sweep: usize) -> Option<f64> {
        if chunks_in_sweep == 0 {
            return None;
        }
        Self::sweep_duration_secs(azimuth_rate_dps).map(|d| d / chunks_in_sweep as f64)
    }

    /// Predicted inter-sweep gap in seconds based on elevation angle change and
    /// waveform transition.
    ///
    /// Formula: `0.7 + (|from_elevation - to_elevation| * 0.08) + waveform_penalty`
    ///
    /// The base 0.7s represents mode switching overhead. The 0.08s per degree represents
    /// antenna slew rate during transitions (much faster than the survey rotation rate).
    /// The waveform penalty accounts for additional mode-switch overhead when the
    /// waveform type changes between sweeps (see [`waveform_transition_penalty_secs`]).
    pub fn inter_sweep_gap_secs(
        from_elevation_deg: f64,
        to_elevation_deg: f64,
        from_waveform: Option<WaveformType>,
        to_waveform: Option<WaveformType>,
    ) -> f64 {
        let elevation_change = (to_elevation_deg - from_elevation_deg).abs();
        INTER_SWEEP_BASE_GAP_SECS
            + (elevation_change * INTER_SWEEP_ELEVATION_RATE_SECS_PER_DEG)
            + waveform_transition_penalty_secs(from_waveform, to_waveform)
    }

    /// Predicted inter-volume gap in seconds (constant 8.5s).
    ///
    /// Measures the gap from the End chunk of one volume to the Start chunk of the next.
    pub fn inter_volume_gap_secs() -> f64 {
        INTER_VOLUME_GAP_SECS
    }

    /// Predicted gap in seconds from the Start chunk to the first intermediate chunk
    /// within the same volume (constant 1.5s — see
    /// [`START_TO_FIRST_INTERMEDIATE_GAP_SECS`]).
    ///
    /// Distinct from [`inter_volume_gap_secs`]: that measures End → Start across volumes,
    /// whereas this measures Start → first intermediate within a single volume.
    pub fn start_to_first_intermediate_gap_secs() -> f64 {
        START_TO_FIRST_INTERMEDIATE_GAP_SECS
    }

    /// Estimate the time interval in seconds between two consecutive chunks.
    ///
    /// Thin wrapper around [`Self::estimate_chunk_interval_breakdown`] for callers
    /// that only need the total. Diagnostic paths should call the breakdown form
    /// directly so the component split is observable.
    pub fn estimate_chunk_interval_secs(previous: &ChunkMetadata, next: &ChunkMetadata) -> f64 {
        Self::estimate_chunk_interval_breakdown(previous, next).total_secs
    }

    /// Estimate the time interval between two consecutive chunks, with the
    /// physics decomposition exposed for diagnostics.
    ///
    /// Three cases:
    /// 1. **Start chunk** (inter-volume): fixed inter-volume gap (~8.5s).
    /// 2. **First chunk in a new sweep** (inter-sweep): chunk duration +
    ///    inter-sweep gap (base + elevation slew + waveform penalty).
    /// 3. **Intra-sweep chunk**: pure chunk duration (sweep_duration /
    ///    chunks_in_sweep).
    ///
    /// Falls back to a static chunk-duration default if the azimuth rate is
    /// zero or unavailable; in that case `chunk_duration_secs` in the
    /// returned breakdown is `None` to signal the fallback was used.
    pub fn estimate_chunk_interval_breakdown(
        previous: &ChunkMetadata,
        next: &ChunkMetadata,
    ) -> PhysicsBreakdown {
        // Case 1: Start chunk (beginning of new volume)
        if next.is_start_chunk() {
            return PhysicsBreakdown {
                case: IntervalCase::InterVolume,
                total_secs: Self::inter_volume_gap_secs(),
                chunk_duration_secs: None,
                inter_sweep_gap_secs: None,
                waveform_penalty_secs: None,
            };
        }

        let chunk_duration =
            Self::chunk_duration_secs(next.azimuth_rate_dps(), next.chunks_in_sweep());

        // Case 2: First chunk in a new sweep (inter-sweep transition)
        if next.is_first_in_sweep() {
            let waveform_penalty =
                waveform_transition_penalty_secs(previous.waveform_type(), next.waveform_type());
            let elevation_change =
                (next.elevation_angle_deg() - previous.elevation_angle_deg()).abs();
            let gap = INTER_SWEEP_BASE_GAP_SECS
                + elevation_change * INTER_SWEEP_ELEVATION_RATE_SECS_PER_DEG
                + waveform_penalty;
            let total = match chunk_duration {
                Some(d) => d + gap,
                None => gap + Self::fallback_chunk_duration_secs(),
            };
            return PhysicsBreakdown {
                case: IntervalCase::InterSweep,
                total_secs: total,
                chunk_duration_secs: chunk_duration,
                inter_sweep_gap_secs: Some(gap),
                waveform_penalty_secs: if waveform_penalty > 0.0 {
                    Some(waveform_penalty)
                } else {
                    None
                },
            };
        }

        // Case 3: Intra-sweep chunk
        let total = chunk_duration.unwrap_or(Self::fallback_chunk_duration_secs());
        PhysicsBreakdown {
            case: IntervalCase::IntraSweep,
            total_secs: total,
            chunk_duration_secs: chunk_duration,
            inter_sweep_gap_secs: None,
            waveform_penalty_secs: None,
        }
    }

    /// Fallback chunk duration when azimuth rate is unavailable.
    ///
    /// Uses the midpoint of observed chunk durations (~4s) as a conservative default.
    fn fallback_chunk_duration_secs() -> f64 {
        4.0
    }
}

/// Extra inter-sweep gap in seconds attributable to a waveform-type transition.
///
/// The base `inter_sweep_gap_secs` already covers antenna slew and a small mode-switch
/// overhead. This function adds a further penalty when the waveform type itself changes,
/// which empirically dominates the physics-only prediction error on sweep boundaries.
///
/// Calibration is from two VCP 212 volumes (2026-04-24). The initial values were
/// tuned from the first volume; after deploying them, a second volume showed residual
/// pred_err of +1.8 to +3.1s on CS→CDW (penalty was diluted ~30% by the physics/historical
/// blend in `scan_timing_projection`) and +1.95s on B→CDWO. These values are sized to
/// land predictions within ~0.5s of observed after blend dilution.
///
/// Specific pairs are matched first; asymmetric catch-alls apply when only one side is
/// known.
fn waveform_transition_penalty_secs(from: Option<WaveformType>, to: Option<WaveformType>) -> f64 {
    let (from, to) = match (from, to) {
        (Some(f), Some(t)) => (f, t),
        // Start chunk or otherwise unknown: no additional penalty — the caller
        // (inter_volume_gap branch, or base inter-sweep gap) already covers it.
        _ => return 0.0,
    };

    if std::mem::discriminant(&from) == std::mem::discriminant(&to) {
        return 0.0;
    }

    match (from, to) {
        // CS → CDW: same-angle SAILS/MRLE transition. The dominant source of
        // remaining empty polls. Effective shift after 70/30 blend dilution is ~2.8s.
        (WaveformType::CS, WaveformType::CDW) => 4.0,
        // CDW → CS: reverse direction, observed ~+1s drift (held on Δstart across both volumes).
        (WaveformType::CDW, WaveformType::CS) => 1.0,
        // B → CDWO: high-elevation transition between B sweeps and CDWO. Bumped after
        // a second volume showed +1.95s residual pred_err at this transition.
        (WaveformType::B, WaveformType::CDWO) => 3.5,
        // CDWO leaving to any other waveform: small penalty, ~+1s.
        (WaveformType::CDWO, _) => 1.0,
        // Anything arriving at B: small penalty, ~+1s.
        (_, WaveformType::B) => 1.0,
        // Catch-all for untabulated waveform changes: conservative middle ground.
        _ => 2.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    const EPS: f64 = 1e-9;

    #[wasm_bindgen_test]
    fn sweep_duration_normal_rate() {
        // 360/18 - 0.67 = 20 - 0.67 = 19.33.
        let d = ChunkTimingModel::sweep_duration_secs(18.0).unwrap();
        assert!((d - 19.33).abs() < EPS, "got {d}");
    }

    #[wasm_bindgen_test]
    fn sweep_duration_zero_and_negative_rate_is_none() {
        assert_eq!(ChunkTimingModel::sweep_duration_secs(0.0), None);
        assert_eq!(ChunkTimingModel::sweep_duration_secs(-5.0), None);
    }

    #[wasm_bindgen_test]
    fn sweep_duration_high_rate_goes_negative() {
        // KNOWN UNCLAMPED EDGE: a rate above 360/0.67 ≈ 537.3 dps makes
        // 360/rate < 0.67, so the bias subtraction yields a NEGATIVE duration.
        // This pins the current (unclamped) behavior; if a lower clamp is ever
        // added this test should change deliberately rather than silently.
        let d = ChunkTimingModel::sweep_duration_secs(600.0).unwrap();
        // 360/600 - 0.67 = 0.6 - 0.67 = -0.07.
        assert!((d - (-0.07)).abs() < EPS, "got {d}");
        assert!(d < 0.0);
    }

    #[wasm_bindgen_test]
    fn chunk_duration_divides_sweep_evenly() {
        // sweep = 19.33; / 6 chunks.
        let d = ChunkTimingModel::chunk_duration_secs(18.0, 6).unwrap();
        assert!((d - 19.33 / 6.0).abs() < EPS, "got {d}");
    }

    #[wasm_bindgen_test]
    fn chunk_duration_zero_chunks_is_none() {
        assert_eq!(ChunkTimingModel::chunk_duration_secs(18.0, 0), None);
    }

    #[wasm_bindgen_test]
    fn chunk_duration_bad_rate_is_none() {
        assert_eq!(ChunkTimingModel::chunk_duration_secs(0.0, 6), None);
    }

    #[wasm_bindgen_test]
    fn inter_sweep_gap_base_plus_slew_plus_waveform() {
        // Same elevation, no waveform change → just the 0.7 base.
        let g = ChunkTimingModel::inter_sweep_gap_secs(1.0, 1.0, None, None);
        assert!((g - 0.7).abs() < EPS, "got {g}");
        // 5° elevation change, no waveform info → 0.7 + 5*0.08 = 1.1.
        let g = ChunkTimingModel::inter_sweep_gap_secs(1.0, 6.0, None, None);
        assert!((g - 1.1).abs() < EPS, "got {g}");
        // 5° change + CS→CDW waveform penalty (4.0) → 1.1 + 4.0 = 5.1.
        let g = ChunkTimingModel::inter_sweep_gap_secs(
            1.0,
            6.0,
            Some(WaveformType::CS),
            Some(WaveformType::CDW),
        );
        assert!((g - 5.1).abs() < EPS, "got {g}");
    }

    #[wasm_bindgen_test]
    fn inter_volume_and_start_gap_constants() {
        assert!((ChunkTimingModel::inter_volume_gap_secs() - 8.5).abs() < EPS);
        assert!((ChunkTimingModel::start_to_first_intermediate_gap_secs() - 1.5).abs() < EPS);
    }

    #[wasm_bindgen_test]
    fn breakdown_inter_volume_when_next_is_start() {
        // next is a Start chunk (sequence 1) → fixed inter-volume gap, no
        // chunk-duration / inter-sweep / waveform components.
        let prev = ChunkMetadata::for_test(20, Some(3), 5, 6, false, 18.0);
        let next = ChunkMetadata::for_test(1, None, 0, 1, false, 0.0);
        let b = ChunkTimingModel::estimate_chunk_interval_breakdown(&prev, &next);
        assert_eq!(b.case, IntervalCase::InterVolume);
        assert!((b.total_secs - 8.5).abs() < EPS);
        assert_eq!(b.chunk_duration_secs, None);
        assert_eq!(b.inter_sweep_gap_secs, None);
        assert_eq!(b.waveform_penalty_secs, None);
    }

    #[wasm_bindgen_test]
    fn breakdown_inter_sweep_first_in_sweep() {
        // next is the first chunk in a new sweep (same elevation angle 0.5 as
        // the for_test default, so no slew term), no waveform info on either
        // chunk → gap = base 0.7, total = chunk_duration + 0.7.
        let prev = ChunkMetadata::for_test(7, Some(1), 5, 6, false, 18.0);
        let next = ChunkMetadata::for_test(8, Some(2), 0, 6, true, 18.0);
        let b = ChunkTimingModel::estimate_chunk_interval_breakdown(&prev, &next);
        assert_eq!(b.case, IntervalCase::InterSweep);
        let chunk_dur = 19.33 / 6.0;
        assert!((b.chunk_duration_secs.unwrap() - chunk_dur).abs() < EPS);
        assert!((b.inter_sweep_gap_secs.unwrap() - 0.7).abs() < EPS);
        // for_test sets waveform_type=None, so no penalty is recorded.
        assert_eq!(b.waveform_penalty_secs, None);
        assert!(
            (b.total_secs - (chunk_dur + 0.7)).abs() < EPS,
            "got {}",
            b.total_secs
        );
    }

    #[wasm_bindgen_test]
    fn breakdown_inter_sweep_falls_back_when_rate_unavailable() {
        // azimuth rate 0 → chunk_duration None → fallback 4.0 added to the gap,
        // and chunk_duration_secs in the breakdown stays None to signal it.
        let prev = ChunkMetadata::for_test(7, Some(1), 5, 6, false, 0.0);
        let next = ChunkMetadata::for_test(8, Some(2), 0, 6, true, 0.0);
        let b = ChunkTimingModel::estimate_chunk_interval_breakdown(&prev, &next);
        assert_eq!(b.case, IntervalCase::InterSweep);
        assert_eq!(b.chunk_duration_secs, None);
        assert!((b.inter_sweep_gap_secs.unwrap() - 0.7).abs() < EPS);
        // total = gap (0.7) + fallback (4.0) = 4.7.
        assert!((b.total_secs - 4.7).abs() < EPS, "got {}", b.total_secs);
    }

    #[wasm_bindgen_test]
    fn breakdown_intra_sweep_is_pure_chunk_duration() {
        // next is not start, not first-in-sweep → pure chunk duration.
        let prev = ChunkMetadata::for_test(9, Some(2), 1, 6, false, 18.0);
        let next = ChunkMetadata::for_test(10, Some(2), 2, 6, false, 18.0);
        let b = ChunkTimingModel::estimate_chunk_interval_breakdown(&prev, &next);
        assert_eq!(b.case, IntervalCase::IntraSweep);
        let chunk_dur = 19.33 / 6.0;
        assert!(
            (b.total_secs - chunk_dur).abs() < EPS,
            "got {}",
            b.total_secs
        );
        assert!((b.chunk_duration_secs.unwrap() - chunk_dur).abs() < EPS);
        assert_eq!(b.inter_sweep_gap_secs, None);
        assert_eq!(b.waveform_penalty_secs, None);
    }

    #[wasm_bindgen_test]
    fn breakdown_intra_sweep_fallback_when_rate_unavailable() {
        let prev = ChunkMetadata::for_test(9, Some(2), 1, 6, false, 0.0);
        let next = ChunkMetadata::for_test(10, Some(2), 2, 6, false, 0.0);
        let b = ChunkTimingModel::estimate_chunk_interval_breakdown(&prev, &next);
        assert_eq!(b.case, IntervalCase::IntraSweep);
        assert_eq!(b.chunk_duration_secs, None);
        assert!((b.total_secs - 4.0).abs() < EPS); // fallback
    }

    #[wasm_bindgen_test]
    fn waveform_penalty_table() {
        use WaveformType::*;
        let pen = waveform_transition_penalty_secs;
        // None on either side → 0.0.
        assert_eq!(pen(None, None), 0.0);
        assert_eq!(pen(Some(CS), None), 0.0);
        assert_eq!(pen(None, Some(CDW)), 0.0);
        // Same discriminant → 0.0.
        assert_eq!(pen(Some(CS), Some(CS)), 0.0);
        assert_eq!(pen(Some(B), Some(B)), 0.0);
        // Tabulated specific pairs.
        assert_eq!(pen(Some(CS), Some(CDW)), 4.0);
        assert_eq!(pen(Some(CDW), Some(CS)), 1.0);
        assert_eq!(pen(Some(B), Some(CDWO)), 3.5);
        // CDWO leaving to any other → 1.0 (matches before the catch-all).
        assert_eq!(pen(Some(CDWO), Some(CS)), 1.0);
        assert_eq!(pen(Some(CDWO), Some(B)), 1.0);
        // Anything arriving at B → 1.0.
        assert_eq!(pen(Some(CS), Some(B)), 1.0);
        assert_eq!(pen(Some(SPP), Some(B)), 1.0);
        // Catch-all for an untabulated change → 2.0.
        assert_eq!(pen(Some(CS), Some(SPP)), 2.0);
        assert_eq!(pen(Some(CDW), Some(CDWO)), 2.0);
    }
}
