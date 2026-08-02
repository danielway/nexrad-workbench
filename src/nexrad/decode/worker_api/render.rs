//! WASM exports for render operations (single-elevation and volume).

use super::*;

/// Render a specific elevation from pre-computed sweep data in IndexedDB.
///
/// Called from the Web Worker via worker.js. Fetches a single pre-computed
/// sweep blob and returns the data for GPU upload — no decoding needed.
///
/// Parameters (JS object): `{ scanKey: string, elevationNumber: number, product: string }`
/// Returns (JS object): `{ azimuths: Float32Array, gateValues: Float32Array, azimuthCount, gateCount, ... }`
#[allow(unreachable_pub)] // wasm_bindgen export invoked from worker.js; must stay pub
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
            .map_err(|e| wasm_bindgen::JsValue::from_str(&format!("Invalid scanKey: {e}")))?;

        let store = idb_store().await?;

        // Fetch raw IDB ArrayBuffer (no Rust-side copy)
        let t_fetch = web_time::Instant::now();
        let sweep_key = SweepDataKey::new(scan_key, elevation_number, &product_str);
        let blob_buffer = store
            .get_sweep(&sweep_key)
            .await
            .map_err(|e| wasm_bindgen::JsValue::from_str(&format!("Failed to fetch sweep: {}", e)))?
            .ok_or_else(|| {
                worker_error(
                    "not_found",
                    format!(
                        "No pre-computed sweep for elev={} product={}",
                        elevation_number, product_str,
                    ),
                )
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

        // Compute angular spacing between adjacent sorted radials.
        // Sweep blobs always store sorted azimuths covering (near) a full rotation,
        // so 360 / count is accurate for the shader's search threshold.
        let azimuth_spacing_deg = if header.azimuth_count > 0 {
            360.0f32 / header.azimuth_count as f32
        } else {
            1.0
        };

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
            azimuth_spacing_deg,
        };
        let result = serde_wasm_bindgen::to_value(&response)
            .map_err(|e| JsValue::from_str(&format!("Failed to serialize response: {}", e)))?;
        // ArrayBuffer fields must be set directly (not serializable via serde)
        attach_buffer_field(&result, "azimuths", &az_buf);
        attach_buffer_field(&result, "gateValues", &val_buf);
        if let Some(rt) = rt_buf {
            attach_buffer_field(&result, "radialTimes", &rt);
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
#[allow(unreachable_pub)] // wasm_bindgen export invoked from worker.js; must stay pub
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
            .map_err(|e| wasm_bindgen::JsValue::from_str(&format!("Invalid scanKey: {e}")))?;

        let store = idb_store().await?;

        // Collect all sweep data into a packed buffer.
        // We keep native word size when all sweeps are u8 to halve transfer cost.
        // Only widen to u16 when at least one sweep has u16 data.
        let mut packed_data: Vec<u8> = Vec::new();
        let mut sweep_meta_vec: Vec<VolumeRenderSweepMeta> = Vec::new();
        let mut data_offset: u32 = 0; // offset in values (not bytes)

        // First pass: read all sweep blobs and headers, determine word size
        struct SweepBlob {
            blob_buffer: js_sys::ArrayBuffer,
            header: SweepHeader,
        }
        let mut sweep_blobs: Vec<SweepBlob> = Vec::new();

        for &elev_num in &elevation_numbers {
            let sweep_key = SweepDataKey::new(scan_key.clone(), elev_num, &product_str);
            let blob_buffer = match store.get_sweep(&sweep_key).await {
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

            sweep_blobs.push(SweepBlob {
                blob_buffer,
                header,
            });
        }

        // Order the sweeps by true elevation angle and drop duplicate cuts.
        // The blobs arrive in elevation-*number* order, which SAILS/MRLE
        // rescans and split cuts leave non-monotonic in angle — an ordering
        // the ray marcher's bracket search cannot survive.
        let candidates: Vec<SweepCandidate> = sweep_blobs
            .iter()
            .map(|sb| SweepCandidate {
                elevation_deg: sb.header.mean_elevation,
                coverage_km: (sb.header.first_gate_range_km
                    + sb.header.gate_count as f64 * sb.header.gate_interval_km)
                    as f32,
                sweep_start_secs: sb.header.sweep_start_secs,
            })
            .collect();
        let plan = plan_volume_sweeps(&candidates, MAX_VOLUME_SWEEPS);
        if plan.len() < sweep_blobs.len() {
            log::debug!(
                "render_volume: {} of {} sweeps kept after elevation dedup/cap",
                plan.len(),
                sweep_blobs.len(),
            );
        }

        // Second pass: pack data using native word size when all u8,
        // widening to u16 only when mixed. Judged over the *kept* sweeps, so a
        // discarded duplicate can't force the whole volume wide.
        let has_u16 = plan
            .iter()
            .any(|&i| sweep_blobs[i].header.data_word_size != 1);
        let word_size: u8 = if has_u16 { 2 } else { 1 };

        for &plan_idx in &plan {
            let sb = &sweep_blobs[plan_idx];
            let header = &sb.header;
            let src_az_count = header.azimuth_count as usize;
            let gate_count = header.gate_count as usize;
            if src_az_count == 0 || gate_count == 0 {
                continue;
            }

            // Radial azimuths are irregular and start at an arbitrary angle, so
            // resample onto a uniform grid the shader can index arithmetically
            // instead of searching. Without this the shader's uniform-bin
            // assumption puts a seam wherever the sorted array wraps.
            let azimuths: Vec<f32> = {
                let view = js_sys::Float32Array::new_with_byte_offset_and_length(
                    &sb.blob_buffer,
                    header.azimuths_offset,
                    header.azimuth_count,
                );
                let mut v = vec![0f32; src_az_count];
                view.copy_to(&mut v);
                v
            };
            let bin_count = match choose_bin_count(median_azimuth_spacing_deg(&azimuths)) {
                0 => continue, // fewer than two radials — nothing to resample onto
                n => n as usize,
            };
            let bins = plan_azimuth_bins(&azimuths, bin_count as u32);

            // One contiguous read of the source rows, then gather.
            let src_word = header.data_word_size as usize;
            let src_row_bytes = gate_count * src_word;
            let src_bytes: Vec<u8> = {
                let len = src_az_count * src_row_bytes;
                let view = js_sys::Uint8Array::new_with_byte_offset_and_length(
                    &sb.blob_buffer,
                    header.gate_values_offset,
                    len as u32,
                );
                let mut v = vec![0u8; len];
                view.copy_to(&mut v);
                v
            };

            let out_row_bytes = gate_count * word_size as usize;
            let base = packed_data.len();
            // Bins with no radial within the gap threshold stay zero, which is
            // the below-threshold sentinel the shader already rejects.
            packed_data.resize(base + bin_count * out_row_bytes, 0);

            for (bin_i, slot) in bins.iter().enumerate() {
                let Some(src_i) = *slot else { continue };
                let src_start = src_i as usize * src_row_bytes;
                let dst_start = base + bin_i * out_row_bytes;
                if src_word == word_size as usize {
                    packed_data[dst_start..dst_start + out_row_bytes]
                        .copy_from_slice(&src_bytes[src_start..src_start + src_row_bytes]);
                } else {
                    // Mixed-width volume: widen this u8 sweep to u16.
                    for g in 0..gate_count {
                        let v = src_bytes[src_start + g] as u16;
                        packed_data[dst_start + g * 2..dst_start + g * 2 + 2]
                            .copy_from_slice(&v.to_le_bytes());
                    }
                }
            }

            sweep_meta_vec.push(VolumeRenderSweepMeta {
                elevation_deg: header.mean_elevation as f64,
                // Now a uniform bin count, not the raw radial count.
                azimuth_count: bin_count as u32,
                gate_count: header.gate_count,
                first_gate_km: header.first_gate_range_km,
                gate_interval_km: header.gate_interval_km,
                max_range_km: header.max_range_km,
                data_offset,
                scale: header.scale as f64,
                offset: header.offset as f64,
            });

            data_offset += (bin_count * gate_count) as u32;
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
        attach_buffer_field(&result, "buffer", &packed_buffer);

        Ok(result)
    })
}
