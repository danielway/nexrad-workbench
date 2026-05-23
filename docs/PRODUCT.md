# NEXRAD Workbench — Product Definition

## 1. Vision and Principles

NEXRAD Workbench is a browser-based technical workbench for viewing and analyzing NEXRAD radar data. It operates entirely client-side with no backend services; all data is fetched and processed in the browser using WebAssembly.

The product prioritizes transparency, inspectability, and correctness over abstraction. Users should be able to see exactly what the radar data contains and how it maps to the rendered visualization. Performance and responsiveness are first-order concerns; heavy computation is isolated in Web Workers so the UI thread stays interactive at all times.

The application avoids brittle, multi-step workflows that can fail partway through and require custom recovery UI. User actions do simple, consistent things regardless of the current application state: enqueue archives, toggle streaming, change the playback position. Complexity is managed by the system, not imposed on the user.

The interface is offered in two tiers — **Basic** (a minimal default for casual users) and **Advanced** (full controls and diagnostics) — toggled by a single pill in the top bar. Power-user surfaces (left sidebar, render-pipeline knobs, tools, storage management, developer mode) only appear in Advanced.

## 2. Core Concepts and Terminology

### Spatial Hierarchy

A **radar site** is a physical NEXRAD WSR-88D installation identified by a site ID (e.g. `KDMX`). The site defines the geographic origin for all spatial calculations and rendering.

The fundamental unit of data is the **volume scan**: a full multi-elevation volume sampled by the radar, with a typical duration of 5–10 minutes. A scan is composed of multiple **sweeps**, each corresponding to a specific elevation angle. Elevations may repeat within a single scan (SAILS/MRLE supplemental low-tilt sweeps). The ordered sequence and parameters of sweeps define the **Volume Coverage Pattern (VCP)**.

Each sweep contains **radials**, single rays extending outward from the radar at a specific azimuth and elevation. Radials are the smallest spatial unit directly rendered to the canvas. Each radial contains a sequence of **gates**, fixed-distance samples representing measurement values at specific ranges from the radar. Gate spacing and count are product-dependent.

### Products

A **product** is a specific radar measurement type. The workbench supports the seven base WSR-88D moments:

| Product | Code | Unit | Value range |
| --- | --- | --- | --- |
| Reflectivity | `REF` | dBZ | −32 … 95 |
| Velocity | `VEL` | m/s | −64 … 64 |
| Spectrum Width | `SW` | m/s | 0 … 30 |
| Differential Reflectivity | `ZDR` | dB | −2 … 6 |
| Correlation Coefficient | `CC` | (dimensionless) | 0 … 1.05 |
| Differential Phase | `KDP` | °/km | 0 … 360 |
| Clutter Filter Power | `CFP` | dB | −20 … 20 |

Each sweep collects data for one or more products simultaneously; within a sweep, each radial contains gate values for each product. Products define their own value domains, units, and color tables, and are rendered independently. Reflectivity uses a custom OKLab-interpolated color table with a low-end alpha ramp; the other products use the continuous color scales from the `nexrad_render` crate. All color tables are rendered as 1024-entry GPU LUT textures.

Derived products (storm tracking, precipitation estimates) are out of scope.

### Data Sources

The workbench sources data from public AWS S3 buckets provided by AWS Open Data:

- **Archive II** files live in the `noaa-nexrad-level2` and `unidata-nexrad-level2` buckets; each file is a complete volume scan in Archive II format.
- **Real-time chunks** are published incrementally to the `unidata-nexrad-level2-chunks` realtime buckets as LDM records, assembled into volumes as they arrive.

Two additional external services are used for overlays, both fetched directly from the browser:

- **National radar mosaic** — NOAA NCEP GeoServer (`opengeo.ncep.noaa.gov`) WMS, MRMS CONUS quality-controlled base reflectivity, refreshed every ~2 minutes.
- **NWS active alerts** — `api.weather.gov/alerts/active` GeoJSON feed, polled when the alerts layer is enabled.

### Data Structure

A scan is composed of one **header record** followed by a sequence of **data records**. Each record is Bzip2-compressed and contains radar data spanning one or more radials; records often cover only part of a sweep and may cross sweep boundaries. The terms "record" and "chunk" are effectively equivalent.

