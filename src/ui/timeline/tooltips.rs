//! Tooltip rendering for timeline elements: scans, sweeps, and realtime volumes.

use super::{format_timestamp_full, DetailLevel};
use crate::data::ScanCompleteness;
use crate::state::radar_data::RadarTimeline;
use eframe::egui::{self, Color32, Pos2, Rect, RichText, Vec2};

/// Render hover tooltip for timeline elements.
#[allow(clippy::too_many_arguments)]
pub(super) fn render_timeline_tooltip(
    ui: &mut egui::Ui,
    timeline: &RadarTimeline,
    live_state: &crate::state::LiveModeState,
    hover_ts: f64,
    hover_pos: Pos2,
    scan_rect: &Rect,
    sweep_rect: &Rect,
    detail_level: DetailLevel,
    use_local: bool,
    now_secs: f64,
) {
    let in_sweep_track = detail_level == DetailLevel::Sweeps && hover_pos.y > sweep_rect.top();

    // Find the scan at the hovered timestamp
    let scan = timeline
        .scans_in_range(hover_ts - 0.5, hover_ts + 0.5)
        .find(|s| s.start_time <= hover_ts && s.end_time >= hover_ts);

    // Check if hovering within the active real-time volume (including projected future)
    let in_active_volume =
        scan.is_none() && live_state.is_active() && live_state.current_volume_start.is_some() && {
            let vol_start = live_state.current_volume_start.unwrap();
            let expected_dur = live_state.last_volume_duration_secs.unwrap_or(300.0);
            hover_ts >= vol_start && hover_ts <= vol_start + expected_dur
        };

    // If in sweep track, search for sweep across ALL visible scans (not just the
    // scan containing hover_ts). This handles edge cases where a sweep's time range
    // extends before its parent scan's start_time.
    let (sweep, sweep_parent_scan) = if in_sweep_track {
        let mut found = None;
        for s in timeline.scans_in_range(hover_ts - 600.0, hover_ts + 600.0) {
            if let Some(sw) = s
                .sweeps
                .iter()
                .find(|sw| sw.start_time <= hover_ts && sw.end_time >= hover_ts)
            {
                found = Some((sw, s));
                break;
            }
        }
        match found {
            Some((sw, s)) => (Some(sw), Some(s)),
            None => (None, None),
        }
    } else {
        (None, None)
    };

    if scan.is_none() && sweep.is_none() && !in_active_volume {
        return;
    }

    egui::Tooltip::always_open(
        ui.ctx().clone(),
        egui::LayerId::new(egui::Order::Tooltip, ui.id()),
        ui.id().with("tl_tooltip"),
        Rect::from_center_size(hover_pos, Vec2::splat(20.0)),
    )
    .show(|ui: &mut egui::Ui| {
        if let Some(sweep) = sweep {
            render_sweep_tooltip_content(ui, sweep, sweep_parent_scan, use_local);
        } else if in_active_volume {
            render_realtime_volume_tooltip(
                ui,
                live_state,
                hover_ts,
                now_secs,
                in_sweep_track,
                use_local,
            );
        } else if let Some(scan) = scan {
            render_scan_tooltip_content(ui, scan, live_state, use_local);
        }
    });

    let _ = scan_rect; // suppress unused warning when not in sweep mode
}

