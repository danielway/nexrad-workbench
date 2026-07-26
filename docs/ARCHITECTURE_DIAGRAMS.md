# NEXRAD Workbench — Architecture in Diagrams

A visual, diagram-first companion to [ARCHITECTURE.md](../ARCHITECTURE.md). The
prose doc is the canonical *engineering map* (every file and its purpose); this
doc shows **how the pieces relate structurally** and **how data flows at
runtime**. Diagrams are Mermaid (rendered inline by GitHub and most Markdown
viewers).

Deep dives live in the subsystem references — this doc links into them rather
than duplicating them:
[CORE_SHELL.md](CORE_SHELL.md) (the architecture standard),
[RENDERING.md](RENDERING.md) (GPU pipeline + 3D),
[STREAMING.md](STREAMING.md) (live sequencing),
[TIMING.md](TIMING.md) (the three time categories),
[INDEXEDDB.md](INDEXEDDB.md) (cache schema).

**Reading order:** §1 context → §2 threading model → §3 the architecture
standard → §4 module map → §5 state ownership → §6 the frame loop → §7–10 the
runtime pipelines → §11–13 the leaf subsystems (storage / GPU / UI).

---

## 1. System Context

NEXRAD Workbench is a **100% client-side** Rust→WASM application. There is no
backend of our own; the browser talks directly to public data services.

```mermaid
flowchart TB
    user(["👤 User<br/>(desktop / mobile browser)"])

    subgraph browser["Browser tab"]
        app["NEXRAD Workbench<br/>(Rust → WASM, egui + WebGL2)"]
        idb[("IndexedDB<br/>nexrad-workbench v5")]
        ls[("localStorage<br/>preferences, timing stats")]
        sw["service-worker.js<br/>COOP/COEP + net metrics"]
    end

    s3["AWS S3<br/>noaa-nexrad-level2<br/>(archive + realtime chunks)"]
    nws["NWS API<br/>api.weather.gov<br/>(active alerts)"]
    mping["mPING API<br/>(crowd-sourced reports)"]
    mrms["NOAA MRMS WMS<br/>(CONUS mosaic PNG)"]

    user <--> app
    app <--> idb
    app <--> ls
    app -.COOP/COEP, metrics.-> sw
    app -->|HTTPS GET| s3
    app -->|HTTPS GET| nws
    app -->|HTTPS GET| mping
    app -->|HTTPS GET| mrms

    classDef ext fill:#2d3b55,stroke:#7aa2f7,color:#fff;
    class s3,nws,mping,mrms ext;
```

| External service | Used for | Code |
|---|---|---|
| **AWS S3** (`noaa-nexrad-level2`) | Archive volume scans + realtime chunk stream | `nexrad::download`, `nexrad::realtime` |
| **NWS** (`api.weather.gov`) | Active severe-weather alert polygons | `alerts/` |
| **mPING** | Crowd-sourced ground-truth precip reports | `mping/` |
| **NOAA MRMS WMS** | CONUS composite reflectivity overlay | `nexrad::national_mosaic` |

Everything else — decode, projection, rendering, caching — happens locally.
Cross-origin isolation (COOP/COEP via the service worker) unlocks
`SharedArrayBuffer`; the app degrades gracefully if it's unavailable.

---

## 2. Threading Model — Thin Shell + Fat Worker

The single most important structural fact: **the main thread is a thin UI shell;
all heavy data work runs in a pool of Web Workers.** They communicate by
`postMessage` with *Transferable* `ArrayBuffer`s (zero-copy).

```mermaid
flowchart LR
    subgraph main["🖥️ Main thread (UI shell)"]
        direction TB
        egui["egui update loop<br/>(synchronous, 60fps)"]
        coord["Coordinators<br/>Acquisition · Render · Live"]
        gpu["WebGL2 / glow<br/>GPU texture upload + shaders"]
        egui --> coord
        coord --> gpu
    end

    subgraph pool["⚙️ Web Worker pool (worker.js × N)"]
        direction TB
        w0["Worker 0<br/>(pinned: live chunks)"]
        w1["Worker 1"]
        wN["Worker N…<br/>(round-robin: ingest/render)"]
    end

    store[("IndexedDB<br/>(shared, each worker<br/>has its own connection)")]

    coord -->|"postMessage<br/>ingest · render · render_live<br/>(Transferable ArrayBuffers)"| pool
    pool -->|"postMessage<br/>decoded · ingested<br/>(Transferable ArrayBuffers)"| coord
    pool <-->|"read/write<br/>sweep blobs"| store
    gpu -->|"R32F texture<br/>upload"| screen([Canvas / screen])

    s3e["AWS S3"]
    pool -->|realtime chunks| pool
    coord -->|archive HTTPS| s3e
```

What runs where:

| Main thread (shell) | Worker pool (fat) |
|---|---|
| egui panels, canvas painting, input | bzip2 decompress, NEXRAD decode |
| Coordinators + channel polling | sweep extraction → pre-computed blobs |
| **GPU** texture upload + shader passes | **IndexedDB** blob I/O |
| Archive S3 download (async channel) | live-chunk accumulation (`CHUNK_ACCUM`) |
| dedup / prefetch *decisions* (pure) | marshaling raw gates for GPU upload |

**Pool sizing & dispatch** (`nexrad::decode_worker::pool`):
`default_pool_size() = clamp(hw_concurrency − 1, 1..=4)`.
- `ingest`, `render`, `render_volume` → **round-robin** (each worker has its own
  IDB connection, so parallel decompress fans out across cores).
- `ingest_chunk`, `render_live` → **pinned to worker 0** — the live accumulator
  is a thread-local (`CHUNK_ACCUM`) that must stay consistent across a volume's
  chunks.

Each worker correlates requests by id via per-worker `pending_*` maps; the main
thread drains all workers each frame with `WorkerPool::try_recv() -> Vec<WorkerOutcome>`.

---

## 3. The Architecture Standard — Functional Core, Thin Shell

This is the **binding rule** for all code (full reference:
[CORE_SHELL.md](CORE_SHELL.md)). Three layers; only two things ever cross the
boundary: **intents in, view-model out** (effects are *described* by the core and
*executed* by the shell).

```mermaid
flowchart LR
    input(["user input"]) -->|intent| core
    core -->|view-model| paint["egui / canvas paint"]
    core -->|effect| runtime
    runtime -->|result → new intent| core
    runtime -->|IO| io[("IndexedDB · HTTP<br/>Worker · GPU · localStorage<br/>geolocation · URL · timers")]

    subgraph core["🧠 FUNCTIONAL CORE (pure)"]
        direction TB
        c1["state + business logic<br/>(state, intent) → (next state, effects)"]
        c2["src/core/** + pure fns in src/state/**<br/>no egui · no web-sys · no await · no IO"]
    end

    subgraph runtime["⚡ EFFECT RUNTIME (imperative shell)"]
        direction TB
        r1["WorkbenchApp::apply_effects<br/>executes Effect values"]
    end

    classDef pure fill:#1f3b2d,stroke:#73daca,color:#fff;
    class core pure;
```

The test recipe is the whole point — **a feature test never touches a browser:**

```rust
let mut core = Core::new(/* fixture */);
let effects = core.handle(Intent::SeekTo(ts));            // send an intent
assert_eq!(core.view_model().displayed_frame_ts, ts);     // assert the projection
assert!(effects.contains(&Effect::RenderSweep { .. }));   // assert the IO it ASKED for
// the effect is NOT executed — we assert the DECISION, not the side effect
```

### The four seams in this codebase

```mermaid
flowchart TB
    ui["UI shell — src/ui/**<br/>(panels, canvas, overlays, modals)"]
    cmd["① AppCommand / Intent<br/>= intents in"]
    derived["② subsystem::Derived + *Vm<br/>= view-model out"]
    owners["③ 7 subsystems + AppState<br/>= state owners"]
    eff["④ core::Effect (+ local action enums<br/>like PrevSweepAction)<br/>= effects out"]
    rt["Effect runtime<br/>app::effects::apply_effects"]

    ui -->|emits| cmd
    ui -->|reads| derived
    cmd --> owners
    owners --> derived
    owners -->|core returns| eff
    eff --> rt
    rt -->|IO results feed back as intents| cmd
```

| Seam | Role | Where |
|---|---|---|
| `AppCommand` (→ `Intent`) | intents in | `state::AppCommand`, `core::intent` |
| `subsystem::Derived` + `*Vm` | view-model out | `subsystem::derived`, `core::diagnostics::DiagnosticsVm` |
| 7 subsystems + `AppState` | state owners | `subsystem/**`, `state/**` |
| `core::Effect` + local action enums | effects out | `core::effect`, `core::playback_manager::PrevSweepAction` |

**Migration status:** P0–P4 + P6 complete, P5 partial (pure logic extracted; the
broad interactive `&mut`→intent rewrite is QA-gated). Carve-outs: GPU paint is
irreducibly shell; camera/projection math is already pure; `realtime/streaming.rs`
is de-scoped. See [CORE_SHELL.md](CORE_SHELL.md) for the per-phase detail.

---

## 4. Module Map

The crate is layered top-down: the **shell** orchestrates, the **UI** projects,
**subsystems** own state, the **core** decides (pure), **domain/services**
implement features, and **foundation** is the leaf.