The **header record** (the first record in every scan) contains radar operational parameters, scan configuration, and the Volume Coverage Pattern (VCP). This record is required to correctly interpret all subsequent records. When real-time streaming begins mid-volume, the system always fetches record 1 in addition to the latest chunk so the VCP is available.

### Time Model

The codebase distinguishes three categories of time:

- **ACTUAL** — parsed from radial headers or read from the wall clock. Drives the radar canvas, the timeline cursor, the timeline's completed scan blocks, and the "Age" display.
- **PROJECTED COLLECTION** — when the radar will physically scan a future chunk. Drives timeline placeholders for the in-progress volume and future sweeps.
- **PROJECTED AVAILABILITY** — when a chunk will be downloadable from S3. Drives the download scheduler and the "next in N s" countdown.

The two projected categories differ by the NEXRAD ingest lag (typically 5–15 s). The UI never conflates them: timeline placeholders are placed in collection time; countdowns are quoted in availability time.

**Playback position** is the moment in radar time whose data is currently displayed. The visualization includes only data collected at or before that moment. Playback position is independent of wall-clock time during archive playback; in live mode it tracks wall-clock time and the user can jog backwards within the historical window without exiting streaming.

The distinction between archive and real-time data is intentionally blurred at the data level. Both are composed of chunks; archive files are simply the accumulated result of chunk delivery. Each Archive II volume file contains exactly one volume scan.

## 3. Application Layout

### Overview

The desktop layout consists of:

- **Top bar** — site context, mode badge, view-mode pills, UI tier toggle, help, alerts chip
- **Left sidebar** (Advanced only) — radar operations (azimuth view, elevation view, VCP breakdown)
- **Center** — map canvas
- **Right sidebar** — product, layers, rendering (Advanced), 3D volume (Advanced/Globe), tools (Advanced), events (Advanced), storage (Advanced), developer mode (Advanced)
- **Bottom dock** — acquisition drawer (Advanced + dev only), timeline track, transport bar with session stats

The conceptual flow:

1. **Site selection** establishes "where" — which radar site to work with.
2. **Timeline interaction** establishes "when" and "what to acquire" — users express intent (view a moment, download a range, stream real-time) through the timeline.
3. **Acquisition executes visibly** — requests and transfers are observable in the transport bar and (in dev mode) the acquisition drawer.
4. **Playback position drives the UI** — the current moment determines what the map renders, what the left sidebar displays, and what data is relevant.

The timeline is the central control surface: it presents temporal context, drives data acquisition, and governs the playback position from which all other UI state derives.

### Top Bar

A thin 3 px mode-accent bar sits above the top bar, colored by app mode (Idle / Archive / Live).

Left side (in order):

- **Left-sidebar toggle** (Advanced only) — collapses the radar-operations panel.
- **App title** — "NEXRAD Workbench".
- **Site button** — `Site: {site_id}`. Opens the site selection modal.
- **NWS alerts chip** (2D view only) — appears when active alert polygons intersect the visible map bounds. Shows the highest-severity event type and color. Click a single alert to open the detail modal, multiple to open the list modal.
- **Worker init error banner** (when present) — red banner with a Retry button if the Web Worker fails to start.
- **Mode badge** — unified pill showing `Idle` / `Archive` / `Live`. The Live badge pulses and exposes phase detail (`acquiring lock... Ns`, `(N chunks) receiving...`, `(N chunks) next in Ns`). Click to open a `Go live` / `Stop streaming` menu.
- **Status message** — transient text (auto-fades after 8 s, dismissed after 10 s) in Archive / Idle modes.

Right side (in order):

- **Right-sidebar toggle**.
- **Help button** — opens the keyboard-shortcuts overlay (`?`).
- **UI tier pill** — Basic / Advanced (Ctrl+Shift+A). New users default to Basic; existing users are migrated to Advanced.
- **Version stamp** — clickable link to the GitHub releases page.
- **View-mode pills** — Basic shows `2D / 3D`; Advanced shows `2D / 3D Site / 3D Planet / 3D Free`. Keys `1`–`4` activate the four modes; `T` toggles between the last 2D and last 3D mode.

