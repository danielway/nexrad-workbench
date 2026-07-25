//! Visualization domain vocabulary — radar products, sweep identity,
//! elevation selection, and render-processing options.

use crate::data::ScanKey;

/// Available radar products for display.
#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RadarProduct {
    #[default]
    Reflectivity,
    Velocity,
    SpectrumWidth,
    DifferentialReflectivity,
    CorrelationCoefficient,
    DifferentialPhase,
    ClutterFilterPower,
}

impl RadarProduct {
    pub(crate) fn label(&self) -> &'static str {
        match self {
            RadarProduct::Reflectivity => "Reflectivity",
            RadarProduct::Velocity => "Velocity",
            RadarProduct::SpectrumWidth => "Spectrum Width",
            RadarProduct::DifferentialReflectivity => "Differential Reflectivity",
            RadarProduct::CorrelationCoefficient => "Correlation Coefficient",
            RadarProduct::DifferentialPhase => "Differential Phase",
            RadarProduct::ClutterFilterPower => "Clutter Filter Power",
        }
    }

    /// Unit string for display (e.g., "dBZ", "m/s").
    pub(crate) fn unit(&self) -> &'static str {
        match self {
            RadarProduct::Reflectivity => "dBZ",
            RadarProduct::Velocity => "m/s",
            RadarProduct::SpectrumWidth => "m/s",
            RadarProduct::DifferentialReflectivity => "dB",
            RadarProduct::CorrelationCoefficient => "",
            RadarProduct::DifferentialPhase => "\u{00B0}/km",
            RadarProduct::ClutterFilterPower => "dB",
        }
    }

    /// Short code for URL parameters.
    pub(crate) fn short_code(&self) -> &'static str {
        match self {
            RadarProduct::Reflectivity => "REF",
            RadarProduct::Velocity => "VEL",
            RadarProduct::SpectrumWidth => "SW",
            RadarProduct::DifferentialReflectivity => "ZDR",
            RadarProduct::CorrelationCoefficient => "CC",
            RadarProduct::DifferentialPhase => "KDP",
            RadarProduct::ClutterFilterPower => "CFP",
        }
    }

    /// Parse from a short code string.
    pub(crate) fn from_short_code(code: &str) -> Option<Self> {
        match code {
            "REF" => Some(RadarProduct::Reflectivity),
            "VEL" => Some(RadarProduct::Velocity),
            "SW" => Some(RadarProduct::SpectrumWidth),
            "ZDR" => Some(RadarProduct::DifferentialReflectivity),
            "CC" => Some(RadarProduct::CorrelationCoefficient),
            "KDP" => Some(RadarProduct::DifferentialPhase),
            "CFP" => Some(RadarProduct::ClutterFilterPower),
            _ => None,
        }
    }

    pub(crate) fn all() -> &'static [RadarProduct] {
        &[
            RadarProduct::Reflectivity,
            RadarProduct::Velocity,
            RadarProduct::SpectrumWidth,
            RadarProduct::DifferentialReflectivity,
            RadarProduct::CorrelationCoefficient,
            RadarProduct::DifferentialPhase,
            RadarProduct::ClutterFilterPower,
        ]
    }

    /// String identifier used by the worker protocol.
    pub(crate) fn to_worker_string(self) -> &'static str {
        match self {
            RadarProduct::Reflectivity => "reflectivity",
            RadarProduct::Velocity => "velocity",
            RadarProduct::SpectrumWidth => "spectrum_width",
            RadarProduct::DifferentialReflectivity => "differential_reflectivity",
            RadarProduct::CorrelationCoefficient => "correlation_coefficient",
            RadarProduct::DifferentialPhase => "differential_phase",
            RadarProduct::ClutterFilterPower => "reflectivity", // fallback
        }
    }
}

/// Fully-qualified identity of a single sweep — site, scan, elevation, product.
///
/// The single canonical "which sweep" identifier shared by the render
/// coordinator's dedup cache, the on-GPU `displayed` slot, and the
/// resolver that maps user intent to a concrete render target. By
/// construction, two `SweepIdentity` values compare equal iff they
/// reference the same on-disk sweep blob in IndexedDB.
///
/// `product` is the worker-string form (matches `SweepDataKey` and
/// `RadarProduct::to_worker_string`).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub(crate) struct SweepIdentity {
    pub scan_key: ScanKey,
    pub elevation_number: u8,
    pub product: String,
}

