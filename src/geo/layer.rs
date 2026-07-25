//! Geographic layer data structures.

use super::{MapProjection, ProjectionFingerprint};
use eframe::egui::{Align2, Color32, Pos2, Vec2};
use geo_types::Coord;
use shapefile::dbase::FieldValue;
use std::cell::RefCell;
use std::io::Cursor;

/// Visibility settings for geographic map layers.
#[derive(Clone)]
pub(crate) struct GeoLayerVisibility {
    /// Show state/province boundaries
    pub states: bool,
    /// Show county boundaries (auto-hidden at low zoom)
    pub counties: bool,
    /// Show labels for geographic features
    pub labels: bool,
    /// Show NEXRAD radar sites (other sites, not current)
    pub nexrad_sites: bool,
    /// Show major cities
    pub cities: bool,
    /// Show major highways
    pub highways: bool,
    /// Show lakes and water bodies
    pub lakes: bool,
    /// Show the national radar mosaic overlay (CONUS composite)
    pub national_mosaic: bool,
    /// Show NWS warning polygons (the urgent, storm-based alerts)
    pub alerts_warnings: bool,
    /// Show NWS watch/advisory/statement polygons (everything that isn't a warning)
    pub alerts_other: bool,
    /// Show mPING crowd-sourced storm reports
    pub mping: bool,
    /// Show the user's current GPS location as a dot on the map.
    /// Per-session only — not persisted to UserPreferences.
    pub gps_location: bool,
}

