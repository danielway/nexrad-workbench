//! Archive and chunk ingestion phases.
//!
//! Provides the core decode pipeline that runs inside the Web Worker:
//! decompression, VCP extraction, radial grouping by elevation, and
//! pre-computed sweep blob generation for IndexedDB storage.

use crate::data::keys::*;
use std::collections::HashMap;

pub(crate) const PRODUCTS: &[(nexrad_render::Product, &str)] = &[
    (nexrad_render::Product::Reflectivity, "reflectivity"),
    (nexrad_render::Product::Velocity, "velocity"),
    (nexrad_render::Product::SpectrumWidth, "spectrum_width"),
    (
        nexrad_render::Product::DifferentialReflectivity,
        "differential_reflectivity",
    ),
    (
        nexrad_render::Product::CorrelationCoefficient,
        "correlation_coefficient",
    ),
    (
        nexrad_render::Product::DifferentialPhase,
        "differential_phase",
    ),
];

pub(crate) fn decode_with_vcp_extraction<'a>(
    messages: impl IntoIterator<Item = nexrad_decode::messages::Message<'a>>,
    extracted_vcp: &mut Option<ExtractedVcp>,
) -> Vec<::nexrad::model::data::Radial> {
    use nexrad_decode::messages::MessageContents;

    let mut radials = Vec::new();
    for msg in messages {
        let has_full_vcp = extracted_vcp
            .as_ref()
            .map(|v| !v.elevations.is_empty())
            .unwrap_or(false);

        match msg.contents() {
            MessageContents::VolumeCoveragePattern(ref vcp_msg) if !has_full_vcp => {
                let header = vcp_msg.header();
                let elevations: Vec<ExtractedVcpElevation> = vcp_msg
                    .elevations()
                    .iter()
                    .map(|e| ExtractedVcpElevation {
                        // nexrad-decode's decode_angle() sums bit 15 as a
                        // positive 180° contribution instead of treating it as
                        // the sign bit, so negative elevations (e.g. KMAX's
                        // -0.2°) come back wrapped near 360°. Real VCP
                        // elevations never exceed ~20°, so any value above 180°
                        // is a wrapped negative.
                        angle: {
                            let a = e.elevation_angle() as f32;
                            if a > 180.0 {
                                a - 360.0
                            } else {
                                a
                            }
                        },
                        waveform: format!("{:?}", e.waveform_type()),
                        prf_number: e.surveillance_prf_number(),
                        is_sails: e.is_sails_cut(),
                        is_mrle: e.is_mrle_cut(),
                        is_base_tilt: e.is_base_tilt_cut(),
                        azimuth_rate: {
                            let rate = e.azimuth_rate();
                            if rate > 0.0 {
                                Some(rate as f32)
                            } else {
                                None
                            }
                        },
                    })
                    .collect();
                *extracted_vcp = Some(ExtractedVcp {
                    number: header.pattern_number(),
                    elevations,
                });
            }
            MessageContents::DigitalRadarData(ref m) if extracted_vcp.is_none() => {
                if let Some(vol_block) = m.volume_data_block() {
                    let raw = vol_block.volume_coverage_pattern_number();
                    if raw > 0 {
                        *extracted_vcp = Some(ExtractedVcp {
                            number: raw,
                            elevations: Vec::new(),
                        });
                    }
                }
            }
            _ => {}
        }
        match msg.into_contents() {
            MessageContents::DigitalRadarData(m) => {
                if let Ok(radial) = m.into_radial() {
                    radials.push(radial);
                }
            }
            MessageContents::DigitalRadarDataLegacy(m) => {
                if let Ok(radial) = m.into_radial() {
                    radials.push(radial);
                }
            }
            _ => {}
        }
    }
    radials
}

pub(crate) struct DecodeResult {
    pub all_radials: Vec<::nexrad::model::data::Radial>,
    pub radial_metas: Vec<(i64, u8, f32, f32)>,
    pub decompress_ms: f64,
    pub decode_ms: f64,
    pub compressed_count: u32,
    pub extracted_vcp: Option<ExtractedVcp>,
    pub has_vcp: bool,
}

