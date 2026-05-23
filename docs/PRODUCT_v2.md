# Weather Radar Visualization Tool — Product Document

*Draft. Items marked **(proposed)** are defaults I've filled in for previously open questions and need your sign-off; everything else reflects requirements you've already stated. Open decisions are collected in §9.*

---

## 1. Overview & Goals

A tool for exploring weather radar data on a single, zoomable timeline. It serves both **archived data** (roughly 1991 through ~30 minutes before now) and **real-time streaming** data, and renders the selected moment onto a canvas with accompanying metadata.

The application runs **entirely in the browser** via WebAssembly: data is fetched directly from public sources, decoded, and rendered client-side. There is no backend.

The interface is organized around one central idea: a timeline that shows *where data exists and whether we have it*, paired with a playback position that determines *what is being rendered right now*.

**Apparent data source (proposed / confirm):** the domain model below — volume scans, VCPs, the 1991 floor, and the 3-or-6-chunk real-time structure — matches NEXRAD Level II (WSR-88D). The spec assumes that target; flag if it should be source-agnostic.

### Guiding principles

- **Invisible acquisition.** Downloading is a side effect of what the user looks at, not a separate manual chore. The user navigates; data acquisition reacts.
- **Guarded acquisition.** Because acquisition is automatic, it must be hard to accidentally trigger large downloads, and the app must clearly signal when it cannot get the data it needs.
- **Progressive detail.** The timeline reveals more structure as the user zooms in — contiguous blocks become scans, scans become sweeps.
- **The current moment is always legible.** The playback position is always clearly visible, and it is unambiguous which sweep(s) are feeding the canvas.
- **Transparency over abstraction.** The user should be able to see what the data contains and how it maps to the rendered pixels. Nothing important is hidden behind opaque steps.
- **Always responsive.** Heavy work (decompression, decoding, storage I/O) runs off the UI thread. The interface never freezes on data.
- **Resilient actions.** User actions do simple, consistent things regardless of current state — enqueue, toggle, seek. The user should not need a mental model of the app's mode to predict what a click will do.

---

## 2. Domain Model

| Concept | Definition |
|---|---|
| **Radar site** | A physical radar installation, identified by a short site ID (e.g. `KDMX`). The site fixes the geographic origin for all rendering; the timeline is scoped to one site at a time. |
| **Radial** | A single ray of data along one azimuth. The smallest unit. |
| **Sweep** | One full rotation at a single elevation, composed of radials. |
| **Volume scan** | A complete set of sweeps across several elevations. The unit an archive file represents. |
| **Dead time** | The gap between sweeps while the instrument changes elevation and adjusts speed for the next sweep's parameters. |
| **VCP (Volume Coverage Pattern)** | Dictates the pattern of sweeps (elevations, products, ordering, timing) for each scan. |
| **Product / Elevation** | Filterable dimensions enumerated by the active VCP. Elevations are addressed by **elevation number** within the VCP, which is unique within a scan even when the literal angle is repeated (SAILS / MRLE supplemental low-tilt cuts). |

### Archive vs. streaming

- **Archive data** is downloaded as complete scans, after the fact.
- **Streaming data** arrives as **chunks**, downloaded individually in real time. A sweep is composed of **3 or 6 chunks**. As chunks accrete into complete sweeps and complete scans, they become byte-equivalent to the archive.

**Records and chunks** are the same underlying unit. Archive files are a sequence of records (a header record followed by data records); real-time exposes those same records as chunks delivered incrementally. The terms are interchangeable. The first record of any scan is the **header record**, which carries the VCP and other operational parameters — without it, subsequent records cannot be interpreted.

**Equivalence guarantee:** a fully-streamed scan is equivalent to the same scan downloaded later from the archive. The cache should treat them identically once complete, so a scan streamed in real time needs no re-download if revisited.

### Three categories of time

Three distinct time concepts coexist in the app and must not be conflated:

- **Actual** — parsed from radial headers or read from the wall clock. Drives the canvas, the playback cursor, and completed timeline blocks.
- **Projected collection** — when the radar will *physically sample* a future sweep, computed from VCP timing and dead time.
- **Projected availability** — when a chunk will *be downloadable* from the source (collection + a typical 5–15 s ingest lag).

Timeline projections (§3) sit at *collection* times; failure detection and countdowns (§7) compare against *availability* times. A chunk past its collection time is not yet late if the ingest-lag window hasn't elapsed.

---

## 3. The Timeline

The timeline spans the full available history of the active site (~1991 → now − ~30 min) and is **zoomable**, with the level of structural detail tied to zoom:

- **Zoomed out:** contiguous blocks indicating *where data is available* (and whether we hold it).
- **Mid zoom:** individual scan boundaries become visible.
- **Zoomed in:** the constituent **sweeps** within each scan are shown.

### 3.1 Selection vs. playback position

These are two distinct concepts the timeline must show simultaneously:

- **Selection** — a single position *or* a range. Represents the user's region of interest.
- **Playback position** — the single instant actively being rendered to the canvas and reflected in the metadata panels. Always clearly visible.

