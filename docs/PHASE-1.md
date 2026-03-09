# Phase 1 — Core Single-Site Viewer

> **Status: Largely complete.** All major features are implemented except local file upload. See notes below for per-feature status.

The baseline product. A single radar site can be selected, data navigated in time, radar imagery viewed, and real-time data streamed — all with the simplest viable rendering model.

## First-Run Experience

On first launch with no prior state, the site selection modal opens automatically. Once the user selects a site, the application enters real-time mode and acquires the most recent complete sweep for immediate display. This may require crawling through recent real-time chunks until a complete sweep is assembled, since individual chunks may not contain a full sweep.

## Map Canvas

The primary view is a map canvas displaying radar data overlaid on geographic context. Radar data is rendered in polar coordinates centered on the radar site and projected onto the map. The canvas supports standard map interactions: pan and zoom.

The geographic base layer includes state boundaries and labels at all zoom levels. County boundaries and labels appear when zoomed sufficiently close. These geographic layers are optional and can be toggled by the user. The application supports dark and light map themes, matching the active appearance mode.

## Radar Operations Panel

The left sidebar hosts the radar operations panel, providing multiple coordinated views driven by the current playback position:

- **Azimuth view**: A top-down view displaying a rotating sweep line and an icon at the radar location.
- **Elevation view**: A side-profile view displaying the elevation angle of the current sweep.
- **VCP view**: Renders the volume coverage pattern as a structured sequence, displaying sweep elevations and parameters as a "playlist" and highlighting the current sweep's position within the scan.

## Product Selection

Product selection controls are located in the right sidebar. A single scan may include multiple radar products (reflectivity, velocity, spectrum width, differential reflectivity, correlation coefficient, and others), collected at different elevations and possibly repeated within a scan. The viewer allows users to control what is rendered:

- Focus on a specific product (e.g. reflectivity only)
- Focus on a specific elevation (e.g. 0.5° tilt)
- Show "most recent" data regardless of product or elevation

These selections directly influence rendering behavior and data freshness semantics. Each product has a default color table informed by NWS standards. Color tables are user-configurable.

## Rendering Model

### Core Invariants

1. **The playback position is a hard temporal boundary.** Only data collected at or before the playback position is eligible for rendering. The canvas never displays data from the future relative to the playback position, even if that data exists in the cache.
2. **Product and elevation selection is a hard eligibility filter.** When the user selects a specific product and elevation (e.g. 0.5° REF), only radials matching both criteria are eligible. When no filter is applied ("most recent" mode), all products and elevations are eligible.
3. **At each spatial position, the most recent eligible value is rendered.** Each point on the canvas corresponds to a specific azimuth and range. The rendered value is the most recent eligible gate value at or before the playback position.

### Accumulation Strategy

Phase 1 supports a single accumulation strategy: **complete sweeps**. The canvas displays only fully completed sweeps. There is no progressive sweep animation; the display updates discretely when a sweep's collection end time falls behind the playback position. At that moment, the most recent complete sweep matching the active filter is rendered as a full 360°.

At high playback speeds where the playback position advances past multiple sweeps between rendered frames, the renderer shows the most recent complete eligible sweep rather than flashing through every intermediate sweep.

### Data Age Visualization

**Periodic timestamp markers**: Placed at fixed angular intervals (e.g. every 90°) around the render, these label the absolute timestamp or relative age of the data at that azimuth. This ensures the user always has a time reference nearby regardless of where they are looking, even within a single sweep.

## Timeline and Playback

### Timeline Bounds

The timeline represents a continuous time axis with hard bounds. The right bound is `now + ε` (a small fixed future buffer). The left bound is the start of available NEXRAD data collection. User interaction cannot extend beyond these bounds.

### Zoom and Scale

The timeline supports zooming to change temporal scale. Zoom level governs which operations are available:

- When zoomed too far out, playback is disabled to avoid data acquisition and processing bottlenecks.
- When zoomed sufficiently far in, real-time mode becomes available.

Zoom may be locked depending on application mode.

### Data Availability Visualization

The timeline's visual representation changes with zoom level, progressively revealing more structural detail. At all zoom levels, data availability segments are color-coded by Volume Coverage Pattern, making VCP transitions visible even at the broadest scales.

