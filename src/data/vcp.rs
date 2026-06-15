//! Volume Coverage Pattern (VCP) definitions.
//!
//! Contains static metadata about common VCPs used by NEXRAD radars.

/// A single elevation angle within a VCP
#[derive(Clone, Debug)]
pub struct VcpElevation {
    /// Elevation angle in degrees
    pub angle: f32,
    /// Waveform type: "CS" (Contiguous Surveillance) or "CD" (Contiguous Doppler)
    pub waveform: &'static str,
    /// PRF category: "Low", "Med", or "High"
    pub prf: &'static str,
}

/// Definition of a Volume Coverage Pattern
#[derive(Clone, Debug)]
pub struct VcpDefinition {
    /// VCP number (e.g., 215, 35)
    #[allow(dead_code)]
    pub number: u16,
    /// Short name for the VCP
    pub name: &'static str,
    /// Description of when this VCP is used
    #[allow(dead_code)]
    pub description: &'static str,
    /// List of elevation angles in this VCP
    pub elevations: &'static [VcpElevation],
}

/// VCP 215 - Precipitation Mode (most common)
/// 14 elevations, ~5 minute volume scan
static VCP_215_ELEVATIONS: &[VcpElevation] = &[
    VcpElevation {
        angle: 0.5,
        waveform: "CS",
        prf: "Low",
    },
    VcpElevation {
        angle: 0.9,
        waveform: "CS",
        prf: "Low",
    },
    VcpElevation {
        angle: 1.3,
        waveform: "CS",
        prf: "Low",
    },
    VcpElevation {
        angle: 1.8,
        waveform: "CS",
        prf: "Low",
    },
    VcpElevation {
        angle: 2.4,
        waveform: "CD",
        prf: "Med",
    },
    VcpElevation {
        angle: 3.1,
        waveform: "CD",
        prf: "Med",
    },
    VcpElevation {
        angle: 4.0,
        waveform: "CD",
        prf: "Med",
    },
    VcpElevation {
        angle: 5.1,
        waveform: "CD",
        prf: "High",
    },
    VcpElevation {
        angle: 6.4,
        waveform: "CD",
        prf: "High",
    },
    VcpElevation {
        angle: 8.0,
        waveform: "CD",
        prf: "High",
    },
    VcpElevation {
        angle: 10.0,
        waveform: "CD",
        prf: "High",
    },
    VcpElevation {
        angle: 12.5,
        waveform: "CD",
        prf: "High",
    },
    VcpElevation {
        angle: 15.6,
        waveform: "CD",
        prf: "High",
    },
    VcpElevation {
        angle: 19.5,
        waveform: "CD",
        prf: "High",
    },
];

static VCP_215: VcpDefinition = VcpDefinition {
    number: 215,
    name: "Precipitation",
    description: "Precipitation Mode - 14 elevations, ~5 min scan",
    elevations: VCP_215_ELEVATIONS,
};

/// VCP 35 - Clear Air Mode
/// 5 elevations, ~10 minute volume scan (slower rotation for sensitivity)
static VCP_35_ELEVATIONS: &[VcpElevation] = &[
    VcpElevation {
        angle: 0.5,
        waveform: "CS",
        prf: "Low",
    },
    VcpElevation {
        angle: 1.5,
        waveform: "CS",
        prf: "Low",
    },
    VcpElevation {
        angle: 2.5,
        waveform: "CS",
        prf: "Low",
    },
    VcpElevation {
        angle: 3.5,
        waveform: "CS",
        prf: "Low",
    },
    VcpElevation {
        angle: 4.5,
        waveform: "CS",
        prf: "Low",
    },
];

static VCP_35: VcpDefinition = VcpDefinition {
    number: 35,
    name: "Clear Air",
    description: "Clear Air Mode - 5 elevations, ~10 min scan",
    elevations: VCP_35_ELEVATIONS,
};

