# Audit: `data/`, `geo/`, `alerts/`, `mping/`, `net/`

Part of the [2026-07-25 architecture review](README.md).

Scope covered against intent in `ARCHITECTURE.md`, `docs/INDEXEDDB.md`, `docs/CORE_SHELL.md`. Line counts split code vs. tests where relevant (many files are ~50% `#[wasm_bindgen_test]`). Citations are `file:line` as of this date.

## 1. Storage stack: layering is real for `keys → idb`, but the "facade" is not the gateway it's presented as

**The facade wraps only the main-thread read/eviction subset; the write + render-read hot paths bypass it entirely.**

`DataFacade` (`src/data/facade.rs`) exposes exactly six async methods: `open` (41), `scan_availability` (46), `list_scans` (51), `total_cache_size` (61), `clear_all` (66), `check_and_evict` (76). Five of the six are one-line pass-throughs to the identically-named `IndexedDbStore` method (e.g. `facade.rs:47` `self.store.scan_availability(scan).await`). Only `check_and_evict` adds logic — and even that just gathers two sizes and delegates the actual decision to the pure `decide_eviction` in `quota.rs:70` (`facade.rs:83-89`). So the facade's genuine added value is one composed method.

Meanwhile the store's write path (`upsert_scan`, `mod.rs:458`), render read path (`get_sweep`, `mod.rs:564`), and eviction primitives (`delete_scan` `mod.rs:764`, `evict_to_size` `mod.rs:814`) are **not on the facade at all**. The worker calls `IndexedDbStore` directly through a long-lived thread-local:
- `src/nexrad/worker_api/mod.rs:231` `static WORKER_IDB: IndexedDbStore = IndexedDbStore::new();`
- writes: `src/nexrad/worker_api/ingest.rs:135` and `:545` `store.upsert_scan(...)`
- reads: `src/nexrad/worker_api/render.rs:33` and `:209` `store.get_sweep(...)`

`DataFacade` callers are all main-thread orchestration: `main.rs:474`, `nexrad/download.rs:138`, `nexrad/acquisition_coordinator.rs:26`, `nexrad/cache_channel.rs:54`, `nexrad/realtime/streaming.rs:113`, `subsystem/acquisition.rs:70`. No `state/` or `ui/` code touches either the facade or the store directly — that boundary is clean.

**Verdict:** there are two parallel entry points to the same DB — `DataFacade` (main thread, query + eviction) and raw `IndexedDbStore` (worker, write + render-read). The worker bypass is *justified* (the worker holds a persistent connection to avoid open-time overhead — `INDEXEDDB.md` §intro, `worker_api/mod.rs:226-231`), but the consequence is that "facade" oversells: it is a main-thread read/eviction wrapper, not the single storage gateway the layering diagram implies. `keys → idb primitive` is a real layer; `idb → facade → all callers` is only half-true.

## 2. Doc/code drift in the storage write API (`create_scan`/`put_scan` no longer exist)

`INDEXEDDB.md` §6 documents a two-method write split — `create_scan` and `put_scan` — as a "correctness-critical contract" (lines 258-278, 466-484), and `src/lib.rs:7` and the integration-test description reference the same names. The code has since been consolidated into a **single** `upsert_scan` that internally dispatches first-write vs. merge on a `scan_availability` lookup (`src/data/indexeddb/mod.rs:458-556`), guarded by a new `UpsertScanGuard` + `DataError::ConcurrentUpsert` (`mod.rs:145-176`, `:463`) that the docs never mention. The module-level comment in `mod.rs:27-32` correctly describes `upsert_scan`, but the canonical schema doc (`INDEXEDDB.md`) and `lib.rs` are stale. Anyone reading the docs as "source of truth for the schema" (`ARCHITECTURE.md:321`) will look for methods that don't exist.

## 3. `keys.rs` (1800 lines) is not "key types" — it has accreted serialization, VCP physics, and a live-volume state machine

Tests begin at line 880, so ~879 lines are production code and ~921 are tests. Within the code, only about a third is actual key vocabulary:

- **Key types** (the stated purpose): `SiteId` (40), `UnixMillis` (80), `ScanKey` (132), `SweepDataKey` (274), `KeyParseError` (20).
- **Binary blob serialization** (a whole subsystem): `GateValues` (340), `PrecomputedSweep::to_bytes` (419), `parse_sweep_header` (503), `SweepHeader` (478), the 72-byte layout constant (415). This is the on-disk format engine, not a key.
- **VCP timing physics**: `ExtractedVcp::sweep_durations` (724) and `estimated_volume_duration` (765) implement Method-A/Method-B azimuth-rate weighting and reach *sideways* into `crate::data::vcp::fallback_azimuth_rate` (740, 779). This is domain math, unrelated to storage keys.
- **Live-streaming state machine**: `ConfirmedStart` (210), `ProvisionalStart` (218), `LiveVolumeAnchor` with `best_start_secs`/`confirm` (238-268) — provisional→confirmed transition logic for the live volume, explicitly "replaces fields that previously lived on `LiveModeState`" (222-224).
- **Write-path DTOs**: `SweepTiming` (622), `ProductBlob` (637), `ElevationUpload::to_cached_sweep` (662), `ScanHeader` (679).
- **Completeness domain logic**: `ScanCompleteness::from_counts` (571), `ScanIndexEntry` accessors `completeness`/`has_elevation`/etc. (833-877).

Note the doc index (`ARCHITECTURE.md:156`) lists a `SweepMeta` type that no longer exists in the file — another small drift. `keys.rs` is really the data-model-plus-serialization module; the name undersells it and the file would split cleanly into `keys` / `blob_format` / `vcp_timing` / `live_anchor`.

## 4. `alerts/` vs `mping/`: channel + fetch skeleton are near-clones; managers have genuinely diverged

Both modules have the same six-file shape. The duplication is real but uneven:

**`channel.rs` — effectively identical (copy-paste).** `AlertsChannel` (`src/alerts/channel.rs:27-46`) and `MpingChannel` (`src/mping/channel.rs:23-42`) differ only in the event enum name. `new`/`push`/`drain` are byte-for-byte the same (`Rc<RefCell<Vec<_>>>`, `std::mem::take`). `mping/channel.rs:3` even documents "Mirrors `crate::alerts::channel::AlertsChannel`."

**`api.rs` — the fetch skeleton is a near-clone.**
- `err_text` is **byte-for-byte identical**: `alerts/api.rs:142-150` vs `mping/api.rs:187-195`.
- `spawn_fetch` has the same body shape — `spawn_local` → `match fetch_inner(...)` → build event → `channel.push(event)` → `ctx.request_repaint()` (`alerts/api.rs:24-44` vs `mping/api.rs:41-61`).
- `fetch_attempt` shares the same 8-step skeleton: `window()` guard, `RequestInit` + `Cors`, `Headers::new`, `Request::new_with_str_and_init`, `fetch_with_request`, `dyn_into::<Response>`, status branching, `resp.text()` → `as_string()` (`alerts/api.rs:60-140` vs `mping/api.rs:117-185`).

The *differences* are policy, not structure: alerts adds ETag/`If-None-Match` + 304 + `Retry-After` parsing and treats a network error as `Retry` (`alerts/api.rs:93`); mping adds `Token` auth + URL building and treats a network error as `Terminal` (`mping/api.rs:146`). Both wrap the attempt in `with_retry(&DEFAULT_POLICY, ...)` (`alerts/api.rs:52`, `mping/api.rs:68`).

**`manager.rs` — structurally parallel head, diverged body.** Both have `channel` + `fetch_in_flight` fields, `Default`, `new`, and a `tick` that opens with the identical drain-then-clear-flag-then-apply loop (`alerts/manager.rs:63-69` vs `mping/manager.rs:105-111`), and an `apply_event` with the same `fetch_in_flight=false` → match `Updated`/`Error` → set `last_error`/`last_success_ms` → `errors.push(...)` shape (`alerts/manager.rs:114-143` vs `mping/manager.rs:185-226`). The polling *cadence* logic then diverges substantially: alerts is a flat interval (`POLL_INTERVAL_MS`/`RETRY_INTERVAL_MS`, `manager.rs:16-20`, 144 lines total); mping is a two-regime coverage-window model (`Covered`, `refetch_needed`, live-tail vs. historical, `manager.rs:64-269`, 505 lines with ~280 of tests). Alerts also carries a `ZoneResolver` step mping lacks (`manager.rs:87`).

**Verdict:** a shared "polling feed" abstraction would cleanly absorb the truly-duplicated parts — `channel.rs` in its entirety, `err_text`, `spawn_fetch`, and the `fetch_attempt` request-building skeleton (a `build_request(headers, url) → Verdict` seam with per-feed status classifiers). It would *not* unify the managers: the alerts flat-interval and mping coverage-window policies are legitimately different problems, so this is not superficial similarity in that layer. The channel + fetch-skeleton clone is the removable ~120 lines.

