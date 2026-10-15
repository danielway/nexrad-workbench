//! Per-frame live-stream measurements.
//!
//! [`LiveStatus`] is the stream half of the session: derived once per frame
//! in [`crate::subsystem::Live::refresh`] and fed into
//! [`crate::core::session::SessionVm::build`], which owns every word a
//! surface prints about the stream (the activity line, the now cap, the
//! chip's lag). Nothing renders from `LiveStatus` directly except timing
//! hints (the countdown repaint cadence).
//!
//! The stream's activity is orthogonal to where the playhead is: that half
//! (pinned to now or at a time, with its lag) is the session's playhead fact
//! ([`crate::core::session::Playhead`]).

use crate::core::live_mode::{ExpectedSweep, LiveModeState, LivePhase};
use crate::core::StreamActivity;

/// Frame-cached live-status snapshot: pure data, no egui types. Built by
/// [`derive_live_status`]; read from [`crate::subsystem::Live::frame_status`].
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct LiveStatus {
    pub activity: StreamActivity,
    /// Chunks received this session — the number that visibly ticks up.
    pub chunks_received: u32,
    /// Seconds until the next eligible sweep is expected in S3. `Some` only
    /// while [`StreamActivity::Waiting`] — a stalled stream has no honest
    /// countdown.
    pub countdown_secs: Option<f64>,
    /// Next eligible sweep for the active filter. Present while waiting or
    /// stalled so copy can name the expected cut.
    pub expected_sweep: Option<ExpectedSweep>,
    /// Wall-now minus the last radial's collection timestamp (PRODUCT.md
    /// §11.3 data age). `None` before the first radial or when idle.
    pub data_age_secs: Option<f64>,
    /// Seconds spent in the current phase — feeds the connecting elapsed
    /// readout.
    pub phase_elapsed_secs: f64,
}

impl Default for LiveStatus {
    fn default() -> Self {
        Self {
            activity: StreamActivity::Off,
            chunks_received: 0,
            countdown_secs: None,
            expected_sweep: None,
            data_age_secs: None,
            phase_elapsed_secs: 0.0,
        }
    }
}

/// Inputs [`derive_live_status`] reads. Kept explicit so the derivation has
/// no back-reference to the subsystem.
pub(crate) struct LiveStatusInputs<'a> {
    pub mode_state: &'a LiveModeState,
    /// From [`crate::subsystem::Live::countdown_remaining_secs`] (this frame's
    /// projection); already `None` outside `WaitingForChunk`.
    pub countdown_secs: Option<f64>,
    /// Next eligible sweep from this frame's projection, already cleared
    /// when the plan's filter does not match the current selection.
    pub expected_sweep: Option<ExpectedSweep>,
    pub now_secs: f64,
}

