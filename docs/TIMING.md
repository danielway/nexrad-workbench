# Timing model

Every timestamp in the codebase falls into one of three categories. This
document defines them, traces where each value comes from, lists which
fields hold which category, and pins the invariants that the UI relies on.

## Why three categories

NEXRAD data takes a non-trivial path from "the radar physically scans"
to "we can download the chunk." Three distinct times exist:

| Category | When | Source |
| --- | --- | --- |
| **ACTUAL** | What was observed | Parsed from radial/message headers, or the literal wall clock |
| **PROJECTED COLLECTION** | When the radar will physically scan a future chunk | VCP physics + history, anchored on a real collection time |
| **PROJECTED AVAILABILITY** | When a chunk will be downloadable from S3 | Empirical S3 deltas + a lag estimate |

The two projected categories differ by the NEXRAD ingest lag (~5–15 s on
typical sites). Conflating them — for example, drawing a "future sweep"
placeholder at its projected S3-arrival time rather than its projected
collection time — produces a UI that's offset late by the pipeline lag.

The three principles the UI obeys:

1. The radar canvas and the current-time indicator show **ACTUAL** times only.
2. Timeline placeholders for future sweeps and chunks use **PROJECTED COLLECTION** times.
3. The download scheduler and "next in N s" countdown use **PROJECTED AVAILABILITY** times.

## 1. ACTUAL times

### 1a. Collection times (from radial headers, parsed in the worker)

NEXRAD Message 31 carries each radial's collection time as
`days-since-epoch + ms-since-midnight`. The decoder exposes this via
`Radial::collection_timestamp() -> i64` (milliseconds since Unix epoch).
We always divide by 1000.0 to produce `f64` seconds with millisecond
precision; ms precision is appropriate because radials arrive ~40 ms
apart at typical scan rates, finer than whole-second resolution.

| Field | What it is | Source |
| --- | --- | --- |
| `volume_header_time_secs` | Time of the radial with `RadialStatus::ScanStart` (the volume's first radial) | `decode/record_decode.rs::extract_volume_start_time` |
| `chunk_min_time_secs` / `chunk_max_time_secs` | Earliest and latest radial collection times within one chunk | `decode/ingest_phases.rs::compute_chunk_time_spans` |
| `PrecomputedSweep.sweep_start_secs` / `sweep_end_secs` / `radial_times` | Per-sweep min/max + per-radial vector | `decode/record_decode.rs::build_precomputed_sweep` |
| `CachedSweep.start` / `CachedSweep.end` | Persisted in the scan_index entry; same data as PrecomputedSweep | `decode/ingest_phases.rs::timing_for_elevation` → `SweepTiming`, persisted by `upsert_scan` |

These are the authoritative "when did the radar physically scan this"
values. They drive:
- The radar canvas's "Time:" label and "Age" display
- The timeline's completed scan blocks and sweep blocks
- The in-progress scan's left edge (`LiveVolumeAnchor::best_start_secs`
  on `LiveModeState.current_volume`)
- The projector's collection anchor (`ProjectionEngine::set_collection_anchor`)
- The empirical availability-lag stat (`s3_last_modified − chunk_max`)

### 1b. S3 upload times (when the chunk became downloadable)

`ChunkIdentifier::upload_date_time()` returns S3's `Last-Modified` HTTP
header for the chunk object. Surfaced as:

- `ChunkArrivalStat.s3_last_modified_at` — recorded for every chunk
  fetch in the streaming loop
- `ProjectionEngine::record_inter_chunk_duration` — the streaming loop
  feeds each S3-upload delta into the engine's rolling
  `ChunkTimingStats`

S3 `Last-Modified` has **1-second resolution** by HTTP convention, so any
single derived measurement (lag, ETA error) is ±1 s noisy. Medians and
averages across many samples wash this out.

### 1c. Wall clock

`js_sys::Date::now() / 1000.0`, wrapped in:
- `current_timestamp_f64() -> f64` (`nexrad/live/realtime/streaming.rs`)
- `TimeModel::wall_clock_time()` (`state/playback.rs` — the shell half
  of the pure `core::TimeModel`)

