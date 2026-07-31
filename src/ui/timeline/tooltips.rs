//! Tooltip rendering for timeline elements: scans, sweeps, available
//! (not-yet-downloaded) regions, and realtime volumes.
//!
//! Every tooltip follows the same shape: a status header answering
//! "what is this and do I have it", the time/availability essentials,
//! then a separator and the expert detail (VCP, waveform, products)
//! in weak text.

use super::{format_timestamp_full, DateTimeComponents, TimelineFrame};
use crate::core::projection::SweepAvailability;
use crate::data::ScanCompleteness;
use crate::ui::colors::timeline as tl_colors;
use crate::ui::colors::ui as ui_colors;
use eframe::egui::{self, Color32, Pos2, Rect, RichText, Vec2};

/// Shared tooltip header: `<title> · <status>`, with the status word
/// tinted to match the visual language of the block being hovered.
fn tooltip_header(ui: &mut egui::Ui, title: &str, status: &str, status_color: Color32) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        ui.label(RichText::new(title).strong().size(12.0));
        ui.label(RichText::new("\u{00B7}").size(12.0).weak());
        ui.label(
            RichText::new(status)
                .strong()
                .size(12.0)
                .color(status_color),
        );
    });
}

/// `2026-06-10 21:36:05 - 21:42:11 (366s)` — date once, start and end
/// times, duration. The essentials line of scan/sweep tooltips.
/// (ASCII dash: the default egui font has no glyph for an arrow here.)
fn format_time_range(start: f64, end: f64, use_local: bool) -> String {
    let s = DateTimeComponents::from_timestamp(start.floor() as i64, use_local);
    let e = DateTimeComponents::from_timestamp(end.floor() as i64, use_local);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02} - {:02}:{:02}:{:02} ({:.0}s)",
        s.year,
        s.month,
        s.day,
        s.hour,
        s.minute,
        s.second,
        e.hour,
        e.minute,
        e.second,
        (end - start).max(0.0)
    )
}

/// Human description of a VCP for the expert-detail section.
fn vcp_detail_line(vcp: u16) -> String {
    let mode = match vcp {
        215 | 212 => "Precipitation Mode",
        31 | 32 | 35 => "Clear Air Mode",
        12 | 121 => "Severe Weather Mode",
        _ => "",
    };
    if vcp == 0 {
        "VCP unknown".to_string()
    } else if mode.is_empty() {
        format!("VCP {}", vcp)
    } else {
        format!("VCP {} \u{2014} {}", vcp, mode)
    }
}

