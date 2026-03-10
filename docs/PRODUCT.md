# NEXRAD Workbench — Product Definition

## 1. Vision and Principles

NEXRAD Workbench is a browser-based technical workbench for viewing and analyzing NEXRAD radar data. It operates entirely client-side with no backend services; all data is fetched and processed in the browser.

The product prioritizes transparency, inspectability, and correctness over abstraction. Users should be able to see exactly what the radar data contains and how it maps to the rendered visualization. Performance and responsiveness are first-order concerns.

The application avoids brittle, multi-step workflows that can fail partway through and require custom recovery UI. User actions should do simple, consistent things regardless of the current application state: enqueue archives, toggle streaming, change the playback position. Complexity is managed by the system, not imposed on the user.

## 2. Core Concepts and Terminology

### Spatial Hierarchy

A **radar site** is a physical NEXRAD WSR-88D installation identified by a site ID (e.g. `KDMX`). The site defines the geographic origin for all spatial calculations and rendering.

The fundamental unit of data is the **volume scan**: a full multi-elevation volume sampled by the radar, with a typical duration of 5-10 minutes. A scan is composed of multiple **sweeps**, each corresponding to a specific elevation angle. Elevations may repeat within a single scan. The ordered sequence and parameters of sweeps define the **Volume Coverage Pattern (VCP)**.

Each sweep contains **radials**, single rays extending outward from the radar at a specific azimuth and elevation. Radials are the smallest spatial unit directly rendered to the canvas. Each radial contains a sequence of **gates**, fixed-distance samples representing measurement values at specific ranges from the radar. Gate spacing and count are product-dependent.

### Products

A **product** is a specific radar measurement type (e.g. reflectivity, velocity). Each sweep collects data for one or more products simultaneously; within a sweep, each radial contains gate values for each product. Products define their own value domains, units, and color tables, and are rendered independently.

### Data Sources

The workbench sources data from public AWS S3 buckets provided by AWS Open Data and Unidata.

**Archive data** consists of Archive II volume files, each containing a complete volume scan. Archive data is historical, complete, and immutable, accessed by time and site.

**Real-time data** consists of chunks (LDM records) delivered incrementally as the radar produces them. Volume scans are assembled from chunks as they arrive and may be partially available at any given moment.

### Data Structure

A scan is composed of one **header record** followed by a sequence of **data records**. Each record is Bzip2-compressed and contains radar data spanning one or more radials; records often cover only part of a sweep and may cross sweep boundaries. The terms "record" and "chunk" are effectively equivalent.

The **header record** (the first record in every scan) contains radar operational parameters, scan configuration, and the Volume Coverage Pattern (VCP). This record is required to correctly interpret all subsequent records.

### Time Model

**Playback position** is the moment in radar time whose data is currently displayed. The visualization may include data at or before that moment, depending on viewer parameters. Playback position is independent of wall-clock time during archive playback.

**Wall-clock time** is the current real-world time, relevant primarily for real-time streaming.

**Real-time mode** is a timeline/playback mode (distinct from real-time data as a source). In real-time mode, playback position tracks wall-clock time and the timeline is snapped to "now". In archive mode, playback position and wall-clock time are decoupled.

The distinction between archive and real-time data is intentionally blurred at the data level. Both are composed of chunks; archive files are simply the accumulated result of chunk delivery. Each Archive II volume file contains exactly one volume scan.

## 3. Delivery Phases

The product is delivered in four phases, each building on the previous. Each phase produces a usable product; later phases layer in rendering richness, operational depth, and multi-site support.

**Phase 1 — Core single-site viewer** ([PHASE-1.md](PHASE-1.md)) — *largely complete*. A complete single-site radar viewer with the simplest viable rendering model. Users can select a site, navigate time via the timeline, view radar imagery rendered as complete sweeps, and stream real-time data. Data acquisition feedback is minimal — a progress indicator and error notifications. This phase establishes the full application layout, the timeline with zoom-dependent data availability decomposition, playback controls, product/elevation selection, and the IndexedDB caching layer. Local file upload is not yet implemented.

**Phase 2 — Rich rendering** ([PHASE-2.md](PHASE-2.md)) — *not started*. Adds progressive sweep animation and full data age visualization. Two additional accumulation strategies — continuous (wiper) and sweep-isolated — enable the viewer to animate radar sweeps as they progress, giving users fine-grained temporal control over what appears on the canvas. Sweep boundary lines, age labels, and optional age attenuation make the temporal structure of rendered data visually explicit.

**Phase 3 — Acquisition depth** ([PHASE-3.md](PHASE-3.md)) — *partially complete*. Replaces the simple acquisition feedback with a full transparency layer. An expandable acquisition queue shows individual requests with status, progress, and controls to pause, cancel, or reprioritize. Per-chunk latency metrics surface radar collection delay and distribution latency. Error handling pauses the queue to prevent cascading failures. Real-time streaming gains predictive visualization: a shaded future region, estimated time until the next chunk, and a sweep line extending beyond received data. The predictive visualization for real-time streaming is implemented; the acquisition queue and error-pause behavior are not.

**Phase 4 — Multi-site** ([PHASE-4.md](PHASE-4.md)) — *not started*. Extends the application to support multiple simultaneous radar sites. Multi-site selection, overlapping polar projections with mosaic compositing, and stacked per-site timeline tracks sharing a single playback position. All prior single-site capabilities apply independently to each active site.

