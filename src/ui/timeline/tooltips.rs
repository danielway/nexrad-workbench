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
    state: &crate::state::AppState,
    hover_ts: f64,
    hover_pos: Pos2,
    scan_rect: &Rect,
    sweep_rect: &Rect,
    detail_level: DetailLevel,
    use_local: bool,
    now_secs: f64,
) {
    let live_state = &state.live_mode_state;
    let in_sweep_track = detail_level == DetailLevel::Sweeps && hover_pos.y > sweep_rect.top();

    // Find the scan at the hovered timestamp
    let scan = timeline
        .scans_in_range(hover_ts - 0.5, hover_ts + 0.5)
        .find(|s| s.start_time <= hover_ts && s.end_time >= hover_ts);

    // Check if hovering within the active real-time volume (including projected future)
    let live_model = &state.live_radar_model;
    let in_active_volume = scan.is_none()
        && live_model.active
        && live_model
            .position
            .as_ref()
            .is_some_and(|p| hover_ts >= p.volume_start && hover_ts <= p.volume_end);

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
            if let Some(ref position) = live_model.position {
                render_realtime_volume_tooltip(
                    ui,
                    position,
                    live_state,
                    hover_ts,
                    now_secs,
                    in_sweep_track,
                    use_local,
                );
            }
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
    model: &crate::state::VcpPositionModel,
    live_state: &crate::state::LiveModeState,
    hover_ts: f64,
    now_secs: f64,
    in_sweep_track: bool,
    use_local: bool,
) {
    let vol_start = model.volume_start;
    let expected_dur = model.volume_end - vol_start;
    let expected_end = model.volume_end;
    let now = now_secs;
    let past_now = hover_ts > now;
    let vcp_num = model.vcp_number;
    let expected_count = model.sweeps.len();

    // -- Per-sweep tooltip when hovering the sweep track --
    if in_sweep_track && expected_count > 0 {
        let vcp_def = crate::state::get_vcp_definition(vcp_num);

        // Find which sweep block contains hover_ts (or snap to nearest).
        let mut hovered_sweep: Option<&crate::state::SweepPosition> = None;
        let mut nearest_sweep: Option<&crate::state::SweepPosition> = None;
        let mut nearest_dist: f64 = f64::MAX;

        for sp in &model.sweeps {
            if hover_ts >= sp.start && hover_ts <= sp.end {
                hovered_sweep = Some(sp);
                break;
            }
            let dist = if hover_ts < sp.start {
                sp.start - hover_ts
            } else {
                hover_ts - sp.end
            };
            if nearest_sweep.is_none() || dist < nearest_dist {
                nearest_sweep = Some(sp);
                nearest_dist = dist;
            }
        }

        // Snap to nearest sweep if hover_ts missed due to frame-to-frame drift.
        if hovered_sweep.is_none()
            && nearest_dist < (expected_dur / expected_count.max(1) as f64) * 0.5
        {
            hovered_sweep = nearest_sweep;
        }

        if let Some(sp) = hovered_sweep {
            let elev_num = sp.elevation_number;

            let state_label = if sp.is_complete() {
                "Complete"
            } else if sp.is_in_progress() {
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
                    sp.elevation_angle, elev_num, expected_count
                ))
                .size(10.0)
                .weak(),
            );
            ui.separator();

            if sp.is_complete() {
                if sp.is_observed() {
                    let duration = sp.duration();
                    let start_str = format_timestamp_full(sp.start, use_local);
                    ui.label(format!("Time: {} ({:.0}s)", start_str, duration));
                }
                ui.label(
                    RichText::new("Data received and stored.")
                        .size(10.0)
                        .color(Color32::from_rgb(100, 200, 100)),
                );
            } else if sp.is_in_progress() {
                let (total_radials, completed_chunks) = match &sp.status {
                    crate::state::SweepStatus::InProgress {
                        radials_received,
                        chunks_received,
                        ..
                    } => (*radials_received, *chunks_received as usize),
                    _ => (0, 0),
                };
                let in_progress_radials = live_state.current_in_progress_radials.unwrap_or(0);

                ui.label(format!("Radials: {}/360 collected", total_radials));

                let total_volume_chunks = live_state.chunks_received;

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
                    for (i, chunk) in sp.chunks.iter().enumerate() {
                        let chunk_num = i + 1;
                        let label = format!(
                            "  Chunk {}/{}: {} radials, collected",
                            chunk_num, display_total, chunk.radial_count
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

                let countdown = live_state.countdown_remaining_secs(now);
                if let Some(remaining) = countdown {
                    ui.label(format!("Next chunk in ~{}s", remaining.ceil() as i32));
                }
            } else {
                let duration = sp.duration();
                ui.label(format!("Est. duration: ~{:.0}s", duration));
                ui.label(
                    RichText::new("Not yet started \u{2014} bounds are estimated.")
                        .size(10.0)
                        .italics()
                        .color(Color32::from_rgba_unmultiplied(180, 200, 220, 160)),
                );
            }

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

    let received = model.completed_count();
    let expected = model.sweeps.len();
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