A range selection becomes a **playback loop boundary** during play, with three loop modes: **Loop** (wrap to the start), **Ping-Pong** (reverse at the endpoints), and **Once** (stop at the end). The selection does not constrain manual seeks. Scrubbing while in live mode exits live (acquisition continues in the background; the user can return to "now" to resume).

### 3.2 Timeline layers

The timeline is best modeled as stacked semantic layers, each rendering differently per zoom level:

1. **Availability** — contiguous blocks of what is cached/downloaded.
2. **Scan & sweep structure** — scan boundaries and, when zoomed in, the sweeps within them. **Partially-downloaded scans render with a partial fill** so the user can see a scan is incomplete.
3. **Archive positions (estimated).** Archive object names expose only the scan's *start* time, so the position/spacing of subsequent scans is estimated from the **average scan duration for the active VCP**. These positions are guesses and should render with a distinct **uncertainty treatment**, visually separate from confirmed cached scans. *(proposed: treat estimated archive positions and confirmed cached scans as two different visual states.)*
4. **Projection** — upcoming sweeps and scans, placed at their **projected collection** times (§2), rendered as **ghosted** markers so the user can anticipate what's coming.
5. **Playback markers** — the **1–2 actively rendered sweeps**, clearly highlighted.
6. **Acquisition status** — in real-time mode, the **"up next"** sweep/chunk we're waiting on, marked distinctly and tied to the acquisition state (§7).

### 3.3 Saved selections

A named range selection can be persisted as a **saved event**, becoming a reusable bookmark across sessions. Saved events render as colored bars on the timeline, surface in a list grouped by site, and offer a "navigate to" action that jumps the playback position and selection to the event's range — switching the active site if necessary.

---

## 4. Rendering & Sweep Matching

### 4.1 Filters

The user selects an **elevation** (by number within the active VCP — see §2) and a **product** from the available set. The filter does double duty:

- It **drives sweep matching** for rendering (below).
- It **scopes acquisition** (§5): only matching data is fetched/cached.

### 4.2 Sweep matching — the 0–2 rule

For any playback position, the app renders **0, 1, or 2 sweeps**:

- **0 sweeps** — no matching sweep precedes the playback position within the **lookback range**. Nothing is rendered.
- **1 sweep** — either exactly one sweep matches, *or* multi-sweep mode is disabled. The single matching sweep is displayed.
- **2 sweeps** — multi-sweep mode is enabled *and* multiple sweeps match. Portions of both are rendered.

**Lookback range** is the maximum time the matcher looks *backward* from the playback position for the most recent matching (filtered) sweep. It bounds staleness — beyond it, data is considered too old to represent "now," and the app shows nothing rather than a stale sweep. *(proposed: make the lookback a defined, possibly configurable parameter rather than unbounded.)*

**Why two sweeps?** During the transition between an old and new sweep, the freshest data covers only part of the 360°. Compositing the newer sweep over the older one yields a complete picture — newest data where it exists, prior data elsewhere — rather than a half-empty render. This pairs directly with radial playback (§6).

### 4.3 Metadata panels

The accompanying panels always describe whatever sweep(s) are currently rendered, staying in sync with the playback position.

### 4.4 Views

The radar can be projected to the canvas in two views (2D by default); sweep matching and the playback model are identical across them — only the projection differs.

