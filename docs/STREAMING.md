# Real-time streaming

How the live-mode loop predicts when each chunk will be downloadable,
sleeps until that moment, polls S3 with a bounded retry budget when the
prediction fires early, and learns from each arrival to make the next
prediction better.

This document is about **sequencing and timing** only. The mechanics of
decoding a chunk and rendering it are covered in
[RENDERING.md](RENDERING.md). The semantic distinction between ACTUAL,
PROJECTED COLLECTION, and PROJECTED AVAILABILITY times is in
[TIMING.md](TIMING.md) — read it first if you have not. This doc layers
on top of those definitions and focuses on the operational flow.

---

## 1. Components

| Layer | Type | Role |
| --- | --- | --- |
| Channel | [`RealtimeChannel`](../src/nexrad/realtime.rs) | The single owner the egui update loop talks to. Holds the shared state cell, spawns the streaming task, and exposes the result queue + observation setters + filter sync. |
| Async task | `streaming_loop` (in [`realtime.rs`](../src/nexrad/realtime.rs)) | The long-running future. One per active site. Owns the iteration, sleeping, polling, and emit. |
| Iterator state | [`StreamingState`](../src/nexrad/streaming_state.rs) | Replaces `nexrad_data`'s `ChunkIterator`. Holds the current `ChunkIdentifier`, the VCP, the elevation/chunk mapper, and the rolling timing stats. |
| Predictor | [`timing/`](../src/nexrad/timing/) | Pure functions over `(current_chunk, vcp, mapper, stats)` that return either a wait duration (scheduler) or a per-chunk projection (timeline). |

The async task drives the streaming. The channel is just the shared
mailbox between it and the UI thread.

### Communication shape

```
egui frame ──poll──▶ RealtimeChannel.results
                              ▲
                              │ push
                        streaming_loop
                              │
                  predict ─ sleep ─ fetch ─ emit
                              │
                  observation ◀──pending_*──── main.rs
                  (collection time, lag, filter)
```

The `pending_*` setters on `RealtimeChannel` are mutate-and-forget — the
streaming loop drains them at the top of each iteration. This avoids
needing a second async channel for the worker → loop direction and keeps
the loop's borrow of `iter` exclusive.

---

## 2. The streaming loop

`streaming_loop` is structured in three phases:

1. **Acquire** — discover the latest volume, download the latest chunk
   plus (if mid-volume) the Start chunk, build the elevation/chunk
   mapper from the VCP, warm up timing stats from localStorage.
2. **Init backfill** — emit the chunks the renderer needs to display
   the user's selected sweep on connect, even though we joined
   mid-volume.
3. **Steady state** — repeating predict-sleep-fetch-emit cycle until
   stop.

### 2a. Acquire (init)

Wrapped in a 10 s timeout (`ACQUIRE_TIMEOUT_SECS`). When the timeout
wins the `select`, the in-flight HTTP futures get dropped, which
cancels them — there is no orphan request to recover. Failure emits a
`RealtimeResult::Error` and exits.

`acquire_streaming_state` calls `nexrad_data::aws::realtime::get_latest_volume`
(rotated-array binary search across the 999 volume buckets) and then
`StreamingState::init_at_volume`. Init lists the volume's chunks, fetches
the **latest** chunk, and — if that chunk is not the Start — separately
fetches the Start chunk so the VCP can be parsed. Without the VCP there
is no `ElevationChunkMapper` and the loop cannot predict anything.

The cached volume number written to localStorage by `cache_volume_number`
is currently **not consulted** — discovery always goes through
`get_latest_volume`. The cache exists for a future fast-path hint.

Once `StreamingState` exists we also try to load a previously-persisted
`ChunkTimingStats` snapshot from localStorage (key
`nexrad_timing_stats_<SITE>`) so the very first prediction in this
session does not have to fall back to pure physics.

### 2b. Init backfill (mid-volume join)