Used for: the timeline's "now" marker, the live-mode playhead position,
"Age" computation, scheduler timing measurements (`scheduled_at`,
`success_at`, `first_empty_poll_at`).

---

## 2. PROJECTED COLLECTION times

> When the radar will physically scan a future chunk.

Drives timeline placeholders for not-yet-arrived data: the in-progress
volume block's right edge, future-sweep dashed outlines, and the
next-chunk placeholder slot.

### Algorithm (`scan_timing_projection.rs::project_scan_timing`)

```
anchor_collection = engine's collection anchor                 (preferred — current chunk's max radial time)
                 ?? (anchor.upload_time − median_lag)           (fallback when no M chunk yet this volume)
                 ?? (anchor.upload_time − 5s)                   (fallback when no lag stats yet)

for seq in (anchor.sequence + 1)..=final:
    interval        = estimate_interval(prev_meta, next_meta, bucket, stats).seconds
                      (shared primitive; pure physics when no stats, else 70/30 blend)
    cumulative     += interval
    projected_collection_time[seq] = anchor_collection + cumulative
```

The `estimate_interval` primitive lives in
[`interval_estimate.rs`](../src/core/timing/interval_estimate.rs) and is
shared with the scheduler (§3b) and the multi-hop filter path. All three
prediction sites apply the same blend formula, so a regression in one is
observable in all. (The whole `timing/` module — the tuning knobs live in
[`config.rs`](../src/core/timing/config.rs) — moved from `src/nexrad/` to
`src/core/` intact; it is now composed by the
[`ProjectionEngine`](../src/core/projection/engine.rs).)

### The physics term — `ChunkTimingModel`

`estimate_chunk_interval_secs(prev_meta, next_meta)` covers three cases:

- **Inter-volume gap** (between volumes): 8.5 s constant
- **Inter-sweep gap** (between sweeps within a volume):
  `INTER_SWEEP_BASE_GAP_SECS (0.7 s) + elevation_slew + chunk_duration + waveform_transition_penalty`
- **Intra-sweep** (within a sweep): pure `chunk_duration` derived from the VCP's azimuth rate

The **waveform transition penalty** is an empirically-tuned table at
`chunk_timing_model.rs::waveform_transition_penalty_secs` — CS→CDW = 4.0 s,
B→CDWO = 3.5 s, etc. — accounting for hardware reconfiguration cost
between sweeps with different waveforms.

### The historical blend — `ChunkTimingStats`

The `estimate_interval` primitive reads the rolling window of the last
10 observed samples for the target bucket and averages them. Buckets
are keyed on `ChunkCharacteristics = { chunk_type, waveform_type,
channel_configuration, is_first_in_sweep }`.

The lookup is keyed on the **arriving** chunk's characteristics, not the
anchor's (commit `eac0f3f`). Writes record under the arriving chunk's
key, so reading under the anchor's key would silently miss and fall back
to pure physics.

The 70/30 blend hedges against systematic physics bias while not
over-fitting to a small sample window.

### Anchor plumbing (the recently-fixed bit)

The anchor must be the **current chunk's collection-end time**, not the
volume's start time, because the projector adds cumulative intervals
starting from the anchor *forward*. Plumbing path:

```
worker (nexrad/decode/worker_api/ingest.rs)
  └─ emits chunk_max_time_secs in ChunkIngestResponse

chunk-ingest reducer (core/worker_ingest.rs, shell: app/worker_results.rs)
  └─ ChunkIngestActions.record_chunk_collection_end_secs
       └─ live.channel.record_chunk_collection_end_secs(secs)

RealtimeChannel (nexrad/live/realtime/mod.rs)
  └─ sends ProjectorObservation::CollectionEndSecs down the
     observations channel

streaming loop (nexrad/live/realtime/streaming.rs)
  └─ drain_pending_observations at top of iteration (and in the sleep)
       └─ engine.set_collection_anchor(iter.current_id(), secs)

ProjectionEngine (core/projection/engine.rs)
  └─ anchors project_scan_timing on the collection end
```