/// Render tooltip content when hovering over a sweep block.
fn render_sweep_tooltip_content(
    ui: &mut egui::Ui,
    sweep: &crate::state::radar_data::Sweep,
    parent_scan: Option<&crate::state::radar_data::Scan>,
    use_local: bool,
) {
    ui.label(
        RichText::new(format!("Elevation Sweep #{}", sweep.elevation_number))
            .strong()
            .size(12.0),
    );
    ui.label(
        RichText::new("One 360\u{00B0} rotation at a single antenna tilt angle.")
            .size(10.0)
            .weak(),
    );
    ui.separator();

    let sweep_count = parent_scan
        .and_then(|s| s.vcp_pattern.as_ref().map(|v| v.elevations.len()))
        .or_else(|| parent_scan.map(|s| s.sweeps.len()))
        .unwrap_or(0);
    if sweep_count > 0 {
        ui.label(format!(
            "Elevation: {:.1}\u{00B0} (cut #{} of {})",
            sweep.elevation, sweep.elevation_number, sweep_count
        ));
    } else {
        ui.label(format!(
            "Elevation: {:.1}\u{00B0} (cut #{})",
            sweep.elevation, sweep.elevation_number
        ));
    }

    let duration = sweep.end_time - sweep.start_time;
    let start_str = format_timestamp_full(sweep.start_time, use_local);
    let end_str = format_timestamp_full(sweep.end_time, use_local);
    ui.label(format!(
        "Time: {} \u{2192} {} ({:.0}s)",
        start_str, end_str, duration
    ));

    // Warn if sweep extends outside its parent scan
    if let Some(ps) = parent_scan {
        if sweep.start_time < ps.start_time || sweep.end_time > ps.end_time {
            ui.label(
                RichText::new("Note: sweep time range extends outside its parent scan")
                    .size(9.0)
                    .italics()
                    .color(Color32::from_rgb(255, 200, 100)),
            );
        }
    }

    // Waveform and products from VCP
    if let Some(vcp) = parent_scan.and_then(|s| s.vcp_pattern.as_ref()) {
        if let Some(vcp_elev) = vcp
            .elevations
            .get(sweep.elevation_number.saturating_sub(1) as usize)
        {
            let wf_label = match vcp_elev.waveform.as_str() {
                "CS" | "ContiguousSurveillance" => "Contiguous Surveillance",
                "CDW" | "ContiguousDopplerWithGating" => "Contiguous Doppler (Gated)",
                "CDWO" | "ContiguousDopplerWithoutGating" => "Contiguous Doppler",
                "B" | "Batch" => "Batch",
                "SPP" | "StaggeredPulsePair" => "Staggered Pulse Pair",
                other => other,
            };
            let products = match vcp_elev.waveform.as_str() {
                "CS" | "ContiguousSurveillance" => "Reflectivity",
                "CDW"
                | "CDWO"
                | "ContiguousDopplerWithGating"
                | "ContiguousDopplerWithoutGating" => "Velocity",
                "B" | "Batch" => "Reflectivity / Velocity",
                "SPP" | "StaggeredPulsePair" => "Reflectivity / Velocity / Differential",
                _ => "Unknown",
            };
            ui.label(format!("Waveform: {}", wf_label));
            ui.label(format!("Products: {}", products));

            let mut flags = Vec::new();
            if vcp_elev.is_sails {
                flags.push("SAILS");
            }
            if vcp_elev.is_mrle {
                flags.push("MRLE");
            }
            if vcp_elev.is_base_tilt {
                flags.push("Base Tilt");
            }
            if !flags.is_empty() {
                ui.label(format!("Flags: {}", flags.join(", ")));
            }
        }
    }
}

