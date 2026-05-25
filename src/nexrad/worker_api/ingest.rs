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
        let timestamp_secs = p.timestamp_secs;
        let file_name = p.file_name;

        log::debug!(
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

        log::debug!(
            "ingest: split into {} records in {:.1}ms",
            records.len(),
            split_ms,
        );

        let store = idb_store().await?;
        let scan_key = ScanKey::new(site_id.as_str(), UnixMillis::from_secs_f64(timestamp_secs));

        // --- Phase 1: Decompress + decode all records into radials ---
        let t_decode = web_time::Instant::now();
        let decoded = crate::nexrad::ingest_phases::decompress_and_decode_records(&records)?;
        let all_radials = decoded.all_radials;
        let radial_metas = decoded.radial_metas;
        let decompress_ms_total = decoded.decompress_ms;
        let decode_only_ms = decoded.decode_ms;
        let compressed_count = decoded.compressed_count;
        let extracted_vcp = decoded.extracted_vcp;
        let phase1_ms = t_decode.elapsed().as_secs_f64() * 1000.0;

        log::debug!(
            "ingest: decompressed {} records, decoded {} radials in {:.1}ms (decompress: {:.1}ms, decode: {:.1}ms)",
            compressed_count,
            all_radials.len(),
            phase1_ms,
            decompress_ms_total,
            decode_only_ms,
        );

        // --- Phase 2: Group + extract sweeps into uploads ---
        let t_extract = web_time::Instant::now();
        let by_elevation = crate::nexrad::ingest_phases::group_radials_by_elevation(&all_radials);
        let elevations =
            crate::nexrad::ingest_phases::build_elevation_uploads(&by_elevation, &radial_metas);
        let extract_ms = t_extract.elapsed().as_secs_f64() * 1000.0;

        let sweep_count: u32 = elevations.iter().map(|e| e.blobs.len() as u32).sum();
        let total_sweep_bytes: u64 = elevations
            .iter()
            .flat_map(|e| e.blobs.iter())
            .map(|b| b.bytes.len() as u64)
            .sum();
        let elevation_numbers: Vec<u8> = elevations.iter().map(|e| e.elevation_number).collect();
        let sweeps: Vec<CachedSweep> = elevations.iter().map(|e| e.to_cached_sweep()).collect();

        log::debug!(
            "ingest: extracted {} sweeps across {} elevations ({:.1}MB) in {:.1}ms",
            sweep_count,
            elevation_numbers.len(),
            total_sweep_bytes as f64 / (1024.0 * 1024.0),
            extract_ms,
        );

        // --- Phase 3: Atomically store sweep blobs + scan-index entry ---
        let t_store = web_time::Instant::now();
        let header = ScanHeader {
            scan: scan_key.clone(),
            vcp: extracted_vcp.clone(),
            file_name: Some(file_name.clone()),
        };
        store.upsert_scan(&header, &elevations).await.map_err(|e| {
            wasm_bindgen::JsValue::from_str(&format!("Failed to store scan: {}", e))
        })?;
        let store_ms = t_store.elapsed().as_secs_f64() * 1000.0;
        let index_ms = 0.0;

        let total_ms = t_total.elapsed().as_secs_f64() * 1000.0;

        log::debug!(
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
    pub completed_sweep_metas: Vec<CachedSweep>,
    pub vcp: Option<ExtractedVcp>,
    pub has_vcp: bool,
    pub total_chunks: u32,
    pub total_size_bytes: u64,
    pub file_name: String,
    /// Volume scan start (Unix seconds, sub-second precision). Same value
    /// for every chunk in a volume; used to construct the IDB scan key.
    pub timestamp_secs: f64,
}

// Per-worker chunk accumulator.
//
// ## Safety invariant: no `.await` inside accumulator access
//
// `CHUNK_ACCUM` is a per-worker thread-local, mutated incrementally as
// chunks arrive. The streaming-loop and ingest paths share a single
// worker, so accumulator state is single-threaded; the only concurrency
// comes from `.await` points yielding back to the worker scheduler.
//
// If a borrow of the cell is held across an `.await`, two failure modes
// open:
//   1. A re-entrant future scheduled on the same worker tries to borrow
//      the cell and panics (`already borrowed`).
//   2. The borrowed-from accumulator state is observed mid-mutation by
//      another task before the original closure completes.
//
// Every access here goes through the [`with_chunk_accum`] /
// [`with_chunk_accum_mut`] / [`set_chunk_accum`] helpers below. They
// take a synchronous `FnOnce` (or no closure at all) and drop the
// borrow before returning, so the no-await-inside invariant is
// type-enforced: you literally cannot `.await` inside the closure.
thread_local! {
    static CHUNK_ACCUM: std::cell::RefCell<Option<ChunkAccumulator>> =
        const { std::cell::RefCell::new(None) };
}

/// Run `f` with shared (read-only) access to the chunk accumulator.
///
/// `f` receives `None` when no accumulator is active. The borrow guard
/// is dropped before this helper returns, so awaiting on the return value
/// is safe; awaiting *inside* `f` is impossible (it's a synchronous
/// `FnOnce`).
pub(super) fn with_chunk_accum<R>(f: impl FnOnce(Option<&ChunkAccumulator>) -> R) -> R {
    CHUNK_ACCUM.with(|cell| f(cell.borrow().as_ref()))
}

/// Like [`with_chunk_accum`] but gives `f` exclusive access.
pub(super) fn with_chunk_accum_mut<R>(f: impl FnOnce(Option<&mut ChunkAccumulator>) -> R) -> R {
    CHUNK_ACCUM.with(|cell| f(cell.borrow_mut().as_mut()))
}

/// Install or clear the accumulator. Used by the ingest path on Start
/// chunks (install fresh) and end-of-volume (clear). Distinct from
/// [`with_chunk_accum_mut`] because that helper hands out `&mut` to
/// the inner value — it can't replace the `Option` itself.
pub(super) fn set_chunk_accum(value: Option<ChunkAccumulator>) {
    CHUNK_ACCUM.with(|cell| *cell.borrow_mut() = value);
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
        let timestamp_secs = p.timestamp_secs;
        let chunk_index = p.chunk_index;
        let is_start = p.is_start;
        let is_end = p.is_end;
        let file_name = p.file_name;
        let is_last_in_sweep = p.is_last_in_sweep;

        let data_len = data.len();

        // --- Decode the chunk's record(s) into radials ---
        let (chunk_radials, chunk_vcp, chunk_has_vcp, mut volume_header_time_secs);

        if is_start {
            let result = crate::nexrad::ingest_phases::decode_start_chunk(data, false);
            chunk_radials = result.chunk_radials;
            chunk_vcp = result.chunk_vcp;
            chunk_has_vcp = result.chunk_has_vcp;
            volume_header_time_secs = result.volume_header_time_secs;

            let scan_key =
                ScanKey::new(site_id.as_str(), UnixMillis::from_secs_f64(timestamp_secs));

            // Pre-populate completed_elevations from any pre-existing IDB
            // entry for this scan, so a resume doesn't reprocess already-
            // cached sweeps. New scans return None here and start empty.
            let mut pre_completed = std::collections::HashSet::new();
            let store = idb_store().await?;
            if let Ok(Some(entry)) = store.scan_availability(&scan_key).await {
                for s in &entry.cached_sweeps {
                    pre_completed.insert(s.elevation_number);
                }
            }
            if !pre_completed.is_empty() {
                log::debug!(
                    "ingest_chunk: pre-populated {} completed elevations from IDB",
                    pre_completed.len()
                );
            }

            // --- Reset accumulator ---
            set_chunk_accum(Some(ChunkAccumulator {
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
            }));
        } else {
            let accum_has_full_vcp = with_chunk_accum(|accum| {
                accum
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
            let accum_radials =
                with_chunk_accum(|accum| accum.map(|a| a.current_radials.len()).unwrap_or(0));
            log::debug!(
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

        with_chunk_accum_mut(|accum| {
            let accum = accum.ok_or_else(|| {
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

            // Flush-on-last-chunk: when the projection says this is the last
            // chunk in the sweep, complete the elevation immediately rather
            // than waiting for the next elevation's first chunk.
            if is_last_in_sweep {
                if let Some(elev) = accum.current_elevation {
                    if !accum.completed_elevations.contains(&elev) {
                        log::debug!(
                            "Chunk#{}: last in sweep for elev {} — flushing ({} radials)",
                            chunk_index,
                            elev,
                            accum.current_radials.len(),
                        );
                        newly_completed.push(elev);
                        accum.completed_elevations.insert(elev);
                    }
                }
            }

            Ok::<(), wasm_bindgen::JsValue>(())
        })?;

        // On end, finalize the current (last) elevation.
        if is_end {
            with_chunk_accum_mut(|accum| {
                if let Some(accum) = accum {
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

        if !newly_completed.is_empty() {
            let store = idb_store().await?;

            // Build elevation uploads from the current elevation's radials.
            // With flush-on-transition, only the just-completed elevation's
            // radials are in memory — no filtering needed.
            let elevations = with_chunk_accum_mut(|accum| {
                let accum = accum.unwrap();
                let result = crate::nexrad::ingest_phases::build_elevation_uploads_for_flush(
                    &accum.current_radials,
                    &accum.current_radial_metas,
                    &newly_completed,
                );
                // Mirror the derived manifest into the accumulator's response
                // log, then clean up radials. The IDB layer derives the same
                // CachedSweep set on its side from these uploads.
                accum
                    .completed_sweep_metas
                    .extend(result.iter().map(|e| e.to_cached_sweep()));

                if is_last_in_sweep {
                    // Flushed the current elevation on its last chunk.
                    // Keep radials in the accumulator so render_live can still
                    // read the complete sweep data for the final GPU upload.
                    // They'll be cleared when the next elevation's first chunk
                    // arrives (via the transition logic).
                } else {
                    // Transition flush: discard the completed elevation's radials,
                    // retain the new elevation's radials from the transition chunk.
                    accum
                        .current_radials
                        .retain(|r| !newly_completed.contains(&r.elevation_number()));
                    accum
                        .current_radial_metas
                        .retain(|&(_, elev, _, _)| !newly_completed.contains(&elev));
                }
                result
            });

            sweeps_stored = elevations.iter().map(|e| e.blobs.len() as u32).sum();

            // Snapshot the accumulator state for the upsert header.
            let (scan_key, accum_vcp, accum_file_name) = with_chunk_accum(|accum| {
                let accum = accum.unwrap();
                (
                    accum.scan_key.clone(),
                    accum.vcp.clone(),
                    accum.file_name.clone(),
                )
            });

            // The worker pool serializes per-scan via the per-worker
            // `CHUNK_ACCUM` thread-local, so we're the sole writer for this
            // scan key. `upsert_scan` internally dispatches first-write vs
            // merge against the existing entry; `scan_touches` is seeded
            // only on the first write.
            let header = ScanHeader {
                scan: scan_key,
                vcp: accum_vcp,
                file_name: Some(accum_file_name),
            };
            store.upsert_scan(&header, &elevations).await.map_err(|e| {
                wasm_bindgen::JsValue::from_str(&format!("Failed to store scan: {}", e))
            })?;
        }

        // --- Build the scan key for response ---
        let scan_key_str = with_chunk_accum(|accum| {
            accum
                .map(|a| a.scan_key.to_storage_key())
                .unwrap_or_default()
        });

        // All completed sweep metadata, accumulated incrementally during flushes.
        let all_sweeps = with_chunk_accum(|accum| accum.unwrap().completed_sweep_metas.clone());

        let vcp = with_chunk_accum(|accum| accum.and_then(|a| a.vcp.clone()));

        let total_ms = t_total.elapsed().as_secs_f64() * 1000.0;

        let accum_info = with_chunk_accum(|accum| {
            accum
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
        let chunk_detail = with_chunk_accum(|accum| {
            let Some(accum) = accum else {
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

        log::debug!(
            "ingest_chunk: chunk={} is_start={} is_end={} radials={} vcp={:?} has_vcp={} completed_elevs={:?} sweeps_stored={} {:.1}ms {}",
            chunk_index, is_start, is_end,
            accum_info.0, accum_info.2, accum_info.1,
            newly_completed, sweeps_stored, total_ms,
            chunk_detail,
        );

        // Current in-progress elevation info
        let current_elevation = with_chunk_accum(|accum| accum.and_then(|a| a.current_elevation));
        let current_elevation_radials = with_chunk_accum(|accum| {
            accum.and_then(|a| a.current_elevation.map(|_| a.current_radials.len() as u32))
        });

        // --- Clear accumulator on end ---
        if is_end {
            set_chunk_accum(None);
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
