//! Geographic layer data structures.

use eframe::egui::Color32;
use geo_types::Coord;
use shapefile::dbase::FieldValue;
use std::io::Cursor;

/// Type of geographic layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeoLayerType {
    States,
    Counties,
}

impl GeoLayerType {
    /// Returns the default color for this layer type.
    pub fn default_color(&self) -> Color32 {
        match self {
            GeoLayerType::States => Color32::from_rgb(100, 100, 120),
            GeoLayerType::Counties => Color32::from_rgb(70, 70, 90),
        }
    }

    /// Returns the default line width for this layer type.
    pub fn default_line_width(&self) -> f32 {
        match self {
            GeoLayerType::States => 1.5,
            GeoLayerType::Counties => 0.8,
        }
    }

    /// Minimum zoom level at which this layer becomes visible.
    pub fn min_zoom(&self) -> f32 {
        match self {
            GeoLayerType::States => 0.0,
            GeoLayerType::Counties => 1.5,
        }
    }

    /// Minimum zoom level at which labels for this layer become visible.
    pub fn min_label_zoom(&self) -> f32 {
        match self {
            GeoLayerType::States => 0.0,
            GeoLayerType::Counties => 3.0,
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
    /// A closed polygon with optional label
    Polygon {
        exterior: Vec<Coord<f64>>,
        #[allow(dead_code)] // Part of polygon data model
        holes: Vec<Vec<Coord<f64>>>,
        label: Option<String>,
    },
    /// Multiple polygons with optional label
    MultiPolygon {
        #[allow(clippy::type_complexity)]
        polygons: Vec<(Vec<Coord<f64>>, Vec<Vec<Coord<f64>>>)>,
        label: Option<String>,
    },
    /// A single point (for cities, landmarks)
    Point(Coord<f64>, Option<String>),
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
        }
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

fn convert_shapefile_shape(
    shape: &shapefile::Shape,
    label: Option<String>,
) -> Option<GeoFeature> {
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
                Some(GeoFeature::Polygon {
                    exterior: outer_rings.remove(0),
                    holes: current_holes,
                    label,
                })
            } else {
                #[allow(clippy::type_complexity)]
                let polygons: Vec<(Vec<Coord<f64>>, Vec<Vec<Coord<f64>>>)> = outer_rings
                    .into_iter()
                    .map(|ext| (ext, Vec::new()))
                    .collect();
                Some(GeoFeature::MultiPolygon { polygons, label })
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
}

impl GeoLayerSet {
    /// Creates a new empty layer set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns an iterator over all loaded layers.
    pub fn iter(&self) -> impl Iterator<Item = &GeoLayer> {
        [self.states.as_ref(), self.counties.as_ref()]
            .into_iter()
            .flatten()
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
        }
        Ok(())
    }
}