/// Render tooltip for the in-progress realtime volume.
///
/// When hovering the sweep track, this identifies which realtime sweep block
/// is under the cursor and shows per-sweep details including chunk progress.
/// When hovering the scan track, it shows the volume-level summary.
#[allow(clippy::too_many_arguments)]
fn render_realtime_volume_tooltip(
    ui: &mut egui::Ui,
    live_state: &crate::state::LiveModeState,
    hover_ts: f64,
    now_secs: f64,
    in_sweep_track: bool,
    use_local: bool,
) {
    let vol_start = live_state.current_volume_start.unwrap();
    let expected_dur = live_state.last_volume_duration_secs.unwrap_or(300.0);
    let expected_end = vol_start + expected_dur;
    let now = now_secs;
    let past_now = hover_ts > now;
    let vcp_num = live_state.current_vcp_number.unwrap_or(0);
    let expected_count = live_state.expected_elevation_count.unwrap_or(0) as usize;

    // -- Per-sweep tooltip when hovering the sweep track --
    if in_sweep_track && expected_count > 0 {
        let vcp_def = crate::state::get_vcp_definition(vcp_num);

        // Per-elevation sweep durations (same logic as render_realtime_progress)
        let sweep_dur_for = |idx: usize| -> f64 {
            live_state
                .sweep_duration_for(idx)
                .unwrap_or(expected_dur / expected_count.max(1) as f64)
        };
        let sweep_start_offset_for = |idx: usize| -> f64 {
            live_state
                .sweep_start_offset(idx)
                .unwrap_or(idx as f64 * expected_dur / expected_count.max(1) as f64)
        };

        // Replicate the sweep-to-timestamp mapping from render_realtime_progress
        // to find which elevation block contains hover_ts.
        let mut hovered_elev: Option<(u8, f64, f64)> = None;
        let mut nearest_elev: Option<(u8, f64, f64)> = None;
        let mut nearest_dist: f64 = f64::MAX;
        for elev_idx in 0..expected_count {
            let elev_num = (elev_idx + 1) as u8;
            let is_complete = live_state.elevations_received.contains(&elev_num);
            let this_sweep_dur = sweep_dur_for(elev_idx);

            let (sw_start, sw_end) = if is_complete {
                if let Some(meta) = live_state
                    .completed_sweep_metas
                    .iter()
                    .find(|m| m.elevation_number == elev_num)
                {
                    (meta.start, meta.end)
                } else {
                    let offset = sweep_start_offset_for(elev_idx);
                    (vol_start + offset, vol_start + offset + this_sweep_dur)
                }
            } else {
                let anchor_end = live_state
                    .completed_sweep_metas
                    .iter()
                    .filter(|m| m.elevation_number < elev_num)
                    .max_by_key(|m| m.elevation_number)
                    .map(|m| m.end);

                let chunk_min = live_state
                    .chunk_elev_spans
                    .iter()
                    .filter(|&&(e, _, _, _)| e == elev_num)
                    .map(|&(_, s, _, _)| s)
                    .reduce(f64::min);
                let chunk_max = live_state
                    .chunk_elev_spans
                    .iter()
                    .filter(|&&(e, _, _, _)| e == elev_num)
                    .map(|&(_, _, e, _)| e)
                    .reduce(f64::max);

                let sw_start_actual = match (chunk_min, anchor_end) {
                    (Some(cm), _) => cm,
                    (None, Some(ae)) => {
                        let anchor_elev_num = live_state
                            .completed_sweep_metas
                            .iter()
                            .filter(|m| m.elevation_number < elev_num)
                            .max_by_key(|m| m.elevation_number)
                            .map(|m| m.elevation_number)
                            .unwrap_or(0);
                        let anchor_idx = anchor_elev_num as usize;
                        let remaining_dur = (vol_start + expected_dur) - ae;
                        let remaining_weight_sum: f64 =
                            (anchor_idx..expected_count).map(&sweep_dur_for).sum();
                        if remaining_weight_sum > 0.0 {
                            let offset_from_anchor: f64 = (anchor_idx..elev_idx)
                                .map(|i| (sweep_dur_for(i) / remaining_weight_sum) * remaining_dur)
                                .sum();
                            ae + offset_from_anchor
                        } else {
                            ae
                        }
                    }
                    (None, None) => vol_start + sweep_start_offset_for(elev_idx),
                };

                let est_sweep_end = sw_start_actual + this_sweep_dur;
                let sw_end_actual = match chunk_max {
                    Some(cm) => cm.max(est_sweep_end),
                    None => est_sweep_end,
                };

                (sw_start_actual, sw_end_actual)
            };

            if hover_ts >= sw_start && hover_ts <= sw_end {
                hovered_elev = Some((elev_num, sw_start, sw_end));
                break;
            }

            // Track nearest sweep so we can snap to it if hover_ts falls in a
            // gap (e.g. due to timeline auto-scroll shifting hover_ts between
            // frames). Without this, the tooltip flickers between per-sweep
            // and volume-level content as the cursor drifts across boundaries.
            let dist = if hover_ts < sw_start {
                sw_start - hover_ts
            } else {
                hover_ts - sw_end
            };
            if nearest_elev.is_none() || dist < nearest_dist {
                nearest_elev = Some((elev_num, sw_start, sw_end));
                nearest_dist = dist;
            }
        }

        // Snap to nearest sweep if hover_ts missed due to frame-to-frame drift
        if hovered_elev.is_none()
            && nearest_dist < (expected_dur / expected_count.max(1) as f64) * 0.5
        {
            hovered_elev = nearest_elev;
        }

        if let Some((elev_num, sw_start, sw_end)) = hovered_elev {
            let is_complete = live_state.elevations_received.contains(&elev_num);
            let is_downloading =
                !is_complete && live_state.current_in_progress_elevation == Some(elev_num);
            let elev_angle = vcp_def
                .and_then(|d| d.elevations.get(elev_num.saturating_sub(1) as usize))
                .map(|e| e.angle)
                .unwrap_or(0.5 * elev_num as f32);

            // Header
            let state_label = if is_complete {
                "Complete"
            } else if is_downloading {
                "Collecting"
            } else {
                "Pending"
            };
            ui.label(
                RichText::new(format!(
                    "Elevation Sweep #{} \u{2014} {}",
                    elev_num, state_label
                ))
                .strong()
                .size(12.0),
            );
            ui.label(
                RichText::new(format!(
                    "{:.1}\u{00B0} (cut #{} of {})",
                    elev_angle, elev_num, expected_count
                ))
                .size(10.0)
                .weak(),
            );
            ui.separator();

            if is_complete {
                // Show actual timing for completed sweeps
                if let Some(meta) = live_state
                    .completed_sweep_metas
                    .iter()
                    .find(|m| m.elevation_number == elev_num)
                {
                    let duration = meta.end - meta.start;
                    let start_str = format_timestamp_full(meta.start, use_local);
                    ui.label(format!("Time: {} ({:.0}s)", start_str, duration));
                }
                ui.label(
                    RichText::new("Data received and stored.")
                        .size(10.0)
                        .color(Color32::from_rgb(100, 200, 100)),
                );
            } else if is_downloading {
                // Show chunk-level progress
                let chunks_for_elev: Vec<_> = live_state
                    .chunk_elev_spans
                    .iter()
                    .filter(|&&(e, _, _, _)| e == elev_num)
                    .collect();
                let completed_chunks = chunks_for_elev.len();
                let in_progress_radials = live_state.current_in_progress_radials.unwrap_or(0);

                let total_radials: u32 =
                    chunks_for_elev.iter().map(|&&(_, _, _, r)| r).sum::<u32>()
                        + in_progress_radials;

                ui.label(format!("Radials: {}/360 collected", total_radials));

                // Total chunks received for the whole volume gives context
                let total_volume_chunks = live_state.chunks_received;

                // Show per-chunk breakdown
                if completed_chunks > 0 || in_progress_radials > 0 {
                    ui.separator();
                    let has_active = in_progress_radials > 0
                        || live_state.phase == crate::state::LivePhase::Streaming;
                    let display_total = if has_active {
                        completed_chunks + 1
                    } else {
                        completed_chunks
                    };
                    ui.label(
                        RichText::new(format!(
                            "Chunks for this elevation ({} total in volume):",
                            total_volume_chunks
                        ))
                        .size(10.0)
                        .weak(),
                    );
                    for (i, &&(_, _, _, cr)) in chunks_for_elev.iter().enumerate() {
                        let chunk_num = i + 1;
                        let label = format!(
                            "  Chunk {}/{}: {} radials, collected",
                            chunk_num, display_total, cr
                        );
                        ui.label(RichText::new(label).size(10.0));
                    }
                    if has_active {
                        let chunk_num = completed_chunks + 1;
                        let label = format!(
                            "  Chunk {}/{}: {} radials, collecting\u{2026}",
                            chunk_num, display_total, in_progress_radials
                        );
                        ui.label(
                            RichText::new(label)
                                .size(10.0)
                                .color(Color32::from_rgb(100, 180, 255)),
                        );
                    }
                }

                // Countdown if waiting
                let countdown = live_state.countdown_remaining_secs(now);
                if let Some(remaining) = countdown {
                    ui.label(format!("Next chunk in ~{}s", remaining.ceil() as i32));
                }
            } else {
                // Future/pending sweep
                let duration = sw_end - sw_start;
                ui.label(format!("Est. duration: ~{:.0}s", duration));
                ui.label(
                    RichText::new("Not yet started \u{2014} bounds are estimated.")
                        .size(10.0)
                        .italics()
                        .color(Color32::from_rgba_unmultiplied(180, 200, 220, 160)),
                );
            }

            // VCP waveform info if available
            if let Some(vcp_def) = vcp_def {
                if let Some(vcp_elev) = vcp_def.elevations.get(elev_num.saturating_sub(1) as usize)
                {
                    ui.separator();
                    let wf_label = match vcp_elev.waveform {
                        "CS" | "ContiguousSurveillance" => "Contiguous Surveillance",
                        "CDW" | "ContiguousDopplerWithGating" => "Contiguous Doppler (Gated)",
                        "CDWO" | "ContiguousDopplerWithoutGating" => "Contiguous Doppler",
                        "B" | "Batch" => "Batch",
                        "SPP" | "StaggeredPulsePair" => "Staggered Pulse Pair",
                        other => other,
                    };
                    ui.label(format!("Waveform: {}", wf_label));
                }
            }

            return;
        }
    }

    // -- Volume-level tooltip (scan track or no sweep match) --
    let vcp_label = if vcp_num > 0 {
        format!("VCP {}", vcp_num)
    } else {
        "Unknown VCP".to_string()
    };
    ui.label(
        RichText::new(format!("Volume Scan In Progress ({})", vcp_label))
            .strong()
            .size(12.0),
    );

    let mode_desc = match vcp_num {
        215 | 212 => "Precipitation Mode",
        31 | 32 | 35 => "Clear Air Mode",
        12 | 121 => "Severe Weather Mode",
        _ if vcp_num > 0 => "Known Mode",
        _ => "Unknown Mode",
    };
    ui.label(
        RichText::new(format!(
            "Radar is actively collecting data. ({})",
            mode_desc
        ))
        .size(10.0)
        .weak(),
    );
    ui.separator();

    let start_str = format_timestamp_full(vol_start, use_local);
    ui.label(format!("Started: {}", start_str));
    // Round to whole seconds so text doesn't change every frame (avoids tooltip resize flicker)
    let elapsed = (now - vol_start).floor();
    let remaining = (expected_end - now).ceil();
    if remaining > 0.0 {
        ui.label(format!(
            "Elapsed: {}s / est. {:.0}s total",
            elapsed as i64, expected_dur
        ));
    } else {
        ui.label(format!(
            "Elapsed: {}s (expected ~{:.0}s)",
            elapsed as i64, expected_dur
        ));
    }

    let received = live_state.elevations_received.len();
    let expected = live_state.expected_elevation_count.unwrap_or(0);
    if expected > 0 {
        ui.label(format!("Elevations: {}/{} received", received, expected));
    } else if received > 0 {
        ui.label(format!("Elevations: {} received", received));
    }

    if past_now {
        ui.separator();
        ui.label(
            RichText::new("Projected area \u{2014} data not yet collected")
                .size(10.0)
                .italics()
                .color(Color32::from_rgba_unmultiplied(180, 200, 180, 160)),
        );
        if remaining > 0.0 {
            ui.label(format!("Est. ~{}s remaining", remaining as i64));
        }
    } else {
        ui.separator();
        ui.label(
            RichText::new(format!(
                "Live: {}/{} elevations received",
                received, expected
            ))
            .color(Color32::from_rgb(100, 200, 100)),
        );
    }
}

