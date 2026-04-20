//! GPU-based radar renderer using WebGL2 shaders via glow.
//!
//! Renders polar radar data (azimuths x gates) directly on the GPU using a fragment
//! shader that performs polar-to-Cartesian conversion and color lookup from a LUT texture.

mod inspect;
pub(crate) mod shaders;
mod textures;

use crate::state::RenderProcessing;
use glow::HasContext;
use std::sync::Arc;

/// Find the nearest azimuth index in an array of azimuth angles.
///
/// Returns `None` if the nearest azimuth is farther than 1.5x the expected spacing
/// (gap detection). Used by CPU-side inspector lookups.
///
/// Azimuths < 0 mark empty padded slots from the live partial-sweep path and
/// are skipped.
fn find_nearest_azimuth_index(
    azimuths: &[f32],
    azimuth_count: usize,
    target_deg: f32,
) -> Option<usize> {
    let mut best_idx = 0usize;
    let mut best_dist = 360.0f32;
    let mut found = false;
    for (i, &az) in azimuths.iter().enumerate() {
        if az < 0.0 {
            continue;
        }
        let mut d = (target_deg - az).abs();
        if d > 180.0 {
            d = 360.0 - d;
        }
        if d < best_dist {
            best_dist = d;
            best_idx = i;
            found = true;
        }
    }

    if !found {
        return None;
    }

    let az_spacing = 360.0 / azimuth_count as f32;
    if best_dist > az_spacing * 1.5 {
        return None;
    }

    Some(best_idx)
}

/// All uniform locations for the radar shader program.
struct UniformLocations {
    radar_center: glow::UniformLocation,
    radar_radius: glow::UniformLocation,
    gate_count: glow::UniformLocation,
    azimuth_count: glow::UniformLocation,
    first_gate_km: glow::UniformLocation,
    gate_interval_km: glow::UniformLocation,
    max_range_km: glow::UniformLocation,
    value_min: glow::UniformLocation,
    value_range: glow::UniformLocation,
    viewport_size: glow::UniformLocation,
    interpolation: glow::UniformLocation,
    opacity: glow::UniformLocation,
    data_age_desaturation: glow::UniformLocation,
    offset: glow::UniformLocation,
    scale: glow::UniformLocation,
    sweep_enabled: glow::UniformLocation,
    sweep_azimuth: glow::UniformLocation,
    sweep_start: glow::UniformLocation,
    prev_offset: glow::UniformLocation,
    prev_scale: glow::UniformLocation,
    prev_gate_count: glow::UniformLocation,
    prev_azimuth_count: glow::UniformLocation,
    prev_first_gate_km: glow::UniformLocation,
    prev_gate_interval_km: glow::UniformLocation,
    prev_max_range_km: glow::UniformLocation,
    sweep_chunk_boundary: glow::UniformLocation,
    azimuth_spacing_deg: glow::UniformLocation,
    prev_azimuth_spacing_deg: glow::UniformLocation,
}

/// Spatial metadata for a single sweep (current or previous).
struct SweepState {
    azimuth_count: u32,
    gate_count: u32,
    first_gate_km: f64,
    gate_interval_km: f64,
    max_range_km: f64,
    data_offset: f32,
    data_scale: f32,
    /// Median angular spacing between adjacent sorted radials, in degrees.
    /// The shader uses this for search thresholds rather than deriving from
    /// azimuth_count (which would be wrong for partial/clustered sweeps).
    azimuth_spacing_deg: f32,
    sweep_id: Option<String>,
}

impl Default for SweepState {
    fn default() -> Self {
        Self {
            azimuth_count: 0,
            gate_count: 0,
            first_gate_km: 0.0,
            gate_interval_km: 0.0,
            max_range_km: 0.0,
            data_offset: 0.0,
            data_scale: 1.0,
            azimuth_spacing_deg: 1.0,
            sweep_id: None,
        }
    }
}

/// CPU-side shadow copies of sweep data for inspector/detection lookups.
#[derive(Default)]
struct CpuShadowData {
    azimuths: Vec<f32>,
    gate_values: Vec<f32>,
    radial_times: Vec<f64>,
}

