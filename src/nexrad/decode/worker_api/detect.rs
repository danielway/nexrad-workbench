//! WASM exports for storm cell detection: on one cut of a volume
//! (`worker_detect`, one site on the map) and on the composite grid of
//! several sites' cuts (`worker_detect_composite`).
//!
//! The main thread decides which cut (the volume's lowest reflectivity cut,
//! see `core::storm_cells`) and where to read it: the stored sweep blob in
//! IndexedDB, or the live accumulator while the stream is still filling the
//! cut. The detector (`crate::storm_cells`) runs here, off the UI thread.
//!
//! A volume request (`volumeCuts` non-empty, always a stored cut) also
//! samples the volume's other reflectivity tilts over the cells' footprints
//! for their columns (`crate::storm_cells::volume`). It detects the cut
//! again, since the footprints do not cross the worker boundary; the main
//! thread asks for it only after the cells themselves are shown.

use super::ingest::with_chunk_accum;
use super::*;
use crate::storm_cells::volume::{TiltInput, TiltValues};
use crate::storm_cells::{
    detect_cells, detect_cells_for_volume, DetectionInput, DetectionParams, StormCellsResult,
};

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
    /// For a volume request, the cuts to sample (the detected cut among
    /// them); empty for cells alone.
    #[serde(default)]
    pub volume_cuts: Vec<u8>,
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
    /// Reading the other tilts of a volume request.
    pub volume_fetch_ms: f64,
    /// Sampling the other tilts over the footprints.
    pub volume_ms: f64,
}

/// One tilt's raw reflectivity codes, owned, ready for [`TiltInput`].
struct TiltData {
    elevation_number: u8,
    azimuths: Vec<f32>,
    values: RawValues,
    azimuth_count: usize,
    gate_count: usize,
    first_gate_km: f64,
    gate_interval_km: f64,
    scale: f32,
    offset: f32,
    elevation_deg: f32,
}

enum RawValues {
    U8(Vec<u8>),
    U16(Vec<u16>),
}

impl TiltData {
    fn input(&self) -> TiltInput<'_> {
        TiltInput {
            elevation_number: self.elevation_number,
            elevation_deg: self.elevation_deg,
            azimuths: &self.azimuths,
            values: match &self.values {
                RawValues::U8(v) => TiltValues::U8(v),
                RawValues::U16(v) => TiltValues::U16(v),
            },
            azimuth_count: self.azimuth_count,
            gate_count: self.gate_count,
            first_gate_km: self.first_gate_km,
            gate_interval_km: self.gate_interval_km,
            data_scale: self.scale,
            data_offset: self.offset,
        }
    }
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
        let volume = !p.volume_cuts.is_empty();
        let live = if p.input == "live" && !volume {
            live_cut(&scan_key.site.0, p.elevation_number)
        } else {
            None
        };
        let complete = live.is_none();
        let cut = match live {
            Some(cut) => cut,
            None => stored_cut(scan_key.clone(), p.elevation_number).await?,
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
        let (mut result, sampler) = if volume {
            let (r, s) =
                detect_cells_for_volume(&input, &p.params, p.elevation_number, cut.elevation_deg);
            (r, Some(s))
        } else {
            (detect_cells(&input, &p.params), None)
        };
        let detect_ms = t_detect.elapsed().as_secs_f64() * 1000.0;

        // The volume's other tilts, read together, then sampled one at a
        // time so only one tilt's codes are in wasm memory at once.
        let (mut volume_fetch_ms, mut volume_ms, mut tilts) = (0.0, 0.0, 0);
        if let Some(mut sampler) = sampler {
            let t_vfetch = web_time::Instant::now();
            let others: Vec<u8> = p
                .volume_cuts
                .iter()
                .copied()
                .filter(|&n| n != p.elevation_number)
                .collect();
            let blobs = futures_util::future::join_all(
                others.iter().map(|&n| stored_blob(scan_key.clone(), n)),
            )
            .await;
            volume_fetch_ms = t_vfetch.elapsed().as_secs_f64() * 1000.0;
            for (&n, blob) in others.iter().zip(blobs) {
                let t = web_time::Instant::now();
                match blob.and_then(|b| tilt_from_blob(&b, n)) {
                    Ok(tilt) => {
                        sampler.add_tilt(&tilt.input());
                        tilts += 1;
                    }
                    Err(e) => log::warn!("detect: volume tilt elev={n} skipped: {e:?}"),
                }
                volume_ms += t.elapsed().as_secs_f64() * 1000.0;
            }
            let t = web_time::Instant::now();
            // Whether these are all the volume's tilts is the main thread's
            // to say (it knows the plan).
            result.volume = Some(sampler.finish(false));
            volume_ms += t.elapsed().as_secs_f64() * 1000.0;
        }
        result.source.sweep_id = Some(format!("{}|{}|{}", p.scan_key, p.elevation_number, PRODUCT));
        result.source.product = Some("Reflectivity".to_string());
        result.source.elevation_deg = Some(cut.elevation_deg);

        let total_ms = t_total.elapsed().as_secs_f64() * 1000.0;
        if volume {
            log::info!(
                "detect volume: {} {} cells over {} tilts | columns {:.1}ms, tilt fetch {:.1}ms, detect {:.1}ms, total {:.1}ms",
                p.scan_key,
                result.cells.len(),
                tilts + 1,
                volume_ms,
                volume_fetch_ms,
                detect_ms,
                total_ms,
            );
        }
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
            volume_fetch_ms,
            volume_ms,
        };
        serde_wasm_bindgen::to_value(&response)
            .map_err(|e| JsValue::from_str(&format!("Failed to serialize detect response: {}", e)))
    })
}