If we joined after sequence 1, the latest chunk by itself is not enough
for the renderer — it expects the chunks of the user's currently-selected
sweep to all arrive in order. The init phase backfills as follows:

| Filter | What gets backfilled |
| --- | --- |
| `All` (latest) | The current sweep's chunks (sequences earlier than the latest, same elevation as the latest). |
| `Elevation(n)` | Every already-published chunk of elevation `n` in this volume, including completed earlier sweeps of `n` if any (rare). |

`filter_backfill_sequences` resolves the candidate set against the
mapper. `cached_elevations_for_scan` then trims sweeps that are already
in IndexedDB — on a resume after a brief stop within the same volume,
the worker would treat re-flushes as no-ops, so the bandwidth would be
wasted. Each survivor is downloaded in parallel via `join_all` on
`download_chunk` and emitted in sequence order via `emit_backfill_chunks`.

The provisional scan-start timestamp for the volume is
`upload_date_time(start_chunk) − median_lag` — see
[TIMING.md §3c](TIMING.md#3c-the-scankey-provisional-timestamp).

### 2c. Steady state

```
loop {
    drain pending observations (collection time, lag) into iter

    if filter changed:
        run mid-stream backfill, advance epoch, clear per-chunk diagnostics

    predict wait until next chunk available
    sleep that wait + 750 ms pad (interruptible by stop / filter change)

    fetch (try_next), retrying on 404 / transient under REALTIME_CHUNK_POLICY
    if filter excludes everything remaining: synthesize volume end

    emit ChunkData + ChunkReceived (with arrival diagnostics)
    persist timing stats to localStorage
}
```

Per-chunk diagnostics (`cur_predicted_at`, `cur_last_empty_at`,
`cur_diagnostics`, `none_retries`) are captured on the **first**
iteration for a given chunk and reset on success — subsequent retry
iterations re-enter the predict block with a near-zero wait, so without
the `is_first_iter_for_chunk` guard the original prediction would be
overwritten.

---

## 3. Predicting "next chunk available at"

There are two prediction paths in this codebase, answering related but
distinct questions. The streaming loop uses the **scheduler** path; the
timeline UI uses the **projector** path. Both are detailed in
[TIMING.md §2 and §3](TIMING.md). Briefly:

| Path | Function | What it returns | When the loop uses it |
| --- | --- | --- | --- |
| Scheduler (single-hop) | [`estimate_chunk_processing_diagnostics`](../src/nexrad/timing/estimate_next_chunk_time.rs) → `iter.time_until_next()` | Duration until the **next** chunk's S3 availability. | `StreamingFilter::All` |
| Scheduler (multi-hop) | [`estimate_chunk_processing_time_to_target`](../src/nexrad/timing/estimate_next_chunk_time.rs) → `iter.next_matching_chunk_diagnostics()` | Duration until the next chunk **matching the filter predicate**, summing physics across every skipped hop. | `StreamingFilter::Elevation(_)` |
| Cross-volume | `iter.time_until_next_filtered_chunk_across_volumes()` | Duration until the user's elevation reappears in the **next volume**: projected end-of-current-volume + 8.5 s inter-volume gap + intra-next-volume hops + median lag. | Filter excludes everything remaining in the current volume. |
| Projector | [`project_scan_timing`](../src/nexrad/timing/scan_timing_projection.rs) | Per-chunk collection + availability times for **all remaining chunks**. | UI (timeline placeholders, forecast modal, `ChunkProjectionInfo`). |

### 3a. Scheduler decision tree

`estimate_chunk_processing_diagnostics`:

1. **Start chunk** → constant 1.5 s (`START_TO_FIRST_INTERMEDIATE_GAP_SECS`).
   Tagged `SchedulerPath::StartConstant`. The Start chunk lands almost
   immediately, the first M chunk follows by a measured 1.5 s.
2. **Have metadata for both chunks** → call the shared
   [`estimate_interval`](../src/nexrad/timing/interval_estimate.rs)
   primitive, which returns a 70/30 physics/historical blend when stats
   are available for the bucket and pure physics otherwise. The
   scheduler then adds a `(avg_attempts − 1) seconds` retry budget when
   stats are present (if past chunks of this bucket usually took 2
   polls, allocate one extra second so we are ready to retry without
   sleeping). Tagged `Blended` when stats contributed, `Physics`
   otherwise.
3. **No metadata** (very rare; e.g. malformed VCP) → static legacy table
   keyed by waveform/channel-config. Tagged `Legacy`.

The bucket lookup is keyed on the **arriving** chunk's characteristics,
not the anchor's. Writes (`StreamingState::update_timing_stats`) record
under the arriving chunk's bucket; reading under the anchor's would
silently miss and fall back to physics. See `chunk_timing_stats.rs` and
the fixed-bug commentary in TIMING.md §2.

The shared [`estimate_interval`](../src/nexrad/timing/interval_estimate.rs)
primitive is also used by the projector (§3 below) and the multi-hop
filter path (§3b), so all three predict-the-interval call sites apply
the same blend formula and a regression in one is observable in all.

### 3b. Multi-hop diagnostics (filter mode)

When the user has selected a single elevation, the loop's "next chunk"
is rarely sequence `current+1`. `next_matching_chunk_diagnostics`:

1. Walks the mapper for the next sequence whose elevation matches the
   filter predicate.
2. Sums `estimate_interval(prev, next, bucket, stats).seconds` for
   every hop from `current+1` up to `target` — every hop uses the same
   blended primitive as the single-hop scheduler, so each step is
   either pure physics (no stats yet) or a 70/30 physics/historical
   blend.
3. Adds `(avg_attempts − 1) seconds` retry budget on top, keyed on the
   target chunk's bucket.
4. Returns `(target_sequence, EstimatedChunkProcessing)` with `path =
   Blended` when any hop pulled in historical samples, else `Physics`.

This collapses what would otherwise be N successive predict-sleep-fetch
cycles (for chunks the user does not want) into a single sleep of
`Σ blended_intervals + retry_budget`.

### 3c. Cross-volume forecasting

When the filter excludes every remaining sequence in this volume — for
example the user picked elevation 3 and we are mid-way through the last
sweep at elevation 14 — the multi-hop diagnostic returns `None`. The
loop falls through to `time_until_next_filtered_chunk_across_volumes`,
which adds:

```
projected_volume_end_collection_secs                  (from the projector)
+ ChunkTimingModel::inter_volume_gap_secs (8.5 s)
+ start_to_first_intermediate_gap_secs (1.5 s)         (if target > 1)
+ Σ physics from sequence 2 up to (target − 1)         (assuming same VCP)
+ median_availability_lag_secs (or 5 s default)
```

The next-volume VCP assumption is fine in practice — VCP changes
mid-stream are rare, and if one happens the estimate is just revised on
the next iteration when the new mapper takes over. Without this, the
loop would fall back to the legacy single-hop estimate, burn through
its retry budget polling for a chunk that physically cannot exist for
another 60 s, then fail and give up.

When the cross-volume target is reached without an actual End chunk,
the loop emits a synthetic `is_volume_end` `ChunkReceived` so the
timeline still draws the volume boundary — see §5b.

---

## 4. Waiting and polling

### 4a. The first-poll pad

The scheduler returns a duration based on either historical S3 deltas
or pure physics. Before sleeping, the loop adds a 750 ms pad
(`POLL_DELAY_AFTER_PREDICTED_MS`):

```rust
let mut wait_ms = wait_duration.as_millis() as u32;
if is_first_iter_for_chunk && wait_ms > 0 {
    wait_ms = wait_ms.saturating_add(POLL_DELAY_AFTER_PREDICTED_MS);
}
```

Sized from observed prediction error: the scheduler's collection-space
prediction is accurate, but the residual is S3 availability lag the
projector under-estimates (~900 ms early in availability-space, with
empty-poll wait clustering tightly at ~670 ms). 750 ms covers the
typical case while leaving outliers (one-chunk lag spikes) to the retry
path. The pad applies **only to the prediction-driven first poll**, not
to retry waits, so each chunk eats it exactly once.

### 4b. `interruptible_sleep`

Sleeping is broken into 250 ms increments so the loop can:

- Wake immediately on stop (`state.stop_requested`).
- Wake on filter change (`state.filter_epoch != wake_epoch`) and
  re-evaluate without finishing the now-stale sleep.
- Update the user-facing countdown (`state.time_until_next`) in tight
  enough increments that the UI feels live.

`SleepOutcome::FilterChanged` causes the loop to `continue` back to the
top, where the filter-change branch runs the mid-stream backfill before
re-targeting.

### 4c. Retry policy

After the first prediction-driven attempt the loop enters an inline
retry loop driven by [`REALTIME_CHUNK_POLICY`](../src/net/retry.rs):

| Knob | Value | Why |
| --- | --- | --- |
| `base` | 500 ms | Real-time chunks land seconds late, not milliseconds. |
| `cap` | 4 s | Upper bound on the per-retry jitter window. |
| `max_attempts` | 6 | Roughly matches the prior 25×500 ms + 2.5 s grace. |
| `total_budget` | 15 s | Wall-clock fallback even if `max_attempts` would allow more. |
| `per_attempt_timeout` | 5 s | A 404 round-trip is ~1 s; a stuck request gets cancelled. |

Backoff is full-jitter exponential: the delay before retry N is drawn
from `random_uniform(0, min(cap, base * 2^(N-1)))`. The retry loop is
inlined (rather than going through `with_retry`) because each attempt
borrows `iter` mutably and the resulting future cannot escape an
`FnMut` closure body.

`Verdict::Retry` is returned for `Ok(None)` (S3 404 — chunk not yet
published) and for transport-layer errors. Decoding errors and AWS
identifier errors are `Verdict::Terminal` and abort the stream.

`SyntheticVolumeEnd` from the filter-aware fetch path is **not** a
retry — it is `Verdict::Ok(FilterFetchResult::SyntheticEnd)` which
breaks the retry loop and is then unwrapped to the synthetic-end emit
branch.

### 4d. The loop never sleeps blindly into the future

Two safeguards:

- The cross-volume estimator (§3c) ensures the wait is sized for the
  **next-volume's matching chunk**, not the next sequential chunk that
  may be 60 s away.
- If `time_until_next_opt` is `None` the loop skips the sleep and goes
  straight to fetch — this is what catches the case where the
  prediction says "now or earlier".

---

## 5. Boundary cases

### 5a. Volume rollover

`StreamingState::try_next` checks `current_sequence == final_sequence`
and, if so, calls `try_fetch_volume_start(volume.next())`:

1. List chunks in the next volume.
2. **Always emit the Start chunk first**, even if several Intermediates
   have already been published while we were sleeping. The worker's
   accumulator is cleared on the previous volume's End and only
   re-initializes when an `is_start: true` chunk arrives — emitting
   anything else first errors with "No accumulator — missing Start
   chunk?".
3. Subsequent `try_next` iterations fetch chunks 2, 3, … in normal
   sequence order.

On Start chunk receipt the loop also:

- Resets `chunks_in_volume` to 0.
- Re-derives `current_scan_start_secs = upload − median_lag`.
- Caches the volume number to localStorage (used as a hint by future
  acquire calls — currently disabled but the cache is kept warm).
- Clears `emitted_sequences_this_volume` (the dedup set used by the
  mid-stream backfill).
- Clears `iter.latest_chunk_collection_end_secs` so the next chunk's
  collection time becomes the new anchor.

### 5b. Synthetic volume end

Filter mode + every remaining sequence excluded =
`TryNextOutcome::SyntheticVolumeEnd`. Behavior:

1. `StreamingState::advance_current_to_synthetic_end` advances
   `current` to `final_sequence` with type `End` (no fetch).
2. The streaming loop emits a `ChunkReceived { is_volume_end: true,
   time_until_next: <cross-volume estimate>, … }`. **No `ChunkData`** —
   nothing was downloaded.
3. The loop's per-chunk tracking resets and the next iteration enters
   `try_fetch_volume_start` like any other rollover.

Without the cross-volume `time_until_next` estimate the UI would stick
on "Streaming…" for the entire inter-volume gap because
`time_until_next: None` reads as "no countdown available".

### 5c. Filter changes mid-stream

`RealtimeChannel::sync_filter` is called once per egui frame from the
update loop. It maps the user's `ElevationSelection` into a
`StreamingFilter` and calls `set_filter`, which short-circuits on equal
values; on a real change it:

- Stores the new filter in `pending_filter`.
- Bumps `filter_epoch` (wrapping `u64`).

`interruptible_sleep` wakes within ≤250 ms via the
`filter_epoch != wake_epoch` check. The streaming loop's filter-change
branch then:

1. Discards the per-chunk diagnostics (`cur_predicted_at`,
   `cur_last_empty_at`, `cur_diagnostics`, `none_retries`) — they were
   aimed at the previous target sequence.
2. Runs `run_mid_stream_backfill` to download already-published chunks
   of the new elevation (consulting `cached_elevations_for_scan` to
   avoid re-downloading what is already in IDB).
3. Adopts the new filter and epoch.

The mid-stream backfill skips any sequence in
`emitted_sequences_this_volume` so a flip-flop "elev 3 → elev 5 → elev
3" within one volume does not double-download elevation 3's chunks.

### 5d. Stop

`stop_requested` is checked at three sites: top of every loop
iteration, top of every retry attempt, and inside
`interruptible_sleep`. The loop exits cleanly within ~250 ms of the
flag being set.

---

## 6. Learning from arrivals

Every successful chunk fetch updates `ChunkTimingStats` in two places:

1. **`StreamingState::update_timing_stats`** — called inside
   `try_fetch_chunk` and `try_fetch_volume_start`. Records the S3
   `Last-Modified` delta (`upload − previous_upload`) and the attempt
   count (1 here; the retry loop tracks attempts separately and records
   them via the path described next), keyed on the arriving chunk's
   characteristics.

2. **`StreamingState::record_availability_lag_for_current`** — called
   from the loop's `drain_pending_ingest_observations` after `main.rs`
   pushes a freshly-parsed lag (S3 upload − latest radial collection
   time) following each worker ingest. Attaches the lag to the most
   recent sample of the current chunk's bucket.

Each `ChunkCharacteristics` bucket holds the last 10 samples
(`MAX_TIMING_SAMPLES`). The bucket key is
`(chunk_type, waveform_type, channel_configuration, is_first_in_sweep)` —
keeping `is_first_in_sweep` separate is essential because
first-chunks-in-sweep carry the inter-sweep transition penalty and
mixing them with intra-sweep samples prevents the rolling average from
converging on either value.

After every emit the loop calls `save_timing_stats(&site_id, …)` which
writes the bucket map to `nexrad_timing_stats_<SITE>` in localStorage.
Schema is `version: 2`; v1 payloads (which lacked the
`availability_lag_ms` field) are dropped on load. On the next session
`load_cached_timing_stats` reads it back so the very first prediction
starts on warm stats.

---

## 7. Observations pushed in from `main.rs`

Two observations the streaming loop cannot compute itself, because they
require the worker to have decoded the chunk's radials:

| Observation | Source | Setter | Used by |
| --- | --- | --- | --- |
| `chunk_max_time_secs` | Latest radial collection time in the chunk | `RealtimeChannel::record_chunk_collection_end_secs` → `StreamingState::record_chunk_collection_end_secs` | Anchor for `project_scan_timing` (§3) |
| `lag_secs = s3_last_modified − chunk_max_time` | Computed in `main.rs` from the matched arrival stat | `RealtimeChannel::record_availability_lag_secs` → `StreamingState::record_availability_lag_for_current` | Most recent bucket sample's `availability_lag` field |

Both go through `RealtimeState`'s `pending_*` fields and are drained
into `iter` at the top of each streaming loop iteration. The collection
anchor is also reset to `None` on every Start chunk (new volume) so a
fresh anchor lands on the first M chunk.

The `pending_*` fields are not a queue; if `main.rs` pushes twice
before the loop drains, the second value wins. This matches reality —
only the most recent chunk's collection time matters for the next
prediction.

---

## 8. Output: what the loop emits

Two `RealtimeResult` variants per chunk (in this order):

- **`ChunkData`** — raw bytes for the worker to ingest. Carries
  `is_start`, `is_end`, `timestamp` (provisional scan-start, used as
  IDB key), and `is_last_in_sweep` (resolved from the mapper at emission
  time so the worker can flush mid-sweep without waiting for the next
  sweep's first chunk — important under filter mode where that next
  chunk may never arrive).
- **`ChunkReceived`** — UI status update. Carries `chunks_in_volume`,
  `time_until_next` (the next prediction, fed to the countdown UI),
  `is_volume_end`, the projected volume-end times (collection +
  availability), the full per-chunk projection list (built by
  `build_chunk_projections`), and an `arrival_stat` bundle with
  per-chunk diagnostics (predicted vs actual, scheduler path used,
  bucket sample count at prediction time, physics breakdown, anchor
  source).

Init backfill chunks emit the same pair but with `time_until_next: None`
and `arrival_stat: None` — they were not predicted, just pulled from
the historical chunk list.

---

## 9. Where to look when X is wrong

- **Loop doesn't start** — check `acquire_streaming_state` against the
  10 s `ACQUIRE_TIMEOUT_SECS`. Most often the site's volume bucket has
  not been published yet.
- **"Next in N s" stays at the same N** — `interruptible_sleep` is not
  updating `state.time_until_next`. Likely the loop fell off into an
  unguarded `await` (any new code added inside the streaming task must
  pass through `interruptible_sleep` for cancellation to work).
- **First chunk of each volume always burns the retry budget** —
  scheduler is taking the `Legacy` path because there are no historical
  samples yet for the Start chunk's bucket. Expected on cold start; if
  it persists across sessions, check `save_timing_stats`/
  `load_cached_timing_stats` for the site.
- **Filter change does nothing** — `RealtimeChannel::set_filter` is
  short-circuiting against the existing `pending_filter`, or
  `interruptible_sleep` is not reading `filter_epoch`. Both checks are
  cheap; add a log to confirm `set_filter` is being called.
- **Loop polls forever after a synthetic volume end** — the cross-
  volume estimate returned `None` and the loop fell through to the
  legacy single-hop. Check `projected_volume_end_collection_secs` —
  cold-start with no projection means the estimator can't run.
- **`Blended` path never fires** — bucket lookup is keyed wrong.
  `get_average_timing` keys on the **arriving** chunk's
  characteristics, and `update_timing_stats` writes under the same
  key. If a regression flips one of them to the anchor's
  characteristics, the lookup will silently miss every time. See the
  bucket-lookup comment in `estimate_next_chunk_time.rs` near the
  `estimate_interval` call.
- **Predictions are systematically early or late** — inspect the
  diagnostics modal for path mix and `physics_breakdown` per chunk.
  Systematic bias on a specific waveform transition usually means
  `waveform_transition_penalty_secs` needs a bump for that pair (see
  the table in `chunk_timing_model.rs` line 253-281).
- **Resumes within a volume re-download cached sweeps** —
  `cached_elevations_for_scan` returned empty. Check the IDB
  `scan_index` entry for that scan and confirm `cached_sweeps` is
  populated. Partial sweeps don't appear there by design.
