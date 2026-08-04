//! Exponential-backoff retry with full jitter, applied uniformly across all
//! outbound HTTP calls.
//!
//! Each call site supplies a closure that performs one attempt and classifies
//! the outcome as [`Verdict::Ok`], [`Verdict::Retry`], or [`Verdict::Terminal`].
//! The helper handles per-attempt timeouts, between-attempt sleeps, the
//! `Retry-After` hint, and the overall attempt/wall-clock budget.
//!
//! Two presets cover the call sites we have today:
//! - [`DEFAULT_POLICY`] — transient-error recovery for one-shot user-driven
//!   fetches (archive list/download, alerts, geocoding).
//! - [`REALTIME_CHUNK_POLICY`] — recovery after a timing-prediction-driven
//!   first attempt misses on a real-time chunk that hasn't been published yet.

use std::future::Future;
use std::time::Duration;
use wasm_bindgen::prelude::*;

/// Outcome of a single attempt.
pub(crate) enum Verdict<T> {
    /// Attempt succeeded — return this value.
    Ok(T),
    /// Attempt failed transiently — back off and try again.
    /// `after` carries an optional `Retry-After` hint from the server.
    Retry { after: Option<Duration> },
    /// Attempt failed in a way that further retries cannot fix.
    Terminal(String),
}

/// Backoff parameters. See module docs for preset rationale.
#[derive(Clone, Debug)]
pub(crate) struct RetryPolicy {
    /// Base delay for exponential backoff. Delay before retry N is drawn from
    /// `random_uniform(0, min(cap, base * 2^(N-1)))` (full jitter).
    pub base: Duration,
    /// Upper bound on the per-retry delay window.
    pub cap: Duration,
    /// Total attempts including the first. `max_attempts = 1` disables retry.
    pub max_attempts: u32,
    /// Wall-clock budget across all attempts. Whichever fires first
    /// (`max_attempts` or `total_budget`) ends the loop.
    pub total_budget: Duration,
    /// Timeout applied to each individual attempt.
    pub per_attempt_timeout: Duration,
}

/// Default policy for one-shot user-driven fetches: archive list/download,
/// NWS alerts, zip-code lookup, mosaic refresh.
pub(crate) const DEFAULT_POLICY: RetryPolicy = RetryPolicy {
    base: Duration::from_millis(250),
    cap: Duration::from_secs(4),
    max_attempts: 4,
    total_budget: Duration::from_secs(30),
    per_attempt_timeout: Duration::from_secs(15),
};

/// Recovery policy for real-time chunk fetch when the timing-prediction-driven
/// first attempt returns 404. Larger base delay (chunks land seconds late, not
/// milliseconds), shorter per-attempt timeout (a 404 round-trip is fast).
///
/// Sized for robustness against prediction error rather than for the
/// best-case path: an 8s cap lets a single backoff sleep span the upper tail
/// of inter-volume gap variance (observed range 7–10s) without spinning
/// through wasted 404s, and a 45s total budget leaves ~30s of *sleep* budget
/// after subtracting worst-case per-attempt waits. The cost is that a stream
/// against a genuinely failing endpoint takes longer to surface the error;
/// for a real-time viewer that's the right trade — visual idleness already
/// communicates the failure.
pub(crate) const REALTIME_CHUNK_POLICY: RetryPolicy = RetryPolicy {
    base: Duration::from_millis(500),
    cap: Duration::from_secs(8),
    max_attempts: 8,
    total_budget: Duration::from_secs(45),
    per_attempt_timeout: Duration::from_secs(5),
};

/// Run `op` under `policy`. Returns the first `Ok` value, or the first
/// `Terminal`, or a budget-exceeded error.
///
/// The closure receives the 1-based attempt number for diagnostics. Note: the
/// returned future must not borrow from the closure's environment — capture
/// state by clone or via shared ownership. Call sites that need to mutate
/// captured state across attempts (e.g. an `&mut Iterator`) should use the
/// per-attempt primitives ([`attempt_with_timeout`], [`compute_delay`],
/// [`sleep_duration`]) directly in an inline loop.
pub(crate) async fn with_retry<F, Fut, T>(
    policy: &RetryPolicy,
    label: &str,
    mut op: F,
) -> Result<T, String>
where
    F: FnMut(u32) -> Fut,
    Fut: Future<Output = Verdict<T>>,
{
    let started = web_time::Instant::now();
    let mut last_msg: Option<String> = None;

    for attempt in 1..=policy.max_attempts {
        let verdict = attempt_with_timeout(op(attempt), policy.per_attempt_timeout).await;
        match verdict {
            Verdict::Ok(v) => return Ok(v),
            Verdict::Terminal(msg) => return Err(msg),
            Verdict::Retry { after } => {
                last_msg = Some(format!("attempt {} failed", attempt));
                if attempt >= policy.max_attempts {
                    break;
                }
                let elapsed = started.elapsed();
                if elapsed >= policy.total_budget {
                    return Err(format!(
                        "{}: budget {}s exhausted after {} attempts",
                        label,
                        policy.total_budget.as_secs(),
                        attempt
                    ));
                }
                let mut delay = compute_delay(policy, attempt, after);
                let remaining = policy.total_budget.saturating_sub(elapsed);
                if delay > remaining {
                    delay = remaining;
                }
                sleep_duration(delay).await;
            }
        }
    }

    Err(format!(
        "{}: gave up after {} attempts ({})",
        label,
        policy.max_attempts,
        last_msg.as_deref().unwrap_or("retry exhausted")
    ))
}

