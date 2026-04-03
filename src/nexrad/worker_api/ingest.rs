//! WASM exports for ingest operations (full archive and per-chunk streaming).

use super::*;

/// Ingest a raw NEXRAD archive file: split into LDM records, probe for elevation
/// metadata, store in IndexedDB, and return metadata.
///
/// Called from the Web Worker via worker.js.
///
/// Parameters (JS object): `{ data: ArrayBuffer, siteId: string, timestampSecs: number, fileName: string }`
/// Returns (JS object): `{ recordsStored, scanKey, elevationMap, totalMs, sweepsJson, vcpJson? }`
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn worker_ingest(params: wasm_bindgen::JsValue) -> js_sys::Promise {
    init_logger();
    wasm_bindgen_futures::future_to_promise(async move {
        let t_total = web_time::Instant::now();

        // --- Extract parameters from JS ---
        let data = extract_data_bytes(&params)?;
        let p: IngestParams = serde_wasm_bindgen::from_value(params)
            .map_err(|e| JsValue::from_str(&format!("Invalid ingest params: {}", e)))?;
        let site_id = p.site_id;
        let timestamp_secs = p.timestamp_secs as i64;
        let file_name = p.file_name;

        log::info!(
            "ingest: received {} ({:.1}MB)",
            file_name,
            data.len() as f64 / (1024.0 * 1024.0),
        );

        // --- Phase 0: Split into LDM records ---
        let t_split = web_time::Instant::now();
        let file = nexrad_data::volume::File::new(data);
        let records = file.records().map_err(|e| {
            wasm_bindgen::JsValue::from_str(&format!("Failed to split archive: {}", e))
        })?;
        let split_ms = t_split.elapsed().as_secs_f64() * 1000.0;

        if records.is_empty() {
            return Err(wasm_bindgen::JsValue::from_str("No records found"));
        }

        log::info!(
            "ingest: split into {} records in {:.1}ms",
            records.len(),
            split_ms,
        );

        let store = idb_store().await?;
        let scan_key = ScanKey::new(site_id.as_str(), UnixMillis::from_secs(timestamp_secs));

        // --- Phase 1: Decompress + decode all records into radials ---
        let t_decode = web_time::Instant::now();
        let decoded = crate::nexrad::ingest_phases::decompress_and_decode_records(&records)?;
        let all_radials = decoded.all_radials;
        let radial_metas = decoded.radial_metas;
        let decompress_ms_total = decoded.decompress_ms;
        let decode_only_ms = decoded.decode_ms;
        let compressed_count = decoded.compressed_count;
        let extracted_vcp = decoded.extracted_vcp;
        let has_vcp = decoded.has_vcp;
        let phase1_ms = t_decode.elapsed().as_secs_f64() * 1000.0;

        let sweeps = crate::nexrad::ingest_phases::build_sweep_meta(&radial_metas);
        let elevation_numbers: Vec<u8> = sweeps.iter().map(|s| s.elevation_number).collect();
        let end_timestamp_secs = sweeps
            .iter()
            .map(|s| s.end as i64)
            .max()
            .unwrap_or(timestamp_secs);

        log::info!(
            "ingest: decompressed {} records, decoded {} radials across {} elevations in {:.1}ms (decompress: {:.1}ms, decode: {:.1}ms)",
            compressed_count,
            all_radials.len(),
            elevation_numbers.len(),
            phase1_ms,
            decompress_ms_total,
            decode_only_ms,
        );

        // --- Phase 2: Extract sweep data for all (elevation, product) pairs ---
        let t_extract = web_time::Instant::now();
        let by_elevation = crate::nexrad::ingest_phases::group_radials_by_elevation(&all_radials);
        let sweep_blobs = crate::nexrad::ingest_phases::extract_sweep_blobs(
            &by_elevation,
            &elevation_numbers,
            &scan_key,
        );
        let extract_ms = t_extract.elapsed().as_secs_f64() * 1000.0;

        let sweep_count = sweep_blobs.len() as u32;
        let total_sweep_bytes: u64 = sweep_blobs
            .iter()
            .map(|(_, b): &(String, Vec<u8>)| b.len() as u64)
            .sum();

        log::info!(
            "ingest: extracted {} sweeps ({:.1}MB) in {:.1}ms",
            sweep_count,
            total_sweep_bytes as f64 / (1024.0 * 1024.0),
            extract_ms,
        );

        // --- Phase 2.5: Delete any overlapping scans from IDB ---
        let archive_end_ms = end_timestamp_secs * 1000;
        let deleted = store
            .delete_overlapping_scans(
                &SiteId(site_id.clone()),
                scan_key.scan_start,
                archive_end_ms,
                &scan_key,
            )
            .await
            .map_err(|e| {
                wasm_bindgen::JsValue::from_str(&format!(
                    "Failed to delete overlapping scans: {}",
                    e
                ))
            })?;
        if deleted > 0 {
            log::info!("ingest: replaced {} overlapping scan(s)", deleted);
        }

        // --- Phase 3: Store sweep blobs in IDB ---
        let t_store = web_time::Instant::now();
        store.put_sweeps_batch(&sweep_blobs).await.map_err(|e| {
            wasm_bindgen::JsValue::from_str(&format!("Failed to store sweeps batch: {}", e))
        })?;
        let store_ms = t_store.elapsed().as_secs_f64() * 1000.0;

        // --- Phase 4: Store scan index entry ---
        let t_index = web_time::Instant::now();
        let mut scan_entry = ScanIndexEntry::new(scan_key.clone());
        scan_entry.has_vcp = has_vcp;
        scan_entry.vcp = extracted_vcp.clone();
        scan_entry.present_records = records.len() as u32;
        scan_entry.file_name = Some(file_name.clone());
        scan_entry.total_size_bytes = total_sweep_bytes;
        scan_entry.end_timestamp_secs = Some(end_timestamp_secs);
        scan_entry.sweeps = Some(sweeps.clone());
        scan_entry.has_precomputed_sweeps = true;

        store.put_scan_index_entry(&scan_entry).await.map_err(|e| {
            wasm_bindgen::JsValue::from_str(&format!("Failed to store scan index: {}", e))
        })?;
        let index_ms = t_index.elapsed().as_secs_f64() * 1000.0;

        let total_ms = t_total.elapsed().as_secs_f64() * 1000.0;

        log::info!(
            "ingest: complete {} in {:.0}ms | split {:.1} | decompress {:.1} | decode {:.1} | extract {:.1} | store {:.1} | index {:.1} | {} records, {} radials, {} elevations, {} sweeps, {:.1}MB",
            file_name, total_ms, split_ms, decompress_ms_total, decode_only_ms,
            extract_ms, store_ms, index_ms,
            records.len(), all_radials.len(), elevation_numbers.len(),
            sweep_count, total_sweep_bytes as f64 / (1024.0 * 1024.0),
        );

        // --- Build JS response ---
        let response = IngestResponse {
            records_stored: sweep_count,
            scan_key: scan_key.to_storage_key(),
            elevation_numbers: &elevation_numbers,
            total_ms,
            split_ms,
            decompress_ms: decompress_ms_total,
            decode_ms: decode_only_ms,
            extract_ms,
            store_ms,
            index_ms,
            sweeps: &sweeps,
            vcp: extracted_vcp.as_ref(),
        };
        serde_wasm_bindgen::to_value(&response)
            .map_err(|e| JsValue::from_str(&format!("Failed to serialize response: {}", e)))
    })
}

