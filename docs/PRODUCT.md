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

1. **Site selection** establishes "where" — which radar site(s) to work with.
2. **Timeline interaction** establishes "when" and "what to acquire" — users express their intent (view a moment, download a range, stream real-time) through the timeline, governed by the active mode.
3. **Acquisition executes visibly** — every request, queue state, and transfer is observable.
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

The top bar prominently displays the active radar site(s). A button opens the site selection modal, which presents all NEXRAD sites with checkboxes for multi-selection. The number of simultaneously active sites is limited (initially ~3) to manage resource consumption. Each active site has a corresponding timeline track in the bottom dock.

In multi-site operation, all active sites are listed in the top bar. Site management (adding, removing, reordering) is handled through the selection modal.

### First-Run Experience

On first launch with no prior state, the site selection modal opens automatically. Once the user selects a site, the application enters real-time mode and acquires the most recent complete sweep for immediate display. This may require crawling through recent real-time chunks until a complete sweep is assembled, since individual chunks may not contain a full sweep.

### Left Sidebar: Radar Operations

The left sidebar presents radar operations state derived from the current playback position. This panel is read-only — it reflects what the radar was doing at the displayed moment. Contents include antenna position, volume coverage pattern plan and position, site location and metadata, and technical radar status. The sidebar is collapsible to maximize map canvas area.

### Right Sidebar: Rendering Parameters

The right sidebar contains user-controlled settings governing how data is displayed: product selection and elevation filtering, processing options (filtering, smoothing, thresholding), and rendering options (color table, opacity, interpolation). This sidebar is also collapsible. Both sidebars can be toggled independently via keyboard shortcuts.

### Bottom Dock: Timeline Complex

The bottom dock is the primary interaction surface, organized in three layers:

**Mode and acquisition status bar.** Displays the current timeline interaction mode (see §5 Timeline Modes) and a compact summary of acquisition activity (e.g. active download count and progress). An expand toggle opens the full acquisition queue and history as a drawer expanding upward, showing individual requests with their status, progress, and controls to pause or cancel. The drawer provides the detailed acquisition transparency described in §6.

**Timeline track.** The zoomable temporal axis displaying data availability, the playback position indicator, and any active range selection. This is the primary interaction surface; click and drag behavior depends on the active timeline mode. Scroll-to-zoom changes temporal scale. When multiple sites are active, each site has its own track stacked vertically, sharing the temporal axis and playback position (see §5 Multi-Site Timelines).

**Transport bar.** Playback controls: play/pause, step forward/back, speed selector, current playback position readout, loop mode toggle, and a compact summary of the currently displayed data (product, elevation, sweep position, data staleness).

### Status Bar

A thin status bar at the bottom edge of the application displays session statistics and performance metrics: active and total network requests, volume of data downloaded in the current session, number of cached scans and records, total volume of cached data, rendering performance (FPS), decompression time, processing time, rendering time, and the number of active background workers. This bar is always visible and provides continuous insight into application health and resource usage.

### Binary Inspector

The binary structure viewer (see §7) is not a persistent panel in the default layout. It activates as a dedicated mode or overlay when the user inspects raw data structure, and may replace or overlay a sidebar or expand as a supplementary panel.

## 4. Visualization and Interaction

### Map Canvas

The primary view is a map canvas displaying radar data overlaid on geographic context. Radar data is rendered in polar coordinates centered on the radar site and projected onto the map. The canvas supports standard map interactions: pan, zoom, and rotation.

The geographic base layer includes state boundaries and labels at all zoom levels. County boundaries and labels appear when zoomed sufficiently close. These geographic layers are optional and can be toggled by the user. The application supports dark and light map themes, matching the active appearance mode.

### Sweep Playback and Animation

In continuous and sweep-isolated accumulation strategies (see Rendering Model), the viewer animates the radar sweep progressively, rendering radials as the playback position advances through a sweep's collection time. The visual sweep animation is synchronized with the sweep's azimuthal progression and the temporal playback position. In complete sweeps mode, there is no progressive animation; the display updates discretely when each sweep completes.