```mermaid
flowchart TB
    subgraph L0["Entry / Shell — orchestration"]
        main["main.rs<br/>WorkbenchApp + update() frame loop"]
        appm["app/**<br/>frame-loop impl methods + effect runtime"]
    end
    subgraph L1["UI shell — projection only"]
        ui["ui/**<br/>panels · canvas · overlays · timeline · mobile · modals"]
    end
    subgraph L2["Subsystems — bounded state owners"]
        sub["subsystem/**<br/>Acquisition · Render · Live · Timeline · Playback · Chrome · Diagnostics · Derived"]
    end
    subgraph L3["Functional core — pure decisions"]
        core["core/**<br/>intent · effect · persist · diagnostics · canvas · render · panels · acquisition"]
    end
    subgraph L4["Domain state — data + pure fns"]
        state["state/**<br/>AppState · playback · viz · live_mode · radar_data · …"]
    end
    subgraph L5["Domain services"]
        nexrad["nexrad/**<br/>download · worker · decode · gpu_renderer · realtime · projection · timing · detection"]
        geo["geo/**<br/>camera · projection · renderers · layers"]
        alerts["alerts/**"]
        mpingm["mping/**"]
    end
    subgraph L6["Foundation — leaves"]
        data["data/**<br/>indexeddb · facade · keys · quota · sites · vcp"]
        net["net/**<br/>retry policy"]
    end

    main --> appm --> ui
    appm --> sub
    ui --> sub
    sub --> core
    sub --> nexrad
    sub --> geo
    core --> state
    core --> data
    state --> data
    nexrad --> data
    nexrad --> net
    nexrad --> geo
    geo --> state
    alerts --> net
    mpingm --> net
    appm --> alerts
    appm --> mpingm
```

| Module | Responsibility |
|---|---|
| `main.rs` / `app/**` | App entry, `update()` frame loop, effect runtime, frame-loop methods |
| `ui/**` | egui panels, canvas + overlays, timeline, mobile chrome, modals — *projection + intents only* |
| `subsystem/**` | 7 bounded contexts that own a coherent slice of state + a typed API |
| `core/**` | Pure decision functions: the home of the architecture contract |
| `state/**` | `AppState` data tree + already-pure decision helpers (~533 tests live here) |
| `nexrad/**` | The data pipeline: acquire → worker → decode → cache → render + 3D + timing + detection |
| `geo/**` | Camera, projection, geographic feature + globe rendering |
| `data/**` | IndexedDB, sweep storage facade, key types, quota, site/VCP tables |
| `alerts/**`, `mping/**`, `net/**` | NWS alerts, mPING reports, shared HTTP retry policy |

---

## 5. State Ownership

`WorkbenchApp` is a thin coordinator. State is split across **`AppState`** (the
shared data tree) and **7 subsystems** (bounded contexts), plus the GPU resources
and a few shell-only fields.

```mermaid
flowchart TB
    app["WorkbenchApp<br/>(thin coordinator)"]

    subgraph subs["7 subsystems (bounded state owners)"]
        acq["Acquisition<br/>download pipeline + queue + op tracking"]
        rend["Render<br/>worker pool + dedup + prev-sweep cache"]
        live["Live<br/>streaming channel + LivePhase + projection engine"]
        tl["Timeline<br/>scan inventory + shadow boundaries"]
        pb["Playback<br/>cursor · speed · loop · realtime lock"]
        chr["Chrome<br/>sidebar/modal visibility flags"]
        diag["Diagnostics<br/>alerts · mPING · GPS · network monitor"]
    end

    state["AppState<br/>viz_state · layer_state · render_processing<br/>session_stats · national_mosaic · saved_events<br/>theme · is_mobile · frame_now · commands queue"]

    gpu["GpuResources<br/>RadarGpuRenderer · Globe · GeoLine<br/>GlobeRadar · VolumeRay + GL context"]

    misc["persistence · modals · geo_layers · last_favicon_mode"]

    app --> subs
    app --> state
    app --> gpu
    app --> misc

    derived["subsystem::Derived (per-frame view-model)<br/>frame_now_secs · visible_bounds<br/>data_is_live · effective_sweep_animation"]
    state -.->|"for_frame()"| derived
    pb -.-> derived
```

**Per-frame view-model.** `Derived::for_frame(&state, &playback)` is computed once
near the top of the frame and once before panels render, so every UI consumer
reads identical values (no intra-frame drift). It carries the small set of
cross-cutting facts panels need: `frame_now_secs`, `visible_bounds`,
`data_is_live`, `effective_sweep_animation`. Richer per-feature view-models
(e.g. `DiagnosticsVm`) are built the same way.

**`AppMode`** (`Idle` / `Archive` / `Live`) is *derived* each frame from live
state and drives the favicon color + title prefix.

---

## 6. The Per-Frame Update Loop

`WorkbenchApp::update()` runs ~60×/sec. The order is **load-bearing** — comments
in `main.rs` flag the invariants. Stages group into six phases:

