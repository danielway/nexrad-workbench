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
pub enum Verdict<T> {
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
pub struct RetryPolicy {
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
pub const DEFAULT_POLICY: RetryPolicy = RetryPolicy {
    base: Duration::from_millis(250),
    cap: Duration::from_secs(4),
    max_attempts: 4,
    total_budget: Duration::from_secs(30),
    per_attempt_timeout: Duration::from_secs(15),
};

/// Recovery policy for real-time chunk fetch when the timing-prediction-driven
/// first attempt returns 404. Larger base delay (chunks land seconds late, not
/// milliseconds), shorter per-attempt timeout (a 404 round-trip is fast),
/// total budget sized to roughly match the prior 25×500ms+2.5s polling window.
pub const REALTIME_CHUNK_POLICY: RetryPolicy = RetryPolicy {
    base: Duration::from_millis(500),
    cap: Duration::from_secs(4),
    max_attempts: 6,
    total_budget: Duration::from_secs(15),
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
pub async fn with_retry<F, Fut, T>(
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
pub fn compute_delay(
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
pub async fn attempt_with_timeout<Fut, T>(fut: Fut, timeout: Duration) -> Verdict<T>
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
pub async fn sleep_duration(dur: Duration) {
    let ms = dur.as_millis().min(u32::MAX as u128) as u32;
    sleep_ms(ms).await;
}

/// Wait `ms` milliseconds via browser `setTimeout`.
pub async fn sleep_ms(ms: u32) {
    #[wasm_bindgen]
    extern "C" {
        #[wasm_bindgen(js_name = setTimeout)]
        fn set_timeout(closure: &Closure<dyn FnMut()>, millis: u32) -> i32;
    }

    let (tx, rx) = futures_channel::oneshot::channel::<()>();
    let closure = Closure::once(move || {
        let _ = tx.send(());
    });
    set_timeout(&closure, ms);
    let _ = rx.await;
}

/// Parse an HTTP `Retry-After` header value.
///
/// Accepts either delta-seconds (e.g., `"120"`) or HTTP-date. Returns `None`
/// for malformed values; HTTP-date parsing is best-effort via chrono.
#[allow(dead_code)] // wired in by the alerts endpoint in a follow-up commit
pub fn parse_retry_after(header: &str) -> Option<Duration> {
    let trimmed = header.trim();
    if let Ok(secs) = trimmed.parse::<u64>() {
        return Some(Duration::from_secs(secs));
    }
    let parsed = chrono::DateTime::parse_from_rfc2822(trimmed).ok()?;
    let now = chrono::Utc::now();
    let delta = parsed.with_timezone(&chrono::Utc) - now;
    delta.to_std().ok()
}
