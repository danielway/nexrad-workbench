//! WebGL2 globe sphere renderer.
//!
//! Draws a unit-sphere with simple directional lighting inside an egui
//! PaintCallback. The sphere is rendered with depth-write so that
//! subsequent layers (geo lines, radar patch) are naturally occluded
//! on the far side.

use crate::geo::camera::GlobeCamera;
use glow::HasContext;
use std::sync::Arc;

/// Number of longitude segments for the UV sphere.
const LON_SEGMENTS: u32 = 64;
/// Number of latitude segments for the UV sphere.
const LAT_SEGMENTS: u32 = 32;

pub struct GlobeRenderer {
    program: glow::Program,
    vao: glow::VertexArray,
    _vbo: glow::Buffer,
    ibo: glow::Buffer,
    index_count: i32,
    u_view_projection: glow::UniformLocation,
    u_light_dir: glow::UniformLocation,
}

impl GlobeRenderer {
    /// Create the globe renderer: compile shaders, generate sphere mesh, upload.
    pub fn new(gl: &Arc<glow::Context>) -> Self {
        unsafe { Self::new_inner(gl) }
    }

    unsafe fn new_inner(gl: &Arc<glow::Context>) -> Self {
        // ── Shaders ─────────────────────────────────────────────
        let vert_src = r#"#version 300 es
precision highp float;

uniform mat4 u_view_projection;

in vec3 a_position;

out vec3 v_normal;

void main() {
    v_normal = a_position; // unit sphere: normal == position
    gl_Position = u_view_projection * vec4(a_position, 1.0);
}
"#;

        let frag_src = r#"#version 300 es
precision highp float;

uniform vec3 u_light_dir;

in vec3 v_normal;
out vec4 fragColor;

void main() {
    vec3 n = normalize(v_normal);
    float diffuse = max(dot(n, u_light_dir), 0.0);
    float ambient = 0.15;
    float light = ambient + (1.0 - ambient) * diffuse;

    // Dark blue-gray base
    vec3 base = vec3(0.07, 0.09, 0.13);
    fragColor = vec4(base * light, 1.0);
}
"#;

        let program = compile_program(gl, vert_src, frag_src);
        let u_view_projection = gl
            .get_uniform_location(program, "u_view_projection")
            .unwrap();
        let u_light_dir = gl.get_uniform_location(program, "u_light_dir").unwrap();

        // ── Sphere mesh ─────────────────────────────────────────
        let (vertices, indices) = generate_uv_sphere(LON_SEGMENTS, LAT_SEGMENTS);

        let vao = gl.create_vertex_array().unwrap();
        gl.bind_vertex_array(Some(vao));

        let vbo = gl.create_buffer().unwrap();
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
        gl.buffer_data_u8_slice(
            glow::ARRAY_BUFFER,
            cast_f32_to_u8(&vertices),
            glow::STATIC_DRAW,
        );

        // a_position: vec3 at location 0
        let a_position = gl.get_attrib_location(program, "a_position").unwrap();
        gl.enable_vertex_attrib_array(a_position);
        gl.vertex_attrib_pointer_f32(
            a_position,
            3,
            glow::FLOAT,
            false,
            3 * 4, // stride = 3 floats
            0,
        );

        let ibo = gl.create_buffer().unwrap();
        gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(ibo));
        gl.buffer_data_u8_slice(
            glow::ELEMENT_ARRAY_BUFFER,
            cast_u32_to_u8(&indices),
            glow::STATIC_DRAW,
        );

        gl.bind_vertex_array(None);