### Radar Operations Panel

The left sidebar hosts the radar operations panel, providing multiple coordinated views driven by the current playback position:

- **Azimuth view**: A top-down view displaying a rotating sweep line and an icon at the radar location.
- **Elevation view**: A side-profile view displaying the elevation angle of the current sweep.
- **VCP view**: Renders the volume coverage pattern as a structured sequence, displaying sweep elevations and parameters as a "playlist" and highlighting the current sweep's position within the scan.

### Product Selection

Product selection controls are located in the right sidebar. A single scan may include multiple radar products (reflectivity, velocity, spectrum width, differential reflectivity, correlation coefficient, and others), collected at different elevations and possibly repeated within a scan. The viewer allows users to control what is rendered:

- Focus on a specific product (e.g. reflectivity only)
- Focus on a specific elevation (e.g. 0.5° tilt)
- Show "most recent" data regardless of product or elevation

These selections directly influence rendering behavior and data freshness semantics.

### Rendering Model

The rendering model defines how the playback position, data availability, and user selections combine to determine what appears on the canvas.

#### Core Invariants

1. **The playback position is a hard temporal boundary.** Only data collected at or before the playback position is eligible for rendering. The canvas never displays data from the future relative to the playback position, even if that data exists in the cache.
2. **Product and elevation selection is a hard eligibility filter.** When the user selects a specific product and elevation (e.g. 0.5° REF), only radials matching both criteria are eligible. When no filter is applied ("most recent" mode), all products and elevations are eligible.
3. **At each spatial position, the most recent eligible value is rendered.** Each point on the canvas corresponds to a specific azimuth and range. The rendered value is the most recent eligible gate value at or before the playback position.

#### Accumulation Strategies

The user selects an accumulation strategy that governs how data builds up and persists on the canvas. This is the primary control over the balance between spatial completeness and temporal purity.

**Continuous (wiper).** The default strategy. At each azimuth/range, the most recent eligible value is shown. As the radar sweeps, new data progressively overwrites older data at each azimuth. The canvas always shows maximum spatial coverage — a full 360° once initially populated. Data from different times coexists on the canvas simultaneously. Lookback is bounded to a reasonable horizon to prevent rendering arbitrarily old data without clear indication of its age.

**Sweep-isolated.** When a new sweep begins for the active product/elevation filter, the canvas clears entirely. Data paints in fresh from the sweep's starting azimuth. Only data from the current sweep is ever visible — a growing wedge until the sweep completes, then a full circle until the next matching sweep clears it. This guarantees temporal purity: all visible data comes from a single sweep. This strategy also inherently prevents mixing data from different elevations.

**Complete sweeps.** The canvas displays only fully completed sweeps. There is no progressive sweep animation; the display updates discretely when a sweep's collection end time falls behind the playback position. At that moment, the most recent complete sweep matching the active filter is rendered as a full 360°. This is the required strategy for playback speeds above near-real-time, where progressive animation would be meaningless. At high playback speeds where the playback position advances past multiple sweeps between rendered frames, the renderer shows the most recent complete eligible sweep rather than flashing through every intermediate sweep.

#### Data Age Visualization

The age of rendered data relative to the playback position must always be clearly communicated to the user. There is no hard staleness cutoff that hides old data; instead, age is made visually unambiguous so the user can judge data relevance themselves.

**Sweep boundary lines** (always on): Thin radial lines on the canvas at azimuths where data from different sweeps meets. These make the temporal structure of the rendered data visible — where one sweep's data ends and another's begins. Always present when data from multiple sweeps is on the canvas.

**Age labels at sweep boundaries** (always on): At each sweep boundary line, the age of the data on each side is annotated (e.g. "3m12s"). These provide precise temporal context at the transitions between sweeps.

**Periodic timestamp markers** (always on): Placed at fixed angular intervals (e.g. every 90°) around the render, these label the absolute timestamp or relative age of the data at that azimuth. This ensures the user always has a time reference nearby regardless of where they are looking, even within a single sweep.

