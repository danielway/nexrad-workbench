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
    #[allow(dead_code)] // Part of data model API
    pub number: u16,
    /// Short name for the VCP
    pub name: &'static str,
    /// Description of when this VCP is used
    pub description: &'static str,
    /// List of elevation angles in this VCP
    pub elevations: &'static [VcpElevation],
}

/// VCP 215 - Precipitation Mode (most common)
/// 14 elevations, ~5 minute volume scan
static VCP_215_ELEVATIONS: &[VcpElevation] = &[
    VcpElevation { angle: 0.5, waveform: "CS", prf: "Low" },
    VcpElevation { angle: 0.9, waveform: "CS", prf: "Low" },
    VcpElevation { angle: 1.3, waveform: "CS", prf: "Low" },
    VcpElevation { angle: 1.8, waveform: "CS", prf: "Low" },
    VcpElevation { angle: 2.4, waveform: "CD", prf: "Med" },
    VcpElevation { angle: 3.1, waveform: "CD", prf: "Med" },
    VcpElevation { angle: 4.0, waveform: "CD", prf: "Med" },
    VcpElevation { angle: 5.1, waveform: "CD", prf: "High" },
    VcpElevation { angle: 6.4, waveform: "CD", prf: "High" },
    VcpElevation { angle: 8.0, waveform: "CD", prf: "High" },
    VcpElevation { angle: 10.0, waveform: "CD", prf: "High" },
    VcpElevation { angle: 12.5, waveform: "CD", prf: "High" },
    VcpElevation { angle: 15.6, waveform: "CD", prf: "High" },
    VcpElevation { angle: 19.5, waveform: "CD", prf: "High" },
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
    VcpElevation { angle: 0.5, waveform: "CS", prf: "Low" },
    VcpElevation { angle: 1.5, waveform: "CS", prf: "Low" },
    VcpElevation { angle: 2.5, waveform: "CS", prf: "Low" },
    VcpElevation { angle: 3.5, waveform: "CS", prf: "Low" },
    VcpElevation { angle: 4.5, waveform: "CS", prf: "Low" },
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
    VcpElevation { angle: 0.5, waveform: "CS", prf: "Low" },
    VcpElevation { angle: 0.9, waveform: "CS", prf: "Low" },
    VcpElevation { angle: 1.3, waveform: "CS", prf: "Low" },
    VcpElevation { angle: 1.8, waveform: "CS", prf: "Low" },
    VcpElevation { angle: 2.4, waveform: "CD", prf: "Med" },
    VcpElevation { angle: 3.1, waveform: "CD", prf: "Med" },
    VcpElevation { angle: 4.0, waveform: "CD", prf: "High" },
    VcpElevation { angle: 5.1, waveform: "CD", prf: "High" },
    VcpElevation { angle: 6.4, waveform: "CD", prf: "High" },
    VcpElevation { angle: 8.0, waveform: "CD", prf: "High" },
    VcpElevation { angle: 10.0, waveform: "CD", prf: "High" },
    VcpElevation { angle: 12.5, waveform: "CD", prf: "High" },
    VcpElevation { angle: 15.6, waveform: "CD", prf: "High" },
    VcpElevation { angle: 19.5, waveform: "CD", prf: "High" },
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
