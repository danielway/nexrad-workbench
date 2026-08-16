//! Per-frame live-status view-model.
//!
//! Every surface that talks about the live stream — the transport LIVE
//! button and activity chip, the timeline now-cap, the top-bar mode badge,
//! the mobile transport row — projects from one [`LiveStatus`] derived once
//! per frame in [`crate::subsystem::Live::refresh`]. Before this existed each
//! surface hand-rolled its own `match` on [`LivePhase`], so "is data actually
//! arriving?" read differently (or not at all) depending on where you looked.
//!
//! The two orthogonal facts (docs/PRODUCT.md §6):
//!
//! - [`StreamActivity`] — is the stream pulling data right now?
//! - [`LiveTether`] — is the playhead riding the live edge?
//!
//! A detached playhead over a healthy stream and a tethered one report the
//! same activity; a tethered playhead over a stalled stream must say so.

use crate::core::live_mode::{ExpectedSweep, LiveModeState, LivePhase};
use crate::core::{format_lag, StreamActivity};

/// The playhead's relationship to the live edge. Orthogonal to
/// [`StreamActivity`]: the tether says where the *user* is, the activity says
/// what the *stream* is doing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum LiveTether {
    /// No stream running.
    None,
    /// Playhead riding the live edge (pinned or lookback).
    Tethered,
    /// Stream running while the playhead browses elsewhere.
    Detached,
}

impl LiveTether {
    /// Whether a stream session exists at all (tethered or backgrounded).
    pub(crate) fn is_streaming(self) -> bool {
        !matches!(self, LiveTether::None)
    }
}

/// Frame-cached live-status snapshot: pure data, no egui types. Built by
/// [`derive_live_status`]; read from [`crate::subsystem::Live::frame_status`].
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct LiveStatus {
    pub activity: StreamActivity,
    pub tether: LiveTether,
    /// Chunks received this session — the number that visibly ticks up.
    pub chunks_received: u32,
    /// Seconds until the next eligible sweep is expected in S3. `Some` only
    /// while [`StreamActivity::Waiting`] — a stalled stream has no honest
    /// countdown.
    pub countdown_secs: Option<f64>,
    /// Next eligible sweep for the active filter. Present while waiting or
    /// stalled so copy can name the expected cut.
    pub expected_sweep: Option<ExpectedSweep>,
    /// Wall-now minus playhead — the "behind" readout. `Some` only while
    /// [`LiveTether::Detached`].
    pub lag_secs: Option<f64>,
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
            tether: LiveTether::None,
            chunks_received: 0,
            countdown_secs: None,
            expected_sweep: None,
            lag_secs: None,
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
    /// Whether the playhead rides the live edge (pinned or lookback).
    pub tethered: bool,
    pub playback_position_secs: f64,
    pub now_secs: f64,
}

/// Derive this frame's [`LiveStatus`]. Pure.
pub(crate) fn derive_live_status(inputs: LiveStatusInputs<'_>) -> LiveStatus {
    let LiveStatusInputs {
        mode_state,
        countdown_secs,
        expected_sweep,
        tethered,
        playback_position_secs,
        now_secs,
    } = inputs;
    let activity = mode_state.stream_activity(now_secs, expected_sweep.as_ref());
    let tether = if !mode_state.is_active() {
        LiveTether::None
    } else if tethered {
        LiveTether::Tethered
    } else {
        LiveTether::Detached
    };
    LiveStatus {
        activity,
        tether,
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
        lag_secs: (tether == LiveTether::Detached).then_some(now_secs - playback_position_secs),
        data_age_secs: if mode_state.is_active() {
            mode_state.last_radial_time_secs.map(|t| now_secs - t)
        } else {
            None
        },
        phase_elapsed_secs: mode_state.phase_elapsed_secs(now_secs),
    }
}

