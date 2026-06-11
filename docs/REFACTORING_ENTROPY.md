# Timeline / IDB Cache / Projection — Entropy-Reduction Refactor Plan

> **Status (2026-06-11):** All 14 planned workstreams are implemented and
> committed on `simplify-user-interface` (T4, T2, T1, T3, T5; I1, I2, I3,
> I4; P2, P1, P3, P4, P5 — see `git log` from "Add entropy-reduction
> refactor plan…"). Only **P6** (decompose `realtime/streaming.rs` into
> persistence/probe/backfill modules — pure moves) remains, as the optional
> stretch item. **Outstanding before building on this:** the manual
> `trunk serve` live-session smoke test below — T1 (playhead transitions),
> T2 (frame clock), P1 (collection-domain estimates, behavior-changing),
> P3 (volume rollover), P4 (diagnostics modals) have no automated UI
> coverage.

## Context

Three subsystems have accumulated entropy through feature development and bug fixes: the **timeline/playback** stack, the **IndexedDB cache**, and the **sweep/chunk projection & estimation** pipeline. An architecture review (3 explorer passes + verification against code) found the bones are healthy — `TimelineView` as a per-frame adapter, the `write_tx` no-await enforcement, the recently-landed single-owner `ProjectionEngine` — but interfaces have rotted at the edges: implicit mode flags with 9-file write fan-out, asymmetric key parse APIs, quota thresholds in three places, a verified time-domain conflation in timing stats, dead "transition compatibility" state, and five words for one concept.

This plan defines **15 workstreams in 3 independent tracks**, each delegable as one session/PR. Everything is behavior-preserving **except P1** (deliberately flagged: it fixes the availability-vs-collection domain mixing, shifting estimate numbers — the known prerequisite for the planned projection-accuracy tuning work).

**Suggested step 0:** copy this plan into `docs/REFACTORING_ENTROPY.md` so implementing sessions can reference it (the repo already keeps refactor plans in `docs/`; this plan deliberately does NOT overlap `docs/REFACTORING_STRATEGIC.md` S1–S4 — see Non-goals).

## How to execute

- Tracks T (timeline), I (IDB), P (projection) touch nearly disjoint files — interleave freely; within a track, order is fixed.
- Per CLAUDE.md: commit per workstream milestone; `cargo check` + `cargo clippy -- -D warnings` + `cargo test --bin nexrad-workbench` must be clean (pre-commit runs these). `tests/idb.rs` (real IDB, Chromium) runs in CI — extend it for I-track items.
- **No UI automation exists.** Workstreams marked SMOKE need a manual `trunk serve` live-session check; archive-only workstreams are covered by the bin suite + visual spot-check.
- If trimming scope, the payload items are **T1, I1, P1**; the rest are enablers or cleanup.

## Architecture assessment

**Timeline/playback** — Healthy: `state/timeline_view.rs` is a genuinely good per-frame merge point (cache/shadows/live → one view; renderers read only it); `ui/transport.rs` already centralizes play/pause branching. Unhealthy: the playhead mode is encoded in two bools (`TimeModel.locked_to_realtime`, `PlaybackState.lookback_active`) whose transitions are duplicated with slight variations across **9 files** (`state/playback.rs`, `app/live_mode.rs`, `ui/transport.rs`, `ui/timeline/{interaction,now_edge}.rs`, `ui/playback_controls.rs`, `ui/shortcuts.rs`, `ui/mobile/{scrubber,tabs}.rs`); invariants live in convention only (`seek_to` silently no-ops under the realtime lock — `playback.rs:327`; `snap_to_now` exists solely to bypass that guard). A dead legacy-field chain survives ("Legacy fields maintained for compatibility during transition", `playback.rs:384-395`). Three uncoordinated per-frame `now` captures coexist with ~56 raw `js_sys::Date::now()` sites (intra-frame drift). Sub-renderers take 10–16 positional args and re-derive the same `ts_to_x` closure in ≥3 files.

