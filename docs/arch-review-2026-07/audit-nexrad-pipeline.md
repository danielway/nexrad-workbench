# Audit: `src/nexrad/**` + `worker.js`

Part of the [2026-07-25 architecture review](README.md).

Scope: 55 `.rs` files under `src/nexrad/` (~28 top-level entries, 26,755 LOC per `wc -l`) plus `worker.js` (243 lines). Evidence is cited as `file:line` as of this date.

## Summary of significance
1. The **projection/timing concern is fragmented across 5 sibling locations** (~9,900 LOC) mid-refactor — the dominant cohesion problem, larger than the module's own docs acknowledge.
2. **`realtime/streaming/` is 2,044 lines with an 883-line `streaming_loop`** — the single largest decomposition debt; explicitly deferred in `CORE_SHELL.md`.
3. **Worker protocol is stringly-typed and triple-defined** (Rust send struct → `worker.js` remap → Rust receive struct); drift is guarded only by convention + unit tests.
4. **Two misfit modules** (`network_monitor.rs`, `persistence_manager.rs`) are not data-pipeline code and are owned by non-pipeline subsystems.
5. **Three GPU renderers share no abstraction** and have inconsistent constructors/update APIs.
6. **Docs are materially stale** — reference nonexistent files and disagree with each other on live constants.

---

## 1. Module cohesion — concern sprawl and a fragmented projection layer

`src/nexrad/mod.rs:12-38` declares 28 modules spanning at least seven concerns. Grouped by actual imports (`grep use super::/crate::nexrad`), the natural sub-modules are:

| Concern | Files | ~LOC |
|---|---|---|
| Archive acquisition | `download.rs`, `download_queue.rs`, `archive_index.rs`, `cache_channel.rs`, `acquisition_coordinator.rs`, `types.rs` | ~2,800 |
| Live streaming | `realtime/` (2), `streaming_state.rs`, `streaming_filter.rs`, `streaming_plan.rs` | ~3,600 |
| **Projection / timing prediction** | `projector.rs`, `projection/` (7), `timing/` (9) | **~9,900** |
| Decode / ingest | `record_decode.rs`, `ingest_phases.rs`, `worker_api/` (4), `decode_worker/` (5) | ~4,400 |
| GPU rendering | `gpu_renderer/` (4), `globe_radar_renderer.rs`, `volume_ray_renderer.rs`, `color_table.rs`, `national_mosaic.rs`, `render_coordinator.rs`, `render_request.rs` | ~3,600 |
| Analysis | `detection/` (3) | ~911 |
| Misc/observability | `network_monitor.rs`, `persistence_manager.rs` | ~270 |

**The projection concern is the real cohesion failure**, and it is not what ARCHITECTURE.md describes. Forward-looking timing is split across five owners with a wrapping-chain dependency:

- `timing/` (9 files, ~4,500 LOC) — the pure predictor primitives.
- `projector.rs:55` `Projector` — owns VCP + mapper + stats + filter + anchor, builds a `StreamingPlan`.
- `projection/engine.rs:19,67` `ProjectionEngine` **wraps** `Projector` (`projector: Projector::new()`).
- `projection/` (7 files, 4,025 LOC) — a self-described *incremental* "unified projection architecture."
- `streaming_plan.rs` (553) + `streaming_state.rs` (409) — the plan type and download cursor.

Two doc-comments confirm this is an unfinished migration, not a settled design:
- `projection/mod.rs:1-14`: "*being built incrementally … Phase 0 introduces only `Projection`, a thin wrapper that contains today's `StreamingPlan`. Later phases enrich it … and migrate consumers onto it.*" Six `#[allow(unused_imports)]` re-exports (`projection/mod.rs:24-40`) exist because "*a few helpers are still consumed only within this module … until every consumer converges.*"
- `projector.rs:9-13`: "*Extracted from `StreamingState` … Today `StreamingState` still owns the projector … a later commit moves ownership to the main thread.*"

So there are simultaneously three overlapping projection owners (`Projector`, `ProjectionEngine`, `StreamingState`) plus the pure `timing/` crate-fork. This is the highest-value consolidation target in the module.

**Misfits (do not belong in `src/nexrad`):**
- `network_monitor.rs` — service-worker telemetry (`network_monitor.rs:1-5`), owned by `subsystem/diagnostics.rs:58` (`network_monitor: Option<NetworkMonitor>`). It is observability, not the data pipeline; it never touches radar data. Belongs with diagnostics.
- `persistence_manager.rs` — URL-state + preference throttling. It is now a thin shell over `crate::core::decide_persist` (`persistence_manager.rs:9,66`) handling `Effect`s for `WorkbenchApp` — zero NEXRAD coupling. Belongs in `core`/`app`, not `nexrad`.