**Age attenuation** (optional): A configurable visual effect where older data is progressively dimmed, desaturated, or otherwise attenuated relative to the newest data. When enabled, this provides an at-a-glance sense of data freshness across the entire canvas without reading labels. Configurable in the rendering parameters sidebar.

### Real-Time Locked Visualization

When streaming real-time data with playback locked to real time, the viewer:

- Animates the sweep using the most recent VCP information and received radials
- Renders all data received so far for the active sweep
- Renders a shaded future region indicating data currently being collected or expected in the next chunk
- Displays an overlay showing estimated time until the next chunk
- Allows the sweep line to continue beyond received data into the shaded region

When streaming but not locked to real time, visualization behavior is identical to archive playback.

### Latency Metrics

For each chunk, the system may surface latency measurements:

- Latency since the first radial in the chunk was collected
- Latency since the last radial in the chunk was collected
- Latency between chunk availability in the S3 bucket and download completion

These metrics provide insight into radar collection delay, distribution latency, and client-side acquisition performance.

## 5. Timeline and Playback Behavior

### Timeline Bounds

The timeline represents a continuous time axis with hard bounds. The right bound is `now + ε` (a small fixed future buffer). The left bound is the start of available NEXRAD data collection. User interaction cannot extend beyond these bounds.

### Zoom and Scale

The timeline supports zooming to change temporal scale. Zoom level governs which operations are available:

- When zoomed too far out, playback is disabled to avoid data acquisition and processing bottlenecks.
- When zoomed sufficiently far in, real-time mode becomes available.

Zoom may be locked depending on application mode.

### Timeline Modes

The timeline operates in distinct interaction modes that determine how user gestures are interpreted and what data acquisition behavior results:

- **Navigate mode** (default): Click sets the playback position. Drag scrubs through time. Data for the targeted moment is acquired on demand if not already cached.
- **Range mode**: Click-drag or shift-click defines a time range. The selected range becomes both the playback loop boundary and the acquisition target — archive downloads begin for all scans within the range.
- **Real-time mode**: The timeline locks to "now" and continuously advances. The application streams incoming chunks as the radar produces them, and playback position tracks wall-clock time.

Modes are indicated in the timeline dock's status bar and can be switched via controls or keyboard shortcuts. Some transitions are implicit: selecting a time range enters range mode; enabling streaming enters real-time mode.

### Real-Time Mode

In real-time mode, the right edge of the timeline is snapped to "now" and continuously held there. The left edge is constrained to a fixed window into the past (e.g. ~1 hour) to prevent unbounded historical buffering. Real-time mode may implicitly lock zoom and timeline bounds.

### Playback Controls

The timeline always has a playback position representing the moment whose data is displayed. Playback supports:

- Pause and resume
- Variable playback speed, ranging from real-time (1:1) to accelerated rates (e.g. 1 second of data per minute of wall-clock time, or faster)

At playback speeds above near-real-time, the complete sweeps accumulation strategy is required (see §4 Rendering Model). Certain playback speeds may be further restricted in specific modes to avoid acquisition or processing overload.

### Time Range Selection

Users can define a time range selection via shift-click-drag or by clicking to set an anchor and shift-clicking to set the range end. A selected time range becomes the playback range:

- Playback position is constrained within the range
- Playback proceeds forward and either loops or rocks back and forth (ping-pong), depending on configuration

### Archive Playback

When viewing archive data, the application downloads full Archive II volume files. If the user selects a playback time range, all required archive data for that range is fetched to ensure complete, gap-free playback.

### Real-Time Streaming

When streaming real-time data, the user may begin playback mid-archive and initially receive only a subset of chunks for the current volume. Regardless of start time, the system always fetches chunk 1 (record 1) of the archive, which contains critical metadata (Volume Coverage Pattern / VCP) required to interpret subsequent records.

### Completeness Visibility

The data manager and timeline must jointly model and expose archive completeness: whether a full archive is cached or only partial. For partial archives, the system tracks whether the VCP (chunk 1) is available and which chunks are present or missing. This state is communicated to the user so expectations around playback completeness are clear.

### Data Availability Visualization