/// Compute the backoff delay between attempt `n` and attempt `n+1`. Honors a
/// server-supplied `Retry-After`, otherwise full-jitter exponential.
pub(crate) fn compute_delay(
    policy: &RetryPolicy,
    failure_count: u32,
    retry_after: Option<Duration>,
) -> Duration {
    if let Some(after) = retry_after {
        // Honor server hint, but bound it so a misbehaving server cannot
        // pin us indefinitely. 2× the configured cap is the ceiling.
        let ceiling = policy.cap.saturating_mul(2);
        return after.min(ceiling);
    }

    // Exponential window: base * 2^(failure_count - 1), saturating at cap.
    // failure_count is 1-based (1 means we just had our first failure, so
    // this is the wait before attempt 2).
    let shift = failure_count.saturating_sub(1).min(20);
    let exp_ms = (policy.base.as_millis() as u64).saturating_mul(1u64 << shift);
    let cap_ms = policy.cap.as_millis() as u64;
    let window_ms = exp_ms.min(cap_ms);

    // Full jitter: uniform random in [0, window_ms).
    let jitter = js_sys::Math::random() * window_ms as f64;
    Duration::from_millis(jitter as u64)
}

/// Run `fut` to completion or treat it as a transient failure if it doesn't
/// finish within `timeout`.
pub(crate) async fn attempt_with_timeout<Fut, T>(fut: Fut, timeout: Duration) -> Verdict<T>
where
    Fut: Future<Output = Verdict<T>>,
{
    let timer = sleep_duration(timeout);
    futures_util::pin_mut!(fut);
    futures_util::pin_mut!(timer);
    match futures_util::future::select(fut, timer).await {
        futures_util::future::Either::Left((verdict, _)) => verdict,
        futures_util::future::Either::Right(((), _)) => Verdict::Retry { after: None },
    }
}

/// Wait approximately `dur` using browser `setTimeout`. The browser clamps
/// the minimum delay (~4ms in modern browsers); below that `setTimeout(0)`
/// just yields.
pub(crate) async fn sleep_duration(dur: Duration) {
    let ms = dur.as_millis().min(u32::MAX as u128) as u32;
    sleep_ms(ms).await;
}

/// Wait `ms` milliseconds via browser `setTimeout`, cancelling the underlying
/// timer if the future is dropped before it fires. Necessary because a stale
/// `setTimeout` would otherwise invoke a dropped wasm closure and throw
/// "closure invoked recursively or after being dropped".
pub(crate) fn sleep_ms(ms: u32) -> impl Future<Output = ()> {
    use std::cell::Cell;
    use std::pin::Pin;
    use std::rc::Rc;
    use std::task::{Context, Poll, Waker};

    #[wasm_bindgen]
    extern "C" {
        #[wasm_bindgen(js_name = setTimeout)]
        fn set_timeout(closure: &Closure<dyn FnMut()>, millis: u32) -> i32;
        #[wasm_bindgen(js_name = clearTimeout)]
        fn clear_timeout(id: i32);
    }

    struct Timer {
        id: Cell<Option<i32>>,
        fired: Rc<Cell<bool>>,
        waker: Rc<Cell<Option<Waker>>>,
        _closure: Closure<dyn FnMut()>,
    }

    impl Drop for Timer {
        fn drop(&mut self) {
            if let Some(id) = self.id.take() {
                clear_timeout(id);
            }
        }
    }

    impl Future for Timer {
        type Output = ();
        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
            if self.fired.get() {
                self.id.set(None);
                Poll::Ready(())
            } else {
                self.waker.set(Some(cx.waker().clone()));
                Poll::Pending
            }
        }
    }

    let fired = Rc::new(Cell::new(false));
    let waker: Rc<Cell<Option<Waker>>> = Rc::new(Cell::new(None));

    let fired_cb = fired.clone();
    let waker_cb = waker.clone();
    let closure = Closure::wrap(Box::new(move || {
        fired_cb.set(true);
        if let Some(w) = waker_cb.take() {
            w.wake();
        }
    }) as Box<dyn FnMut()>);

    let id = set_timeout(&closure, ms);

    Timer {
        id: Cell::new(Some(id)),
        fired,
        waker,
        _closure: closure,
    }
}