**Borderline (defensible but odd):**
- `detection/` (3 files, 911 LOC) — connected-component storm-cell analysis on reflectivity; an analysis stage, not acquisition/decode/render.
- `national_mosaic.rs` — a separate NOAA MRMS WMS fetch+decode+GPU feature (`national_mosaic.rs`), self-contained and unrelated to the S3 radar pipeline.

---

## 2. The worker boundary — stringly-typed, triple-defined, convention-guarded

The protocol is defined **three times** for every message, with no generation step:

1. **Rust send** — serialized structs with `#[serde(rename_all = "camelCase")]`, tagged via `RequestType::as_str()` (`decode_worker/types.rs:20-41`, `318-380`).
2. **`worker.js`** — hardcoded `type` string literals at `worker.js:109,138,158,177,198,223`, then **manual re-mapping of every field** into the WASM call, e.g. `worker.js:143-149` (`ingest`) and `160-169` (`ingest_chunk`).
3. **Rust receive** — separate deserialize structs (`worker_api/mod.rs:20-75`) and response parsers (`decode_worker/types.rs:120-310`, `ResponseType::parse` at `types.rs:70-82`).

Payload serialization is `serde_wasm_bindgen` for scalars, with ArrayBuffers attached out-of-band for zero-copy (`decode_worker/send.rs:234-243`; `worker_api/mod.rs:108-110`, `render.rs:155-160`). That part is clean.

**Drift risk is real but well-fenced.** `worker.js` does not import any shared constant — the message tags and field names are literals. The contract is pinned only by:
- A prose contract comment (`worker.js:36-41`, `73`): "*Adding a new message type requires changes in BOTH places.*"
- Round-trip unit tests: `request_type_strings_are_snake_case` (`types.rs:640-650`), `response_type_strings_roundtrip` (`613-630`), `worker_error_kind_deserializes_known_strings` (`652-671`), plus `worker_api/mod.rs:270-388` param-parse tests.

These tests pin the *Rust* side to fixed literals, so a Rust typo is caught — but nothing mechanically ties `worker.js`'s literals to those. A `worker.js`-only rename (e.g. `siteId`→`site`) compiles, passes all Rust tests, and fails only at runtime. The `WorkerErrorKind` path is the best-designed part: structured `{kind, message}` with `#[serde(other)] Unknown` forward-compat (`types.rs:93-109`, `worker.js:73-98`). `CORE_SHELL.md:246-248` already lists "Schema-first worker IPC" as the intended fix. The mid-hop remapping in `worker.js` (reading `msg.siteId` only to rebuild `{siteId: msg.siteId}`) is pure boilerplate that a generated dispatcher would eliminate.

---

## 3. Coordinators — two clean facades, one facade over a monolith

- **`AcquisitionCoordinator` (`acquisition_coordinator.rs`, 105 LOC)** — a genuine thin facade. Owns 5 fields (`:16-27`) and every method is a one-line delegate (`try_recv_download :60`, `load_site_timeline :75`, `all_boundaries_for_site :92`). No orchestration, no I/O logic of its own. Owned by `subsystem/acquisition.rs:32`.
- **`RenderCoordinator` (`render_coordinator.rs`, 411 LOC, but 143 of them are tests `:267-411`)** — narrow real surface (~5 fields `:17-38`). Dedup logic is correctly delegated to the pure core (`should_dispatch` at `:115,154`). Not a god-object; it is a request router. Owned by `subsystem/render.rs:44`.
- **`RealtimeChannel` (`realtime/mod.rs`, 546 LOC incl. tests)** — a clean channel facade: 3 typed `mpsc` queues + `Rc<Cell<bool>>` (`:193-207`), narrow API (`start :292`, `stop :343`, `observe :364`, `sync_filter :398`, `poll :403`). Holds no loop state. Owned by `subsystem/live.rs:38`.

The god-object is not any coordinator — it is what `RealtimeChannel::start` spawns: `streaming_loop` (see §5). Note ARCHITECTURE.md's `StreamingManager` (`ARCHITECTURE.md:88,257`) **does not exist** — `grep StreamingManager` returns zero hits; the role is split between `RealtimeChannel` and `subsystem::live`.

---

## 4. Archive vs live duplication — decode shared, marshaling/acquisition duplicated