Reset on every Start chunk (new volume).

### Consumers

- `ChunkProjectedTimes.collection_time_secs` — per chunk, carried on
  the `StreamingPlan`'s `ChunkProjectionInfo` list
- `ScanProjection` (`core/projection/mod.rs`) — the live-scan display
  container the timeline reads for the in-progress block's right edge,
  future-sweep outlines, and per-chunk placeholders
- The VCP forecast diagnostics
  (`core/domain/forecast.rs::derive_volume_forecast` →
  `SweepForecast.predicted_start/end/duration`) — predicted-vs-observed
  sweep timing in the forecast modal

---

## 3. PROJECTED AVAILABILITY times

> When a chunk will be downloadable from S3.

There are **two distinct quantities** here — intentionally, because
they answer different questions. Both are carried per chunk on the
canonical `StreamingPlan` (`core/streaming_plan.rs`) as
`ChunkProjectedTimes::{available_at_secs, poll_at_secs}`.

### 3a. Per-chunk projection availability

For each future chunk in the per-volume projection. Drives the live
diagnostics modal's predicted-vs-observed comparison.

```
availability_lag = anchor.upload_time − anchor_collection                (when anchor collection is known)
                ?? ChunkTimingStats::median_availability_lag_secs()       (median across all buckets)
                ?? 5.0s                                                    (cold-start default)

projected_available_at[i] = projected_collection_time[i] + availability_lag
```

This is a **single scalar lag** applied to all forward chunks of one
projection. We don't yet model per-characteristics lag. (See "open
gaps" at the end.)

### 3b. Scheduler / "next in N s" wait time

The wait the streaming loop sleeps and the user-facing countdown. Both
read the same object: the per-iteration `StreamingPlan`, whose
next-target `poll_at_secs` the projector kernel computes from the
interval decision tree below. (Historically these were two separate
computation paths — a loop-side `time_until_next()` and a UI
projection — which drifted apart; the plan unified them.)

`estimate_next_chunk_time.rs::estimate_chunk_processing_time` (the
decision tree behind the interval term):

```
if current chunk is Start:
    return START_TO_FIRST_INTERMEDIATE_GAP_SECS  (1.5 s)

if VCP metadata is available for current and next chunk:
    interval = estimate_interval(prev, next, bucket, stats).seconds
               (shared primitive — same 70/30 blend the projector uses)
    if ChunkTimingStats has samples for next_chunk.characteristics:
        interval += (avg_attempts − 1) seconds
                    ↑ retry budget — if past chunks usually took N polls,
                      allocate N−1 extra seconds
    return interval
            (path = Blended when stats contributed, else Physics)

else:
    return legacy_static_table[waveform][channel_config]
```

The first-poll target is folded into the forecast by the projector:
`poll_at_secs = available_at_secs + retry_budget + poll_bias_secs`
(`interval_estimate.rs`). The poll bias is **750 ms** —
`TimingTuning::DEFAULT.poll_bias_secs` in
[`core/timing/config.rs`](../src/core/timing/config.rs), the single
home for every estimation tuning knob. The streaming loop sleeps
directly to `poll_at_secs`; there is no separate loop-side pad
constant anymore. On `Ok(None)` (chunk not yet published) the loop
retries under `REALTIME_CHUNK_POLICY` (`net/retry.rs`): full-jitter
exponential backoff, base 500 ms, cap 8 s, up to 8 attempts within a
45 s budget — see STREAMING.md §4.

The 750 ms bias covers WASM/setTimeout slop plus the observed
availability-lag underestimate; it biases only the poll axis and
applies once per chunk (the prediction-driven first poll, not the
retries).

### Consumers

- `RealtimeResult::ChunkReceived.plan` → the Live subsystem's frame
  projection → `subsystem::Live::countdown_remaining_secs(now)`