## 5. `geo/` assessment

**`camera.rs` (2181 lines) is essentially pure math, well-tested.** Tests start at line 1298, so ~1297 code / ~884 test lines across two test modules. Only **2** egui references exist and both are value types, not UI: `use eframe::egui::{Pos2, Rect, Vec2}` (`camera.rs:25`) and a `Rect` in a test (`:1312`). **Zero** `glow`/`web_sys`/`WebGl`/`Painter` — this matches `CORE_SHELL.md:227` ("Camera / projection math is already pure (S4)"). It holds **4 camera modes** — `Flat2D`, `PlanetOrbit`, `SiteOrbit`, `FreeLook` (`Camera` enum, `camera.rs:214-219`; `CameraMode`, `:34-42`) — plus per-mode view/projection matrices (`planet_orbit_view_matrix` etc., 260-345), ~13 mutation verbs (`orbit`/`tilt_rotate`/`free_look`/`zoom`/`center_on`/`switch_to_*`, 567-1148), URL snapshot round-tripping (`UrlCameraSnapshot`, 1151-1275), and a `GlobeProjection` adapter (1277-1297).

**"Multiple projection modules" is a name collision across two unrelated domains — they do not overlap.**
- `geo/projection.rs` (581 lines) is **spatial**: geographic lat/lon ↔ screen-pixel transforms. It owns the `Projection` trait (`geo_to_screen`/`screen_to_geo`/`visible_bounds`, `projection.rs:21-36`) and `MapProjection` (equirectangular, `:40-118`). Its trait has two impls: `MapProjection` (2D) and `camera.rs`'s `GlobeProjection` (3D).
- `nexrad/projection/` (4025 lines across 7 files) is **temporal**: forward-looking radar *sweep-timing* forecasting — `SweepProjection`/`ScanProjection`/`ProjectionEngine`, collection-vs-availability status (`projection/mod.rs:1-13, 137-359`). Nothing to do with coordinate transforms.
- `nexrad/projector.rs` is the **timing math kernel** (`Projector::build_plan` → `StreamingPlan`, `projector.rs:1-13, 51+`) that `nexrad/projection/engine.rs`'s `ProjectionEngine` wraps (`projection/mod.rs:10-13`, `engine.rs:1-8`).

So there is no coordinate-projection duplication. The only genuine "why two?" is the temporal pair `projector.rs` (kernel) vs `projection/` (engine wrapping it) — and that is an explicitly documented in-progress consolidation (`projection/mod.rs:9-13` "Phase 0… wrapped `plan` retained as the math carrier until those migrations land"), not accidental overlap. The real smell is nominal: `geo::projection` and `nexrad::projection` sharing a name for orthogonal concepts.

**Renderers are cleanly separated from data.** The three renderers consume the data modules and are consumed by nobody in `geo/`:
- `renderer.rs` (2D egui, `renderer.rs:8-9` imports `layer` + `MapProjection`),
- `globe_renderer.rs` (WebGL2/glow sphere, `:8-9` imports `Camera` + `glow`),
- `geo_line_renderer.rs` (WebGL2/glow lines, `:7-8` imports `layer` + `Camera`).

`layer.rs` and `cities.rs` never import the renderers — the dependency arrow points one way. One accretion: `layer.rs` (998 lines) is billed as "layer data structures" (`:1`) but also embeds **shapefile decoding** (`use shapefile::dbase::FieldValue`, `Cursor`, `layer.rs:6-8`) and **projection/label caching** (`FeatureProjection`, `LabelCacheToken`, `LayerLabelCache`, imported into `renderer.rs:8`). So `layer.rs` mixes DTOs + IO parsing + render-cache state.

## 6. Static data embedding is inconsistent: two hand-written tables, one generated asset, no unifying pipeline

- **`sites.rs` (1886 lines): hand-written.** `pub static NEXRAD_SITES: &[NexradSite]` is a literal table of **207** `NexradSite { … }` entries (`sites.rs:50+`; grep-counted). The header says "Data sourced from NOAA/NCEI NEXRAD Stations ArcGIS Feature Service" (`:3`) but it is hand-transcribed Rust source — no generator. Tests start at 1740.
- **`cities.rs` (992 lines): hand-written.** `static CITIES: &[CityEntry]` is a literal table of **135** entries (42 Major + 31 Medium + 62 Small; `cities.rs:27+`, confirmed by the test at `:881`). No generator.
- **`alerts/zones.rs` (613 lines): generated + embedded.** This is the *only* one with a real pipeline: `static ZONE_GEOMETRY_JSON: &[u8] = include_bytes!("../../assets/zone_geometry.json")` (`zones.rs:32`), a ~1.2 MB asset produced by `tools/build_zone_geometry.py` from official NWS shapefiles (`zones.rs:9-11, 29-32`).

