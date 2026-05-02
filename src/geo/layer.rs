//! Geographic layer data structures.

use super::{MapProjection, ProjectionFingerprint};
use eframe::egui::{Align2, Color32, Pos2, Vec2};
use eframe::epaint::Galley;
use geo_types::Coord;
use shapefile::dbase::FieldValue;
use std::cell::RefCell;
use std::io::Cursor;
use std::sync::Arc;

/// Type of geographic layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum GeoLayerType {
    States,
    Counties,
    Cities,
    Highways,
    Lakes,
}

impl GeoLayerType {
    /// Returns the default color for this layer type.
    pub fn default_color(&self) -> Color32 {
        match self {
            GeoLayerType::States => Color32::from_rgb(100, 100, 120),
            GeoLayerType::Counties => Color32::from_rgb(70, 70, 90),
            GeoLayerType::Cities => Color32::from_rgb(180, 180, 200),
            GeoLayerType::Highways => Color32::from_rgb(100, 80, 60),
            GeoLayerType::Lakes => Color32::from_rgb(60, 80, 120),
        }
    }

    /// Returns the default line width for this layer type.
    pub fn default_line_width(&self) -> f32 {
        match self {
            GeoLayerType::States => 1.5,
            GeoLayerType::Counties => 0.8,
            GeoLayerType::Cities => 0.0, // Points, not lines
            GeoLayerType::Highways => 1.0,
            GeoLayerType::Lakes => 0.8,
        }
    }

    /// Minimum zoom level at which this layer becomes visible.
    pub fn min_zoom(&self) -> f32 {
        match self {
            GeoLayerType::States => 0.0,
            GeoLayerType::Counties => 1.5,
            GeoLayerType::Cities => 0.0,
            GeoLayerType::Highways => 1.0,
            GeoLayerType::Lakes => 0.5,
        }
    }

    /// Minimum zoom level at which labels for this layer become visible.
    pub fn min_label_zoom(&self) -> f32 {
        match self {
            GeoLayerType::States => 0.0,
            GeoLayerType::Counties => 3.0,
            GeoLayerType::Cities => 0.0,
            GeoLayerType::Highways => 2.0,
            GeoLayerType::Lakes => 2.0,
        }
    }
}

/// A geographic feature that can be rendered.
#[derive(Debug, Clone)]
pub enum GeoFeature {
    /// A series of connected line segments (for boundaries, rivers, etc.)
    LineString(Vec<Coord<f64>>),
    /// Multiple line strings (for complex boundaries)
    MultiLineString(Vec<Vec<Coord<f64>>>),
    /// A closed polygon with optional label.
    ///
    /// `label_anchor` is the precomputed centroid (in lat/lon) used as the
    /// label anchor point — calculated once at load time so the label
    /// renderer can skip the per-frame shoelace pass.
    Polygon {
        exterior: Vec<Coord<f64>>,
        #[allow(dead_code)]
        holes: Vec<Vec<Coord<f64>>>,
        label: Option<String>,
        label_anchor: Coord<f64>,
    },
    /// Multiple polygons with optional label.
    ///
    /// `label_anchor` is the precomputed centroid of the largest polygon
    /// (by bounding-box area), matching the label-placement choice at
    /// render time. Computed once at load.
    MultiPolygon {
        #[allow(clippy::type_complexity)]
        polygons: Vec<(Vec<Coord<f64>>, Vec<Vec<Coord<f64>>>)>,
        label: Option<String>,
        label_anchor: Coord<f64>,
    },
    /// A single point (for cities, landmarks)
    Point(Coord<f64>, Option<String>),
}