**Decode logic is well-shared.** Both `worker_ingest` (archive) and `worker_ingest_chunk` (live) call the same `ingest_phases` primitives:
- Archive: `worker_api/ingest.rs:78,103,108` → `decompress_and_decode_records`, `group_radials_by_elevation`, `build_elevation_uploads`.
- Live: `worker_api/ingest.rs:299,349,370,492` → `decode_start_chunk`, `decode_subsequent_chunk`, `compute_chunk_time_spans`, `build_elevation_uploads_for_flush`.
- Both, plus `render_live`, share `record_decode.rs` (`decode_record_to_radials :20`, `extract_sweep_data_from_sorted :56`). Good.

**Render marshaling is duplicated.** `worker_render` (`render.rs:134-160`) and `worker_render_live` (`render_live.rs:134-159`) each independently build a `RenderResponse` and `attach_buffer_field` the three ArrayBuffers. The struct shape is shared (`worker_api/mod.rs:137-159`) but the marshaling code is copy-pasted and has **diverged**: `azimuth_spacing_deg` is `360/count` in the archive path (`render.rs:86-90`) but a median-gap computation in the live path (`render_live.rs:119-132`). That divergence is defensible (partial sweeps) but is exactly the kind of silent drift a shared marshaler would surface.

**Acquisition is entirely un-shared — two separate network stacks.** The archive path uses hand-rolled fetchers + `net::retry` (`download.rs:242 fetch_archive_listing`, `:319 download_specific_file`). The live path uses upstream `nexrad_data::aws::realtime` (`streaming.rs:168,255,286` `download_chunk`/`list_chunks_in_volume`; `streaming_state.rs:16-17,80,84,106`). They share no listing, download, or retry code — archive uses `DEFAULT_POLICY`, live uses an inline retry loop with `REALTIME_CHUNK_POLICY` (`STREAMING.md:293`, because "*each attempt borrows `iter` mutably*"). This is a deliberate but real duplication of the "list → pick → download → track bytes" pattern.

---

## 5. `realtime/streaming/` — 2,044 lines, one 883-line function

The file holds ~35 free functions plus `streaming_loop` (`:366-1249` = **883 lines** in a single `async fn`). Distinct responsibilities co-located, at clear seams:

- **Loop control state**: `LoopState` + `drain_control` (`:28-76`), `SleepOutcome` (`:78`), `interruptible_sleep` (`:1319`).
- **Init/acquire**: `acquire_streaming_state` (`:1728`), `volume_header_start_secs` (`:99`).
- **Backfill**: `cached_elevations_for_scan` (`:112`), `filter_backfill_sequences` (`:132`), `emit_backfill_chunks` (`:155`), `run_mid_stream_backfill` (`:240`).
- **Fetch/poll**: `wait_for_next_target` (`:1434`), `classify_filter_outcome` (`:1265`), `classify_chunk_result` (`:1292`), `should_list_now`/`slot_is_fresh`/`target_present_in_listing` (`:1373-1402`), `probe_latest_from_hint` (`:1661`).
- **Observations/plan**: `drain_pending_observations` (`:330`), `build_engine_plan` (`:354`).
- **Persistence**: `volume_cache_key`/`encode`/`decode`/`cache_volume_number`/`get_cached_volume_hint` (`:1587-1636`), `save_timing_stats`/`load_cached_timing_stats` (`:1699-1728`).
- **Stats**: `StatsTracker` (`:1543`).

A seam-level decomposition (the free functions already are the seams): `streaming/loop_state.rs`, `streaming/acquire.rs`, `streaming/backfill.rs`, `streaming/poll.rs` (the ~450-line fetch/list-probe cluster), `streaming/persist.rs` (volume/timing localStorage), leaving `streaming_loop` as a thin orchestrator over them. `CORE_SHELL.md:232,256` explicitly defers this ("*reopens live QA; do it when that file next needs substantial change*"), and STREAMING.md §2 already documents the three-phase structure, so the decomposition is low-conceptual-risk.

---

## 6. `timing/` fork — nearly extractable, one app-type leak

`timing/mod.rs:1-20` documents it as a fork of `nexrad_data::aws::realtime` timing logic intended for upstream contribution, deliberately keeping dead code (`#![allow(dead_code, unused_imports)]` at `:5`) to "*preserve the upstream shape for easy diffing.*"

**Isolation is good but not complete:**
- **Clean deps**: it imports only upstream plumbing types it declares it will not fork — `ChunkIdentifier`, `ChunkType`, `VolumeIndex` (`scan_timing_projection.rs:6`, `chunk_timing_stats.rs:2`, `interval_estimate.rs:26`). These are legitimately upstream.
- **The one entanglement**: `timing/elevation_chunk_mapper.rs:1` imports `super::super::streaming_filter::StreamingFilter` (used by `has_remaining_match :264`). `StreamingFilter` is an app type living outside `timing/`.

