//! Result types of storm cell detection.
//!
//! Plain data (`Clone + Debug + PartialEq`, serde for the worker transport),
//! no workbench types, so the module can move upstream unchanged. Positions use the flat (equirectangular,
//! 111 km per degree) projection around the radar that the 2D canvas uses.

use super::DetectionParams;
use serde::{Deserialize, Serialize};

/// A storm cell: the finest distinct object found by the nested-threshold
/// segmentation (see the module docs of `storm_cells`).
///
/// Its *footprint* is its share of the envelope (the connected echo at or
/// above the user's threshold that holds it): a lone cell's footprint is
/// the whole envelope; the cells of a group split their envelope by a
/// watershed grown from each cell's core, so they tile it without overlap.
/// Every field below except `level_dbz` describes the footprint.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct StormCell {
    /// Index within this result, 0 = strongest. Cells are sorted by
    /// `peak_dbz` (descending), then area. Not stable across volumes.
    pub id: u32,
    /// The group (an envelope component holding two or more cells) this cell
    /// belongs to, if any.
    pub group_id: Option<u32>,
    /// The level (dBZ) at which the cell's core became distinct: the
    /// envelope level (the user's threshold) for a lone storm, the level
    /// that separated it from its neighbours for a storm in a group. The
    /// core at this level seeds the footprint; the footprint itself reaches
    /// down to the envelope level.
    pub level_dbz: f32,
    /// Maximum of the median-filtered field inside the footprint (it lies
    /// in the core).
    pub peak_dbz: f32,
    /// Mean of the filtered field over the whole footprint, in dB (so it
    /// includes the weaker echo around the core).
    pub mean_dbz: f32,
    /// Footprint area in km² (sum of gate footprints, so range-scaled).
    pub area_km2: f32,
    /// Number of gates in the footprint.
    pub gate_count: u32,
    /// Centroid latitude of the footprint, each gate weighted by how far it
    /// is above the envelope level (so the core pulls hardest).
    pub lat: f64,
    /// Centroid longitude, weighted as `lat`.
    pub lon: f64,
    /// Bounding box of the footprint (min_lat, min_lon, max_lat, max_lon).
    pub bounds: (f64, f64, f64, f64),
    /// Compass bearing (0° = N, clockwise) from radar to centroid.
    pub bearing_from_radar_deg: f32,
    /// Distance from radar to centroid, km.
    pub range_from_radar_km: f32,
    /// Orientation of the footprint's major axis in compass degrees, in
    /// [0, 180), from the same weighted covariance as the centroid.
    pub orientation_deg: f32,
    /// √(λ_major / λ_minor) of that covariance. 1.0 = round.
    pub elongation: f32,
    /// Footprint boundary as a closed ring of (lat, lon) points (first point
    /// repeated at the end), simplified to about 1 km with no edge longer
    /// than 10 km and at most [`MAX_OUTLINE_VERTICES`] distinct vertices.
    /// Outer boundary only. This is the footprint the area, centroid and
    /// hit test refer to; the overlay draws `core_outline` as the cell and,
    /// for a lone cell, this ring (its envelope) faintly around it. A lone
    /// cell's ring is simplified like a group's (about 2.5 km).
    pub outline: Vec<(f64, f64)>,
    /// The boundary of the cell's core, what the overlay draws at full
    /// stroke (same conventions as `outline`). For a grouped cell, the
    /// component at `level_dbz` that seeded the footprint; for a lone cell
    /// (`level_dbz` is the envelope), the component around its peak at the
    /// core level, the lowest ladder rung at or above the envelope plus the
    /// core margin (45 dBZ by default). Empty only if the peak never reaches
    /// that level, which the core minimum rules out; such a cell draws
    /// `outline` instead.
    pub core_outline: Vec<(f64, f64)>,
}

