# Backlog

Outstanding bugs, improvements, and feature ideas. For a description of the product as it exists today, see [PRODUCT.md](PRODUCT.md).

## Bugs

- Timeline interactions (jumping/scrubbing) are erratic in some cases
- Sweep line jerks back on chunk receive during real-time streaming
- Sweep/chunks from real-time not displaying at closer zoom levels
- Scan VCP info and sweep animation not always shown during real-time streaming (more reliable when streaming across scan boundaries)
- Real-time VCP not appearing correctly (panel showing fewer elevations than sweeps received)
- Real-time scan estimated lengths are short — review calculation
- Streamed scan's length/end changes when streaming ends
- Scan line pattern is non-parallel at edges — investigate
- Timeline scan ghost sizes sometimes shorter than expected

## Timeline

- Show start/end boundaries of distinct segments with timestamps (with live age for recent data)
- Explore two playback modes:
  1. **Real-time**: playback matches collection time (current behavior); may stutter depending on VCP
  2. **Uniform**: frames shown as equidistant blocks regardless of collection time; simpler playback

## Rendering

- Replace 0.5 pre-render with double buffering or prefetch-only
- **Progressive sweep animation**: Two additional accumulation strategies beyond the current complete-sweeps model:
  - **Continuous (wiper)**: At each azimuth/range, the most recent eligible value is shown. As the radar sweeps, new data progressively overwrites older data. The canvas always shows maximum spatial coverage. Lookback is bounded to prevent rendering arbitrarily old data.
  - **Sweep-isolated**: When a new sweep begins, the canvas clears entirely. Data paints in fresh from the starting azimuth. Only data from the current sweep is ever visible — a growing wedge until the sweep completes, then a full circle until the next matching sweep clears it. Guarantees temporal purity.
- **Data age visualization**:
  - **Sweep boundary lines**: Thin radial lines on the canvas at azimuths where data from different sweeps meets, making temporal structure visible.
  - **Age labels at sweep boundaries**: At each boundary line, annotate the age of data on each side (e.g. "3m12s").
  - **Age attenuation**: Configurable visual effect where older data is progressively dimmed or desaturated relative to the newest data. Optional, off by default.

## Data Pipeline

- Parallelize download → decompress → store pipeline (currently serial within the single WASM thread)
- Take pipeline end-to-end per record rather than batching all downloads, then all decompress, etc.
- Consider a worker per record for further parallelism (if browser supports multiple workers)

## Real-Time Streaming

- Allow fetching all intermediate chunks from the partial volume up to now
- Begin processing as chunks stream in rather than waiting for completion
- Add fallback to restart streaming after X consecutive chunk failures
- **Predictive visualization**: Render a shaded future region indicating data currently being collected or expected in the next chunk. Allow the sweep line to continue beyond received data into the shaded region. (Live mode timing is tracked but not yet rendered on the canvas.)

## Data Acquisition

- **Acquisition queue**: An expandable acquisition drawer in the bottom dock showing individual requests with status, progress, and controls. Users can pause, cancel, or reprioritize queued items. If the user changes their selection, in-progress downloads may be canceled or deprioritized.
- **Network activity detail**: For streaming, distinct phases visible (acquisition/polling vs. chunk download). For archive downloads, queued/active/completed downloads individually enumerable with target, status, progress, and timing.
- **Per-chunk latency metrics**: Surface time since first/last radial in each chunk, and latency between chunk availability in S3 and download completion.
- **Error-pause behavior**: When a download or streaming request fails, pause the entire acquisition queue to prevent cascading failures. User can retry, skip, or resume.

## Data Products

- Confirm no assumptions that sweeps start at 0°
- Add Level 3 products and custom derived products

## Initial Load Experience

- Prompt for location (site, zip code, or device location)
- Download current volume data without starting real-time streaming
- Add short timeout to acquisition to avoid indefinite search
- Fix known bug in acquisition logic; add invariants/safeguards

## Cell Detection & Analysis

- Improve cell detection quality (currently too simplistic)
- Optimize cell detection performance
- Distance tool that locks to cells or positions with distance and storm-relative timing

## Local File Upload

Upload local Archive II files for viewing without requiring network access. Supports offline analysis and examination of data from non-standard sources.

## Multi-Site Support

Extends the application from a single-site viewer to support multiple simultaneous radar sites.

- **Multi-site selection**: Site selection modal with checkboxes for multi-selection. Number of simultaneous sites limited (initially ~3) to manage resource consumption. All active sites listed in the top bar.
- **Overlapping rendering**: Multiple sites render as overlapping polar projections on a single shared canvas. A mosaic algorithm governs compositing of overlapping regions. Each site's data rendered independently according to active accumulation strategy and product/elevation selection.
- **Stacked timeline tracks**: Each active site receives its own timeline track, stacked vertically in the bottom dock. All tracks share the same temporal axis (synchronized pan/zoom) and a single playback position line spans all tracks. Each track independently displays data availability, VCP color-coding, scan/sweep decomposition, and acquisition progress.
- **Shared playback semantics**: A single playback position governs all sites. Per-track data availability makes gaps clear without special handling.
