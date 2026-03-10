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

## 3. Application Layout

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

The top bar prominently displays the active radar site. A button opens the site selection modal, which presents all NEXRAD sites with three selection methods: browsing a searchable site list, entering a zip code, or using browser geolocation.

### Left Sidebar: Radar Operations

Read-only panel displaying radar operations state derived from the current playback position. Provides three coordinated views:

- **Azimuth view**: A top-down compass visualization displaying a rotating sweep line and the current azimuth.
- **Elevation view**: A side-profile diagram displaying the elevation angle of the current sweep.
- **VCP view**: Renders the volume coverage pattern as a structured sequence, displaying sweep elevations and parameters as a "playlist" and highlighting the current sweep's position within the scan.

Collapsible via keyboard shortcut.

### Right Sidebar: Rendering Parameters

User-controlled settings for product selection, processing, and rendering options. Includes:

- **Product selection**: Reflectivity, Velocity, Spectrum Width, Differential Reflectivity, Correlation Coefficient, Differential Phase, and Clutter Filter Power.
- **Elevation selection**: Slider with VCP snap-to-nearest behavior.
- **Render mode**: Fixed Tilt (specific product and elevation) or Most Recent (latest data regardless of product/elevation).
- **Tools**: Inspector tool (hover for lat/lon and data values), distance measurement tool (click two points), and storm cell detection (configurable dBZ threshold with canvas overlay).
- **Geographic layers**: Toggles for state boundaries, county boundaries, city labels, site markers.
- **Storage management**: Cache quota slider (100 MB to 20 GB), usage display, clear cache and reset app controls.

Collapsible via keyboard shortcut.

### Bottom Dock: Timeline Complex

The bottom dock is the primary interaction surface, organized in three layers:

**Mode and acquisition status bar.** Displays the current timeline interaction mode (NAVIGATE or REAL-TIME) and a compact summary of acquisition activity (active download count and progress).

**Timeline track.** The zoomable temporal axis displaying data availability, the playback position indicator, and any active range selection. Scroll-to-zoom changes temporal scale. Data availability segments are color-coded by Volume Coverage Pattern. At closer zoom levels, segments decompose into individual scans with visual indicators for completeness (fully downloaded vs. partial) and VCP identity. At sweep-level zoom, scans decompose into constituent sweeps reflecting the VCP structure.

**Transport bar.** Playback controls: play/pause, step forward/back, speed selector (seven levels from 1x real-time to 20 min/s), current playback position readout, loop mode toggle (loop, ping-pong, once), and a compact summary of the currently displayed data.

### Status Bar

A thin status bar at the bottom edge displays session statistics and performance metrics: active and total network requests, volume of data downloaded, number of cached scans and records, total cached data volume, and a stats modal for detailed session information.

## 4. Map Canvas

The primary view is a map canvas displaying radar data overlaid on geographic context. Radar data is rendered in polar coordinates centered on the radar site and projected onto the map. The canvas supports pan and zoom.

### 2D View

The default view renders radar data on a flat map projection with geographic overlays (state boundaries, county boundaries, city labels). Geographic layers are toggleable. The application supports dark and light map themes, matching the active appearance mode.

### 3D Globe View

An alternative rendering mode projects radar data onto a 3D globe surface using a volumetric ray-marching renderer. The globe view supports three camera modes: planet orbit, site orbit, and free look. Camera controls include WASD movement, mouse rotation, and adjustable speed (Shift for 2x, Ctrl for 1/4x). The view can be toggled between 2D and 3D via keyboard shortcuts (T to toggle, 1-4 for specific camera modes).

### Tools

- **Inspector**: Hover over the canvas to see lat/lon coordinates and data values at the cursor position.
- **Distance measurement**: Click two points on the canvas to measure the distance between them.
- **Storm cell detection**: Identifies storm cells based on a configurable dBZ threshold (default 35.0 dBZ), renders cell boundaries and centroids as a canvas overlay with area calculations.