/// A group: one envelope component that resolved into two or more cells
/// (a supercell line, an MCS, a hurricane's rain shield).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct StormGroup {
    /// Index within this result, 0 = strongest (by peak, then area).
    pub id: u32,
    /// Envelope level (dBZ), the user's threshold.
    pub level_dbz: f32,
    /// Maximum of the filtered field inside the group.
    pub peak_dbz: f32,
    /// Envelope area in km².
    pub area_km2: f32,
    /// Ids of the member cells, ascending.
    pub cell_ids: Vec<u32>,
    /// Reflectivity-weighted centroid latitude of the envelope.
    pub lat: f64,
    /// Reflectivity-weighted centroid longitude of the envelope.
    pub lon: f64,
    /// Envelope boundary, same conventions as [`StormCell::outline`] but
    /// simplified more loosely (about 2.5 km): drawn as a faint ring.
    pub outline: Vec<(f64, f64)>,
}

/// Where a result came from: enough to tell whether it is stale.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct DetectionSource {
    /// Caller-supplied identity of the sweep the cells were detected on.
    pub sweep_id: Option<String>,
    /// Product name, when the caller knows it (e.g. "Reflectivity").
    pub product: Option<String>,
    /// Elevation angle of the sweep, when the caller knows it.
    pub elevation_deg: Option<f32>,
    /// The parameters the detector ran with.
    pub params: DetectionParams,
}

/// The output of one detection run.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct StormCellsResult {
    /// Cells sorted strongest first; `cells[i].id == i`.
    pub cells: Vec<StormCell>,
    /// Groups sorted strongest first; `groups[i].id == i`.
    pub groups: Vec<StormGroup>,
    pub source: DetectionSource,
    /// The cells' columns through the volume's higher tilts; `None` until
    /// the volume part has been computed (only the lowest cut seen, or a
    /// result that is not the displayed volume's).
    #[serde(default)]
    pub volume: Option<VolumeStructure>,
}

impl StormCellsResult {
    /// No cells were found (or nothing has been detected yet).
    pub(crate) fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    /// Forget all cells and groups.
    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }
}

/// The vertical structure of a result's cells (`storm_cells::volume`):
/// each cell's column sampled over its footprint on every reflectivity tilt
/// of the volume that was on hand.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct VolumeStructure {
    /// The tilts sampled, lowest angle first; the lowest cut is the first.
    pub tilts: Vec<VolumeTilt>,
    /// One column per cell, `cells[i]` for cell id `i`.
    pub cells: Vec<CellColumn>,
    /// Every tilt of the volume's plan was sampled. False while a live
    /// volume is still arriving or only some cuts are on device: the
    /// columns are then provisional (the top may be higher).
    pub complete: bool,
}

/// One sampled tilt.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct VolumeTilt {
    pub elevation_number: u8,
    /// Measured angle, degrees.
    pub elevation_deg: f32,
}

/// A cell's column. Heights are of the beam centre above the radar, in km
/// (4/3 earth radius model).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct CellColumn {
    /// Highest sample at or above 18 dBZ over the footprint, from tilts with
    /// at least three such gates. `None` when no tilt qualifies.
    pub echo_top_km: Option<f32>,
    /// The highest tilt sampled still has 18 dBZ echo over the footprint:
    /// the real top is above `echo_top_km`.
    pub echo_top_capped: bool,
    /// Strongest echo in the column (composite peak), after a three-gate
    /// median along each radial.
    pub max_dbz: Option<f32>,
    /// Height of `max_dbz`.
    pub max_dbz_height_km: Option<f32>,
    /// Strongest echo per tilt, parallel to [`VolumeStructure::tilts`];
    /// `None` where the tilt has no echo over the footprint.
    pub profile: Vec<Option<f32>>,
}

/// Upper bound on distinct vertices in an outline ring. Generous: the
/// ~1 km tolerance and the 10 km edge bound decide the count; this only
/// keeps a pathological ring from growing without limit.
pub(crate) const MAX_OUTLINE_VERTICES: usize = 512;
