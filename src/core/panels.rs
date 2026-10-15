//! Pure panel-derivation logic, lifted out of the chrome panels.
//!
//! The panels (left/right/top/bottom) should *render a view and emit intents*.
//! This module holds the read-only derivations and small decision predicates
//! they previously computed inline: the radar-state query the left panel shows,
//! the high-speed animation freeze rule and the archive sweep-azimuth. All
//! pure and unit-tested.
//!
//! Note: the broad `&mut`-state → intent migration for the interactive panels
//! (transport, layer toggles, camera) is intentionally *not* done here — see
//! `docs/CORE_SHELL_MIGRATION_LOG.md` (D6) for why that half is QA-gated.

use crate::core::Scan;

// ---------------------------------------------------------------------------
// Small shared predicates / math
// ---------------------------------------------------------------------------

/// Whether animated radar state (azimuth, elevation, sweep indicator, progress)
/// should be frozen this frame: playback is advancing *and* faster than 30 s/s.
/// Above that the rotating indicators flash violently, so they're held still
/// while static VCP info keeps rendering. (Was an inline `>30.0` in the left
/// panel; the canvas sweep-line uses the same 30 s/s ceiling.)
pub(crate) fn animation_frozen(playing: bool, speed_secs_per_sec: f64) -> bool {
    playing && speed_secs_per_sec > 30.0
}

/// The archive sweep-line azimuth from linear progress through `[start, end]`,
/// or `None` for a non-positive-duration sweep. Mirrors the left panel's inline
/// `progress * 360 % 360` (note: the `f32` cast happens after the multiply, as
/// in the original).
pub(crate) fn archive_azimuth_from_progress(start: f64, end: f64, ts: f64) -> Option<f32> {
    let dur = end - start;
    if dur <= 0.0 {
        return None;
    }
    let progress = (ts - start) / dur;
    Some(((progress * 360.0) as f32) % 360.0)
}

// ---------------------------------------------------------------------------
// Left-panel radar-state derivation
// ---------------------------------------------------------------------------

/// State queried from the radar timeline at the current playback timestamp, for
/// the left panel's azimuth/elevation/VCP readouts. Read-only projection of
/// the timeline/live/playback state slices; holds borrows tied to them.
pub(crate) struct RadarStateAtTimestamp<'a> {
    /// Current azimuth angle in degrees (0-360), from actual radial data.
    pub azimuth: Option<f32>,
    /// Current elevation angle in degrees, from actual radial data.
    pub elevation: Option<f32>,
    /// Current VCP number.
    pub vcp: Option<u16>,
    /// Elevation number (1-based VCP cut ordinal) of the sweep currently on the
    /// canvas. The VCP-row highlight keys off this so it stays in sync with the
    /// displayed cut even when the scan is missing elevations.
    pub current_elevation_number: Option<u8>,
    /// Scan progress as a fraction (0.0-1.0).
    pub scan_progress: Option<f32>,
    /// Reference to the current scan (for the elevation list).
    pub scan: Option<&'a Scan>,
    /// Extracted VCP pattern from live streaming (used when `scan` is `None`).
    pub live_vcp_pattern: Option<&'a crate::data::ExtractedVcp>,
    /// Unified position model with sweep timing (live or archived).
    pub position: Option<crate::core::projection::ScanProjection>,
}

