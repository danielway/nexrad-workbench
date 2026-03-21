//! CPU-side inspection methods: value lookups and storm cell detection.

use super::{find_nearest_azimuth_index, RadarGpuRenderer};

impl RadarGpuRenderer {
    /// Look up the raw data value at a given polar coordinate.
    ///
    /// When `sweep_params` is `Some((sweep_azimuth, sweep_start))`, determines
    /// whether the queried position falls in the previous-sweep region and
    /// returns the appropriate value. Pass `None` for non-animated lookups.
    pub fn value_at_polar(
        &self,
        azimuth_deg: f32,
        range_km: f64,
        sweep_params: Option<(f32, f32)>,
    ) -> Option<f32> {
        if let Some((sweep_az, sweep_start)) = sweep_params {
            let swept_arc = (sweep_az - sweep_start).rem_euclid(360.0);
            let pixel_from_start = (azimuth_deg - sweep_start).rem_euclid(360.0);
            if pixel_from_start >= swept_arc {
                return self.prev_value_at_polar(azimuth_deg, range_km);
            }
        }
        if !self.has_data || self.cpu.azimuths.is_empty() {
            return None;
        }

        if range_km < self.current.first_gate_km || range_km >= self.current.max_range_km {
            return None;
        }

        let az_idx = find_nearest_azimuth_index(
            &self.cpu.azimuths,
            self.current.azimuth_count as usize,
            azimuth_deg,
        )?;

        let gate_count = self.current.gate_count as usize;
        let gate_idx = ((range_km - self.current.first_gate_km) / self.current.gate_interval_km)
            .floor() as usize;
        if gate_idx >= gate_count {
            return None;
        }

        let offset = az_idx * gate_count + gate_idx;
        if offset >= self.cpu.gate_values.len() {
            return None;
        }

        let raw = self.cpu.gate_values[offset];
        if raw <= 1.0 {
            return None;
        }

        if self.current.data_scale == 0.0 {
            Some(raw)
        } else {
            Some((raw - self.current.data_offset) / self.current.data_scale)
        }
    }

    /// Look up the radial collection timestamp (Unix seconds) at a given azimuth.
    ///
    /// When `sweep_params` is `Some((sweep_azimuth, sweep_start))`, determines
    /// whether the queried position falls in the previous-sweep region and
    /// returns the appropriate timestamp. Pass `None` for non-animated lookups.
    pub fn collection_time_at_polar(
        &self,
        azimuth_deg: f32,
        sweep_params: Option<(f32, f32)>,
    ) -> Option<f64> {
        if let Some((sweep_az, sweep_start)) = sweep_params {
            let swept_arc = (sweep_az - sweep_start).rem_euclid(360.0);
            let pixel_from_start = (azimuth_deg - sweep_start).rem_euclid(360.0);
            if pixel_from_start >= swept_arc {
                return self.prev_collection_time_at_polar(azimuth_deg);
            }
        }
        if self.cpu.radial_times.is_empty() || self.cpu.azimuths.is_empty() {
            return None;
        }

        let az_idx = find_nearest_azimuth_index(
            &self.cpu.azimuths,
            self.current.azimuth_count as usize,
            azimuth_deg,
        )?;

        self.cpu.radial_times.get(az_idx).copied()
    }

    /// Look up value in the previous sweep's CPU data using evenly-spaced azimuth indexing.
    fn prev_value_at_polar(&self, azimuth_deg: f32, range_km: f64) -> Option<f32> {
        let az_count = self.prev.azimuth_count as usize;
        let gate_count = self.prev.gate_count as usize;
        if az_count == 0 || gate_count == 0 || self.prev_cpu.gate_values.is_empty() {
            return None;
        }

        if range_km < self.prev.first_gate_km || range_km >= self.prev.max_range_km {
            return None;
        }

        // Evenly-spaced azimuth indexing (same as GPU shader for prev sweep)
        let az_idx = ((azimuth_deg * az_count as f32 / 360.0).round() as usize) % az_count;

        let gate_idx =
            ((range_km - self.prev.first_gate_km) / self.prev.gate_interval_km).floor() as usize;
        if gate_idx >= gate_count {
            return None;
        }

        let offset = az_idx * gate_count + gate_idx;
        if offset >= self.prev_cpu.gate_values.len() {
            return None;
        }

        let raw = self.prev_cpu.gate_values[offset];
        if raw <= 1.0 {
            return None;
        }

        if self.prev.data_scale == 0.0 {
            Some(raw)
        } else {
            Some((raw - self.prev.data_offset) / self.prev.data_scale)
        }
    }