impl Default for GeoLayerVisibility {
    fn default() -> Self {
        Self {
            states: true,
            counties: true,
            labels: true,
            nexrad_sites: false,
            cities: true,
            highways: false,
            lakes: false,
            national_mosaic: false,
            alerts_warnings: true,
            alerts_other: false,
            mping: false,
            gps_location: false,
        }
    }
}

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
pub(crate) fn compute_polygon_centroid(coords: &[Coord<f64>]) -> Coord<f64> {
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
/// `anchor`. The feature-selection pass (which features get a label, at what
/// size) is debounced on camera-settle; the galley itself is laid out fresh
/// each frame from `text`/`font_size` via egui's internal galley cache, which
/// — unlike a stored `Arc<Galley>` — is invalidated when egui recreates the
/// font atlas (it does so as the atlas fills during load). Storing a galley
/// here instead left it pointing at stale glyph positions after a repack,
/// rendering garbled until the next zoom forced a rebuild.
#[derive(Debug, Clone)]
pub(crate) struct LabelEntry {
    /// Lat/lon anchor — projected each frame to obtain the screen position
    /// at which the label is drawn.
    pub anchor: Coord<f64>,
    /// How the label should be aligned around the projected anchor.
    pub align: Align2,
    /// Pixel offset from the anchor's screen position before alignment.
    /// Used by point labels that sit a few pixels to the right of their
    /// marker dot.
    pub pixel_offset: Vec2,
    /// Label text, laid out each frame (cheap — memoized by egui).
    pub text: String,
    /// Proportional font size, chosen at selection time from layer + zoom.
    pub font_size: f32,
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

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn c(x: f64, y: f64) -> Coord<f64> {
        Coord { x, y }
    }

    /// The shoelace centroid of a unit square is its geometric center (0.5, 0.5)
    /// — and for a symmetric square this also equals the vertex average, so this
    /// pins the happy path.
    #[wasm_bindgen_test]
    fn centroid_unit_square() {
        let sq = vec![c(0.0, 0.0), c(1.0, 0.0), c(1.0, 1.0), c(0.0, 1.0)];
        let centroid = compute_polygon_centroid(&sq);
        assert!((centroid.x - 0.5).abs() < 1e-9, "x {}", centroid.x);
        assert!((centroid.y - 0.5).abs() < 1e-9, "y {}", centroid.y);
    }

    /// An L-shape's true area centroid differs from the naive vertex average —
    /// this proves the shoelace path is taken, not a fallback. Hand-computed:
    /// the L (2x2 square minus its top-right 1x1 quadrant) has area centroid
    /// (5/6, 5/6) — derived by composition: 2x2 square (area 4, centroid (1,1))
    /// minus the top-right 1x1 (area 1, centroid (1.5, 1.5)) → (4·1 − 1.5)/3 =
    /// 5/6. Its 6-vertex average is (1.0, 1.0), so the two are distinct.
    #[wasm_bindgen_test]
    fn centroid_l_shape_uses_shoelace_not_vertex_average() {
        // Vertices CCW: (0,0)(2,0)(2,1)(1,1)(1,2)(0,2).
        let l = vec![
            c(0.0, 0.0),
            c(2.0, 0.0),
            c(2.0, 1.0),
            c(1.0, 1.0),
            c(1.0, 2.0),
            c(0.0, 2.0),
        ];
        let centroid = compute_polygon_centroid(&l);
        assert!((centroid.x - 5.0 / 6.0).abs() < 1e-9, "x {}", centroid.x);
        assert!((centroid.y - 5.0 / 6.0).abs() < 1e-9, "y {}", centroid.y);
        // Vertex average is (1.0, 1.0) — distinctly different from the area
        // centroid, confirming the shoelace formula (not the fallback) ran.
        let xs = [0.0_f64, 2.0, 2.0, 1.0, 1.0, 0.0];
        let avg_x = xs.iter().sum::<f64>() / xs.len() as f64;
        assert!((avg_x - 1.0).abs() < 1e-9, "avg_x {avg_x}");
        assert!((centroid.x - avg_x).abs() > 1e-3);
    }

    /// A 2-vertex input is degenerate (<3 verts) → vertex-average fallback.
    #[wasm_bindgen_test]
    fn centroid_two_points_falls_back_to_average() {
        let pts = vec![c(0.0, 0.0), c(4.0, 2.0)];
        let centroid = compute_polygon_centroid(&pts);
        assert!((centroid.x - 2.0).abs() < 1e-9);
        assert!((centroid.y - 1.0).abs() < 1e-9);
    }

    /// A collinear (zero-area) triple falls back to the vertex average without
    /// producing NaN from the divide-by-(6*area).
    #[wasm_bindgen_test]
    fn centroid_collinear_falls_back_without_nan() {
        let line = vec![c(0.0, 0.0), c(2.0, 0.0), c(4.0, 0.0)];
        let centroid = compute_polygon_centroid(&line);
        assert!(centroid.x.is_finite() && centroid.y.is_finite());
        // Vertex average of the three collinear points.
        assert!((centroid.x - 2.0).abs() < 1e-9);
        assert!((centroid.y - 0.0).abs() < 1e-9);
    }

    /// Empty input yields the origin (documented degenerate case).
    #[wasm_bindgen_test]
    fn centroid_empty_is_origin() {
        let centroid = compute_polygon_centroid(&[]);
        assert_eq!(centroid.x, 0.0);
        assert_eq!(centroid.y, 0.0);
    }

    /// `polygon_bbox_area` is 0 for empty input and width*height otherwise.
    #[wasm_bindgen_test]
    fn bbox_area_empty_and_rectangle() {
        assert_eq!(polygon_bbox_area(&[]), 0.0);
        // Bounding box spans x in [1,4] (w=3), y in [2,7] (h=5) → area 15.
        let pts = vec![c(1.0, 2.0), c(4.0, 5.0), c(2.0, 7.0)];
        assert!((polygon_bbox_area(&pts) - 15.0).abs() < 1e-9);
    }
}

/// Collection of all geographic layers.
#[derive(Debug, Clone, Default)]
pub(crate) struct GeoLayerSet {
    pub states: Option<GeoLayer>,
    pub counties: Option<GeoLayer>,
    pub cities: Option<GeoLayer>,
    pub highways: Option<GeoLayer>,
    pub lakes: Option<GeoLayer>,
}

impl GeoLayerSet {
    /// Creates a new empty layer set.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Loads a layer from shapefile bytes.
    pub(crate) fn load_layer_from_shapefile(
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
    pub(crate) fn set_layer(&mut self, layer: GeoLayer) {
        match layer.layer_type {
            GeoLayerType::States => self.states = Some(layer),
            GeoLayerType::Counties => self.counties = Some(layer),
            GeoLayerType::Cities => self.cities = Some(layer),
            GeoLayerType::Highways => self.highways = Some(layer),
            GeoLayerType::Lakes => self.lakes = Some(layer),
        }
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use eframe::egui::{Align2, Rect};
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn default_geo_visibility_matches_product_defaults() {
        // Pins the out-of-the-box overlay set: base geography + warnings on,
        // opt-in/heavier overlays off.
        let v = GeoLayerVisibility::default();
        assert!(v.states);
        assert!(v.counties);
        assert!(v.labels);
        assert!(v.cities);
        assert!(v.alerts_warnings);

        assert!(!v.nexrad_sites);
        assert!(!v.highways);
        assert!(!v.lakes);
        assert!(!v.national_mosaic);
        assert!(!v.alerts_other);
        assert!(!v.mping);
        assert!(!v.gps_location);
    }

    fn c(x: f64, y: f64) -> Coord<f64> {
        Coord { x, y }
    }

    fn test_proj() -> MapProjection {
        let mut p = MapProjection::new(39.0, -98.0);
        p.update(
            1.0,
            Vec2::ZERO,
            Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0)),
        );
        p
    }

    // ----- GeoLayerType::default_color -----

    #[wasm_bindgen_test]
    fn default_colors_are_distinct_per_type() {
        assert_eq!(
            GeoLayerType::States.default_color(),
            Color32::from_rgb(100, 100, 120)
        );
        assert_eq!(
            GeoLayerType::Counties.default_color(),
            Color32::from_rgb(70, 70, 90)
        );
        assert_eq!(
            GeoLayerType::Cities.default_color(),
            Color32::from_rgb(180, 180, 200)
        );
        assert_eq!(
            GeoLayerType::Highways.default_color(),
            Color32::from_rgb(100, 80, 60)
        );
        assert_eq!(
            GeoLayerType::Lakes.default_color(),
            Color32::from_rgb(60, 80, 120)
        );
    }

    // ----- GeoLayerType::default_line_width -----

    #[wasm_bindgen_test]
    fn default_line_widths() {
        assert!((GeoLayerType::States.default_line_width() - 1.5).abs() < 1e-6);
        assert!((GeoLayerType::Counties.default_line_width() - 0.8).abs() < 1e-6);
        // Cities are points, so line width is zero.
        assert!((GeoLayerType::Cities.default_line_width() - 0.0).abs() < 1e-6);
        assert!((GeoLayerType::Highways.default_line_width() - 1.0).abs() < 1e-6);
        assert!((GeoLayerType::Lakes.default_line_width() - 0.8).abs() < 1e-6);
    }

    // ----- GeoLayerType::min_zoom -----

    #[wasm_bindgen_test]
    fn min_zoom_values() {
        assert!((GeoLayerType::States.min_zoom() - 0.0).abs() < 1e-6);
        assert!((GeoLayerType::Counties.min_zoom() - 1.5).abs() < 1e-6);
        assert!((GeoLayerType::Cities.min_zoom() - 0.0).abs() < 1e-6);
        assert!((GeoLayerType::Highways.min_zoom() - 1.0).abs() < 1e-6);
        assert!((GeoLayerType::Lakes.min_zoom() - 0.5).abs() < 1e-6);
    }

    // ----- GeoLayerType::min_label_zoom -----

    #[wasm_bindgen_test]
    fn min_label_zoom_values() {
        assert!((GeoLayerType::States.min_label_zoom() - 0.0).abs() < 1e-6);
        assert!((GeoLayerType::Counties.min_label_zoom() - 3.0).abs() < 1e-6);
        assert!((GeoLayerType::Cities.min_label_zoom() - 0.0).abs() < 1e-6);
        assert!((GeoLayerType::Highways.min_label_zoom() - 2.0).abs() < 1e-6);
        assert!((GeoLayerType::Lakes.min_label_zoom() - 2.0).abs() < 1e-6);
    }

    // ----- GeoFeature::label_anchor -----

    #[wasm_bindgen_test]
    fn label_anchor_polygon_with_label_returns_anchor() {
        let f = GeoFeature::Polygon {
            exterior: vec![c(0.0, 0.0), c(2.0, 0.0), c(2.0, 2.0), c(0.0, 2.0)],
            holes: vec![],
            label: Some("Region".to_string()),
            label_anchor: c(1.0, 1.0),
        };
        let anchor = f.label_anchor().expect("labeled polygon has anchor");
        assert!((anchor.x - 1.0).abs() < 1e-9);
        assert!((anchor.y - 1.0).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn label_anchor_polygon_without_label_is_none() {
        let f = GeoFeature::Polygon {
            exterior: vec![c(0.0, 0.0), c(1.0, 0.0), c(1.0, 1.0)],
            holes: vec![],
            label: None,
            label_anchor: c(0.5, 0.5),
        };
        assert!(f.label_anchor().is_none());
    }

    #[wasm_bindgen_test]
    fn label_anchor_multipolygon_with_label() {
        let f = GeoFeature::MultiPolygon {
            polygons: vec![(vec![c(0.0, 0.0), c(1.0, 0.0), c(1.0, 1.0)], vec![])],
            label: Some("Multi".to_string()),
            label_anchor: c(3.0, 4.0),
        };
        let anchor = f.label_anchor().expect("labeled multipolygon has anchor");
        assert!((anchor.x - 3.0).abs() < 1e-9);
        assert!((anchor.y - 4.0).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn label_anchor_point_with_label_returns_point() {
        let f = GeoFeature::Point(c(-98.0, 39.0), Some("City".to_string()));
        let anchor = f.label_anchor().expect("labeled point has anchor");
        assert!((anchor.x + 98.0).abs() < 1e-9);
        assert!((anchor.y - 39.0).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn label_anchor_point_without_label_is_none() {
        let f = GeoFeature::Point(c(1.0, 2.0), None);
        assert!(f.label_anchor().is_none());
    }

    #[wasm_bindgen_test]
    fn label_anchor_linestring_is_none() {
        let f = GeoFeature::LineString(vec![c(0.0, 0.0), c(1.0, 1.0)]);
        assert!(f.label_anchor().is_none());
    }

    // ----- GeoFeature::label_text -----

    #[wasm_bindgen_test]
    fn label_text_variants() {
        let poly = GeoFeature::Polygon {
            exterior: vec![c(0.0, 0.0)],
            holes: vec![],
            label: Some("Poly".to_string()),
            label_anchor: c(0.0, 0.0),
        };
        assert_eq!(poly.label_text(), Some("Poly"));

        let point = GeoFeature::Point(c(0.0, 0.0), Some("Pt".to_string()));
        assert_eq!(point.label_text(), Some("Pt"));

        let multi = GeoFeature::MultiPolygon {
            polygons: vec![],
            label: Some("M".to_string()),
            label_anchor: c(0.0, 0.0),
        };
        assert_eq!(multi.label_text(), Some("M"));

        let unlabeled = GeoFeature::Point(c(0.0, 0.0), None);
        assert_eq!(unlabeled.label_text(), None);

        let line = GeoFeature::MultiLineString(vec![vec![c(0.0, 0.0)]]);
        assert_eq!(line.label_text(), None);
    }

    // ----- GeoLayer::new + effective_color / effective_line_width -----

    #[wasm_bindgen_test]
    fn new_layer_defaults() {
        let layer = GeoLayer::new(GeoLayerType::Counties);
        assert_eq!(layer.layer_type, GeoLayerType::Counties);
        assert!(layer.features.is_empty());
        assert!(layer.color.is_none());
        assert!(layer.line_width.is_none());
        assert!(layer.visible);
    }

    #[wasm_bindgen_test]
    fn effective_color_falls_back_to_default() {
        let layer = GeoLayer::new(GeoLayerType::Highways);
        assert_eq!(layer.effective_color(), Color32::from_rgb(100, 80, 60));
    }

    #[wasm_bindgen_test]
    fn effective_color_uses_override() {
        let mut layer = GeoLayer::new(GeoLayerType::Highways);
        let custom = Color32::from_rgb(1, 2, 3);
        layer.color = Some(custom);
        assert_eq!(layer.effective_color(), custom);
    }

    #[wasm_bindgen_test]
    fn effective_line_width_default_and_override() {
        let mut layer = GeoLayer::new(GeoLayerType::States);
        assert!((layer.effective_line_width() - 1.5).abs() < 1e-6);
        layer.line_width = Some(7.25);
        assert!((layer.effective_line_width() - 7.25).abs() < 1e-6);
    }

    // ----- project_feature / refresh_projection_cache / cached_entries -----

    #[wasm_bindgen_test]
    fn point_feature_projects_to_empty() {
        let proj = test_proj();
        let pf = project_feature(&GeoFeature::Point(c(-98.0, 39.0), None), &proj);
        assert!(matches!(pf, FeatureProjection::Empty));
    }

    #[wasm_bindgen_test]
    fn linestring_feature_projects_to_single() {
        let proj = test_proj();
        let f = GeoFeature::LineString(vec![c(-98.0, 39.0), c(-97.0, 39.5)]);
        let pf = project_feature(&f, &proj);
        match pf {
            FeatureProjection::Single(pts) => {
                assert_eq!(pts.len(), 2);
                // First coord is the projection center → screen center.
                let center = proj.screen_rect.center();
                assert!((pts[0].x - center.x).abs() < 1e-3);
                assert!((pts[0].y - center.y).abs() < 1e-3);
            }
            _ => panic!("expected Single projection"),
        }
    }

    #[wasm_bindgen_test]
    fn multilinestring_feature_projects_to_multi() {
        let proj = test_proj();
        let f = GeoFeature::MultiLineString(vec![
            vec![c(-98.0, 39.0), c(-97.5, 39.0)],
            vec![c(-96.0, 38.0)],
        ]);
        let pf = project_feature(&f, &proj);
        match pf {
            FeatureProjection::Multi(parts) => {
                assert_eq!(parts.len(), 2);
                assert_eq!(parts[0].len(), 2);
                assert_eq!(parts[1].len(), 1);
            }
            _ => panic!("expected Multi projection"),
        }
    }

    #[wasm_bindgen_test]
    fn multipolygon_feature_projects_exteriors_only() {
        let proj = test_proj();
        let f = GeoFeature::MultiPolygon {
            polygons: vec![
                (
                    vec![c(-98.0, 39.0), c(-97.0, 39.0), c(-97.0, 40.0)],
                    vec![vec![c(-97.8, 39.2)]], // hole ignored by projection
                ),
                (vec![c(-96.0, 38.0), c(-95.0, 38.0)], vec![]),
            ],
            label: None,
            label_anchor: c(0.0, 0.0),
        };
        let pf = project_feature(&f, &proj);
        match pf {
            FeatureProjection::Multi(parts) => {
                assert_eq!(parts.len(), 2);
                // Only exterior coords projected; hole has no effect on count.
                assert_eq!(parts[0].len(), 3);
                assert_eq!(parts[1].len(), 2);
            }
            _ => panic!("expected Multi projection"),
        }
    }

    #[wasm_bindgen_test]
    fn refresh_projection_cache_builds_parallel_entries() {
        let proj = test_proj();
        let mut layer = GeoLayer::new(GeoLayerType::States);
        layer.features.push(GeoFeature::Point(c(-98.0, 39.0), None));
        layer
            .features
            .push(GeoFeature::LineString(vec![c(-98.0, 39.0), c(-97.0, 39.0)]));

        layer.refresh_projection_cache(&proj);
        let entries = layer.cached_entries();
        assert_eq!(entries.len(), 2);
        assert!(matches!(entries[0], FeatureProjection::Empty));
        assert!(matches!(entries[1], FeatureProjection::Single(_)));
    }

    // ----- GeoLayerSet::new + set_layer routing -----

    #[wasm_bindgen_test]
    fn layer_set_new_is_all_none() {
        let set = GeoLayerSet::new();
        assert!(set.states.is_none());
        assert!(set.counties.is_none());
        assert!(set.cities.is_none());
        assert!(set.highways.is_none());
        assert!(set.lakes.is_none());
    }

    #[wasm_bindgen_test]
    fn set_layer_routes_to_matching_slot() {
        let mut set = GeoLayerSet::new();
        set.set_layer(GeoLayer::new(GeoLayerType::Lakes));
        assert!(set.lakes.is_some());
        assert!(set.states.is_none());
        assert_eq!(set.lakes.as_ref().unwrap().layer_type, GeoLayerType::Lakes);

        set.set_layer(GeoLayer::new(GeoLayerType::Cities));
        assert!(set.cities.is_some());
        assert_eq!(
            set.cities.as_ref().unwrap().layer_type,
            GeoLayerType::Cities
        );
    }

    // ----- LabelCacheToken default / equality -----

    #[wasm_bindgen_test]
    fn label_cache_token_default_and_eq() {
        let a = LabelCacheToken::default();
        assert_eq!(a.zoom_bucket, 0);
        assert!(!a.dark);
        assert!(!a.show_labels);

        let b = LabelCacheToken {
            zoom_bucket: 0,
            dark: false,
            show_labels: false,
        };
        assert_eq!(a, b);

        let c_tok = LabelCacheToken {
            zoom_bucket: 4,
            dark: true,
            show_labels: true,
        };
        assert_ne!(a, c_tok);
    }

    // ----- LabelEntry construction sanity (pure struct) -----

    #[wasm_bindgen_test]
    fn label_entry_holds_fields() {
        let e = LabelEntry {
            anchor: c(-98.0, 39.0),
            align: Align2::CENTER_CENTER,
            pixel_offset: Vec2::new(3.0, 0.0),
            text: "Topeka".to_string(),
            font_size: 12.0,
            color: Color32::WHITE,
        };
        assert_eq!(e.text, "Topeka");
        assert!((e.font_size - 12.0).abs() < 1e-6);
        assert!((e.anchor.y - 39.0).abs() < 1e-9);
        assert!((e.pixel_offset.x - 3.0).abs() < 1e-6);
    }
}