At coarse zoom levels, the timeline renders solid filled segments indicating contiguous regions where data exists, answering only: "Is there any data here?" VCP color-coding is the primary structural information — the user can see at a glance when a site transitioned between clear-air and precipitation modes. Segments that would be visually negligible at the current scale (e.g. one hour of data within a months-wide view) may be artificially expanded as a discoverability affordance.

At closer zoom, segments decompose into individual scans. Visual indicators communicate completeness (fully downloaded vs. partial), VCP identity, and VCP transition boundaries. The system tracks which scans are cached, whether each is complete or partial, and whether the VCP header record is present.

At sweep-level zoom, scans decompose into constituent sweeps reflecting the active VCP structure. Visual encodings indicate sweep parameters (elevation angle) at a glance. For incomplete scans, the timeline shows expected temporal extent with gaps for missing sweeps.

### Timeline Modes

The timeline operates in distinct interaction modes that determine how user gestures are interpreted and what data acquisition behavior results:

- **Navigate mode** (default): Click sets the playback position. Drag scrubs through time. Data for the targeted moment is acquired on demand if not already cached. When a time range is selected (shift-click-drag or shift-click to set range endpoints), the range becomes the playback loop boundary and archive downloads begin for all scans within it. Playback within a range loops or ping-pongs depending on configuration.
- **Real-time mode**: The timeline locks to "now" and continuously advances. The right edge is snapped to wall-clock time; the left edge is constrained to a fixed historical window (e.g. ~1 hour). The application streams incoming chunks as the radar produces them. Zoom and timeline bounds may be implicitly locked. The user may scrub backward within the historical window while streaming continues in the background — data acquisition and playback position are independent.

Modes are indicated in the timeline dock's status bar and can be switched via controls or keyboard shortcuts. Enabling streaming implicitly enters real-time mode.

Regardless of mode, when streaming begins mid-volume, the system always fetches record 1 (the header) of the current archive, which contains the VCP and other metadata required to interpret subsequent records.

### Playback Controls

The timeline always has a playback position representing the moment whose data is displayed. Playback supports:

- Pause and resume
- Variable playback speed, from real-time (1:1) to accelerated rates
- At speeds above near-real-time, the complete sweeps accumulation strategy is required

## Data Acquisition

When the playback position targets uncached data, the required scans are fetched on demand. When a time range is selected, scans within the range are enumerated and downloaded.

Acquisition feedback is minimal: a progress indicator showing active and completed fetches, and a count of pending downloads. When a request fails, an error notification appears with the option to retry or dismiss. Failures do not block other pending downloads.

## Real-Time Streaming

When streaming real-time data with playback locked to real time, the viewer renders all data received so far for the active sweep. There is no predictive visualization — the canvas shows what has been received and nothing more. When streaming but not locked to real time, visualization behavior is identical to archive playback.

## Caching

### Storage Model

Data is persisted in browser storage (IndexedDB) in two logical categories:

**Payload storage** holds the actual radar data as individual records in their native Bzip2-compressed form. This format is space-efficient and preserves original data boundaries. Records are the atomic unit of storage and retrieval, fetched in batches when fulfilling playback time ranges or archive downloads.

**Index storage** is a lightweight index tracking what data exists in storage. The index enables fast timeline construction and efficient planning of batch record loads. It tracks which scans are present, whether each scan is complete or partial, temporal bounds, and data availability within scans. The index must also include metadata about which products and elevations are present in each scan, enabling efficient lookback queries when the rendering model needs to find the most recent eligible data matching the active product/elevation filter without decompressing records.

### Cache Behavior

Downloaded data is cached and reused when the user navigates to previously-viewed time ranges. Cache persists across sessions. When storage limits are reached, older data is evicted according to a least-recently-used policy. Users can manually clear the cache via a button in the status bar (with confirmation). Timeline-based cache management is also available: "clear data in selection" and "clear everything except selection" allow targeted cleanup based on the current time range selection.

## Local File Upload

> **Status: Not yet implemented.**

Users can upload local Archive II files for viewing without requiring network access. This supports offline analysis and examination of data from non-standard sources.