/// GPU-based radar renderer using WebGL2 shaders.
pub struct RadarGpuRenderer {
    program: glow::Program,
    vao: glow::VertexArray,
    #[allow(dead_code)] // Retained to prevent GPU resource deallocation
    vbo: glow::Buffer,

    data_texture: glow::Texture,
    lut_texture: glow::Texture,
    azimuth_texture: glow::Texture,

    // Previous scan textures (for sweep animation)
    prev_data_texture: glow::Texture,
    prev_azimuth_texture: glow::Texture,

    uniforms: UniformLocations,
    current: SweepState,
    prev: SweepState,
    cpu: CpuShadowData,
    prev_cpu: CpuShadowData,

    has_data: bool,
    value_min: f32,
    value_range: f32,
}

impl RadarGpuRenderer {
    /// Create a new GPU renderer, compiling shaders and allocating GL resources.
    ///
    /// Returns `Err` if shader compilation, program linking, or GL resource
    /// allocation fails, allowing the caller to fall back gracefully.
    pub fn new(gl: &Arc<glow::Context>) -> Result<Self, String> {
        unsafe {
            let program = Self::build_program(gl)?;

            // Fullscreen quad (two triangles)
            let vertices: [f32; 12] = [
                -1.0, -1.0, 1.0, -1.0, 1.0, 1.0, -1.0, -1.0, 1.0, 1.0, -1.0, 1.0,
            ];

            let vbo = gl
                .create_buffer()
                .map_err(|e| format!("Cannot create VBO: {}", e))?;
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            gl.buffer_data_u8_slice(
                glow::ARRAY_BUFFER,
                bytemuck_cast_slice(&vertices),
                glow::STATIC_DRAW,
            );

            let vao = gl
                .create_vertex_array()
                .map_err(|e| format!("Cannot create VAO: {}", e))?;
            gl.bind_vertex_array(Some(vao));
            let a_position = gl
                .get_attrib_location(program, "a_position")
                .ok_or("Missing a_position")?;
            gl.enable_vertex_attrib_array(a_position);
            gl.vertex_attrib_pointer_f32(a_position, 2, glow::FLOAT, false, 8, 0);
            gl.bind_vertex_array(None);

            // Create placeholder textures (1x1)
            let data_texture = create_r32f_texture(gl, 1, 1, &[0.0]);
            let azimuth_texture = create_r32f_texture(gl, 1, 1, &[0.0]);
            let lut_texture = create_rgba8_texture(gl, 1, 1, &[0, 0, 0, 0]);

            // Helper to look up a required uniform location
            let uniform = |name: &str| -> Result<glow::UniformLocation, String> {
                gl.get_uniform_location(program, name)
                    .ok_or_else(|| format!("Missing uniform: {}", name))
            };

            // Bind texture units to sampler uniforms
            gl.use_program(Some(program));

            let u_data_tex = uniform("u_data_tex")?;
            gl.uniform_1_i32(Some(&u_data_tex), 0);
            let u_lut_tex = uniform("u_lut_tex")?;
            gl.uniform_1_i32(Some(&u_lut_tex), 1);
            let u_azimuth_tex = uniform("u_azimuth_tex")?;
            gl.uniform_1_i32(Some(&u_azimuth_tex), 2);
            let u_prev_data_tex = uniform("u_prev_data_tex")?;
            gl.uniform_1_i32(Some(&u_prev_data_tex), 3);
            let u_prev_azimuth_tex = uniform("u_prev_azimuth_tex")?;
            gl.uniform_1_i32(Some(&u_prev_azimuth_tex), 4);

            let uniforms = UniformLocations {
                radar_center: uniform("u_radar_center")?,
                radar_radius: uniform("u_radar_radius")?,
                gate_count: uniform("u_gate_count")?,
                azimuth_count: uniform("u_azimuth_count")?,
                first_gate_km: uniform("u_first_gate_km")?,
                gate_interval_km: uniform("u_gate_interval_km")?,
                max_range_km: uniform("u_max_range_km")?,
                value_min: uniform("u_value_min")?,
                value_range: uniform("u_value_range")?,
                viewport_size: uniform("u_viewport_size")?,
                interpolation: uniform("u_interpolation")?,
                opacity: uniform("u_opacity")?,
                data_age_desaturation: uniform("u_data_age_desaturation")?,
                offset: uniform("u_offset")?,
                scale: uniform("u_scale")?,
                sweep_enabled: uniform("u_sweep_enabled")?,
                sweep_azimuth: uniform("u_sweep_azimuth")?,
                sweep_start: uniform("u_sweep_start")?,
                prev_offset: uniform("u_prev_offset")?,
                prev_scale: uniform("u_prev_scale")?,
                prev_gate_count: uniform("u_prev_gate_count")?,
                prev_azimuth_count: uniform("u_prev_azimuth_count")?,
                prev_first_gate_km: uniform("u_prev_first_gate_km")?,
                prev_gate_interval_km: uniform("u_prev_gate_interval_km")?,
                prev_max_range_km: uniform("u_prev_max_range_km")?,
                sweep_chunk_boundary: uniform("u_sweep_chunk_boundary")?,
                azimuth_spacing_deg: uniform("u_azimuth_spacing_deg")?,
                prev_azimuth_spacing_deg: uniform("u_prev_azimuth_spacing_deg")?,
            };

            // Create placeholders for previous sweep textures
            let prev_data_texture = create_r32f_texture(gl, 1, 1, &[0.0]);
            let prev_azimuth_texture = create_r32f_texture(gl, 1, 1, &[0.0]);

            gl.use_program(None);

            Ok(Self {
                program,
                vao,
                vbo,
                data_texture,
                lut_texture,
                azimuth_texture,
                prev_data_texture,
                prev_azimuth_texture,
                uniforms,
                current: SweepState::default(),
                prev: SweepState::default(),
                cpu: CpuShadowData::default(),
                prev_cpu: CpuShadowData::default(),
                has_data: false,
                value_min: 0.0,
                value_range: 1.0,
            })
        }
    }