// ---------------------------------------------------------------------------
// Per-chunk incremental ingest
// ---------------------------------------------------------------------------

/// Accumulator for per-chunk ingest. Holds decoded radials across chunks
/// until an elevation is complete, then flushes sweep blobs to IDB.
#[allow(dead_code)]
pub(super) struct ChunkAccumulator {
    pub scan_key: ScanKey,
    pub site_id: String,
    /// Radials for the current (in-progress) elevation only.
    /// Previous elevations are flushed to IDB on transition.
    pub current_radials: Vec<::nexrad::model::data::Radial>,
    /// Parallel metadata for current elevation radials.
    pub current_radial_metas: Vec<(i64, u8, f32, f32)>,
    /// Current elevation number being accumulated.
    pub current_elevation: Option<u8>,
    /// Elevation numbers that have been flushed to IDB.
    pub completed_elevations: std::collections::HashSet<u8>,
    /// Sweep metadata accumulated from flushed elevations (for response).
    pub completed_sweep_metas: Vec<SweepMeta>,
    pub vcp: Option<ExtractedVcp>,
    pub has_vcp: bool,
    pub total_chunks: u32,
    pub total_size_bytes: u64,
    pub file_name: String,
    pub timestamp_secs: i64,
}

thread_local! {
    pub(super) static CHUNK_ACCUM: std::cell::RefCell<Option<ChunkAccumulator>> =
        const { std::cell::RefCell::new(None) };
}

