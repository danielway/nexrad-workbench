//! WASM export for live (partial sweep) render from in-memory ChunkAccumulator.

use super::ingest::with_chunk_accum;
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

/// What a live render request for `target` finds in the accumulator.
#[derive(Debug, PartialEq, Eq)]
enum LiveTarget {
    /// The accumulator holds radials for the elevation: draw them.
    Radials,
    /// The radials are gone because the elevation was flushed to IndexedDB
    /// (or the accumulator moved on to a later elevation) after the main
    /// thread asked. The request is stale, not wrong: the completing
    /// chunk's acknowledgement already asked for the stored sweep.
    Superseded,
    /// No radials and no sign the elevation was ever collected: a real error.
    Missing,
}

/// Classify a live render request. Pure, so the race it handles is testable.
///
/// Chunk ingests are queued in `worker.js` but `render_live` runs as soon as
/// its message arrives. When several chunks reach the worker together (the
/// backfill burst on joining mid-sweep, or any catch-up), the worker ingests
/// chunk k and the completing chunk k+1 back to back before the main thread
/// has read k's acknowledgement and posted `render_live` for it. By then
/// k+1 has flushed the elevation and dropped its radials.
fn classify_live_target(
    target: u8,
    has_radials: bool,
    current_elevation: Option<u8>,
    completed_elevations: &std::collections::HashSet<u8>,
) -> LiveTarget {
    if has_radials {
        LiveTarget::Radials
    } else if completed_elevations.contains(&target)
        || current_elevation.is_some_and(|current| current > target)
    {
        LiveTarget::Superseded
    } else {
        LiveTarget::Missing
    }
}

/// Response for a request that [`classify_live_target`] found superseded.
/// `worker.js` turns it into a `live_superseded` message, which the main
/// thread treats as a no-op.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LiveSupersededResponse {
    superseded: bool,
    elevation_number: u8,
    current_elevation: Option<u8>,
}

/// Render the current partial sweep from the in-memory ChunkAccumulator.
///
/// This reads directly from memory (no IDB), so it's very fast (~1ms).
/// Returns the same RenderResponse shape as `worker_render`, or
/// `{ superseded: true, elevationNumber, currentElevation }` when the
/// elevation was stored after the request was sent (see
/// [`classify_live_target`]). An elevation the accumulator never held is
/// still an error.
///
/// Parameters (JS object): `{ product: string, elevationNumber?: number }`
#[allow(unreachable_pub)] // wasm_bindgen export invoked from worker.js; must stay pub
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn worker_render_live(params: wasm_bindgen::JsValue) -> Result<JsValue, JsValue> {
    use crate::nexrad::color_table::product_from_str;
    use crate::nexrad::decode::record_decode::extract_sweep_data_from_sorted;

    let t_total = web_time::Instant::now();

    let p: RenderLiveParams = serde_wasm_bindgen::from_value(params)
        .map_err(|e| JsValue::from_str(&format!("Invalid render_live params: {}", e)))?;

    let product = product_from_str(&p.product);

    with_chunk_accum(|accum| {
        let accum = accum.ok_or_else(|| JsValue::from_str("No chunk accumulator active"))?;

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

        match classify_live_target(
            target_elev,
            !sorted.is_empty(),
            accum.current_elevation,
            &accum.completed_elevations,
        ) {
            LiveTarget::Radials => {}
            LiveTarget::Superseded => {
                log::debug!(
                    "render_live: elev={} already stored (current={:?}), request superseded",
                    target_elev,
                    accum.current_elevation,
                );
                let response = LiveSupersededResponse {
                    superseded: true,
                    elevation_number: target_elev,
                    current_elevation: accum.current_elevation,
                };
                return serde_wasm_bindgen::to_value(&response).map_err(|e| {
                    JsValue::from_str(&format!("Failed to serialize response: {}", e))
                });
            }
            LiveTarget::Missing => {
                return Err(JsValue::from_str(&format!(
                    "No radials for elevation {} in accumulator",
                    target_elev
                )));
            }
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
            crate::data::GateValues::U8(v) => v.iter().map(|&x| x as f32).collect(),
            crate::data::GateValues::U16(v) => v.iter().map(|&x| x as f32).collect(),
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

        // Median angular spacing between adjacent sorted radials. Using the
        // median (rather than arc / count) naturally handles wrap-around
        // partial sweeps: the big gap where the sorted array crosses the
        // uncovered arc sorts to the top and gets excluded by the median.
        let azimuth_spacing_deg = if sweep.azimuth_count > 1 {
            let az = sweep.azimuths.as_slice();
            let mut gaps: Vec<f32> = az.windows(2).map(|w| (w[1] - w[0]).abs()).collect();
            gaps.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            // Guard against a zero-gap median (collocated radials) or missing data.
            let median = gaps[gaps.len() / 2];
            if median > 0.0 {
                median
            } else {
                1.0
            }
        } else {
            1.0
        };

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
            azimuth_spacing_deg,
        };
        let result = serde_wasm_bindgen::to_value(&response)
            .map_err(|e| JsValue::from_str(&format!("Failed to serialize response: {}", e)))?;
        attach_buffer_field(&result, "azimuths", &az_buf);
        attach_buffer_field(&result, "gateValues", &val_buf);
        attach_buffer_field(&result, "radialTimes", &rt_buf);
        Ok(result)
    })
}