### Left Sidebar: Radar Operations (Advanced + desktop)

Read-only panel derived from the current playback position. Resizable (235–400 px).

- **Azimuth view** — a 100 × 100 px top-down compass with range rings, cardinal markers, the current azimuth line, and (in live mode) a shaded 90° future sector ahead of the sweep line.
- **Elevation view** — a 120 × 100 px side profile with the ground line, the tower, reference rays (0°, 10°, 20°), and the current beam.
- **VCP breakdown** — header (`VCP {n}` + name), progress bar (volume completion 0–100 %), then a monospace grid with one row per elevation: `Elev | Wf | PRF | Time | Products`. Waveforms render as `CS`, `CD`, `B`, or `SP`; PRF compresses to `L` / `M` / `H`; per-elevation timing shows offset from volume start (prefixed `~` when estimated); products render as colored single-letter badges (`R V S Z P C`). The current elevation is highlighted in bright green.

### Right Sidebar: Controls

Resizable (180–350 px). Sections are collapsible.

**Product** (always shown)

- Product dropdown — the seven supported moments.
- "Auto (latest sweep)" checkbox — when on, displays the most recent completed sweep regardless of elevation. When off, the user picks a specific elevation.
- Elevation list — one row per VCP cut: elevation number, angle, a waveform badge (`CS` green, others blue), and `SAILS` / `MRLE` badges in orange where applicable. Rows where the product hasn't been cached for that elevation are disabled.

**Layers** (always shown; some entries Advanced-only)

- State Lines, County Lines, Cities (default on).
- NEXRAD Sites (Advanced).
- Labels (Advanced).
- National Mosaic (Advanced) — disabled outside live mode.
- Weather Alerts — disabled when the active scan is more than 15 min old.
- My Location — one-shot GPS dot on the map (requires browser permission; surface an inline error in red on failure).
- Storm Reports (mPING) (Advanced) — disabled without an API key; a gear button opens the mPING settings modal.

**Rendering** (Advanced)

- Interpolation: Nearest / Bilinear.
- Sweep Animation — progressively reveals new data behind the sweep line (disabled in macro-zoom mode).
- Data Age Desaturation — desaturates the oldest data behind the sweep line (only when sweep animation is on).
- Opacity slider — 0–100 %.

**3D Volume** (Advanced, Globe view only)

- Enable Volume Rendering — switches the globe between the single-elevation surface patch and the volumetric ray-march.
- Min Value slider (0–30, product-dependent units) — suppresses thin returns below the threshold.

**Tools** (Advanced)

- Inspector — hover for lat / lon, azimuth / range, product value, and (when available) the radial collection timestamp.
- Distance Measure — click two points to measure great-circle distance.
- Storm Cells — detect connected components above a configurable dBZ core threshold (default 35 dBZ, edge margin 5 dBZ, minimum 15 km², minimum 8 gates). Detected cells render as bounding boxes on the canvas.

**Events** (Advanced)

- "Save Selection as Event" button — enabled when a timeline range is selected (Shift-drag). Opens an editor to name the event.
- Current site's events — listed with edit and "navigate to" buttons; the navigate button jumps the playback position and selection to the event range (switching sites if necessary).
- Other sites' events — collapsible group, same controls.
- Saved-event ranges appear as colored bars on the timeline.

**Storage** (Advanced)

- Cache usage readout: `{used} / {quota} ({pct}%)` with a color-coded progress bar (green ≤ 70 %, orange ≤ 90 %, red above).
- Quota slider — logarithmic, **100 MB – 20 GB** (default 2 GB; eviction target 80 % of quota).
- Clear Cache — wipes all cached radar data.
- Reset App — opens a wipe-confirmation modal; on confirmation wipes IndexedDB plus localStorage and reloads the page.

**Developer** (Advanced)

- Toggle that reveals diagnostic UI (per-frame timing, FPS, network metrics, pipeline indicator, COI badge, VCP forecast modal, acquisition drawer). Adds `?dev=true` to the URL.

### Bottom Dock

Three layers stacked top-to-bottom:

**Acquisition drawer** (visible only when dev mode is enabled and the user has expanded it). Resizable 100–600 px. Two tabs:

- *Queue* — operations with status, progress, age, retry / cancel / skip / resume controls. A banner appears when the queue is paused on error.
- *Network* — recent requests grouped by operation, with a "Full Log" button for the network-log modal.

**Timeline track** (fixed 53 px). Top-to-bottom: an adaptive ruler with date/time labels at the appropriate granularity (years → 1 s), a scan-track layer of solid colored blocks (color = VCP), an optional sweep-track layer revealed at deep zoom (highlights the current elevation and draws elevation connectors), the playback-position cursor (2 px), and overlays for pending/downloading scans, the real-time live edge, saved-event ranges, and the selection range.

Interactions:

- Click to seek.
- Drag the cursor to scrub.
- Shift-drag (or shift-click endpoints) to set a selection range. The range becomes the playback loop boundary; in Advanced mode the Download button enqueues all scans within it.
- Scroll to zoom (toward the cursor).

The timeline operates in two playback modes derived from zoom:

- **Micro** (≥ 1 px/sec) — continuous time-based playback. All speeds available. Real-time lock is only available in micro.
- **Macro** (< 1 px/sec) — frame-stepping between matching sweeps. Frame-rate speeds replace time-rate speeds. Sweep animation is disabled.

**Transport bar** (24 px). Left-to-right:

- Current playback timestamp (monospace, `YYYY-MM-DD HH:MM:SS [Z]`). In Advanced mode clicking opens the datetime-jump dialog (UTC or local toggle, validation, Enter to submit, Esc to cancel).
- Live indicator (when live mode is active) — `CONNECTING Ns` while acquiring lock; `LIVE (N chunks) receiving...` or `next in Ns` while streaming.
- Play/Stop, Step Back, Step Forward, Now (jump to current time). Disabled in Idle.
- Speed dropdown — micro speeds: `1x (real)`, `2x (real)`, `15s/s`, `30s/s`, `1 min/s`, `2 min/s`, `5 min/s` (default), `10 min/s`, `20 min/s`. Macro speeds are frame-based: `1 / 2 / 5 / 10 / 15 fps`.
- Loop mode dropdown (Advanced, only when a selection is set) — Loop / Ping-Pong / Once.
- Clear selection button — appears when a selection is set.
- Download button (Advanced) — downloads the scan at the current position, or every scan in the selected range. Shows `DL {n}/{total}` while a batch is in flight.
- UTC / Local toggle.

Right-aligned session stats (FPS, pipeline indicator, active-request count, request totals, COI badge) appear only in dev mode; cache size and clear-cache control are always visible.

### Map Canvas

The primary view. Radar data is rendered in polar coordinates centered on the radar site and projected onto the map. Pan and zoom are always available.

**2D view (Flat2D)** — default. Equirectangular projection with toggleable state boundaries, county boundaries, cities, labels, and other NEXRAD site markers. Always rendered in dark mode (the workbench currently ships dark mode only).

**3D view (Globe3D)** — projects radar onto a 3D sphere via WebGL2. Three camera modes:

- **Planet Orbit** — orbit around Earth's center; left-drag rotates the globe; right-drag applies tilt and yaw.
- **Site Orbit** — orbit around the radar site, always facing it.
- **Free Look** — first-person flying camera. WASD/arrows move, Q/E vertical, mouse rotates, Shift = 2×, Ctrl = ¼×, R resets, F focuses, N aligns north, Home resets pivot.

The globe renders one of two GPU pipelines depending on the "Enable Volume Rendering" toggle:

- *Surface patch* — a spherical-cap mesh lifted to a 1.003 Earth-radii radius, sampling the same flat-renderer textures. Renders a single elevation.
- *Volumetric* — packs all (up to 25) elevations into one large 2D unsigned-integer texture and ray-marches at half resolution with trilinear blending across elevation pairs, front-to-back alpha compositing, and a user-controlled density cutoff.

Sweep animation, the previous-sweep slot, data-age desaturation, the map scale bar, and the inspector / distance / storm-cell tools are 2D-only. Alert polygons are drawn on the 2D canvas only.

#### Canvas overlays

Drawn in order (each toggleable except where noted):