/// Render tooltip content when hovering over a scan block.
fn render_scan_tooltip_content(
    ui: &mut egui::Ui,
    scan: &crate::state::radar_data::Scan,
    live_state: &crate::state::LiveModeState,
    use_local: bool,
) {
    let vcp_label = if scan.vcp > 0 {
        format!("VCP {}", scan.vcp)
    } else {
        "Unknown VCP".to_string()
    };
    ui.label(
        RichText::new(format!("Volume Scan ({})", vcp_label))
            .strong()
            .size(12.0),
    );

    let mode_desc = match scan.vcp {
        215 | 212 => "Precipitation Mode",
        31 | 32 | 35 => "Clear Air Mode",
        12 | 121 => "Severe Weather Mode",
        _ if scan.vcp > 0 => "Known Mode",
        _ => "Unknown Mode",
    };
    let elev_count = scan
        .vcp_pattern
        .as_ref()
        .map(|v| v.elevations.len())
        .unwrap_or(scan.sweeps.len());
    let desc = if elev_count > 0 {
        format!(
            "A complete 360\u{00B0} survey at {} elevation angles. ({})",
            elev_count, mode_desc
        )
    } else {
        format!("A volume scan using {}.", mode_desc)
    };
    ui.label(RichText::new(desc).size(10.0).weak());
    ui.separator();

    let duration = scan.end_time - scan.start_time;
    let start_str = format_timestamp_full(scan.start_time, use_local);
    let end_str = format_timestamp_full(scan.end_time, use_local);
    ui.label(format!("Start: {}", start_str));
    ui.label(format!("End:   {} ({:.0}s)", end_str, duration));

    if elev_count > 0 {
        ui.label(format!("Elevations: {} sweeps", elev_count));
    }

    // Completeness
    let completeness_str = match scan.completeness {
        Some(ScanCompleteness::Complete) => "Complete",
        Some(ScanCompleteness::PartialWithVcp) => "Partial (VCP known)",
        Some(ScanCompleteness::PartialNoVcp) => "Partial (no VCP)",
        Some(ScanCompleteness::Missing) => "Missing",
        None => "Unknown",
    };
    if let (Some(present), Some(expected)) = (scan.present_records, scan.expected_records) {
        ui.label(format!(
            "Records: {}/{} ({})",
            present, expected, completeness_str
        ));
    } else {
        ui.label(format!("Status: {}", completeness_str));
    }

    // Live mode info if this scan matches the active volume
    if live_state.is_active() {
        if let Some(vol_start) = live_state.current_volume_start {
            if (scan.start_time - vol_start).abs() < 30.0 {
                ui.separator();
                let received = live_state.elevations_received.len();
                let expected = live_state.expected_elevation_count.unwrap_or(0);
                ui.label(
                    RichText::new(format!(
                        "Live: {}/{} elevations received",
                        received, expected
                    ))
                    .color(Color32::from_rgb(100, 200, 100)),
                );
            }
        }
    }
}