## 5. Rendering Model

### Core Invariants

1. **The playback position is a hard temporal boundary.** Only data collected at or before the playback position is eligible for rendering. The canvas never displays data from the future relative to the playback position, even if that data exists in the cache.
2. **Product and elevation selection is a hard eligibility filter.** When the user selects a specific product and elevation (e.g. 0.5° REF), only radials matching both criteria are eligible. When no filter is applied ("most recent" mode), all products and elevations are eligible.
3. **At each spatial position, the most recent eligible value is rendered.** Each point on the canvas corresponds to a specific azimuth and range. The rendered value is the most recent eligible gate value at or before the playback position.

### Accumulation Strategy

The viewer uses a **complete sweeps** accumulation strategy. The canvas displays only fully completed sweeps. There is no progressive sweep animation; the display updates discretely when a sweep's collection end time falls behind the playback position. At that moment, the most recent complete sweep matching the active filter is rendered as a full 360°.

At high playback speeds where the playback position advances past multiple sweeps between rendered frames, the renderer shows the most recent complete eligible sweep rather than flashing through every intermediate sweep.

## 6. Timeline and Playback

### Timeline Bounds

The timeline represents a continuous time axis with hard bounds. The right bound is `now + ε` (a small fixed future buffer). The left bound is the start of available NEXRAD data collection. User interaction cannot extend beyond these bounds.

### Zoom and Scale

The timeline supports zooming to change temporal scale. Zoom level governs which operations are available:

- When zoomed too far out, playback is disabled to avoid data acquisition and processing bottlenecks.
- When zoomed sufficiently far in, real-time mode becomes available.

### Data Availability Visualization

The timeline's visual representation changes with zoom level, progressively revealing more structural detail. At all zoom levels, data availability segments are color-coded by Volume Coverage Pattern, making VCP transitions visible even at the broadest scales.

At coarse zoom levels, the timeline renders solid filled segments indicating contiguous regions where data exists. VCP color-coding is the primary structural information — the user can see at a glance when a site transitioned between clear-air and precipitation modes.

At closer zoom, segments decompose into individual scans. Visual indicators communicate completeness (fully downloaded vs. partial), VCP identity, and VCP transition boundaries. Hatch patterns indicate partial scans.

At sweep-level zoom, scans decompose into constituent sweeps reflecting the active VCP structure.

### Timeline Modes

The timeline operates in distinct interaction modes:

- **Navigate mode** (default): Click sets the playback position. Drag scrubs through time. Data for the targeted moment is acquired on demand if not already cached. When a time range is selected (shift-click-drag or shift-click to set range endpoints), the range becomes the playback loop boundary and archive downloads begin for all scans within it. Playback within a range loops or ping-pongs depending on configuration.
- **Real-time mode**: The timeline locks to "now" and continuously advances. The right edge is snapped to wall-clock time; the left edge is constrained to a fixed historical window. The application streams incoming chunks as the radar produces them. The user may scrub backward within the historical window while streaming continues in the background — data acquisition and playback position are independent.

Modes are indicated in the timeline dock's status bar and can be switched via controls or keyboard shortcuts. Enabling streaming implicitly enters real-time mode.

Regardless of mode, when streaming begins mid-volume, the system always fetches record 1 (the header) of the current archive, which contains the VCP and other metadata required to interpret subsequent records.

### Playback Controls

The timeline always has a playback position representing the moment whose data is displayed. Playback supports:

- Pause and resume
- Step forward and back
- Variable playback speed, from real-time (1:1) to accelerated rates (seven levels up to 20 min/s)
- Loop modes: loop, ping-pong, and once

### Datetime Jump

A datetime picker dialog allows navigating directly to a specific date and time. Supports both UTC and local timezone input with validation feedback.

## 7. Data Acquisition

When the playback position targets uncached data, the required scans are fetched on demand. When a time range is selected, scans within the range are enumerated and downloaded.

