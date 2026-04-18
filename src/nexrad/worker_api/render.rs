//! WASM exports for render operations (single-elevation and volume).

use super::*;

/// Render a specific elevation from pre-computed sweep data in IndexedDB.
///
/// Called from the Web Worker via worker.js. Fetches a single pre-computed
/// sweep blob and returns the data for GPU upload — no decoding needed.
///
/// Parameters (JS object): `{ scanKey: string, elevationNumber: number, product: string }`
/// Returns (JS object): `{ azimuths: Float32Array, gateValues: Float32Array, azimuthCount, gateCount, ... }`
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn worker_render(params: wasm_bindgen::JsValue) -> js_sys::Promise {
    init_logger();
    wasm_bindgen_futures::future_to_promise(async move {
        let t_total = web_time::Instant::now();

        let p: RenderParams = serde_wasm_bindgen::from_value(params)
            .map_err(|e| JsValue::from_str(&format!("Invalid render params: {}", e)))?;
        let scan_key_str = p.scan_key;
        let elevation_number = p.elevation_number;
        let product_str = p.product;

        let scan_key = ScanKey::from_storage_key(&scan_key_str)
            .ok_or_else(|| wasm_bindgen::JsValue::from_str("Invalid scanKey format"))?;

        let store = idb_store().await?;

        // Fetch raw IDB ArrayBuffer (no Rust-side copy)
        let t_fetch = web_time::Instant::now();
        let sweep_key = SweepDataKey::new(scan_key, elevation_number, &product_str);
        let blob_buffer = store
            .get_sweep_as_js(&sweep_key.to_storage_key())
            .await
            .map_err(|e| wasm_bindgen::JsValue::from_str(&format!("Failed to fetch sweep: {}", e)))?
            .ok_or_else(|| {
                wasm_bindgen::JsValue::from_str(&format!(
                    "No pre-computed sweep for elev={} product={}",
                    elevation_number, product_str
                ))
            })?;
        let fetch_ms = t_fetch.elapsed().as_secs_f64() * 1000.0;
        let blob_len = blob_buffer.byte_length();

        // Parse header only (72 bytes) — no array allocations
        let t_deser = web_time::Instant::now();
        let header_bytes = {
            let view = js_sys::Uint8Array::new_with_byte_offset_and_length(&blob_buffer, 0, 72);
            let mut buf = [0u8; 72];
            view.copy_to(&mut buf);
            buf
        };
        let header = parse_sweep_header(&header_bytes).map_err(|e| {
            wasm_bindgen::JsValue::from_str(&format!("Failed to parse sweep header: {}", e))
        })?;

        // Validate full blob size
        let az = header.azimuth_count as usize;
        let gc = header.gate_count as usize;
        let ws = header.data_word_size as usize;
        let expected = header.gate_values_offset as usize + az * gc * ws;
        if (blob_len as usize) < expected {
            return Err(wasm_bindgen::JsValue::from_str(&format!(
                "Sweep blob too small: {} < {} expected",
                blob_len, expected
            )));
        }
        let deser_ms = t_deser.elapsed().as_secs_f64() * 1000.0;

        // Marshal: create typed array views over raw IDB ArrayBuffer
        let t_marshal = web_time::Instant::now();

        let az_view = js_sys::Float32Array::new_with_byte_offset_and_length(
            &blob_buffer,
            header.azimuths_offset,
            header.azimuth_count,
        );
        let az_buf = az_view.slice(0, header.azimuth_count).buffer();

        // Extract radial_times if present (format version >= 1)
        let rt_buf = if header.radial_times_offset > 0 {
            let rt_view = js_sys::Float64Array::new_with_byte_offset_and_length(
                &blob_buffer,
                header.radial_times_offset as u32,
                header.azimuth_count,
            );
            Some(rt_view.slice(0, header.azimuth_count).buffer())
        } else {
            None
        };

        // Convert native-width gate values to f32 for GPU upload
        let gate_count_total = header.azimuth_count * header.gate_count;
        let val_buf = if header.data_word_size == 1 {
            let u8_view = js_sys::Uint8Array::new_with_byte_offset_and_length(
                &blob_buffer,
                header.gate_values_offset,
                gate_count_total,
            );
            js_sys::Float32Array::new(&u8_view).buffer()
        } else {
            let u16_view = js_sys::Uint16Array::new_with_byte_offset_and_length(
                &blob_buffer,
                header.gate_values_offset,
                gate_count_total,
            );
            js_sys::Float32Array::new(&u16_view).buffer()
        };

        let marshal_ms = t_marshal.elapsed().as_secs_f64() * 1000.0;
        let total_ms = t_total.elapsed().as_secs_f64() * 1000.0;

        log::debug!(
            "render: elev={} {} {}x{} ({:.1}KB) in {:.1}ms | fetch {:.1} | deser {:.1} | marshal {:.1}",
            elevation_number, product_str,
            header.azimuth_count, header.gate_count,
            blob_len as f64 / 1024.0,
            total_ms, fetch_ms, deser_ms, marshal_ms,
        );

        // Serialize scalar fields, then attach ArrayBuffer fields separately
        let response = RenderResponse {
            azimuth_count: header.azimuth_count,
            gate_count: header.gate_count,
            first_gate_range_km: header.first_gate_range_km,
            gate_interval_km: header.gate_interval_km,
            max_range_km: header.max_range_km,
            product: product_str,
            radial_count: header.radial_count,
            scale: header.scale as f64,
            offset: header.offset as f64,
            mean_elevation: header.mean_elevation as f64,
            sweep_start_secs: header.sweep_start_secs,
            sweep_end_secs: header.sweep_end_secs,
            fetch_ms,
            deser_ms,
            total_ms,
            marshal_ms,
        };
        let result = serde_wasm_bindgen::to_value(&response)
            .map_err(|e| JsValue::from_str(&format!("Failed to serialize response: {}", e)))?;
        // ArrayBuffer fields must be set directly (not serializable via serde)
        js_sys::Reflect::set(&result, &"azimuths".into(), &az_buf).ok();
        js_sys::Reflect::set(&result, &"gateValues".into(), &val_buf).ok();
        if let Some(rt) = rt_buf {
            js_sys::Reflect::set(&result, &"radialTimes".into(), &rt).ok();
        }
        Ok(result)
    })
}