/// Render hover tooltip for timeline elements.
///
/// Reads only the frame's view: cached ("settled") scans for the
/// non-live volumes, and the merged in-progress volume for the live one —
/// so the tooltip stays consistent with what the tracks draw (in particular,
/// a resumed volume's already-cached cuts read as "Complete").
pub(super) fn render_timeline_tooltip(
    ui: &mut egui::Ui,
    frame: &TimelineFrame<'_>,
    live: &crate::subsystem::Live,
    hover_ts: f64,
    hover_pos: Pos2,
) {
    let view = &frame.view;
    let (use_local, now_secs) = (frame.use_local, frame.now_secs);
    let live_state = &live.mode_state;
    // Frames-first: one main track. A hover inside the frame-cell inset band
    // (where the cells are painted) reads as hovering a frame cell → the sweep
    // tooltip; the thin margins around it read as the scan container → the scan
    // tooltip. Only meaningful in the Micro tier (cells exist there).
    let track = &frame.rects.scan;
    let cell_top = track.top() + super::style::CONTAINER_INSET_Y + super::style::CELL_INSET_Y;
    let cell_bot = track.bottom() - super::style::CONTAINER_INSET_Y - super::style::CELL_INSET_Y;
    let in_sweep_track = frame.tier == crate::core::TimelineTier::Micro
        && hover_pos.y >= cell_top
        && hover_pos.y <= cell_bot;

    // Find the cached (settled) scan at the hovered timestamp, using the
    // same clamped block extents the scan track draws — a sparse scan's
    // raw end_time can be seconds after its start while its block spans
    // the full VCP-projected width. The in-progress volume is excluded
    // here — it is handled by the realtime path below so its merged sweep
    // states (cached + collecting + projected) drive the tooltip rather
    // than a stale cached-scan snapshot.
    let scan = view
        .settled_scans_in_range(hover_ts - 600.0, hover_ts + 600.0)
        .find(|(s, clamped_end)| s.start_time <= hover_ts && hover_ts <= *clamped_end)
        .map(|(s, _)| s);

    // Check if hovering within the active real-time volume (including projected future)
    let live_position = view.live_volume();
    let in_active_volume = scan.is_none()
        && live_position.is_some_and(|p| hover_ts >= p.volume_start && hover_ts <= p.volume_end);

    // Countdown for the realtime tooltip — computed here (we have `live`) and
    // passed into the volume tooltip, which only carries `live_state`.
    let countdown = live.countdown_remaining_secs(now_secs);

    // If in sweep track, search for a cached sweep across settled scans (not
    // just the scan containing hover_ts). This handles edge cases where a
    // sweep's time range extends before its parent scan's start_time. The
    // live volume's sweeps are handled by the realtime path instead.
    let (sweep, sweep_parent_scan) = if in_sweep_track {
        let mut found = None;
        for (s, _) in view.settled_scans_in_range(hover_ts - 600.0, hover_ts + 600.0) {
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

    // Nothing cached/live under the cursor — check for an available
    // (in-archive, not downloaded) region on the scan track.
    let shadow = if scan.is_none() && sweep.is_none() && !in_active_volume && !in_sweep_track {
        view.shadow_boundaries()
            .iter()
            .find(|b| {
                (b.start as f64) <= hover_ts
                    && hover_ts <= (b.end as f64)
                    && !view.is_covered_by_cached(b.start)
            })
            .copied()
    } else {
        None
    };

    if scan.is_none() && sweep.is_none() && !in_active_volume && shadow.is_none() {
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
            if let Some(position) = live_position {
                render_realtime_volume_tooltip(
                    ui,
                    position,
                    countdown,
                    hover_ts,
                    now_secs,
                    in_sweep_track,
                    use_local,
                );
            }
        } else if let Some(scan) = scan {
            render_scan_tooltip_content(
                ui,
                scan,
                live_state,
                live.radar_model.volume.as_ref().map(|v| &v.roster),
                use_local,
            );
        } else if let Some(boundary) = shadow {
            render_available_tooltip_content(ui, &boundary, use_local);
        }
    });
}

/// Tooltip for an available (in cloud archive, not downloaded) region.
fn render_available_tooltip_content(
    ui: &mut egui::Ui,
    boundary: &crate::core::ScanBoundary,
    use_local: bool,
) {
    tooltip_header(
        ui,
        "Volume scan",
        "In cloud archive",
        tl_colors::status_available(),
    );

    // Boundary times come from S3 listing gaps, so they're approximate.
    let s = DateTimeComponents::from_timestamp(boundary.start, use_local);
    let e = DateTimeComponents::from_timestamp(boundary.end, use_local);
    ui.label(format!(
        "~{:04}-{:02}-{:02} {:02}:{:02} - {:02}:{:02}",
        s.year, s.month, s.day, s.hour, s.minute, e.hour, e.minute
    ));
    ui.label(
        RichText::new("Not downloaded \u{2014} click here and it loads automatically.")
            .size(10.0)
            .weak(),
    );
}

/// Render tooltip content when hovering over a sweep block.
fn render_sweep_tooltip_content(
    ui: &mut egui::Ui,
    sweep: &crate::core::Sweep,
    parent_scan: Option<&crate::core::Scan>,
    use_local: bool,
) {
    let display_angle = parent_scan
        .map(|s| s.display_angle(sweep))
        .unwrap_or(sweep.elevation);
    tooltip_header(
        ui,
        &format!(
            "Tilt {} \u{00B7} {:.1}\u{00B0}",
            sweep.elevation_number, display_angle
        ),
        "On device",
        tl_colors::status_cached(),
    );

    let sweep_count = parent_scan
        .and_then(|s| s.vcp_pattern.as_ref().map(|v| v.elevations.len()))
        .or_else(|| parent_scan.map(|s| s.sweeps.len()))
        .unwrap_or(0);

    ui.label(format_time_range(
        sweep.start_time,
        sweep.end_time,
        use_local,
    ));
    if sweep_count > 0 {
        ui.label(format!(
            "Cut {} of {} in this volume",
            sweep.elevation_number, sweep_count
        ));
    }

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

    // Expert detail: waveform and products from the VCP definition.
    if let Some(vcp) = parent_scan.and_then(|s| s.vcp_pattern.as_ref()) {
        if let Some(vcp_elev) = vcp
            .elevations
            .get(sweep.elevation_number.saturating_sub(1) as usize)
        {
            ui.separator();
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
            ui.label(
                RichText::new(format!("Waveform: {}", wf_label))
                    .size(10.0)
                    .weak(),
            );
            ui.label(
                RichText::new(format!("Products: {}", products))
                    .size(10.0)
                    .weak(),
            );

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
                ui.label(
                    RichText::new(format!("Flags: {}", flags.join(", ")))
                        .size(10.0)
                        .weak(),
                );
            }
        }
    }
}

