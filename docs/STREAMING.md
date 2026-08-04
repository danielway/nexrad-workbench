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
| Channel | [`RealtimeChannel`](../src/nexrad/live/realtime/mod.rs) | The handle the egui update loop talks to. Owns three typed `futures_channel::mpsc` queues + an `active: Rc<Cell<bool>>` flag, and spawns the streaming task. Holds no shared mutable loop state. |
| Async task | `streaming_loop` (in [`live/realtime/streaming/`](../src/nexrad/live/realtime/streaming/)) | The long-running future. One per active site. Owns the iteration, sleeping, polling, emit — and its own `LoopState` (stop flag, active filter, filter epoch) as local variables. |
| Iterator state | [`StreamingState`](../src/nexrad/live/streaming_state.rs) | Replaces `nexrad_data`'s `ChunkIterator`. Holds the current `ChunkIdentifier`, the VCP, and the elevation/chunk mapper. |
| Projection engine | [`core/projection/`](../src/core/projection/) | Single owner of forward-looking timing. The loop feeds it arrivals, listings, observations, and the filter; each iteration it emits one [`StreamingPlan`](../src/core/streaming_plan.rs) that the sleep target, the UI countdown, and the diagnostics all read. |
| Timing primitives | [`core/timing/`](../src/core/timing/) | Pure physics/statistics functions over `(chunk metadata, vcp, mapper, stats)` — the interval blend, projections, tuning knobs — composed by the projection engine. |

The async task drives the streaming. The channels are just typed mailboxes
between it and the UI thread.

### Communication shape

Three typed channels (all replaced on each `start()` so a winding-down previous
loop can't leak messages into the new session) plus an `active` flag:

```
                 results (loop → UI)
egui frame ◀──── try_recv ──────────┐
    │                               │ push RealtimeResult
    │  observe(ProjectorObservation)│
    ├──── observations (UI → loop) ─┼──▶ streaming_loop
    │     (collection-end, lag)     │      │
    │  set_filter / stop            │   predict ─ sleep ─ fetch ─ emit
    └──── control (UI → loop) ──────┘      │  (drains observations + control
          (SetFilter, Stop)                │   each iteration & inside sleep)
                                           ▼
                                   active: Rc<Cell<bool>>
                                   (set by start(), cleared on exit/stop)
```

- **results** (loop → UI): every [`RealtimeResult`] the loop produces; the UI
  drains them with `try_recv` / `poll` each frame.
- **observations** (UI → loop): projector hints the UI gathers from worker
  results — `ProjectorObservation::{CollectionEndSecs, AvailabilityLagSecs}` — sent
  via `RealtimeChannel::observe` (and the `record_*` convenience wrappers).
- **control** (UI → loop): `ControlMessage::{SetFilter, Stop}`, sent via
  `set_filter` / `sync_filter` / `stop`. `drain_control` applies them into the
  loop's local `LoopState`, de-duping no-op filter changes and bumping a
  `filter_epoch` on real ones.

`drain_control` and the observation drain both run at the top of each iteration
*and* inside the sleep loop, so a filter swap or stop interrupts a long wait
instead of waiting for the current chunk to land. Because the control/filter
state is loop-local (not an `Rc<RefCell<_>>` shared cell), the loop keeps an
exclusive borrow of `iter` throughout.

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

The scan-start timestamp for the volume is parsed from the Start
chunk's volume header (`volume_header_start_secs`; a chunk with no
readable header is a fatal stream error) — see TIMING.md §3c.

### 2c. Steady state