// ---------------------------------------------------------------------------
// Volume render (all elevations packed for ray marching)
// ---------------------------------------------------------------------------

/// Render all elevations for a scan, packing raw gate data into a single buffer
/// for volumetric ray-march rendering on the GPU.
///
/// Parameters (JS object): `{ scanKey: string, product: string, elevationNumbers: number[] }`
/// Returns (JS object): `{ buffer: ArrayBuffer, sweepMeta: [...], product, totalMs }`
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn worker_render_volume(params: wasm_bindgen::JsValue) -> js_sys::Promise {
    init_logger();
    wasm_bindgen_futures::future_to_promise(async move {
        let t_total = web_time::Instant::now();

        let p: RenderVolumeParams = serde_wasm_bindgen::from_value(params)
            .map_err(|e| JsValue::from_str(&format!("Invalid render_volume params: {}", e)))?;
        let scan_key_str = p.scan_key;
        let product_str = p.product;
        let elevation_numbers = p.elevation_numbers;

        let scan_key = ScanKey::from_storage_key(&scan_key_str)
            .ok_or_else(|| wasm_bindgen::JsValue::from_str("Invalid scanKey format"))?;

        let store = idb_store().await?;

        // Collect all sweep data into a packed buffer.
        // We keep native word size when all sweeps are u8 to halve transfer cost.
        // Only widen to u16 when at least one sweep has u16 data.
        let mut packed_data: Vec<u8> = Vec::new();
        let mut sweep_meta_vec: Vec<VolumeRenderSweepMeta> = Vec::new();
        let mut data_offset: u32 = 0; // offset in values (not bytes)
        let mut has_u16 = false;

        // First pass: read all sweep blobs and headers, determine word size
        struct SweepBlob {
            blob_buffer: js_sys::ArrayBuffer,
            header: SweepHeader,
            total_values: usize,
        }
        let mut sweep_blobs: Vec<SweepBlob> = Vec::new();

        for &elev_num in &elevation_numbers {
            let sweep_key = SweepDataKey::new(scan_key.clone(), elev_num, &product_str);
            let blob_buffer = match store.get_sweep_as_js(&sweep_key.to_storage_key()).await {
                Ok(Some(buf)) => buf,
                _ => continue, // skip missing elevations
            };

            let header_bytes = {
                let view = js_sys::Uint8Array::new_with_byte_offset_and_length(&blob_buffer, 0, 72);
                let mut buf = [0u8; 72];
                view.copy_to(&mut buf);
                buf
            };
            let header = match parse_sweep_header(&header_bytes) {
                Ok(h) => h,
                Err(_) => continue,
            };

            if header.data_word_size != 1 {
                has_u16 = true;
            }

            let total_values = header.azimuth_count as usize * header.gate_count as usize;
            sweep_blobs.push(SweepBlob {
                blob_buffer,
                header,
                total_values,
            });
        }

        // Second pass: pack data using native word size when all u8,
        // widening to u16 only when mixed.
        let word_size: u8 = if has_u16 { 2 } else { 1 };

        for sb in &sweep_blobs {
            let header = &sb.header;
            let total_values = sb.total_values;

            if word_size == 1 {
                // All sweeps are u8: copy raw bytes, no widening
                let u8_view = js_sys::Uint8Array::new_with_byte_offset_and_length(
                    &sb.blob_buffer,
                    header.gate_values_offset,
                    total_values as u32,
                );
                let prev_len = packed_data.len();
                packed_data.resize(prev_len + total_values, 0);
                u8_view.copy_to(&mut packed_data[prev_len..]);
            } else if header.data_word_size == 1 {
                // Mixed volume: widen this u8 sweep to u16
                let u8_view = js_sys::Uint8Array::new_with_byte_offset_and_length(
                    &sb.blob_buffer,
                    header.gate_values_offset,
                    total_values as u32,
                );
                let mut tmp = vec![0u8; total_values];
                u8_view.copy_to(&mut tmp);
                for &val in &tmp {
                    packed_data.extend_from_slice(&(val as u16).to_le_bytes());
                }
            } else {
                // Native u16: copy raw bytes directly
                let u8_view = js_sys::Uint8Array::new_with_byte_offset_and_length(
                    &sb.blob_buffer,
                    header.gate_values_offset,
                    (total_values * 2) as u32,
                );
                let prev_len = packed_data.len();
                packed_data.resize(prev_len + total_values * 2, 0);
                u8_view.copy_to(&mut packed_data[prev_len..]);
            }

            sweep_meta_vec.push(VolumeRenderSweepMeta {
                elevation_deg: header.mean_elevation as f64,
                azimuth_count: header.azimuth_count,
                gate_count: header.gate_count,
                first_gate_km: header.first_gate_range_km,
                gate_interval_km: header.gate_interval_km,
                max_range_km: header.max_range_km,
                data_offset,
                scale: header.scale as f64,
                offset: header.offset as f64,
            });

            data_offset += total_values as u32;
        }

        let total_ms = t_total.elapsed().as_secs_f64() * 1000.0;

        log::debug!(
            "render_volume: {} sweeps, {} values packed ({:.1}KB, u{}) in {:.1}ms",
            sweep_meta_vec.len(),
            data_offset,
            packed_data.len() as f64 / 1024.0,
            word_size * 8,
            total_ms,
        );

        // Serialize scalar/struct fields, then attach the packed buffer separately
        let response = VolumeRenderResponse {
            sweep_count: sweep_meta_vec.len() as u32,
            word_size,
            sweep_meta: sweep_meta_vec,
            product: product_str,
            total_ms,
        };
        let result = serde_wasm_bindgen::to_value(&response)
            .map_err(|e| JsValue::from_str(&format!("Failed to serialize response: {}", e)))?;

        // ArrayBuffer must be set directly for zero-copy transfer
        let packed_u8 = js_sys::Uint8Array::from(&packed_data[..]);
        let packed_buffer = packed_u8.buffer();
        js_sys::Reflect::set(&result, &"buffer".into(), &packed_buffer).ok();

        Ok(result)
    })
}
