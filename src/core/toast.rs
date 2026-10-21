//! The toast surface (one-instrument §10): one transient line under the top
//! bar for refusals, confirmations and failures that have no other home.
//!
//! "Range too long", "nothing to inspect here", "forecast copied", a decode
//! worker failure. Stream health and download failures are not toasts — they
//! belong to the activity chip — and a frame/playhead disagreement is the
//! canvas caption's job.
//!
//! The queue is core-owned and clock-injected: the shell pushes text, calls
//! [`ToastQueue::tick`] once per frame with the frame's wall clock, and draws
//! whatever [`ToastQueue::view`] returns. Every timing rule (four seconds on
//! screen, the fade at each end, repeats restarting the clock, the bounded
//! backlog) lives here and is tested headlessly.

use std::collections::VecDeque;

use crate::core::{AppError, ErrorContext};

/// How long one toast stays on screen, fades included (§10: four seconds).
pub(crate) const SHOW_MS: f64 = 4_000.0;
/// Fade-in at the start of [`SHOW_MS`].
pub(crate) const FADE_IN_MS: f64 = 150.0;
/// Fade-out at the end of [`SHOW_MS`].
pub(crate) const FADE_OUT_MS: f64 = 400.0;
/// Toasts waiting behind the one on screen. Past this the oldest waiting one
/// is dropped: a burst of refusals should not keep the surface busy for half a
/// minute.
pub(crate) const MAX_PENDING: usize = 3;

/// What a toast reports. The shell marks a failure with the ⚠ glyph in text
/// color — never red, which means *now* only (§3).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ToastKind {
    /// A refusal or a confirmation: "Selection too long", "Forecast copied".
    Notice,
    /// Something failed and nothing else on screen says so.
    Failure,
}

#[derive(Clone, PartialEq, Debug)]
struct Toast {
    kind: ToastKind,
    text: String,
    /// When it first reached the screen; `None` until the next tick shows it.
    shown_at_ms: Option<f64>,
}

/// The queue behind the toast surface. Lives on `AppState::toasts`.
#[derive(Default, Debug)]
pub(crate) struct ToastQueue {
    current: Option<Toast>,
    pending: VecDeque<Toast>,
    /// [`ErrorContext::pushed_count`] already turned into toasts.
    errors_seen: u64,
}

/// What the shell draws this frame.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct ToastVm {
    pub kind: ToastKind,
    pub text: String,
    /// 0..=1, the fade at each end.
    pub opacity: f32,
    /// Until the toast leaves, so the shell can schedule its repaint.
    pub remaining_ms: f64,
}

impl ToastQueue {
    /// Queue a refusal or confirmation.
    pub(crate) fn notice(&mut self, text: impl Into<String>) {
        self.push(ToastKind::Notice, text.into());
    }

    /// Queue a failure that has no other surface.
    pub(crate) fn failure(&mut self, text: impl Into<String>) {
        self.push(ToastKind::Failure, text.into());
    }

    /// Queue a toast. Empty text is ignored. Repeating the toast on screen
    /// restarts its clock rather than queuing a copy; repeating one already
    /// waiting is a no-op.
    pub(crate) fn push(&mut self, kind: ToastKind, text: String) {
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        if let Some(cur) = &mut self.current {
            if cur.kind == kind && cur.text == text {
                cur.shown_at_ms = None;
                return;
            }
        }
        if self
            .pending
            .iter()
            .any(|t| t.kind == kind && t.text == text)
        {
            return;
        }
        self.pending.push_back(Toast {
            kind,
            text,
            shown_at_ms: None,
        });
        while self.pending.len() > MAX_PENDING {
            self.pending.pop_front();
        }
    }

    /// Advance the queue to `now_ms`: retire the toast whose time is up, bring
    /// on the next, and start its clock.
    pub(crate) fn tick(&mut self, now_ms: f64) {
        if let Some(Toast {
            shown_at_ms: Some(at),
            ..
        }) = &self.current
        {
            if now_ms - at >= SHOW_MS {
                self.current = None;
            }
        }
        if self.current.is_none() {
            self.current = self.pending.pop_front();
        }
        if let Some(cur) = &mut self.current {
            if cur.shown_at_ms.is_none() {
                cur.shown_at_ms = Some(now_ms);
            }
        }
    }

    /// The user clicked the toast away. The next one shows on the next tick.
    pub(crate) fn dismiss(&mut self) {
        self.current = None;
    }