/// Parse an HTTP `Retry-After` header value.
///
/// Accepts either delta-seconds (e.g., `"120"`) or HTTP-date. Returns `None`
/// for malformed values; HTTP-date parsing is best-effort via chrono.
pub(crate) fn parse_retry_after(header: &str) -> Option<Duration> {
    let trimmed = header.trim();
    if let Ok(secs) = trimmed.parse::<u64>() {
        return Some(Duration::from_secs(secs));
    }
    let parsed = chrono::DateTime::parse_from_rfc2822(trimmed).ok()?;
    let now = chrono::Utc::now();
    let delta = parsed.with_timezone(&chrono::Utc) - now;
    delta.to_std().ok()
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn default_policy_constants_are_documented_values() {
        assert_eq!(DEFAULT_POLICY.base, Duration::from_millis(250));
        assert_eq!(DEFAULT_POLICY.cap, Duration::from_secs(4));
        assert_eq!(DEFAULT_POLICY.max_attempts, 4);
        assert_eq!(DEFAULT_POLICY.total_budget, Duration::from_secs(30));
        assert_eq!(DEFAULT_POLICY.per_attempt_timeout, Duration::from_secs(15));
    }

    #[wasm_bindgen_test]
    fn realtime_chunk_policy_constants_are_documented_values() {
        assert_eq!(REALTIME_CHUNK_POLICY.base, Duration::from_millis(500));
        assert_eq!(REALTIME_CHUNK_POLICY.cap, Duration::from_secs(8));
        assert_eq!(REALTIME_CHUNK_POLICY.max_attempts, 8);
        assert_eq!(REALTIME_CHUNK_POLICY.total_budget, Duration::from_secs(45));
        assert_eq!(
            REALTIME_CHUNK_POLICY.per_attempt_timeout,
            Duration::from_secs(5)
        );
    }

    #[wasm_bindgen_test]
    fn compute_delay_honors_retry_after_when_below_ceiling() {
        // cap is 4s, ceiling is 2× cap = 8s; a 3s hint passes through verbatim.
        let d = compute_delay(&DEFAULT_POLICY, 1, Some(Duration::from_secs(3)));
        assert_eq!(d, Duration::from_secs(3));
    }

    #[wasm_bindgen_test]
    fn compute_delay_clamps_retry_after_to_twice_the_cap() {
        // A misbehaving server asking for 100s is pinned to 2× cap = 8s.
        let d = compute_delay(&DEFAULT_POLICY, 1, Some(Duration::from_secs(100)));
        assert_eq!(d, DEFAULT_POLICY.cap.saturating_mul(2));
        assert_eq!(d, Duration::from_secs(8));
    }

    #[wasm_bindgen_test]
    fn compute_delay_full_jitter_stays_within_first_window() {
        // failure_count 1 → window = base = 250ms. Full jitter ⇒ [0, 250ms).
        for _ in 0..500 {
            let d = compute_delay(&DEFAULT_POLICY, 1, None);
            assert!(
                d < Duration::from_millis(250),
                "delay {d:?} exceeded window"
            );
        }
    }

    #[wasm_bindgen_test]
    fn compute_delay_window_grows_then_saturates_at_cap() {
        // failure_count 2 → base*2 = 500ms window.
        for _ in 0..200 {
            let d = compute_delay(&DEFAULT_POLICY, 2, None);
            assert!(d < Duration::from_millis(500));
        }
        // Very large failure_count saturates at cap (shift capped at 20, then
        // min(cap)). Window never exceeds cap = 4s.
        for _ in 0..200 {
            let d = compute_delay(&DEFAULT_POLICY, 30, None);
            assert!(d <= DEFAULT_POLICY.cap, "delay {d:?} exceeded cap");
        }
    }

    #[wasm_bindgen_test]
    fn compute_delay_does_not_overflow_for_extreme_failure_counts() {
        // Guards the 1u64 << shift path against panics on huge counts.
        let d = compute_delay(&DEFAULT_POLICY, u32::MAX, None);
        assert!(d <= DEFAULT_POLICY.cap);
    }

    #[wasm_bindgen_test]
    fn parse_retry_after_delta_seconds() {
        assert_eq!(parse_retry_after("120"), Some(Duration::from_secs(120)));
        assert_eq!(parse_retry_after("0"), Some(Duration::from_secs(0)));
        // Surrounding whitespace is trimmed.
        assert_eq!(parse_retry_after("  42 "), Some(Duration::from_secs(42)));
    }

    #[wasm_bindgen_test]
    fn parse_retry_after_rejects_garbage() {
        assert_eq!(parse_retry_after(""), None);
        assert_eq!(parse_retry_after("abc"), None);
        assert_eq!(parse_retry_after("12.5"), None);
        assert_eq!(parse_retry_after("-3"), None);
    }

    #[wasm_bindgen_test]
    fn parse_retry_after_future_http_date_is_positive_duration() {
        // A date comfortably in the future parses to a positive remaining delay.
        let d = parse_retry_after("Wed, 21 Oct 2099 07:28:00 GMT");
        assert!(d.is_some(), "expected Some for future date");
        assert!(d.unwrap() > Duration::from_secs(0));
    }

    #[wasm_bindgen_test]
    fn parse_retry_after_past_http_date_is_none() {
        // A past date has a negative delta → not representable as a Duration.
        assert_eq!(parse_retry_after("Wed, 21 Oct 1999 07:28:00 GMT"), None);
    }
}