/// VCP 212 - Precipitation Mode (faster)
/// 14 elevations, ~4.5 minute volume scan
static VCP_212_ELEVATIONS: &[VcpElevation] = &[
    VcpElevation {
        angle: 0.5,
        waveform: "CS",
        prf: "Low",
    },
    VcpElevation {
        angle: 0.9,
        waveform: "CS",
        prf: "Low",
    },
    VcpElevation {
        angle: 1.3,
        waveform: "CS",
        prf: "Low",
    },
    VcpElevation {
        angle: 1.8,
        waveform: "CS",
        prf: "Low",
    },
    VcpElevation {
        angle: 2.4,
        waveform: "CD",
        prf: "Med",
    },
    VcpElevation {
        angle: 3.1,
        waveform: "CD",
        prf: "Med",
    },
    VcpElevation {
        angle: 4.0,
        waveform: "CD",
        prf: "High",
    },
    VcpElevation {
        angle: 5.1,
        waveform: "CD",
        prf: "High",
    },
    VcpElevation {
        angle: 6.4,
        waveform: "CD",
        prf: "High",
    },
    VcpElevation {
        angle: 8.0,
        waveform: "CD",
        prf: "High",
    },
    VcpElevation {
        angle: 10.0,
        waveform: "CD",
        prf: "High",
    },
    VcpElevation {
        angle: 12.5,
        waveform: "CD",
        prf: "High",
    },
    VcpElevation {
        angle: 15.6,
        waveform: "CD",
        prf: "High",
    },
    VcpElevation {
        angle: 19.5,
        waveform: "CD",
        prf: "High",
    },
];

static VCP_212: VcpDefinition = VcpDefinition {
    number: 212,
    name: "Precip Fast",
    description: "Precipitation Mode (Fast) - 14 elevations, ~4.5 min scan",
    elevations: VCP_212_ELEVATIONS,
};

/// Get the VCP definition for a given VCP number
pub fn get_vcp_definition(vcp: u16) -> Option<&'static VcpDefinition> {
    match vcp {
        215 => Some(&VCP_215),
        35 => Some(&VCP_35),
        212 => Some(&VCP_212),
        _ => None,
    }
}

/// Whether a VCP number is a clear-air mode pattern.
pub fn is_clear_air_vcp(vcp: u16) -> bool {
    matches!(vcp, 31 | 32 | 35)
}

/// Method B fallback: estimate azimuth rate (deg/s) from waveform type and PRF number
/// when the actual azimuth rate is not available from the VCP message.
///
/// Based on empirical analysis of 851 sweep measurements across VCPs 12, 34, 35, 212.
/// PRF numbers map to categories: 1-2 = Low, 3 = Med, 4-5 = High.
pub fn fallback_azimuth_rate(is_clear_air: bool, waveform: &str, prf_number: u8) -> f64 {
    // Accept both "B" (short code used by `ExtractedVcp.waveform`) and
    // "Batch" (library Debug format) so we don't silently drop into the
    // default branch when callers happen to use the short form.
    if is_clear_air {
        match (waveform, prf_number) {
            ("CS", 1) => 5.0,
            ("CS", 2) => 5.5,
            ("CS", _) => 5.0, // Default CS clear-air
            ("CDW", _) => 15.7,
            ("CDWO", _) => 8.5,
            ("B" | "Batch", 3) => 14.6,
            ("B" | "Batch", 4) => 17.8,
            ("B" | "Batch", 5) => 16.9,
            ("B" | "Batch", _) => 18.1,
            _ => 10.0, // Conservative default for unknown clear-air waveforms
        }
    } else {
        match (waveform, prf_number) {
            ("CS", 1) => 21.1,
            ("CS", 2) => 23.0,
            ("CS", _) => 21.1, // Default CS precip
            ("CDW", _) => 18.8,
            ("CDWO", _) => 28.5,
            ("B" | "Batch", 3) => 26.2,
            ("B" | "Batch", 4) => 26.9,
            ("B" | "Batch", 5) => 27.7,
            ("B" | "Batch", _) => 26.8,
            _ => 22.0, // Conservative default for unknown precip waveforms
        }
    }
}

