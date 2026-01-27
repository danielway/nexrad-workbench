# NEXRAD Workbench — Product Definition

## 1. Vision and Principles

NEXRAD Workbench is a browser-based technical workbench for viewing and analyzing NEXRAD radar data. It operates entirely client-side with no backend services; all data is fetched and processed in the browser.

The product prioritizes transparency, inspectability, and correctness over abstraction. Users should be able to see exactly what the radar data contains and how it maps to the rendered visualization. Performance and responsiveness are first-order concerns.

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

## 3. Visualization and Interaction

### Map Canvas

The primary view is a map canvas displaying radar data overlaid on geographic context. Radar data is rendered in polar coordinates centered on the radar site and projected onto the map. The canvas supports standard map interactions: pan, zoom, and rotation.

### Sweep Playback and Animation

During playback, the viewer animates the radar sweep second-by-second, rendering data spatially as if the radar were operating in real time. The visual sweep animation is synchronized with the sweep's azimuthal progression and the temporal playback position.

### Radar Operations Panel

The application provides a dedicated radar operations panel with multiple coordinated views:

- **Azimuth view**: A top-down view displaying a rotating sweep line and an icon at the radar location.
- **Elevation view**: A side-profile view displaying the elevation angle of the current sweep.
- **VCP view**: Renders the volume coverage pattern as a structured sequence, displaying sweep elevations and parameters as a "playlist" and highlighting the current sweep's position within the scan.

### Product Selection

A single scan may include multiple radar products (reflectivity, velocity, spectrum width, differential reflectivity, correlation coefficient, and others), collected at different elevations and possibly repeated within a scan. The viewer allows users to control what is rendered:

- Focus on a specific product (e.g. reflectivity only)
- Focus on a specific elevation (e.g. 0.5° tilt)
- Show "most recent" data regardless of product or elevation

These selections directly influence rendering behavior and data freshness semantics.

### Fixed-Tilt Rendering

When the user selects a specific product and elevation, the viewer renders the complete sweep at that tilt. Subsequent radar operation may not immediately collect data at that elevation, so the displayed data may become stale over time. The UI clearly indicates the sweep being shown, its elevation and product, the collection timestamp, and how out-of-date the data is relative to "now".

### Most-Recent Rendering

In "most recent" mode, the viewer may blend data from multiple sweeps and elevations to emphasize temporal immediacy over sweep purity. Rendering strategies include continuously wiping older data as the sweep progresses or clearing the display at the end of a sweep and starting fresh.

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

## 4. Timeline and Playback Behavior

### Timeline Bounds

The timeline represents a continuous time axis with hard bounds. The right bound is `now + ε` (a small fixed future buffer). The left bound is the start of available NEXRAD data collection. User interaction cannot extend beyond these bounds.

### Zoom and Scale

The timeline supports zooming to change temporal scale. Zoom level governs which operations are available:

- When zoomed too far out, playback is disabled to avoid data acquisition and processing bottlenecks.
- When zoomed sufficiently far in, real-time mode becomes available.

Zoom may be locked depending on application mode.

### Real-Time Mode

In real-time mode, the right edge of the timeline is snapped to "now" and continuously held there. The left edge is constrained to a fixed window into the past (e.g. ~1 hour) to prevent unbounded historical buffering. Real-time mode may implicitly lock zoom and timeline bounds.

### Playback Controls

The timeline always has a playback position representing the moment whose data is displayed. Playback supports:

- Pause and resume
- Variable playback speed, ranging from real-time (1:1) to accelerated rates (e.g. 1 second of data per minute of wall-clock time, or faster)

Certain playback speeds may be disallowed in specific modes to avoid acquisition or processing overload.

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

At coarse zoom levels, the timeline does not display individual scans or gaps. Instead, it renders solid filled segments indicating contiguous regions where data exists. The representation answers only: "Is there any data here?" Individual scans and inter-scan gaps are not discerned.

When zoomed out so far that a data region would be visually negligible (e.g. viewing months of time with only one hour of data), the timeline may artificially expand the visual width of that segment. This is a purely visual affordance for discoverability and does not imply actual temporal extent.

Once the user zooms in sufficiently, solid segments decompose into individual scans rendered discretely. At this level, visual indicators may communicate whether a scan is complete (fully downloaded) or partial (constructed from streamed data).

Zooming in further decomposes scans into constituent sweeps. Visual encodings may indicate sweep parameters (e.g. elevation angle) at a glance. For incomplete scans, the timeline shows the expected temporal extent; sweeps that occurred before streaming began appear as gaps, while cached sweeps appear in their correct positions.

### Playback While Streaming

In real-time streaming mode, the user may play back data from a bounded window preceding "now" (e.g. ~1 hour), enabling review of recent data while new chunks continue to arrive. Two orthogonal behaviors coexist:

- **Data acquisition mode**: streaming chunks in real time as they become available
- **Playback lock mode**: optionally locking playback to the latest moment ("now")

While streaming, the timeline prevents playback position and time-range selection from extending beyond the allowed historical window. This constraint applies whether or not playback is locked to "now".

## 5. Data Acquisition and Caching

### Automatic Archive Download

An optional automatic download mode performs proactive acquisition of archive data based on the current playback position or selected time range. When a time range is selected, archive downloads begin immediately for the scans required to fulfill that range. If the user changes the selection, in-progress downloads may be canceled or deprioritized.

### Data Acquisition Queue

Archive downloads are managed via a data acquisition queue. If multiple scans are required (e.g. five scans for a selected range), they are enumerated explicitly. The queue reflects pending, active, and completed downloads. Users can pause downloads, cancel downloads, or modify the queue by adjusting selections.

### Network Activity Visibility

The application provides clear, continuous feedback about network activity for both real-time streaming and archive downloads.

For streaming, distinct phases are visible: acquisition/polling phase and chunk download phase. Expected delays between chunks are apparent, and retry attempts are observable.

For archive downloads, queued, active, and completed downloads are clearly distinguishable.

### Storage Model

Data is persisted in browser storage (IndexedDB) in two logical categories:

**Payload storage** holds the actual radar data as individual records in their native Bzip2-compressed form. This format is space-efficient and preserves original data boundaries. Records are the atomic unit of storage and retrieval, fetched in batches when fulfilling playback time ranges or archive downloads.

**Index storage** is a lightweight index tracking what data exists in storage. The index enables fast timeline construction and efficient planning of batch record loads. It tracks which scans are present, whether each scan is complete or partial, temporal bounds, and data availability within scans.

### Cache Behavior

Downloaded data is cached and reused when the user navigates to previously-viewed time ranges. Cache persists across sessions. When storage limits are reached, older data is evicted according to a least-recently-used policy.

## 6. Inspection and Technical Transparency

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

## 7. Constraints and Intentional Limitations

### Front-End-Only Architecture

The application is a front-end-only system: hosted and distributed as static assets, operating entirely in the browser (with a potential desktop variant), with no proprietary backend services. All data acquisition uses publicly available sources.

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
- **Multi-site compositing**: The current design focuses on single-site visualization; radar mosaics combining multiple sites are not yet supported
- **Derived products**: The workbench displays base radar moments; derived products (storm tracking, precipitation estimates) are out of scope