```
loop {
    drain control (stop / SetFilter) + observations (collection time, lag) into
        LoopState + iter

    if stop requested: break
    if filter changed:
        run mid-stream backfill, advance epoch, clear per-chunk diagnostics

    build the canonical StreamingPlan from the projection engine
    sleep to the plan's next-target poll_at (750 ms poll bias + retry
        budget already folded in; interruptible by stop / filter change)

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

All forward-looking timing now comes from **one computation**: each
iteration the projection engine builds a [`StreamingPlan`](../src/core/streaming_plan.rs)
(via the projector kernel's `project_scan_timing_with_next`,
[`core/timing/scan_timing_projection.rs`](../src/core/timing/scan_timing_projection.rs)) —
per-chunk collection / availability / poll times for every remaining
chunk. The loop sleeps to the plan's `next_target().poll_at_secs`; the
UI countdown and diagnostics read the same plan, so they cannot drift
apart. (Historically the scheduler and the UI projector were separate
paths — `time_until_next()` vs. `project_remaining_scan` — that did
drift; the plan replaced both.) The cases the old paths handled are
now facets of the one plan:

| Case | How the plan handles it | When it applies |
| --- | --- | --- |
| Next sequential chunk | The next target is `current + 1` in the current volume. | `StreamingFilter::All` |
| Filtered target (multi-hop) | The next target is the next sequence whose elevation matches the filter; the projection sums the same blended intervals across every skipped hop. | `StreamingFilter::Elevation(_)` |
| Cross-volume | When the filter excludes everything remaining, the projection extends into the **next volume** (`next_volume_chunks`, `next_target_in_next_volume()`): projected end-of-current-volume + 8.5 s inter-volume gap + intra-next-volume hops + lag. | Filter excludes the rest of the current volume. |
| Timeline placeholders | The same plan's `ChunkProjectionInfo` list feeds the timeline placeholders and the forecast modal. | Always. |

The semantic time categories behind these numbers are detailed in
[TIMING.md §2 and §3](TIMING.md).

### 3a. The per-hop interval decision

For each projected hop (tagged per chunk on the plan as
`SchedulerPath`, [`core/timing/estimate_next_chunk_time.rs`](../src/core/timing/estimate_next_chunk_time.rs)):

1. **Start chunk** → constant 1.5 s (`START_TO_FIRST_INTERMEDIATE_GAP_SECS`).
   Tagged `SchedulerPath::StartConstant`. The Start chunk lands almost
   immediately, the first M chunk follows by a measured 1.5 s.
2. **Have metadata for both chunks** → call the shared
   [`estimate_interval`](../src/core/timing/interval_estimate.rs)
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
not the anchor's. Writes (`ProjectionEngine::record_inter_chunk_duration`)
record under the arriving chunk's bucket; reading under the anchor's
would silently miss and fall back to physics. See
`chunk_timing_stats.rs` and the fixed-bug commentary in TIMING.md §2.

The shared [`estimate_interval`](../src/core/timing/interval_estimate.rs)
primitive backs every hop the plan projects — single-hop, multi-hop,
and cross-volume alike — so all interval predictions apply the same
blend formula and a regression in one is observable in all.

### 3b. Multi-hop targets (filter mode)

When the user has selected a single elevation, the loop's "next chunk"
is rarely sequence `current+1`. The plan's target selection:

1. Walks the mapper for the next sequence whose elevation matches the
   filter predicate; that becomes `next_target_key`.
2. The projection sums `estimate_interval(prev, next, bucket, stats)`
   for every hop from `current+1` up to the target — every hop uses
   the same blended primitive, so each step is either pure physics (no
   stats yet) or a 70/30 physics/historical blend.
3. The target's poll time additionally carries the
   `(avg_attempts − 1) seconds` retry budget, keyed on the target
   chunk's bucket.
4. The target's `scheduler_path` reads `Blended` when historical
   samples contributed, else `Physics`.

This collapses what would otherwise be N successive predict-sleep-fetch
cycles (for chunks the user does not want) into a single sleep of
`Σ blended_intervals + retry_budget`.

### 3c. Cross-volume forecasting

When the filter excludes every remaining sequence in this volume — for
example the user picked elevation 3 and we are mid-way through the last
sweep at elevation 14 — the plan extends into the next volume
(`next_volume_chunks`), and the cross-volume target's timing sums:

```
projected end of current volume                        (from the projection)
+ INTER_VOLUME_GAP_SECS (8.5 s)
+ START_TO_FIRST_INTERMEDIATE_GAP_SECS (1.5 s)         (if target > 1)
+ Σ intervals from sequence 2 up to (target − 1)       (assuming same VCP)
+ availability lag (median, or the 5 s cold-start default)
```

The next-volume VCP assumption is fine in practice — VCP changes
mid-stream are rare, and if one happens the estimate is just revised on
the next iteration when the new mapper takes over. Without this, the
loop would sleep for the next *sequential* chunk, burn through its
retry budget polling for a chunk the filter will discard anyway, and
stall the countdown for the entire inter-volume gap.

When the cross-volume target is reached without an actual End chunk,
the loop emits a synthetic `is_volume_end` `ChunkReceived` so the
timeline still draws the volume boundary — see §5b.

---

## 4. Waiting and polling

### 4a. The first-poll bias

The projector folds a 750 ms bias into each forecast's poll target
(`poll_at_secs = available_at_secs + retry_budget + poll_bias_secs`,
[`interval_estimate.rs`](../src/core/timing/interval_estimate.rs);
the value is `TimingTuning::DEFAULT.poll_bias_secs` in
[`config.rs`](../src/core/timing/config.rs)). The loop sleeps directly
to `poll_at_secs` — there is no loop-side pad constant.

Sized from observed prediction error: the collection-space prediction
is accurate, but the residual is S3 availability lag the projector
under-estimates (~900 ms early in availability-space, with empty-poll
wait clustering tightly at ~670 ms). 750 ms covers the typical case
while leaving outliers (one-chunk lag spikes) to the retry path. The
bias applies **only to the prediction-driven first poll**, not to
retry waits, so each chunk eats it exactly once.

### 4b. `interruptible_sleep`

Sleeping is broken into 250 ms increments. On each increment the loop drains the
control channel into its `LoopState`, so it can:

- Wake immediately on stop (`loop_state.stop_requested`, set when a `Stop`
  control message drains in).
- Wake on filter change (`loop_state.filter_epoch != wake_epoch`, bumped when a
  `SetFilter` control message drains in) and re-evaluate without finishing the
  now-stale sleep.
- Refresh the user-facing countdown in increments tight enough that the UI feels
  live (the resolved wait is reported back via `WaitResolution`).

`SleepOutcome::FilterChanged` causes the loop to `continue` back to the
top, where the filter-change branch runs the mid-stream backfill before
re-targeting.

### 4c. Retry policy

After the first prediction-driven attempt the loop enters an inline
retry loop driven by [`REALTIME_CHUNK_POLICY`](../src/net/retry.rs):

| Knob | Value | Why |
| --- | --- | --- |
| `base` | 500 ms | Real-time chunks land seconds late, not milliseconds. |
| `cap` | 8 s | Lets a single backoff sleep span the upper tail of inter-volume gap variance (observed 7–10 s) without spinning through wasted 404s. |
| `max_attempts` | 8 | Sized for robustness against prediction error. |
| `total_budget` | 45 s | Leaves ~30 s of sleep budget after worst-case per-attempt waits; a genuinely failing endpoint takes longer to surface, which is the right trade for a live viewer. |
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
- Re-derives `current_scan_start_secs` from the new Start chunk's
  volume header (`volume_header_start_secs`).
- Caches the volume number to localStorage (used as a hint by future
  acquire calls — currently disabled but the cache is kept warm).
- Clears `emitted_sequences_this_volume` (the dedup set used by the
  mid-stream backfill).
- Resets the engine's collection anchor so the next chunk's collection
  time becomes the new anchor.

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
`StreamingFilter` and calls `set_filter`, which sends a
`ControlMessage::SetFilter` down the control channel (cheap to send every
frame). When `drain_control` applies it, it short-circuits on a value equal
to the loop's current `active_filter`; on a real change it:

- Adopts the new filter as `loop_state.active_filter`.
- Bumps `loop_state.filter_epoch` (wrapping `u64`).

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

`RealtimeChannel::stop()` sends a `ControlMessage::Stop` down the control
channel and eagerly clears the `active` flag so `is_active()` reflects the
user's intent immediately. When `drain_control` applies the message it sets
`loop_state.stop_requested`, which is checked at three sites: top of every
loop iteration, top of every retry attempt, and inside
`interruptible_sleep`. The loop exits cleanly within ~250 ms of the stop
message and clears `active` again on the way out as a backstop.

---

## 6. Learning from arrivals

Every successful chunk fetch updates the engine-owned
`ChunkTimingStats` in two places:

1. **`ProjectionEngine::record_inter_chunk_duration`** — called by the
   loop for each arriving chunk. Records the S3 `Last-Modified` delta
   (`upload − previous_upload`) and the attempt count, keyed on the
   arriving chunk's characteristics.

2. **`ProjectionEngine::record_availability_lag_for`** — applied when
   the loop drains a `ProjectorObservation::AvailabilityLagSecs`
   (pushed after each worker ingest: S3 upload − latest radial
   collection time). Attaches the lag to the most recent sample of the
   current chunk's bucket.

Each `ChunkCharacteristics` bucket holds the last 10 samples
(`TimingTuning::max_timing_samples`). The bucket key is
`(chunk_type, waveform_type, channel_configuration, is_first_in_sweep)` —
keeping `is_first_in_sweep` separate is essential because
first-chunks-in-sweep carry the inter-sweep transition penalty and
mixing them with intra-sweep samples prevents the rolling average from
converging on either value.

After every emit the loop calls `save_timing_stats(&site_id, …)` which
writes the bucket map to `nexrad_timing_stats_<SITE>` in localStorage.
The payload carries a schema version (`PERSIST_SCHEMA_VERSION`,
currently 3); payloads with a mismatched version are dropped on load.
On the next session `load_cached_timing_stats` reads it back so the
very first prediction starts on warm stats.

---

## 7. Observations pushed in from the main thread

Two observations the streaming loop cannot compute itself, because they
require the worker to have decoded the chunk's radials. The UI sends them
down the **observations channel** as `ProjectorObservation` variants:

| Observation | Source | Sent via | Used by |
| --- | --- | --- | --- |
| `CollectionEndSecs` — latest radial collection time in the chunk | Worker-decoded radials | `RealtimeChannel::record_chunk_collection_end_secs` → `observe` → `ProjectionEngine::set_collection_anchor` | Anchor for the plan's collection axis (§3) |
| `AvailabilityLagSecs` — `s3_last_modified − chunk_max_time` | Computed by the chunk-ingest reducer (`core::worker_ingest`) from the matched arrival stat | `RealtimeChannel::record_availability_lag_secs` → `observe` → `ProjectionEngine::record_availability_lag_for` | Most recent bucket sample's `availability_lag` field |

The loop drains the observations channel into the projection engine at
the top of each iteration (and inside the sleep). The collection anchor
is also reset on every Start chunk (new volume) so a fresh anchor lands
on the first M chunk.

The channel is unbounded and drained in order, so if `main.rs` pushes two
collection times before the loop drains, both are applied and the later one
wins — which matches reality: only the most recent chunk's collection time
matters for the next prediction. Adding a new observation kind is just a new
`ProjectorObservation` variant plus a drain-dispatch arm — no new channel or
state field.

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
  `is_volume_end`, `fetch_latency_ms`, a `plan: Option<StreamingPlan>`, and an
  `arrival_stat: Option<ChunkArrivalStat>`. The `plan` is the single canonical
  forward-looking projection consumed by the timeline countdown, the in-progress
  sweep rendering, and the next-scan ghost; it replaces an older bag of
  `time_until_next` + projected volume-end times + per-chunk projection lists
  that used to drift apart and let the UI countdown disagree with the loop's
  actual sleep. `arrival_stat` is the per-chunk diagnostic bundle (predicted vs
  actual, scheduler path used, bucket sample count at prediction time, physics
  breakdown, anchor source).

Init backfill chunks emit the same pair but with `plan: None` and
`arrival_stat: None` — they were not predicted, just pulled from the historical
chunk list.

---

## 9. Where to look when X is wrong

- **Loop doesn't start** — check `acquire_streaming_state` against the
  10 s `ACQUIRE_TIMEOUT_SECS`. Most often the site's volume bucket has
  not been published yet.
- **"Next in N s" stays at the same N** — the emitted plan's countdown isn't
  being refreshed (`interruptible_sleep`'s `WaitResolution` isn't feeding the
  next `ChunkReceived.plan`). Likely the loop fell off into an unguarded `await`
  (any new code added inside the streaming task must pass through
  `interruptible_sleep` for cancellation to work).
- **First chunk of each volume always burns the retry budget** —
  scheduler is taking the `Legacy` path because there are no historical
  samples yet for the Start chunk's bucket. Expected on cold start; if
  it persists across sessions, check `save_timing_stats`/
  `load_cached_timing_stats` for the site.
- **Filter change does nothing** — the `SetFilter` control message is being
  de-duped in `drain_control` against the loop's current `active_filter`, or the
  sleep isn't draining control / reading `filter_epoch`. Add a log in
  `drain_control` to confirm the message arrives.
- **Loop polls forever after a synthetic volume end** — the plan has
  no next-volume extension (`next_volume_chunks` is `None`), so there
  is no cross-volume target. Check the projected end of the current
  volume — cold-start with no projection means the extension can't be
  built.
- **`Blended` path never fires** — bucket lookup is keyed wrong.
  `estimate_interval` reads the **arriving** chunk's characteristics,
  and `record_inter_chunk_duration` writes under the same key. If a
  regression flips one of them to the anchor's characteristics, the
  lookup will silently miss every time. See the bucket-lookup comment
  in `estimate_next_chunk_time.rs` near the `estimate_interval` call.
- **Predictions are systematically early or late** — inspect the
  diagnostics modal for path mix and `physics_breakdown` per chunk.
  Systematic bias on a specific waveform transition usually means
  `waveform_transition_penalty_secs` needs a bump for that pair (see
  the table in `chunk_timing_model.rs`).
- **Resumes within a volume re-download cached sweeps** —
  `cached_elevations_for_scan` returned empty. Check the IDB
  `scan_index` entry for that scan and confirm `cached_sweeps` is
  populated. Partial sweeps don't appear there by design.
