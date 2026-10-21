//! WASM export for storm cell detection on one cut of a volume.
//!
//! The main thread decides which cut (the volume's lowest reflectivity cut,
//! see `core::storm_cells`) and where to read it: the stored sweep blob in
//! IndexedDB, or the live accumulator while the stream is still filling the
//! cut. The detector (`crate::storm_cells`) runs here, off the UI thread.

use super::ingest::with_chunk_accum;
use super::*;
use crate::storm_cells::{detect_cells, DetectionInput, DetectionParams, StormCellsResult};

/// Parameters for `worker_detect`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DetectParams {
    pub scan_key: String,
    pub elevation_number: u8,
    /// `"stored"` (IndexedDB) or `"live"` (the accumulator, falling back to
    /// IndexedDB when the stream has already finished the cut).
    pub input: String,
    pub params: DetectionParams,
    pub radar_lat: f64,
    pub radar_lon: f64,
}

/// Response from `worker_detect`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DetectResponse {
    pub result: StormCellsResult,
    pub fetch_ms: f64,
    pub detect_ms: f64,
    pub total_ms: f64,
    pub azimuth_count: u32,
    pub gate_count: u32,
    /// The cut was complete when read (stored, or flushed before this ran).
    pub complete: bool,
}

/// One cut's reflectivity, owned, ready for [`DetectionInput`].
struct CutData {
    azimuths: Vec<f32>,
    gate_values: Vec<f32>,
    azimuth_count: usize,
    gate_count: usize,
    first_gate_km: f64,
    gate_interval_km: f64,
    scale: f32,
    offset: f32,
    elevation_deg: f32,
}

const PRODUCT: &str = "reflectivity";

/// Detect storm cells on one reflectivity cut.
///
/// Parameters (JS object): `{ scanKey, elevationNumber, input: "stored" | "live",
/// params: DetectionParams, radarLat, radarLon }`
/// Returns (JS object): `{ result: StormCellsResult, fetchMs, detectMs, totalMs,
/// azimuthCount, gateCount, complete }`
#[allow(unreachable_pub)] // wasm_bindgen export invoked from worker.js; must stay pub
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn worker_detect(params: wasm_bindgen::JsValue) -> js_sys::Promise {
    init_logger();
    wasm_bindgen_futures::future_to_promise(async move {
        let t_total = web_time::Instant::now();
        let p: DetectParams = serde_wasm_bindgen::from_value(params)
            .map_err(|e| JsValue::from_str(&format!("Invalid detect params: {}", e)))?;
        let scan_key = ScanKey::from_storage_key(&p.scan_key)
            .map_err(|e| JsValue::from_str(&format!("Invalid scanKey: {e}")))?;

        let t_fetch = web_time::Instant::now();
        let live = if p.input == "live" {
            live_cut(p.elevation_number)
        } else {
            None
        };
        let complete = live.is_none();
        let cut = match live {
            Some(cut) => cut,
            None => stored_cut(scan_key, p.elevation_number).await?,
        };
        let fetch_ms = t_fetch.elapsed().as_secs_f64() * 1000.0;

        let t_detect = web_time::Instant::now();
        let input = DetectionInput {
            azimuths: &cut.azimuths,
            gate_values: &cut.gate_values,
            azimuth_count: cut.azimuth_count,
            gate_count: cut.gate_count,
            first_gate_km: cut.first_gate_km,
            gate_interval_km: cut.gate_interval_km,
            data_scale: cut.scale,
            data_offset: cut.offset,
            radar_lat: p.radar_lat,
            radar_lon: p.radar_lon,
            // Median gap of the supplied azimuths: right for a full sweep
            // and for a live partial alike.
            azimuth_spacing_deg: None,
        };
        let mut result = detect_cells(&input, &p.params);
        let detect_ms = t_detect.elapsed().as_secs_f64() * 1000.0;
        result.source.sweep_id = Some(format!("{}|{}|{}", p.scan_key, p.elevation_number, PRODUCT));
        result.source.product = Some("Reflectivity".to_string());
        result.source.elevation_deg = Some(cut.elevation_deg);

        let total_ms = t_total.elapsed().as_secs_f64() * 1000.0;
        log::info!(
            "detect: {} elev={} ({:.1}°) {} {}x{}, {} cells in {} groups (envelope {:.0} dBZ) | detect {:.1}ms, fetch {:.1}ms, total {:.1}ms",
            p.scan_key,
            p.elevation_number,
            cut.elevation_deg,
            if complete { "complete" } else { "partial" },
            cut.azimuth_count,
            cut.gate_count,
            result.cells.len(),
            result.groups.len(),
            p.params.threshold_dbz,
            detect_ms,
            fetch_ms,
            total_ms,
        );

        let response = DetectResponse {
            result,
            fetch_ms,
            detect_ms,
            total_ms,
            azimuth_count: cut.azimuth_count as u32,
            gate_count: cut.gate_count as u32,
            complete,
        };
        serde_wasm_bindgen::to_value(&response)
            .map_err(|e| JsValue::from_str(&format!("Failed to serialize detect response: {}", e)))
    })
}