/// Computes the true geometric centroid of a polygon using the shoelace formula.
/// Falls back to vertex average for degenerate (≤2 verts or zero area) polygons.
pub fn compute_polygon_centroid(coords: &[Coord<f64>]) -> Coord<f64> {
    if coords.is_empty() {
        return Coord { x: 0.0, y: 0.0 };
    }
    if coords.len() < 3 {
        let (sum_x, sum_y) = coords
            .iter()
            .fold((0.0, 0.0), |(sx, sy), c| (sx + c.x, sy + c.y));
        return Coord {
            x: sum_x / coords.len() as f64,
            y: sum_y / coords.len() as f64,
        };
    }

    let mut signed_area = 0.0;
    let mut cx = 0.0;
    let mut cy = 0.0;

    for i in 0..coords.len() {
        let j = (i + 1) % coords.len();
        let cross = coords[i].x * coords[j].y - coords[j].x * coords[i].y;
        signed_area += cross;
        cx += (coords[i].x + coords[j].x) * cross;
        cy += (coords[i].y + coords[j].y) * cross;
    }

    signed_area *= 0.5;

    if signed_area.abs() < 1e-10 {
        let (sum_x, sum_y) = coords
            .iter()
            .fold((0.0, 0.0), |(sx, sy), c| (sx + c.x, sy + c.y));
        return Coord {
            x: sum_x / coords.len() as f64,
            y: sum_y / coords.len() as f64,
        };
    }

    Coord {
        x: cx / (6.0 * signed_area),
        y: cy / (6.0 * signed_area),
    }
}

/// Bounding-box area of a polygon. Used to pick the label-bearing ring of
/// a `MultiPolygon`.
fn polygon_bbox_area(coords: &[Coord<f64>]) -> f64 {
    if coords.is_empty() {
        return 0.0;
    }
    let (min_x, max_x, min_y, max_y) = coords.iter().fold(
        (f64::MAX, f64::MIN, f64::MAX, f64::MIN),
        |(mn_x, mx_x, mn_y, mx_y), c| (mn_x.min(c.x), mx_x.max(c.x), mn_y.min(c.y), mx_y.max(c.y)),
    );
    (max_x - min_x) * (max_y - min_y)
}

/// A geographic layer containing multiple features.
#[derive(Debug, Clone)]
pub struct GeoLayer {
    /// Type of this layer
    pub layer_type: GeoLayerType,
    /// Features in this layer
    pub features: Vec<GeoFeature>,
    /// Override color (None = use default)
    pub color: Option<Color32>,
    /// Override line width (None = use default)
    pub line_width: Option<f32>,
    /// Whether this layer is visible
    pub visible: bool,
    /// Per-frame cache of projected screen points, parallel to `features`.
    ///
    /// Rebuilt whenever the [`MapProjection`] fingerprint changes.
    /// Invisible (idle) views hit the cache every frame and skip all
    /// trig on feature coords.
    cache: RefCell<LayerProjectionCache>,
    /// Cache of laid-out label galleys + their lat/lon anchors. Rebuilt
    /// only when the camera has settled and the label-cache token (zoom
    /// bucket, theme) has changed. Per-frame label rendering reprojects
    /// the cached anchors and reuses the cached galleys with halo offsets,
    /// avoiding text layout on every frame.
    label_cache: RefCell<LayerLabelCache>,
}

/// Single retained label, ready to paint at the projected position of its
/// `anchor`. Built once per camera-settle event; reused every frame in
/// between.
#[derive(Debug, Clone)]
pub(crate) struct LabelEntry {
    /// Lat/lon anchor — projected each frame to obtain the screen position
    /// at which the label is drawn.
    pub anchor: Coord<f64>,
    /// How the galley should be aligned around the projected anchor.
    pub align: Align2,
    /// Pixel offset from the anchor's screen position before alignment.
    /// Used by point labels that sit a few pixels to the right of their
    /// marker dot.
    pub pixel_offset: Vec2,
    /// Pre-laid-out text. Reused every frame — `Arc::clone` is cheap.
    pub galley: Arc<Galley>,
    /// Foreground text color (halo offsets paint in black).
    pub color: Color32,
}

