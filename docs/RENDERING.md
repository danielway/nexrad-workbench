# Rendering

The radar renderer takes raw NEXRAD gate values and paints a fully
shaded radar image directly on the GPU. The CPU never builds a polar→
Cartesian image; the fragment shader does the projection, the
interpolation, the raw→physical conversion, and the color lookup —
once per pixel, every frame.

Module: [src/nexrad/render/gpu_renderer/](../src/nexrad/render/gpu_renderer/).

## What the GPU does (and why)

For each on-screen pixel, the fragment shader:

1. Converts pixel position → polar (azimuth°, range km) about the
   radar center.
2. Range-checks against the loaded sweep's `[first_gate_km, max_range_km)`.
3. Locates the nearest azimuth radial via binary search over a sorted
   azimuth texture (10 iterations covers up to 1024 radials).
4. Samples the gate value from a 2D `R32F` texture (gate × azimuth).
5. Rejects sentinel values (`raw <= 1`: 0 = below threshold,
   1 = range-folded).
6. Converts raw → physical: `(raw - offset) / scale`.
7. Normalizes against the product's value range and looks up RGBA
   from a 1024-entry LUT texture (`GL_LINEAR`-filtered, no visible
   quantization).
8. Outputs premultiplied alpha.

This avoids the entire decode-to-image step every frame: pan, zoom,
opacity, palette, and sweep animation all reduce to uniform changes,
which is why frame-to-frame interaction has no perceptible cost.

### Why raw gate values on the GPU

Storing raw `u16` values cast to `f32` (rather than physical units)
preserves two properties the renderer relies on:

- The sentinels `0` and `1` survive transport intact, so the shader's
  `is_valid(v) = v > 1.5` test cleanly rejects them.
- The transform `(raw - offset) / scale` is linear, so bilinear
  interpolation in raw space and bilinear interpolation in physical
  space produce the same answer. The shader interpolates first and
  converts last.

The same trick lets `value_at_polar()` and `detect_storm_cells()` —
both CPU consumers — share the GPU's `gate_values` buffer without
maintaining a parallel "physical units" copy.

## File layout

| File | Role |
| --- | --- |
| [`mod.rs`](../src/nexrad/render/gpu_renderer/mod.rs) | `RadarGpuRenderer` struct, GL setup, uniform table, `paint()` |
| [`shaders.rs`](../src/nexrad/render/gpu_renderer/shaders.rs) | All GLSL — vertex shader, fragment-shader builder, shared snippets |
| [`textures.rs`](../src/nexrad/render/gpu_renderer/textures.rs) | Texture upload entry points: data, previous data, LUT |
| [`inspect.rs`](../src/nexrad/render/gpu_renderer/inspect.rs) | CPU-side polar lookups (inspector hover, storm-cell detection) — thin binders over the pure lookup math in [`core::canvas`](../src/core/canvas.rs) |

Everything else under [src/nexrad/](../src/nexrad/) feeds this module
or composes its output:

- The decode worker pool produces the gate-value/azimuth/time arrays
  consumed by `update_data()`.
- The globe-mode renderer ([`globe_radar_renderer.rs`](../src/nexrad/render/globe_radar_renderer.rs))
  reuses the GLSL snippets exported from `shaders.rs` (`FRAGMENT_PREAMBLE`,
  `SAMPLE_DATA_P`, `IS_VALID`, `FIND_NEAREST_AZ_P`, `FIND_BRACKET_AZ_P`,
  `RAW_TO_PHYSICAL`, `COLOR_LOOKUP`, `PREMULTIPLIED_ALPHA_OUTPUT`) so
  the flat and globe paths cannot drift on lookup or color logic. Only
  the coordinate source differs (vertex attribute vs. screen-space math).

## GL resources

`RadarGpuRenderer::new()` builds, once per app session:

- A linked GL program from `VERTEX_SHADER` and the dynamically
  assembled flat fragment shader (see `build_flat_fragment_shader`
  in [shaders.rs](../src/nexrad/render/gpu_renderer/shaders.rs)).