/// Render tooltip for the in-progress realtime volume.
///
/// When hovering the sweep track, this identifies which realtime sweep block
/// is under the cursor and shows per-sweep details including chunk progress.
/// When hovering the scan track, it shows the volume-level summary.
fn render_realtime_volume_tooltip(
    ui: &mut egui::Ui,
    model: &crate::core::projection::ScanProjection,
    countdown: Option<f64>,
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
        let vcp_def = crate::data::vcp::get_vcp_definition(vcp_num);

        // Find which sweep block contains hover_ts (or snap to nearest).
        let mut hovered_sweep: Option<&crate::core::projection::SweepProjection> = None;
        let mut nearest_sweep: Option<&crate::core::projection::SweepProjection> = None;
        let mut nearest_dist: f64 = f64::MAX;

        for sp in &model.sweeps {
            if hover_ts >= sp.collection_start_secs && hover_ts <= sp.collection_end_secs {
                hovered_sweep = Some(sp);
                break;
            }
            let dist = if hover_ts < sp.collection_start_secs {
                sp.collection_start_secs - hover_ts
            } else {
                hover_ts - sp.collection_end_secs
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

            // One vocabulary per concept. "In archive" matches the scan
            // inspector's cell label and the scan-level "In cloud archive"
            // header; the bare word "Available" was a third phrasing for the
            // same state and read as a capability rather than a location.
            let (state_label, state_color) = match sp.availability() {
                SweepAvailability::Cached => ("On device", tl_colors::status_cached()),
                SweepAvailability::Collecting => ("Collecting now", ui_colors::ACTIVE),
                SweepAvailability::Available => ("In archive", tl_colors::status_available()),
                SweepAvailability::Projected => ("Projected", tl_colors::status_available()),
            };
            tooltip_header(
                ui,
                &format!(
                    "Tilt {} \u{00B7} {:.1}\u{00B0}",
                    elev_num, sp.elevation_angle
                ),
                state_label,
                state_color,
            );
            ui.label(
                RichText::new(format!(
                    "Cut {} of {} in this volume",
                    elev_num, expected_count
                ))
                .size(10.0)
                .weak(),
            );

            if sp.is_complete() {
                if sp.is_observed() {
                    let duration = sp.duration();
                    let start_str = format_timestamp_full(sp.collection_start_secs, use_local);
                    ui.label(format!("Time: {} ({:.0}s)", start_str, duration));
                }
                ui.label(
                    RichText::new("Data received and stored.")
                        .size(10.0)
                        .color(ui_colors::SUCCESS),
                );
            } else if sp.is_in_progress() {
                let completed_chunks = sp.chunks_received as usize;
                let in_progress_radials = model.in_progress_radials.unwrap_or(0);

                ui.label(format!("Radials: {}/360 collected", sp.radials_received));

                // One line of chunk progress — the per-chunk breakdown
                // belongs to the chunk slots drawn on the track itself.
                let is_last_partial = in_progress_radials > 0 && completed_chunks > 0;
                if is_last_partial {
                    let total = if sp.chunks_in_sweep > 0 {
                        format!(" of {}", sp.chunks_in_sweep)
                    } else {
                        String::new()
                    };
                    ui.label(
                        RichText::new(format!(
                            "Chunk {}{} collecting \u{2014} {} radials",
                            completed_chunks, total, in_progress_radials
                        ))
                        .size(10.0)
                        .color(ui_colors::ACTIVE),
                    );
                } else if completed_chunks > 0 {
                    ui.label(format!("{} chunks received", completed_chunks));
                }

                if let Some(remaining) = countdown {
                    ui.label(format!("Next data in ~{}s", remaining.ceil() as i32));
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
                    ui.label(
                        RichText::new(format!("Waveform: {}", wf_label))
                            .size(10.0)
                            .weak(),
                    );
                }
            }

            return;
        }
    }

    // -- Volume-level tooltip (scan track or no sweep match) --
    tooltip_header(ui, "Live volume", "Collecting now", ui_colors::ACTIVE);

    let start_str = format_timestamp_full(vol_start, use_local);
    ui.label(format!("Started: {}", start_str));
    let elapsed = (now - vol_start).floor();
    let remaining = (expected_end - now).ceil();
    ui.label(format!(
        "Elapsed: {}s / est. {:.0}s total",
        elapsed as i64, expected_dur
    ));

    let received = model.completed_count();
    if expected_count > 0 {
        ui.label(format!("{} of {} tilts received", received, expected_count));
    } else if received > 0 {
        ui.label(format!("{} tilts received", received));
    }
    if let Some(remaining_cd) = countdown {
        ui.label(format!("Next data in ~{}s", remaining_cd.ceil() as i32));
    }

    if past_now {
        ui.label(
            RichText::new("Projected area \u{2014} data not yet collected")
                .size(10.0)
                .italics()
                .color(Color32::from_rgba_unmultiplied(180, 200, 180, 160)),
        );
        if remaining > 0.0 {
            ui.label(format!("Est. ~{}s remaining", remaining as i64));
        }
    }

    ui.separator();
    ui.label(RichText::new(vcp_detail_line(vcp_num)).size(10.0).weak());
}