1. National radar mosaic (CONUS composite) — with a circular cutout around the active site so the per-site radar isn't obscured.
2. Geographic layers — states, counties, cities, labels, other NEXRAD sites.
3. The radar texture itself.
4. Range rings and radial guides.
5. NWS alert polygon footprints (2D only).
6. Sweep line and donut chart (when sweep animation is on).
7. NEXRAD site markers.
8. Info overlay (top-left) — site, time, elevation, age.
9. Color scale legend (right edge).
10. Map scale bar (bottom-left, 2D only) — stacked km / miles bar, snaps to round `{1, 2, 5} × 10ⁿ` values, recomputed every frame from the projection.
11. Inspector tooltip and crosshair (on hover, when active).
12. Distance-measurement line.
13. Storm-cell bounding boxes.
14. GPS location dot ("My Location").
15. mPING storm-report markers.
16. Compass rose (3D only).

## 4. Rendering Model

### Core invariants

1. **The playback position is a hard temporal boundary.** The canvas never displays data from the future relative to the playback position, even if such data exists in the cache. Sweep resolution uses `sweep_end ≤ playback_position`.
2. **Product and elevation selection is a hard eligibility filter.** When the user selects a specific product and elevation (e.g. 0.5° REF), only matching sweeps are eligible. When "Auto (latest sweep)" is on, the most recently completed sweep — any elevation — is eligible.
3. **At each spatial position, the most recent eligible value is rendered.** Each on-screen pixel resolves to an azimuth/range tuple and samples the most recent eligible gate value.

### GPU pipeline

Rendering is fully GPU-resident. There is no CPU polar→Cartesian image step:

1. The fragment shader converts pixel position to (azimuth, range) about the radar center.
2. Range-checks against the sweep's `[first_gate_km, max_range_km)`.
3. Binary-searches the sorted azimuth texture for the nearest radial (rejecting hits more than `1.5 × azimuth_spacing_deg` away).
4. Samples a raw gate value from a 2D R32F texture (gate × azimuth).
5. Rejects sentinels: raw `0` = below threshold, raw `1` = range-folded — tested with `v > 1.5`.
6. Converts raw → physical with `(raw − offset) / scale` (uniform per frame).
7. Normalizes against the product's value range and samples the 1024-entry RGBA LUT.
8. Outputs premultiplied alpha.

Pan, zoom, opacity, palette, and sweep animation are uniform changes; per-frame cost is dominated by the fullscreen quad. Bilinear interpolation works directly on raw values because the raw-to-physical transform is linear.

### Accumulation strategy

In archive mode the canvas displays only fully completed sweeps. There is no progressive sweep animation; the displayed sweep updates discretely when the next eligible sweep's end time falls behind the playback position.

In real-time mode the renderer additionally shows partially-collected sweeps. The current sweep's already-arrived radials are drawn live; the "previous sweep" slot continues to hold the most recently completed sweep, blended through the sweep-line transition. Optional sweep animation reveals new data behind the rotating arc; optional data-age desaturation fades the oldest radials ahead of the sweep line.

At high playback speeds where the playback position advances past multiple sweeps between rendered frames, the renderer shows the most recent complete eligible sweep rather than flashing through every intermediate sweep.

## 5. Timeline and Playback

### Bounds and zoom

The timeline represents a continuous time axis with hard bounds. The right bound is `now + ε`. The left bound is the start of available NEXRAD data collection. User interaction cannot extend beyond these bounds.

Zoom is continuous (pixels per second) and changes which behaviors are available (see *Micro* / *Macro* above). Tick spacing adapts down to one-second granularity at maximum zoom.

### Data availability visualization

At all zoom levels, data availability segments are color-coded by Volume Coverage Pattern, making VCP transitions visible at the broadest scales.

At coarse zoom, the timeline renders solid filled segments indicating contiguous regions where data exists.

At closer zoom, segments decompose into individual scans with visual indicators communicating completeness (fully downloaded vs. partial via hatch patterns), VCP identity, and VCP transition boundaries.

At sweep-level zoom, scans decompose into constituent sweeps reflecting the active VCP structure, with the currently-selected elevation highlighted and connectors drawn between sweeps of the same elevation across volumes.

### Modes