    /// The toast on screen at `now_ms`, or `None`.
    pub(crate) fn view(&self, now_ms: f64) -> Option<ToastVm> {
        let cur = self.current.as_ref()?;
        let at = cur.shown_at_ms?;
        let elapsed = (now_ms - at).max(0.0);
        let remaining = SHOW_MS - elapsed;
        if remaining <= 0.0 {
            return None;
        }
        let fade_in = (elapsed / FADE_IN_MS).min(1.0);
        let fade_out = (remaining / FADE_OUT_MS).min(1.0);
        Some(ToastVm {
            kind: cur.kind,
            text: cur.text.clone(),
            opacity: (fade_in * fade_out).clamp(0.0, 1.0) as f32,
            remaining_ms: remaining,
        })
    }

    /// Move the legacy `AppState::status_message` write port into the queue.
    ///
    /// A few surfaces still assign that string directly; the top bar no
    /// longer prints it, so each frame whatever was written becomes a notice
    /// and the port is cleared.
    pub(crate) fn absorb_status(&mut self, status: &mut String) {
        if !status.is_empty() {
            self.notice(std::mem::take(status));
        }
    }

    /// Turn errors pushed since the last call into failure toasts. Download
    /// failures are skipped: the activity chip counts them and offers retry.
    pub(crate) fn absorb_errors(&mut self, errors: &ErrorContext) {
        let pushed = errors.pushed_count();
        let fresh = pushed.saturating_sub(self.errors_seen) as usize;
        self.errors_seen = pushed;
        let fresh = fresh.min(errors.len());
        let skip = errors.len() - fresh;
        let texts: Vec<String> = errors
            .iter()
            .skip(skip)
            .filter_map(|e| error_text(&e.error))
            .collect();
        for text in texts {
            self.failure(text);
        }
    }
}