pub(crate) fn decompress_and_decode_records(
    records: &[nexrad_data::volume::Record<'_>],
) -> Result<DecodeResult, wasm_bindgen::JsValue> {
    use crate::nexrad::record_decode::decode_record_to_radials;

    let mut decompress_ms_total = 0.0f64;
    let mut decode_only_ms = 0.0f64;
    let mut all_radials: Vec<::nexrad::model::data::Radial> = Vec::new();
    let mut radial_metas: Vec<(i64, u8, f32, f32)> = Vec::new();
    let mut has_vcp = false;
    let mut extracted_vcp: Option<ExtractedVcp> = None;
    let mut compressed_count = 0u32;

    for (record_id, record) in records.iter().enumerate() {
        let record_id = record_id as u32;

        let radials = if record.compressed() {
            compressed_count += 1;
            let t_decompress = web_time::Instant::now();
            let decompressed = record.decompress().map_err(|e| {
                wasm_bindgen::JsValue::from_str(&format!(
                    "Failed to decompress record {}: {}",
                    record_id, e
                ))
            })?;
            decompress_ms_total += t_decompress.elapsed().as_secs_f64() * 1000.0;
            let t_radials = web_time::Instant::now();

            let needs_vcp = extracted_vcp
                .as_ref()
                .map(|v| v.elevations.is_empty())
                .unwrap_or(true);
            let r = if needs_vcp {
                match decompressed.messages() {
                    Ok(msgs) => decode_with_vcp_extraction(msgs, &mut extracted_vcp),
                    Err(_) => Vec::new(),
                }
            } else {
                decompressed.radials().unwrap_or_default()
            };

            decode_only_ms += t_radials.elapsed().as_secs_f64() * 1000.0;
            r
        } else {
            let t_radials = web_time::Instant::now();
            let r = decode_record_to_radials(record.data()).unwrap_or_default();
            decode_only_ms += t_radials.elapsed().as_secs_f64() * 1000.0;
            r
        };

        if record_id == 0 {
            has_vcp = true;
        }

        if !radials.is_empty() {
            for r in &radials {
                radial_metas.push((
                    r.collection_timestamp(),
                    r.elevation_number(),
                    r.elevation_angle_degrees(),
                    r.azimuth_angle_degrees(),
                ));
            }
            all_radials.extend(radials);
        }
    }

    Ok(DecodeResult {
        all_radials,
        radial_metas,
        decompress_ms: decompress_ms_total,
        decode_ms: decode_only_ms,
        compressed_count,
        extracted_vcp,
        has_vcp,
    })
}

pub(crate) fn group_radials_by_elevation(
    all_radials: &[::nexrad::model::data::Radial],
) -> HashMap<u8, Vec<&::nexrad::model::data::Radial>> {
    let mut by_elevation: HashMap<u8, Vec<&::nexrad::model::data::Radial>> = HashMap::new();
    for radial in all_radials {
        by_elevation
            .entry(radial.elevation_number())
            .or_default()
            .push(radial);
    }
    for group in by_elevation.values_mut() {
        group.sort_by(|a, b| {
            a.azimuth_angle_degrees()
                .partial_cmp(&b.azimuth_angle_degrees())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }
    by_elevation
}

pub(crate) fn extract_sweep_blobs(
    by_elevation: &HashMap<u8, Vec<&::nexrad::model::data::Radial>>,
    elevation_numbers: &[u8],
    scan_key: &ScanKey,
) -> Vec<(String, Vec<u8>)> {
    use crate::nexrad::record_decode::extract_sweep_data_from_sorted;

    let mut sweep_blobs: Vec<(String, Vec<u8>)> = Vec::new();
    for &elev_num in elevation_numbers {
        if let Some(sorted_radials) = by_elevation.get(&elev_num) {
            for (product, product_name) in PRODUCTS {
                if let Some(sweep) = extract_sweep_data_from_sorted(sorted_radials, *product) {
                    let key = SweepDataKey::new(scan_key.clone(), elev_num, *product_name);
                    sweep_blobs.push((key.to_storage_key(), sweep.to_bytes()));
                }
            }
        }
    }
    sweep_blobs
}

pub(crate) struct ChunkDecodeResult {
    pub chunk_radials: Vec<::nexrad::model::data::Radial>,
    pub chunk_vcp: Option<ExtractedVcp>,
    pub chunk_has_vcp: bool,
    pub volume_header_time_secs: Option<f64>,
}

pub(crate) fn decode_start_chunk(data: Vec<u8>, accum_has_full_vcp: bool) -> ChunkDecodeResult {
    use crate::nexrad::record_decode::decode_record_to_radials;

    let mut chunk_radials: Vec<::nexrad::model::data::Radial> = Vec::new();
    let mut chunk_vcp: Option<ExtractedVcp> = None;
    let mut chunk_has_vcp = false;
    let mut volume_header_time_secs: Option<f64> = None;

    let file = nexrad_data::volume::File::new(data);

    if let Some(header) = file.header() {
        if let Some(dt) = header.date_time() {
            volume_header_time_secs = Some(dt.timestamp() as f64);
        }
    }

    let records = match file.records() {
        Ok(r) => r,
        Err(e) => {
            log::warn!("Failed to split start chunk: {}", e);
            return ChunkDecodeResult {
                chunk_radials,
                chunk_vcp,
                chunk_has_vcp,
                volume_header_time_secs,
            };
        }
    };

    for (i, record) in records.iter().enumerate() {
        if record.compressed() {
            match record.decompress() {
                Ok(decompressed) => {
                    if !accum_has_full_vcp
                        && chunk_vcp
                            .as_ref()
                            .map(|v| v.elevations.is_empty())
                            .unwrap_or(true)
                    {
                        if let Ok(msgs) = decompressed.messages() {
                            let r = decode_with_vcp_extraction(msgs, &mut chunk_vcp);
                            chunk_radials.extend(r);
                        }
                    } else {
                        chunk_radials.extend(decompressed.radials().unwrap_or_default());
                    }
                }
                Err(e) => {
                    log::warn!("Failed to decompress record {} in start chunk: {}", i, e);
                }
            }
        } else {
            chunk_radials.extend(decode_record_to_radials(record.data()).unwrap_or_default());
        }
        if chunk_vcp.is_some() {
            chunk_has_vcp = true;
        }
    }

    ChunkDecodeResult {
        chunk_radials,
        chunk_vcp,
        chunk_has_vcp,
        volume_header_time_secs,
    }
}

pub(crate) fn decode_subsequent_chunk(
    data: &[u8],
    accum_has_full_vcp: bool,
    chunk_index: u32,
) -> ChunkDecodeResult {
    use crate::nexrad::record_decode::decode_record_to_radials;
    use nexrad_data::volume::Record;

    let mut chunk_radials: Vec<::nexrad::model::data::Radial> = Vec::new();
    let mut chunk_vcp: Option<ExtractedVcp> = None;

    let record = Record::from_slice(data);

    if record.compressed() {
        match record.decompress() {
            Ok(decompressed) => {
                if !accum_has_full_vcp {
                    if let Ok(msgs) = decompressed.messages() {
                        let r = decode_with_vcp_extraction(msgs, &mut chunk_vcp);
                        chunk_radials.extend(r);
                    }
                } else {
                    chunk_radials.extend(decompressed.radials().unwrap_or_default());
                }
            }
            Err(e) => {
                log::warn!("Failed to decompress chunk {}: {}", chunk_index, e);
            }
        }
    } else {
        chunk_radials.extend(decode_record_to_radials(record.data()).unwrap_or_default());
    }

    ChunkDecodeResult {
        chunk_radials,
        chunk_vcp: chunk_vcp.clone(),
        chunk_has_vcp: chunk_vcp.is_some(),
        volume_header_time_secs: None,
    }
}

pub(crate) struct ChunkTimeSpans {
    pub chunk_min_ts_secs: Option<f64>,
    pub chunk_max_ts_secs: Option<f64>,
    pub chunk_elev_spans: Vec<(u8, f64, f64, u32)>,
    pub chunk_elev_az_ranges: Vec<(u8, f32, f32)>,
    pub first_radial_azimuth: Option<f32>,
    pub last_radial_azimuth: Option<f32>,
    pub last_radial_time_secs: Option<f64>,
}

pub(crate) fn compute_chunk_time_spans(
    chunk_radials: &[::nexrad::model::data::Radial],
) -> ChunkTimeSpans {
    let chunk_min_ts_secs: Option<f64> = chunk_radials
        .iter()
        .map(|r| r.collection_timestamp() as f64 / 1000.0)
        .reduce(f64::min);
    let chunk_max_ts_secs: Option<f64> = chunk_radials
        .iter()
        .map(|r| r.collection_timestamp() as f64 / 1000.0)
        .reduce(f64::max);

    let chunk_elev_spans: Vec<(u8, f64, f64, u32)> = {
        let mut map: std::collections::BTreeMap<u8, (f64, f64, u32)> =
            std::collections::BTreeMap::new();
        for r in chunk_radials {
            let elev = r.elevation_number();
            let t = r.collection_timestamp() as f64 / 1000.0;
            map.entry(elev)
                .and_modify(|(min, max, count)| {
                    if t < *min {
                        *min = t;
                    }
                    if t > *max {
                        *max = t;
                    }
                    *count += 1;
                })
                .or_insert((t, t, 1));
        }
        map.into_iter()
            .map(|(elev, (min, max, count))| (elev, min, max, count))
            .collect()
    };

    let chunk_elev_az_ranges: Vec<(u8, f32, f32)> = {
        let mut map: std::collections::BTreeMap<u8, (f32, f32)> = std::collections::BTreeMap::new();
        for r in chunk_radials {
            let elev = r.elevation_number();
            let az = r.azimuth_angle_degrees();
            map.entry(elev)
                .and_modify(|(_, last)| *last = az)
                .or_insert((az, az));
        }
        map.into_iter()
            .map(|(elev, (first, last))| (elev, first, last))
            .collect()
    };

    let first_radial_azimuth: Option<f32> =
        chunk_radials.first().map(|r| r.azimuth_angle_degrees());
    let last_radial_azimuth: Option<f32> = chunk_radials.last().map(|r| r.azimuth_angle_degrees());
    let last_radial_time_secs: Option<f64> = chunk_radials
        .last()
        .map(|r| r.collection_timestamp() as f64 / 1000.0);

    ChunkTimeSpans {
        chunk_min_ts_secs,
        chunk_max_ts_secs,
        chunk_elev_spans,
        chunk_elev_az_ranges,
        first_radial_azimuth,
        last_radial_azimuth,
        last_radial_time_secs,
    }
}

pub(crate) fn build_flush_sweep_blobs(
    all_radials: &[::nexrad::model::data::Radial],
    radial_metas: &[(i64, u8, f32, f32)],
    newly_completed: &[u8],
    scan_key: &ScanKey,
) -> (Vec<(String, Vec<u8>)>, Vec<SweepMeta>) {
    use crate::nexrad::record_decode::extract_sweep_data_from_sorted;

    let by_elevation = group_radials_by_elevation(all_radials);

    let mut blobs: Vec<(String, Vec<u8>)> = Vec::new();
    let mut metas: Vec<SweepMeta> = Vec::new();

    for &elev_num in newly_completed {
        if let Some(sorted_radials) = by_elevation.get(&elev_num) {
            for (product, product_name) in PRODUCTS {
                if let Some(sweep) = extract_sweep_data_from_sorted(sorted_radials, *product) {
                    let key = SweepDataKey::new(scan_key.clone(), elev_num, *product_name);
                    blobs.push((key.to_storage_key(), sweep.to_bytes()));
                }
            }

            let elev_metas: Vec<&(i64, u8, f32, f32)> = radial_metas
                .iter()
                .filter(|(_, en, _, _)| *en == elev_num)
                .collect();
            if !elev_metas.is_empty() {
                let min_ts = elev_metas.iter().map(|(t, _, _, _)| *t).min().unwrap();
                let max_ts = elev_metas.iter().map(|(t, _, _, _)| *t).max().unwrap();
                let angle_sum: f64 = elev_metas.iter().map(|(_, _, a, _)| *a as f64).sum();
                let count = elev_metas.len();
                let first_az = elev_metas
                    .iter()
                    .min_by_key(|(t, _, _, _)| *t)
                    .map(|(_, _, _, az)| *az)
                    .unwrap_or(0.0);
                metas.push(SweepMeta {
                    start: min_ts as f64 / 1000.0,
                    end: max_ts as f64 / 1000.0,
                    elevation: (angle_sum / count as f64) as f32,
                    elevation_number: elev_num,
                    start_azimuth: first_az,
                });
            }
        }
    }

    (blobs, metas)
}

pub(crate) fn build_sweep_meta(radial_metas: &[(i64, u8, f32, f32)]) -> Vec<SweepMeta> {
    use std::collections::BTreeMap;

    struct Accum {
        min_ts_ms: i64,
        max_ts_ms: i64,
        angle_sum: f64,
        count: u32,
        first_azimuth: f32,
    }

    let mut groups: BTreeMap<u8, Accum> = BTreeMap::new();

    for &(ts_ms, elev_num, elev_angle, azimuth) in radial_metas {
        let entry = groups.entry(elev_num).or_insert(Accum {
            min_ts_ms: ts_ms,
            max_ts_ms: ts_ms,
            angle_sum: 0.0,
            count: 0,
            first_azimuth: azimuth,
        });
        if ts_ms < entry.min_ts_ms {
            entry.min_ts_ms = ts_ms;
            entry.first_azimuth = azimuth;
        }
        entry.max_ts_ms = entry.max_ts_ms.max(ts_ms);
        entry.angle_sum += elev_angle as f64;
        entry.count += 1;
    }

    groups
        .into_iter()
        .map(|(elev_num, acc)| SweepMeta {
            start: acc.min_ts_ms as f64 / 1000.0,
            end: acc.max_ts_ms as f64 / 1000.0,
            elevation: (acc.angle_sum / acc.count as f64) as f32,
            elevation_number: elev_num,
            start_azimuth: acc.first_azimuth,
        })
        .collect()
}
