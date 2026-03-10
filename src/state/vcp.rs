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
}