The timeline's visual representation changes with zoom level, progressively revealing more structural detail. At all zoom levels, data availability segments are color-coded by the active Volume Coverage Pattern, making VCP transitions visible even at the broadest scales.

At coarse zoom levels, the timeline renders solid filled segments indicating contiguous regions where data exists. The representation answers only: "Is there any data here?" Individual scans and inter-scan gaps are not discerned. VCP color-coding is the primary structural information at this scale — the user can see at a glance when a site transitioned between clear-air and precipitation modes.

When zoomed out so far that a data region would be visually negligible (e.g. viewing months of time with only one hour of data), the timeline may artificially expand the visual width of that segment. This is a purely visual affordance for discoverability and does not imply actual temporal extent.

Once the user zooms in sufficiently, solid segments decompose into individual scans rendered discretely. At this level, visual indicators communicate whether a scan is complete (fully downloaded) or partial (constructed from streamed data). Each scan is identifiable by its VCP, and VCP transition points — where the radar switches from one coverage pattern to another — are marked as distinct boundaries.

Zooming in further decomposes scans into constituent sweeps. The sweep structure directly reflects the active VCP: sweep count, elevation angles, and timing are all VCP-determined. Visual encodings indicate sweep parameters (e.g. elevation angle) at a glance. For incomplete scans, the timeline shows the expected temporal extent; sweeps that occurred before streaming began appear as gaps, while cached sweeps appear in their correct positions.

### Playback While Streaming

In real-time streaming mode, the user may play back data from a bounded window preceding "now" (e.g. ~1 hour), enabling review of recent data while new chunks continue to arrive. Two orthogonal behaviors coexist:

- **Data acquisition mode**: streaming chunks in real time as they become available
- **Playback lock mode**: optionally locking playback to the latest moment ("now")

While streaming, the timeline prevents playback position and time-range selection from extending beyond the allowed historical window. This constraint applies whether or not playback is locked to "now".

### Multi-Site Timelines

When multiple radar sites are active, each site receives its own timeline track, stacked vertically within the bottom dock. All tracks share the same temporal axis — pan and zoom are synchronized — and a single playback position line spans all tracks.

Each track independently displays:

- Data availability segments for that site
- VCP color-coding and transition markers for that site
- Scan and sweep decomposition at close zoom levels
- Acquisition progress (cached, downloading, or missing scans)

The shared playback position means all sites render data at the same moment. Some sites may have data at the playback position while others do not; per-track data availability makes this clear without requiring any special handling.

## 6. Data Acquisition and Caching

### Automatic Archive Download

An optional automatic download mode performs proactive acquisition of archive data based on the current playback position or selected time range. When a time range is selected, archive downloads begin immediately for the scans required to fulfill that range. If the user changes the selection, in-progress downloads may be canceled or deprioritized.

### Data Acquisition Queue

Archive downloads are managed via a data acquisition queue. If multiple scans are required (e.g. five scans for a selected range), they are enumerated explicitly. The queue reflects pending, active, and completed downloads. Users can pause downloads, cancel downloads, or modify the queue by adjusting selections.

### Network Activity Visibility

Data acquisition transparency is a core requirement. Users must be able to see the state of every request: what is being fetched, what is queued, what has completed, and what has failed. The acquisition status bar and its expandable drawer in the bottom dock (see §3) provide this visibility.

For streaming, distinct phases are visible: acquisition/polling phase and chunk download phase. Expected delays between chunks are apparent, and retry attempts are observable.

For archive downloads, queued, active, and completed downloads are individually enumerable. Each download shows its target (site, scan, record), status, progress, and timing. Users can pause, cancel, or reprioritize queued downloads directly from the acquisition drawer.

### Error Handling and Recovery

When a download or streaming request fails, the error is displayed in the acquisition drawer with diagnostic information available on click or hover. A failure pauses the entire acquisition queue — both archive downloads and active streaming — to prevent cascading failures and give the user a clear moment to assess the situation. The user can retry the failed request, skip it, or resume the queue to continue with remaining items.