    /// Maximum range of the currently loaded data in km.
    pub fn max_range_km(&self) -> f64 {
        self.current.max_range_km
    }

    // --- Accessors for globe radar renderer ---

    pub fn gate_count(&self) -> u32 {
        self.current.gate_count
    }
    pub fn azimuth_count(&self) -> u32 {
        self.current.azimuth_count
    }
    pub fn first_gate_km(&self) -> f64 {
        self.current.first_gate_km
    }
    pub fn gate_interval_km(&self) -> f64 {
        self.current.gate_interval_km
    }
    pub fn value_min(&self) -> f32 {
        self.value_min
    }
    pub fn value_range(&self) -> f32 {
        self.value_range
    }
    pub fn data_offset(&self) -> f32 {
        self.current.data_offset
    }
    pub fn data_scale(&self) -> f32 {
        self.current.data_scale
    }
    pub fn azimuth_spacing_deg(&self) -> f32 {
        self.current.azimuth_spacing_deg
    }
    pub fn data_texture(&self) -> glow::Texture {
        self.data_texture
    }
    pub fn lut_texture(&self) -> glow::Texture {
        self.lut_texture
    }
    pub fn azimuth_texture(&self) -> glow::Texture {
        self.azimuth_texture
    }

    /// Render the radar data using the current GL context.
    ///
    /// Called from within an `egui_glow::CallbackFn`.
    /// `radar_center` and `radar_radius` are in physical pixels (not points).
    ///
    /// egui_glow restores its own GL state after each paint callback,
    /// so we don't need to save/restore state ourselves.
    #[allow(clippy::too_many_arguments)]
    pub fn paint(
        &self,
        gl: &glow::Context,
        radar_center: [f32; 2],
        radar_radius: f32,
        viewport_size: [f32; 2],
        processing: &RenderProcessing,
        sweep_info: Option<(f32, f32)>,
        sweep_chunk_boundary: Option<f32>,
    ) {
        if !self.has_data {
            return;
        }

        unsafe {
            gl.use_program(Some(self.program));
            gl.bind_vertex_array(Some(self.vao));

            // Premultiplied alpha blending
            gl.enable(glow::BLEND);
            gl.blend_func_separate(
                glow::ONE,
                glow::ONE_MINUS_SRC_ALPHA,
                glow::ONE,
                glow::ONE_MINUS_SRC_ALPHA,
            );
            gl.disable(glow::SCISSOR_TEST);

            // Bind textures to units
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(self.data_texture));
            gl.active_texture(glow::TEXTURE1);
            gl.bind_texture(glow::TEXTURE_2D, Some(self.lut_texture));
            gl.active_texture(glow::TEXTURE2);
            gl.bind_texture(glow::TEXTURE_2D, Some(self.azimuth_texture));
            gl.active_texture(glow::TEXTURE3);
            gl.bind_texture(glow::TEXTURE_2D, Some(self.prev_data_texture));
            gl.active_texture(glow::TEXTURE4);
            gl.bind_texture(glow::TEXTURE_2D, Some(self.prev_azimuth_texture));

            // Set uniforms
            gl.uniform_2_f32(
                Some(&self.uniforms.radar_center),
                radar_center[0],
                radar_center[1],
            );
            gl.uniform_1_f32(Some(&self.uniforms.radar_radius), radar_radius);
            gl.uniform_1_f32(
                Some(&self.uniforms.gate_count),
                self.current.gate_count as f32,
            );
            gl.uniform_1_f32(
                Some(&self.uniforms.azimuth_count),
                self.current.azimuth_count as f32,
            );
            gl.uniform_1_f32(
                Some(&self.uniforms.first_gate_km),
                self.current.first_gate_km as f32,
            );
            gl.uniform_1_f32(
                Some(&self.uniforms.gate_interval_km),
                self.current.gate_interval_km as f32,
            );
            gl.uniform_1_f32(
                Some(&self.uniforms.max_range_km),
                self.current.max_range_km as f32,
            );
            gl.uniform_1_f32(Some(&self.uniforms.value_min), self.value_min);
            gl.uniform_1_f32(Some(&self.uniforms.value_range), self.value_range);
            gl.uniform_2_f32(
                Some(&self.uniforms.viewport_size),
                viewport_size[0],
                viewport_size[1],
            );

            // Processing uniforms
            let interp_mode = match processing.interpolation {
                crate::state::InterpolationMode::Nearest => 0,
                crate::state::InterpolationMode::Bilinear => 1,
            };
            gl.uniform_1_i32(Some(&self.uniforms.interpolation), interp_mode);
            gl.uniform_1_f32(Some(&self.uniforms.opacity), processing.opacity);
            gl.uniform_1_i32(
                Some(&self.uniforms.data_age_desaturation),
                processing.data_age_desaturation as i32,
            );

            // Raw-to-physical conversion
            gl.uniform_1_f32(Some(&self.uniforms.offset), self.current.data_offset);
            gl.uniform_1_f32(Some(&self.uniforms.scale), self.current.data_scale);

            // Sweep animation uniforms — enable even without prev data; the 1x1
            // placeholder texture returns 0.0 (below-threshold sentinel -> transparent),
            // so the first sweep progressively reveals against a blank background.
            let sweep_on = sweep_info.is_some();
            gl.uniform_1_i32(Some(&self.uniforms.sweep_enabled), sweep_on as i32);
            let (sweep_az, sweep_start) = sweep_info.unwrap_or((0.0, 0.0));
            gl.uniform_1_f32(Some(&self.uniforms.sweep_azimuth), sweep_az);
            gl.uniform_1_f32(Some(&self.uniforms.sweep_start), sweep_start);
            gl.uniform_1_f32(Some(&self.uniforms.prev_offset), self.prev.data_offset);
            gl.uniform_1_f32(Some(&self.uniforms.prev_scale), self.prev.data_scale);
            gl.uniform_1_f32(
                Some(&self.uniforms.prev_gate_count),
                self.prev.gate_count as f32,
            );
            gl.uniform_1_f32(
                Some(&self.uniforms.prev_azimuth_count),
                self.prev.azimuth_count as f32,
            );
            gl.uniform_1_f32(
                Some(&self.uniforms.prev_first_gate_km),
                self.prev.first_gate_km as f32,
            );
            gl.uniform_1_f32(
                Some(&self.uniforms.prev_gate_interval_km),
                self.prev.gate_interval_km as f32,
            );
            gl.uniform_1_f32(
                Some(&self.uniforms.prev_max_range_km),
                self.prev.max_range_km as f32,
            );

            gl.uniform_1_f32(
                Some(&self.uniforms.sweep_chunk_boundary),
                sweep_chunk_boundary.unwrap_or(-1.0),
            );

            gl.uniform_1_f32(
                Some(&self.uniforms.azimuth_spacing_deg),
                self.current.azimuth_spacing_deg,
            );
            gl.uniform_1_f32(
                Some(&self.uniforms.prev_azimuth_spacing_deg),
                self.prev.azimuth_spacing_deg,
            );

            // Draw fullscreen quad
            gl.draw_arrays(glow::TRIANGLES, 0, 6);

            // Unbind our resources so we don't interfere with egui
            gl.bind_vertex_array(None);
            gl.use_program(None);
            gl.active_texture(glow::TEXTURE0);
        }
    }
}