impl LiveStatus {
    /// Short trailing readout for the activity chip / top-bar badge:
    /// what the stream is doing, with the part that visibly moves (chunk
    /// count, countdown, stall duration). `None` when no stream is running.
    pub(crate) fn detail_text(&self) -> Option<String> {
        match self.activity {
            StreamActivity::Off => None,
            StreamActivity::Connecting => {
                Some(format!("connecting… {}s", self.phase_elapsed_secs as i64))
            }
            StreamActivity::Receiving => Some(format!("receiving · {}", self.chunks_received)),
            StreamActivity::Waiting => {
                let wait = self
                    .expected_sweep
                    .map(|e| format!("waiting for {}", e.label()))
                    .unwrap_or_else(|| "waiting for next sweep".to_string());
                Some(match self.countdown_secs {
                    Some(s) => format!("{wait} · expected in ~{}s", s.max(0.0).ceil() as i64),
                    None => wait,
                })
            }
            StreamActivity::Stalled => Some(match self.expected_sweep {
                Some(e) => format!("stalled — {} is late", e.label()),
                None => format!(
                    "stalled — no data for {}",
                    format_lag(self.phase_elapsed_secs)
                ),
            }),
        }
    }

    /// Hover/tooltip sentence for the current activity. `None` when off.
    pub(crate) fn hover_text(&self) -> Option<&'static str> {
        match self.activity {
            StreamActivity::Off => None,
            StreamActivity::Connecting => Some("Connecting to the live feed"),
            StreamActivity::Receiving => Some("Receiving live data now"),
            StreamActivity::Waiting => Some("Waiting for the next expected sweep"),
            StreamActivity::Stalled => Some(if self.expected_sweep.is_some() {
                "Expected sweep is late — change elevation or stop streaming"
            } else {
                "No data has arrived for a while"
            }),
        }
    }

    /// Compact countdown suffix for the timeline now-cap while attached
    /// ("· 12s"). `Some` only while waiting with a known next-chunk ETA, so
    /// the cap stays a two-word pill the rest of the time.
    pub(crate) fn cap_suffix(&self) -> Option<String> {
        self.countdown_secs
            .map(|s| format!("· {}s", s.max(0.0).ceil() as i64))
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
            tethered: false,
            playback_position_secs: 0.0,
            now_secs: 0.0,
        }
    }

    // ── tether truth table (mirrors Live::is_detached) ──

    #[wasm_bindgen_test]
    fn tether_none_when_stream_idle_even_if_playhead_pinned() {
        let s = LiveModeState::default();
        let status = derive_live_status(LiveStatusInputs {
            tethered: true,
            ..inputs(&s)
        });
        assert_eq!(status.tether, LiveTether::None);
        assert_eq!(status.activity, StreamActivity::Off);
        assert!(!status.tether.is_streaming());
    }

    #[wasm_bindgen_test]
    fn tether_tethered_when_active_and_playhead_live() {
        let s = state_in_phase(LivePhase::Streaming, 0.0);
        let status = derive_live_status(LiveStatusInputs {
            tethered: true,
            now_secs: 1.0,
            ..inputs(&s)
        });
        assert_eq!(status.tether, LiveTether::Tethered);
        assert!(status.tether.is_streaming());
    }

    #[wasm_bindgen_test]
    fn tether_detached_when_active_and_playhead_free() {
        let s = state_in_phase(LivePhase::Streaming, 0.0);
        let status = derive_live_status(LiveStatusInputs {
            tethered: false,
            now_secs: 1.0,
            ..inputs(&s)
        });
        assert_eq!(status.tether, LiveTether::Detached);
    }

    #[wasm_bindgen_test]
    fn error_phase_is_not_active_so_tether_is_none_but_reads_stalled() {
        // Error is inactive for the tether (matches is_active()) while the
        // activity still reports the stall so the failure is visible.
        let s = state_in_phase(LivePhase::Error, 0.0);
        let status = derive_live_status(LiveStatusInputs {
            tethered: true,
            now_secs: 1.0,
            ..inputs(&s)
        });
        assert_eq!(status.tether, LiveTether::None);
        assert_eq!(status.activity, StreamActivity::Stalled);
    }

    // ── lag ──

    #[wasm_bindgen_test]
    fn lag_only_when_detached() {
        let s = state_in_phase(LivePhase::Streaming, 0.0);
        let detached = derive_live_status(LiveStatusInputs {
            tethered: false,
            playback_position_secs: 900.0,
            now_secs: 1000.0,
            ..inputs(&s)
        });
        assert_eq!(detached.lag_secs, Some(100.0));

        let tethered = derive_live_status(LiveStatusInputs {
            tethered: true,
            playback_position_secs: 900.0,
            now_secs: 1000.0,
            ..inputs(&s)
        });
        assert_eq!(tethered.lag_secs, None);

        let idle = LiveModeState::default();
        let off = derive_live_status(LiveStatusInputs {
            playback_position_secs: 900.0,
            now_secs: 1000.0,
            ..inputs(&idle)
        });
        assert_eq!(off.lag_secs, None);
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
        assert_eq!(
            status.detail_text().as_deref(),
            Some("waiting for 2.4° · expected in ~40s")
        );
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
        assert_eq!(
            late.detail_text().as_deref(),
            Some("stalled — 2.4° is late")
        );
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
        assert_eq!(
            after_filter.detail_text().as_deref(),
            Some("waiting for next sweep")
        );
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
        assert_eq!(off.detail_text(), None);
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

    // ── detail_text arms ──

    #[wasm_bindgen_test]
    fn detail_text_none_when_off() {
        assert_eq!(LiveStatus::default().detail_text(), None);
    }

    #[wasm_bindgen_test]
    fn detail_text_connecting_shows_truncated_elapsed() {
        let s = state_in_phase(LivePhase::AcquiringLock, 100.0);
        let status = derive_live_status(LiveStatusInputs {
            now_secs: 105.9,
            ..inputs(&s)
        });
        assert_eq!(status.detail_text().as_deref(), Some("connecting… 5s"));
    }

    #[wasm_bindgen_test]
    fn detail_text_receiving_shows_chunk_count() {
        let mut s = state_in_phase(LivePhase::Streaming, 100.0);
        s.chunks_received = 12;
        let status = derive_live_status(LiveStatusInputs {
            now_secs: 101.0,
            ..inputs(&s)
        });
        assert_eq!(status.detail_text().as_deref(), Some("receiving · 12"));
    }

    #[wasm_bindgen_test]
    fn detail_text_waiting_shows_sweep_and_countdown_ceil() {
        let s = state_in_phase(LivePhase::WaitingForChunk, 100.0);
        let status = derive_live_status(LiveStatusInputs {
            countdown_secs: Some(39.2),
            expected_sweep: Some(expected(Some(3), Some(2.4), 141.0)),
            now_secs: 101.0,
            ..inputs(&s)
        });
        assert_eq!(
            status.detail_text().as_deref(),
            Some("waiting for 2.4° · expected in ~40s")
        );
    }

    #[wasm_bindgen_test]
    fn detail_text_waiting_without_eta_falls_back() {
        let s = state_in_phase(LivePhase::WaitingForChunk, 100.0);
        let status = derive_live_status(LiveStatusInputs {
            now_secs: 101.0,
            ..inputs(&s)
        });
        assert_eq!(
            status.detail_text().as_deref(),
            Some("waiting for next sweep")
        );
    }

    #[wasm_bindgen_test]
    fn detail_text_stalled_names_the_late_sweep() {
        let s = state_in_phase(LivePhase::WaitingForChunk, 1000.0);
        let status = derive_live_status(LiveStatusInputs {
            expected_sweep: Some(expected(Some(3), None, 1000.0)),
            now_secs: 1120.0,
            ..inputs(&s)
        });
        assert_eq!(
            status.detail_text().as_deref(),
            Some("stalled — el 3 is late")
        );
        assert_eq!(
            status.hover_text(),
            Some("Expected sweep is late — change elevation or stop streaming")
        );
    }

    #[wasm_bindgen_test]
    fn detail_text_connect_stall_keeps_generic_silence() {
        let s = state_in_phase(LivePhase::AcquiringLock, 1000.0);
        let status = derive_live_status(LiveStatusInputs {
            now_secs: 1120.0,
            ..inputs(&s)
        });
        assert_eq!(status.activity, StreamActivity::Stalled);
        assert_eq!(
            status.detail_text().as_deref(),
            Some("stalled — no data for 2:00")
        );
    }

    // ── cap_suffix ──

    #[wasm_bindgen_test]
    fn cap_suffix_only_while_waiting_with_eta() {
        let s = state_in_phase(LivePhase::WaitingForChunk, 100.0);
        let waiting = derive_live_status(LiveStatusInputs {
            countdown_secs: Some(11.4),
            now_secs: 101.0,
            ..inputs(&s)
        });
        assert_eq!(waiting.cap_suffix().as_deref(), Some("· 12s"));

        // No ETA → no suffix.
        let no_eta = derive_live_status(LiveStatusInputs {
            now_secs: 101.0,
            ..inputs(&s)
        });
        assert_eq!(no_eta.cap_suffix(), None);

        // Receiving → no suffix.
        let s = state_in_phase(LivePhase::Streaming, 100.0);
        let receiving = derive_live_status(LiveStatusInputs {
            countdown_secs: Some(11.4),
            now_secs: 101.0,
            ..inputs(&s)
        });
        assert_eq!(receiving.cap_suffix(), None);
    }

    #[wasm_bindgen_test]
    fn cap_suffix_clamps_negative_eta_to_zero() {
        let s = state_in_phase(LivePhase::WaitingForChunk, 100.0);
        let status = derive_live_status(LiveStatusInputs {
            countdown_secs: Some(-3.0),
            now_secs: 101.0,
            ..inputs(&s)
        });
        assert_eq!(status.cap_suffix().as_deref(), Some("· 0s"));
    }

    // ── hover_text ──

    #[wasm_bindgen_test]
    fn hover_text_present_for_every_active_activity() {
        for (phase, elapsed, expected_sweep) in [
            (LivePhase::AcquiringLock, 1.0, None),
            (LivePhase::Streaming, 1.0, None),
            (LivePhase::WaitingForChunk, 1.0, None),
            (
                LivePhase::WaitingForChunk,
                999.0,
                Some(expected(Some(1), Some(0.5), 1000.0)),
            ),
        ] {
            let s = state_in_phase(phase, 1000.0);
            let status = derive_live_status(LiveStatusInputs {
                expected_sweep,
                now_secs: 1000.0 + elapsed,
                ..inputs(&s)
            });
            assert!(
                status.hover_text().is_some(),
                "hover for {:?} after {elapsed}s",
                phase
            );
        }
        assert_eq!(LiveStatus::default().hover_text(), None);
    }

    // ── LiveTether::is_streaming ──

    #[wasm_bindgen_test]
    fn tether_is_streaming_only_when_a_session_exists() {
        assert!(!LiveTether::None.is_streaming());
        assert!(LiveTether::Tethered.is_streaming());
        assert!(LiveTether::Detached.is_streaming());
    }

    // ── default ──

    #[wasm_bindgen_test]
    fn default_is_off_untethered_and_silent() {
        let d = LiveStatus::default();
        assert_eq!(d.activity, StreamActivity::Off);
        assert_eq!(d.tether, LiveTether::None);
        assert_eq!(d.chunks_received, 0);
        assert_eq!(d.countdown_secs, None);
        assert_eq!(d.lag_secs, None);
        assert_eq!(d.data_age_secs, None);
        assert!(!d.tether.is_streaming());
        assert_eq!(d.cap_suffix(), None);
    }
}