/// One site's cut in a `worker_detect_composite` request.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CompositeSiteParams {
    pub scan_key: String,
    pub elevation_number: u8,
    /// `"stored"` or `"live"`, as for `worker_detect`.
    pub input: String,
    pub radar_lat: f64,
    pub radar_lon: f64,
}

/// Parameters for `worker_detect_composite`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DetectCompositeParams {
    pub sites: Vec<CompositeSiteParams>,
    pub params: DetectionParams,
}

/// Detect storm cells on the composite grid of several sites' cuts
/// (`crate::storm_cells::composite`). Each cut is read as `worker_detect`
/// reads it. The reply has `worker_detect`'s shape; `azimuthCount` and
/// `gateCount` are the grid's rows and columns (0 with no echo), and
/// `complete` is whether every cut was complete.
///
/// Parameters (JS object): `{ sites: [{ scanKey, elevationNumber, input,
/// radarLat, radarLon }], params: DetectionParams }`
#[allow(unreachable_pub)] // wasm_bindgen export invoked from worker.js; must stay pub
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn worker_detect_composite(params: wasm_bindgen::JsValue) -> js_sys::Promise {
    use crate::storm_cells::composite::{detect_composite, CompositeCut};
    init_logger();
    wasm_bindgen_futures::future_to_promise(async move {
        let t_total = web_time::Instant::now();
        let p: DetectCompositeParams = serde_wasm_bindgen::from_value(params)
            .map_err(|e| JsValue::from_str(&format!("Invalid detect_composite params: {}", e)))?;

        let t_fetch = web_time::Instant::now();
        let mut cuts: Vec<(String, i64, CutData, &CompositeSiteParams)> = Vec::new();
        let mut complete = true;
        for site in &p.sites {
            let scan_key = ScanKey::from_storage_key(&site.scan_key)
                .map_err(|e| JsValue::from_str(&format!("Invalid scanKey: {e}")))?;
            let live = if site.input == "live" {
                live_cut(&scan_key.site.0, site.elevation_number)
            } else {
                None
            };
            complete &= live.is_none();
            let id = scan_key.site.0.clone();
            let time_ms = scan_key.scan_start.0;
            let cut = match live {
                Some(cut) => cut,
                None => stored_cut(scan_key, site.elevation_number).await?,
            };
            cuts.push((id, time_ms, cut, site));
        }
        let fetch_ms = t_fetch.elapsed().as_secs_f64() * 1000.0;

        let t_detect = web_time::Instant::now();
        let inputs: Vec<CompositeCut> = cuts
            .iter()
            .map(|(id, time_ms, cut, site)| CompositeCut {
                site: id,
                time_ms: *time_ms,
                sweep: DetectionInput {
                    azimuths: &cut.azimuths,
                    gate_values: &cut.gate_values,
                    azimuth_count: cut.azimuth_count,
                    gate_count: cut.gate_count,
                    first_gate_km: cut.first_gate_km,
                    gate_interval_km: cut.gate_interval_km,
                    data_scale: cut.scale,
                    data_offset: cut.offset,
                    radar_lat: site.radar_lat,
                    radar_lon: site.radar_lon,
                    azimuth_spacing_deg: None,
                },
            })
            .collect();
        let (mut result, grid) = detect_composite(&inputs, &p.params);
        let detect_ms = t_detect.elapsed().as_secs_f64() * 1000.0;
        let (rows, cols) = grid.unwrap_or((0, 0));
        result.source.sweep_id = Some(
            p.sites
                .iter()
                .map(|s| format!("{}|{}", s.scan_key, s.elevation_number))
                .collect::<Vec<_>>()
                .join(" + "),
        );
        result.source.product = Some("Reflectivity".to_string());
        result.source.elevation_deg = cuts
            .iter()
            .map(|(_, _, c, _)| c.elevation_deg)
            .reduce(f32::min);

        let total_ms = t_total.elapsed().as_secs_f64() * 1000.0;
        log::debug!(
            "detect_composite: {} sites ({}) grid {}x{} ({} cells of 0.5 km), {} cells in {} groups (envelope {:.0} dBZ) | detect {:.1}ms, fetch {:.1}ms, total {:.1}ms",
            cuts.len(),
            if complete { "complete" } else { "partial" },
            rows,
            cols,
            rows * cols,
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
            azimuth_count: rows as u32,
            gate_count: cols as u32,
            complete,
            // Scene cells have no column.
            volume_fetch_ms: 0.0,
            volume_ms: 0.0,
        };
        serde_wasm_bindgen::to_value(&response).map_err(|e| {
            JsValue::from_str(&format!(
                "Failed to serialize detect_composite response: {}",
                e
            ))
        })
    })
}