**IndexedDB cache** — Healthy: `write_tx`'s `FnOnce` closure design makes the compiler reject `.await` inside readwrite transactions (the platform constraint, enforced); `logic.rs` as a pure tested decision layer; the 3-store design and `UpsertScanGuard` are deliberate and documented. Unhealthy: key strings are asymmetric (`ScanKey::from_storage_key` exists returning bare `Option`; `SweepDataKey` has no inverse at all); prefix-range logic is smeared across three layers (`keys.rs` type → `logic.rs` string bounds → `helpers.rs` IdbKeyRange wrap); quota thresholds live in three places with three units (5 MB headroom in `logic.rs`, 2 GB/80% in `state/settings.rs`, 10% browser fraction in `facade.rs`); `DataError` is mostly stringly so callers can't tell transient from permanent; `facade.check_and_evict` — the one function with real branching — has zero tests.

**Projection & timing** — Healthy: the recent single-owner `ProjectionEngine` refactor landed well (worker feeds `VolumeObservations` directly; consumers read the engine's `Projection`); the 4-tier provenance cascade (`status.rs::cascade_current_sweeps`) is the justified core of the feature. Unhealthy: **verified domain conflation** — `TimingStat.duration` samples are S3-upload→S3-upload deltas (AVAILABILITY domain, documented at `chunk_timing_stats.rs:92-99`) but `get_average_timing` feeds `interval_estimate.rs::estimate_interval`'s 0.7-physics/0.3-historical blend as a COLLECTION-interval proxy, and `project_times` adds the result to a collection anchor. Tuning constants are scattered across four files; the forward-looking vocabulary uses five words (`Projection`/`Forecast`/`Estimate`/`ChunkProjection`/`ScanTimingProjection`) plus a literal type-name collision (`SweepTiming` in both `data/keys.rs:548` and `state/vcp_forecast.rs:15`); volume-boundary engine calls are a 4-call cluster buried at `realtime/streaming.rs:1064-1071` inside a 1,846-line file.

## Facts the implementer should trust (verified, correcting first impressions)

- `decode_worker/receive.rs:276` uses `.unwrap_or_else(|| context.scan_key.clone())` (silent fallback), not `.expect()`. `worker_api/render.rs:24,186` already return proper errors.
- `read_touch` (`indexeddb/mod.rs:~575`) is an intentional `#[doc(hidden)]` test surface used by `tests/idb.rs` — **not** dead code.
- `clear_all` IS tested (`tests/idb.rs:461`). The real gaps: `facade.check_and_evict`, error paths.
- The legacy playback chain is fully dead: `total_frames` never written ⇒ `timestamp_to_frame` (`playback.rs:694`) always returns `Some(0)`; `current_frame` write-only (`interaction.rs:83`); `data_start/end_timestamp` (written `worker_results.rs:87-88`) feed only `data_duration_secs`, only called by `timestamp_to_frame`.
- `LiveModeState.last_completed_volume` is a justified seal-before-reset snapshot (`worker_results.rs:570-578` captures from engine observations immediately before the engine resets). P4 prunes dead fields and the modal's parallel derive path — not the snapshot.
- Collection anchor IS reset (`streaming.rs:1066`) — the problem is scatter, not a missing call.
- Timing-stats persistence already has clean schema invalidation (`PERSIST_SCHEMA_VERSION` in `chunk_timing_stats.rs`); P1 just bumps it.
- CLAUDE.md is stale on IDB schema: code is `DATABASE_VERSION = 5` with three stores (`sweeps`, `scan_index`, `scan_touches`); doc says version 3, two stores. Fixed in I4.

---

## Track T — Timeline / Playback (order: T4 → T2 → T1 → T3; T5 anytime)

### T4 — Delete the dead legacy chain; make macro dirty-check testable (S, low risk)
**Goal:** Remove provably dead state; turn the `MacroPlaybackState` rebuild decision into a pure unit instead of inline `render_loop` logic.
**Design:**
- Delete `data_start_timestamp`, `data_end_timestamp`, `current_frame`, `total_frames`, `timestamp_to_frame`, `data_duration_secs` (`state/playback.rs:384-395, 685-706`) plus write sites `app/worker_results.rs:87-88`, `ui/timeline/interaction.rs:82-84`.
- Replace the four `cached_*` dirty-check fields with one comparable input key; owner (`render_loop`) still rebuilds:
```rust
#[derive(PartialEq, Clone)]
pub struct MacroFrameInputs { pub elevation: ElevationSelection, pub bounds: Option<(f64, f64)>, pub scan_count: usize }
impl MacroPlaybackState {
    /// None = clean; Some(cause) → rebuild (cause distinguishes elevation change, which drives snap-to-frame).
    pub fn rebuild_cause(&self, inputs: &MacroFrameInputs) -> Option<RebuildCause>;
    pub fn store_rebuilt(&mut self, inputs: MacroFrameInputs, frames: Vec<f64>);
}
```
- Keep `cached_playback_position` (seek detection — different concern); rename `last_seen_position`.
**Files:** `src/state/playback.rs`, `src/app/render_loop.rs:24-77`, `src/app/worker_results.rs`, `src/ui/timeline/interaction.rs`.
**Accept:** grep deleted names → 0; bin tests for `rebuild_cause` (elevation vs bounds vs scan-count change; elevation change preserves snap-to-frame per `render_loop.rs:54-65`); clippy clean.

### T2 — FrameClock: one `now` per frame (M, low-med risk, SMOKE)
**Goal:** One wall-clock capture per `update()`; kill intra-frame drift across the three existing captures (`subsystem/derived.rs:55`, `subsystem/live.rs:98`, `ui/timeline/mod.rs:367`) and scattered `Date::now()` in UI paths.
**Design:** `#[derive(Clone, Copy)] pub struct FrameNow(pub f64);` captured in `app/frame_setup.rs::apply_frame_setup`, stored on `AppState`. Consumers switch: `tick_live` (`app/live_mode.rs:66`), staleness math (`frame_setup.rs:41`), `Derived::for_frame` (takes it as param), `Live::refresh`, `render_timeline`, canvas overlays, mobile scrubber. `TimeModel::wall_clock_time()` stays the single low-level accessor.
**Explicit exclusions** (event-time stamping / async tasks off the frame loop — leave alone): `nexrad/realtime/streaming.rs`, `nexrad/archive_index.rs`, `net/`, acquisition op timestamps, `state/saved_events.rs`, `state/errors.rs`, worker-side code.
**Files:** `src/app/frame_setup.rs`, `src/state/` (small new home for `FrameNow`), `src/subsystem/{derived,live}.rs`, `src/app/live_mode.rs`, `src/ui/timeline/mod.rs`, `src/ui/canvas_overlays/*`, `src/ui/mobile/scrubber.rs`.
**Accept:** `rg "Date::now" src/ui src/subsystem src/app` → capture site + documented exceptions only. SMOKE: live mode — now-line, countdown, staleness move smoothly together.

### T1 — Explicit playhead mode + single transition API (L, medium risk, SMOKE)
**Goal:** Replace the two mode bools and 9-file write fan-out with one enum + named transitions owning the invariants (bounds ownership, lock semantics, selection clearing). Delete the `seek_to` silent no-op / `snap_to_now` bypass pair.
**Design decision:** Do **not** fold `playing` or the live session bit into the enum — `live.mode_state.is_active()` is the stream-session flag (stream survives lookback enter/exit) and `playing` has clean meaning in archive. The playhead has exactly three modes:
```rust
pub enum PlayheadMode {
    Free,          // archive: free seeking; `playing` drives advance()
    PinnedToNow,   // live: per-frame tick pins position to wall clock; position writes require a transition out
    LookbackLoop,  // live replay: bounds owned by the tick's sliding window; always macro; invariant playing == true
}
impl PlaybackState {
    pub fn enter_pinned_live(&mut self, now: f64);
    pub fn exit_live(&mut self, freeze: FreezeAt);     // FreezeAt::Now | FreezeAt::Keep (seek/jog paths)
    pub fn enter_lookback(&mut self, seed: Option<f64>);
    pub fn exit_lookback_to_now(&mut self);
    pub fn seek(&mut self, ts: f64);                   // pure position write; debug_assert mode == Free
    pub fn pin_tick(&mut self, now: FrameNow);         // replaces snap_to_now; valid only in PinnedToNow
}
```
Mode enum lives on `TimeModel` (it governs position semantics); transitions are `PlaybackState` methods so they can also reset macro/selection state. Orchestration split: channel/engine start-stop and status messages stay in `app/live_mode.rs`, but its state mutations become single transition calls. All 9 write sites call transitions instead of poking fields. `seek_to`'s guard is deleted (every live-mode caller already exits live first — the API makes that explicit). `effective_playback_mode()` becomes `matches!(mode, LookbackLoop)` → Macro.
**Files:** `src/state/playback.rs`, `src/app/live_mode.rs`, `src/ui/transport.rs`, `src/ui/timeline/{interaction,now_edge}.rs`, `src/ui/playback_controls.rs`, `src/ui/shortcuts.rs:475`, `src/ui/mobile/{scrubber.rs:138,tabs.rs:148,240}`, `src/app/acquisition_intent.rs` (reads `lookback_active` → reads mode).
**Accept:** grep: no writes to mode/`playing` outside `state/playback.rs`. Bin tests assert the transition table: `enter_lookback` sets Loop + resets macro cursor + playing; `exit_lookback_to_now` clears bounds + re-pins; `exit_live(Keep)` preserves position; a shift-drag selection's bounds survive lookback exit. SMOKE: go live → play (lookback) → pause (back to now) → stop (freeze) → archive seek; seek-while-live exits live; datetime jump; Ctrl+L; mobile scrubber/tabs exits.

### T3 — `TimelineFrame` render context; isolate mutation (M, low risk)
**Goal:** Collapse 10–16-arg renderer signatures into one read-only frame context; make mutation sites explicit.
**Design:**
```rust
pub(super) struct TimelineFrame<'a> {
    pub view: &'a TimelineView<'a>,
    pub now_secs: f64,        // from FrameNow (T2)
    pub view_start: f64, pub zoom: f64,
    pub rects: TrackRects,    // full / tick / scan / sweep(Option)
    pub detail: DetailLevel, pub dark: bool,
}
impl TimelineFrame<'_> { fn ts_to_x(&self, ts: f64) -> f32; fn view_end(&self) -> f64; }
```
Sub-renderers become `fn render_x(painter, frame: &TimelineFrame, <≤3 extras>)` — e.g. `render_realtime_progress`'s 14 params (`overlays.rs:193-207`) → `(painter, frame, model, ctx, active/prev_active)`. Only `handle_timeline_interaction` and `now_edge` actions keep `&mut` refs; `render_timeline`'s sole write (`timeline_width_px`, `mod.rs:252`) stays explicit at top. Drop unused `_chrome` param (note: `derived` IS used — `mod.rs:407`).
**Files:** all of `src/ui/timeline/`.
**Accept:** child renderers read-only over the context; `#[allow(clippy::too_many_arguments)]` removed from timeline renderers; `rg "Date::now" src/ui/timeline` → 0; pixel-identical output (visual spot-check: macro + micro zoom, live overlay, tooltips).

### T5 — Unit hygiene: one sec→ms conversion (S, low risk, anytime)
**Goal:** Single definition of the seconds↔milliseconds coverage-key conversion.
**Design:** `impl UnixMillis { pub fn from_secs_f64(s: f64) -> Self { Self((s * 1000.0).round() as i64) } }` in `data/keys.rs`; use at `state/timeline_view.rs:152, 295, 305`; audit other `* 1000.0).round()` sites in state/ui.
**Accept:** grep `* 1000.0).round()` → keys.rs only; bin test agreeing with `UnixMillis::from_secs` for whole seconds.

## Track I — IndexedDB (order: I1 → I3 → I4; I2 anytime)

### I1 — Key symmetry + collapse range plumbing into `keys.rs` (M, low risk)
**Goal:** Symmetric parse/format for both key types with a real error type; range bounds become key-type methods.
**Design:**
```rust
// data/keys.rs
pub enum KeyParseError { WrongFieldCount { expected: usize, got: usize }, BadTimestamp(String), BadElevation(String) }
impl ScanKey {
    pub fn from_storage_key(s: &str) -> Result<Self, KeyParseError>;  // was Option
    pub fn idb_prefix_bounds(&self) -> (String, String);              // moved from logic.rs::scan_prefix_bounds
}
impl SweepDataKey { pub fn from_storage_key(s: &str) -> Result<Self, KeyParseError>; }  // NEW
impl SiteId { pub fn idb_prefix_bounds(&self) -> (String, String); }  // moved from logic.rs::site_prefix_bounds
```
`helpers.rs::{site,scan}_prefix_range` keep only the `IdbKeyRange` wrap; string math + its tests move from `logic.rs` to `keys.rs` (still bin suite — purity preserved). Edges: `receive.rs:276` keeps fallback but logs a warning on parse error (today silent); `worker_api/render.rs:24,186` map `KeyParseError` into their JsValue error; `indexeddb/mod.rs:634` skips + logs.
**Files:** `src/data/keys.rs`, `src/data/indexeddb/{logic,helpers,mod}.rs`, `src/nexrad/decode_worker/receive.rs`, `src/nexrad/worker_api/render.rs`.
**Accept:** round-trip bin tests both key types (valid/malformed/extra/missing fields — extend `test_scan_key_from_storage_key_invalid` family, `keys.rs:~970`); moved bounds tests pass unchanged; `rg "split('\|')" src --glob '!src/data/keys.rs'` → 0.

### I2 — Single `QuotaPolicy`; pure eviction decision (M, low risk)
**Goal:** All quota thresholds in one module; `check_and_evict` branching becomes a pure, tested function.
**Design:**
```rust
// data/quota.rs
pub struct QuotaPolicy {
    pub ingest_headroom_bytes: u64,     // 5 MB  (was logic.rs)
    pub browser_low_fraction: f64,      // 0.10  (was facade.rs:91)
    pub eviction_target_fraction: f64,  // 0.80  (was settings.rs:32)
}
// pure, in logic.rs or quota.rs:
// decide_eviction(current_size, app_quota, browser_estimate, &policy) -> EvictionDecision { evict_to: Option<u64>, warning: Option<QuotaWarning> }
```
`facade.check_and_evict` becomes read-sizes → `decide_eviction` → execute. User-configured quota value stays in `StorageSettings`; policy owns rules. Same numbers ⇒ behavior-preserving.
**Files:** new `src/data/quota.rs`, `src/data/indexeddb/{logic,mod}.rs`, `src/data/facade.rs`, `src/state/settings.rs`.
**Accept:** bin tests for the decision matrix (app-over only / browser-low only / both / neither / no estimate); one new `tests/idb.rs` orchestration test driving `check_and_evict` on a real DB (CI); thresholds defined in exactly one module (grep).

### I3 — `DataError` taxonomy: classification, not restructuring (S, low risk)
**Goal:** Callers can distinguish transient / permanent / quota without rewriting variants.
**Design:** Add `pub enum ErrorKind { Transient, Permanent, Quota }` + `DataError::kind()`. `js_err` returns structured `JsErrorInfo { name: Option<String>, message: String }` so DOMException names (`QuotaExceededError`, `AbortError`, `TimeoutError`) drive classification inside `TransactionFailed`/`RequestFailed`; add `DataError::KeyParse(KeyParseError)` (from I1). Mapping: `QuotaExceeded`→Quota; `NotOpen`/`ConcurrentUpsert`/abort/timeout→Transient; `NotFound`/`SerdeError`/`KeyParse`→Permanent. **No retry machinery** — classification + differentiated logging only.
**Files:** `src/data/indexeddb/{mod,helpers}.rs`, a few logging call sites.
**Accept:** bin tests for `kind()` incl. DOMException-name parsing; no caller behavior change beyond log levels.

### I4 — Dead code + stale docs (S, low risk)
**Design:** Delete `LiveVolumeAnchor::is_confirmed` (`keys.rs:218`) and `ExtractedVcp::sweep_start_offsets` (`keys.rs:726`) with their only-tests. **Keep `read_touch`** (test surface). Update CLAUDE.md IndexedDB section: version 5, three stores, one-sentence `scan_touches` rationale.
**Accept:** clippy clean with the `#[allow(dead_code)]` attributes removed (proves deadness); CLAUDE.md matches code.

## Track P — Projection & timing (order: P2 → P1 → P3 → P4 → P5; P6 stretch)

### P2 — `TimingTuning`: consolidated constants (S-M, low risk)
**Goal:** One documented home for the five tuning constants, vary-able in tests (prep for accuracy tuning).
**Design:**
```rust
// nexrad/timing/config.rs
pub struct TimingTuning {
    pub hist_weight: f64,                   // 0.3  — historical weight in the blend (interval_estimate.rs:30)
    pub poll_bias_secs: f64,                // 0.75 — first poll lands after expected upload
    pub default_availability_lag_secs: f64, // 5.0  — fallback before any lag sample (scan_timing_projection.rs:76)
    pub max_timing_samples: usize,          // 10   — rolling window per characteristics bucket
    pub default_volume_duration_secs: f64,  // 300  — pre-VCP fallback (observations.rs:22)
}
impl Default for TimingTuning { /* current values */ }
```
One instance owned by `Projector`, passed by reference into `estimate_interval` (new param) and the lag-fallback paths. Doc comments state which time axis each constant biases.
**Files:** new `src/nexrad/timing/config.rs`, `src/nexrad/timing/{interval_estimate,chunk_timing_stats,scan_timing_projection}.rs`, `src/nexrad/projection/observations.rs`, `src/nexrad/projector.rs`.
**Accept:** the five constants exist only in config.rs (grep); existing timing tests pass on `Default`; one test varies the tuning and observes the blend change.

### P1 — Split AVAILABILITY vs COLLECTION timing domains (M-L, medium risk, SMOKE) ⚠ BEHAVIOR-CHANGING (intended)
**Goal:** Fix the verified conflation so `estimate_interval`'s historical term is collection-domain — the blocker for accuracy tuning.
**Design:**
```rust
// chunk_timing_stats.rs — rename by domain
struct TimingStat {
    availability_interval: Duration,        // was `duration` (S3 upload→upload delta)
    collection_interval: Option<Duration>,  // NEW: delta of parsed radial collection-end times
    availability_lag: Option<Duration>,     // unchanged (upload − collection)
    attempts: usize,
}
// accessors: average_availability_interval(bucket)  [rename of get_average_timing]
//            average_collection_interval(bucket)    [NEW — ignores None samples]
```
- **Recording:** the drain at `streaming.rs:330-347` already has the `CollectionEndSecs` observation and `iter.current_id()` in scope. Change `Projector::record_chunk_collection_end_secs` → `record_collection_end(chunk_id, secs)`: when `latest_chunk_collection_end_secs` is `Some(prev)` (same volume — resets at boundary), record `secs − prev` as a `collection_interval` sample for the chunk's bucket via existing `characteristics_for_sequence`, then update the anchor. Engine setter `set_collection_anchor_secs` forwards the chunk id.
- **Consumption:** `estimate_interval`'s historical term reads `average_collection_interval`; with no collection samples, **fall back to pure physics** (`used_historical = false`) — never to availability deltas (would re-conflate). Availability intervals stay recorded for diagnostics.
- Bump `PERSIST_SCHEMA_VERSION` (clean invalidation already in place).
**Files:** `src/nexrad/timing/{chunk_timing_stats,interval_estimate}.rs`, `src/nexrad/projector.rs`, `src/nexrad/projection/engine.rs`, `src/nexrad/realtime/streaming.rs` (drain site), `src/state/vcp_forecast.rs` + `src/ui/vcp_forecast_modal.rs` if they display the renamed stat.
**Accept:** bin tests — collection-interval recorded from consecutive collection ends and NOT across a volume boundary/anchor reset; serde round-trip at new version + old version rejected; `estimate_interval` uses collection history when present, pure physics otherwise. New volume-level invariant test in `scan_timing_projection.rs` (currently thin): for every projected chunk, `available_at == collection_at + lag_used`, and all three axes monotonic across the projected volume. SMOKE: live session — no poll storms, sane "next in Xs" countdown, reasonable acquisition-drawer latencies, forecast modal provenance correct.

### P3 — Engine volume-lifecycle API (S, low-med risk, SMOKE)
**Goal:** The Start-chunk boundary becomes one named engine call instead of the 4-call cluster at `streaming.rs:1064-1071`.
**Design:**
```rust
impl ProjectionEngine {
    /// Stream-side volume boundary (Start chunk): install new VCP, clear the
    /// collection anchor, bound inventory memory, set scan start.
    pub fn begin_volume(&mut self, vcp: Message<'static>, scan_start_secs: f64, keep_from: VolumeIndex);
}
```
Do **not** fold in `reset_volume_observations` — that is the *ingest-side* boundary, intentionally later (seal-before-reset at `worker_results.rs:570-578`; session stop at `subsystem/live.rs:77`). Document the two-phase boundary on the engine: `begin_volume` (stream sees Start chunk) vs `reset_volume_observations` (ingest reports volume end / session stop).
**Files:** `src/nexrad/projection/engine.rs`, `src/nexrad/realtime/streaming.rs`.
**Accept:** engine bin test (`begin_volume` clears anchor, sets scan start, installs VCP, bumps revision); streaming Start-chunk arm reduces to loop-local bookkeeping + one engine call. SMOKE: live volume rollover re-anchors cleanly.

### P4 — Diagnostics consolidation + dead-field removal (M, low risk, SMOKE)
**Goal:** Diagnostics read engine-owned data through one path; delete write-only/dead fields.
**Design:** Delete write-only `Projection.revision` mirror (`projection/mod.rs:368` — consumers read `plan.revision`); delete `ScanProjection`'s `allow(dead_code)` fields (`projection/mod.rs:215,218`) if unread. **Keep** the `last_completed_volume` seal-before-reset snapshot (justified), but prune `VolumeForecastSnapshot`/`SweepForecast` fields under `allow(dead_code)` that nothing renders (`vcp_forecast.rs:64-67,121-124,452`); make the forecast modal's *live* view read `engine.last_projection()`/observations rather than parallel derive helpers in `vcp_forecast.rs` — delete duplicating helpers. Leave `SweepBounds.{is_complete,is_in_progress}` (`status.rs:183-185`) — golden-test contract fields.
**Files:** `src/nexrad/projection/mod.rs`, `src/state/{vcp_forecast,live_mode}.rs`, `src/ui/{vcp_forecast_modal,stats_modal}.rs`.
**Accept:** clippy clean with removed `allow(dead_code)` gone. SMOKE: stats + VCP forecast modals render mid-volume, after completion, after stop.

### P5 — Naming sweep (M wide/shallow, low risk, strictly last in track)
**Goal:** One word per concept: **projection** = forward-looking placement; **observation** = measured; **estimate** = interval math; **forecast** = diagnostics predicted-vs-actual; **provenance** = cascade tier labels.
**Design:** Mechanical renames with an up-front table in the PR description. Mandatory: `state/vcp_forecast.rs::SweepTiming` → `ForecastTimingLabel` (kills collision with `data/keys.rs::SweepTiming`). Review-and-decide: the `ChunkProjection` (`scan_timing_projection.rs:132`) / `ChunkProjectionInfo` / `ChunkForecast` (`realtime/mod.rs:59`) triple — the realtime-facing one should not say "forecast" if forecast is reserved for diagnostics. Keep: `Projection`, `SweepProjection`, `VolumeObservations`, `IntervalEstimate`, `SweepTimingProvenance` (already conform).
**Accept:** `rg` old names → 0; check + clippy clean; zero behavior change.

### P6 (stretch, optional) — Decompose `realtime/streaming.rs` (M, low-med risk)
**Goal:** Reduce the 1,846-line file to `streaming_loop` (~900 lines) + drains by extracting cohesive helper families — pure moves, no logic changes.
**Design:** `realtime/persistence.rs` (volume-hint + timing-stats localStorage, ~1583-1722), `realtime/probe.rs` (listing/slot-freshness/`wait_for_next_target`, ~1369-1560), `realtime/backfill.rs` (~112-330), `pub(super)` visibility. Loop body untouched.
**Dependencies:** only after P1 + P3 merge (same file); do not parallelize with them.
**Accept:** `git diff --color-moved` shows moves only; check clean; module docs state ownership.

## Sequencing summary

| Track | Order | Payload item |
|---|---|---|
| T | T4 → T2 → T1 → T3 (T5 anytime) | **T1** |
| I | I1 → I3 → I4 (I2 anytime) | **I1** |
| P | P2 → P1 → P3 → P4 → P5 (→ P6) | **P1** |

Tracks are independent (nearly disjoint files). Behavior-preserving throughout except **P1** (flagged) and T1's deliberate removal of the `seek_to` silent no-op (bug-shaped behavior; all reachable callers already exit live before seeking).

## Non-goals — leave these alone

- The provenance cascade (`status.rs::cascade_current_sweeps`) — justified, golden-tested.
- 3-store IDB design — `scan_touches` separation deliberately avoids the RMW race with chunk-ingest entry merges (`mod.rs:541-545`).
- Destructive schema upgrade (delete+recreate stores) — fine for a cache-only tool.
- `UpsertScanGuard` — documented backstop for the non-atomic read-then-write.
- `read_touch` — intentional test surface.
- The `write_tx` FnOnce no-await design — constraint enforcement, not entropy.
- `streaming_loop`'s internal control flow — P6 only relocates helpers; restructuring the scheduler overlaps `docs/REFACTORING_STRATEGIC.md` S2 (async model) and is out of scope.
- Subsystem decomposition / AppState ownership — strategic initiative S1; these workstreams must not expand into it.
- No new crate dependencies; stable toolchain; wasm32-unknown-unknown only.

## Verification

Per workstream: `cargo check` → `cargo clippy -- -D warnings` → `cargo test --bin nexrad-workbench` (pre-commit enforces); commit at each milestone per CLAUDE.md. I-track additions to `tests/idb.rs` run in CI (`CHROMEDRIVER=/usr/bin/chromedriver cargo test --test idb` locally if chromium available). Grep-based acceptance checks are listed per workstream — run them before committing.

**Manual smoke (no UI automation exists)** — required for T1, T2, P1, P3, P4 via `trunk serve` with a live session:
1. Go live on an active site → now-line/countdown/staleness coherent (T2), chunks arrive without poll storms, "next in Xs" sane (P1).
2. Transport cycle: play (lookback loop) → pause (re-pin to now) → stop (freeze at now) → archive seek; seek-while-live and datetime-jump exit live; Ctrl+L; mobile scrubber + tabs (T1).
3. Ride across a volume rollover → overlay re-anchors cleanly (P3); stats + VCP forecast modals correct mid-volume / after completion / after stop (P4).
4. Archive mode: scrub, elevation change, macro/micro zoom, tooltips, download ghosts — visual parity (T3/T4/T5).