/// What is currently on the GPU canvas.
///
/// Populated only after a successful `update_data()` call in
/// `handle_decoded_outcome` / `handle_live_decoded_outcome`. Consumed by
/// the timeline (active border), canvas overlay (timestamp, elevation
/// text), and staleness counters.
///
/// This is the "displayed" half of the intent-vs-displayed split: user
/// intent lives in `elevation_selection`, the resolver produces a
/// target, the worker fulfils it, and only then does this slot move.
/// Reading this field is therefore safe to interpret as "what the user
/// is actually looking at right now."
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct DisplayedSweep {
    pub identity: SweepIdentity,
    /// Sweep start time (Unix seconds, sub-second precision).
    pub start_time: f64,
    /// Sweep end time (Unix seconds, sub-second precision).
    pub end_time: f64,
    /// Physical elevation angle in degrees, for overlay text.
    pub elevation_deg: f32,
}

impl SweepIdentity {
    pub(crate) fn new(scan_key: ScanKey, elevation_number: u8, product: impl Into<String>) -> Self {
        Self {
            scan_key,
            elevation_number,
            product: product.into(),
        }
    }

    /// Sub-second Unix-seconds form of the scan key.
    ///
    /// Used by the timeline to compare against `Scan::key_timestamp` (also
    /// `f64` with sub-second precision); round-trips through `UnixMillis`.
    pub(crate) fn scan_timestamp_secs(&self) -> f64 {
        self.scan_key.scan_start.as_secs_f64()
    }

    #[cfg(test)]
    pub(crate) fn site_id(&self) -> &str {
        &self.scan_key.site.0
    }
}

/// User's elevation selection — by specific VCP cut or auto (latest) mode.
#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) enum ElevationSelection {
    /// A specific VCP elevation number. The f32 is the angle at time of
    /// selection, used for resilience when VCP changes.
    Fixed { elevation_number: u8, angle: f32 },
    /// Auto: show the most recently completed sweep (any elevation).
    Latest,
}

impl Default for ElevationSelection {
    fn default() -> Self {
        ElevationSelection::Fixed {
            elevation_number: 1,
            angle: 0.5,
        }
    }
}

impl ElevationSelection {
    pub(crate) fn is_auto(&self) -> bool {
        matches!(self, ElevationSelection::Latest)
    }

    pub(crate) fn elevation_number(&self) -> Option<u8> {
        match self {
            ElevationSelection::Fixed {
                elevation_number, ..
            } => Some(*elevation_number),
            ElevationSelection::Latest => None,
        }
    }

    pub(crate) fn angle(&self) -> f32 {
        match self {
            ElevationSelection::Fixed { angle, .. } => *angle,
            ElevationSelection::Latest => 0.5,
        }
    }

    /// On VCP change, find the closest angle match and update elevation_number.
    pub(crate) fn resolve_for_vcp(&mut self, entries: &[ElevationListEntry]) {
        if let ElevationSelection::Fixed {
            angle,
            elevation_number,
        } = self
        {
            if let Some(best) = entries.iter().min_by(|a, b| {
                (a.angle - *angle)
                    .abs()
                    .partial_cmp(&(b.angle - *angle).abs())
                    .unwrap_or(std::cmp::Ordering::Equal)
            }) {
                *elevation_number = best.elevation_number;
                *angle = best.angle;
            }
        }
    }
}

/// One row in the elevation list UI.
#[derive(Clone, Debug)]
pub(crate) struct ElevationListEntry {
    pub elevation_number: u8,
    pub angle: f32,
    pub waveform: String,
    pub is_sails: bool,
    pub is_mrle: bool,
    /// Product names (matching `SweepDataKey` / worker strings) available at
    /// this elevation. Empty means "unknown" — skip product-availability checks.
    pub cached_products: Vec<String>,
}

/// Interpolation mode for radar rendering.
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum InterpolationMode {
    /// Raw nearest-neighbor sampling (blocky, traditional).
    #[default]
    Nearest,
    /// Bilinear interpolation between adjacent gates and azimuths.
    Bilinear,
}

impl InterpolationMode {
    pub fn label(&self) -> &'static str {
        match self {
            InterpolationMode::Nearest => "Nearest",
            InterpolationMode::Bilinear => "Bilinear",
        }
    }

    pub fn all() -> &'static [InterpolationMode] {
        &[InterpolationMode::Nearest, InterpolationMode::Bilinear]
    }
}

/// GPU rendering processing options (shader uniforms).
#[derive(Clone)]
pub struct RenderProcessing {
    /// Interpolation mode (nearest vs bilinear).
    pub interpolation: InterpolationMode,
    /// Global opacity for radar data (0.0..1.0).
    pub opacity: f32,
    /// Whether sweep animation is enabled (progressive radial reveal during playback).
    pub sweep_animation: bool,
    /// Whether data age desaturation is shown (desaturates oldest data behind sweep line).
    pub data_age_desaturation: bool,
}

impl Default for RenderProcessing {
    fn default() -> Self {
        Self {
            interpolation: InterpolationMode::Nearest,
            opacity: 1.0,
            sweep_animation: false,
            data_age_desaturation: true,
        }
    }
}

