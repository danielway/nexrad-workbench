//! Geographic layer data structures.

use eframe::egui::Color32;
use geo_types::Coord;
use geojson::{Feature, FeatureCollection, GeoJson, Geometry, Value};
use shapefile::dbase::FieldValue;
use std::io::Cursor;

/// Type of geographic layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeoLayerType {
    States,
    Counties,
    Rivers,
    Lakes,
    Cities,
    Roads,
    Coastline,
}

impl GeoLayerType {
    /// Returns the default color for this layer type.
    pub fn default_color(&self) -> Color32 {
        match self {
            GeoLayerType::States => Color32::from_rgb(100, 100, 120),
            GeoLayerType::Counties => Color32::from_rgb(70, 70, 90),
            GeoLayerType::Rivers => Color32::from_rgb(60, 100, 180),
            GeoLayerType::Lakes => Color32::from_rgb(50, 90, 160),
            GeoLayerType::Cities => Color32::from_rgb(200, 200, 100),
            GeoLayerType::Roads => Color32::from_rgb(120, 100, 80),
            GeoLayerType::Coastline => Color32::from_rgb(80, 80, 100),
        }
    }

    /// Returns the default line width for this layer type.
    pub fn default_line_width(&self) -> f32 {
        match self {
            GeoLayerType::States => 1.5,
            GeoLayerType::Counties => 0.8,
            GeoLayerType::Rivers => 1.0,
            GeoLayerType::Lakes => 1.0,
            GeoLayerType::Cities => 0.0, // Points, not lines
            GeoLayerType::Roads => 0.5,
            GeoLayerType::Coastline => 1.2,
        }
    }