```mermaid
flowchart TB
    start([egui calls update])

    subgraph s1["① PER-FRAME SETUP"]
        a1["apply_frame_setup<br/>capture frame_now · theme · staleness · storm cells"]
    end
    subgraph s2["② INTAKE — drain all inputs"]
        b1["dispatch_commands → CommandOutcome"]
        b2["handle_worker_results<br/>(ingested · decoded · live · errors)"]
        b3["pump_download_queue (consumes CommandOutcome)"]
        b4["handle_streaming_results (realtime chunks)"]
        b1 --> b2 --> b3 --> b4
    end
    subgraph s3["③ BACKGROUND TICKS"]
        c1["national_mosaic.poll_tick"]
        c2["diagnostics.tick (alerts · mPING poll)"]
    end
    subgraph s4["④ COMPUTE"]
        d1["tick_live (pin playhead to now / slide lookback)"]
        d2["reconcile_tier · advance_playback"]
        d3["pump prefetch · lookback · listings · selection"]
        d4["sync_prev_sweep_texture"]
        d5["request_render_if_needed (dedup → worker.render)"]
        d6["update_network_stats · persist_url_state"]
        d1 --> d2 --> d3 --> d4 --> d5 --> d6
    end
    subgraph s5["⑤ FRAME SNAPSHOT"]
        e1["live.refresh → LiveRadarModel + AppMode"]
        e2["refresh_mobile_mode · drain GPS · favicon"]
        e3["Derived::for_frame (2nd snapshot)"]
        e1 --> e2 --> e3
    end
    subgraph s6["⑥ RENDER"]
        f1["render_layout — Layer registry (chrome + modals)"]
        f2["render_canvas_with_geo — GPU radar + overlays"]
        f3["handle_shortcuts"]
        f1 --> f2 --> f3
    end

    start --> s1 --> s2 --> s3 --> s4 --> s5 --> s6 --> done([repaint])
```

**Why the order matters (key invariants):**
- ② `dispatch_commands` precedes `pump_download_queue` (the pump consumes its
  `CommandOutcome`); the pump waits until *after* `handle_worker_results` so
  newly-decoded sweeps are visible before download decisions.
- ④ `advance_playback` precedes `request_render_if_needed` so a new playhead
  position triggers a render *the same frame* (no one-frame lag).
- ⑤ snapshots (`live.refresh`, `Derived::for_frame`) run before ⑥ panels read
  them.
- ⑥ side/top/bottom panels render before the CentralPanel canvas (egui layout
  requirement); modals layer last.

---

## 7. The Core Data Pipeline — Acquire → Ingest → Render → Display

The canonical archive path. Note the **pre-computation trick**: sweep blobs are
built once at ingest and stored GPU-ready, so later renders (scrub, elevation
change) are near-zero-cost reads.

```mermaid
flowchart LR
    subgraph acquire["ACQUIRE (main + S3)"]
        sel["user picks site/scan<br/>(or range selection)"]
        dq["DownloadQueueManager<br/>enqueue → advance"]
        dl["DownloadChannel<br/>S3 GET"]
        sel --> dq --> dl
    end
    subgraph ingest["INGEST (worker, round-robin)"]
        sp["split LDM records"]
        dc["decompress (bzip2) + decode"]
        gr["group radials by elevation"]
        vcp["extract VCP (msg type 5)"]
        blob["build PrecomputedSweep blobs<br/>(per elevation × product)"]
        sp --> dc --> gr --> vcp --> blob
    end
    store[("IndexedDB<br/>sweeps · scan_index · scan_touches")]
    subgraph render["RENDER (worker, dedup)"]
        ded["RenderCoordinator<br/>should_dispatch dedup"]
        rd["worker_render:<br/>read 1 blob → marshal<br/>Float32Array (zero-copy)"]
        ded --> rd
    end
    subgraph display["DISPLAY (main, GPU)"]
        up["upload raw gates → R32F texture"]
        sh["fragment shader:<br/>polar→Cartesian + raw→physical + LUT"]
        up --> sh
    end

    dl -->|"DownloadResult bytes"| sp
    blob -->|"store_sweeps"| store
    blob -.->|"metadata back"| ded
    store -->|"get_sweep"| rd
    rd -->|"Transferable buffer"| up
    sh --> px([radar disc on canvas])
```

