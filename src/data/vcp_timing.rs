//! VCP timing physics.
//!
//! The Volume Coverage Pattern extracted from a NEXRAD Message Type 5
//! record, plus the duration derivations built on it: per-elevation sweep
//! durations via Method-A azimuth-rate weighting (falling back to Method-B
//! category rates from `crate::data::vcp`) and total volume duration
//! estimation.

use serde::{Deserialize, Serialize};

/// A single elevation cut extracted from a VCP message (Message Type 5).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedVcpElevation {
    /// Elevation angle in degrees.
    pub angle: f32,
    /// Waveform type: "CS", "CDW", "CDWO", "B", "SPP".
    pub waveform: String,
    /// Surveillance PRF number (1-8), relates to unambiguous range.
    pub prf_number: u8,
    /// SAILS (Supplemental Adaptive Intra-Volume Low-Level Scan) cut.
    pub is_sails: bool,
    /// MRLE (Mid-Volume Rescan of Low-Level Elevations) cut.
    pub is_mrle: bool,
    /// BASE TILT cut.
    pub is_base_tilt: bool,
    /// Azimuth rotation rate in degrees/second from the VCP message.
    /// Primary input for sweep duration estimation: duration ≈ 360° / rate.
    #[serde(default)]
    pub azimuth_rate: Option<f32>,
}

/// Full Volume Coverage Pattern extracted from a NEXRAD VCP message (Type 5).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedVcp {
    /// VCP number (e.g., 215, 35, 212).
    pub number: u16,
    /// Ordered elevation cuts in this VCP.
    pub elevations: Vec<ExtractedVcpElevation>,
}

impl ExtractedVcp {
    /// Compute per-elevation sweep durations as fractions of total volume duration.
    ///
    /// Uses Method A (weight = 1/azimuth_rate) when azimuth rates are available,
    /// falling back to Method B category-based weights from empirical study, and
    /// finally to even distribution if neither is available.
    ///
    /// Returns a `Vec<f64>` with one entry per elevation, each being the estimated
    /// sweep duration in seconds for the given `total_volume_duration`.
    pub fn sweep_durations(&self, total_volume_duration: f64) -> Vec<f64> {
        if self.elevations.is_empty() {
            return Vec::new();
        }

        let weights: Vec<f64> = self
            .elevations
            .iter()
            .map(|e| {
                if let Some(rate) = e.azimuth_rate {
                    if rate > 0.0 {
                        return 1.0 / rate as f64;
                    }
                }
                // Method B fallback: use category-based weights
                let is_clear_air = crate::data::vcp::is_clear_air_vcp(self.number);
                1.0 / crate::data::vcp::fallback_azimuth_rate(
                    is_clear_air,
                    &e.waveform,
                    e.prf_number,
                )
            })
            .collect();

        let total_weight: f64 = weights.iter().sum();
        if total_weight <= 0.0 {
            // Shouldn't happen, but fall back to even distribution
            let even = total_volume_duration / self.elevations.len() as f64;
            return vec![even; self.elevations.len()];
        }

        weights
            .iter()
            .map(|w| (w / total_weight) * total_volume_duration)
            .collect()
    }