- **2D map.** Equirectangular projection centered on the active site, drawn alongside the toggleable overlays in §8.1.
- **3D globe.** WebGL2 sphere with three camera modes — **Planet Orbit** (around Earth's center), **Site Orbit** (around the radar site, always facing it), and **Free Look** (first-person flying camera). The globe additionally offers a **volumetric ray-march** rendering that composites all elevations of the active scan into a single 3D pass with a user-controlled density cutoff, instead of rendering a single elevation as a surface.

---

## 5. Acquisition

Acquisition is **reactive and invisible**: it's a function of *(playback position, selection, viewport, active filter)*. The app fetches what's needed to render the current position and the near future, scoped by the filter — never more by default.

### 5.1 Two classes of acquisition

- **Implicit prefetch** — bounded and fully automatic. Covers the current sweep plus a small lookahead. It is **debounced**, so transient positions produced while the user is actively scrubbing or zooming do not fire fetches; the view must settle first. Subject to hard caps on **concurrent downloads** and **total auto-fetched volume**. *(proposed)*
- **Explicit bulk** — a wide range selection does **not** silently download everything inside it. Instead, the range fills in lazily as the playback position approaches, or via an explicit "download this range" action that first shows an **estimate** (scan count / approximate size) and **confirms above a threshold**. *(proposed)*

This split is the primary guardrail against accidental mass downloads.

### 5.2 Filter-scoped fetching

- **Archive:** only sweeps matching the active filter are cached.
- **Streaming:** only chunks matching the filter are downloaded, and the app waits between them (rather than greedily pulling the full chunk stream).

**Mid-volume entry.** When real-time streaming begins partway through a volume, the **header record** (§2) is always co-fetched alongside the latest chunk so the VCP is available to interpret what follows. The user does not see this as a separate step.

### 5.3 Cache

Completed scans (archive or streamed) are cached identically per the equivalence guarantee (§2). Storage is bounded by a user-configurable quota (default **2 GB**); eviction is **least-recently-used** by per-scan touch timestamp, targeting **80% of quota** to avoid thrashing near the limit. Reads bump the touch timestamp, throttled to once per scan per minute so heavy scrubbing doesn't churn the LRU order.

---

## 6. Playback

### 6.1 Modes

- **Radial mode.** Animates a sweep line, revealing/rendering individual radials as the line passes. The current sweep is composited with the previous one into a single continuous 360° render (see §4.2). This is the live "sweeping" presentation.
- **Frames mode.** Whole-sweep renders with no sweep-line animation — each sweep is a discrete frame.

### 6.2 Zoom-dependent timing

Playback operates in one of two timing regimes, selected by zoom level with a threshold of **1 pixel per second**:

- **Micro (≥ 1 px/sec): true-time playback.** The playback position advances evenly in wall-clock time, and radials are revealed at their actual collection timestamps. Speeds are time multipliers: `1× (real)`, `2× (real)`, `15 s/s`, `30 s/s`, `1 min/s`, `2 min/s`, `5 min/s` (default), `10 min/s`, `20 min/s`. Real-time lock is only available in micro.
- **Macro (< 1 px/sec): frame-stepping.** The position snaps between matching sweeps as equidistant frames, giving each sweep equal screen time regardless of its true collection duration or the dead time around it. Speeds are frame rates: `1 / 2 / 5 / 10 / 15 fps`. Sweep animation is disabled in macro.

Crossing the threshold mid-playback swaps the speed list; the active speed snaps to the nearest equivalent in the other list.

---

## 7. States & Failure Handling

Because VCP timing and dead time are known, the app computes both the **expected collection time** and the **expected availability time** for the next chunk/sweep (§2), and detects problems precisely rather than guessing.

### 7.1 Acquisition states *(proposed)*

| State | Meaning | Indication |
|---|---|---|
| **Live** | Real-time data is arriving on or before expected availability; we're keeping up. | Normal up-next marker on the timeline. |
| **Lagging** | Past expected availability but still progressing toward arrival. | Up-next marker shows a "behind" treatment. |
| **Stalled** | Significantly past expected availability with no progress. | Distinct stalled indicator at the up-next position. |
| **Failed** | Source error; we cannot acquire (e.g., stream dropped, archive gap). | Clear failure state with a manual retry affordance. |

### 7.2 Recovery

Transient problems retry automatically with **backoff**; persistent ones surface a **manual retry** rather than retrying forever. The failure must be visible on the timeline (at the affected position) *and* legible in the canvas/metadata area, so a user never mistakes "no data available" for "app is broken" or vice versa.

---

## 8. Overlays, Tools, and Platforms

The radar is the centerpiece, but the workbench composes auxiliary layers, tools, and chrome around it.

### 8.1 Overlays

Independent rendering layers that can be toggled on/off:

- **Geographic** — state lines, county lines, cities, labels, and other radar site markers.
- **National mosaic** — a CONUS-wide quality-controlled base-reflectivity composite, fetched live and refreshed periodically. Rendered with a **circular cutout** around the active site so the per-site render isn't obscured.
- **NWS active alerts** — polygon footprints of active National Weather Service alerts, plus a top-bar chip surfacing the highest-severity alert intersecting the visible map.
- **mPING storm reports** — crowdsourced surface observation markers.
- **My Location** — a one-shot GPS dot for the user's current position.

### 8.2 Tools

Interactive measurement and analysis affordances on the 2D canvas:

- **Inspector** — hover for lat/lon, azimuth/range, the underlying gate value, and the radial's collection timestamp.
- **Distance Measure** — click two points to read the great-circle distance.
- **Storm Cells** — detect connected components above a configurable reflectivity threshold; detected cells render as bounding boxes.

### 8.3 Mobile

On narrow viewports the application switches to touch-first chrome: sidebars collapse, the timeline is replaced by a compact scrubber, and a tabbed settings sheet exposes controls. The 2D canvas accepts pinch-zoom and drag-pan; the 3D view is desktop-only.

### 8.4 Deep linking and sharing

Application state is encoded in the URL: site, playback time, product/elevation, view, and map/camera position. Any view of the data is shareable as a single link; opening the link reconstructs the view. URL updates are throttled so back/forward navigation works naturally.

---

## 9. Open Decisions

Carried forward for resolution:

1. **Audience & primary use cases** — researchers, forecasters, enthusiasts? Shapes defaults (filters, zoom, cache size).
2. **Data source scope** — commit to NEXRAD Level II, or keep source-agnostic? (Sites, VCPs, header records, and the 3/6-chunk structure already lean toward NEXRAD; this decision is whether to make that explicit and drop the source-agnostic option.)
3. **Lookback range** — fixed, VCP-derived, or user-configurable? What default?
4. **Multi-sweep + radial interaction** — when two sweeps match in radial mode, do both animate, or does the older one freeze as a backdrop?
5. **Acquisition caps & thresholds** — concrete numbers for concurrency, auto-fetch volume, and the bulk-download confirm threshold.
