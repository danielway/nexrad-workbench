//! CPU-side inspection methods: value lookups and storm cell detection.

use super::RadarGpuRenderer;
use crate::core::canvas::{
    collection_time_current, collection_time_prev, polar_in_prev_region, value_at_polar_current,
    value_at_polar_prev, PolarSweepMeta,
};

impl RadarGpuRenderer {
    /// Spatial metadata for the current sweep's CPU buffers.
    fn current_polar_meta(&self) -> PolarSweepMeta {
        PolarSweepMeta {
            azimuth_count: self.current.azimuth_count,
            gate_count: self.current.gate_count,
            first_gate_km: self.current.first_gate_km,
            gate_interval_km: self.current.gate_interval_km,
            max_range_km: self.current.max_range_km,
            data_offset: self.current.data_offset,
            data_scale: self.current.data_scale,
        }
    }

    /// Spatial metadata for the previous sweep's CPU buffers.
    fn prev_polar_meta(&self) -> PolarSweepMeta {
        PolarSweepMeta {
            azimuth_count: self.prev.azimuth_count,
            gate_count: self.prev.gate_count,
            first_gate_km: self.prev.first_gate_km,
            gate_interval_km: self.prev.gate_interval_km,
            max_range_km: self.prev.max_range_km,
            data_offset: self.prev.data_offset,
            data_scale: self.prev.data_scale,
        }
    }

    /// Look up the raw data value at a given polar coordinate.
    ///
    /// When `sweep_params` is `Some((sweep_azimuth, sweep_start))`, determines
    /// whether the queried position falls in the previous-sweep region and
    /// returns the appropriate value. Pass `None` for non-animated lookups.
    /// The lookup math lives in [`crate::core::canvas`]; this binds it to the
    /// renderer's CPU shadow buffers.
    pub fn value_at_polar(
        &self,
        azimuth_deg: f32,
        range_km: f64,
        sweep_params: Option<(f32, f32)>,
    ) -> Option<f32> {
        if polar_in_prev_region(azimuth_deg, sweep_params) {
            return value_at_polar_prev(
                azimuth_deg,
                range_km,
                &self.prev_polar_meta(),
                &self.prev_cpu.gate_values,
            );
        }
        if !self.has_data {
            return None;
        }
        value_at_polar_current(
            azimuth_deg,
            range_km,
            &self.current_polar_meta(),
            &self.cpu.azimuths,
            &self.cpu.gate_values,
        )
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
        if polar_in_prev_region(azimuth_deg, sweep_params) {
            return collection_time_prev(
                azimuth_deg,
                self.prev.azimuth_count,
                &self.prev_cpu.radial_times,
            );
        }
        collection_time_current(
            azimuth_deg,
            self.current.azimuth_count,
            &self.cpu.azimuths,
            &self.cpu.radial_times,
        )
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
