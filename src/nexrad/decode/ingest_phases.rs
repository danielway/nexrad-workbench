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
}

pub(crate) fn decompress_and_decode_records(
    records: &[nexrad_data::volume::Record<'_>],
) -> Result<DecodeResult, wasm_bindgen::JsValue> {
    use super::record_decode::decode_record_to_radials;

    let mut decompress_ms_total = 0.0f64;
    let mut decode_only_ms = 0.0f64;
    let mut all_radials: Vec<::nexrad::model::data::Radial> = Vec::new();
    let mut radial_metas: Vec<(i64, u8, f32, f32)> = Vec::new();
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

/// Aggregate per-elevation timing (start/end Unix-secs, mean angle, first
/// azimuth) from a flat list of radial metadata. Returns `None` for
/// elevations with no entries.
fn timing_for_elevation(radial_metas: &[(i64, u8, f32, f32)], elev_num: u8) -> Option<SweepTiming> {
    let mut min_ts_ms = i64::MAX;
    let mut max_ts_ms = i64::MIN;
    let mut angle_sum: f64 = 0.0;
    let mut count: u32 = 0;
    let mut first_az_at_min_ts: f32 = 0.0;

    for &(ts_ms, en, elev_angle, azimuth) in radial_metas {
        if en != elev_num {
            continue;
        }
        if ts_ms < min_ts_ms {
            min_ts_ms = ts_ms;
            first_az_at_min_ts = azimuth;
        }
        if ts_ms > max_ts_ms {
            max_ts_ms = ts_ms;
        }
        angle_sum += elev_angle as f64;
        count += 1;
    }

    if count == 0 {
        return None;
    }
    Some(SweepTiming {
        start_secs: min_ts_ms as f64 / 1000.0,
        end_secs: max_ts_ms as f64 / 1000.0,
        elevation_angle: (angle_sum / count as f64) as f32,
        start_azimuth: first_az_at_min_ts,
    })
}

/// Build an `ElevationUpload` for one elevation: extract a blob per
/// product that has data, attach timing. Returns `None` when no product
/// extracted (the elevation contributes nothing storable) — this is the
/// drop point that prevents phantom manifest entries from being passed
/// down to the IDB layer.
fn elevation_upload_for(
    sorted_radials: &[&::nexrad::model::data::Radial],
    radial_metas: &[(i64, u8, f32, f32)],
    elev_num: u8,
) -> Option<ElevationUpload> {
    use super::record_decode::extract_sweep_data_from_sorted;

    let mut blobs: Vec<ProductBlob> = Vec::new();
    for (product, product_name) in PRODUCTS {
        if let Some(sweep) = extract_sweep_data_from_sorted(sorted_radials, *product) {
            blobs.push(ProductBlob {
                product: product_name,
                bytes: sweep.to_bytes(),
            });
        }
    }
    if blobs.is_empty() {
        return None;
    }

    let timing = timing_for_elevation(radial_metas, elev_num)?;
    Some(ElevationUpload {
        elevation_number: elev_num,
        timing,
        blobs,
    })
}

/// Build an `ElevationUpload` per elevation present in `by_elevation`.
/// Elevations with no extractable product yield no upload — the manifest
/// the IDB layer derives from this list is guaranteed to describe blobs
/// that will actually be written.
pub(crate) fn build_elevation_uploads(
    by_elevation: &HashMap<u8, Vec<&::nexrad::model::data::Radial>>,
    radial_metas: &[(i64, u8, f32, f32)],
) -> Vec<ElevationUpload> {
    let mut uploads: Vec<ElevationUpload> = by_elevation
        .iter()
        .filter_map(|(&elev_num, sorted_radials)| {
            elevation_upload_for(sorted_radials, radial_metas, elev_num)
        })
        .collect();
    uploads.sort_by_key(|u| u.elevation_number);
    uploads
}

pub(crate) struct ChunkDecodeResult {
    pub chunk_radials: Vec<::nexrad::model::data::Radial>,
    pub chunk_vcp: Option<ExtractedVcp>,
    pub chunk_has_vcp: bool,
    pub volume_header_time_secs: Option<f64>,
}

pub(crate) fn decode_start_chunk(data: Vec<u8>, accum_has_full_vcp: bool) -> ChunkDecodeResult {
    use super::record_decode::decode_record_to_radials;

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
    use super::record_decode::decode_record_to_radials;
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

/// Build an `ElevationUpload` for each newly-completed elevation in a
/// chunk flush. Same drop-on-no-extracted-product semantics as
/// `build_elevation_uploads`.
pub(crate) fn build_elevation_uploads_for_flush(
    all_radials: &[::nexrad::model::data::Radial],
    radial_metas: &[(i64, u8, f32, f32)],
    newly_completed: &[u8],
) -> Vec<ElevationUpload> {
    let by_elevation = group_radials_by_elevation(all_radials);

    newly_completed
        .iter()
        .filter_map(|&elev_num| {
            let sorted_radials = by_elevation.get(&elev_num)?;
            elevation_upload_for(sorted_radials, radial_metas, elev_num)
        })
        .collect()
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    use ::nexrad::model::data::{Radial, RadialStatus};

    /// Construct a minimal `Radial` with no moment data. The pure functions
    /// under test only read timestamp, azimuth/elevation angle, and elevation
    /// number — all moment fields are `None`, which makes
    /// `extract_sweep_data_from_sorted` return `None` for every product (so no
    /// blobs are extractable). That exercises the "drop on no extracted
    /// product" path deterministically.
    fn radial(
        collection_timestamp_ms: i64,
        elevation_number: u8,
        elevation_angle_degrees: f32,
        azimuth_angle_degrees: f32,
    ) -> Radial {
        Radial::new(
            collection_timestamp_ms,
            0,
            azimuth_angle_degrees,
            1.0,
            RadialStatus::IntermediateRadialData,
            elevation_number,
            elevation_angle_degrees,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
    }

    // ---- timing_for_elevation -------------------------------------------

    #[wasm_bindgen_test]
    fn timing_for_elevation_none_when_no_match() {
        let metas: Vec<(i64, u8, f32, f32)> = vec![(1000, 1, 0.5, 10.0)];
        assert!(timing_for_elevation(&metas, 2).is_none());
    }

    #[wasm_bindgen_test]
    fn timing_for_elevation_none_when_empty() {
        let metas: Vec<(i64, u8, f32, f32)> = vec![];
        assert!(timing_for_elevation(&metas, 1).is_none());
    }

    #[wasm_bindgen_test]
    fn timing_for_elevation_computes_bounds_and_mean() {
        // Elevation 1 rows: ts 3000 (az 30), 1000 (az 10), 2000 (az 20).
        // Elevation 2 row is ignored.
        let metas: Vec<(i64, u8, f32, f32)> = vec![
            (3000, 1, 0.5, 30.0),
            (1000, 1, 0.5, 10.0),
            (5000, 2, 9.9, 99.0),
            (2000, 1, 0.8, 20.0),
        ];
        let t = timing_for_elevation(&metas, 1).expect("elev 1 present");
        // min ts 1000ms -> 1.0s, max ts 3000ms -> 3.0s
        assert!((t.start_secs - 1.0).abs() < 1e-9, "start {}", t.start_secs);
        assert!((t.end_secs - 3.0).abs() < 1e-9, "end {}", t.end_secs);
        // mean of 0.5, 0.5, 0.8 = 0.6
        assert!(
            (t.elevation_angle - 0.6).abs() < 1e-5,
            "angle {}",
            t.elevation_angle
        );
        // azimuth at the minimum timestamp (1000ms) is 10.0
        assert!(
            (t.start_azimuth - 10.0).abs() < 1e-5,
            "start_az {}",
            t.start_azimuth
        );
    }

    #[wasm_bindgen_test]
    fn timing_for_elevation_single_row() {
        let metas: Vec<(i64, u8, f32, f32)> = vec![(7500, 3, 4.0, 123.0)];
        let t = timing_for_elevation(&metas, 3).unwrap();
        assert!((t.start_secs - 7.5).abs() < 1e-9);
        assert!((t.end_secs - 7.5).abs() < 1e-9);
        assert!((t.elevation_angle - 4.0).abs() < 1e-5);
        assert!((t.start_azimuth - 123.0).abs() < 1e-5);
    }

    // ---- group_radials_by_elevation -------------------------------------

    #[wasm_bindgen_test]
    fn group_radials_by_elevation_buckets_and_sorts() {
        let radials = vec![
            radial(1000, 1, 0.5, 50.0),
            radial(1001, 2, 1.5, 10.0),
            radial(1002, 1, 0.5, 20.0),
            radial(1003, 1, 0.5, 35.0),
            radial(1004, 2, 1.5, 5.0),
        ];
        let grouped = group_radials_by_elevation(&radials);
        assert_eq!(grouped.len(), 2);

        let e1 = grouped.get(&1).expect("elev 1");
        assert_eq!(e1.len(), 3);
        // sorted ascending by azimuth: 20, 35, 50
        let az1: Vec<f32> = e1.iter().map(|r| r.azimuth_angle_degrees()).collect();
        assert!((az1[0] - 20.0).abs() < 1e-5);
        assert!((az1[1] - 35.0).abs() < 1e-5);
        assert!((az1[2] - 50.0).abs() < 1e-5);

        let e2 = grouped.get(&2).expect("elev 2");
        assert_eq!(e2.len(), 2);
        let az2: Vec<f32> = e2.iter().map(|r| r.azimuth_angle_degrees()).collect();
        assert!((az2[0] - 5.0).abs() < 1e-5);
        assert!((az2[1] - 10.0).abs() < 1e-5);
    }

    #[wasm_bindgen_test]
    fn group_radials_by_elevation_empty() {
        let radials: Vec<Radial> = vec![];
        let grouped = group_radials_by_elevation(&radials);
        assert!(grouped.is_empty());
    }

    // ---- compute_chunk_time_spans ---------------------------------------

    #[wasm_bindgen_test]
    fn compute_chunk_time_spans_empty() {
        let radials: Vec<Radial> = vec![];
        let spans = compute_chunk_time_spans(&radials);
        assert!(spans.chunk_min_ts_secs.is_none());
        assert!(spans.chunk_max_ts_secs.is_none());
        assert!(spans.chunk_elev_spans.is_empty());
        assert!(spans.chunk_elev_az_ranges.is_empty());
        assert!(spans.first_radial_azimuth.is_none());
        assert!(spans.last_radial_azimuth.is_none());
        assert!(spans.last_radial_time_secs.is_none());
    }

    #[wasm_bindgen_test]
    fn compute_chunk_time_spans_min_max_and_first_last() {
        let radials = vec![
            radial(2000, 1, 0.5, 100.0),
            radial(1000, 1, 0.5, 200.0),
            radial(3000, 2, 1.5, 300.0),
        ];
        let spans = compute_chunk_time_spans(&radials);
        // overall min ts 1000ms -> 1.0, max 3000ms -> 3.0
        assert!((spans.chunk_min_ts_secs.unwrap() - 1.0).abs() < 1e-9);
        assert!((spans.chunk_max_ts_secs.unwrap() - 3.0).abs() < 1e-9);
        // first radial is index 0 (az 100), last is index 2 (az 300)
        assert!((spans.first_radial_azimuth.unwrap() - 100.0).abs() < 1e-5);
        assert!((spans.last_radial_azimuth.unwrap() - 300.0).abs() < 1e-5);
        // last radial collection time 3000ms -> 3.0s
        assert!((spans.last_radial_time_secs.unwrap() - 3.0).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn compute_chunk_time_spans_elev_spans_btree_sorted() {
        // Provide elevation 2 rows before elevation 1 to confirm BTree ordering.
        let radials = vec![
            radial(5000, 2, 1.5, 10.0),
            radial(6000, 2, 1.5, 20.0),
            radial(1000, 1, 0.5, 30.0),
            radial(4000, 1, 0.5, 40.0),
            radial(2000, 1, 0.5, 50.0),
        ];
        let spans = compute_chunk_time_spans(&radials);
        // elev_spans is BTreeMap-ordered: elev 1 then elev 2.
        assert_eq!(spans.chunk_elev_spans.len(), 2);

        let (e1, e1_min, e1_max, e1_count) = spans.chunk_elev_spans[0];
        assert_eq!(e1, 1);
        assert!((e1_min - 1.0).abs() < 1e-9, "e1 min {}", e1_min);
        assert!((e1_max - 4.0).abs() < 1e-9, "e1 max {}", e1_max);
        assert_eq!(e1_count, 3);

        let (e2, e2_min, e2_max, e2_count) = spans.chunk_elev_spans[1];
        assert_eq!(e2, 2);
        assert!((e2_min - 5.0).abs() < 1e-9, "e2 min {}", e2_min);
        assert!((e2_max - 6.0).abs() < 1e-9, "e2 max {}", e2_max);
        assert_eq!(e2_count, 2);
    }

    #[wasm_bindgen_test]
    fn compute_chunk_time_spans_az_ranges_first_last() {
        // az range per elevation = (first az seen, last az seen) in iteration order.
        let radials = vec![
            radial(1000, 1, 0.5, 11.0),
            radial(2000, 1, 0.5, 22.0),
            radial(3000, 1, 0.5, 33.0),
            radial(4000, 2, 1.5, 90.0),
        ];
        let spans = compute_chunk_time_spans(&radials);
        assert_eq!(spans.chunk_elev_az_ranges.len(), 2);

        let (e1, first1, last1) = spans.chunk_elev_az_ranges[0];
        assert_eq!(e1, 1);
        assert!((first1 - 11.0).abs() < 1e-5, "first {}", first1);
        assert!((last1 - 33.0).abs() < 1e-5, "last {}", last1);

        let (e2, first2, last2) = spans.chunk_elev_az_ranges[1];
        assert_eq!(e2, 2);
        // single radial: first == last
        assert!((first2 - 90.0).abs() < 1e-5);
        assert!((last2 - 90.0).abs() < 1e-5);
    }

    // ---- build_elevation_uploads (drop-on-no-product semantics) ---------

    #[wasm_bindgen_test]
    fn build_elevation_uploads_empty_input() {
        let by_elevation = group_radials_by_elevation(&[]);
        let metas: Vec<(i64, u8, f32, f32)> = vec![];
        let uploads = build_elevation_uploads(&by_elevation, &metas);
        assert!(uploads.is_empty());
    }

    #[wasm_bindgen_test]
    fn build_elevation_uploads_drops_radials_without_moment_data() {
        // Radials carry no moment data, so no product blob can be extracted;
        // every elevation is dropped -> no phantom manifest entries.
        let radials = vec![
            radial(1000, 1, 0.5, 10.0),
            radial(1001, 1, 0.5, 20.0),
            radial(1002, 2, 1.5, 30.0),
        ];
        let by_elevation = group_radials_by_elevation(&radials);
        let metas: Vec<(i64, u8, f32, f32)> = vec![
            (1000, 1, 0.5, 10.0),
            (1001, 1, 0.5, 20.0),
            (1002, 2, 1.5, 30.0),
        ];
        let uploads = build_elevation_uploads(&by_elevation, &metas);
        assert!(
            uploads.is_empty(),
            "expected no uploads for moment-less radials, got {}",
            uploads.len()
        );
    }

    // ---- build_elevation_uploads_for_flush ------------------------------

    #[wasm_bindgen_test]
    fn build_elevation_uploads_for_flush_empty_when_no_completed() {
        let radials = vec![radial(1000, 1, 0.5, 10.0)];
        let metas: Vec<(i64, u8, f32, f32)> = vec![(1000, 1, 0.5, 10.0)];
        let uploads = build_elevation_uploads_for_flush(&radials, &metas, &[]);
        assert!(uploads.is_empty());
    }

    #[wasm_bindgen_test]
    fn build_elevation_uploads_for_flush_drops_absent_and_momentless() {
        let radials = vec![radial(1000, 1, 0.5, 10.0), radial(2000, 1, 0.5, 20.0)];
        let metas: Vec<(i64, u8, f32, f32)> = vec![(1000, 1, 0.5, 10.0), (2000, 1, 0.5, 20.0)];
        // Elevation 9 is not present in the radials; elevation 1 has no moment
        // data. Both yield nothing.
        let uploads = build_elevation_uploads_for_flush(&radials, &metas, &[9, 1]);
        assert!(uploads.is_empty());
    }
}