    /// Estimate total volume scan duration (seconds) from per-elevation azimuth rates.
    ///
    /// Computes `sum(360° / rate_i)` for each elevation. When azimuth rates are not
    /// available, uses Method B fallback rates. Returns `None` if there are no elevations.
    pub fn estimated_volume_duration(&self) -> Option<f64> {
        if self.elevations.is_empty() {
            return None;
        }

        let total: f64 = self
            .elevations
            .iter()
            .map(|e| {
                let rate = if let Some(r) = e.azimuth_rate {
                    if r > 0.0 {
                        r as f64
                    } else {
                        let is_clear_air = crate::data::vcp::is_clear_air_vcp(self.number);
                        crate::data::vcp::fallback_azimuth_rate(
                            is_clear_air,
                            &e.waveform,
                            e.prf_number,
                        )
                    }
                } else {
                    let is_clear_air = crate::data::vcp::is_clear_air_vcp(self.number);
                    crate::data::vcp::fallback_azimuth_rate(is_clear_air, &e.waveform, e.prf_number)
                };
                360.0 / rate
            })
            .sum();

        Some(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    // ── ExtractedVcp::sweep_durations / estimated_volume_duration ────────────

    fn elev(rate: Option<f32>) -> ExtractedVcpElevation {
        ExtractedVcpElevation {
            angle: 0.5,
            waveform: "CS".to_string(),
            prf_number: 1,
            is_sails: false,
            is_mrle: false,
            is_base_tilt: false,
            azimuth_rate: rate,
        }
    }

    /// Method A: with explicit azimuth rates the per-cut durations are
    /// proportional to 1/rate and sum to the total volume duration. A faster
    /// rate yields a shorter sweep.
    #[wasm_bindgen_test]
    fn sweep_durations_method_a_proportional_to_inverse_rate() {
        let vcp = ExtractedVcp {
            number: 212, // precip (not clear-air)
            elevations: vec![elev(Some(20.0)), elev(Some(10.0))],
        };
        let durs = vcp.sweep_durations(300.0);
        // weights = [1/20, 1/10] = [0.05, 0.10]; total 0.15.
        // durations = [0.05/0.15*300, 0.10/0.15*300] = [100, 200].
        assert!((durs[0] - 100.0).abs() < 1e-9, "got {}", durs[0]);
        assert!((durs[1] - 200.0).abs() < 1e-9, "got {}", durs[1]);
        // Sums to the total (weights normalize).
        assert!((durs.iter().sum::<f64>() - 300.0).abs() < 1e-9);
        // Faster rate (cut 0) → shorter duration.
        assert!(durs[0] < durs[1]);
    }

    /// Method B fallback: a `None` rate falls through to the category-based
    /// `fallback_azimuth_rate`. Two identical fallback cuts split the total
    /// evenly, and the rate used matches the hand-computed table value.
    #[wasm_bindgen_test]
    fn sweep_durations_method_b_uses_fallback_rate() {
        let vcp = ExtractedVcp {
            number: 212, // precip → fallback_azimuth_rate(false, "CS", 1) == 21.1
            elevations: vec![elev(None), elev(None)],
        };
        let durs = vcp.sweep_durations(300.0);
        // Equal fallback weights → even split.
        assert!((durs[0] - 150.0).abs() < 1e-9, "got {}", durs[0]);
        assert!((durs[1] - 150.0).abs() < 1e-9);
        assert!((durs.iter().sum::<f64>() - 300.0).abs() < 1e-9);

        // estimated_volume_duration uses the same fallback: 2 * (360 / 21.1).
        let expected = 2.0 * (360.0 / 21.1);
        let est = vcp.estimated_volume_duration().unwrap();
        assert!((est - expected).abs() < 1e-9, "got {est}, want {expected}");
    }

    /// A cut with `azimuth_rate = Some(0.0)` (non-positive) is demoted to the
    /// fallback, exactly like `None`.
    #[wasm_bindgen_test]
    fn sweep_durations_zero_rate_demoted_to_fallback() {
        let zero = ExtractedVcp {
            number: 212,
            elevations: vec![elev(Some(0.0)), elev(Some(0.0))],
        };
        let none = ExtractedVcp {
            number: 212,
            elevations: vec![elev(None), elev(None)],
        };
        // Zero rate and None produce identical durations (both use fallback).
        assert_eq!(zero.sweep_durations(300.0), none.sweep_durations(300.0));
        assert_eq!(
            zero.estimated_volume_duration(),
            none.estimated_volume_duration()
        );
    }

    /// Mixed VCP: some cuts carry rates, others don't — Method A and Method B
    /// weights coexist and the durations still sum to the total.
    #[wasm_bindgen_test]
    fn sweep_durations_mixed_rates_sum_to_total() {
        let vcp = ExtractedVcp {
            number: 212,
            elevations: vec![elev(Some(20.0)), elev(None), elev(Some(10.0))],
        };
        let durs = vcp.sweep_durations(300.0);
        assert_eq!(durs.len(), 3);
        assert!((durs.iter().sum::<f64>() - 300.0).abs() < 1e-9);
        // The explicit-rate cuts retain their ordering (20 dps shorter than 10).
        assert!(durs[0] < durs[2]);

        // estimated_volume_duration = 360/20 + 360/21.1 + 360/10.
        let expected = 360.0 / 20.0 + 360.0 / 21.1 + 360.0 / 10.0;
        let est = vcp.estimated_volume_duration().unwrap();
        assert!((est - expected).abs() < 1e-9, "got {est}, want {expected}");
    }

    /// Empty VCP: no elevations → an empty duration vec and `None` estimate.
    #[wasm_bindgen_test]
    fn sweep_durations_empty_vcp() {
        let vcp = ExtractedVcp {
            number: 212,
            elevations: vec![],
        };
        assert!(vcp.sweep_durations(300.0).is_empty());
        assert_eq!(vcp.estimated_volume_duration(), None);
    }

    /// `estimated_volume_duration` is the positive sum of 360/rate over the
    /// cuts, for a hand-computed all-rates VCP.
    #[wasm_bindgen_test]
    fn estimated_volume_duration_sum_of_360_over_rate() {
        let vcp = ExtractedVcp {
            number: 212,
            elevations: vec![elev(Some(20.0)), elev(Some(10.0)), elev(Some(18.0))],
        };
        // 360/20 + 360/10 + 360/18 = 18 + 36 + 20 = 74.
        let est = vcp.estimated_volume_duration().unwrap();
        assert!((est - 74.0).abs() < 1e-9, "got {est}");
        assert!(est > 0.0);
    }
}