/// Lightweight storm cell info for rendering on the canvas.
#[derive(Clone, Debug)]
pub struct StormCellInfo {
    /// Reflectivity-weighted centroid latitude.
    pub lat: f64,
    /// Reflectivity-weighted centroid longitude.
    pub lon: f64,
    /// Maximum reflectivity (dBZ) anywhere in the cell.
    pub max_dbz: f32,
    /// Mean reflectivity (dBZ) across the cell's gates.
    pub mean_dbz: f32,
    /// Cell footprint area in km².
    pub area_km2: f32,
    /// Bounding box (min_lat, min_lon, max_lat, max_lon).
    pub bounds: (f64, f64, f64, f64),
    /// Compass bearing (0° = N, clockwise) from radar to centroid.
    pub bearing_from_radar_deg: f32,
    /// Great-circle-approximate distance from radar to centroid, km.
    pub range_from_radar_km: f32,
    /// Orientation of the cell's major axis in compass degrees, folded
    /// into [0, 180) since an axis is undirected.
    pub orientation_deg: f32,
    /// √(λ_major / λ_minor) from the pixel-weighted covariance. 1.0 = round.
    pub elongation: f32,
    /// Number of gates comprising the cell. Useful for debugging / further
    /// filtering.
    pub gate_count: u32,
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    use crate::data::keys::UnixMillis;
    use crate::data::ScanKey;

    fn scan_key(site: &str, ms: i64) -> ScanKey {
        ScanKey::new(site, UnixMillis(ms))
    }

    fn elev(elevation_number: u8, angle: f32) -> ElevationListEntry {
        ElevationListEntry {
            elevation_number,
            angle,
            waveform: "CS".to_string(),
            is_sails: false,
            is_mrle: false,
            cached_products: Vec::new(),
        }
    }

    // ---- RadarProduct -----------------------------------------------------

    #[wasm_bindgen_test]
    fn radar_product_label_unit_short_code() {
        assert_eq!(RadarProduct::Reflectivity.label(), "Reflectivity");
        assert_eq!(RadarProduct::Reflectivity.unit(), "dBZ");
        assert_eq!(RadarProduct::Reflectivity.short_code(), "REF");

        assert_eq!(RadarProduct::Velocity.unit(), "m/s");
        assert_eq!(RadarProduct::SpectrumWidth.unit(), "m/s");
        assert_eq!(RadarProduct::Velocity.short_code(), "VEL");
        assert_eq!(RadarProduct::SpectrumWidth.short_code(), "SW");

        // Correlation coefficient has an empty unit string.
        assert_eq!(RadarProduct::CorrelationCoefficient.unit(), "");
        assert_eq!(RadarProduct::CorrelationCoefficient.short_code(), "CC");

        assert_eq!(RadarProduct::DifferentialReflectivity.short_code(), "ZDR");
        assert_eq!(RadarProduct::DifferentialPhase.short_code(), "KDP");
        assert_eq!(RadarProduct::ClutterFilterPower.short_code(), "CFP");
        assert_eq!(RadarProduct::ClutterFilterPower.unit(), "dB");
        assert_eq!(RadarProduct::DifferentialReflectivity.unit(), "dB");
    }

    #[wasm_bindgen_test]
    fn radar_product_short_code_round_trips() {
        // Every product's short_code parses back to itself.
        // (RadarProduct has no Debug derive, so compare via PartialEq.)
        for &p in RadarProduct::all() {
            assert!(RadarProduct::from_short_code(p.short_code()) == Some(p));
        }
        // Unknown / empty / lowercase codes do not parse.
        assert!(RadarProduct::from_short_code("XYZ").is_none());
        assert!(RadarProduct::from_short_code("").is_none());
        assert!(RadarProduct::from_short_code("ref").is_none());
    }

    #[wasm_bindgen_test]
    fn radar_product_all_and_default() {
        let all = RadarProduct::all();
        assert_eq!(all.len(), 7);
        assert!(all[0] == RadarProduct::Reflectivity);
        // Default is Reflectivity.
        assert!(RadarProduct::default() == RadarProduct::Reflectivity);
    }

    #[wasm_bindgen_test]
    fn radar_product_to_worker_string_and_cfp_fallback() {
        assert_eq!(
            RadarProduct::Reflectivity.to_worker_string(),
            "reflectivity"
        );
        assert_eq!(RadarProduct::Velocity.to_worker_string(), "velocity");
        assert_eq!(
            RadarProduct::SpectrumWidth.to_worker_string(),
            "spectrum_width"
        );
        assert_eq!(
            RadarProduct::DifferentialReflectivity.to_worker_string(),
            "differential_reflectivity"
        );
        assert_eq!(
            RadarProduct::CorrelationCoefficient.to_worker_string(),
            "correlation_coefficient"
        );
        assert_eq!(
            RadarProduct::DifferentialPhase.to_worker_string(),
            "differential_phase"
        );
        // ClutterFilterPower deliberately falls back to "reflectivity".
        assert_eq!(
            RadarProduct::ClutterFilterPower.to_worker_string(),
            "reflectivity"
        );
    }