/// Inputs that, when changed, force a label-cache rebuild on next settle:
/// font sizes scale with zoom, and label colors flip with theme. The
/// projection fingerprint is *not* part of the token because pan/zoom
/// within a bucket should reuse the same galleys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct LabelCacheToken {
    /// `(zoom * 4.0).round() as u16` — quarter-zoom resolution. Coarse
    /// enough to absorb tiny floating-point jitter, fine enough that font
    /// sizes update visibly during sustained zoom.
    pub zoom_bucket: u16,
    pub dark: bool,
    pub show_labels: bool,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct LayerLabelCache {
    pub token: Option<LabelCacheToken>,
    pub entries: Vec<LabelEntry>,
}

/// Cached screen-space projection of a single feature's line/ring
/// coordinates. Points are stored in the same order as the source
/// coordinate sequence so downstream rendering can stream segments
/// directly.
#[derive(Debug, Clone, Default)]
pub(crate) enum FeatureProjection {
    /// No line/ring coords (e.g. `Point` features).
    #[default]
    Empty,
    /// A single coord sequence: `LineString`, `Polygon.exterior`.
    Single(Vec<Pos2>),
    /// Multiple coord sequences: `MultiLineString`, `MultiPolygon`.
    Multi(Vec<Vec<Pos2>>),
}

#[derive(Debug, Clone, Default)]
pub(crate) struct LayerProjectionCache {
    fingerprint: Option<ProjectionFingerprint>,
    /// Parallel to [`GeoLayer::features`].
    entries: Vec<FeatureProjection>,
}

fn project_line(coords: &[Coord<f64>], projection: &MapProjection) -> Vec<Pos2> {
    coords
        .iter()
        .map(|c| projection.geo_to_screen(*c))
        .collect()
}

fn project_feature(feature: &GeoFeature, projection: &MapProjection) -> FeatureProjection {
    match feature {
        GeoFeature::Point(_, _) => FeatureProjection::Empty,
        GeoFeature::LineString(coords) => {
            FeatureProjection::Single(project_line(coords, projection))
        }
        GeoFeature::MultiLineString(lines) => {
            FeatureProjection::Multi(lines.iter().map(|l| project_line(l, projection)).collect())
        }
        GeoFeature::Polygon { exterior, .. } => {
            FeatureProjection::Single(project_line(exterior, projection))
        }
        GeoFeature::MultiPolygon { polygons, .. } => FeatureProjection::Multi(
            polygons
                .iter()
                .map(|(ext, _)| project_line(ext, projection))
                .collect(),
        ),
    }
}

impl GeoFeature {
    /// Returns the lat/lon point at which a label for this feature should
    /// be anchored, if it carries a label. Polygons return their precomputed
    /// centroid; points return their location.
    pub fn label_anchor(&self) -> Option<Coord<f64>> {
        match self {
            GeoFeature::Polygon {
                label: Some(_),
                label_anchor,
                ..
            } => Some(*label_anchor),
            GeoFeature::MultiPolygon {
                label: Some(_),
                label_anchor,
                ..
            } => Some(*label_anchor),
            GeoFeature::Point(coord, Some(_)) => Some(*coord),
            _ => None,
        }
    }

    /// The label text, if any.
    pub fn label_text(&self) -> Option<&str> {
        match self {
            GeoFeature::Polygon { label: Some(s), .. }
            | GeoFeature::MultiPolygon { label: Some(s), .. }
            | GeoFeature::Point(_, Some(s)) => Some(s.as_str()),
            _ => None,
        }
    }
}

impl GeoLayer {
    /// Creates a new empty layer of the specified type.
    pub fn new(layer_type: GeoLayerType) -> Self {
        Self {
            layer_type,
            features: Vec::new(),
            color: None,
            line_width: None,
            visible: true,
            cache: RefCell::new(LayerProjectionCache::default()),
            label_cache: RefCell::new(LayerLabelCache::default()),
        }
    }