- A fullscreen-quad VBO + VAO. `paint()` always draws this quad; the
  fragment shader decides what's inside the radar disc and what's
  transparent.
- 1×1 placeholder textures for `data`, `azimuth`, `lut`,
  `prev_data`, `prev_azimuth`. The placeholders matter — they keep
  the shader's logic uniform even when no previous sweep has been
  uploaded yet (sampling the 1×1 prev returns 0, which the sentinel
  test discards as transparent).

Texture units are wired permanently:

| Unit | Sampler | Format |
| --- | --- | --- |
| 0 | `u_data_tex` | R32F (gate × azimuth) |
| 1 | `u_lut_tex` | RGBA8 (1024 × 1) |
| 2 | `u_azimuth_tex` | R32F (N × 1) |
| 3 | `u_prev_data_tex` | R32F (gate × azimuth) |
| 4 | `u_prev_azimuth_tex` | R32F (N × 1) |

When data arrives, `update_data()` deletes and re-creates the relevant
textures rather than calling `tex_sub_image_2d`. Sweep dimensions
change between scans (different VCPs, partial live sweeps), so the
allocation cost is unavoidable and the simpler reallocate-on-update
contract avoids dimension-mismatch bugs.

## Sweep state and the previous-sweep slot

Two parallel sets of state — `current` and `prev` — back the radar
disc. Each holds:

- Spatial metadata: `first_gate_km`, `gate_interval_km`,
  `max_range_km`, `gate_count`, `azimuth_count`, `azimuth_spacing_deg`.
- Raw→physical conversion: `data_offset`, `data_scale`.
- Identity: `sweep_id` (used by the orchestration layer to tell whether
  a re-render is needed).

CPU shadow copies of `azimuths`, `gate_values`, and `radial_times`
also exist for both sweeps. They power inspector hovers and storm-cell
detection without round-tripping the GPU.

The fragment shader picks current vs. previous per-pixel based on
`u_sweep_enabled` and the swept-arc test, then funnels both branches
through the same lookup/color/conversion code path using `s_*` locals
that alias either the current or previous uniforms. This was a
deliberate refactor: previously the two paths were duplicated and
drifted (different range checks, different interpolation rules).

### `azimuth_spacing_deg` is a threshold, not a layout

The shader's azimuth search is binary-search over the sorted azimuth
texture. It returns "no radial" when the nearest hit is more than
`azimuth_spacing_deg * 1.5` from the target. Crucially this spacing
is the **median spacing between adjacent radials**, supplied by the
decoder, not `360 / azimuth_count`. The two diverge for partial live
sweeps (where the radials cover < 360°) and clustered sweeps; using
the index-derived value would either reject valid hits or accept
gaps as data.

## Sweep animation and live data

`paint()` accepts two optional inputs:

- `sweep_info: Option<(sweep_azimuth, sweep_start)>` — the current
  rotating-line angle and the angle where data collection began.
- `sweep_chunk_boundary: Option<f32>` — the azimuth corresponding to
  the most recent uploaded chunk's edge. `-1.0` disables.

When sweep is on, every pixel computes
`pixel_from_start = (azimuth - sweep_start) mod 360`. If this is past
the swept arc, the pixel comes from the previous sweep; otherwise
from the current sweep. The first sweep ever — when no previous data
exists — works against the 1×1 placeholder, so the disc fills in
progressively against transparent background.

### Data-age desaturation

When `data_age_desaturation` is on, sweep animation is effectively
active for the current view, and the chunk boundary is known, three
angular zones get different treatments. Effective animation requires
the usual live/mode gates **and** — with a fixed elevation filter —
that the antenna is currently collecting that cut (so age fades stop
between visits while other tilts collect).

```
   [fresh data] ← received edge ← [gap] ← now line ← [fade 90°] ← [no desat]
     saturated         fully desat         gradient → 0%        previous sweep
```

