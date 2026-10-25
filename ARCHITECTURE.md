# NEXRAD Workbench Architecture

A WebAssembly-based NEXRAD weather radar visualization application built with Rust and egui.

This is the canonical engineering map of the codebase. For a **diagram-first
overview** — structural relationships plus runtime sequence/flow diagrams — see
the visual companion [ARCHITECTURE_DIAGRAMS.md](docs/ARCHITECTURE_DIAGRAMS.md).
For depth on a specific
subsystem, follow the dedicated references: [RENDERING.md](docs/RENDERING.md)
(GPU shader pipeline + 3D), [STREAMING.md](docs/STREAMING.md) (real-time
sequencing/timing), [TIMING.md](docs/TIMING.md) (the three time categories), and
[INDEXEDDB.md](docs/INDEXEDDB.md) (cache layer + schema). Product/UX intent lives
in [PRODUCT.md](docs/PRODUCT.md); the binding architecture standard + migration
status in [CORE_SHELL.md](docs/CORE_SHELL.md); building and running in
[CONTRIBUTING.md](CONTRIBUTING.md); the agent/build guide in
[CLAUDE.md](CLAUDE.md).

## Module Structure

```
src/
├── main.rs              # WorkbenchApp, eframe update loop, frame orchestration
├── lib.rs               # Thin lib facade exposing `data` for tests/idb.rs
├── core/                # Headless functional core: domain types, Intent/Effect,
│                        #   pure decision fns + reducers, projection engine, timing
├── state/               # AppState root container + shell-side impls for core types
├── subsystem/           # Bounded state owners (Acquisition, Render, Live, …)
├── app/                 # Imperative shell: assemble → reduce → execute over the
│                        #   core reducers; the Effect runtime
├── nexrad/              # Data pipeline: acquisition/, live/, decode/, render/
├── storm_cells/         # Leaf: storm cell detection (std only, extractable)
├── ui/                  # egui panels, canvas, overlays, mobile chrome (thin shell)
├── geo/                 # Camera, map projection, geographic feature rendering
├── data/                # Leaf: sites, VCPs, keys, blob format, IndexedDB, quota
├── alerts/              # NWS active-alerts polling feed
├── mping/               # mPING storm-report polling feed
└── net/                 # Shared HTTP retry policy
```