- **Navigate** (default): click to seek; drag the cursor to scrub. Data for the targeted moment is acquired on demand if not already cached. Shift-click-drag or shift-click-endpoints sets a selection range; in Advanced mode the Download button enqueues every scan in the range.
- **Real-time**: the timeline locks to "now"; the application streams incoming chunks. The user may scrub backward within the historical window while streaming continues in the background — data acquisition and playback position are independent. Scrubbing exits live mode (recorded as `UserSeeked`).

Live mode is implicitly entered by clicking the mode-badge "Go live" option or via Ctrl+L.

### Playback controls

Playback supports play/pause, step backward/forward (snapping to sweep ends in micro mode), Now (jump to wall-clock), variable speed (nine micro speeds, five macro speeds), and loop modes (Loop / Ping-Pong / Once) when a selection is set.

### Datetime jump

In Advanced mode, clicking the current-time readout opens a date/time picker (separate Y/M/D and H/M/S fields). Supports both UTC and local timezone input with validation feedback.

## 6. Data Acquisition

When the playback position targets uncached data, the required scans are fetched on demand. When a time range is selected and downloaded, scans within the range are enumerated, queued, and fetched.

The download pipeline runs through a unified retry policy (`net::retry::with_retry`) with full-jitter exponential backoff. Real-time chunk fetches use a tuned policy: 500 ms base, 4 s cap, 6 attempts max, 15 s total budget, 5 s per-attempt timeout.

Acquisition feedback appears in two places:

- **Transport bar** — active / completed / queued request count, transferred byte total, pipeline indicator (DL → PROC → GPU phases lit by current activity).
- **Acquisition drawer** (dev mode) — per-operation status, progress, retry / cancel / skip controls; per-operation network log under a second tab.

When a request fails, an error notification appears with retry and dismiss controls. Failures do not block other pending downloads.

## 7. Real-Time Streaming

### Phases and lifecycle

Live mode progresses through five phases (surfaced in the top bar and transport bar):

- **AcquiringLock** — initial connection. Bounded by a 10 s timeout.
- **Streaming** — actively receiving data.
- **WaitingForChunk** — sleeping until the next chunk is expected to be available.
- **Error** — connection failed or lost.
- **Idle** — not live.

### Loop

The streaming task drives a predict–sleep–fetch–emit loop:

1. **Acquire**: parallel S3 list across the 999 real-time volume buckets, fetch the latest chunk, and (if mid-volume) separately fetch the volume's Start chunk so the VCP can be parsed.
2. **Init backfill**: emit chunks the renderer needs to display the user's selected sweep on connect — current sweep's prior chunks (Auto mode) or all already-published chunks of the selected elevation (Fixed mode). `cached_elevations_for_scan` skips anything already in IndexedDB.
3. **Steady state**: drain pending observations, predict the next chunk's S3 availability time, sleep that wait plus a 750 ms pad, fetch with bounded retries, emit `ChunkData` + `ChunkReceived` (with arrival diagnostics), persist timing stats.

Filter changes during streaming run a mid-stream backfill before re-targeting; volume rollovers always emit the Start chunk first; when the filter excludes everything remaining in the current volume the loop projects across the volume boundary to predict the next match in the following volume.

### Predictions

Predicted intervals are computed from a shared `estimate_interval` primitive that returns either pure physics (when no stats are available) or a 70/30 blend of physics and the rolling average of the last 10 observed S3 deltas for that chunk's bucket. Buckets are keyed on `(chunk_type, waveform, channel_configuration, is_first_in_sweep)`.

Physics covers inter-volume gaps (constant 8.5 s), inter-sweep gaps (`0.7 s + elevation_slew + chunk_duration + waveform_transition_penalty`), and intra-sweep timing derived from the VCP's azimuth rate. The waveform-transition penalty is an empirically tuned table (e.g. CS → CDW = 4.0 s, B → CDWO = 3.5 s).

Timing stats persist to localStorage per site (`nexrad_timing_stats_<SITE>`, schema version 2) so the first prediction in a new session benefits from prior observations.

### What's available where

Most modal diagnostics ride above the streaming loop: the live-mode phase, chunk count and ETA appear in the top bar and transport bar; the timeline draws future-sweep dashed outlines and a per-chunk placeholder at projected collection times; the VCP forecast modal (dev mode) compares predicted vs. observed sweep timing.

