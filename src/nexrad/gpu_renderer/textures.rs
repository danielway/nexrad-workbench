//! Texture creation, upload, and state management for the GPU radar renderer.

use super::{create_r32f_texture, create_rgba8_texture, RadarGpuRenderer};
use crate::nexrad::color_table::{
    build_reflectivity_lut, continuous_color_scale, product_from_str, product_value_range,
};
use glow::HasContext;
use nexrad_render::Product;

impl RadarGpuRenderer {
    /// Upload decoded radar data to GPU textures.
    ///
    /// `gate_values` contains raw u16 values cast to f32.
    /// Sentinels: 0 = below threshold, 1 = range folded.
    /// Physical value = (raw - offset) / scale.
    #[allow(clippy::too_many_arguments)]
    pub fn update_data(
        &mut self,
        gl: &glow::Context,
        azimuths: &[f32],
        gate_values: &[f32],
        azimuth_count: u32,
        gate_count: u32,
        first_gate_km: f64,
        gate_interval_km: f64,
        max_range_km: f64,
        offset: f32,
        scale: f32,
        azimuth_spacing_deg: f32,
        radial_times: &[f64],
    ) {
        let t_total = web_time::Instant::now();

        self.current.azimuth_count = azimuth_count;
        self.current.gate_count = gate_count;
        self.current.first_gate_km = first_gate_km;
        self.current.gate_interval_km = gate_interval_km;
        self.current.max_range_km = max_range_km;
        self.current.data_offset = offset;
        self.current.data_scale = scale;
        self.current.azimuth_spacing_deg = if azimuth_spacing_deg > 0.0 {
            azimuth_spacing_deg
        } else {
            1.0
        };
        self.has_data = azimuth_count > 0 && gate_count > 0;

        // Keep CPU copies for inspector value lookup
        let t_copy = web_time::Instant::now();
        self.cpu.azimuths = azimuths.to_vec();
        self.cpu.gate_values = gate_values.to_vec();
        self.cpu.radial_times = radial_times.to_vec();
        let copy_ms = t_copy.elapsed().as_secs_f64() * 1000.0;

        if !self.has_data {
            return;
        }

        let t_upload = web_time::Instant::now();
        unsafe {
            // Re-create data texture (gates x azimuths, R32F)
            gl.delete_texture(self.data_texture);
            self.data_texture =
                create_r32f_texture(gl, gate_count as i32, azimuth_count as i32, gate_values);

            // Re-create azimuth texture (Nx1, R32F)
            gl.delete_texture(self.azimuth_texture);
            self.azimuth_texture = create_r32f_texture(gl, azimuth_count as i32, 1, azimuths);
        }
        let upload_ms = t_upload.elapsed().as_secs_f64() * 1000.0;
        let total_ms = t_total.elapsed().as_secs_f64() * 1000.0;

        let expected_len = (azimuth_count * gate_count) as usize;
        let actual_len = gate_values.len();
        let az_first = azimuths.first().copied().unwrap_or(f32::NAN);
        let az_last = azimuths.last().copied().unwrap_or(f32::NAN);
        if actual_len != expected_len {
            log::warn!(
                "GPU update_data: dimension MISMATCH — {}x{} expects {} values, got {} (diff={})",
                azimuth_count,
                gate_count,
                expected_len,
                actual_len,
                actual_len as i64 - expected_len as i64,
            );
        }
        log::debug!(
            "GPU update_data: {}x{} (az x gates), azimuths=[{:.1}..{:.1}], vals={}, range {:.1}-{:.1} km, offset={} scale={}, {:.1}ms (copy: {:.1}ms, upload: {:.1}ms)",
            azimuth_count,
            gate_count,
            az_first,
            az_last,
            actual_len,
            first_gate_km,
            max_range_km,
            offset,
            scale,
            total_ms,
            copy_ms,
            upload_ms,
        );
    }

    /// Upload decoded radar data to the *previous* texture slot for sweep
    /// animation compositing. Stores per-sweep spatial metadata so the shader
    /// can sample the previous texture with correct gate/range mapping even
    /// when the current and previous sweeps have different dimensions.
    #[allow(clippy::too_many_arguments)]
    pub fn update_previous_data(
        &mut self,
        gl: &glow::Context,
        azimuths: &[f32],
        gate_values: &[f32],
        azimuth_count: u32,
        gate_count: u32,
        first_gate_km: f64,
        gate_interval_km: f64,
        max_range_km: f64,
        offset: f32,
        scale: f32,
        azimuth_spacing_deg: f32,
        sweep_id: Option<String>,
        radial_times: &[f64],
    ) {
        self.prev.data_offset = offset;
        self.prev.data_scale = scale;
        self.prev.azimuth_count = azimuth_count;
        self.prev.gate_count = gate_count;
        self.prev.first_gate_km = first_gate_km;
        self.prev.gate_interval_km = gate_interval_km;
        self.prev.max_range_km = max_range_km;
        self.prev.azimuth_spacing_deg = if azimuth_spacing_deg > 0.0 {
            azimuth_spacing_deg
        } else {
            1.0
        };
        self.prev.sweep_id = sweep_id;
        self.prev_cpu.gate_values = gate_values.to_vec();
        self.prev_cpu.radial_times = radial_times.to_vec();

        if azimuth_count == 0 || gate_count == 0 {
            return;
        }

        unsafe {
            gl.delete_texture(self.prev_data_texture);
            self.prev_data_texture =
                create_r32f_texture(gl, gate_count as i32, azimuth_count as i32, gate_values);

            gl.delete_texture(self.prev_azimuth_texture);
            self.prev_azimuth_texture = create_r32f_texture(gl, azimuth_count as i32, 1, azimuths);
        }
    }