// ============================================================================
// Helpers
// ============================================================================

/// Cast an `&[f32]` to `&[u8]` for GL upload.
fn bytemuck_cast_slice(data: &[f32]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(data.as_ptr() as *const u8, data.len() * 4) }
}

/// Create an R32F texture with the given dimensions and data.
unsafe fn create_r32f_texture(
    gl: &glow::Context,
    width: i32,
    height: i32,
    data: &[f32],
) -> glow::Texture {
    let texture = gl.create_texture().expect("Cannot create texture");
    gl.bind_texture(glow::TEXTURE_2D, Some(texture));
    gl.tex_parameter_i32(
        glow::TEXTURE_2D,
        glow::TEXTURE_MIN_FILTER,
        glow::NEAREST as i32,
    );
    gl.tex_parameter_i32(
        glow::TEXTURE_2D,
        glow::TEXTURE_MAG_FILTER,
        glow::NEAREST as i32,
    );
    gl.tex_parameter_i32(
        glow::TEXTURE_2D,
        glow::TEXTURE_WRAP_S,
        glow::CLAMP_TO_EDGE as i32,
    );
    gl.tex_parameter_i32(
        glow::TEXTURE_2D,
        glow::TEXTURE_WRAP_T,
        glow::CLAMP_TO_EDGE as i32,
    );
    gl.tex_image_2d(
        glow::TEXTURE_2D,
        0,
        glow::R32F as i32,
        width,
        height,
        0,
        glow::RED,
        glow::FLOAT,
        glow::PixelUnpackData::Slice(Some(bytemuck_cast_slice(data))),
    );
    gl.bind_texture(glow::TEXTURE_2D, None);
    texture
}