The "gap" between the received-data edge and the now-line gets a flat
desaturation (S3 chunks haven't arrived yet). The 90° wedge ahead of
the now-line — the oldest data, about to be overwritten — fades from
strong desaturation back to none. When no chunk boundary is supplied,
a fallback applies a uniform age-based desaturation to the trailing
quarter of the rotation.

## Update entry points

These are the only mutators the orchestration layer calls:

| Method | Effect |
| --- | --- |
| `update_data(...)` | Replace the current sweep's textures + metadata |
| `update_previous_data(...)` | Replace the previous-sweep textures + metadata |
| `promote_current_to_previous(gl)` | Copy current → previous (e.g. on elevation transition during live streaming) |
| `set_current_sweep_id(Some(id))` | Tag the loaded sweep so the coordinator can detect re-render needs |
| `update_color_table(gl, product)` | Rebuild the LUT for a new product |
| `clear_data()` | Drop both sweep slots (e.g. site change) |
| `clear_previous_data()` | Drop just the previous slot |

`update_color_table` builds a 1024-entry RGBA LUT. Reflectivity gets
a custom OKLab-interpolated palette with an alpha ramp at low values
([`color_table::build_reflectivity_lut`](../src/nexrad/render/color_table.rs));
other products use a continuous color scale evaluated at LUT positions.

## Painting

`RadarGpuRenderer::paint()` runs inside an `egui_glow::CallbackFn`
issued by [`ui/canvas.rs::draw_radar_gpu`](../src/ui/canvas.rs). The
caller passes the radar center and radius in **physical pixels** —
the shader works in pixel space, not points, so the call site
multiplies through `pixels_per_point` before invoking `paint()`.

The radar's max range in km comes from the loaded data, not a fixed
constant. The canvas projects `radar_lon ± lon_range` to screen pixels
to compute the on-screen radius, so the shader's pixel-to-km mapping
matches the geographic projection exactly at any zoom.

`egui_glow` saves and restores its GL state across paint callbacks,
so the renderer doesn't need to. It does explicitly:

- Use its own program and VAO.
- Enable premultiplied-alpha blending (egui's compositing convention).
- Disable the scissor test (the fragment shader masks the disc itself).
- Unbind its program/VAO and reset to texture unit 0 on the way out.

## CPU lookups

[`inspect.rs`](../src/nexrad/render/gpu_renderer/inspect.rs) exposes:

- `value_at_polar(azimuth_deg, range_km, sweep_params)` — physical
  units, or `None` for sentinels / out-of-range / behind the sweep
  line in a partial sweep.
- `collection_time_at_polar(azimuth_deg, sweep_params)` — Unix seconds
  for the radial that produced this azimuth.
- `detect_storm_cells(radar_lat, radar_lon, threshold_dbz)` — adapter
  over [`detection`](../src/nexrad/detection/) that wraps the CPU
  shadow data into a `DetectionInput`.

The inspector path uses `find_nearest_azimuth_index` (the CPU twin of
`FIND_NEAREST_AZ_P`, in [`core::canvas`](../src/core/canvas.rs) — the
pure lookup math lives there; `inspect.rs` binds it to the renderer's
CPU shadow buffers) which applies the same `< spacing * 1.5` gap rule
as the shader, so hover values match what's drawn. Negative azimuths
in the array — padding slots from the live partial-sweep path — are
skipped.

The previous-sweep CPU lookups use evenly-spaced azimuth indexing
rather than searching the array, mirroring the shader's prev-sweep
path. This is correct because the previous-sweep slot only ever
holds completed full sweeps with regular spacing.

## 3D rendering

The flat 2D path described above is the default. There are two
additional GPU paths for the globe view, both implemented as separate
renderers that compose with the flat one:

| Renderer | File | What it draws |
| --- | --- | --- |
| `RadarGpuRenderer` | [`gpu_renderer/`](../src/nexrad/render/gpu_renderer/) | Flat 2D radar disc (the default) |
| `GlobeRadarRenderer` | [`globe_radar_renderer.rs`](../src/nexrad/render/globe_radar_renderer.rs) | Single-elevation radar patch on a 3D sphere |
| `VolumeRayRenderer` | [`volume_ray_renderer.rs`](../src/nexrad/render/volume_ray_renderer.rs) | Full volumetric ray-march of all elevations |

In globe mode the paint callback in [`ui/canvas_overlays/globe.rs`](../src/ui/canvas_overlays/globe.rs)
draws the sphere and geo lines first, then picks one of the two radar
paths based on `volume_3d_enabled`: volumetric if enabled and a volume
is loaded, otherwise the surface patch. Both reuse the flat renderer's
already-uploaded GPU textures and metadata — there's only one source of
truth for sweep data.

### Surface patch — `GlobeRadarRenderer`

This is the natural extension of the flat renderer to a curved surface.
A spherical-cap mesh is generated once per site change in
[`generate_radar_patch`](../src/nexrad/render/globe_radar_renderer.rs):
`PATCH_AZ_STEPS = 180` × `PATCH_RANGE_STEPS = 60` great-circle samples
plus a center vertex. Each vertex carries five floats:
`[x, y, z, azimuth_deg, range_km]`. The vertex shader passes
`(azimuth, range)` straight through; the fragment shader reads it as
`v_polar` instead of computing it from screen-space pixel math like
the flat path does.

Everything else is shared. The fragment shader is built from the same
GLSL snippets — `FRAGMENT_PREAMBLE`, `SAMPLE_DATA_P`, `IS_VALID`,
`FIND_NEAREST_AZ_P`, `FIND_BRACKET_AZ_P`, `RAW_TO_PHYSICAL`,
`COLOR_LOOKUP`, `PREMULTIPLIED_ALPHA_OUTPUT` — exported from
[`gpu_renderer/shaders.rs`](../src/nexrad/render/gpu_renderer/shaders.rs).
Lookup, interpolation rules, sentinel handling, raw→physical
conversion, and palette lookup are shared by construction; the flat
and globe paths cannot drift on those rules.

`GlobeRadarRenderer::paint()` binds the *flat renderer's* `data`,
`lut`, and `azimuth` textures (units 0/1/2) and uploads scalar
metadata via accessors on the flat renderer (`gate_count()`,
`first_gate_km()`, `data_offset()`, `azimuth_spacing_deg()`, …).
Z-fighting against the unit sphere is avoided by lifting the patch to
`PATCH_RADIUS = 1.003`. Depth test is on but depth-write is off so
later transparent layers composite correctly. The mesh is rebuilt
only when `(lat, lon, max_range_km)` changes — all per-frame work is
uniforms + a single `draw_elements`.

The surface patch shows a single elevation. Sweep animation, partial
live sweeps, and the previous-sweep slot are 2D-only: the fragment
shader has no `use_prev` branch and no sweep-line uniforms.

### Volumetric — `VolumeRayRenderer`

When the user enables 3D volume rendering, the entire volume coverage
pattern (every elevation sweep) is rendered together as a
semi-transparent shell using ray marching.

**Assembly (`core::volume_plan`, executed by the worker packer).** The
shader's addressing assumes two things raw NEXRAD data does not
provide, so the pure decisions in `core/volume_plan.rs` establish them
before upload:

- **Ascending, deduped elevations.** Sweeps arrive ordered by
  elevation *number*, which SAILS/MRLE rescans and split cuts leave
  non-monotonic in *angle*. `plan_volume_sweeps` sorts by true angle
  and keeps one sweep per distinct angle (within 0.15°), preferring
  the greatest range coverage — the surveillance half of a split cut —
  and breaking ties toward the freshest rescan.
- **A uniform azimuth grid.** Radial azimuths are irregular, start at
  an arbitrary angle, and can be missing. `plan_azimuth_bins` maps
  each uniform bin to its nearest radial, wrap-aware, leaving a bin
  empty when nothing falls within 1.5× the median spacing (the same
  gap rule the 2D path applies). Empty bins carry the below-threshold
  sentinel, so gaps read as gaps rather than smear.

**Data layout.** Resampled sweep rows are concatenated into one linear
buffer and uploaded as a 4096-wide 2D texture in `R8UI` (1-byte
products) or `R16UI` (2-byte products), clamped to
`GL_MAX_TEXTURE_SIZE` (widening the row before giving up). Per-sweep
metadata — elevation, gate count, azimuth *bin* count, first-gate
range, gate interval, offset into the buffer, scale, offset — is
uploaded as fixed-length uniform arrays of length 25 (`MAX_SWEEPS`);
post-dedup no operational VCP comes close to that. Indexing uses
`y = idx / tex_width; x = idx - y * tex_width` and `texelFetch`: the
integer formats can't be hardware-filtered, so interpolation is done
explicitly in the shader.

**Ray march.** A fullscreen quad is drawn into a half-resolution FBO
(`RESOLUTION_DIVISOR = 2`). For each pixel:

1. Reconstruct a world-space ray from `u_inv_view_projection` and
   `u_camera_pos`.
2. Intersect against the inner sphere (data shell, ~1.003), the outer
   sphere (`inner + max_beam_height / earth_radius`), **and a
   site-centred bounding sphere** enclosing everything within one max
   slant range of the radar. All three extents come from the loaded
   sweeps via `derive_shell_extents` — not from fixed constants. The
   site sphere is both the early-out for far-away pixels and what
   keeps the step near gate scale: without it the interval was a
   globe-scale chord and a single step could span tens of kilometres.
3. `MAX_STEPS = 96` through the clipped interval, with the first
   sample jittered by interleaved gradient noise. A fixed offset put
   every ray's samples on shells of constant camera distance, which is
   what produced concentric rings and horizontal slabs.
4. At each step, invert the 4/3-earth beam geometry to recover
   `(elevation, slant range)` from the sample's ground arc and height
   — the reference implementation and its round-trip tests live in
   `core::volume_plan::beam_from_ground_position`, and the GLSL is a
   line-by-line transcription. Find the bracketing elevation pair
   (linear scan over ≤25 sweeps), bilinearly sample each in azimuth
   and range, convert to physical units with **that sweep's own**
   scale/offset, then blend across elevation.
5. Front-to-back Beer–Lambert compositing:
   `alpha = 1 - exp(-sigma * ds_km)` with extinction derived from a
   gamma curve over the value range. `ALPHA_CUTOFF = 0.99` early-
   terminates and a `density_cutoff` user knob suppresses thin
   reflectivity below a threshold.

Colour comes from the same LUT as the 2D path (the texture is borrowed
from the flat renderer), but **opacity does not** — the LUT's alpha
ramp saturates around 25 dBZ, so using it as extinction pinned nearly
every in-cloud sample to the same value and rendered the volume as a
first-hit isosurface. The half-resolution result is blit to the main
framebuffer with a trivial textured-quad shader.

**Trade-offs.** Half resolution is the dominant performance lever —
each pixel walks the ray independently with no acceleration structure,
and 4× fewer pixels means 4× fewer marches. Sampling costs four
`texelFetch` calls per sweep per step; the site-sphere clip pays for
that by cutting the wasted march outside the radar domain. Resampling
azimuth on the CPU keeps the shader's bin index pure arithmetic rather
than a per-step binary search, at no memory cost for a full-rotation
sweep (bin count equals radial count). The packed-texture layout
sidesteps WebGL2's lack of 3D texture arrays for integer formats, at
the cost of computing a 1D→2D index in the shader.

**Why two 3D renderers.** The surface patch is much cheaper than the
volume march and is what the user sees most of the time on the globe;
the volume renderer only kicks in when explicitly enabled. They share
no GL resources beyond the flat renderer's data and LUT textures.

## Constraints inherited from the WASM target

- WebGL2 only; no compute shaders, no SSBOs. All data flows via 2D
  textures.
- `R32F` requires the `EXT_color_buffer_float` extension for render
  targets, but here it's only used as a sampling source, which is in
  core WebGL2.
- The fragment shader's azimuth binary search has a fixed depth of
  10 iterations because GLSL ES 3.00 disallows non-constant loop
  bounds. 10 covers up to 1024 radials, well above any operational
  VCP's per-sweep count.
- Sampler access has to live in uniform control flow, so the dual
  current/previous branches use explicit `if (use_prev) { ... } else
  { ... }` blocks per sample call rather than a sampler variable.