However, `streaming_filter.rs:17-34` is a 2-variant `Copy` enum with no app dependencies (the `From<ElevationSelection>` impl lives in `realtime/mod.rs:39`, not here). So extraction today is nearly mechanical: move `StreamingFilter` into `timing/` (or accept an `Fn(Option<usize>) -> bool` predicate) and the crate would be free of app types. Verdict: **could be extracted with one small type move** — the fork is disciplined.

---

## 7. Renderer APIs — three renderers, no shared abstraction

`grep trait` across the renderers returns **nothing** — there is no `Renderer` trait. The three public surfaces are inconsistent:

| | `RadarGpuRenderer` | `GlobeRadarRenderer` | `VolumeRayRenderer` |
|---|---|---|---|
| Constructor | `new(gl) -> Result<Self, String>` (`gpu_renderer/mod.rs:120`) | `new(gl) -> Self` (`globe_radar_renderer.rs:162`) | `new(gl) -> Self` (`volume_ray_renderer.rs:347`) |
| Data update | ~12 getters + implicit texture upload (`mod.rs:234-273`) | `update_site(gl, lat, lon, range)` (`:253`) | `update_volume(...)` + `has_data()` (`:531,664`) |
| Draw | `paint(...)` (`:285`) | `paint(...)` (`:290`) | `paint(...)` (`:670`) |

Only `paint` is common by name (signatures differ). The constructor contract is inconsistent (one returns `Result`, two panic-or-infallible). There **is** partial code sharing: `globe_radar_renderer.rs:45` reuses `super::gpu_renderer::shaders::*`, so the globe path is not pure copy-paste. But there is no unifying `trait Renderer { fn paint(&self, ...); }`, no shared lifecycle, and `color_table.rs` is the one genuinely shared abstraction across all render surfaces (consumed by `gpu_renderer/textures.rs`, `worker_api/render_live.rs`, and `ui/canvas_overlays/color_scale.rs`). The inconsistency is cosmetic-to-moderate — a shared trait would mostly buy uniformity, not eliminate duplication.

---

## 8. Documentation drift (found while mapping)

The docs the audit was told to trust as "intent" are materially out of date with the tree:

- `ARCHITECTURE.md:88` lists `streaming_manager.rs` — **no such file** (`grep StreamingManager` = 0 hits).
- `ARCHITECTURE.md:96` lists `realtime.rs` as a single file — it is `realtime/mod.rs` + `realtime/streaming/`. `STREAMING.md:157` and `TIMING.md:71-73,253` still cite `realtime.rs::current_timestamp` / `::provisional_scan_start_secs`; those now live in `realtime/streaming/` (e.g. `current_timestamp_f64` at `streaming.rs:1574`).
- The `ARCHITECTURE.md` nexrad table (`:70-105`) **omits the entire `projection/` module (4,025 LOC)**, plus `streaming_filter.rs`, `streaming_plan.rs`, `timing/interval_estimate.rs`, and `timing/config.rs`.
- **The two timing docs disagree on a live constant.** `TIMING.md:232` states the first-poll pad is `POLL_DELAY_AFTER_PREDICTED_MS (400 ms)` with `CHUNK_POLL_INTERVAL_MS (500 ms)` / `CHUNK_POLL_MAX_RETRIES (25)`; `STREAMING.md:256-262` states **750 ms** (`POLL_DELAY_AFTER_PREDICTED_MS`) with a `REALTIME_CHUNK_POLICY` (`max_attempts 6`, `total_budget 15 s`). The code agrees with STREAMING.md: `timing/config.rs:42` `poll_bias_secs: 0.750`. `TIMING.md` §3b is stale.

These are worth flagging because both `ARCHITECTURE.md` and `TIMING.md` are cited as the canonical map, and a reader wiring against them would target files and constants that no longer exist.

---

### Net assessment
The **decode/worker/coordinator spine is clean** (thin facades, shared decode primitives, structured errors, delegated dedup). The **debt is concentrated in the forward-looking projection stack** — five overlapping owners mid-migration (~9,900 LOC) — and in the **2,044-line `streaming.rs`**. The worker protocol is safe today but structurally fragile (triple-defined, test-guarded not generated). Two modules (`network_monitor`, `persistence_manager`) are simply in the wrong module. The three renderers and the archive/live render marshaling are the low-severity items.