### Beyond the Original Phases

Several capabilities have been implemented that were not part of the original phase plan:

- **3D globe view** with volumetric ray-marching renderer and radar projection onto a 3D globe surface
- **Storm cell detection** with configurable dBZ thresholds and canvas overlay
- **Inspector tool** showing lat/lon coordinates and data values on hover
- **Distance measurement tool** between two points on the canvas
- **Datetime jump picker** for navigating directly to a specific date/time
- **Configurable storage management** with quota settings and LRU eviction
- **Web Worker decoding** offloading heavy computation to a dedicated thread

## 4. Application Layout

### Overview

The application uses a panel-based layout centered on the map canvas. The conceptual interaction flow is:

1. **Site selection** establishes "where" — which radar site to work with.
2. **Timeline interaction** establishes "when" and "what to acquire" — users express their intent (view a moment, download a range, stream real-time) through the timeline, governed by the active mode.
3. **Acquisition executes visibly** — requests and transfers are observable.
4. **Playback position drives the UI** — the current moment in the timeline determines what the map renders, what the radar operations panel displays, and what data is relevant.

The **timeline is the central control surface**: it simultaneously presents temporal context, drives data acquisition, and governs the playback position from which all other UI state derives.

The layout consists of six regions:

- **Top bar**: Site context and global state
- **Left sidebar**: Radar operations (read-only state driven by playback position)
- **Center**: Map canvas (primary visualization output)
- **Right sidebar**: Rendering parameters (user-controlled display settings)
- **Bottom dock**: Timeline complex (temporal navigation, data acquisition, playback)
- **Status bar**: Session statistics and performance metrics (bottom edge)

### Site Context Bar

The top bar prominently displays the active radar site. A button opens the site selection modal, which presents all NEXRAD sites. Phase 4 extends this to support multiple simultaneous sites.

### Left Sidebar: Radar Operations

Read-only panel displaying radar operations state derived from the current playback position. Collapsible.

### Right Sidebar: Rendering Parameters

User-controlled settings for product selection, processing, and rendering options. Collapsible. Both sidebars can be toggled independently via keyboard shortcuts.

### Bottom Dock: Timeline Complex

The bottom dock is the primary interaction surface, organized in three layers:

**Mode and acquisition status bar.** Displays the current timeline interaction mode and a compact summary of acquisition activity (e.g. active download count and progress).

**Timeline track.** The zoomable temporal axis displaying data availability, the playback position indicator, and any active range selection. Click and drag behavior depends on the active timeline mode. Scroll-to-zoom changes temporal scale.

**Transport bar.** Playback controls: play/pause, step forward/back, speed selector, current playback position readout, loop mode toggle, and a compact summary of the currently displayed data (product, elevation, sweep position, data staleness).

### Status Bar

A thin status bar at the bottom edge of the application displays session statistics and performance metrics: active and total network requests, volume of data downloaded in the current session, number of cached scans and records, total volume of cached data, rendering performance (FPS), decompression time, processing time, rendering time, and the number of active background workers. This bar is always visible and provides continuous insight into application health and resource usage.

## 5. Constraints

### Browser Execution Model

All core functionality runs client-side: data acquisition, decompression, decoding, and rendering. Heavy computation (Bzip2 decompression, binary decoding, rendering preparation) is isolated from the UI thread using WebWorkers. The main UI thread is reserved for user interaction and final composition.

This architecture creates inherent constraints:

- Processing throughput is limited by available CPU cores and WebWorker parallelism
- Memory is constrained by browser limits
- Storage is constrained by IndexedDB quotas
- Network requests are subject to browser connection limits and CORS policies

### Intentional Limitations

- **Proprietary data sources**: Only publicly available NEXRAD data sources are supported
- **Offline-first operation**: Network access is assumed for data acquisition; the application caches data but does not function as a fully offline tool
- **Derived products**: The workbench displays base radar moments; derived products (storm tracking, precipitation estimates) are out of scope

## 6. Application Configuration

### URL and Deep Linking

The application state is encoded in the URL to support deep linking and sharing. URL parameters fall into two categories:

**Transparent parameters** are human-readable and can be constructed programmatically: site ID, playback time, and product selection. A URL like `?site=KDMX&time=2024-05-20T03:35Z&product=REF` opens the application at a predictable state.

**Opaque parameters** encode remaining view state (map zoom, pan position, sidebar visibility, accumulation strategy, and other UI options) as a single base64-encoded parameter. This preserves the exact view on reload or when sharing, without requiring a large number of individual query parameters.

The URL updates as the user navigates, enabling browser back/forward navigation and bookmarking.

### User Preferences

User preferences are persisted in browser local storage. Preferences include default accumulation strategy, preferred color tables, map layer visibility, playback speed defaults, age attenuation settings, and other rendering parameters. Preferences apply across sessions and are independent of URL state — the URL captures the current view, while preferences capture the user's defaults.

### Appearance

The application supports dark and light appearance modes, defaulting to the operating system's preference. Map base layers, UI chrome, and all interface elements adapt to the active mode. The user may override the OS default via application settings.

### Keyboard Shortcuts

The application provides keyboard shortcuts for power users. Shortcuts cover playback controls (play/pause, step, speed adjustment), sidebar toggling, product/elevation cycling, site switching, and tool activation. A help overlay (`?` key) lists all available shortcuts.