/// Render tooltip content when hovering over a scan block.
fn render_scan_tooltip_content(
    ui: &mut egui::Ui,
    scan: &crate::core::Scan,
    live_state: &crate::core::LiveModeState,
    live_roster: Option<&crate::core::VolumeElevationRoster>,
    use_local: bool,
) {
    let (status, status_color) = match scan.completeness {
        Some(ScanCompleteness::Missing) => ("In cloud archive", tl_colors::status_available()),
        Some(ScanCompleteness::PartialWithVcp | ScanCompleteness::PartialNoVcp) => {
            ("Partially on device", tl_colors::status_cached())
        }
        Some(ScanCompleteness::Complete) | None => ("On device", tl_colors::status_cached()),
    };
    tooltip_header(ui, "Volume scan", status, status_color);

    ui.label(format_time_range(scan.start_time, scan.end_time, use_local));

    let elev_count = scan
        .vcp_pattern
        .as_ref()
        .map(|v| v.elevations.len())
        .unwrap_or(scan.sweeps.len());
    if let (Some(present), Some(expected)) = (scan.cached_sweep_count, scan.planned_sweep_count) {
        ui.label(format!("{} of {} tilts on device", present, expected));
    } else if elev_count > 0 {
        ui.label(format!("{} tilts", elev_count));
    }
    if scan.completeness == Some(ScanCompleteness::Missing) {
        ui.label(
            RichText::new("Not downloaded \u{2014} click here and it loads automatically.")
                .size(10.0)
                .weak(),
        );
    }

    // Live mode info if this scan matches the active volume
    if live_state.is_active() {
        if let Some(vol_start) = live_state
            .current_volume
            .as_ref()
            .map(|a| a.best_start_secs())
        {
            if (scan.start_time - vol_start).abs() < 30.0 {
                let received = live_roster.map(|r| r.received.len()).unwrap_or(0);
                let expected = live_roster.and_then(|r| r.expected_count()).unwrap_or(0);
                ui.label(
                    RichText::new(format!("Live: {}/{} tilts received", received, expected))
                        .color(ui_colors::SUCCESS),
                );
            }
        }
    }

    // Expert detail: VCP identity and survey shape.
    ui.separator();
    ui.label(RichText::new(vcp_detail_line(scan.vcp)).size(10.0).weak());
    if elev_count > 0 {
        ui.label(
            RichText::new(format!(
                "A complete 360\u{00B0} survey at {} elevation angles.",
                elev_count
            ))
            .size(10.0)
            .weak(),
        );
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    // --- format_time_range ---
    // Uses use_local=false so DateTimeComponents goes through chrono's
    // deterministic Utc path (no js_sys::Date, no host timezone).

    #[wasm_bindgen_test]
    fn time_range_formats_date_once_with_duration() {
        // 1749591365 = 2025-06-10 21:36:05 UTC
        // 1749591731 = 2025-06-10 21:42:11 UTC, 366s apart.
        let s = format_time_range(1749591365.0, 1749591731.0, false);
        assert!(s == "2025-06-10 21:36:05 - 21:42:11 (366s)", "got {s}");
    }

    #[wasm_bindgen_test]
    fn time_range_floors_fractional_seconds() {
        // Fractional parts on start/end must be floored before conversion,
        // and the duration is rounded to whole seconds via {:.0}.
        let s = format_time_range(1749591365.9, 1749591731.4, false);
        assert!(s == "2025-06-10 21:36:05 - 21:42:11 (366s)", "got {s}");
    }

    #[wasm_bindgen_test]
    fn time_range_zero_duration_when_start_equals_end() {
        let s = format_time_range(1749591365.0, 1749591365.0, false);
        assert!(s == "2025-06-10 21:36:05 - 21:36:05 (0s)", "got {s}");
    }

    #[wasm_bindgen_test]
    fn time_range_clamps_negative_duration_to_zero() {
        // end < start: the displayed end time is still the (earlier) end,
        // but the duration is clamped to 0 by .max(0.0).
        let s = format_time_range(1749591731.0, 1749591365.0, false);
        assert!(s == "2025-06-10 21:42:11 - 21:36:05 (0s)", "got {s}");
    }

    #[wasm_bindgen_test]
    fn time_range_zero_pads_all_fields() {
        // 1704067205 = 2024-01-01 00:00:05 UTC
        // 1704067271 = 2024-01-01 00:01:11 UTC, 66s apart.
        let s = format_time_range(1704067205.0, 1704067271.0, false);
        assert!(s == "2024-01-01 00:00:05 - 00:01:11 (66s)", "got {s}");
    }

    // --- vcp_detail_line ---

    #[wasm_bindgen_test]
    fn vcp_zero_is_unknown() {
        assert!(vcp_detail_line(0) == "VCP unknown");
    }

    #[wasm_bindgen_test]
    fn vcp_precipitation_mode() {
        // 215 and 212 both map to Precipitation Mode (em-dash separator).
        assert!(vcp_detail_line(215) == "VCP 215 \u{2014} Precipitation Mode");
        assert!(vcp_detail_line(212) == "VCP 212 \u{2014} Precipitation Mode");
    }

    #[wasm_bindgen_test]
    fn vcp_clear_air_mode() {
        assert!(vcp_detail_line(31) == "VCP 31 \u{2014} Clear Air Mode");
        assert!(vcp_detail_line(32) == "VCP 32 \u{2014} Clear Air Mode");
        assert!(vcp_detail_line(35) == "VCP 35 \u{2014} Clear Air Mode");
    }

    #[wasm_bindgen_test]
    fn vcp_severe_weather_mode() {
        assert!(vcp_detail_line(12) == "VCP 12 \u{2014} Severe Weather Mode");
        assert!(vcp_detail_line(121) == "VCP 121 \u{2014} Severe Weather Mode");
    }

    #[wasm_bindgen_test]
    fn vcp_unknown_nonzero_has_no_mode_suffix() {
        // A non-zero VCP not in any table prints just "VCP <n>".
        assert!(vcp_detail_line(999) == "VCP 999");
        assert!(vcp_detail_line(7) == "VCP 7");
    }

    #[wasm_bindgen_test]
    fn vcp_detail_uses_em_dash_not_ascii_dash() {
        // Guard the separator glyph: must be U+2014, never an ASCII hyphen.
        let s = vcp_detail_line(215);
        assert!(s.contains('\u{2014}'));
        assert!(!s.contains(" - "));
    }

    #[wasm_bindgen_test]
    fn vcp_zero_takes_priority_over_empty_mode_branch() {
        // 0 has an empty mode, but the explicit vcp==0 check must win
        // so it never renders the bare "VCP 0" form.
        let s = vcp_detail_line(0);
        assert!(s == "VCP unknown");
        assert!(!s.contains('0'));
    }
}