    /// Look up collection time in the previous sweep's CPU data.
    fn prev_collection_time_at_polar(&self, azimuth_deg: f32) -> Option<f64> {
        let az_count = self.prev.azimuth_count as usize;
        if az_count == 0 || self.prev_cpu.radial_times.is_empty() {
            return None;
        }

        let az_idx = ((azimuth_deg * az_count as f32 / 360.0).round() as usize) % az_count;
        self.prev_cpu.radial_times.get(az_idx).copied()
    }

    /// Detect storm cells from the current CPU-side data.
    ///
    /// Returns lightweight cell info for rendering on the canvas.
    /// Uses nexrad-process connected-component analysis on the reflectivity data.
    pub fn detect_storm_cells(
        &self,
        radar_lat: f64,
        radar_lon: f64,
        threshold_dbz: f32,
    ) -> Vec<crate::state::StormCellInfo> {
        if !self.has_data || self.cpu.azimuths.is_empty() {
            return Vec::new();
        }

        let t_total = web_time::Instant::now();

        let az_count = self.current.azimuth_count as usize;
        let gate_count = self.current.gate_count as usize;

        // Compute azimuth spacing
        let az_spacing = if az_count > 1 {
            360.0 / az_count as f32
        } else {
            1.0
        };

        // Build a SweepField from the CPU data
        let t_field = web_time::Instant::now();
        let mut field = nexrad_model::data::SweepField::new_empty(
            "Reflectivity",
            "dBZ",
            0.5, // elevation doesn't matter for 2D detection
            self.cpu.azimuths.clone(),
            az_spacing,
            self.current.first_gate_km,
            self.current.gate_interval_km,
            gate_count,
        );

        // Populate the field with our gate values (convert raw -> physical)
        let mut valid_gates = 0u32;
        for az_idx in 0..az_count {
            let row_start = az_idx * gate_count;
            for g in 0..gate_count {
                let raw = self.cpu.gate_values[row_start + g];
                // Raw sentinels: 0 = below threshold, 1 = range folded
                if raw > 1.0 {
                    let physical = if self.current.data_scale == 0.0 {
                        raw
                    } else {
                        (raw - self.current.data_offset) / self.current.data_scale
                    };
                    field.set(az_idx, g, physical, nexrad_model::data::GateStatus::Valid);
                    valid_gates += 1;
                }
                // new_empty defaults to NoData, so we only set Valid gates
            }
        }
        let field_ms = t_field.elapsed().as_secs_f64() * 1000.0;

        // Build coordinate system from site location
        use nexrad_model::geo::RadarCoordinateSystem;
        use nexrad_model::meta::Site;
        use nexrad_process::detection::StormCellDetector;

        let site = Site::new(
            *b"SITE",
            radar_lat as f32,
            radar_lon as f32,
            0, // altitude (not critical for 2D detection)
            0, // tower height
        );
        let coord_system = RadarCoordinateSystem::new(&site);

        // Run detection
        let t_detect = web_time::Instant::now();
        let detector: StormCellDetector = match StormCellDetector::new(threshold_dbz, 10) {
            Ok(d) => d,
            Err(_) => return Vec::new(),
        };

        let cells: Vec<nexrad_process::detection::StormCell> =
            match detector.detect(&field, &coord_system) {
                Ok(c) => c,
                Err(e) => {
                    log::warn!("Storm cell detection failed: {}", e);
                    return Vec::new();
                }
            };
        let detect_ms = t_detect.elapsed().as_secs_f64() * 1000.0;

        // Convert to lightweight info, filtering out small noise cells
        let t_convert = web_time::Instant::now();
        const MIN_AREA_KM2: f64 = 5.0;

        let result: Vec<_> = cells
            .iter()
            .filter(|cell| cell.area_km2() >= MIN_AREA_KM2)
            .map(|cell| {
                let centroid = cell.centroid();
                let bounds = cell.bounds();
                crate::state::StormCellInfo {
                    lat: centroid.latitude,
                    lon: centroid.longitude,
                    max_dbz: cell.max_reflectivity_dbz(),
                    area_km2: cell.area_km2() as f32,
                    bounds: (
                        bounds.min_latitude(),
                        bounds.min_longitude(),
                        bounds.max_latitude(),
                        bounds.max_longitude(),
                    ),
                }
            })
            .collect();
        let convert_ms = t_convert.elapsed().as_secs_f64() * 1000.0;
        let total_ms = t_total.elapsed().as_secs_f64() * 1000.0;

        log::info!(
            "detect_storm_cells: {}x{} grid, {} valid gates, {} raw cells, {} after filter (>={:.0} km2), {:.1}ms (field: {:.1}ms, detect: {:.1}ms, convert: {:.1}ms)",
            az_count,
            gate_count,
            valid_gates,
            cells.len(),
            result.len(),
            MIN_AREA_KM2,
            total_ms,
            field_ms,
            detect_ms,
            convert_ms,
        );

        result
    }
}