    /// Borrow the label cache mutably for rebuild.
    pub(crate) fn label_cache_mut(&self) -> std::cell::RefMut<'_, LayerLabelCache> {
        self.label_cache.borrow_mut()
    }

    /// Borrow the label cache for paint.
    pub(crate) fn label_cache(&self) -> std::cell::Ref<'_, LayerLabelCache> {
        self.label_cache.borrow()
    }

    /// Ensures the cache of projected screen points matches the current
    /// projection, reprojecting only on fingerprint change. Returns a
    /// clone-free handle for reading cached entries.
    ///
    /// The cache is keyed on the projection fingerprint rather than any
    /// individual parameter, so any combination of zoom/pan/resize
    /// flushes it and any idle frame skips reprojection entirely.
    pub(crate) fn refresh_projection_cache(&self, projection: &MapProjection) {
        let mut cache = self.cache.borrow_mut();
        let fp = projection.fingerprint();
        if cache.fingerprint == Some(fp) && cache.entries.len() == self.features.len() {
            return;
        }

        cache.entries.clear();
        cache.entries.reserve(self.features.len());
        for feature in &self.features {
            cache.entries.push(project_feature(feature, projection));
        }
        cache.fingerprint = Some(fp);
    }

    pub(crate) fn cached_entries(&self) -> std::cell::Ref<'_, [FeatureProjection]> {
        std::cell::Ref::map(self.cache.borrow(), |c| c.entries.as_slice())
    }

    /// Returns the effective color for this layer.
    pub fn effective_color(&self) -> Color32 {
        self.color
            .unwrap_or_else(|| self.layer_type.default_color())
    }

    /// Returns the effective line width for this layer.
    pub fn effective_line_width(&self) -> f32 {
        self.line_width
            .unwrap_or_else(|| self.layer_type.default_line_width())
    }

    /// Loads features from a shapefile (.shp and .dbf bytes).
    pub fn load_from_shapefile(
        &mut self,
        shp_bytes: &[u8],
        dbf_bytes: Option<&[u8]>,
    ) -> Result<(), String> {
        let shp_cursor = Cursor::new(shp_bytes);
        let mut shape_reader = shapefile::ShapeReader::new(shp_cursor)
            .map_err(|e| format!("Failed to read shapefile: {}", e))?;

        // Load dbf records if available (for getting names/labels)
        let dbf_records: Option<Vec<shapefile::dbase::Record>> = dbf_bytes.and_then(|bytes| {
            let dbf_cursor = Cursor::new(bytes);
            shapefile::dbase::Reader::new(dbf_cursor)
                .ok()
                .and_then(|mut r: shapefile::dbase::Reader<Cursor<&[u8]>>| r.read().ok())
        });

        for (idx, result) in shape_reader.iter_shapes().enumerate() {
            let shape: shapefile::Shape =
                result.map_err(|e| format!("Failed to read shape: {}", e))?;

            // Try to get a name from the dbf record
            let label = dbf_records.as_ref().and_then(|records| {
                records
                    .get(idx)
                    .and_then(|record: &shapefile::dbase::Record| {
                        for field_name in &["NAME", "name", "Name", "NAMELSAD", "FULLNAME"] {
                            if let Some(FieldValue::Character(Some(s))) = record.get(field_name) {
                                return Some(s.trim().to_string());
                            }
                        }
                        None
                    })
            });

            if let Some(feature) = convert_shapefile_shape(&shape, label) {
                self.features.push(feature);
            }
        }

        Ok(())
    }
}