Acquisition feedback includes a progress indicator showing active and completed fetches and download count. When a request fails, an error notification appears with the option to retry or dismiss. Failures do not block other pending downloads.

## 8. Real-Time Streaming

When streaming real-time data with playback locked to real time, the viewer renders all data received so far for the active sweep. The live mode tracks streaming phases (idle, acquiring lock, streaming, waiting for chunk, error) and provides estimated timing for chunk arrival.

When streaming but not locked to real time, visualization behavior is identical to archive playback.

## 9. Caching

### Storage Model

Data is persisted in browser storage (IndexedDB) in two logical categories:

**Payload storage** holds the actual radar data as pre-computed sweep blobs. This format preserves rendered data for efficient retrieval.

**Index storage** is a lightweight index tracking what data exists in storage. The index enables fast timeline construction and efficient planning of batch record loads. It tracks which scans are present, whether each scan is complete or partial, temporal bounds, and per-record metadata.

### Cache Behavior

Downloaded data is cached and reused when the user navigates to previously-viewed time ranges. Cache persists across sessions. When storage limits are reached, older data is evicted according to a least-recently-used policy. Users can configure the storage quota (100 MB to 20 GB) and manually clear the cache via controls in the right sidebar.

## 10. Application Configuration

### URL and Deep Linking

The application state is encoded in the URL to support deep linking and sharing. URL parameters fall into two categories:

**Transparent parameters** are human-readable and can be constructed programmatically: site ID (`site`), playback time as Unix timestamp (`t`), product selection (`product`), and center coordinates (`lat`, `lon`). A URL like `?site=KDMX&t=1700000000&product=REF` opens the application at a predictable state.

**Opaque parameters** encode remaining view state (map zoom, pan position, 3D camera settings, and other UI options) as a single base64-encoded parameter (`v`). This preserves the exact view on reload or when sharing.

The URL updates as the user navigates, enabling browser back/forward navigation and bookmarking.

### User Preferences

User preferences are persisted in browser local storage. Preferences include playback speed, render mode, geographic layer visibility, timezone preference (local vs. UTC), and preferred site. Preferences apply across sessions and are independent of URL state — the URL captures the current view, while preferences capture the user's defaults.

### Appearance

The application supports dark and light appearance modes, defaulting to the operating system's preference. Map base layers, UI chrome, and all interface elements adapt to the active mode. The user may override the OS default via application settings.

### Keyboard Shortcuts

The application provides keyboard shortcuts for power users:

- **Playback**: Space (play/pause), `[`/`]` (step), `-`/`=` (speed), Ctrl+L (live mode), P (cycle product), E (cycle elevation), S (site selection)
- **View**: 1/2/3/4 (2D, site orbit, planet orbit, free look), T (toggle 2D/3D)
- **Camera (3D)**: WASD/arrows (move), Q/E (up/down), Shift (2x speed), Ctrl (1/4x speed), R (reset), F (focus), N (north), Home (reset pivot)
- **General**: `?` (help overlay), Esc (close modal)

### Web Worker Decoding

Heavy computation (Bzip2 decompression, binary decoding, sweep rendering) is offloaded to a dedicated Web Worker, keeping the main UI thread responsive for user interaction.

## 11. Constraints

### Browser Execution Model

All core functionality runs client-side: data acquisition, decompression, decoding, and rendering. Heavy computation is isolated from the UI thread using a Web Worker. The main UI thread is reserved for user interaction and final composition.

This architecture creates inherent constraints:

- Processing throughput is limited by available CPU cores and WebWorker parallelism
- Memory is constrained by browser limits
- Storage is constrained by IndexedDB quotas
- Network requests are subject to browser connection limits and CORS policies

### Intentional Limitations

- **Proprietary data sources**: Only publicly available NEXRAD data sources are supported
- **Offline-first operation**: Network access is assumed for data acquisition; the application caches data but does not function as a fully offline tool
- **Derived products**: The workbench displays base radar moments; derived products (storm tracking, precipitation estimates) are out of scope