        Self {
            program,
            vao,
            _vbo: vbo,
            ibo,
            index_count: indices.len() as i32,
            u_view_projection,
            u_light_dir,
        }
    }

    /// Draw the globe sphere. Caller must have set the viewport.
    /// Enables depth test with depth writes.
    pub fn paint(&self, gl: &glow::Context, camera: &GlobeCamera) {
        unsafe {
            gl.enable(glow::DEPTH_TEST);
            gl.depth_func(glow::LEQUAL);
            gl.depth_mask(true);
            gl.clear(glow::DEPTH_BUFFER_BIT);

            // Back-face cull so inside isn't drawn
            gl.enable(glow::CULL_FACE);
            gl.cull_face(glow::BACK);

            gl.use_program(Some(self.program));
            gl.bind_vertex_array(Some(self.vao));
            gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(self.ibo));

            // View-projection matrix
            let vp = camera.view_projection();
            gl.uniform_matrix_4_f32_slice(
                Some(&self.u_view_projection),
                false,
                &vp.to_cols_array(),
            );

            // Light direction (fixed, upper-right)
            let light = glam::Vec3::new(0.4, 0.7, 0.6).normalize();
            gl.uniform_3_f32(Some(&self.u_light_dir), light.x, light.y, light.z);

            gl.draw_elements(glow::TRIANGLES, self.index_count, glow::UNSIGNED_INT, 0);

            gl.bind_vertex_array(None);
            gl.disable(glow::CULL_FACE);
            // Leave depth test enabled for subsequent layers (geo lines, radar)
        }
    }
}

/// Generate a UV sphere on the unit sphere.
/// Returns (positions: Vec<f32>, indices: Vec<u32>).
fn generate_uv_sphere(lon_segs: u32, lat_segs: u32) -> (Vec<f32>, Vec<u32>) {
    let mut verts = Vec::new();
    let mut idxs = Vec::new();

    for lat in 0..=lat_segs {
        let theta = std::f32::consts::PI * lat as f32 / lat_segs as f32; // 0..PI
        let sin_t = theta.sin();
        let cos_t = theta.cos();

        for lon in 0..=lon_segs {
            let phi = 2.0 * std::f32::consts::PI * lon as f32 / lon_segs as f32; // 0..2PI
            let x = sin_t * phi.sin();
            let y = cos_t;
            let z = sin_t * phi.cos();
            verts.push(x);
            verts.push(y);
            verts.push(z);
        }
    }

    for lat in 0..lat_segs {
        for lon in 0..lon_segs {
            let row0 = lat * (lon_segs + 1);
            let row1 = (lat + 1) * (lon_segs + 1);

            // Two triangles per quad
            idxs.push(row0 + lon);
            idxs.push(row1 + lon);
            idxs.push(row0 + lon + 1);

            idxs.push(row0 + lon + 1);
            idxs.push(row1 + lon);
            idxs.push(row1 + lon + 1);
        }
    }

    (verts, idxs)
}

/// Cast `&[f32]` to `&[u8]` for GL upload.
fn cast_f32_to_u8(data: &[f32]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(data.as_ptr() as *const u8, data.len() * 4) }
}

/// Cast `&[u32]` to `&[u8]` for GL upload.
fn cast_u32_to_u8(data: &[u32]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(data.as_ptr() as *const u8, data.len() * 4) }
}

/// Compile a GLSL program from vertex and fragment source.
pub(crate) unsafe fn compile_program(
    gl: &glow::Context,
    vert_src: &str,
    frag_src: &str,
) -> glow::Program {
    let program = gl.create_program().expect("create program");

    let vs = gl.create_shader(glow::VERTEX_SHADER).expect("create vs");
    gl.shader_source(vs, vert_src);
    gl.compile_shader(vs);
    if !gl.get_shader_compile_status(vs) {
        log::error!("Globe VS compile error: {}", gl.get_shader_info_log(vs));
    }

    let fs = gl.create_shader(glow::FRAGMENT_SHADER).expect("create fs");
    gl.shader_source(fs, frag_src);
    gl.compile_shader(fs);
    if !gl.get_shader_compile_status(fs) {
        log::error!("Globe FS compile error: {}", gl.get_shader_info_log(fs));
    }

    gl.attach_shader(program, vs);
    gl.attach_shader(program, fs);
    gl.link_program(program);
    if !gl.get_program_link_status(program) {
        log::error!("Globe link error: {}", gl.get_program_info_log(program));
    }

    gl.detach_shader(program, vs);
    gl.delete_shader(vs);
    gl.detach_shader(program, fs);
    gl.delete_shader(fs);

    program
}