fn convert_shapefile_shape(shape: &shapefile::Shape, label: Option<String>) -> Option<GeoFeature> {
    match shape {
        shapefile::Shape::Point(p) => {
            let coord = Coord { x: p.x, y: p.y };
            Some(GeoFeature::Point(coord, label))
        }
        shapefile::Shape::Polyline(pl) => {
            let parts = pl.parts();
            if parts.len() == 1 {
                let coords: Vec<Coord<f64>> =
                    parts[0].iter().map(|p| Coord { x: p.x, y: p.y }).collect();
                Some(GeoFeature::LineString(coords))
            } else {
                let lines: Vec<Vec<Coord<f64>>> = parts
                    .iter()
                    .map(|part: &Vec<shapefile::Point>| {
                        part.iter().map(|p| Coord { x: p.x, y: p.y }).collect()
                    })
                    .collect();
                Some(GeoFeature::MultiLineString(lines))
            }
        }
        shapefile::Shape::Polygon(poly) => {
            use shapefile::PolygonRing;

            let mut outer_rings: Vec<Vec<Coord<f64>>> = Vec::new();
            let mut current_holes: Vec<Vec<Coord<f64>>> = Vec::new();

            for ring in poly.rings() {
                let coords: Vec<Coord<f64>> = ring
                    .points()
                    .iter()
                    .map(|p| Coord { x: p.x, y: p.y })
                    .collect();

                match ring {
                    PolygonRing::Outer(_) => {
                        outer_rings.push(coords);
                    }
                    PolygonRing::Inner(_) => {
                        current_holes.push(coords);
                    }
                }
            }

            if outer_rings.is_empty() {
                return None;
            }

            if outer_rings.len() == 1 {
                let exterior = outer_rings.remove(0);
                let label_anchor = compute_polygon_centroid(&exterior);
                Some(GeoFeature::Polygon {
                    exterior,
                    holes: current_holes,
                    label,
                    label_anchor,
                })
            } else {
                #[allow(clippy::type_complexity)]
                let polygons: Vec<(Vec<Coord<f64>>, Vec<Vec<Coord<f64>>>)> = outer_rings
                    .into_iter()
                    .map(|ext| (ext, Vec::new()))
                    .collect();
                let label_anchor = polygons
                    .iter()
                    .max_by(|(a, _), (b, _)| {
                        polygon_bbox_area(a)
                            .partial_cmp(&polygon_bbox_area(b))
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .map(|(ext, _)| compute_polygon_centroid(ext))
                    .unwrap_or(Coord { x: 0.0, y: 0.0 });
                Some(GeoFeature::MultiPolygon {
                    polygons,
                    label,
                    label_anchor,
                })
            }
        }
        shapefile::Shape::NullShape => None,
        _ => None,
    }
}

/// Collection of all geographic layers.
#[derive(Debug, Clone, Default)]
pub struct GeoLayerSet {
    pub states: Option<GeoLayer>,
    pub counties: Option<GeoLayer>,
    pub cities: Option<GeoLayer>,
    pub highways: Option<GeoLayer>,
    pub lakes: Option<GeoLayer>,
}

impl GeoLayerSet {
    /// Creates a new empty layer set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Loads a layer from shapefile bytes.
    pub fn load_layer_from_shapefile(
        &mut self,
        layer_type: GeoLayerType,
        shp_bytes: &[u8],
        dbf_bytes: Option<&[u8]>,
    ) -> Result<(), String> {
        let mut layer = GeoLayer::new(layer_type);
        layer.load_from_shapefile(shp_bytes, dbf_bytes)?;
        match layer_type {
            GeoLayerType::States => self.states = Some(layer),
            GeoLayerType::Counties => self.counties = Some(layer),
            GeoLayerType::Cities => self.cities = Some(layer),
            GeoLayerType::Highways => self.highways = Some(layer),
            GeoLayerType::Lakes => self.lakes = Some(layer),
        }
        Ok(())
    }

    /// Load a pre-built layer directly.
    pub fn set_layer(&mut self, layer: GeoLayer) {
        match layer.layer_type {
            GeoLayerType::States => self.states = Some(layer),
            GeoLayerType::Counties => self.counties = Some(layer),
            GeoLayerType::Cities => self.cities = Some(layer),
            GeoLayerType::Highways => self.highways = Some(layer),
            GeoLayerType::Lakes => self.lakes = Some(layer),
        }
    }
}