`tools/` contains exactly one script — `build_zone_geometry.py` — and it feeds only zones. `build.rs` does **not** generate any of these; it only derives a version string from git (`build.rs:1-46`). So the pattern is three different mechanisms for three static datasets: hand-authored struct literals (sites, cities), and a Python-generated `include_bytes!` JSON blob (zones). The two datasets with clear upstream sources (ArcGIS site feed; a city list) are the ones with no reproducible pipeline.

## 7. `net/retry.rs`: the "every outbound request" claim holds — no unguarded fetches

Every outbound path is under `with_retry` or its sanctioned per-attempt primitives:
- `alerts/api.rs:52` — `with_retry(&DEFAULT_POLICY, "alerts", …)` wrapping the only `window.fetch_with_request` in that module (`:89`).
- `mping/api.rs:68` — `with_retry(&DEFAULT_POLICY, "mping", …)` wrapping its `fetch_with_request` (`:143`).
- `nexrad/download.rs` — `archive_list` (`:248`, `:351`) and `archive_download` (`:384`), all `DEFAULT_POLICY`, wrapping `nexrad_data` `archive::list_files`/`download_file`.
- `national_mosaic.rs:125` — `with_retry(&DEFAULT_POLICY, "national_mosaic", …)` wrapping `fetch_and_decode` (which loads via `HtmlImageElement`, not `window.fetch`, but is still inside the policy).
- `ui/site_modal.rs:214` — `with_retry(&DEFAULT_POLICY, "zip_lookup", …)` (geocoding).
- `realtime/streaming.rs:922-958` — an **inlined** retry loop using `attempt_with_timeout` + `REALTIME_CHUNK_POLICY`, not `with_retry`. This is a documented, sanctioned exception, not a bypass: the closure must borrow `iter` mutably across attempts, which `with_retry`'s `FnMut(u32)->Fut` signature forbids (`streaming.rs:911-913`, and the escape hatch is called out in `retry.rs:82-85`). It is the only user of `REALTIME_CHUNK_POLICY`.
- `archive_index.rs` — **does no I/O at all**; it is a pure in-memory `HashMap` listing cache (`archive_index.rs:1-4`), so it is correctly absent from the retry callers. The listing fetch it caches happens in `download.rs` under `with_retry`.

No unguarded HTTP call site was found outside these. The claim is accurate.

## 8. Cross-module dependency hygiene: `data/` is a clean leaf; `geo/` inverts onto `state/`

- **`data/` — clean.** Every `use crate::…` in `data/*.rs` and `data/indexeddb/*.rs` points only at `crate::data::*` (self). Zero imports of `state`, `ui`, `app`, `subsystem`, or `nexrad`. It is a proper leaf.
- **`geo/` — two upward dependencies on `state/`:** `src/geo/renderer.rs:10` `use crate::state::GeoLayerVisibility;` and `src/geo/camera.rs:24` `use crate::state::ViewMode;`. Per the module hierarchy in `ARCHITECTURE.md`, `state` sits *above* `geo`, so these invert the intended layering. `camera.rs` half-acknowledges it ("`ViewMode` is a *derived* view," `:20`) but still imports the enum from the higher layer rather than owning it locally.
- **`alerts/` and `mping/` managers depend on `state/`:** `alerts/manager.rs:12` `use crate::state::AlertsState;` and `mping/manager.rs:25` `use crate::state::MpingState;`. Defensible (the managers exist to drain into those state structs, and both take the state by `&mut` and stay decoupled from the rest of `AppState`), but it is still an upward type dependency.
- **No `ui/` dependency anywhere** in the audited modules — the more dangerous inversion is absent.

So the intended "data/geo below state/ui" holds for `data/` and for the renderers' data-flow direction, but is violated by `geo::renderer`→`state::GeoLayerVisibility` and `geo::camera`→`state::ViewMode`, and (more weakly) by the two feed managers. These are the concrete inversions to note; none is a cycle, and all four are single-type imports that could be resolved by relocating `ViewMode`/`GeoLayerVisibility` down into `geo` or passing them as parameters.