    /// Promote the current texture to the previous texture slot.
    ///
    /// Called at elevation transitions during live streaming so the just-completed
    /// sweep becomes the background for compositing partial data on top.
    pub fn promote_current_to_previous(&mut self, gl: &glow::Context) {
        if !self.has_data || self.cpu.azimuths.is_empty() {
            return;
        }
        self.update_previous_data(
            gl,
            &self.cpu.azimuths.clone(),
            &self.cpu.gate_values.clone(),
            self.current.azimuth_count,
            self.current.gate_count,
            self.current.first_gate_km,
            self.current.gate_interval_km,
            self.current.max_range_km,
            self.current.data_offset,
            self.current.data_scale,
            self.current.azimuth_spacing_deg,
            self.current.sweep_id.clone(),
            &self.cpu.radial_times.clone(),
        );
    }

    /// Clear all radar data (e.g. on site change).
    pub fn clear_data(&mut self) {
        self.has_data = false;
        self.current.sweep_id = None;
        self.cpu.azimuths.clear();
        self.cpu.gate_values.clear();
        self.cpu.radial_times.clear();
        self.clear_previous_data();
    }

    /// Clear the previous-sweep texture so the shader composites against
    /// transparent until a new previous sweep is loaded. Zeroes the spatial
    /// metadata so the shader's range/gate check bails out on the prev branch
    /// — sweep_id alone isn't enough because the shader samples prev_data_tex
    /// based on the uploaded uniforms, not the identity string.
    pub fn clear_previous_data(&mut self) {
        self.prev.sweep_id = None;
        self.prev.azimuth_count = 0;
        self.prev.gate_count = 0;
        self.prev.max_range_km = 0.0;
        self.prev_cpu.gate_values.clear();
        self.prev_cpu.radial_times.clear();
    }

    /// Returns true if radar data has been uploaded.
    pub fn has_data(&self) -> bool {
        self.has_data
    }

    /// Identity of the sweep currently loaded in the primary data texture.
    pub fn current_sweep_id(&self) -> Option<&str> {
        self.current.sweep_id.as_deref()
    }

    /// Identity of the sweep currently loaded in the previous data texture.
    pub fn prev_sweep_id(&self) -> Option<&str> {
        self.prev.sweep_id.as_deref()
    }

    /// Set the identity of the current sweep (called after `update_data`).
    pub fn set_current_sweep_id(&mut self, id: Option<String>) {
        self.current.sweep_id = id;
    }

    /// Build and upload a color lookup table for the given product.
    pub fn update_color_table(&mut self, gl: &glow::Context, product_str: &str) {
        let t_total = web_time::Instant::now();

        let product = product_from_str(product_str);
        let (min_val, max_val) = product_value_range(product);
        self.value_min = min_val;
        self.value_range = max_val - min_val;

        let t_build = web_time::Instant::now();

        // Build 1024-entry RGBA LUT (continuous gradient + GL_LINEAR = zero visible quantization)
        let lut_size = 1024usize;
        let lut_data = if matches!(product, Product::Reflectivity) {
            // OKLab-interpolated reflectivity palette with alpha ramp
            build_reflectivity_lut(min_val, max_val)
        } else {
            let color_scale = continuous_color_scale(product);
            let mut data = Vec::with_capacity(lut_size * 4);
            for i in 0..lut_size {
                let t = i as f32 / (lut_size - 1) as f32;
                let value = min_val + t * (max_val - min_val);
                let color = color_scale.color(value);
                let rgba = color.to_rgba8();
                data.extend_from_slice(&rgba);
            }
            data
        };
        let build_ms = t_build.elapsed().as_secs_f64() * 1000.0;

        let t_upload = web_time::Instant::now();
        unsafe {
            gl.delete_texture(self.lut_texture);
            self.lut_texture = create_rgba8_texture(gl, lut_size as i32, 1, &lut_data);
        }
        let upload_ms = t_upload.elapsed().as_secs_f64() * 1000.0;
        let total_ms = t_total.elapsed().as_secs_f64() * 1000.0;

        log::debug!(
            "GPU update_color_table: {:?} ({:.1}..{:.1}), {:.1}ms (build: {:.1}ms, upload: {:.1}ms)",
            product,
            min_val,
            max_val,
            total_ms,
            build_ms,
            upload_ms,
        );
    }
}