/// Compute even-distribution sweep durations (fallback when no VCP elevation data
/// is available). Returns a vec of `count` equal durations summing to `total`.
#[allow(dead_code)]
pub fn even_sweep_durations(total: f64, count: usize) -> Vec<f64> {
    if count == 0 {
        return Vec::new();
    }
    vec![total / count as f64; count]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vcp_215_lookup() {
        let def = get_vcp_definition(215).unwrap();
        assert_eq!(def.name, "Precipitation");
        assert_eq!(def.elevations.len(), 14);
        assert_eq!(def.elevations[0].angle, 0.5);
        assert_eq!(def.elevations[13].angle, 19.5);
    }

    #[test]
    fn vcp_35_lookup() {
        let def = get_vcp_definition(35).unwrap();
        assert_eq!(def.name, "Clear Air");
        assert_eq!(def.elevations.len(), 5);
        assert_eq!(def.elevations[0].angle, 0.5);
        assert_eq!(def.elevations[4].angle, 4.5);
    }

    #[test]
    fn vcp_212_lookup() {
        let def = get_vcp_definition(212).unwrap();
        assert_eq!(def.name, "Precip Fast");
        assert_eq!(def.elevations.len(), 14);
    }

    #[test]
    fn vcp_unknown_returns_none() {
        assert!(get_vcp_definition(0).is_none());
        assert!(get_vcp_definition(999).is_none());
    }

    #[test]
    fn vcp_elevations_are_ascending() {
        for &vcp_num in &[215u16, 35, 212] {
            let def = get_vcp_definition(vcp_num).unwrap();
            for w in def.elevations.windows(2) {
                assert!(
                    w[1].angle > w[0].angle,
                    "VCP {} elevations not ascending: {} >= {}",
                    vcp_num,
                    w[0].angle,
                    w[1].angle
                );
            }
        }
    }

    #[test]
    fn is_clear_air_vcp_classification() {
        assert!(is_clear_air_vcp(31));
        assert!(is_clear_air_vcp(32));
        assert!(is_clear_air_vcp(35));
        assert!(!is_clear_air_vcp(12));
        assert!(!is_clear_air_vcp(212));
        assert!(!is_clear_air_vcp(215));
    }

    #[test]
    fn fallback_azimuth_rate_clear_air_slower_than_precip() {
        // Clear air CS/Low should be much slower than precip CS/Low
        let clear_air_rate = fallback_azimuth_rate(true, "CS", 1);
        let precip_rate = fallback_azimuth_rate(false, "CS", 1);
        assert!(
            clear_air_rate < precip_rate,
            "Clear air rate {clear_air_rate} should be less than precip rate {precip_rate}"
        );
    }

    #[test]
    fn fallback_azimuth_rate_all_positive() {
        for is_clear_air in [true, false] {
            for waveform in ["CS", "CDW", "CDWO", "B", "Batch", "Unknown"] {
                for prf in 0..=8 {
                    let rate = fallback_azimuth_rate(is_clear_air, waveform, prf);
                    assert!(
                        rate > 0.0,
                        "Rate should be positive for is_clear_air={is_clear_air}, waveform={waveform}, prf={prf}"
                    );
                }
            }
        }
    }

    #[test]
    fn fallback_azimuth_rate_matches_short_and_long_batch_keys() {
        // Production callers pass "B" (see ExtractedVcp.waveform); make sure
        // it reaches the Batch arms, not the generic default.
        for prf in [3u8, 4, 5, 6] {
            let short = fallback_azimuth_rate(false, "B", prf);
            let long = fallback_azimuth_rate(false, "Batch", prf);
            assert_eq!(short, long, "short/long keys disagree at prf={prf}");
            assert_ne!(short, 22.0, "prf={prf} fell through to default");
        }
        assert_eq!(fallback_azimuth_rate(false, "B", 3), 26.2);
        assert_eq!(fallback_azimuth_rate(false, "B", 4), 26.9);
        assert_eq!(fallback_azimuth_rate(false, "B", 5), 27.7);
        assert_eq!(fallback_azimuth_rate(true, "B", 3), 14.6);
    }

    #[test]
    fn even_sweep_durations_sums_to_total() {
        let total = 300.0;
        let durations = even_sweep_durations(total, 14);
        assert_eq!(durations.len(), 14);
        let sum: f64 = durations.iter().sum();
        assert!((sum - total).abs() < 1e-10);
    }

    #[test]
    fn even_sweep_durations_empty_for_zero_count() {
        assert!(even_sweep_durations(300.0, 0).is_empty());
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn vcp_215_metadata_fields() {
        let def = get_vcp_definition(215).unwrap();
        assert_eq!(def.number, 215);
        assert_eq!(
            def.description,
            "Precipitation Mode - 14 elevations, ~5 min scan"
        );
    }

    #[wasm_bindgen_test]
    fn vcp_35_metadata_fields() {
        let def = get_vcp_definition(35).unwrap();
        assert_eq!(def.number, 35);
        assert_eq!(def.name, "Clear Air");
        assert_eq!(
            def.description,
            "Clear Air Mode - 5 elevations, ~10 min scan"
        );
    }

    #[wasm_bindgen_test]
    fn vcp_212_metadata_fields() {
        let def = get_vcp_definition(212).unwrap();
        assert_eq!(def.number, 212);
        assert_eq!(
            def.description,
            "Precipitation Mode (Fast) - 14 elevations, ~4.5 min scan"
        );
    }

    #[wasm_bindgen_test]
    fn vcp_215_waveform_and_prf_transitions() {
        let def = get_vcp_definition(215).unwrap();
        // First four sweeps are Contiguous Surveillance / Low PRF.
        assert_eq!(def.elevations[0].waveform, "CS");
        assert_eq!(def.elevations[0].prf, "Low");
        assert_eq!(def.elevations[3].waveform, "CS");
        assert_eq!(def.elevations[3].prf, "Low");
        // Doppler begins at index 4 (2.4 deg), Med PRF.
        assert_eq!(def.elevations[4].waveform, "CD");
        assert_eq!(def.elevations[4].prf, "Med");
        // 4.0 deg in VCP 215 is Med PRF (index 6).
        assert!((def.elevations[6].angle - 4.0).abs() < 1e-6);
        assert_eq!(def.elevations[6].prf, "Med");
        // High PRF starts at 5.1 deg (index 7).
        assert_eq!(def.elevations[7].prf, "High");
        assert_eq!(def.elevations[13].prf, "High");
    }

    #[wasm_bindgen_test]
    fn vcp_212_differs_from_215_at_4deg() {
        // VCP 212's 4.0-deg sweep is High PRF, whereas VCP 215's is Med.
        let v212 = get_vcp_definition(212).unwrap();
        let v215 = get_vcp_definition(215).unwrap();
        assert!((v212.elevations[6].angle - 4.0).abs() < 1e-6);
        assert_eq!(v212.elevations[6].prf, "High");
        assert_eq!(v215.elevations[6].prf, "Med");
    }

    #[wasm_bindgen_test]
    fn vcp_35_all_contiguous_surveillance_low() {
        let def = get_vcp_definition(35).unwrap();
        for e in def.elevations {
            assert_eq!(e.waveform, "CS");
            assert_eq!(e.prf, "Low");
        }
        // Whole-degree-plus-half spacing: 0.5, 1.5, 2.5, 3.5, 4.5.
        assert!((def.elevations[1].angle - 1.5).abs() < 1e-6);
        assert!((def.elevations[2].angle - 2.5).abs() < 1e-6);
        assert!((def.elevations[3].angle - 3.5).abs() < 1e-6);
    }

    #[wasm_bindgen_test]
    fn is_clear_air_vcp_boundaries() {
        // Just outside the 31|32|35 set.
        assert!(!is_clear_air_vcp(30));
        assert!(!is_clear_air_vcp(33));
        assert!(!is_clear_air_vcp(34));
        assert!(!is_clear_air_vcp(36));
        assert!(!is_clear_air_vcp(0));
        // Inside the set.
        assert!(is_clear_air_vcp(31));
        assert!(is_clear_air_vcp(32));
    }

    #[wasm_bindgen_test]
    fn fallback_rate_clear_air_cs_arms() {
        assert!((fallback_azimuth_rate(true, "CS", 1) - 5.0).abs() < 1e-9);
        assert!((fallback_azimuth_rate(true, "CS", 2) - 5.5).abs() < 1e-9);
        // CS with prf outside 1/2 falls to the CS default (5.0), not the
        // generic clear-air default (10.0).
        assert!((fallback_azimuth_rate(true, "CS", 3) - 5.0).abs() < 1e-9);
        assert!((fallback_azimuth_rate(true, "CS", 0) - 5.0).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn fallback_rate_clear_air_doppler_and_batch_arms() {
        // CDW / CDWO ignore the PRF number.
        assert!((fallback_azimuth_rate(true, "CDW", 7) - 15.7).abs() < 1e-9);
        assert!((fallback_azimuth_rate(true, "CDWO", 1) - 8.5).abs() < 1e-9);
        // Batch arms for clear air.
        assert!((fallback_azimuth_rate(true, "Batch", 3) - 14.6).abs() < 1e-9);
        assert!((fallback_azimuth_rate(true, "B", 4) - 17.8).abs() < 1e-9);
        assert!((fallback_azimuth_rate(true, "B", 5) - 16.9).abs() < 1e-9);
        // Batch default (prf not 3/4/5).
        assert!((fallback_azimuth_rate(true, "Batch", 6) - 18.1).abs() < 1e-9);
        assert!((fallback_azimuth_rate(true, "B", 0) - 18.1).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn fallback_rate_clear_air_unknown_default() {
        // An unrecognized waveform falls to the conservative clear-air default.
        assert!((fallback_azimuth_rate(true, "ZZ", 1) - 10.0).abs() < 1e-9);
        assert!((fallback_azimuth_rate(true, "", 9) - 10.0).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn fallback_rate_precip_cs_arms() {
        assert!((fallback_azimuth_rate(false, "CS", 1) - 21.1).abs() < 1e-9);
        assert!((fallback_azimuth_rate(false, "CS", 2) - 23.0).abs() < 1e-9);
        // CS default for prf outside 1/2.
        assert!((fallback_azimuth_rate(false, "CS", 5) - 21.1).abs() < 1e-9);
        assert!((fallback_azimuth_rate(false, "CS", 0) - 21.1).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn fallback_rate_precip_doppler_batch_and_default() {
        assert!((fallback_azimuth_rate(false, "CDW", 2) - 18.8).abs() < 1e-9);
        assert!((fallback_azimuth_rate(false, "CDWO", 4) - 28.5).abs() < 1e-9);
        assert!((fallback_azimuth_rate(false, "Batch", 3) - 26.2).abs() < 1e-9);
        assert!((fallback_azimuth_rate(false, "B", 4) - 26.9).abs() < 1e-9);
        assert!((fallback_azimuth_rate(false, "B", 5) - 27.7).abs() < 1e-9);
        // Batch default arm (prf not 3/4/5).
        assert!((fallback_azimuth_rate(false, "Batch", 6) - 26.8).abs() < 1e-9);
        assert!((fallback_azimuth_rate(false, "B", 0) - 26.8).abs() < 1e-9);
        // Unknown precip waveform default.
        assert!((fallback_azimuth_rate(false, "ZZ", 1) - 22.0).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn fallback_rate_clear_air_always_below_precip_for_batch() {
        // Across the shared Batch PRF arms, clear-air is the slower mode.
        for prf in [3u8, 4, 5, 6] {
            let ca = fallback_azimuth_rate(true, "B", prf);
            let pr = fallback_azimuth_rate(false, "B", prf);
            assert!(ca < pr, "clear-air {ca} not < precip {pr} at prf={prf}");
        }
    }

    #[wasm_bindgen_test]
    fn even_sweep_durations_single_and_nonexact() {
        // count == 1 returns the whole total in one slot.
        let one = even_sweep_durations(42.0, 1);
        assert_eq!(one.len(), 1);
        assert!((one[0] - 42.0).abs() < 1e-9);

        // Non-evenly-divisible: each slot is total/count, and the sum still
        // reconstructs total within float tolerance.
        let three = even_sweep_durations(10.0, 3);
        assert_eq!(three.len(), 3);
        for d in &three {
            assert!((d - 10.0 / 3.0).abs() < 1e-9);
        }
        let sum: f64 = three.iter().sum();
        assert!((sum - 10.0).abs() < 1e-9);

        // Zero total with positive count yields zeros, not an empty vec.
        let zeros = even_sweep_durations(0.0, 4);
        assert_eq!(zeros.len(), 4);
        assert!(zeros.iter().all(|&d| d == 0.0));
    }
}
