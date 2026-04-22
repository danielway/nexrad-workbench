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
    /// Thin adapter over `crate::nexrad::detection` — packages the shadow
    /// copies of the rendered sweep into a `DetectionInput` and runs the
    /// in-tree threshold + connected-component detector.
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

        let input = crate::nexrad::detection::DetectionInput {
            azimuths: &self.cpu.azimuths,
            gate_values: &self.cpu.gate_values,
            azimuth_count: az_count,
            gate_count,
            first_gate_km: self.current.first_gate_km,
            gate_interval_km: self.current.gate_interval_km,
            data_scale: self.current.data_scale,
            data_offset: self.current.data_offset,
            radar_lat,
            radar_lon,
        };
        let params = crate::nexrad::detection::DetectionParams {
            threshold_dbz,
            ..Default::default()
        };

        let result = crate::nexrad::detection::detect_cells(&input, &params);

        log::debug!(
            "detect_storm_cells: {}x{} grid, {} cells (>= {:.0} dBZ), {:.1}ms",
            az_count,
            gate_count,
            result.len(),
            threshold_dbz,
            t_total.elapsed().as_secs_f64() * 1000.0,
        );

        result
    }
}