#[cfg(test)]
mod tests {
    use super::super::ingest::{set_chunk_accum, ChunkAccumulator};
    use super::*;
    use std::collections::HashSet;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn set(elevations: &[u8]) -> HashSet<u8> {
        elevations.iter().copied().collect()
    }

    #[wasm_bindgen_test]
    fn classify_draws_radials_when_present() {
        assert_eq!(
            classify_live_target(1, true, Some(1), &set(&[])),
            LiveTarget::Radials
        );
        // Radials win even for a completed elevation (backfill revisits).
        assert_eq!(
            classify_live_target(1, true, Some(2), &set(&[1])),
            LiveTarget::Radials
        );
    }

    #[wasm_bindgen_test]
    fn classify_stored_elevation_is_superseded() {
        // The completing chunk flushed elev 1 while it was still current.
        assert_eq!(
            classify_live_target(1, false, Some(1), &set(&[1])),
            LiveTarget::Superseded
        );
        // The accumulator has moved on to a later elevation.
        assert_eq!(
            classify_live_target(1, false, Some(2), &set(&[])),
            LiveTarget::Superseded
        );
    }

    #[wasm_bindgen_test]
    fn classify_never_seen_elevation_is_missing() {
        assert_eq!(
            classify_live_target(3, false, Some(1), &set(&[1])),
            LiveTarget::Missing
        );
        assert_eq!(
            classify_live_target(1, false, None, &set(&[])),
            LiveTarget::Missing
        );
        assert_eq!(
            classify_live_target(2, false, Some(2), &set(&[1])),
            LiveTarget::Missing
        );
    }

    fn accum(current: Option<u8>, completed: &[u8]) -> ChunkAccumulator {
        ChunkAccumulator {
            scan_key: crate::data::ScanKey::new(
                "KTLX",
                crate::data::UnixMillis::from_secs_f64(1_000.0),
            ),
            current_radials: Vec::new(),
            current_radial_metas: Vec::new(),
            current_elevation: current,
            completed_elevations: set(completed),
            completed_sweep_metas: Vec::new(),
            coverage: crate::core::live_coverage::LiveCoverage::default(),
            vcp: None,
            has_vcp: false,
            total_chunks: 12,
            total_size_bytes: 0,
            file_name: "test".to_string(),
        }
    }

    fn params(elevation: u8) -> JsValue {
        let obj = js_sys::Object::new();
        js_sys::Reflect::set(&obj, &"product".into(), &"reflectivity".into()).unwrap();
        js_sys::Reflect::set(&obj, &"elevationNumber".into(), &elevation.into()).unwrap();
        obj.into()
    }

    #[wasm_bindgen_test]
    fn render_live_after_completing_chunk_is_superseded_not_an_error() {
        // The burst: render_live(1) arrives after the chunk that completed
        // elevation 1 was ingested and its radials dropped.
        set_chunk_accum(Some(accum(Some(1), &[1])));
        let result = worker_render_live(params(1)).expect("superseded is not an error");
        let superseded = js_sys::Reflect::get(&result, &"superseded".into()).unwrap();
        assert_eq!(superseded.as_bool(), Some(true));
        let elev = js_sys::Reflect::get(&result, &"elevationNumber".into()).unwrap();
        assert_eq!(elev.as_f64(), Some(1.0));
        set_chunk_accum(None);
    }

    #[wasm_bindgen_test]
    fn render_live_for_never_seen_elevation_is_still_an_error() {
        set_chunk_accum(Some(accum(Some(1), &[])));
        let err = worker_render_live(params(1)).expect_err("nothing was ever collected");
        assert_eq!(
            err.as_string().as_deref(),
            Some("No radials for elevation 1 in accumulator")
        );
        set_chunk_accum(None);
    }
}