/// The words for an error (its toast, and its line in the activity sheet),
/// or `None` for a download failure, which the activity chip's failure
/// count owns.
pub(crate) fn error_text(error: &AppError) -> Option<String> {
    let source = match error {
        AppError::Download { .. } => return None,
        AppError::Worker { .. } => "Decode worker",
        AppError::Alerts { .. } => "NWS alerts",
        AppError::Mping { .. } => "mPING reports",
        AppError::Other { .. } => "Error",
    };
    Some(format!("{source}: {}", error.message()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::WorkerErrorKind;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn shown(q: &ToastQueue, now: f64) -> Option<String> {
        q.view(now).map(|v| v.text)
    }

    #[wasm_bindgen_test]
    fn a_toast_shows_on_the_next_tick_for_four_seconds() {
        let mut q = ToastQueue::default();
        q.notice("Selection too long");
        assert_eq!(shown(&q, 0.0), None, "nothing until the tick starts it");
        q.tick(1_000.0);
        assert_eq!(shown(&q, 1_000.0).as_deref(), Some("Selection too long"));
        assert_eq!(
            shown(&q, 1_000.0 + SHOW_MS - 1.0).as_deref(),
            Some("Selection too long")
        );
        assert_eq!(shown(&q, 1_000.0 + SHOW_MS), None);
        q.tick(1_000.0 + SHOW_MS);
        assert_eq!(shown(&q, 1_000.0 + SHOW_MS + 1.0), None);
    }

    #[wasm_bindgen_test]
    fn opacity_fades_in_and_out_and_is_full_between() {
        let mut q = ToastQueue::default();
        q.notice("x");
        q.tick(0.0);
        assert_eq!(q.view(0.0).unwrap().opacity, 0.0);
        let mid_in = q.view(FADE_IN_MS / 2.0).unwrap().opacity;
        assert!((mid_in - 0.5).abs() < 1e-3);
        assert_eq!(q.view(SHOW_MS / 2.0).unwrap().opacity, 1.0);
        let mid_out = q.view(SHOW_MS - FADE_OUT_MS / 2.0).unwrap().opacity;
        assert!((mid_out - 0.5).abs() < 1e-3);
        assert_eq!(q.view(SHOW_MS / 2.0).unwrap().remaining_ms, SHOW_MS / 2.0);
    }

    #[wasm_bindgen_test]
    fn toasts_queue_one_after_another() {
        let mut q = ToastQueue::default();
        q.notice("first");
        q.failure("second");
        q.tick(0.0);
        assert_eq!(shown(&q, 10.0).as_deref(), Some("first"));
        q.tick(SHOW_MS);
        let v = q.view(SHOW_MS + 10.0).unwrap();
        assert_eq!(v.text, "second");
        assert_eq!(v.kind, ToastKind::Failure);
    }

    #[wasm_bindgen_test]
    fn repeating_the_toast_on_screen_restarts_its_clock() {
        let mut q = ToastQueue::default();
        q.notice("No scan to inspect");
        q.tick(0.0);
        q.notice("No scan to inspect");
        q.tick(3_000.0);
        assert_eq!(
            shown(&q, 3_000.0 + SHOW_MS - 1.0).as_deref(),
            Some("No scan to inspect"),
            "the repeat runs a fresh four seconds from its tick"
        );
        assert!(q.pending.is_empty(), "no copy queued behind it");
    }

    #[wasm_bindgen_test]
    fn a_repeat_already_waiting_is_not_queued_twice() {
        let mut q = ToastQueue::default();
        q.notice("a");
        q.tick(0.0);
        q.notice("b");
        q.notice("b");
        assert_eq!(q.pending.len(), 1);
        // Same words, different kind, is a different toast.
        q.failure("b");
        assert_eq!(q.pending.len(), 2);
    }

    #[wasm_bindgen_test]
    fn the_backlog_is_bounded_by_dropping_the_oldest_waiting() {
        let mut q = ToastQueue::default();
        q.notice("on screen");
        q.tick(0.0);
        for i in 0..(MAX_PENDING + 2) {
            q.notice(format!("n{i}"));
        }
        assert_eq!(q.pending.len(), MAX_PENDING);
        assert_eq!(q.pending.front().unwrap().text, "n2");
        assert_eq!(shown(&q, 1.0).as_deref(), Some("on screen"));
    }

    #[wasm_bindgen_test]
    fn empty_text_is_ignored_and_text_is_trimmed() {
        let mut q = ToastQueue::default();
        q.notice("   ");
        q.tick(0.0);
        assert_eq!(q.view(0.0), None);
        q.notice("  copied \n");
        q.tick(1.0);
        assert_eq!(shown(&q, 2.0).as_deref(), Some("copied"));
    }

    #[wasm_bindgen_test]
    fn dismiss_brings_on_the_next_toast() {
        let mut q = ToastQueue::default();
        q.notice("a");
        q.notice("b");
        q.tick(0.0);
        q.dismiss();
        assert_eq!(q.view(1.0), None);
        q.tick(5.0);
        assert_eq!(shown(&q, 5.0).as_deref(), Some("b"));
    }

    #[wasm_bindgen_test]
    fn the_legacy_status_port_becomes_a_notice_and_clears() {
        let mut q = ToastQueue::default();
        let mut status = "No scan to inspect at that time".to_string();
        q.absorb_status(&mut status);
        assert!(status.is_empty());
        q.tick(0.0);
        let v = q.view(0.0).unwrap();
        assert_eq!(v.kind, ToastKind::Notice);
        assert_eq!(v.text, "No scan to inspect at that time");
        // An empty port adds nothing.
        q.absorb_status(&mut status);
        assert!(q.pending.is_empty());
    }

    #[wasm_bindgen_test]
    fn new_errors_become_failures_once_and_downloads_are_left_to_the_chip() {
        let mut errors = ErrorContext::default();
        let mut q = ToastQueue::default();
        errors.push(AppError::Worker {
            kind: WorkerErrorKind::IdbFailure,
            message: "IDB write failed".into(),
            scan_timestamp_secs: None,
        });
        errors.push(AppError::Download {
            message: "404".into(),
            scan_start_secs: None,
        });
        q.absorb_errors(&errors);
        q.tick(0.0);
        let v = q.view(0.0).unwrap();
        assert_eq!(v.kind, ToastKind::Failure);
        assert_eq!(v.text, "Decode worker: IDB write failed");
        assert!(q.pending.is_empty(), "the download failure is not a toast");

        // Absorbing again without new errors adds nothing.
        q.absorb_errors(&errors);
        assert!(q.pending.is_empty());

        errors.push(AppError::Alerts {
            message: "HTTP 503".into(),
        });
        q.absorb_errors(&errors);
        assert_eq!(q.pending.front().unwrap().text, "NWS alerts: HTTP 503");
    }

    #[wasm_bindgen_test]
    fn absorbing_after_the_error_ring_wrapped_reads_only_what_it_holds() {
        let mut errors = ErrorContext::default();
        for i in 0..60 {
            errors.push(AppError::Mping {
                message: format!("e{i}"),
            });
        }
        let mut q = ToastQueue::default();
        q.absorb_errors(&errors);
        // Only the newest MAX_PENDING survive the bounded backlog.
        assert_eq!(q.pending.len(), MAX_PENDING);
        assert_eq!(q.pending.back().unwrap().text, "mPING reports: e59");
    }
}