/// Ingest a single real-time chunk: decompress, decode, and store completed
/// elevations to IDB incrementally.
///
/// Called from the Web Worker via worker.js.
///
/// Parameters (JS object):
/// `{ data: ArrayBuffer, siteId: string, timestampSecs: number,
///    chunkIndex: number, isStart: bool, isEnd: bool, fileName: string }`
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn worker_ingest_chunk(params: wasm_bindgen::JsValue) -> js_sys::Promise {
    init_logger();
    wasm_bindgen_futures::future_to_promise(async move {
        let t_total = web_time::Instant::now();

        // --- Extract parameters from JS ---
        let data = extract_data_bytes(&params)?;
        let p: IngestChunkParams = serde_wasm_bindgen::from_value(params)
            .map_err(|e| JsValue::from_str(&format!("Invalid ingest_chunk params: {}", e)))?;
        let site_id = p.site_id;
        let timestamp_secs = p.timestamp_secs as i64;
        let chunk_index = p.chunk_index;
        let is_start = p.is_start;
        let is_end = p.is_end;
        let file_name = p.file_name;
        let skip_overlap_delete = p.skip_overlap_delete;

        let data_len = data.len();

        // --- Decode the chunk's record(s) into radials ---
        let (chunk_radials, chunk_vcp, chunk_has_vcp, mut volume_header_time_secs);

        if is_start {
            let result = crate::nexrad::ingest_phases::decode_start_chunk(data, false);
            chunk_radials = result.chunk_radials;
            chunk_vcp = result.chunk_vcp;
            chunk_has_vcp = result.chunk_has_vcp;
            volume_header_time_secs = result.volume_header_time_secs;

            let scan_key = ScanKey::new(site_id.as_str(), UnixMillis::from_secs(timestamp_secs));

            // Pre-populate completed_elevations from IDB when resuming a
            // volume that already has cached sweep data, so the accumulator
            // won't overwrite existing complete sweeps with partial data.
            let mut pre_completed = std::collections::HashSet::new();

            if skip_overlap_delete {
                log::info!(
                    "ingest_chunk: skipping overlap delete (resuming volume with cached data)"
                );
                let store = idb_store().await?;
                if let Ok(Some(entry)) = store.scan_availability(&scan_key).await {
                    if let Some(ref sweeps) = entry.sweeps {
                        for s in sweeps {
                            pre_completed.insert(s.elevation_number);
                        }
                    }
                }
                if !pre_completed.is_empty() {
                    log::info!(
                        "ingest_chunk: pre-populated {} completed elevations from IDB",
                        pre_completed.len()
                    );
                }
            } else {
                // --- Delete any overlapping scans so we don't double-store ---
                let overlap_start_secs = volume_header_time_secs
                    .map(|t| t as i64)
                    .unwrap_or(timestamp_secs);
                let overlap_start_ms = overlap_start_secs * 1000;
                let overlap_end_ms = (overlap_start_secs + 600) * 1000;
                let store = idb_store().await?;
                let deleted = store
                    .delete_overlapping_scans(
                        &SiteId(site_id.clone()),
                        UnixMillis(overlap_start_ms),
                        overlap_end_ms,
                        &scan_key,
                    )
                    .await
                    .map_err(|e| {
                        wasm_bindgen::JsValue::from_str(&format!(
                            "Failed to delete overlapping scans: {}",
                            e
                        ))
                    })?;
                if deleted > 0 {
                    log::info!(
                        "ingest_chunk: replaced {} overlapping scan(s) before real-time ingest",
                        deleted
                    );
                }
            }

            // --- Reset accumulator ---
            CHUNK_ACCUM.with(|cell| {
                *cell.borrow_mut() = Some(ChunkAccumulator {
                    scan_key,
                    site_id: site_id.clone(),
                    current_radials: Vec::new(),
                    current_radial_metas: Vec::new(),
                    current_elevation: None,
                    completed_elevations: pre_completed,
                    completed_sweep_metas: Vec::new(),
                    vcp: None,
                    has_vcp: false,
                    total_chunks: 0,
                    total_size_bytes: 0,
                    file_name: file_name.clone(),
                    timestamp_secs,
                });
            });
        } else {
            let accum_has_full_vcp = CHUNK_ACCUM.with(|cell| {
                cell.borrow()
                    .as_ref()
                    .and_then(|a| a.vcp.as_ref())
                    .map(|v| !v.elevations.is_empty())
                    .unwrap_or(false)
            });

            let result = crate::nexrad::ingest_phases::decode_subsequent_chunk(
                &data,
                accum_has_full_vcp,
                chunk_index,
            );
            chunk_radials = result.chunk_radials;
            chunk_vcp = result.chunk_vcp;
            chunk_has_vcp = result.chunk_has_vcp;
            volume_header_time_secs = result.volume_header_time_secs;
        }

        if volume_header_time_secs.is_none() {
            volume_header_time_secs =
                crate::nexrad::record_decode::extract_volume_start_time(&chunk_radials);
        }

        // --- Update accumulator with this chunk's radials ---
        // Chunks contain data for exactly one elevation.
        let chunk_elevation = chunk_radials.first().map(|r| r.elevation_number());
        let mut newly_completed: Vec<u8> = Vec::new();

        let time_spans = crate::nexrad::ingest_phases::compute_chunk_time_spans(&chunk_radials);
        let chunk_min_ts_secs = time_spans.chunk_min_ts_secs;
        let chunk_max_ts_secs = time_spans.chunk_max_ts_secs;
        let chunk_elev_spans = time_spans.chunk_elev_spans;
        let chunk_elev_az_ranges = time_spans.chunk_elev_az_ranges;
        let first_radial_azimuth = time_spans.first_radial_azimuth;
        let last_radial_azimuth = time_spans.last_radial_azimuth;
        let last_radial_time_secs = time_spans.last_radial_time_secs;

        // Detailed chunk diagnostics
        {
            let radial_count = chunk_radials.len();
            let accum_radials = CHUNK_ACCUM.with(|cell| {
                cell.borrow()
                    .as_ref()
                    .map(|a| a.current_radials.len())
                    .unwrap_or(0)
            });
            log::info!(
                "Chunk#{} elev={:?} radials={} az_range=[{:.1}..{:.1}] accum_current={} is_start={} is_end={} size={}B",
                chunk_index,
                chunk_elevation,
                radial_count,
                first_radial_azimuth.unwrap_or(0.0),
                last_radial_azimuth.unwrap_or(0.0),
                accum_radials,
                is_start,
                is_end,
                data_len,
            );
        }

        CHUNK_ACCUM.with(|cell| {
            let mut borrow = cell.borrow_mut();
            let accum = borrow.as_mut().ok_or_else(|| {
                wasm_bindgen::JsValue::from_str("No accumulator — missing Start chunk?")
            })?;

            accum.total_chunks += 1;
            accum.total_size_bytes += data_len as u64;

            // Update VCP if newly extracted or if the chunk has a fuller VCP.
            if chunk_has_vcp {
                accum.has_vcp = true;
            }
            if let Some(ref new_vcp) = chunk_vcp {
                let should_upgrade = match accum.vcp {
                    None => true,
                    Some(ref existing) => {
                        existing.elevations.is_empty() && !new_vcp.elevations.is_empty()
                    }
                };
                if should_upgrade {
                    accum.vcp = chunk_vcp.clone();
                }
            }

            // Flush-on-transition: when the chunk's elevation differs from
            // the current accumulator elevation, the previous elevation is
            // complete. Flush it immediately and discard its radials.
            if let Some(elev) = chunk_elevation {
                if let Some(prev) = accum.current_elevation {
                    if elev != prev && !accum.completed_elevations.contains(&prev) {
                        newly_completed.push(prev);
                        accum.completed_elevations.insert(prev);
                    }
                }
                accum.current_elevation = Some(elev);
            }

            // Append radials and metadata for the current elevation.
            for r in &chunk_radials {
                accum.current_radial_metas.push((
                    r.collection_timestamp(),
                    r.elevation_number(),
                    r.elevation_angle_degrees(),
                    r.azimuth_angle_degrees(),
                ));
            }
            accum.current_radials.extend(chunk_radials);

            Ok::<(), wasm_bindgen::JsValue>(())
        })?;

        // On end, finalize the current (last) elevation.
        if is_end {
            CHUNK_ACCUM.with(|cell| {
                let mut borrow = cell.borrow_mut();
                if let Some(accum) = borrow.as_mut() {
                    if let Some(elev) = accum.current_elevation {
                        if !accum.completed_elevations.contains(&elev) {
                            newly_completed.push(elev);
                            accum.completed_elevations.insert(elev);
                        }
                    }
                }
            });
        }

        // --- Flush completed elevations to IDB ---
        let mut sweeps_stored: u32 = 0;
        let new_sweep_metas: Vec<SweepMeta>;
        let new_size_bytes: u64;

        if !newly_completed.is_empty() {
            let store = idb_store().await?;

            // Build sweep blobs from the current elevation's radials.
            // With flush-on-transition, only the just-completed elevation's
            // radials are in memory — no filtering needed.
            let (sweep_blobs, sweep_metas) = CHUNK_ACCUM.with(|cell| {
                let mut borrow = cell.borrow_mut();
                let accum = borrow.as_mut().unwrap();
                let result = crate::nexrad::ingest_phases::build_flush_sweep_blobs(
                    &accum.current_radials,
                    &accum.current_radial_metas,
                    &newly_completed,
                    &accum.scan_key,
                );
                // Store sweep metas for the response, then clear radials.
                accum.completed_sweep_metas.extend(result.1.iter().cloned());
                accum.current_radials.clear();
                accum.current_radial_metas.clear();
                result
            });

            new_size_bytes = sweep_blobs.iter().map(|(_, b)| b.len() as u64).sum();
            sweeps_stored = sweep_blobs.len() as u32;
            new_sweep_metas = sweep_metas;

            // Store sweep blobs
            if !sweep_blobs.is_empty() {
                store.put_sweeps_batch(&sweep_blobs).await.map_err(|e| {
                    wasm_bindgen::JsValue::from_str(&format!("Failed to store sweeps: {}", e))
                })?;
            }

            // Merge scan index entry
            let partial_entry = CHUNK_ACCUM.with(|cell| {
                let borrow = cell.borrow();
                let accum = borrow.as_ref().unwrap();

                let end_ts = new_sweep_metas
                    .iter()
                    .map(|s| s.end as i64)
                    .max()
                    .unwrap_or(accum.timestamp_secs);

                let mut entry = ScanIndexEntry::new(accum.scan_key.clone());
                entry.has_vcp = accum.has_vcp;
                entry.vcp = accum.vcp.clone();
                entry.file_name = Some(accum.file_name.clone());
                entry.end_timestamp_secs = Some(end_ts);
                if let Some(ref vcp) = accum.vcp {
                    entry.expected_records = Some(vcp.elevations.len() as u32);
                }
                entry
            });

            store
                .merge_scan_index_entry(
                    &partial_entry,
                    newly_completed.len() as u32,
                    new_size_bytes,
                    &new_sweep_metas,
                )
                .await
                .map_err(|e| {
                    wasm_bindgen::JsValue::from_str(&format!("Failed to merge scan index: {}", e))
                })?;
        }

        // --- Build the scan key for response ---
        let scan_key_str = CHUNK_ACCUM.with(|cell| {
            cell.borrow()
                .as_ref()
                .map(|a| a.scan_key.to_storage_key())
                .unwrap_or_default()
        });

        // All completed sweep metadata, accumulated incrementally during flushes.
        let all_sweeps = CHUNK_ACCUM.with(|cell| {
            let borrow = cell.borrow();
            let accum = borrow.as_ref().unwrap();
            accum.completed_sweep_metas.clone()
        });

        let vcp = CHUNK_ACCUM.with(|cell| cell.borrow().as_ref().and_then(|a| a.vcp.clone()));

        let total_ms = t_total.elapsed().as_secs_f64() * 1000.0;

        let accum_info = CHUNK_ACCUM.with(|c| {
            c.borrow()
                .as_ref()
                .map(|a| {
                    (
                        a.current_radials.len(),
                        a.has_vcp,
                        a.vcp.as_ref().map(|v| v.number),
                    )
                })
                .unwrap_or((0, false, None))
        });
        // Summary: current elevation in memory + completed elevations count.
        let chunk_detail = CHUNK_ACCUM.with(|cell| {
            let borrow = cell.borrow();
            let Some(accum) = borrow.as_ref() else {
                return String::from("no accum");
            };

            let current_count = accum.current_radials.len();
            let current_elev = accum
                .current_elevation
                .map(|e| format!("e{}", e))
                .unwrap_or_else(|| "none".to_string());
            let completed: Vec<String> = accum
                .completed_elevations
                .iter()
                .map(|e| format!("e{}", e))
                .collect();

            // Product summary from current radials
            let mut products_present: Vec<&str> = Vec::new();
            if let Some(r) = accum.current_radials.first() {
                use nexrad_render::Product;
                for (p, name) in [
                    (Product::Reflectivity, "REF"),
                    (Product::Velocity, "VEL"),
                    (Product::SpectrumWidth, "SW"),
                    (Product::DifferentialReflectivity, "ZDR"),
                    (Product::CorrelationCoefficient, "CC"),
                    (Product::DifferentialPhase, "PHI"),
                ] {
                    if p.moment_data(r).is_some() || p.cfp_moment_data(r).is_some() {
                        products_present.push(name);
                    }
                }
            }

            format!(
                "current={}:{}r completed=[{}] products=[{}]",
                current_elev,
                current_count,
                completed.join(","),
                products_present.join(","),
            )
        });

        log::info!(
            "ingest_chunk: chunk={} is_start={} is_end={} radials={} vcp={:?} has_vcp={} completed_elevs={:?} sweeps_stored={} {:.1}ms {}",
            chunk_index, is_start, is_end,
            accum_info.0, accum_info.2, accum_info.1,
            newly_completed, sweeps_stored, total_ms,
            chunk_detail,
        );

        // Current in-progress elevation info
        let current_elevation =
            CHUNK_ACCUM.with(|c| c.borrow().as_ref().and_then(|a| a.current_elevation));
        let current_elevation_radials = CHUNK_ACCUM.with(|c| {
            c.borrow()
                .as_ref()
                .and_then(|a| a.current_elevation.map(|_| a.current_radials.len() as u32))
        });

        // --- Clear accumulator on end ---
        if is_end {
            CHUNK_ACCUM.with(|cell| {
                *cell.borrow_mut() = None;
            });
        }

        // --- Build JS response ---
        let response = ChunkIngestResponse {
            chunk_index,
            radials_decoded: chunk_elevation.is_some() as u32,
            sweeps_stored,
            scan_key: scan_key_str,
            is_end,
            total_ms,
            sweeps: all_sweeps,
            elevations_completed: newly_completed,
            vcp,
            chunk_min_time_secs: chunk_min_ts_secs,
            chunk_max_time_secs: chunk_max_ts_secs,
            chunk_elev_spans,
            chunk_elev_az_ranges,
            volume_header_time_secs,
            last_radial_azimuth,
            last_radial_time_secs,
            current_elevation,
            current_elevation_radials,
        };
        serde_wasm_bindgen::to_value(&response)
            .map_err(|e| JsValue::from_str(&format!("Failed to serialize response: {}", e)))
    })
}