/// Create an RGBA8 texture with the given dimensions and data.
unsafe fn create_rgba8_texture(
    gl: &glow::Context,
    width: i32,
    height: i32,
    data: &[u8],
) -> glow::Texture {
    let texture = gl.create_texture().expect("Cannot create texture");
    gl.bind_texture(glow::TEXTURE_2D, Some(texture));
    gl.tex_parameter_i32(
        glow::TEXTURE_2D,
        glow::TEXTURE_MIN_FILTER,
        glow::LINEAR as i32,
    );
    gl.tex_parameter_i32(
        glow::TEXTURE_2D,
        glow::TEXTURE_MAG_FILTER,
        glow::LINEAR as i32,
    );
    gl.tex_parameter_i32(
        glow::TEXTURE_2D,
        glow::TEXTURE_WRAP_S,
        glow::CLAMP_TO_EDGE as i32,
    );
    gl.tex_parameter_i32(
        glow::TEXTURE_2D,
        glow::TEXTURE_WRAP_T,
        glow::CLAMP_TO_EDGE as i32,
    );
    gl.tex_image_2d(
        glow::TEXTURE_2D,
        0,
        glow::RGBA as i32,
        width,
        height,
        0,
        glow::RGBA,
        glow::UNSIGNED_BYTE,
        glow::PixelUnpackData::Slice(Some(data)),
    );
    gl.bind_texture(glow::TEXTURE_2D, None);
    texture
}