When streaming begins mid-volume the system always fetches record 1 (the header) of the current archive, which contains the VCP and other metadata required to interpret subsequent records.

## 8. Caching

### Storage layout

Data persists in IndexedDB under database `nexrad-workbench` (schema version 5; destructive upgrades). Three object stores:

- **`sweeps`** — pre-computed sweep blobs keyed `SITE|SCAN_MS|ELEV_NUM|PRODUCT`. Each value is a single `ArrayBuffer` with a 72-byte little-endian header (counts, geometry, scale/offset, sweep start/end times) followed by the sorted azimuth array, optional per-radial collection times (format version 1), and the raw u8 / u16 gate-values matrix. Stored zero-copy to GPU on render.
- **`scan_index`** — per-scan metadata keyed `SITE|SCAN_MS`. Holds the full VCP, source filename, the list of cached sweeps (with elevation, time bounds, start azimuth, product list) and total size. Structured-cloned via `serde-wasm-bindgen`.
- **`scan_touches`** — bare `i64` Unix-millisecond timestamps for LRU bookkeeping, written by `create_scan` (seed) and bumped by `get_sweep` (throttled 60 s per scan).

The split exists for safety: real-time chunk-ingest does a read-modify-write on `scan_index`; if access bumps shared that store they could race with the merge, so access lives in its own single-field store. Two write paths (`create_scan` and `put_scan`) ensure repeated chunk-ingest flushes don't refresh the LRU timestamp.

### Cache behavior

Downloaded data is cached and reused when the user revisits previously-viewed times. The cache survives page reload. When the storage estimate would be exceeded, older scans are evicted by least-recently-used touch timestamp (entries with no touch evict first).

Users can configure the storage quota (100 MB to 20 GB; default 2 GB; eviction target 80 % of quota) and manually clear the cache or wipe the entire application from the Storage section in the right sidebar.

`navigator.storage.estimate()` is consulted before each write; a `QuotaExceeded` outcome surfaces to the user rather than failing mid-transaction.

## 9. Application Configuration

### URL and deep linking

The application state is encoded in the URL to support deep linking and sharing.

**Transparent parameters** (human-readable):

- `site` — site ID (e.g. `KDMX`).
- `t` — playback time as Unix seconds (float).
- `product` — `REF` / `VEL` / `SW` / `ZDR` / `CC` / `KDP` / `CFP`.
- `lat`, `lon` — map center coordinates.
- `dev=true` — enable developer mode.
- `ui=advanced` or `ui=basic` — override the UI tier from preferences.

**Opaque view state** is encoded in a single base64-encoded JSON `v` parameter (map zoom, timeline zoom, view/camera mode and parameters, volume 3D toggle and density cutoff, real-time flag for re-entering live on reload). This evolves freely without changing the URL schema.

URL updates are throttled (~1 / sec) by the persistence manager so browser back/forward navigation and bookmarking work without flooding the history stack.

### User preferences

Preferences persist in `localStorage` and apply across sessions. They are independent of URL state — the URL captures the current view, while preferences capture the user's defaults.

Persisted fields include: playback speed, elevation auto / preferred angle, layer toggles (states, counties, labels, NEXRAD sites, cities, national mosaic, alerts, mPING, GPS location), mPING API key, UTC vs. local time, preferred site (skips the welcome modal on subsequent visits), interpolation mode (Nearest / Bilinear), opacity, sweep animation, data-age desaturation, mobile override (auto / force mobile / force desktop), and Basic vs. Advanced tier.

### Appearance

The workbench currently ships **dark mode only**. Map base layers, UI chrome, and overlays render against a dark palette. (The codebase is structured to add a light theme later without API churn.)

### Mobile layout

On narrow viewports (or when touch input is detected) the application switches to a touch-first chrome:

- Desktop sidebars collapse.
- The timeline is replaced by a compact scrubber.
- A tabbed settings modal exposes playback, product, layer, and miscellaneous controls.
- Multi-touch gestures drive the 2D canvas — single-finger drag pans, two-finger pinch zooms.

