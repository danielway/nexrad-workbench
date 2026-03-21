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
    pub all_radials: Vec<::nexrad::model::data::Radial>,
    pub radial_metas: Vec<(i64, u8, f32, f32)>,
    pub completed_elevations: std::collections::HashSet<u8>,
    pub last_elevation_number: Option<u8>,
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
        use crate::nexrad::extract_elevation_numbers;

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

        let data_len = data.len();

        // --- Decode the chunk's record(s) into radials ---
        let (chunk_radials, chunk_vcp, chunk_has_vcp, mut volume_header_time_secs);

        if is_start {
            let result = crate::nexrad::ingest_phases::decode_start_chunk(data, false);
            chunk_radials = result.chunk_radials;
            chunk_vcp = result.chunk_vcp;
            chunk_has_vcp = result.chunk_has_vcp;
            volume_header_time_secs = result.volume_header_time_secs;

            // --- Delete any overlapping scans so we don't double-store ---
            let scan_key = ScanKey::new(site_id.as_str(), UnixMillis::from_secs(timestamp_secs));
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

            // --- Reset accumulator ---
            CHUNK_ACCUM.with(|cell| {
                *cell.borrow_mut() = Some(ChunkAccumulator {
                    scan_key,
                    site_id: site_id.clone(),
                    all_radials: Vec::new(),
                    radial_metas: Vec::new(),
                    completed_elevations: std::collections::HashSet::new(),
                    last_elevation_number: None,
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
        let chunk_elev_numbers = extract_elevation_numbers(&chunk_radials);
        let mut newly_completed: Vec<u8> = Vec::new();

        let time_spans = crate::nexrad::ingest_phases::compute_chunk_time_spans(&chunk_radials);
        let chunk_min_ts_secs = time_spans.chunk_min_ts_secs;
        let chunk_max_ts_secs = time_spans.chunk_max_ts_secs;
        let chunk_elev_spans = time_spans.chunk_elev_spans;
        let chunk_elev_az_ranges = time_spans.chunk_elev_az_ranges;
        let first_radial_azimuth = time_spans.first_radial_azimuth;
        let last_radial_azimuth = time_spans.last_radial_azimuth;
        let last_radial_time_secs = time_spans.last_radial_time_secs;

        // Detailed chunk diagnostics for real-time streaming debugging
        {
            let radial_count = chunk_radials.len();
            let elev_summary: Vec<String> = chunk_elev_spans
                .iter()
                .map(|(elev, _start, _end, count)| format!("e{}:{}r", elev, count))
                .collect();
            let total_accum_radials = CHUNK_ACCUM.with(|cell| {
                cell.borrow()
                    .as_ref()
                    .map(|a| a.all_radials.len())
                    .unwrap_or(0)
            });
            log::info!(
                "Chunk#{} elev=[{}] radials={} az_range=[{:.1}..{:.1}] accum_total={} is_start={} is_end={} size={}B",
                chunk_index,
                elev_summary.join(", "),
                radial_count,
                first_radial_azimuth.unwrap_or(0.0),
                last_radial_azimuth.unwrap_or(0.0),
                total_accum_radials,
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

            // Update VCP if newly extracted or if the chunk has a fuller VCP
            // (i.e. one with elevation details upgrading a number-only VCP).
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

            // Check for elevation transition → previous elevation is complete
            if let Some(first_elev) = chunk_elev_numbers.first() {
                if let Some(last) = accum.last_elevation_number {
                    if *first_elev != last && !accum.completed_elevations.contains(&last) {
                        newly_completed.push(last);
                        accum.completed_elevations.insert(last);
                    }
                }
            }

            // Append radials and metadata
            for r in &chunk_radials {
                accum.radial_metas.push((
                    r.collection_timestamp(),
                    r.elevation_number(),
                    r.elevation_angle_degrees(),
                    r.azimuth_angle_degrees(),
                ));
            }
            accum.all_radials.extend(chunk_radials);

            // Update last elevation number
            if let Some(&last) = chunk_elev_numbers.last() {
                accum.last_elevation_number = Some(last);
            }

            Ok::<(), wasm_bindgen::JsValue>(())
        })?;

        // On end, finalize all remaining elevations
        if is_end {
            CHUNK_ACCUM.with(|cell| {
                let mut borrow = cell.borrow_mut();
                if let Some(accum) = borrow.as_mut() {
                    // All unique elevation numbers that haven't been completed yet
                    let all_elevs: std::collections::HashSet<u8> = accum
                        .all_radials
                        .iter()
                        .map(|r| r.elevation_number())
                        .collect();
                    for elev in all_elevs {
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

            // Build sweep blobs for completed elevations
            let (sweep_blobs, sweep_metas) = CHUNK_ACCUM.with(|cell| {
                let borrow = cell.borrow();
                let accum = borrow.as_ref().unwrap();
                crate::nexrad::ingest_phases::build_flush_sweep_blobs(
                    &accum.all_radials,
                    &accum.radial_metas,
                    &newly_completed,
                    &accum.scan_key,
                )
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

        // Build sweep metadata for all completed elevations so far
        let all_sweeps = CHUNK_ACCUM.with(|cell| {
            let borrow = cell.borrow();
            let accum = borrow.as_ref().unwrap();
            let all_metas = crate::nexrad::ingest_phases::build_sweep_meta(&accum.radial_metas);
            // Only include completed elevations
            all_metas
                .into_iter()
                .filter(|m| accum.completed_elevations.contains(&m.elevation_number))
                .collect::<Vec<SweepMeta>>()
        });

        let vcp = CHUNK_ACCUM.with(|cell| cell.borrow().as_ref().and_then(|a| a.vcp.clone()));

        let total_ms = t_total.elapsed().as_secs_f64() * 1000.0;

        let accum_info = CHUNK_ACCUM.with(|c| {
            c.borrow()
                .as_ref()
                .map(|a| {
                    (
                        a.all_radials.len(),
                        a.has_vcp,
                        a.vcp.as_ref().map(|v| v.number),
                    )
                })
                .unwrap_or((0, false, None))
        });
        // Build per-elevation summary of the full accumulator state
        let chunk_detail = {
            use std::collections::BTreeMap;
            CHUNK_ACCUM.with(|cell| {
                let borrow = cell.borrow();
                let Some(accum) = borrow.as_ref() else {
                    return String::from("no accum");
                };

                // Per-elevation stats from this chunk's contribution
                // We know chunk added radials at indices [prev_count..current_count]
                // But we don't have prev_count here. Instead, summarize the
                // per-elevation state of the full accumulator.
                let mut by_elev: BTreeMap<u8, Vec<f32>> = BTreeMap::new();
                for r in &accum.all_radials {
                    by_elev
                        .entry(r.elevation_number())
                        .or_default()
                        .push(r.azimuth_angle_degrees());
                }

                let mut parts: Vec<String> = Vec::new();
                for (elev, mut azimuths) in by_elev {
                    azimuths.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                    let count = azimuths.len();
                    let min_az = azimuths.first().copied().unwrap_or(0.0);
                    let max_az = azimuths.last().copied().unwrap_or(0.0);
                    // Compute total angular span (handles wrap-around)
                    let span = if count <= 1 {
                        0.0
                    } else {
                        // Use sorted gaps to detect wrap
                        let mut max_gap = 0.0f32;
                        for i in 1..azimuths.len() {
                            let gap = azimuths[i] - azimuths[i - 1];
                            if gap > max_gap {
                                max_gap = gap;
                            }
                        }
                        let wrap_gap = (360.0 - max_az + min_az).max(0.0);
                        if wrap_gap > max_gap {
                            // No wrap: span is simply max - min
                            max_az - min_az
                        } else {
                            // Wraps through 0: span is 360 - largest_gap
                            360.0 - max_gap
                        }
                    };
                    let completed = if accum.completed_elevations.contains(&elev) {
                        " done"
                    } else {
                        ""
                    };
                    parts.push(format!(
                        "e{}:{}az {:.0}-{:.0}({:.0}°){}",
                        elev, count, min_az, max_az, span, completed
                    ));
                }

                // Product summary: check which products are present in the accumulator
                let mut products_present: Vec<&str> = Vec::new();
                let sample = accum.all_radials.first();
                if let Some(r) = sample {
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
                    "[{}] products=[{}]",
                    parts.join(" | "),
                    products_present.join(",")
                )
            })
        };

        log::info!(
            "ingest_chunk: chunk={} is_start={} is_end={} radials={} vcp={:?} has_vcp={} completed_elevs={:?} sweeps_stored={} {:.1}ms {}",
            chunk_index, is_start, is_end,
            accum_info.0, accum_info.2, accum_info.1,
            newly_completed, sweeps_stored, total_ms,
            chunk_detail,
        );

        // Current in-progress elevation info
        let current_elevation =
            CHUNK_ACCUM.with(|c| c.borrow().as_ref().and_then(|a| a.last_elevation_number));
        let current_elevation_radials = CHUNK_ACCUM.with(|c| {
            c.borrow().as_ref().and_then(|a| {
                a.last_elevation_number
                    .map(|elev| a.radial_metas.iter().filter(|m| m.1 == elev).count() as u32)
            })
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
            radials_decoded: chunk_elev_numbers.len() as u32,
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