    // ---- SweepIdentity ----------------------------------------------------

    #[wasm_bindgen_test]
    fn sweep_identity_new_and_accessors() {
        // 1_700_000_000_500 ms -> 1_700_000_000.5 s
        let id = SweepIdentity::new(scan_key("KDMX", 1_700_000_000_500), 3, "velocity");
        assert_eq!(id.elevation_number, 3);
        assert_eq!(id.product, "velocity");
        assert_eq!(id.site_id(), "KDMX");
        assert!((id.scan_timestamp_secs() - 1_700_000_000.5).abs() < 1e-6);
    }

    #[wasm_bindgen_test]
    fn sweep_identity_equality_by_blob_identity() {
        let a = SweepIdentity::new(scan_key("KDMX", 1000), 1, "reflectivity");
        let b = SweepIdentity::new(scan_key("KDMX", 1000), 1, "reflectivity");
        assert_eq!(a, b);
        // Differing product, elevation, or site each break equality.
        assert_ne!(a, SweepIdentity::new(scan_key("KDMX", 1000), 1, "velocity"));
        assert_ne!(
            a,
            SweepIdentity::new(scan_key("KDMX", 1000), 2, "reflectivity")
        );
        assert_ne!(
            a,
            SweepIdentity::new(scan_key("KLOT", 1000), 1, "reflectivity")
        );
    }

    // ---- ElevationSelection ----------------------------------------------

    #[wasm_bindgen_test]
    fn elevation_selection_default_and_accessors() {
        let def = ElevationSelection::default();
        assert!(!def.is_auto());
        assert_eq!(def.elevation_number(), Some(1));
        assert!((def.angle() - 0.5).abs() < 1e-6);

        let latest = ElevationSelection::Latest;
        assert!(latest.is_auto());
        assert_eq!(latest.elevation_number(), None);
        // Latest reports the sentinel 0.5 angle.
        assert!((latest.angle() - 0.5).abs() < 1e-6);

        let fixed = ElevationSelection::Fixed {
            elevation_number: 5,
            angle: 2.4,
        };
        assert!(!fixed.is_auto());
        assert_eq!(fixed.elevation_number(), Some(5));
        assert!((fixed.angle() - 2.4).abs() < 1e-6);
    }

    #[wasm_bindgen_test]
    fn resolve_for_vcp_picks_closest_angle() {
        let mut sel = ElevationSelection::Fixed {
            elevation_number: 9,
            angle: 1.4,
        };
        // Entries with angles 0.5, 1.5, 3.0 — closest to 1.4 is 1.5 (entry #2).
        let entries = [elev(1, 0.5), elev(2, 1.5), elev(3, 3.0)];
        sel.resolve_for_vcp(&entries);
        assert_eq!(sel.elevation_number(), Some(2));
        assert!((sel.angle() - 1.5).abs() < 1e-6);
    }

    #[wasm_bindgen_test]
    fn resolve_for_vcp_noop_on_latest_and_empty() {
        // Latest is untouched.
        let mut latest = ElevationSelection::Latest;
        latest.resolve_for_vcp(&[elev(1, 0.5)]);
        assert!(latest.is_auto());

        // Empty entry list leaves a Fixed selection unchanged.
        let mut fixed = ElevationSelection::Fixed {
            elevation_number: 7,
            angle: 4.2,
        };
        fixed.resolve_for_vcp(&[]);
        assert_eq!(fixed.elevation_number(), Some(7));
        assert!((fixed.angle() - 4.2).abs() < 1e-6);
    }

    // ---- InterpolationMode / RenderProcessing / defaults ------------------

    #[wasm_bindgen_test]
    fn interpolation_mode_label_all_default() {
        assert_eq!(InterpolationMode::Nearest.label(), "Nearest");
        assert_eq!(InterpolationMode::Bilinear.label(), "Bilinear");
        let all = InterpolationMode::all();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0], InterpolationMode::Nearest);
        assert_eq!(all[1], InterpolationMode::Bilinear);
        assert_eq!(InterpolationMode::default(), InterpolationMode::Nearest);
    }

    #[wasm_bindgen_test]
    fn render_processing_default_values() {
        let rp = RenderProcessing::default();
        assert_eq!(rp.interpolation, InterpolationMode::Nearest);
        assert!((rp.opacity - 1.0).abs() < 1e-6);
        assert!(!rp.sweep_animation);
        assert!(rp.data_age_desaturation);
    }
}