/// Derive the left panel's radar state at the current playback position.
///
/// Pure read over core state slices (no egui, no mutation, no I/O). Moved verbatim
/// from `ui::left_panel`; the high-speed freeze and the archive azimuth now use
/// [`animation_frozen`] / [`archive_azimuth_from_progress`].
pub(crate) fn query_radar_state_at_timestamp<'a>(
    scans: &'a crate::core::RadarTimeline,
    shadow_scan_boundaries: &'a [crate::core::ScanBoundary],
    live_mode: &'a crate::core::live_mode::LiveModeState,
    radar_model: &'a crate::core::live_radar_model::LiveRadarModel,
    playback: &'a crate::core::PlaybackState,
) -> RadarStateAtTimestamp<'a> {
    let ts = playback.playback_position();

    // Resolve position detail through the same single adapter the timeline uses,
    // so the panel can't drift from it. The in-progress volume is excluded from
    // `settled_scan_at` and surfaced via `live_volume()` (with its cached cuts
    // merged in).
    let view = crate::core::TimelineView::build(
        scans,
        shadow_scan_boundaries,
        Some(live_mode),
        radar_model.position.as_ref(),
    );

    match view.settled_scan_at(ts) {
        Some(scan) => {
            // Time-window match: drives the rotating-azimuth animation, only
            // meaningful while the cursor is inside a sweep's [start, end].
            let sweep_at_ts = scan.find_sweep_at_timestamp(ts);
            // Highlight match: in a gap between sweeps, show the most-recently
            // completed sweep. Sweeps are stored in elevation order, not time
            // order (SAILS-style VCPs revisit the lowest cut), so pick by max
            // end_time rather than Vec position.
            let sweep_for_highlight = sweep_at_ts.or_else(|| {
                scan.sweeps
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| s.end_time <= ts)
                    .max_by(|(_, a), (_, b)| {
                        a.end_time
                            .partial_cmp(&b.end_time)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
            });

            // Freeze animated state at high playback speeds; static VCP info
            // (number, name, elevation list) still renders.
            let is_fast = animation_frozen(
                playback.playing,
                playback.speed.timeline_seconds_per_real_second(),
            );

            let azimuth = if is_fast {
                None
            } else {
                sweep_at_ts.and_then(|(_, sweep)| {
                    archive_azimuth_from_progress(sweep.start_time, sweep.end_time, ts)
                })
            };
            let elevation = if is_fast {
                None
            } else {
                sweep_for_highlight.map(|(_, s)| scan.display_angle(s))
            };
            let current_elevation_number = if is_fast {
                None
            } else {
                sweep_for_highlight.map(|(_, s)| s.elevation_number)
            };
            let scan_progress = if is_fast {
                None
            } else {
                scan.progress_at_timestamp(ts)
            };

            RadarStateAtTimestamp {
                azimuth,
                elevation,
                vcp: Some(scan.vcp),
                current_elevation_number,
                scan_progress,
                scan: Some(scan),
                live_vcp_pattern: None,
                position: Some(crate::core::projection::scan_to_projection(scan)),
            }
        }
        None => {
            // In live mode, read the frame-snapshotted derivations from
            // LiveRadarModel rather than re-evaluating with a fresh
            // js_sys::Date::now() — that would drift against every other surface
            // that consumed the same model.
            if let Some(position) = view.live_volume() {
                let frame = &radar_model.frame_now;
                let vcp = Some(position.vcp_number).filter(|&v| v > 0);
                let azimuth = radar_model.estimated_azimuth;
                let sweep_index = frame.sweep_index.or_else(|| {
                    position
                        .in_progress_elevation
                        .map(|e| e.saturating_sub(1) as usize)
                });
                let scan_progress = frame.progress;
                let elevation = frame.elevation_angle.or_else(|| {
                    sweep_index.and_then(|idx| position.sweeps.get(idx).map(|s| s.elevation_angle))
                });
                let current_elevation_number = sweep_index
                    .and_then(|idx| position.sweeps.get(idx).map(|s| s.elevation_number))
                    .or(position.in_progress_elevation);

                RadarStateAtTimestamp {
                    azimuth,
                    elevation,
                    vcp,
                    current_elevation_number,
                    scan_progress,
                    scan: None,
                    live_vcp_pattern: radar_model
                        .volume
                        .as_ref()
                        .and_then(|v| v.vcp_pattern.as_ref()),
                    position: Some(position.clone()),
                }
            } else {
                RadarStateAtTimestamp {
                    azimuth: None,
                    elevation: None,
                    vcp: None,
                    current_elevation_number: None,
                    scan_progress: None,
                    scan: None,
                    live_vcp_pattern: None,
                    position: None,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn animation_freeze_gate() {
        assert!(animation_frozen(true, 31.0));
        assert!(!animation_frozen(true, 30.0)); // not strictly greater
        assert!(!animation_frozen(false, 100.0)); // paused never freezes
        assert!(!animation_frozen(true, 1.0));
    }

    #[wasm_bindgen_test]
    fn archive_azimuth_basic_and_degenerate() {
        // Halfway through a 0..10 sweep → 180°.
        assert_eq!(archive_azimuth_from_progress(0.0, 10.0, 5.0), Some(180.0));
        // Start → 0°.
        assert_eq!(archive_azimuth_from_progress(0.0, 10.0, 0.0), Some(0.0));
        // Zero / negative duration → None.
        assert_eq!(archive_azimuth_from_progress(10.0, 10.0, 10.0), None);
        assert_eq!(archive_azimuth_from_progress(10.0, 5.0, 7.0), None);
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    // --- animation_frozen: boundary just above the 30 s/s ceiling, negatives ---

    #[wasm_bindgen_test]
    fn animation_frozen_just_above_ceiling() {
        // Strictly greater than 30.0 freezes, even by an epsilon.
        assert!(animation_frozen(true, 30.000001));
        // Paused never freezes regardless of speed.
        assert!(!animation_frozen(false, 30.000001));
        // Negative speed (reverse) is below the ceiling → not frozen.
        assert!(!animation_frozen(true, -100.0));
        // Exactly 30.0 is not strictly greater → not frozen.
        assert!(!animation_frozen(true, 30.0));
    }

    // --- archive_azimuth_from_progress: the `% 360.0` wrap and negative paths ---

    #[wasm_bindgen_test]
    fn archive_azimuth_wraps_past_full_revolution() {
        // ts == end → progress 1.0 → 360.0 as f32 % 360.0 == 0.0 (wrap to zero).
        match archive_azimuth_from_progress(0.0, 10.0, 10.0) {
            Some(a) => assert!((a - 0.0).abs() < 1e-3, "az {a}"),
            None => panic!("expected Some"),
        }
        // ts well past end → progress 2.0 → 720.0 % 360.0 == 0.0.
        match archive_azimuth_from_progress(0.0, 10.0, 20.0) {
            Some(a) => assert!((a - 0.0).abs() < 1e-3, "az {a}"),
            None => panic!("expected Some"),
        }
        // progress 1.5 → 540.0 % 360.0 == 180.0.
        match archive_azimuth_from_progress(0.0, 10.0, 15.0) {
            Some(a) => assert!((a - 180.0).abs() < 1e-3, "az {a}"),
            None => panic!("expected Some"),
        }
        // progress 1.25 → 450.0 % 360.0 == 90.0.
        match archive_azimuth_from_progress(0.0, 10.0, 12.5) {
            Some(a) => assert!((a - 90.0).abs() < 1e-3, "az {a}"),
            None => panic!("expected Some"),
        }
    }

    #[wasm_bindgen_test]
    fn archive_azimuth_negative_progress_keeps_sign() {
        // ts before start → progress -0.5 → (-180.0) % 360.0 == -180.0
        // (f32 remainder takes the sign of the dividend).
        match archive_azimuth_from_progress(0.0, 10.0, -5.0) {
            Some(a) => assert!((a - (-180.0)).abs() < 1e-3, "az {a}"),
            None => panic!("expected Some"),
        }
    }

    #[wasm_bindgen_test]
    fn archive_azimuth_nonzero_start_offset() {
        // start=100, end=200, ts=150 → progress 0.5 → 180.0.
        match archive_azimuth_from_progress(100.0, 200.0, 150.0) {
            Some(a) => assert!((a - 180.0).abs() < 1e-3, "az {a}"),
            None => panic!("expected Some"),
        }
        // start=100, end=200, ts=125 → progress 0.25 → 90.0.
        match archive_azimuth_from_progress(100.0, 200.0, 125.0) {
            Some(a) => assert!((a - 90.0).abs() < 1e-3, "az {a}"),
            None => panic!("expected Some"),
        }
    }
}