/// Derive this frame's [`LiveStatus`]. Pure.
pub(crate) fn derive_live_status(inputs: LiveStatusInputs<'_>) -> LiveStatus {
    let LiveStatusInputs {
        mode_state,
        countdown_secs,
        expected_sweep,
        now_secs,
    } = inputs;
    let activity = mode_state.stream_activity(now_secs, expected_sweep.as_ref());
    LiveStatus {
        activity,
        chunks_received: mode_state.chunks_received,
        // A stalled stream is past its cadence — advertising a countdown for a
        // chunk that is already overdue would be dishonest.
        countdown_secs: (activity == StreamActivity::Waiting)
            .then_some(countdown_secs)
            .flatten(),
        expected_sweep: (mode_state.phase == LivePhase::WaitingForChunk
            && matches!(activity, StreamActivity::Waiting | StreamActivity::Stalled))
        .then_some(expected_sweep)
        .flatten(),
        data_age_secs: if mode_state.is_active() {
            mode_state.last_radial_time_secs.map(|t| now_secs - t)
        } else {
            None
        },
        phase_elapsed_secs: mode_state.phase_elapsed_secs(now_secs),
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use crate::core::LivePhase;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// A state `now` seconds into the given phase.
    fn state_in_phase(phase: LivePhase, started_at: f64) -> LiveModeState {
        let mut s = LiveModeState::default();
        s.phase = phase;
        s.phase_started_at = Some(started_at);
        s
    }

    fn expected(
        elevation_number: Option<u8>,
        angle: Option<f32>,
        available_at_secs: f64,
    ) -> ExpectedSweep {
        ExpectedSweep {
            elevation_number,
            elevation_angle: angle,
            available_at_secs,
        }
    }

    fn inputs(mode_state: &LiveModeState) -> LiveStatusInputs<'_> {
        LiveStatusInputs {
            mode_state,
            countdown_secs: None,
            expected_sweep: None,
            now_secs: 0.0,
        }
    }

    // ── countdown ──

    #[wasm_bindgen_test]
    fn countdown_passes_through_only_while_waiting() {
        let s = state_in_phase(LivePhase::WaitingForChunk, 1000.0);
        let waiting = derive_live_status(LiveStatusInputs {
            countdown_secs: Some(12.3),
            now_secs: 1005.0,
            ..inputs(&s)
        });
        assert_eq!(waiting.activity, StreamActivity::Waiting);
        assert_eq!(waiting.countdown_secs, Some(12.3));

        // Receiving with a (stale) countdown input → suppressed.
        let s = state_in_phase(LivePhase::Streaming, 1000.0);
        let receiving = derive_live_status(LiveStatusInputs {
            countdown_secs: Some(12.3),
            now_secs: 1005.0,
            ..inputs(&s)
        });
        assert_eq!(receiving.countdown_secs, None);
    }

    #[wasm_bindgen_test]
    fn countdown_suppressed_once_stalled() {
        // Expected sweep 31s late → stalled; an overdue countdown is a lie.
        let s = state_in_phase(LivePhase::WaitingForChunk, 1000.0);
        let status = derive_live_status(LiveStatusInputs {
            countdown_secs: Some(5.0),
            expected_sweep: Some(expected(Some(1), Some(0.5), 1030.0)),
            now_secs: 1061.0,
            ..inputs(&s)
        });
        assert_eq!(status.activity, StreamActivity::Stalled);
        assert_eq!(status.countdown_secs, None);
    }

    #[wasm_bindgen_test]
    fn long_filtered_gap_stays_waiting_before_grace() {
        // 3 minutes into the phase, but the eligible sweep is still 40s out.
        let s = state_in_phase(LivePhase::WaitingForChunk, 1000.0);
        let status = derive_live_status(LiveStatusInputs {
            countdown_secs: Some(40.0),
            expected_sweep: Some(expected(Some(3), Some(2.4), 1220.0)),
            now_secs: 1180.0,
            ..inputs(&s)
        });
        assert_eq!(status.activity, StreamActivity::Waiting);
        assert_eq!(status.countdown_secs, Some(40.0));
    }

    #[wasm_bindgen_test]
    fn stall_starts_strictly_after_projected_completion_plus_grace() {
        let s = state_in_phase(LivePhase::WaitingForChunk, 1000.0);
        let exp = expected(Some(3), Some(2.4), 1100.0);
        let at_boundary = derive_live_status(LiveStatusInputs {
            expected_sweep: Some(exp),
            now_secs: 1130.0,
            ..inputs(&s)
        });
        assert_eq!(at_boundary.activity, StreamActivity::Waiting);

        let late = derive_live_status(LiveStatusInputs {
            expected_sweep: Some(exp),
            now_secs: 1130.1,
            ..inputs(&s)
        });
        assert_eq!(late.activity, StreamActivity::Stalled);
    }

    #[wasm_bindgen_test]
    fn filter_change_clears_stalled_when_expectation_is_dropped() {
        let s = state_in_phase(LivePhase::WaitingForChunk, 1000.0);
        let stalled = derive_live_status(LiveStatusInputs {
            expected_sweep: Some(expected(Some(3), Some(2.4), 1000.0)),
            now_secs: 1100.0,
            ..inputs(&s)
        });
        assert_eq!(stalled.activity, StreamActivity::Stalled);

        let after_filter = derive_live_status(LiveStatusInputs {
            expected_sweep: None,
            now_secs: 1100.0,
            ..inputs(&s)
        });
        assert_eq!(after_filter.activity, StreamActivity::Waiting);
    }

    #[wasm_bindgen_test]
    fn arrival_clears_stalled_when_phase_leaves_waiting() {
        let mut s = state_in_phase(LivePhase::WaitingForChunk, 1000.0);
        let stalled = derive_live_status(LiveStatusInputs {
            expected_sweep: Some(expected(Some(1), Some(0.5), 1000.0)),
            now_secs: 1100.0,
            ..inputs(&s)
        });
        assert_eq!(stalled.activity, StreamActivity::Stalled);

        s.phase = LivePhase::Streaming;
        s.phase_started_at = Some(1100.0);
        let receiving = derive_live_status(LiveStatusInputs {
            expected_sweep: Some(expected(Some(2), Some(1.5), 1120.0)),
            now_secs: 1100.0,
            ..inputs(&s)
        });
        assert_eq!(receiving.activity, StreamActivity::Receiving);
        assert_eq!(receiving.expected_sweep, None);
    }

    #[wasm_bindgen_test]
    fn stop_clears_stalled() {
        let s = LiveModeState::default();
        let off = derive_live_status(LiveStatusInputs {
            expected_sweep: Some(expected(Some(1), Some(0.5), 1000.0)),
            now_secs: 1100.0,
            ..inputs(&s)
        });
        assert_eq!(off.activity, StreamActivity::Off);
    }

    // ── data age ──

    #[wasm_bindgen_test]
    fn data_age_tracks_last_radial_only_while_active() {
        let mut s = state_in_phase(LivePhase::Streaming, 1000.0);
        s.last_radial_time_secs = Some(980.0);
        let status = derive_live_status(LiveStatusInputs {
            now_secs: 1010.0,
            ..inputs(&s)
        });
        assert_eq!(status.data_age_secs, Some(30.0));

        // Idle state with a leftover radial timestamp → None.
        let mut idle = LiveModeState::default();
        idle.last_radial_time_secs = Some(980.0);
        let off = derive_live_status(LiveStatusInputs {
            now_secs: 1010.0,
            ..inputs(&idle)
        });
        assert_eq!(off.data_age_secs, None);
    }

    #[wasm_bindgen_test]
    fn data_age_none_before_first_radial() {
        let s = state_in_phase(LivePhase::AcquiringLock, 1000.0);
        let status = derive_live_status(LiveStatusInputs {
            now_secs: 1002.0,
            ..inputs(&s)
        });
        assert_eq!(status.data_age_secs, None);
    }

    // ── default ──

    #[wasm_bindgen_test]
    fn default_is_off_and_silent() {
        let d = LiveStatus::default();
        assert_eq!(d.activity, StreamActivity::Off);
        assert_eq!(d.chunks_received, 0);
        assert_eq!(d.countdown_secs, None);
        assert_eq!(d.data_age_secs, None);
    }
}