This approach reflects a broader design principle: the application does not attempt complex automatic recovery sequences. Instead, it stops, clearly communicates what happened, and gives the user simple controls to decide what to do next.

### Storage Model

Data is persisted in browser storage (IndexedDB) in two logical categories:

**Payload storage** holds the actual radar data as individual records in their native Bzip2-compressed form. This format is space-efficient and preserves original data boundaries. Records are the atomic unit of storage and retrieval, fetched in batches when fulfilling playback time ranges or archive downloads.

**Index storage** is a lightweight index tracking what data exists in storage. The index enables fast timeline construction and efficient planning of batch record loads. It tracks which scans are present, whether each scan is complete or partial, temporal bounds, and data availability within scans. The index must also include metadata about which products and elevations are present in each scan, enabling efficient lookback queries when the rendering model needs to find the most recent eligible data matching the active product/elevation filter without decompressing records.

### Cache Behavior

Downloaded data is cached and reused when the user navigates to previously-viewed time ranges. Cache persists across sessions. When storage limits are reached, older data is evicted according to a least-recently-used policy.

## 7. Inspection and Technical Transparency

The workbench serves as both a visualization tool and a learning/verification tool for understanding NEXRAD data formats.

### Local File Inspection

Users can upload local Archive II files for inspection without requiring network access. This supports offline analysis and examination of data from non-standard sources.

### Binary Structure Viewer

The application exposes the parsed binary structure of radar data at multiple levels:

- **Volume header**: Radar operational parameters, scan configuration, and VCP
- **Records**: Individual compressed data units with their boundaries and metadata
- **Messages**: Decoded radar messages within records, including message types and fields
- **Radials and gates**: The decoded moment data with raw numeric values

### Data-to-Visualization Mapping

Users can trace the relationship between raw binary data and rendered imagery. Selecting a gate on the map highlights the corresponding record, message, and byte range in the binary viewer. Conversely, selecting a structure in the binary viewer highlights the corresponding spatial region on the map.

This bidirectional mapping supports verification that the workbench is correctly interpreting the data format and helps users understand how radar data is structured.

## 8. Constraints and Intentional Limitations

### Web Application

The application is a web application: hosted and distributed as static assets, operating entirely in the browser. There is no desktop variant; the browser is the target runtime. There are no proprietary backend services. All data acquisition uses publicly available sources.

### Browser Execution Model

All core functionality runs client-side: data acquisition, decompression, decoding, and rendering. Heavy computation (Bzip2 decompression, binary decoding, rendering preparation) is isolated from the UI thread using WebWorkers. The main UI thread is reserved for user interaction and final composition.

This architecture creates inherent constraints:

- Processing throughput is limited by available CPU cores and WebWorker parallelism
- Memory is constrained by browser limits
- Storage is constrained by IndexedDB quotas
- Network requests are subject to browser connection limits and CORS policies

### Performance-Driven Restrictions

Certain operations are restricted to maintain responsiveness:

- Playback is disabled at coarse zoom levels to avoid overwhelming data acquisition and processing
- Real-time streaming constrains the historical window to prevent unbounded buffering
- Certain playback speeds may be disallowed in specific modes

### Intentional Limitations

The following behaviors are intentionally not supported:

- **Server-side processing**: All computation occurs client-side; there is no server to offload work to
- **Proprietary data sources**: Only publicly available NEXRAD data sources are supported
- **Offline-first operation**: Network access is assumed for data acquisition; the application caches data but does not function as a fully offline tool
- **Multi-site compositing**: Initial implementation focuses on single-site visualization; multi-site mosaics with simultaneous streaming are a planned future capability
- **Derived products**: The workbench displays base radar moments; derived products (storm tracking, precipitation estimates) are out of scope

## 9. Application Configuration

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

The application provides keyboard shortcuts for power users. The specific shortcut model is to be determined, but shortcuts should cover at a minimum: playback controls (play/pause, step, speed adjustment), timeline mode switching, sidebar toggling, product/elevation cycling, and site switching. Shortcuts should be discoverable through a help overlay or cheat sheet.
