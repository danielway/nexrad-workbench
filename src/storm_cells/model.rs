//! Result types of storm cell detection.
//!
//! Plain data (`Clone + Debug + PartialEq`), no workbench types, so the module
//! can move upstream unchanged. Positions use the flat (equirectangular,
//! 111 km per degree) projection around the radar that the 2D canvas uses.

use super::DetectionParams;

/// A storm cell: the finest distinct object found by the nested-threshold
/// segmentation (see the module docs of `storm_cells`).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StormCell {
    /// Index within this result, 0 = strongest. Cells are sorted by
    /// `peak_dbz` (descending), then area. Not stable across volumes.
    pub id: u32,
    /// The group (an envelope component holding two or more cells) this cell
    /// belongs to, if any.
    pub group_id: Option<u32>,
    /// The threshold level (dBZ) whose component is this cell's footprint.
    /// A lone storm keeps its envelope (the user's level); a storm inside a
    /// group uses the level at which it became distinct from its neighbours.
    pub level_dbz: f32,
    /// Maximum of the median-filtered field inside the footprint.
    pub peak_dbz: f32,
    /// Mean of the filtered field over the footprint, in dB.
    pub mean_dbz: f32,
    /// Footprint area in km² (sum of gate footprints, so range-scaled).
    pub area_km2: f32,
    /// Number of gates in the footprint.
    pub gate_count: u32,
    /// Reflectivity-weighted centroid latitude.
    pub lat: f64,
    /// Reflectivity-weighted centroid longitude.
    pub lon: f64,
    /// Bounding box (min_lat, min_lon, max_lat, max_lon).
    pub bounds: (f64, f64, f64, f64),
    /// Compass bearing (0° = N, clockwise) from radar to centroid.
    pub bearing_from_radar_deg: f32,
    /// Distance from radar to centroid, km.
    pub range_from_radar_km: f32,
    /// Orientation of the major axis in compass degrees, in [0, 180).
    pub orientation_deg: f32,
    /// √(λ_major / λ_minor) of the weighted covariance. 1.0 = round.
    pub elongation: f32,
    /// Footprint boundary as a closed ring of (lat, lon) points (first point
    /// repeated at the end), simplified to about 1 km and at most
    /// [`MAX_OUTLINE_VERTICES`] distinct vertices. Outer boundary only.
    pub outline: Vec<(f64, f64)>,
}

/// A group: one envelope component that resolved into two or more cells
/// (a supercell line, an MCS, a hurricane's rain shield).
#[derive(Clone, Debug, PartialEq)]
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
    /// Envelope boundary, same conventions as [`StormCell::outline`].
    pub outline: Vec<(f64, f64)>,
}

/// Where a result came from: enough to tell whether it is stale.
#[derive(Clone, Debug, Default, PartialEq)]
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
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct StormCellsResult {
    /// Cells sorted strongest first; `cells[i].id == i`.
    pub cells: Vec<StormCell>,
    /// Groups sorted strongest first; `groups[i].id == i`.
    pub groups: Vec<StormGroup>,
    pub source: DetectionSource,
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

/// Upper bound on distinct vertices in an outline ring.
pub(crate) const MAX_OUTLINE_VERTICES: usize = 64;