/// `site`'s live accumulator's radials for `elevation_number`, or `None` when
/// it no longer holds them (the cut was flushed to IndexedDB, or no stream).
/// The request's scan key names the site.
fn live_cut(site: &str, elevation_number: u8) -> Option<CutData> {
    use crate::nexrad::color_table::product_from_str;
    use crate::nexrad::decode::record_decode::extract_sweep_data_from_sorted;

    with_chunk_accum(site, |accum| {
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
    let blob = stored_blob(scan_key, elevation_number).await?;
    let (header, azimuths) = blob_header(&blob)?;
    let (az, gc) = (header.azimuth_count as usize, header.gate_count as usize);
    let gate_values: Vec<f32> = match blob_values(&blob, &header)? {
        RawValues::U8(raw) => raw.into_iter().map(f32::from).collect(),
        RawValues::U16(raw) => raw.into_iter().map(f32::from).collect(),
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

/// A stored tilt of a volume request, its codes kept in their word size.
fn tilt_from_blob(blob: &js_sys::ArrayBuffer, elevation_number: u8) -> Result<TiltData, JsValue> {
    let (header, azimuths) = blob_header(blob)?;
    Ok(TiltData {
        elevation_number,
        values: blob_values(blob, &header)?,
        azimuths,
        azimuth_count: header.azimuth_count as usize,
        gate_count: header.gate_count as usize,
        first_gate_km: header.first_gate_range_km,
        gate_interval_km: header.gate_interval_km,
        scale: header.scale,
        offset: header.offset,
        elevation_deg: header.mean_elevation,
    })
}

/// The stored reflectivity sweep blob of a cut.
async fn stored_blob(
    scan_key: ScanKey,
    elevation_number: u8,
) -> Result<js_sys::ArrayBuffer, JsValue> {
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
    Ok(blob)
}

/// A blob's header and azimuths, checked against its length.
fn blob_header(blob: &js_sys::ArrayBuffer) -> Result<(SweepHeader, Vec<f32>), JsValue> {
    if blob.byte_length() < 72 {
        return Err(JsValue::from_str("Sweep blob too small for its header"));
    }
    let header_bytes = {
        let view = js_sys::Uint8Array::new_with_byte_offset_and_length(blob, 0, 72);
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
        blob,
        header.azimuths_offset,
        header.azimuth_count,
    )
    .copy_to(&mut azimuths);
    Ok((header, azimuths))
}

/// A blob's raw gate codes in their word size.
fn blob_values(blob: &js_sys::ArrayBuffer, header: &SweepHeader) -> Result<RawValues, JsValue> {
    let n = header.azimuth_count as usize * header.gate_count as usize;
    let total = n as u32;
    Ok(if header.data_word_size == 1 {
        let mut raw = vec![0u8; n];
        js_sys::Uint8Array::new_with_byte_offset_and_length(blob, header.gate_values_offset, total)
            .copy_to(&mut raw);
        RawValues::U8(raw)
    } else {
        let mut raw = vec![0u16; n];
        js_sys::Uint16Array::new_with_byte_offset_and_length(
            blob,
            header.gate_values_offset,
            total,
        )
        .copy_to(&mut raw);
        RawValues::U16(raw)
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

    #[wasm_bindgen_test]
    fn detect_composite_params_parse_per_site() {
        let json = serde_json::json!({
            "sites": [
                {
                    "scanKey": "KMLB|1728528900000",
                    "elevationNumber": 1,
                    "input": "stored",
                    "radarLat": 28.11,
                    "radarLon": -80.65,
                },
                {
                    "scanKey": "KTBW|1728529000000",
                    "elevationNumber": 2,
                    "input": "live",
                    "radarLat": 27.7,
                    "radarLon": -82.4,
                },
            ],
            "params": serde_json::to_value(DetectionParams::default()).unwrap(),
        });
        let p: DetectCompositeParams = serde_json::from_value(json).expect("deserialize");
        assert_eq!(p.sites.len(), 2);
        assert_eq!(p.sites[1].scan_key, "KTBW|1728529000000");
        assert_eq!(p.sites[1].elevation_number, 2);
        assert_eq!(p.sites[1].input, "live");
        assert_eq!(p.sites[0].radar_lon, -80.65);
        assert_eq!(p.params, DetectionParams::default());
    }
}