/// The live accumulator's radials for `elevation_number`, or `None` when it
/// no longer holds them (the cut was flushed to IndexedDB, or no stream).
fn live_cut(elevation_number: u8) -> Option<CutData> {
    use crate::nexrad::color_table::product_from_str;
    use crate::nexrad::decode::record_decode::extract_sweep_data_from_sorted;

    with_chunk_accum(|accum| {
        let accum = accum?;
        let mut sorted: Vec<&::nexrad::model::data::Radial> = accum
            .current_radials
            .iter()
            .filter(|r| r.elevation_number() == elevation_number)
            .collect();
        if sorted.is_empty() {
            return None;
        }
        sorted.sort_by(|a, b| {
            a.azimuth_angle_degrees()
                .partial_cmp(&b.azimuth_angle_degrees())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let sweep = extract_sweep_data_from_sorted(&sorted, product_from_str(PRODUCT))?;
        let gate_values = match &sweep.gate_values {
            crate::data::GateValues::U8(v) => v.iter().map(|&x| x as f32).collect(),
            crate::data::GateValues::U16(v) => v.iter().map(|&x| x as f32).collect(),
        };
        Some(CutData {
            azimuths: sweep.azimuths,
            gate_values,
            azimuth_count: sweep.azimuth_count as usize,
            gate_count: sweep.gate_count as usize,
            first_gate_km: sweep.first_gate_range_km,
            gate_interval_km: sweep.gate_interval_km,
            scale: sweep.scale,
            offset: sweep.offset,
            elevation_deg: sweep.mean_elevation,
        })
    })
}

/// The stored reflectivity sweep blob for the cut, read the way `render`
/// reads it.
async fn stored_cut(scan_key: ScanKey, elevation_number: u8) -> Result<CutData, JsValue> {
    let store = idb_store().await?;
    let sweep_key = SweepDataKey::new(scan_key, elevation_number, PRODUCT);
    let blob = store
        .get_sweep(&sweep_key)
        .await
        .map_err(|e| JsValue::from_str(&format!("Failed to fetch sweep: {}", e)))?
        .ok_or_else(|| {
            worker_error(
                "not_found",
                format!("No stored reflectivity sweep for elev={elevation_number}"),
            )
        })?;
    if blob.byte_length() < 72 {
        return Err(JsValue::from_str("Sweep blob too small for its header"));
    }
    let header_bytes = {
        let view = js_sys::Uint8Array::new_with_byte_offset_and_length(&blob, 0, 72);
        let mut buf = [0u8; 72];
        view.copy_to(&mut buf);
        buf
    };
    let header = parse_sweep_header(&header_bytes)
        .map_err(|e| JsValue::from_str(&format!("Failed to parse sweep header: {}", e)))?;
    let az = header.azimuth_count as usize;
    let gc = header.gate_count as usize;
    let ws = header.data_word_size as usize;
    let expected = header.gate_values_offset as usize + az * gc * ws;
    if (blob.byte_length() as usize) < expected {
        return Err(JsValue::from_str(&format!(
            "Sweep blob too small: {} < {} expected",
            blob.byte_length(),
            expected
        )));
    }

    let mut azimuths = vec![0f32; az];
    js_sys::Float32Array::new_with_byte_offset_and_length(
        &blob,
        header.azimuths_offset,
        header.azimuth_count,
    )
    .copy_to(&mut azimuths);

    let total = (az * gc) as u32;
    let gate_values: Vec<f32> = if header.data_word_size == 1 {
        let mut raw = vec![0u8; az * gc];
        js_sys::Uint8Array::new_with_byte_offset_and_length(
            &blob,
            header.gate_values_offset,
            total,
        )
        .copy_to(&mut raw);
        raw.into_iter().map(f32::from).collect()
    } else {
        let mut raw = vec![0u16; az * gc];
        js_sys::Uint16Array::new_with_byte_offset_and_length(
            &blob,
            header.gate_values_offset,
            total,
        )
        .copy_to(&mut raw);
        raw.into_iter().map(f32::from).collect()
    };

    Ok(CutData {
        azimuths,
        gate_values,
        azimuth_count: az,
        gate_count: gc,
        first_gate_km: header.first_gate_range_km,
        gate_interval_km: header.gate_interval_km,
        scale: header.scale,
        offset: header.offset,
        elevation_deg: header.mean_elevation,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn detect_params_parse_with_full_detection_params() {
        let params = serde_json::to_value(DetectionParams {
            threshold_dbz: 40.0,
            ..DetectionParams::default()
        })
        .unwrap();
        let json = serde_json::json!({
            "scanKey": "KTBW|1728504000000",
            "elevationNumber": 1,
            "input": "live",
            "params": params,
            "radarLat": 27.7,
            "radarLon": -82.4,
        });
        let p: DetectParams = serde_json::from_value(json).expect("deserialize");
        assert_eq!(p.scan_key, "KTBW|1728504000000");
        assert_eq!(p.elevation_number, 1);
        assert_eq!(p.input, "live");
        assert_eq!(p.params.threshold_dbz, 40.0);
        assert_eq!(
            p.params,
            DetectionParams {
                threshold_dbz: 40.0,
                ..DetectionParams::default()
            }
        );
    }
}
