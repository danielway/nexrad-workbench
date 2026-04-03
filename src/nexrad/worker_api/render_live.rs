//! WASM export for live (partial sweep) render from in-memory ChunkAccumulator.

use super::ingest::CHUNK_ACCUM;
use super::*;

/// Parameters for `worker_render_live`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RenderLiveParams {
    #[serde(default = "default_product")]
    product: String,
    #[serde(default)]
    elevation_number: Option<u8>,
}

/// Render the current partial sweep from the in-memory ChunkAccumulator.
///
/// This reads directly from memory (no IDB), so it's very fast (~1ms).
/// Returns the same RenderResponse shape as `worker_render`.
///
/// Parameters (JS object): `{ product: string, elevationNumber?: number }`
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn worker_render_live(params: wasm_bindgen::JsValue) -> Result<JsValue, JsValue> {
    use crate::nexrad::record_decode::extract_sweep_data_from_sorted;
    use nexrad_render::Product;

    let t_total = web_time::Instant::now();

    let p: RenderLiveParams = serde_wasm_bindgen::from_value(params)
        .map_err(|e| JsValue::from_str(&format!("Invalid render_live params: {}", e)))?;

    let product = match p.product.as_str() {
        "velocity" => Product::Velocity,
        "spectrum_width" => Product::SpectrumWidth,
        "differential_reflectivity" => Product::DifferentialReflectivity,
        "differential_phase" => Product::DifferentialPhase,
        "correlation_coefficient" => Product::CorrelationCoefficient,
        "clutter_filter_power" => Product::ClutterFilterPower,
        _ => Product::Reflectivity,
    };

    CHUNK_ACCUM.with(|cell| {
        let borrow = cell.borrow();
        let accum = borrow
            .as_ref()
            .ok_or_else(|| JsValue::from_str("No chunk accumulator active"))?;

        let target_elev = p
            .elevation_number
            .or(accum.current_elevation)
            .ok_or_else(|| JsValue::from_str("No elevation available in accumulator"))?;

        // With flush-on-transition, only the current elevation's radials
        // are in memory. Sort by azimuth for sweep extraction.
        let mut sorted: Vec<&::nexrad::model::data::Radial> = accum
            .current_radials
            .iter()
            .filter(|r| r.elevation_number() == target_elev)
            .collect();

        if sorted.is_empty() {
            return Err(JsValue::from_str(&format!(
                "No radials for elevation {} in accumulator",
                target_elev
            )));
        }

        sorted.sort_by(|a, b| {
            a.azimuth_angle_degrees()
                .partial_cmp(&b.azimuth_angle_degrees())
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let sweep = extract_sweep_data_from_sorted(&sorted, product).ok_or_else(|| {
            JsValue::from_str(&format!(
                "No {} data for elevation {} in accumulator",
                p.product, target_elev
            ))
        })?;

        // Marshal PrecomputedSweep into the same JS response format as worker_render
        let t_marshal = web_time::Instant::now();

        // Convert gate values to f32 array
        let gate_values_f32: Vec<f32> = match &sweep.gate_values {
            crate::data::keys::GateValues::U8(v) => v.iter().map(|&x| x as f32).collect(),
            crate::data::keys::GateValues::U16(v) => v.iter().map(|&x| x as f32).collect(),
        };

        let az_array = js_sys::Float32Array::from(sweep.azimuths.as_slice());
        let az_buf = az_array.buffer();

        let val_array = js_sys::Float32Array::from(gate_values_f32.as_slice());
        let val_buf = val_array.buffer();

        let rt_array = js_sys::Float64Array::from(sweep.radial_times.as_slice());
        let rt_buf = rt_array.buffer();

        let marshal_ms = t_marshal.elapsed().as_secs_f64() * 1000.0;
        let total_ms = t_total.elapsed().as_secs_f64() * 1000.0;

        let accum_total = accum.current_radials.len();
        let elev_radials = sorted.len();
        let product_radials = sweep.azimuth_count;
        let expected_values = sweep.azimuth_count as usize * sweep.gate_count as usize;
        let actual_values = gate_values_f32.len();
        log::debug!(
            "render_live: elev={} {} {}x{} accum_total={} elev_radials={} product_radials={} vals={}/{} az=[{:.1}..{:.1}] offset={} scale={} in {:.1}ms (marshal: {:.1}ms)",
            target_elev,
            p.product,
            sweep.azimuth_count,
            sweep.gate_count,
            accum_total,
            elev_radials,
            product_radials,
            actual_values,
            expected_values,
            sweep.azimuths.first().copied().unwrap_or(f32::NAN),
            sweep.azimuths.last().copied().unwrap_or(f32::NAN),
            sweep.offset,
            sweep.scale,
            total_ms,
            marshal_ms,
        );

        let response = RenderResponse {
            azimuth_count: sweep.azimuth_count,
            gate_count: sweep.gate_count,
            first_gate_range_km: sweep.first_gate_range_km,
            gate_interval_km: sweep.gate_interval_km,
            max_range_km: sweep.max_range_km,
            product: p.product,
            radial_count: sweep.radial_count,
            scale: sweep.scale as f64,
            offset: sweep.offset as f64,
            mean_elevation: sweep.mean_elevation as f64,
            sweep_start_secs: sweep.sweep_start_secs,
            sweep_end_secs: sweep.sweep_end_secs,
            fetch_ms: 0.0,
            deser_ms: 0.0,
            total_ms,
            marshal_ms,
        };
        let result = serde_wasm_bindgen::to_value(&response)
            .map_err(|e| JsValue::from_str(&format!("Failed to serialize response: {}", e)))?;
        js_sys::Reflect::set(&result, &"azimuths".into(), &az_buf).ok();
        js_sys::Reflect::set(&result, &"gateValues".into(), &val_buf).ok();
        js_sys::Reflect::set(&result, &"radialTimes".into(), &rt_buf).ok();
        Ok(result)
    })
}
