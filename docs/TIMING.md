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
| `volume_header_time_secs` | Time of the radial with `RadialStatus::ScanStart` (the volume's first radial) | `record_decode.rs::extract_volume_start_time` |
| `chunk_min_time_secs` / `chunk_max_time_secs` | Earliest and latest radial collection times within one chunk | `ingest_phases.rs::compute_chunk_time_spans` |
| `PrecomputedSweep.sweep_start_secs` / `sweep_end_secs` / `radial_times` | Per-sweep min/max + per-radial vector | `record_decode.rs::build_precomputed_sweep` |
| `SweepMeta.start` / `SweepMeta.end` | Persisted in the scan_index entry; same data as PrecomputedSweep | `ingest_phases.rs::build_sweep_meta` |

These are the authoritative "when did the radar physically scan this"
values. They drive:
- The radar canvas's "Time:" label and "Age" display
- The timeline's completed scan blocks and sweep blocks
- The in-progress scan's left edge (`live_mode_state.current_volume_start`)
- The projector's anchor (`latest_chunk_collection_end_secs`)
- The empirical availability-lag stat (`s3_last_modified − chunk_max`)

### 1b. S3 upload times (when the chunk became downloadable)

`ChunkIdentifier::upload_date_time()` returns S3's `Last-Modified` HTTP
header for the chunk object. Surfaced as:

- `ChunkArrivalStat.s3_last_modified_at` — recorded for every chunk
  fetch in the streaming loop
- `StreamingState.last_chunk_time` — internal, used to compute
  S3-upload deltas for `ChunkTimingStats`

S3 `Last-Modified` has **1-second resolution** by HTTP convention, so any
single derived measurement (lag, ETA error) is ±1 s noisy. Medians and
averages across many samples wash this out.

### 1c. Wall clock

`js_sys::Date::now() / 1000.0`, wrapped in:
- `current_timestamp() -> i64` and `current_timestamp_f64() -> f64` (`realtime.rs`)
- `TimeModel::wall_clock_time()` (`state/playback.rs`)

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
anchor_collection = latest_chunk_collection_end_secs           (preferred — current chunk's max radial time)
                 ?? (anchor.upload_time − median_lag)           (fallback when no M chunk yet this volume)
                 ?? (anchor.upload_time − 5s)                   (fallback when no lag stats yet)

for seq in (anchor.sequence + 1)..=final:
    physics_interval = ChunkTimingModel::estimate_chunk_interval_secs(prev_meta, next_meta)
    interval         = 0.7 * physics_interval + 0.3 * historical_avg_for(next.characteristics)
                       (only when ChunkTimingStats has a sample for the next chunk's bucket;
                        else pure physics)
    cumulative      += interval
    projected_collection_time[seq] = anchor_collection + cumulative
```

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

`get_average_timing(next_chunk_characteristics)` returns the rolling
average of the last 10 observed S3-upload deltas for that bucket. Buckets
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
worker (worker_api/ingest.rs)
  └─ emits chunk_max_time_secs in ChunkIngestResponse

main.rs ingest handler
  └─ self.streaming.record_chunk_collection_end_secs(chunk_max_secs)

StreamingManager::record_chunk_collection_end_secs
  └─ RealtimeChannel::record_chunk_collection_end_secs

RealtimeChannel
  └─ stores in shared RealtimeState.pending_chunk_collection_end_secs

streaming loop (realtime.rs)
  └─ drain_pending_ingest_observations at top of iteration
       └─ iter.record_chunk_collection_end_secs(secs)

StreamingState.latest_chunk_collection_end_secs
  └─ passed to project_scan_timing as anchor_collection_time_secs
```

Reset on every Start chunk (new volume).

### Consumers

- `ChunkProjectionInfo.projected_collection_time_secs` (per-chunk)
- `RealtimeResult::ChunkReceived.projected_volume_end_collection_secs` (volume end)
- `LiveModeState.projected_volume_end_collection_secs`, fed to:
  - `VcpPositionModel::from_live` → `volume_end` (timeline right-edge of in-progress volume)
  - `try_capture_forecast` → `SweepForecast.predicted_start/end/duration`
  - `capture_mid_prediction` → `forecast.mid_predicted_start/end`
- `render_realtime_progress` (`ui/timeline/overlays.rs`) — places the
  in-progress block, future sweeps, and per-chunk placeholders on the
  X axis

---

## 3. PROJECTED AVAILABILITY times

> When a chunk will be downloadable from S3.

There are **two independent paths** that compute this differently —
intentionally, because they answer different questions.

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

What `iter.time_until_next()` returns to the streaming loop and what
drives the user-facing countdown. **Doesn't go through the projector at
all** — driven by historical S3 deltas with physics fallback.

`estimate_next_chunk_time.rs::estimate_chunk_processing_time`:

```
if current chunk is Start:
    return START_TO_FIRST_INTERMEDIATE_GAP_SECS  (1.5 s)

if ChunkTimingStats has samples for next_chunk.characteristics:
    return historical_avg_S3_delta + (avg_attempts − 1) seconds
                                    ↑ retry budget — if past chunks usually
                                      took N polls, allocate N−1 extra seconds

else if VCP physics is available:
    return ChunkTimingModel::estimate_chunk_interval_secs(current, next)
            (no historical blend in this path — pure physics fallback)

else:
    return legacy_static_table[waveform][channel_config]
```

The streaming loop takes this wait, schedules the first poll at
`predicted_available_at + POLL_DELAY_AFTER_PREDICTED_MS (400 ms)`, then
on `Ok(None)` retries every `CHUNK_POLL_INTERVAL_MS (500 ms)` up to
`CHUNK_POLL_MAX_RETRIES (25)` times, then a `CHUNK_POLL_GRACE_MS (2.5 s)`
final grace.

The 400 ms pad covers WASM/setTimeout slop plus a small bias hedge; was
600 ms before commit `eac0f3f` fixed the historical-stats keying.

### Consumers

- `RealtimeResult::ChunkReceived.time_until_next` →
  `LiveModeState.next_chunk_available_at_secs` →
  `countdown_remaining_secs(now)`
- `playback_controls.rs` — "next in {N} s"
- `top_bar.rs` — "(N chunks) next in {N} s"
- Timeline next-chunk placeholder text label (`overlays.rs`)
- The streaming loop itself for actual scheduling

### 3c. The ScanKey provisional timestamp

When the Start chunk arrives in real-time, its scan-start timestamp is
`upload_date_time(start_chunk) − median_availability_lag` (`realtime.rs::
provisional_scan_start_secs`). Falls back to wall-clock if no upload time
is available. This places `ScanKey.scan_start` within ~1 s of the true
volume-header time before any M chunk has arrived (which is what allows
us to anchor the projection's collection axis on a meaningful value
even at volume start).

`ScanKey.scan_start` is whole-second (`provisional_scan_start_secs(...).
round() as i64`, `UnixMillis::from_secs(...)`). See "Precision" below.

---

## 4. ChunkTimingStats — the rolling-observations cache

The only piece that learns from runtime data. Keyed by
`ChunkCharacteristics`, holds the last 10 samples per bucket. Each sample
carries:

| Field | Written by | Read by |
| --- | --- | --- |
| `duration` (S3-upload delta) | `StreamingState::update_timing_stats` in the streaming loop, on every chunk fetch | `get_average_timing` — used by both the projector blend and the scheduler |
| `availability_lag` (`upload − collection`) | `StreamingState::record_availability_lag_for_current`, called from main.rs after each ingest | `median_availability_lag_secs()` — used by projector lag fallback and `provisional_scan_start_secs` |
| `attempts` (polls before success) | `StreamingState::update_timing_stats` | `get_average_attempts` — used in scheduler wait-time formula |

Persisted to localStorage with schema `version: 2`; v1 payloads are
dropped on load (the lag field can't be backfilled).

---

## 5. Cross-cutting invariants

| Where | Time category | Why |
| --- | --- | --- |
| Radar canvas + "Time:" / "Age:" labels | ACTUAL only | Principle 1 — display only what was observed |
| Live playhead + "now" marker | Wall clock (ACTUAL) | Canvas resolves whichever sweep has `end ≤ playback_position`, so wall-clock tracking shows real data without stutter at chunk boundaries |
| Timeline scan blocks (downloaded scans) | ACTUAL | Read from `SweepMeta.start/end` |
| In-progress volume block left edge | ACTUAL `current_volume_start` | Volume-header time, parsed |
| In-progress volume block right edge | PROJECTED COLLECTION | When the radar will *finish* scanning |
| Future-sweep dashed outlines on timeline | PROJECTED COLLECTION | When the radar will scan, not when it'll upload |
| Next-chunk placeholder on timeline (X position) | PROJECTED COLLECTION | Placement reflects scan timing |
| Next-chunk placeholder text countdown | PROJECTED AVAILABILITY | "When can we download it" |
| "Next in N s" countdown anywhere | PROJECTED AVAILABILITY | Same |
| Scheduler poll timing | PROJECTED AVAILABILITY | When to hit S3 |
| `ScanKey.scan_start` (real-time path) | ACTUAL provisional (`upload − median_lag`); whole-second resolution | Stable IDB key close to true volume-start time |
| `ScanKey.scan_start` (archive path) | ACTUAL parsed from filename | Authoritative |

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
across sessions. **Don't compare `SweepMeta.start` against
`ScanKey.scan_start` expecting an exact match**: the sweep is ms-precise
and the key is rounded to seconds. The first sweep of a volume can end
up with `start` slightly before or slightly after `scan_start` depending
on rounding direction.

---

## 7. Quick "where do I look when X is wrong"

- **Canvas time / age is wrong** → check `viz_state.timestamp` and the
  sweep's `SweepMeta.start/end`; both should be ACTUAL.
- **Future placeholder is in the wrong place on the timeline** → check
  `latest_chunk_collection_end_secs` is being plumbed and that
  `project_scan_timing` is using it as the anchor.
- **"Next in N s" is wrong** → check `estimate_chunk_processing_time`
  and the historical-stats lookup keying. Pure physics fallback is also
  a smell — implies stats aren't warming up.
- **Scan disappears from timeline mid-volume** → check
  `render_realtime_progress`'s early-return guard
  (`expected_end < view_start`) and what's feeding
  `VcpPositionModel.volume_end`. If `projected_volume_end_collection_secs`
  is sliding into the past, the projection anchor is wrong.
- **Lag prediction is biased** → inspect `ChunkTimingStats` median lag
  in the debug panel; if cold, the 5 s default kicks in.
- **Stats not warming up** → confirm
  `record_availability_lag_for_current` is being called;
  `chunk_max_time_secs` and `s3_last_modified_at` must both be available
  in main.rs's ingest handler.

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
3. **Scheduler still drives off `duration_ms` (S3 deltas), not the split
   residuals.** A future change could switch the scheduler to
   `physics + collection_residual + lag_delta` for cleaner attribution
   when predictions go wrong, but the current empirically-tuned path
   works well enough that a rewrite carries regression risk.
4. **No IDB rename to re-anchor `ScanKey.scan_start` on the parsed
   `volume_header_time` once the first M chunk arrives.** Currently the
   provisional scan-start (`upload − median_lag`) stays for the whole
   volume's lifetime in IDB. The residual error is ~1 s and the rename
   would require a cross-store atomic transaction; deferred as a
   follow-up.