- `top_bar.rs` — "next in {N} s"
- Timeline next-chunk placeholder (`ui/timeline/overlays.rs`)
- The streaming loop itself — it sleeps to the same plan's
  `poll_at_secs`, so the countdown can't disagree with the actual wait

### 3c. The ScanKey timestamp (real-time path)

The real-time scan identity is **parsed, not estimated**: when the
Start chunk arrives, `volume_header_start_secs`
(`nexrad/live/realtime/streaming.rs`) decodes its volume header and
takes `header.date_time()` truncated to whole seconds. No AWS upload
time, filename string, or lag estimate is involved, and the archive
path derives the exact same value — so an archive download and a
realtime stream of the same physical volume always produce an
identical `ScanKey`. A Start chunk with no readable header is a fatal
stream error, not a fallback.

The in-flight volume's start-time *display* is tracked by
`LiveVolumeAnchor` ([`src/data/live_anchor.rs`](../src/data/live_anchor.rs)):
the whole-second header-parsed value is the `ProvisionalStart`, upgraded
to the ms-precise radial-parsed `ConfirmedStart` once the worker decodes
the volume's first radial. The `ScanKey` itself never changes.

`ScanKey.scan_start` is whole-second. See "Precision" below.

---

## 4. ChunkTimingStats — the rolling-observations cache

The only piece that learns from runtime data. Keyed by
`ChunkCharacteristics`, holds the last 10 samples per bucket. Each sample
carries:

The stats live inside the `ProjectionEngine`'s projector kernel
(`core/projection/`); the streaming loop and the chunk-ingest reducer
feed them through the engine's recording methods.

| Field | Written by | Read by |
| --- | --- | --- |
| `duration` (S3-upload delta) | `ProjectionEngine::record_inter_chunk_duration`, fed by the streaming loop on every chunk fetch | `estimate_interval` — the shared blend used by both the projector and the scheduler |
| `availability_lag` (`upload − collection`) | `ProjectionEngine::record_availability_lag_for`, fed via `ProjectorObservation::AvailabilityLagSecs` after each worker ingest | `median_availability_lag_secs()` — used by the projector lag fallback |
| `attempts` (polls before success) | `ProjectionEngine::record_inter_chunk_duration` (attempts argument) | `get_average_attempts` — feeds the retry budget in the scheduler wait |

Persisted to localStorage with a schema version
(`PERSIST_SCHEMA_VERSION`, currently 3, `chunk_timing_stats.rs`);
payloads with a mismatched version are dropped on load.

---

## 5. Cross-cutting invariants

| Where | Time category | Why |
| --- | --- | --- |
| Radar canvas + "Time:" / "Age:" labels | ACTUAL only | Principle 1 — display only what was observed |
| Live playhead + "now" marker | Wall clock (ACTUAL) | Canvas resolves whichever sweep has `end ≤ playback_position`, so wall-clock tracking shows real data without stutter at chunk boundaries |
| Timeline scan blocks (downloaded scans) | ACTUAL | Read from `CachedSweep.start/end` |
| In-progress volume block left edge | ACTUAL `LiveVolumeAnchor::best_start_secs` | Volume-header time, parsed |
| In-progress volume block right edge | PROJECTED COLLECTION | When the radar will *finish* scanning |
| Future-sweep dashed outlines on timeline | PROJECTED COLLECTION | When the radar will scan, not when it'll upload |
| Next-chunk placeholder on timeline (X position) | PROJECTED COLLECTION | Placement reflects scan timing |
| Next-chunk placeholder text countdown | PROJECTED AVAILABILITY | "When can we download it" |
| "Next in N s" countdown anywhere | PROJECTED AVAILABILITY | Same |
| Scheduler poll timing | PROJECTED AVAILABILITY | When to hit S3 |
| `ScanKey.scan_start` (real-time path) | ACTUAL parsed from the Start chunk's volume header; whole-second resolution (§3c) | Stable IDB key, identical to the archive path's key for the same volume |
| `ScanKey.scan_start` (archive path) | ACTUAL parsed from the volume header | Authoritative |

---

## 6. Precision