    /// Minimum zoom level at which this layer becomes visible.
    pub fn min_zoom(&self) -> f32 {
        match self {
            GeoLayerType::States => 0.0,
            GeoLayerType::Counties => 1.5,
            GeoLayerType::Rivers => 0.5,
            GeoLayerType::Lakes => 0.5,
            GeoLayerType::Cities => 1.0,
            GeoLayerType::Roads => 2.5,
            GeoLayerType::Coastline => 0.0,
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
        holes: Vec<Vec<Coord<f64>>>,
        label: Option<String>,
    },
    /// Multiple polygons with optional label
    MultiPolygon {
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
    ///
    /// The shp_bytes should be the contents of the .shp file.
    /// The dbf_bytes should be the contents of the .dbf file (for attribute data like names).
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
                        // Try common name fields
                        for field_name in &["NAME", "name", "Name", "NAMELSAD", "FULLNAME"] {
                            if let Some(value) = record.get(*field_name) {
                                if let FieldValue::Character(Some(s)) = value {
                                    return Some(s.trim().to_string());
                                }
                            }
                        }
                        None
                    })
            });

            if let Some(feature) = self.convert_shapefile_shape(&shape, label) {
                self.features.push(feature);
            }
        }

        Ok(())
    }

    fn convert_shapefile_shape(
        &self,
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
                // Shapefile polygons can have multiple outer rings (disconnected parts)
                // and inner rings (holes). We need to separate them properly.
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

                // If single outer ring, return Polygon; otherwise MultiPolygon
                if outer_rings.len() == 1 {
                    Some(GeoFeature::Polygon {
                        exterior: outer_rings.remove(0),
                        holes: current_holes,
                        label,
                    })
                } else {
                    // Multiple outer rings = MultiPolygon
                    // Note: This simplified approach doesn't associate holes with their outer rings
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

    /// Loads features from GeoJSON data.
    pub fn load_from_geojson(&mut self, geojson_str: &str) -> Result<(), String> {
        let geojson: GeoJson = geojson_str
            .parse()
            .map_err(|e| format!("Failed to parse GeoJSON: {}", e))?;

        match geojson {
            GeoJson::FeatureCollection(fc) => {
                self.load_feature_collection(fc);
            }
            GeoJson::Feature(f) => {
                if let Some(feature) = self.convert_feature(&f) {
                    self.features.push(feature);
                }
            }
            GeoJson::Geometry(g) => {
                if let Some(feature) = self.convert_geometry(&g, None) {
                    self.features.push(feature);
                }
            }
        }

        Ok(())
    }

    fn load_feature_collection(&mut self, fc: FeatureCollection) {
        for feature in fc.features {
            if let Some(geo_feature) = self.convert_feature(&feature) {
                self.features.push(geo_feature);
            }
        }
    }

    fn convert_feature(&self, feature: &Feature) -> Option<GeoFeature> {
        let label = feature
            .properties
            .as_ref()
            .and_then(|p| p.get("name").or_else(|| p.get("NAME")))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        feature
            .geometry
            .as_ref()
            .and_then(|g| self.convert_geometry(g, label))
    }

    fn convert_geometry(&self, geometry: &Geometry, label: Option<String>) -> Option<GeoFeature> {
        match &geometry.value {
            Value::Point(coords) => {
                let coord = Coord {
                    x: coords[0],
                    y: coords[1],
                };
                Some(GeoFeature::Point(coord, label))
            }
            Value::MultiPoint(points) => {
                // Convert MultiPoint to multiple Point features
                // For simplicity, we treat the first point as the representative
                if let Some(coords) = points.first() {
                    let coord = Coord {
                        x: coords[0],
                        y: coords[1],
                    };
                    Some(GeoFeature::Point(coord, label))
                } else {
                    None
                }
            }
            Value::LineString(coords) => {
                let line: Vec<Coord<f64>> =
                    coords.iter().map(|c| Coord { x: c[0], y: c[1] }).collect();
                Some(GeoFeature::LineString(line))
            }
            Value::MultiLineString(lines) => {
                let multi: Vec<Vec<Coord<f64>>> = lines
                    .iter()
                    .map(|line| line.iter().map(|c| Coord { x: c[0], y: c[1] }).collect())
                    .collect();
                Some(GeoFeature::MultiLineString(multi))
            }
            Value::Polygon(rings) => {
                if rings.is_empty() {
                    return None;
                }
                let exterior: Vec<Coord<f64>> = rings[0]
                    .iter()
                    .map(|c| Coord { x: c[0], y: c[1] })
                    .collect();
                let holes: Vec<Vec<Coord<f64>>> = rings[1..]
                    .iter()
                    .map(|ring| ring.iter().map(|c| Coord { x: c[0], y: c[1] }).collect())
                    .collect();
                Some(GeoFeature::Polygon {
                    exterior,
                    holes,
                    label,
                })
            }
            Value::MultiPolygon(polygons) => {
                let polygons: Vec<(Vec<Coord<f64>>, Vec<Vec<Coord<f64>>>)> = polygons
                    .iter()
                    .filter_map(|rings| {
                        if rings.is_empty() {
                            return None;
                        }
                        let exterior: Vec<Coord<f64>> = rings[0]
                            .iter()
                            .map(|c| Coord { x: c[0], y: c[1] })
                            .collect();
                        let holes: Vec<Vec<Coord<f64>>> = rings[1..]
                            .iter()
                            .map(|ring| ring.iter().map(|c| Coord { x: c[0], y: c[1] }).collect())
                            .collect();
                        Some((exterior, holes))
                    })
                    .collect();
                Some(GeoFeature::MultiPolygon { polygons, label })
            }
            Value::GeometryCollection(geometries) => {
                // For geometry collections, just take the first convertible geometry
                for g in geometries {
                    if let Some(feature) = self.convert_geometry(g, label.clone()) {
                        return Some(feature);
                    }
                }
                None
            }
        }
    }
}

/// Collection of all geographic layers.
#[derive(Debug, Clone, Default)]
pub struct GeoLayerSet {
    pub states: Option<GeoLayer>,
    pub counties: Option<GeoLayer>,
    pub rivers: Option<GeoLayer>,
    pub lakes: Option<GeoLayer>,
    pub cities: Option<GeoLayer>,
    pub roads: Option<GeoLayer>,
    pub coastline: Option<GeoLayer>,
}

impl GeoLayerSet {
    /// Creates a new empty layer set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns an iterator over all loaded layers.
    pub fn iter(&self) -> impl Iterator<Item = &GeoLayer> {
        [
            self.coastline.as_ref(),
            self.states.as_ref(),
            self.counties.as_ref(),
            self.lakes.as_ref(),
            self.rivers.as_ref(),
            self.roads.as_ref(),
            self.cities.as_ref(),
        ]
        .into_iter()
        .flatten()
    }

    /// Returns a mutable iterator over all loaded layers.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut GeoLayer> {
        [
            self.coastline.as_mut(),
            self.states.as_mut(),
            self.counties.as_mut(),
            self.lakes.as_mut(),
            self.rivers.as_mut(),
            self.roads.as_mut(),
            self.cities.as_mut(),
        ]
        .into_iter()
        .flatten()
    }

    /// Loads a layer from GeoJSON string.
    pub fn load_layer(
        &mut self,
        layer_type: GeoLayerType,
        geojson_str: &str,
    ) -> Result<(), String> {
        let mut layer = GeoLayer::new(layer_type);
        layer.load_from_geojson(geojson_str)?;
        self.set_layer(layer_type, layer);
        Ok(())
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
        self.set_layer(layer_type, layer);
        Ok(())
    }

    fn set_layer(&mut self, layer_type: GeoLayerType, layer: GeoLayer) {
        match layer_type {
            GeoLayerType::States => self.states = Some(layer),
            GeoLayerType::Counties => self.counties = Some(layer),
            GeoLayerType::Rivers => self.rivers = Some(layer),
            GeoLayerType::Lakes => self.lakes = Some(layer),
            GeoLayerType::Cities => self.cities = Some(layer),
            GeoLayerType::Roads => self.roads = Some(layer),
            GeoLayerType::Coastline => self.coastline = Some(layer),
        }
    }
}