The user may override auto-detection in preferences to force the desktop or mobile chrome. The 3D globe view is desktop-only.

### Site selection

The site button opens a modal with three selection paths:

1. **Use My Location** — browser geolocation; finds the nearest NEXRAD site.
2. **Enter Zip Code** — 5-digit US zip lookup.
3. **Browse NEXRAD Sites** — searchable list of all ~200 sites by ID, name, or state.

On first visit (no `preferred_site` saved) the modal opens automatically with a welcome screen; subsequent visits open the same flow on demand from the top-bar site button.

### Keyboard shortcuts

| Group | Shortcut | Action |
| --- | --- | --- |
| Playback | Space | Play / pause |
|  | `[` / `]` | Step backward / forward |
|  | `-` / `=` | Decrease / increase speed |
|  | Ctrl+L | Toggle live mode |
|  | P | Cycle product |
|  | E | Cycle elevation (2D) |
|  | S | Open site selection (2D) |
| View | 1 / 2 / 3 / 4 | 2D / 3D Site / 3D Planet / 3D Free |
|  | T | Toggle last 2D ↔ 3D |
| Camera (3D) | WASD / arrows | Move / pan |
|  | Q / E | Down / up |
|  | Shift | 2× speed |
|  | Ctrl | ¼× speed |
|  | R | Reset camera |
|  | F | Focus on radar site |
|  | N | Align north |
|  | Home | Reset pivot |
| General | `?` | Toggle help overlay |
|  | Esc | Close modal / overlay |
|  | Ctrl+Shift+A | Toggle Basic / Advanced |

### Developer mode

Enabled via the right-sidebar checkbox or `?dev=true`. Adds:

- FPS counter, pipeline indicator (DL / PROC / GPU phases), active-request count, request totals, COI badge.
- Click-through to the stats modal (download / processing / rendering latencies), the network-log modal, and the VCP forecast modal (predicted vs. observed sweep timing, copy-to-clipboard for offline analysis).
- The acquisition drawer (queue + network tabs), expanded by clicking the network-stats indicator.

## 10. Execution Model

### Browser-only

All core functionality runs client-side: data acquisition, decompression, decoding, GPU rendering, and persistence. No backend service is required. The application is served as static assets (Trunk-bundled WASM + JS + HTML).

### Workers and parallelism

Heavy computation runs in a pool of Web Workers:

- Pool size: `hw_concurrency − 1`, clamped to `[1, 4]` (fallback 2).
- Archive ingest and render commands round-robin across the pool; real-time chunk ingest is pinned to worker 0 so per-worker `CHUNK_ACCUM` accumulators don't fragment across workers.
- Six message types: `init`, `ingest`, `ingest_chunk`, `render`, `render_live`, `render_volume`. Payloads cross the boundary via `postMessage` with transferable `ArrayBuffer`s (zero-copy).

A service worker installs Cross-Origin-Opener-Policy and Cross-Origin-Embedder-Policy headers so the document is cross-origin isolated (required for some `SharedArrayBuffer`-based optimizations) and collects per-request network metrics that feed the dev-mode session stats.

### Inherent constraints

- Processing throughput is limited by available CPU cores and Web Worker parallelism.
- Memory is constrained by browser limits.
- Storage is constrained by IndexedDB quotas (`navigator.storage.estimate()`).
- Network requests are subject to browser connection limits and CORS policies; all data sources must be CORS-accessible from the browser.
- WebGL2 only (no compute shaders, no SSBOs); all data flows through 2D textures. The fragment shader's azimuth binary search runs a fixed 10 iterations, sufficient for any operational VCP.

### Intentional limitations

- **Proprietary data sources**: only publicly available NEXRAD data and the public NWS / MRMS overlays are supported.
- **Online-first**: network access is assumed for data acquisition; the application caches data aggressively but doesn't function as a fully offline tool.
- **Derived products** (storm tracking, precipitation estimates) are out of scope. Storm-cell detection is the one exception, exposed as a thresholded detection tool rather than a forecast product.
- **Dark mode only** at present.
- **No cross-tab coordination**: multiple tabs share the IndexedDB store but don't coordinate writes; ingest is idempotent at the scan-key level so concurrent tabs at worst duplicate work.