Two acquisition triggers feed the same queue:
- **Explicit** — user taps a scan (`FetchScan`) or finalizes a timeline range.
- **Reactive** — `pump_implicit_prefetch` fetches scans near the playhead
  (debounced so scrub/zoom transients don't download); `pump_visible_listings`
  keeps S3 listings (→ timeline "shadow" boundaries) populated for the visible
  date range.

---

## 8. Sequence — Archive Download & First Render

```mermaid
sequenceDiagram
    autonumber
    actor U as User
    participant Main as Main thread<br/>(coordinators)
    participant S3 as AWS S3
    participant W as Worker (round-robin)
    participant IDB as IndexedDB
    participant GPU as GPU / shader

    U->>Main: select scan (FetchScan / range)
    Main->>Main: DownloadQueueManager.enqueue + advance
    Main->>S3: download_archive(site, date, file)
    S3-->>Main: DownloadResult (compressed bytes)
    Note over Main: try_recv_download() polled each frame

    Main->>W: postMessage "ingest" (Transferable bytes)
    activate W
    W->>W: split → decompress → decode → group → extract VCP
    W->>W: build PrecomputedSweep blobs (elev × product)
    W->>IDB: store_sweeps + upsert scan_index (+ seed touch)
    W-->>Main: postMessage "ingested" (scan_key, elevations, vcp, timing)
    deactivate W

    Main->>Main: RenderCoordinator.set_scan(key, elevations)
    Main->>Main: request_render_if_needed → SweepIdentity
    Main->>Main: should_dispatch(identity, last_render)? ✓ new

    Main->>W: postMessage "render" (scan_key, elev, product)
    activate W
    W->>IDB: get_sweep(SweepDataKey)
    IDB-->>W: ArrayBuffer (72B header + azimuths + gates)
    W->>W: parse header, marshal Float32Array views (zero-copy)
    W-->>Main: postMessage "decoded" (Transferable buffers)
    deactivate W
    W--)IDB: touch_scan (fire-and-forget, throttled 60s)

    Main->>GPU: upload raw gates → R32F texture
    GPU->>GPU: polar→Cartesian + (raw−offset)/scale + LUT color
    GPU-->>U: radar disc rendered
```

---

## 9. Sequence — Scrub / Elevation / Product Change (the fast path)

The whole reason for pre-computed blobs: changing the playhead, elevation, or
product **never re-decodes** — it's a dedup check, a single IDB read, and a GPU
upload. The pure `should_dispatch` gate suppresses redundant requests.

```mermaid
sequenceDiagram
    autonumber
    actor U as User
    participant PB as Playback / core::canvas
    participant RC as RenderCoordinator
    participant W as Worker
    participant IDB as IndexedDB
    participant GPU as GPU

    U->>PB: scrub timeline / change elevation / change product
    PB->>PB: advance_playback updates playhead
    PB->>PB: select_gpu_sweep (0–2 rule: which sweep matches?)
    PB->>RC: request_render_for(SweepIdentity{scan,elev,product})

    alt identity == last_render
        RC--xRC: should_dispatch=false → suppress (no work)
    else new identity
        RC->>RC: last_render = identity
        RC->>W: render(scan, elev, product)
        W->>IDB: get_sweep(key)
        IDB-->>W: blob
        W-->>RC: decoded (Transferable)
        RC->>GPU: upload → R32F texture
        GPU-->>U: re-render (near-zero latency)
    end
```

---

## 10. Live Streaming

### 10a. LivePhase state machine

```mermaid
stateDiagram-v2
    [*] --> Idle
    Idle --> AcquiringLock: StartLive / ReturnToLive
    AcquiringLock --> Streaming: streaming started (got volume)
    AcquiringLock --> Error: acquire timeout / network fail
    Streaming --> WaitingForChunk: plan has next_target
    WaitingForChunk --> Streaming: chunk arrives
    Streaming --> Idle: stop (UserStopped)
    WaitingForChunk --> Idle: stop (UserStopped)
    Streaming --> Idle: detached too long (DetachedTimeout)
    Streaming --> Error: decode / connection failure
    Error --> Idle: stop / retry
    Idle --> [*]

    note right of Streaming
        detached = playhead left the live edge
        (user scrubbed away). Stream keeps
        ingesting in background up to
        LIVE_DETACHED_STOP_SECS (60 min)
    end note
```

### 10b. Streaming pipeline sequence

```mermaid
sequenceDiagram
    autonumber
    participant UI as Main thread
    participant SL as streaming_loop<br/>(spawn_local)
    participant S3 as AWS S3 (realtime)
    participant Eng as ProjectionEngine
    participant W0 as Worker 0 (pinned)
    participant IDB as IndexedDB
    participant GPU as GPU

    UI->>SL: RealtimeChannel.start(site)
    activate SL
    SL->>S3: get_latest_volume(site) (binary search buckets)
    S3-->>SL: latest volume + Start chunk
    SL->>Eng: set_vcp(extracted VCP)
    SL-->>UI: RealtimeResult::Started

    loop steady state
        SL->>Eng: estimate next chunk time (physics ⊕ stats)
        SL->>SL: interruptible_sleep(predicted + 750ms)
        SL->>S3: fetch next chunk (retry policy)
        S3-->>SL: chunk bytes
        SL-->>UI: ChunkData + ChunkReceived(plan, arrival_stat)

        UI->>W0: ingest_chunk (PINNED worker 0)
        W0->>W0: accumulate in CHUNK_ACCUM (thread-local)
        alt elevation completed
            W0->>IDB: flush completed sweep blob
        end
        W0-->>UI: chunk_ingested (elevations_completed)

        UI->>W0: render_live(elev, product) (PINNED)
        W0->>W0: read partial sweep from CHUNK_ACCUM
        W0-->>UI: live_decoded (partial)
        UI->>GPU: upload → texture (sweep line extrapolated)
    end
    deactivate SL
```

### 10c. The three time categories

The timing model (`nexrad::timing`, `nexrad::projection`) reconciles three
different clocks so the timeline can show *where the radar is now* and *when the
next sweep will appear* before it exists. Full detail in [TIMING.md](TIMING.md).

```mermaid
flowchart LR
    actual["① ACTUAL<br/>observed radial / header times,<br/>S3 Last-Modified"]
    coll["② PROJECTED COLLECTION<br/>when the radar will scan a future chunk<br/>= anchor + Σ VCP-physics intervals"]
    avail["③ PROJECTED AVAILABILITY<br/>when S3 will serve it<br/>= collection + median lag"]

    actual -->|"anchor"| coll
    coll -->|"+ lag"| avail
    avail -->|"drives loop sleep target"| sched["scheduler: when to poll S3"]
    coll -->|"drives timeline placeholders"| tl["next-sweep ghost + forecast"]
```

`ProjectionEngine` is the single owner of projection inputs (VCP, filter,
collection anchor, availability samples, S3 listings, cached sweeps). It memoizes
a `Projection { plan: StreamingPlan, live_scan: ScanProjection }`, invalidated by
an `input_revision` counter. `subsystem::Live` clones the frame's projection into
`LiveRadarModel` so the canvas, timeline, and panels all read one consistent
snapshot.

---

## 11. Storage — IndexedDB

Database `nexrad-workbench` v5, three string-keyed object stores. Schema upgrades
are destructive. Full byte formats + concurrency model in
[INDEXEDDB.md](INDEXEDDB.md).

```mermaid
erDiagram
    sweeps {
        key SITE_MS_ELEV_PRODUCT "ArrayBuffer: 72B header + azimuths + (radial_times) + gate u8/u16"
    }
    scan_index {
        key SITE_MS "ScanIndexEntry: VCP plan + cached_sweeps manifest + total_size_bytes"
    }
    scan_touches {
        key SITE_MS "i64 last-access ms (LRU; isolated to avoid RMW races)"
    }
    scan_index ||--o{ sweeps : "manifests"
    scan_index ||--|| scan_touches : "LRU timestamp"
```

| Store | Key format | Role |
|---|---|---|
| **`sweeps`** | `SITE\|MS\|ELEV\|PRODUCT` | **Primary render path** — GPU-ready blob |
| `scan_index` | `SITE\|MS` | Per-scan plan + realized-sweep manifest (fast timeline queries) |
| `scan_touches` | `SITE\|MS` | LRU last-access (isolated so fire-and-forget touches don't race index writes) |

### The transaction rule (WASM-critical)

In WASM, an IDB transaction **auto-commits when the event loop yields** — so any
`.await` inside a `readwrite` transaction silently voids it. Enforced
*type-level*: the write closure is a synchronous `FnOnce` (not `async`), and the
`.await` happens only *after* it returns.

```mermaid
sequenceDiagram
    participant App
    participant RO as readonly tx
    participant RW as readwrite tx
    App->>RO: read (decide create vs merge)
    RO-->>App: scan_availability
    Note over App,RW: closure is synchronous — NO await inside
    App->>RW: write blobs + index (+ seed touch) synchronously
    App->>RW: wait_for_transaction().await  ← await is OUTSIDE
    RW-->>App: committed
```

Concurrent same-scan upserts are rejected at runtime by an `UpsertScanGuard` RAII
token (`DataError::ConcurrentUpsert`).

### Cache eviction (LRU)

```mermaid
flowchart TB
    trig["decide_eviction (pure, src/data/quota.rs)"]
    a{"app cache_size over app_quota?"}
    b{"browser remaining under 10%?"}
    ev["read scan_index + scan_touches<br/>eviction_order: sort by touch asc<br/>(missing touch → evict first)"]
    del["delete oldest first<br/>(each delete = own tx)<br/>until size ≤ app_quota × 0.80"]
    warn["surface QuotaWarning"]

    trig --> a
    trig --> b
    a -->|yes| ev
    b -->|yes| ev
    b -->|yes| warn
    ev --> del
```

The pure decision functions in `data::indexeddb::logic` —
`should_skip_touch`, `decide_quota`, `eviction_order`,
`filter_scans_by_time_window` — are unit-tested with no real IDB; `DataFacade`
(cloneable, shares one connection) wraps the imperative store.

---

## 12. GPU Render Pipeline & Camera

### 12a. Raw-gate shader pipeline

Gate values are uploaded **raw** (u8/u16 as float); the *fragment shader* does
polar→Cartesian, raw→physical, and color lookup per pixel. Because the
`(raw−offset)/scale` transform is linear, bilinear interpolation works correctly
on raw values, and the sentinels (0 = below-threshold, 1 = range-folded) survive
as a `v > 1.5` validity test. Detail in [RENDERING.md](RENDERING.md).

```mermaid
flowchart LR
    subgraph tex["GPU textures (per sweep)"]
        d["data_tex R32F<br/>(gate × azimuth)"]
        az["azimuth_tex R32F<br/>(sorted angles)"]
        lut["lut_tex RGBA8 1024×1<br/>(OKLab-interpolated colors)"]
    end
    subgraph frag["Fragment shader (per pixel)"]
        p1["pixel → azimuth°, range_km"]
        p2["binary-search azimuth_tex → index<br/>(reject gap > median × 1.5)"]
        p3["sample data_tex(gate, az) → raw"]
        p4["valid? raw > 1.5"]
        p5["physical = (raw − offset) / scale"]
        p6["LUT[(physical − value_min) / value_range]"]
        p7["premultiplied alpha × opacity"]
        p1 --> p2 --> p3 --> p4 --> p5 --> p6 --> p7
    end
    d --> p3
    az --> p2
    lut --> p6
    p7 --> out([radar disc])
```

Key uniforms: `offset`, `scale` (per-product), `value_min`/`value_range` (LUT
window), `interpolation` (0 nearest / 1 bilinear), `opacity`,
`data_age_desaturation`, plus the sweep-animation set (`sweep_enabled`,
`sweep_azimuth`, …) and a duplicate set for the **previous** sweep so the shader
can crossfade between them per pixel.

### 12b. Camera state machine & the 2D/3D split

The camera is an **enum** — exactly one variant is active, and `ViewMode` is
*derived* from it (no separate toggle to keep in sync).

```mermaid
stateDiagram-v2
    [*] --> Flat2D
    Flat2D --> PlanetOrbit: enter 3D
    PlanetOrbit --> Flat2D: exit 3D
    PlanetOrbit --> SiteOrbit: switch
    PlanetOrbit --> FreeLook: switch
    SiteOrbit --> PlanetOrbit: switch
    SiteOrbit --> FreeLook: switch
    FreeLook --> PlanetOrbit: switch

    note left of Flat2D
        2D: MapProjection
        (equirectangular + zoom/pan)
        → RadarGpuRenderer
    end note
    note right of FreeLook
        3D: view/projection matrices
        → Globe + GeoLine + GlobeRadar or VolumeRay
    end note
```

Which renderers are active depends on view mode (and, in 3D, the volume toggle):

```mermaid
flowchart TB
    cam{"Camera variant"}

    subgraph two["2D path"]
        mp["MapProjection (equirect)"]
        rg["RadarGpuRenderer (fullscreen quad)"]
        gl2["GeoLineRenderer (project lat/lon → screen)"]
        ov2["overlays: sweep · cells · sites · scale bar · alerts"]
    end
    subgraph three["3D path"]
        gr["GlobeRenderer (lit sphere + depth)"]
        gl3["GeoLineRenderer (project → unit sphere)"]
        vol{"volume_3d_enabled?"}
        vol -->|no| gradr["GlobeRadarRenderer (single-elev patch)"]
        vol -->|yes| vray["VolumeRayRenderer (march all elevations)"]
        ov3["overlays: compass · color scale · info"]
    end

    cam -->|Flat2D| two
    cam -->|"PlanetOrbit / SiteOrbit / FreeLook"| three
```

Both paths share the same GPU textures (data / lut / azimuth) and the same GLSL
snippet blocks, so 2D and 3D radar can't drift. Geographic layers (states,
counties, cities) are **embedded shapefiles** loaded at compile time; storm-cell
detection (`nexrad::detection`) runs connected-component analysis on reflectivity
and is pure/testable (core), with only the box-drawing in the shell.

---

## 13. UI Shell — Layout Registry & Canvas Overlays

The chrome is a **declarative `Layer` registry**, not hand-ordered calls. Each
`Layer` declares its `kind` (Chrome vs Modal), `z_order`, and a `visible()`
predicate; `render_layout` walks the slice in z-order. Desktop and mobile swap
the chrome set but share the modal set.

```mermaid
flowchart TB
    rl["render_layout(is_mobile, LayoutCtx)"]
    rl --> pick{"is_mobile?"}

    pick -->|desktop| dchrome["Chrome: TopBar(10) · BottomPanel(20)<br/>LeftPanel(30) · RightPanel(40)"]
    pick -->|mobile| mchrome["Chrome: MobileTopBar(10)<br/>MobileChrome bottom bar(20)"]

    dchrome --> modals
    mchrome --> modals
    modals["Modals (shared, z-ordered):<br/>Site · ShortcutsHelp · Wipe · RangeDownload<br/>QueueSheet · ScanInspector · Stats · VcpForecast<br/>NetworkLog · Event · Alerts · mPING · MobileSettings"]

    inv["LayoutCtx carries mutable refs to every subsystem;<br/>each Layer reborrows only the fields it needs<br/>(disjoint-field borrows). Invariants debug-checked:<br/>all Chrome before any Modal, z_order strictly ascending."]
    rl -.-> inv
```

The **canvas** (`render_canvas_with_geo`) is called directly (not via the Layer
registry) because it owns `geo_layers` + `GpuResources`. Overlays paint in a
fixed order:

```mermaid
flowchart TB
    c0["1 National mosaic (CONUS composite, site cutout)"]
    c1["2 Geo layers — Lines pass (states · counties · lakes · highways)"]
    c2["3 Radar texture (GPU sweep)"]
    c3["4 Geo layers — Labels pass + alert polygons (2D)"]
    c4["5 mPING reports · GPS marker · sites"]
    c5["6 Sweep animation (prev↔current crossfade) · storm cells"]
    c6["7 Inspector probe · distance tool"]
    c7["8 Corner chrome (Overlay trait): info · color scale · compass(3D) · scale bar(2D)"]
    c0 --> c1 --> c2 --> c3 --> c4 --> c5 --> c6 --> c7
```

**Input → intents.** `canvas_interaction` translates pan/zoom/click/pinch into
state changes or `AppCommand`s (e.g. clicking a site → open site modal; clicking
an mPING report → focus it; a mobile reveal-tap is swallowed instead of panning).
Per the standard, decision math (hit-tests, geometry) lives in the core; the
shell just emits intents and projects the view-model.

---

## 14. Async / Channel Bridge

egui's `update()` is synchronous, but acquisition, streaming, IDB, and
geolocation are async. The bridge is uniform: **spawn an async task that posts to
an `mpsc` channel; poll the channel every frame.**

```mermaid
sequenceDiagram
    participant F as update() frame N
    participant Ch as mpsc channel
    participant T as spawn_local task
    participant Ext as S3 / IDB / worker / geo

    F->>T: start_operation(ctx, params)
    activate T
    T->>Ext: await IO
    Note over F: frames N+1, N+2 … keep rendering
    Ext-->>T: result
    T->>Ch: send(result)
    T->>F: ctx.request_repaint()
    deactivate T
    loop every frame
        F->>Ch: try_recv()
        Ch-->>F: Some(result) → handle
    end
```

Spawning is platform-gated: WASM uses `wasm_bindgen_futures::spawn_local`; the
native dev stub uses `std::thread::spawn` + `pollster::block_on`. Coordinators
(`AcquisitionCoordinator`, `RealtimeChannel`, `CacheLoadChannel`,
`NetworkMonitor`) each own their channels and expose a `try_recv` drained in the
frame loop's INTAKE phase.

---

## Appendix — Key Types at a Glance

| Type | Module | Role |
|---|---|---|
| `WorkbenchApp` | `main.rs` | Thin coordinator; owns subsystems + GPU + the frame loop |
| `AppState` | `state` | Shared data tree + command queue |
| `AppCommand` / `Intent` | `state` / `core::intent` | The intent vocabulary (UI → loop) |
| `Effect` | `core::effect` | Described side effect the shell executes |
| `subsystem::Derived` | `subsystem::derived` | Per-frame cross-cutting view-model |
| `ScanKey` | `data::keys` | `SITE\|MS` — volume identity |
| `SweepDataKey` | `data::keys` | `SITE\|MS\|ELEV\|PRODUCT` — sweep blob identity |
| `PrecomputedSweep` | `data::keys` | GPU-ready blob (header + azimuths + gates) |
| `SweepIdentity` | `state::viz` | Render dedup key (scan + elev + product) |
| `RenderCoordinator` | `nexrad` | Worker pool + `should_dispatch` dedup |
| `AcquisitionCoordinator` | `nexrad` | Download channel + queue + archive index + facade |
| `ProjectionEngine` | `nexrad::projection` | Owns live timing projection inputs/outputs |
| `LiveModeState` / `LivePhase` | `state::live_mode` | Live streaming state machine |
| `DataFacade` / `IndexedDbStore` | `data` | Cache layer (cloneable, shared connection) |
| `RadarGpuRenderer` | `nexrad::gpu_renderer` | WebGL2 polar→Cartesian + raw→physical shader |
| `Camera` | `geo::camera` | Enum state machine (Flat2D / PlanetOrbit / SiteOrbit / FreeLook) |

---

*Generated 2026-06-15 from the `functional-core-migration` branch. Keep diagrams
in sync with [ARCHITECTURE.md](../ARCHITECTURE.md) when subsystems change; the
prose doc remains the authoritative file-by-file map.*
</content>
</invoke>