The dependency direction between these modules is **enforced at build time** —
see [Architecture enforcement](#architecture-enforcement) below.

### Module Responsibilities

Coarse, module-level map with a few anchor files each. Per-file inventories rot
fastest, so this table names *responsibilities*; find the file with `grep`.

| Module | Purpose | Anchors |
|--------|---------|---------|
| `core` | The pure, headless core. `core/domain/` holds the domain vocabulary shared by every layer (radar model `Scan`/`Sweep`/`Radial`/`RadarTimeline`/`ScanMetadata`, playback time model, viz types incl. `RenderProcessing`/`SweepIdentity`, re-exports of the `storm_cells` result types, `ViewState`, errors, feed states, telemetry records, worker result types). `core` also owns the contract types (`Intent`, `Effect`), the pure decision modules (`persist`, `link` — the URL codec and how a session opens, `prefs` — the versioned preferences, their migration and the reset plan, `keymap`, `diagnostics`, `canvas`, `render`, `panels`, `acquisition`), the worker/frame **reducers** (`worker_ingest`, `worker_decoded`, `render_loop`), the live-mode state machine (`live_mode`, `live_radar_model`), the per-frame session view-model (`session` — stream, playhead, selection and frame facts, and the words every status surface prints), the top bar's view-model and width rule (`top_bar`), the tilt ladder on the volume clock and the one tilt list (`ladder`), the rail's tabs, layer recency sentence and map-tool toggles (`panels`), the toast queue (`toast`), playback sweep-cache logic (`playback_manager`), the timeline view model (`timeline_view`), streaming vocabulary (`streaming_plan`, `streaming_filter`), the projection engine (`projection/` — single owner of forward-looking radar timing; `projector.rs` is its private kernel), and the chunk-timing physics fork (`timing/`, upstream-contribution candidate). No I/O, no egui, no web_sys in decision logic. | `core/mod.rs`, `core/intent.rs`, `core/effect.rs`, `core/projection/engine.rs`, `core/timing/config.rs` |
| `state` | `AppState` root container (frame scratch, status, command queue, settings) plus focused state containers (`viz`, `acquisition`, `calendar`, `url_state`, `recency`, `render_cache`, `saved_events`, `settings`, `stats`, `theme`, `layer`) and the **shell halves** of core types — the impls that touch the browser (`preferences` load/save, `frame_clock` capture, `playback` wall-clock constructors). | `state/mod.rs`, `state/viz.rs`, `state/url_state.rs` |
| `subsystem` | Bounded state owners with typed APIs, each owning a coherent slice of state + channels: `Sites` (one `SiteState` per radar on the map and the focus index), `Acquisition`, `Render`, `Timeline`, `Playback`, `Live`, `Chrome`, `Diagnostics`, `NetworkMonitor`, plus the per-frame `Derived` view-model cache. | `subsystem/mod.rs` |
| `app` | The imperative shell over the core reducers: each frame-loop concern assembles read-only inputs, calls the pure core, then executes the described actions (`worker_results`, `render_loop`, `acquisition_intent`, `command_dispatch`, `live_mode`, `download`, `selection_download`, `frame_setup`). Also the `Effect` runtime (`effects.rs`) and the persistence shell (`persistence_manager.rs`). | `app/mod.rs`, `app/effects.rs` |
| `nexrad` | The data pipeline, grouped by phase: `acquisition/` (S3 download, download queue, archive index, cache-load channel, coordinator), `live/` (realtime streaming loop + channel, `StreamingState`), `decode/` (record decode, ingest phases, `worker_api` WASM exports, `decode_worker` pool), `render/` (GPU renderer, globe/volume renderers, color tables, national mosaic, `RenderCoordinator`). | `nexrad/mod.rs`, `nexrad/live/realtime/streaming/`, `nexrad/decode/ingest_phases.rs` |
| `ui` | Desktop panel layout, timeline, canvas + overlays, playback controls, modals, keyboard shortcuts, mobile chrome and gestures. Thin render shell per the core/shell standard. | `ui/layout.rs`, `ui/canvas.rs`, `ui/timeline/`, `ui/theme.rs` (color tokens, fonts, text sizes) |
| `geo` | Camera system (2D flat + unified 3D orbit camera with fly-to transitions — pure math), map projection, geographic feature types + rendering (`ViewMode`, `GeoLayerVisibility` live here), 3D globe/line renderers, built-in cities. | `geo/camera.rs`, `geo/projection.rs` |
| `data` | True leaf. Static data (`sites`, `vcp`), storage key vocabulary (`keys`), the sweep-blob wire format (`blob_format`), VCP sweep-duration physics (`vcp_timing`), the live-volume anchor state machine (`live_anchor`), the IndexedDB store (`indexeddb/`), quota policy (`quota`), and the main-thread eviction facade (`facade`). | `data/mod.rs`, `data/indexeddb/mod.rs` |
| `alerts` / `mping` | Polling feed modules: API client, parse, async channel, manager (cadence/dedup/viewport policy). Their pure state containers live in `core/domain/feeds.rs`. | `alerts/manager.rs`, `mping/manager.rs` |
| `storm_cells` | Leaf. Storm cell detection: median filter, nested-threshold segmentation, watershed footprints (each envelope shared among its cells), cell/group model and outlines; tracking across volumes (`track`: SCIT-style IDs, motion with a confidence, coasting, split/merge lineage, closest approach, and the converge/diverge rate of two tracks); and the cells' columns through the volume (`volume`: the higher tilts sampled over each footprint by azimuth and 4/3-earth ground range — echo top, composite peak and its height, per-tilt peaks). Imports nothing from the crate (std, plus serde derives for the worker transport) so it can move upstream into the nexrad crates; `core` re-exports its types, `core::storm_cells` decides which cut to detect on, when to ask for the displayed volume's columns (`volume_cuts`: one cut per tilt, complete or partial against the VCP) and caches results, `core::storm_tracks` feeds the tracker one volume at a time and keeps per-volume frames (with the tracker after each, so a late volume is inserted and the later ones replayed) so scrubbing and looping show stable IDs, `core::measure` snaps a distance-tool endpoint to a lettered track and follows it (with the closest-approach readout), and the decode worker's `detect` message runs it on a stored or live sweep, or `detect_composite` on the composite grid of every shown site (`storm_cells::composite`, on a fixed lattice), whose cells have no column and carry the start of the cut that shaped them, the time the tracker matches them at. | `storm_cells/mod.rs` |
| `net` | Unified retry policy (`with_retry`, `Verdict`, per-context policies) applied to every outbound HTTP request. | `net/retry.rs` |

### JavaScript / HTML

| File | Purpose |
|------|---------|
| `worker.js` | ES module Web Worker — dispatches `postMessage` commands to WASM exports |
| `service-worker.js` | App shell cache for the installed app and network metric collection |
| `index.html` | WASM entry point with Trunk build directives, service worker registration |
| `build.rs` | Version stamping + invokes the architecture ratchet (`tools/arch_check.rs`) |

## Architecture Enforcement

The module dependency graph is enforced by a **build-time ratchet**:
[`tools/arch_check.rs`](tools/arch_check.rs), invoked from `build.rs`, runs on
every `cargo check`/`cargo build`. It scans every `crate::<module>` reference in
`src/` (comments stripped) and fails the build when a cross-module edge appears
that is neither:

- **ALLOWED** — the intended layering (leaves `net`/`data`/`geo`/`storm_cells` import nothing;
  `core` may use the data/geo vocabulary and pure feed types; `nexrad` sits on
  `core`+leaves; `state` on the domain modules; `subsystem` on `state` and
  below; `app` on everything except `ui`; `ui` may read every layer), nor
- **GRANDFATHERED** — a known violation being burned down. Each row carries a
  reason and a burn-down pointer. Currently one edge remains:
  `app → ui` (the geolocation effect executor lives in ui; moves in Phase C2).

Reading a failure: the panic lists the offending edge (`from -> to`) with up to
8 `file:line` sites. Fix the dependency direction, or — only with a real
architectural justification — add the edge to `ALLOWED`. **Never add to
GRANDFATHERED.** The ratchet also fails when a grandfathered edge no longer
occurs, forcing its row to be deleted — the table only shrinks.

## Data Flow

Every pump runs once per site on the map, through that site's request
ledger, retries and prefetch settle, and every worker reply carries a scan
key (or a site id) that `core::sites::route` maps back to its site; a reply
for a site no longer on the map is dropped. The flows below are one site's.

### Archive Download (Primary Pipeline)
```
User selects site/date
  → AcquisitionCoordinator fetches AWS S3 listing
  → ArchiveIndex caches listing (radar volumes only: the hourly `_MDM`
    model-data file listed under a volume's own time is dropped, so it can
    never stand in for that volume as the scan under the playhead)
  → User selects scan (or range queued in the download queue)
  → Worker ingest: split records → decompress → decode → extract sweep blobs
    (an archive that splits as truncated is downloaded once more, quietly —
    core::acquisition::IngestRetries — and reported only if it fails again)
  → upsert_scan: pre-computed sweep blobs + scan-index entry into IndexedDB
  → Return sweep metadata to main thread
  → RenderCoordinator sends worker.render(scan_key, elevation, product)
  → Worker reads single sweep blob from IDB → marshals for transfer
  → Main thread uploads raw f32 data to GPU R32F texture
  → Fragment shader: polar→Cartesian + raw→physical conversion + LUT color
```

### Real-time Streaming
```
Start live mode
  → every site's RealtimeChannel (SiteState.live, a SiteLive) starts and
    spawns its own streaming_loop (nexrad/live/realtime/streaming/);
    subsystem::Live holds the one session phase over them
  → Loop: predict next chunk poll time (core ProjectionEngine) → sleep → fetch
  → Each chunk → worker.ingest_chunk → decode + accumulate radials in that
    site's accumulator (keyed by site id)
  → Completed elevations → sweep blobs stored to IDB (upsert_scan)
  → Partial elevations → worker.render_live (reads in-memory accumulator)
  → Storm cells (when on): worker.detect on the volume's lowest reflectivity
    cut — from the accumulator per chunk while the cut fills (provisional),
    then from the stored blob once it completes (final); then, as the volume's
    higher tilts are stored, worker.detect with `volumeCuts` for the cells'
    columns (provisional until every planned tilt is in)
  → The chunk that completes the cut being collected → normal `render` of the
    stored sweep blob (the accumulator no longer holds its radials, so the
    live partial lets go of that cut: `core::worker_ingest::live_cut`)
  → Main thread: core::worker_ingest reducer applies the outcome, describes
    GPU/render/status actions the shell executes
  → GPU texture updated per chunk → sweep line extrapolated between chunks
```

### Playback / Scrubbing
```
Timeline position changes
  → core::render_loop::reduce_advance_playback (pure) decides the sweep to show
  → RenderCoordinator dedup gate (core::render::should_dispatch vs SweepIdentity)
  → Worker.render(scan_key, elevation, product)
  → Reads pre-computed sweep blob from IDB (near-zero decode cost)
  → GPU texture upload → immediate re-render
```

Elevation / product changes take the same path — the dedup gate sees the
changed parameter and dispatches a fresh worker render.

## The Reducer Pattern (Env / Slices / Actions)

The decision mass that used to live inline in `src/app/` methods is extracted
into pure **reducers** in `core`, all following one shape. Using
[`core::worker_ingest`](src/core/worker_ingest.rs) as the exemplar:

1. The shell (`app/worker_results.rs`) assembles a read-only **Env** snapshot
   (`ChunkIngestEnv`: is_live, site, product string, frame time, coordinator
   elevations, frame projection) — everything the decision reads but doesn't
   own.
2. It passes mutable **Slices** over the core-owned state
   (`ChunkIngestSlices`: live-mode state, projection engine, elevation
   selection, playback) — the reducer mutates these in memory only.
3. The reducer (`reduce_chunk_ingested`) returns an **Actions** struct
   (`ChunkIngestActions`) *describing* every side effect: channel records, the
   live render dispatch, GPU texture promotion, status text, intents to queue,
   render-request flags.
4. The shell executes the actions **in field order**.

The same pattern backs `core::worker_decoded` (decode outcomes → GPU upload
decisions), `core::render_loop` (`reduce_advance_playback` +
`decide_prefetch_and_caption`, two decision points separated by effects), and
`core::acquisition` (the prefetch/listing pump reducers). Reducers are
unit-tested headlessly; the shells are mechanical assemble→call→execute
wrappers.

## Key Types

### Data Types
| Type | Where | Description |
|------|-------|-------------|
| `ScanKey` | `data/keys.rs` | Unique identifier: `SITE\|SCAN_START_MS` |
| `SweepDataKey` | `data/keys.rs` | Pre-computed sweep: `SITE\|SCAN_START_MS\|ELEV_NUM\|PRODUCT` |
| `ScanIndexEntry` / `CachedSweep` | `data/keys.rs` | Per-scan IDB metadata: VCP plan + realized cached sweeps |
| `ScanHeader` / `ElevationUpload` | `data/keys.rs` | Inputs to the `upsert_scan` write contract |
| `PrecomputedSweep` | `data/blob_format.rs` | The 72-byte-header sweep blob wire format |
| `ExtractedVcp` | `data/vcp_timing.rs` | VCP pattern data extracted from Message Type 5 |
| `ScanMetadata` | `core/domain/radar.rs` | Lightweight scan metadata for fast timeline queries |
| `Scan` / `Sweep` / `Radial` / `RadarTimeline` | `core/domain/radar.rs` | The in-memory radar timeline model |

### Rendering Types
| Type | Where | Description |
|------|-------|-------------|
| `RadarGpuRenderer` | `nexrad/render/gpu_renderer/` | WebGL2 renderer: polar data texture + LUT + fragment shader |
| `GlobeRadarRenderer` | `nexrad/render/globe_radar_renderer.rs` | Radar projection onto 3D globe surface mesh |
| `VolumeRayRenderer` | `nexrad/render/volume_ray_renderer.rs` | 3D volumetric ray-marching through all elevations |
| `SweepIdentity` / `VolumeRenderRequest` | `core/domain/viz.rs`, `core/render.rs` | Dedup identities checked by `core::render::should_dispatch` |

### Coordination Types
| Type | Where | Description |
|------|-------|-------------|
| `AcquisitionCoordinator` | `nexrad/acquisition/` | Owns download pipeline, archive index, cache load, download queue |
| `RenderCoordinator` | `nexrad/render/` | Owns decode worker dispatch; dedup via the pure `should_dispatch` gate |
| `RealtimeChannel` | `nexrad/live/realtime/` | Typed mailboxes + lifecycle of the live `streaming_loop` |
| `ProjectionEngine` | `core/projection/` | Single owner of forward-looking radar timing (plans, forecasts) |
| `PersistenceManager` | `app/persistence_manager.rs` | Shell over `core::decide_persist` — link push throttle + prefs save (tables in PRODUCT.md §13.4; the link carries the focus as `site=` and the other sites on the map as `also=`) |
| `NetworkMonitor` | `subsystem/network_monitor.rs` | Service worker metric listener (record types in `core/domain/telemetry.rs`) |

## Async Architecture

The application bridges async operations with egui's synchronous update loop using channel-based communication and per-frame polling.

### Channel Pattern
```rust
// Spawn async task
channel.start_operation(ctx.clone(), params);

// Poll each frame in update()
if let Some(result) = channel.try_recv() {
    handle_result(result);
}
```

### Web Worker

Heavy computation (bzip2 decompression, NEXRAD decoding, sweep extraction, IDB I/O) runs in a pool of Web Workers (`worker.js`) to keep the UI thread responsive. The pool is sized at startup (`default_pool_size`); commands are dispatched to the next available worker. Communication uses `postMessage` with Transferable ArrayBuffers for zero-copy data transfer.

| Operation | Direction | Purpose |
|-----------|-----------|---------|
| `init` | Main → Worker | Initialize with Trunk-generated WASM/JS URLs |
| `ingest` | Main → Worker | Full archive: split, decode, extract sweeps, store in IDB |
| `ingest_chunk` | Main → Worker | Real-time chunk: decode, accumulate in the site's own accumulator, flush completed sweeps |
| `render` | Main → Worker | Read pre-computed sweep from IDB, marshal for GPU upload |
| `render_volume` | Main → Worker | Pack all elevations for 3D ray-marching |
| `render_live` | Main → Worker | Read partial sweep from in-memory accumulator (synchronous); answered with `live_decoded`, or `live_superseded` when the elevation was stored after the request was sent (a no-op on the main thread) |
| `detect` | Main → Worker | Storm cell detection on one reflectivity cut (the volume's lowest, see `core::storm_cells`), read from IDB (`stored`) or from the live accumulator (`live`, pinned to the live worker; falls back to IDB once the cut is flushed); answered with `detected`: the cells and groups, their source cut, and the worker's fetch/detect timing. With `volumeCuts` (sent after the cells are shown, displayed volume only) the worker detects the stored cut again and samples those tilts over the footprints, returning the columns in `result.volume` |
| `detect_composite` | Main → Worker | Storm cell detection once on the composite grid of every shown site, used while more than one site is shown: each site's cut is read as for `detect`, resampled onto one 500 m Cartesian grid around the echo (highest dBZ where discs overlap, a mask of the sites with a valid gate) and detected there (`storm_cells::composite`); pinned to the live worker when any cut is live; answered with `detected`, whose azimuth and gate counts are the grid's rows and columns; no `volumeCuts`, so these cells have no column |
| `drop_site` | Main → Worker | A site left the map (or its element now points at another radar): forget its live accumulator. Pinned to the live worker and queued behind the chunk ingests already sent; no reply |

The Rust side of this protocol lives in `nexrad/decode/worker_api/` (the
`#[wasm_bindgen]` exports worker.js calls) and `nexrad/decode/decode_worker/`
(the main-thread pool + typed send/receive).

### GPU Raw Decode Pipeline

Gate values are stored as raw u8/u16 in NEXRAD archives. The physical conversion
`physical = (raw - offset) / scale` happens in the GPU fragment shader, which means:
- Raw values 0 (below threshold) and 1 (range folded) are sentinel values
- The shader checks `v > 1.5` to identify valid data
- Bilinear interpolation and smoothing work correctly on raw values because the
  linear transform is invariant under interpolation
- GPU uniforms `u_offset` and `u_scale` are set per-frame

### Platform-Specific Spawning
- **WASM**: `wasm_bindgen_futures::spawn_local()`
- **Native**: `std::thread::spawn()` + `pollster::block_on()` (development only)

## Caching Strategy

### Pre-computed Sweep Storage

During ingestion, radials are grouped by elevation and product, then serialized as
compact sweep blobs and stored in IndexedDB. At render time, the worker reads a single
blob and marshals it for GPU upload — no decompression or decoding needed. This gives
near-zero render latency for scrubbing and elevation changes.

### IndexedDB Schema

Database `nexrad-workbench`, schema version 5, with three string-keyed object
stores: `sweeps` (pre-computed sweep blobs, the primary render path), `scan_index`
(per-scan metadata for fast timeline queries), and `scan_touches` (per-scan
last-access timestamps for LRU eviction, isolated so fire-and-forget touch bumps
don't race index writes). All writes go through the single `upsert_scan`
create-or-merge entry point. Schema upgrades are destructive.

The full store layout, payload byte formats, concurrency model, and key-range
query rules are documented in [INDEXEDDB.md](docs/INDEXEDDB.md) — the single
source of truth for the schema.

### Scan Completeness States

| State | Description |
|-------|-------------|
| `Missing` | No records present |
| `PartialNoVcp` | Some records, no VCP metadata |
| `PartialWithVcp` | Some records with VCP (can determine expected count) |
| `Complete` | All expected records present |

### Three-Layer Cache

1. **IndexedDB** (persistent, WASM only)
   - Pre-computed sweep blobs and per-scan metadata
   - Configurable quota with LRU eviction
   - Survives page reload

2. **GPU textures** (video memory)
   - R32F data texture (azimuths x gates) for current sweep
   - RGBA8 LUT texture for color mapping
   - Content-signature-based invalidation

3. **In-memory accumulator** (worker only, live mode)
   - One `ChunkAccumulator` per streaming site holds its partial sweeps
   - Flushed to IDB when elevations complete
   - Readable via `render_live` for immediate partial display

## State Management

`WorkbenchApp` (in `main.rs`) is a thin coordinator: it owns `AppState` plus the
bounded subsystems, and its `update()` runs the frame sequence — frame setup,
channel drains (via the core reducers), command dispatch, effect execution,
then UI layout.

- **`AppState`** (`state/mod.rs`) holds cross-cutting state: the per-frame
  clock (`frame_now`), viz/layer state, the toast queue, session stats, storage
  settings, saved events, theme, mobile/width-tier resolution, the recent-error
  ring, and the **command queue** (`VecDeque<Intent>`).
- **Subsystems** (`subsystem/`) own their domains: `Acquisition` (download
  pipeline + operation tracking), `Render` (the worker pool), `Timeline`
  (the storm-activity book), `Playback` (cursor/speed/mode),
  `Live` (the live session: phase, exit reason, detach latch and the
  per-frame `SessionVm`; each site's channel and live models are its own
  `SiteLive`),
  `Chrome` (UI visibility flags + modal booleans), `Diagnostics` (alerts,
  mPING, GPS, network monitor), and `Derived` (the per-frame view-model cache).
- **`Sites`** (`subsystem/sites.rs`) holds one `SiteState` per radar and a
  `focus` index. A `SiteState` owns what differs between radars: the id and
  position, the scan inventory and archive shadows, the render track
  (`core::render::RenderTrack`), the crossfade and scrub caches, the displayed
  frame and its age, storm cells, the GPU renderers (`SiteGpu`; the GL
  context, globe and geo lines stay on `GpuResources`), and the request
  ledger, retries and prefetch settle, and its live stream (`SiteLive`: the
  `RealtimeChannel`, the volume in progress, the projection engine and the
  per-frame live models). `Sites::scene` (`SceneCells`) holds the storm cells
  detected on the composite grid while more than one site is shown, with one
  tracker for the whole map. Worker replies find their site with
  `core::sites::route` and are dropped when it is gone; per-frame shells loop
  over the list, and single-valued surfaces (ladder, readouts, timeline,
  links, preferences) read the focus; `focus` is only an index into the
  list, and the single-site app is a list of one. The list holds up to
  `core::sites::cap` sites (three on a desktop, one on a phone); the
  `AddSite`, `RemoveSite`, `SetFocus` and `OnlyFocus` intents change it
  through the pure decisions in `core::sites`.
- **The map centre** (`VizState::map_centre`) is its own value, apart from
  every radar's position: `SelectSite`, `SetFocus` and the boot (from the
  link's `lat`/`lon`, else the focus site) set it through
  `VizState::centre_map_on`. The camera, the link's `lat`/`lon`, the scale
  bar and the mPING query read it; whatever means "where is the radar"
  reads the site.

### Intent Pattern
State mutations from UI actions are expressed as `Intent` variants
(`core/intent.rs` — defined in the core; there is no separate `AppCommand`
anymore). UI code pushes intents via `AppState::push_command`; the main update
loop drains and dispatches them (`app/command_dispatch.rs`). Decisions the
dispatch needs are pure core functions; the side effects they describe are
executed by the `Effect` runtime (`app/effects.rs`).

## UI Layout

```
┌──────────────────────────────────────────────────────────┐
│ Top Bar: site · product · tilt · frame time + age pill   │
├──────────┬─────────────────────────────┬─────────────────┤
│ Left     │ Canvas                      │ Right Panel     │
│ Panel    │ (a disc per site + Geo      │ • Product       │
│ (Radar   │  layers + Overlays)         │ • Palette       │
│  Ops)    │                             │ • Layers        │
│          │                             │ • Processing    │
│          │                             │ • 3D Options    │
├──────────┴─────────────────────────────┴─────────────────┤
│ Bottom: Timeline | Playback Controls | Activity chip     │
│         Track (focus) + one 11 px lane per other site    │
└──────────────────────────────────────────────────────────┘
```

**Acquisition transparency** is one surface, not several. The always-visible
activity chip (`ui/activity_chip.rs`, shared by the desktop and phone
transport rows; the phone folds its words) opens the activity sheet
(`ui/activity_sheet.rs`), which carries the stage strip, throughput, download
list, the recent errors other than downloads, policy toggles, and a collapsed
Details disclosure. The chip's ⚠ tick counts failed downloads and the errors
the sheet has not shown yet (`core::activity::ErrorSummary`); with more
than one site on the map a decode error that knows its site leads with it
(`KMLB · Decode worker: …`), and the Finishing stage sums every site's
request ledger (`Sites::ledger_summary`, `Sites::awaiting_scans`). Both render a single per-frame projection,
`core::activity::ActivityVm`, so they cannot disagree. The former dev-only
surfaces — the DL/PROC/GPU pipeline lamps, the Performance modal, the network
log modal, and the expandable acquisition drawer — were folded into it; what
was genuinely dev-only now lives inside Details behind `dev_mode`.

**The top bar** (`ui/top_bar.rs`, `ui/mobile/top_bar.rs`) says what is on
screen and nothing else: site, product and tilt pickers, the frame time in the
chosen zone (the zone label toggles local/UTC) and the age pill. Both bars draw
one per-frame `core::top_bar::TopBarVm`; the width rule (`core::top_bar::fold`)
folds the city, then the zone, then help and 2D/3D into ⋯. Refusals,
confirmations and failures with no other home go to the **toast** under the bar
(`core::toast::ToastQueue` on `AppState::toasts`, drawn by `ui/toast.rs`).

When the layout is the phone's (`core::phone::device_layout`, in portrait and
landscape), `ui/mobile/` replaces this desktop layout with the same instrument
at phone width: the top bar, a 120 px dock (the row, `ui/timeline/mini.rs`, over
the transport row, `ui/mobile/tabs.rs`), and a sheet with the rail's four tabs
(bottom in portrait, right in landscape). The phone's decisions — layout, sheet
rect, chrome auto-hide, press tracking, the row's window, pick radii and the
site-switch confirmation — are pure and tested in `core::phone`,
`core::canvas` and `core::dialogs`.

The row is a row of volumes you can touch. `core::phone_strip` owns its three
zoom stops (NOW, 3 H, 24 H; `Intent::SetPhoneStripZoom`, from a pinch on
release or the transport row's segmented control; the stop lives on `Chrome`,
not in preferences or the link), their windows (`strip_window`: pinned, the
window ends just past now; detached, it centres on a playhead that lands off
it, holds still while the finger works inside it and slides near the edges;
at NOW with a range selected it follows the range, `cuts_range_window`), the
scrub and tap landings (`snap_scrub` per cut where cuts are known and just
inside a volume's start elsewhere, `StripUnits::landing_units`; `tap_landing`
a volume's first cut) and the touch (`StripTouch`: press, long-press, pinch;
a still finger never seeks again). `core::phone_row::row_vm` decides
everything drawn, in the row's own points: at NOW one block per volume
(containers and listed volumes no container covers; x from the start, width
the span less a 3 px gap; filled on the device with cut separators, hollow
not, white-outlined under the playhead, the live one growing to now and
filled by its cuts received over expected), with start-time labels that give
way to each other; at 3 H 11 px columns, 6 px plus the volume's storm share
(over the view's `ViewScale`) times 26 px, with hour labels; at 24 H the
storm hill through the half-hour samples (`activity_bars`), a monotone
cubic (`monotone_cubic`) sampled every 2 px into slivers the shell fills as
quads in one mesh, the held stretch in slate, with a white line at the playhead and labels at 06/12/18/00;
at every stop the red now line with its `NOW` cap and one amber bracket over
a selected range. It also decides the loading state (`LoadingVm`): while the
selection is not pinned, the volume under the playhead is not on the device
and the frame on screen is another volume's, the selected block carries an
`Arc` (the download's stage from the acquisition's queued / active /
in-flight spans, and while it downloads the bytes received over the
response's `Content-Length` or the listing's size, from the
`DownloadBytes` the streaming archive GET in `nexrad::acquisition::download`
reports chunk by chunk; spinning when none is known). The strip writes that to
`Chrome::phone_loading` each frame; the canvas reads it for a 40 % dim and a
`Loading 23:49` pill (the phone draws no canvas caption), the chip for its
dashed outline and the top bar for its clock. `core::phone_dock` holds the
rest of the phone chrome's words: the chip's one form per state
(`phone_chip`: `Live`, `23:49`, `Last 4 frames`, `11:28 → 11:34`), the LIVE
button's intents (`live_button`, only while not pinned; it brings now into
view first when the row does not show it), the top bar's one clock
(`phone_clock`, dated only when not today), the age pill (`phone_age`) and
the bell (`phone_bell`, the alerts with a dot when something failed; the dot
takes the tap to the activity sheet). The row's press area reaches 12 px up
into the map, which neither pans on that press nor zooms on the row's pinch.

The phone's overview is the calendar sheet, "Go to a day"
(`ui/go_to_day.rs` on the dialog component's `Sheet` and the Esc
stack; its state, `Chrome::go_to_day`, changes only through
`Intent::OpenGoToDay` / `Intent::Calendar`). Everything it shows is
`core::calendar::calendar_vm`: the Monday-first month grid with each day's
storm level from the `ActivityBook` (index days, scaled over the month by
`ViewScale`; unknown is not quiet), the on-device flag from the scan index's
held spans, today and future flags in the chosen zone (handed in as a
`calendar::Zone`), the year and years views clamped to the archive and the
site's index range, and the swipe rule. It makes no S3 request. A tapped day
(`Intent::GoToDay`) stops a running stream in place
(`calendar::landing_stops_stream`, so the transport row offers LIVE), sets the
row to its 3 H stop (`strip_zoom_after`), seeks as a typed seek does and pauses, so the row centres on the landing
volume with its columns; `calendar::land_on_day` picks the day's stormiest
volume from `ActivityBook::listing` (the per-day listing the strip asks for
at 3 H and DAY), or local
midday plus a pending `Landing` that `app::calendar::settle_go_to_day`
finishes once per frame with one re-seek; a landing volume not yet held
waits just inside its listed start, as the strip lands a volume with no
known cuts (`phone_strip::volume_landing`, `Landing::volume`), and moves onto its first frame's end
once ingested, so a frame shows. The shell applies those re-seeks directly
(a queued `Seek` would read as a touch); the landing is dropped by
`landing_after` on any playhead intent or by a moved or playing playhead.

The desktop draws the same calendar as a `Popover` above the transport
clock's calendar button (also G and the chip menu's "Go to a day…"). A
picked day also runs `calendar::frame_day`: the overview at its default 48 h
around the day and the strip showing the whole day, cut at now, as a click
on an overview bar does. Under the grid sits the clock's time field; a time
typed there is `Intent::GoToTime`, which stops a stream in place, seeks as a
typed seek, pauses and frames the overview around it
(`calendar::frame_time`).

Ranges on the phone are never drawn (`core::phone_range`). A long-press on
the strip emits `Intent::OpenPressSheet` with the snapped time and the scan
under the finger; the sheet (`ui/mobile/press_sheet.rs`, a `Sheet` on the Esc
stack as `DialogId::PressSheet`) draws `press_sheet_vm` — the heading and
four rows, each disabled with its reason — and a tapped row emits
`Intent::PressSheetRow`, which the dispatcher turns into `row_intent`'s
intent: `InspectScan`, `SelectRange` (so `transport::select_range` applies
the "ends at now" pin and the paused start), `ParkRangeStart` (a half range,
as the I key parks one) or `SaveEventAt` / `SaveSelectionAsEvent` (the event
dialog's prefill is `dialogs::event_prefill`). On the row a range is one
amber bracket (`phone_row::Bracket`); its ends are not draggable.
The selection-fetch gate asks over `acquisition::bulk_confirm_secs` — 6 h on
the desktop, 2 h on the phone — and the confirmation shows
`range_download_text` from a `RangeDownloadPrompt` priced at release.

Canvas overlays live in `ui/canvas_overlays/` — one module per overlay
(national mosaic, alerts, mPING reports, GPS location, sweep line/donut, site
markers, info text, color scale, scale bar, compass, globe). `ui/canvas.rs`
owns the draw order and the radar texture pass: every site's disc, older
frame first (`core::sites::draw_order`), each with its own Rays marks, the
focus's marks on top. In 3D, `ui/canvas_overlays/globe.rs` draws every site's
surface patch, then their volumes back to front from the camera
(`core::volume_plan::globe_draw_plan`); `core::volume_plan::resident_volumes`
decides which sites may hold a volume texture (three on a desktop, one on a
phone).

## Platform Support

The codebase targets WASM primarily, with native stubs for development.

```toml
# .cargo/config.toml
[build]
target = "wasm32-unknown-unknown"
```

Conditional compilation via `#[cfg(target_arch = "wasm32")]` gates:
- IndexedDB storage
- Web Worker communication
- Async spawning mechanism
- Browser-specific APIs (js-sys, web-sys)

## Build

```bash
# Development server with hot reload
trunk serve

# Production build
trunk build --release

# Check only (no bundle) — also runs the architecture ratchet
cargo check
```

Pre-commit hooks enforce `cargo fmt`, `cargo clippy -D warnings`, and the fast
headless test suite (`cargo test --bin nexrad-workbench`) via cargo-husky.
`#![warn(unreachable_pub)]` is enabled crate-wide (the `data` module is
exempted as the lib facade consumed by `tests/idb.rs`).