ACTUAL collection times (radial-derived) are **millisecond-precise**
end-to-end:

- Source: `Radial::collection_timestamp() -> i64` is ms-since-epoch
- Conversion: `ts as f64 / 1000.0` preserves precision (ms values fit
  comfortably within f64's ~15 decimal digits)
- IDB persistence: `PrecomputedSweep`'s binary blob stores
  `sweep_start_secs`, `sweep_end_secs`, and per-radial times as f64
- ChunkTimingStats samples are stored as `i64` ms

S3 upload times are **whole-second** by HTTP `Last-Modified` convention.
Single derived measurements (per-chunk lag, prediction error) are ±1 s
noisy from this side; medians and rolling averages smooth it.

Wall clock is sub-millisecond from `js_sys::Date::now()` but is only
displayed at second-or-coarser precision (the user-facing "now" line).

`ScanKey.scan_start` is **whole-second** in both real-time and archive
paths. This is intentional — it's a key, used for stable identification
across sessions. **Don't compare `CachedSweep.start` against
`ScanKey.scan_start` expecting an exact match**: the sweep is ms-precise
and the key is truncated to seconds. The first sweep of a volume can end
up with `start` slightly before or slightly after `scan_start` depending
on rounding direction.

---

## 7. Quick "where do I look when X is wrong"

- **Canvas time / age is wrong** → check the playback position and the
  displayed sweep's `CachedSweep.start/end`; both should be ACTUAL.
- **Future placeholder is in the wrong place on the timeline** → check
  the collection anchor is being plumbed
  (`ProjectorObservation::CollectionEndSecs` →
  `ProjectionEngine::set_collection_anchor`) and that
  `project_scan_timing` is anchored on it.
- **"Next in N s" is wrong** → check `estimate_chunk_processing_time`
  and the historical-stats lookup keying. Pure physics fallback is also
  a smell — implies stats aren't warming up.
- **Scan disappears from timeline mid-volume** → check the
  `ScanProjection` the timeline reads (`core/projection/mod.rs`) and
  the projected volume end it carries. If the projected end is sliding
  into the past, the projection anchor is wrong.
- **Lag prediction is biased** → inspect `ChunkTimingStats` median lag
  in the debug panel; if cold, the 5 s default kicks in.
- **Stats not warming up** → confirm the
  `ProjectorObservation::AvailabilityLagSecs` observation is being sent
  after each ingest (`ChunkIngestActions.record_availability_lag_secs`
  in the `core::worker_ingest` reducer); `chunk_max_time_secs` and
  `s3_last_modified_at` must both be available in the ingest outcome.

---

## 8. Open gaps

1. **Per-characteristics lag isn't used.** Each `TimingStat` carries
   `availability_lag` keyed by characteristics, but only the *global*
   median is exposed via `median_availability_lag_secs()`. If e.g. Start
   chunks systematically have a different lag than M chunks, the
   projector won't pick that up.
2. **Projection availability uses a constant scalar lag across all
   forward chunks** (3a). A more sophisticated model would use a
   per-chunk lag estimate so e.g. an in-progress sweep's tail chunks get
   the right lag. The scheduler (3b) wins on per-chunk accuracy because
   it uses per-characteristics historical S3 deltas, so this hasn't been
   a problem in practice.
3. **Scheduler still drives off `duration_ms` (S3 deltas), not split
   residuals.** A future change could switch the historical input from
   raw S3-upload-delta to `physics + collection_residual + lag_delta`
   for cleaner attribution when predictions go wrong. The 70/30
   physics/historical blend (now shared with the projector via
   [`estimate_interval`](../src/core/timing/interval_estimate.rs)) is
   already a partial mitigation; the deeper split is still open.
4. ~~No IDB rename to re-anchor `ScanKey.scan_start` on the parsed
   `volume_header_time`.~~ **Resolved**: the real-time scan key is now
   parsed directly from the Start chunk's volume header (§3c), so the
   key never needs re-anchoring; the `LiveVolumeAnchor`
   provisional→confirmed transition covers the display-precision
   upgrade.
