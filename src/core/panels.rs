//! Pure panel-derivation logic, lifted out of the chrome panels.
//!
//! The panels (left/right/top/bottom) should *render a view and emit intents*.
//! This module holds the read-only derivations and small decision predicates
//! they previously computed inline: the radar-state query the left panel shows,
//! the high-speed animation freeze rule, the archive sweep-azimuth, and the
//! top-bar status-message auto-dismiss/fade. All pure and unit-tested.
//!
//! Note: the broad `&mut`-state → intent migration for the interactive panels
//! (transport, layer toggles, camera) is intentionally *not* done here — see
//! `docs/CORE_SHELL_MIGRATION_LOG.md` (D6) for why that half is QA-gated.

use crate::state::radar_data::Scan;

// ---------------------------------------------------------------------------
// Small shared predicates / math
// ---------------------------------------------------------------------------

/// Whether animated radar state (azimuth, elevation, sweep indicator, progress)
/// should be frozen this frame: playback is advancing *and* faster than 30 s/s.
/// Above that the rotating indicators flash violently, so they're held still
/// while static VCP info keeps rendering. (Was an inline `>30.0` in the left
/// panel; the canvas sweep-line uses the same 30 s/s ceiling.)
pub fn animation_frozen(playing: bool, speed_secs_per_sec: f64) -> bool {
    playing && speed_secs_per_sec > 30.0
}

/// The archive sweep-line azimuth from linear progress through `[start, end]`,
/// or `None` for a non-positive-duration sweep. Mirrors the left panel's inline
/// `progress * 360 % 360` (note: the `f32` cast happens after the multiply, as
/// in the original).
pub fn archive_azimuth_from_progress(start: f64, end: f64, ts: f64) -> Option<f32> {
    let dur = end - start;
    if dur <= 0.0 {
        return None;
    }
    let progress = (ts - start) / dur;
    Some(((progress * 360.0) as f32) % 360.0)
}

/// Fade window for the transient top-bar status message (Idle/Archive).
const STATUS_FADE_START_MS: f64 = 8000.0;
/// Dismiss threshold for the status message.
const STATUS_DISMISS_MS: f64 = 10000.0;

/// What the top bar should do with the transient status message this frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StatusVisibility {
    /// Past the dismiss window — the shell should clear the message.
    Dismiss,
    /// Still showing at this `alpha`; `fading` ⇒ request a repaint to animate.
    Visible { alpha: u8, fading: bool },
}

/// Decide the status message's visibility given when it was set (`set_ms`, ms
/// since epoch; `<= 0` means "never set") and `now_ms`. Pure extraction of the
/// top-bar auto-dismiss/fade math.
pub fn status_message_visibility(set_ms: f64, now_ms: f64) -> StatusVisibility {
    let age_ms = now_ms - set_ms;
    if set_ms > 0.0 && age_ms >= STATUS_DISMISS_MS {
        return StatusVisibility::Dismiss;
    }
    let alpha = if set_ms <= 0.0 || age_ms < STATUS_FADE_START_MS {
        255u8
    } else {
        let t = 1.0 - (age_ms - STATUS_FADE_START_MS) / (STATUS_DISMISS_MS - STATUS_FADE_START_MS);
        (t.clamp(0.0, 1.0) * 255.0) as u8
    };
    let fading = (STATUS_FADE_START_MS..STATUS_DISMISS_MS).contains(&age_ms);
    StatusVisibility::Visible { alpha, fading }
}

// ---------------------------------------------------------------------------
// Left-panel radar-state derivation
// ---------------------------------------------------------------------------

/// State queried from the radar timeline at the current playback timestamp, for
/// the left panel's azimuth/elevation/VCP readouts. Read-only projection of
/// `Timeline` + `Live` + `Playback`; holds borrows tied to those subsystems.
pub struct RadarStateAtTimestamp<'a> {
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
    pub live_vcp_pattern: Option<&'a crate::data::keys::ExtractedVcp>,
    /// Unified position model with sweep timing (live or archived).
    pub position: Option<crate::nexrad::projection::ScanProjection>,
}

/// Derive the left panel's radar state at the current playback position.
///
/// Pure read over the subsystems (no egui, no mutation, no I/O). Moved verbatim
/// from `ui::left_panel`; the high-speed freeze and the archive azimuth now use
/// [`animation_frozen`] / [`archive_azimuth_from_progress`].
pub fn query_radar_state_at_timestamp<'a>(
    timeline: &'a crate::subsystem::Timeline,
    live: &'a crate::subsystem::Live,
    playback: &'a crate::subsystem::Playback,
) -> RadarStateAtTimestamp<'a> {
    let ts = playback.state.playback_position();

    // Resolve position detail through the same single adapter the timeline uses,
    // so the panel can't drift from it. The in-progress volume is excluded from
    // `settled_scan_at` and surfaced via `live_volume()` (with its cached cuts
    // merged in).
    let view = crate::state::TimelineView::build(
        &timeline.scans,
        &timeline.shadow_scan_boundaries,
        Some(&live.mode_state),
        live.radar_model.position.as_ref(),
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
                playback.state.playing,
                playback.state.speed.timeline_seconds_per_real_second(),
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
                position: Some(crate::nexrad::projection::scan_to_projection(scan)),
            }
        }
        None => {
            // In live mode, read the frame-snapshotted derivations from
            // LiveRadarModel rather than re-evaluating with a fresh
            // js_sys::Date::now() — that would drift against every other surface
            // that consumed the same model.
            if let Some(position) = view.live_volume() {
                let frame = &live.radar_model.frame_now;
                let vcp = Some(position.vcp_number).filter(|&v| v > 0);
                let azimuth = live.radar_model.estimated_azimuth;
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
                    live_vcp_pattern: live
                        .radar_model
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

    #[wasm_bindgen_test]
    fn status_visibility_full_fade_dismiss() {
        // Fresh message → fully opaque, not fading.
        assert_eq!(
            status_message_visibility(1000.0, 1000.0),
            StatusVisibility::Visible {
                alpha: 255,
                fading: false
            }
        );
        // Within the steady window (age 5s) → opaque, not fading.
        assert_eq!(
            status_message_visibility(1000.0, 6000.0),
            StatusVisibility::Visible {
                alpha: 255,
                fading: false
            }
        );
        // Mid-fade (age 9s, halfway through the 8..10s window) → ~half alpha, fading.
        match status_message_visibility(0.0 + 1000.0, 1000.0 + 9000.0) {
            StatusVisibility::Visible { alpha, fading } => {
                assert!(fading);
                assert!((120..=135).contains(&alpha), "alpha {alpha}");
            }
            other => panic!("expected Visible, got {other:?}"),
        }
        // Past dismiss (age 10s) → Dismiss.
        assert_eq!(
            status_message_visibility(1000.0, 11000.0),
            StatusVisibility::Dismiss
        );
        // Never-set sentinel (set_ms <= 0) → always opaque, never dismissed.
        assert_eq!(
            status_message_visibility(0.0, 999_999.0),
            StatusVisibility::Visible {
                alpha: 255,
                fading: false
            }
        );
    }
}
