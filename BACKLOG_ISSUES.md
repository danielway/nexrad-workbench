# Backlog Issues

Issues to create in GitHub. Delete this file after issues are created.

---

## Bug: Timeline interaction and display issues

Several bugs related to timeline interactions and visual display.

- Timeline interactions (jumping/scrubbing) are erratic in some cases
- Timeline scan ghost sizes sometimes shorter than expected

Timeline interactions should be smooth and predictable. Scan ghosts should accurately reflect scan duration.

---

## Bug: Real-time streaming display issues

Several bugs related to rendering and state display during real-time streaming.

- Sweep line jerks back on chunk receive during real-time streaming
- Sweep/chunks from real-time not displaying at closer zoom levels
- Scan VCP info and sweep animation not always shown during real-time streaming (more reliable when streaming across scan boundaries)
- Real-time VCP not appearing correctly (panel showing fewer elevations than sweeps received)
- Real-time scan estimated lengths are short — review calculation
- Streamed scan's length/end changes when streaming ends
- Scan line pattern is non-parallel at edges — investigate

---

## Progressive sweep animation and data age visualization

Add two new accumulation strategies beyond the current complete-sweeps model, along with visual indicators for data age. Also replace the current 0.5 pre-render with double buffering or prefetch-only.

### Progressive Sweep Animation

Two additional render modes:

**Continuous (wiper)**: At each azimuth/range, the most recent eligible value is shown. As the radar sweeps, new data progressively overwrites older data. The canvas always shows maximum spatial coverage. Lookback is bounded to prevent rendering arbitrarily old data.

**Sweep-isolated**: When a new sweep begins, the canvas clears entirely. Data paints in fresh from the starting azimuth. Only data from the current sweep is ever visible — a growing wedge until the sweep completes, then a full circle until the next matching sweep clears it. Guarantees temporal purity.

### Data Age Visualization

- **Sweep boundary lines**: Thin radial lines on the canvas at azimuths where data from different sweeps meets, making temporal structure visible
- **Age labels at sweep boundaries**: At each boundary line, annotate the age of data on each side (e.g. "3m12s")
- **Age attenuation**: Configurable visual effect where older data is progressively dimmed or desaturated relative to the newest data. Optional, off by default

### Rendering Improvement

- Replace 0.5 pre-render with double buffering or prefetch-only

---

## Acquisition queue and network activity UI

Add a full acquisition queue UI and detailed network activity display to the bottom dock.

### Acquisition Queue

An expandable acquisition drawer in the bottom dock showing individual requests with status, progress, and controls. Users can pause, cancel, or reprioritize queued items. If the user changes their selection, in-progress downloads may be canceled or deprioritized.

### Network Activity Detail

- For streaming: distinct phases visible (acquisition/polling vs. chunk download)
- For archive downloads: queued/active/completed downloads individually enumerable with target, status, progress, and timing

### Per-Chunk Latency Metrics

Surface time since first/last radial in each chunk, and latency between chunk availability in S3 and download completion.

### Error-Pause Behavior

When a download or streaming request fails, pause the entire acquisition queue to prevent cascading failures. User can retry, skip, or resume.

---

## Real-time streaming enhancements

Improvements to real-time streaming beyond the current core implementation.

- **Intermediate chunk fetching**: Allow fetching all intermediate chunks from the partial volume up to now (not just streaming forward from the current point)
- **Stream-as-you-go processing**: Begin processing chunks as they stream in rather than waiting for completion
- **Fallback restart**: Add fallback to restart streaming after X consecutive chunk failures
- **Predictive visualization**: Render a shaded future region on the canvas indicating data currently being collected or expected in the next chunk. Allow the sweep line to continue beyond received data into the shaded region. (Live mode timing is already tracked but not yet rendered on the canvas.)

---

## Data pipeline parallelization

The current data pipeline (download → decompress → store) is serial within the single WASM thread. This limits throughput, especially for batch downloads.

- Parallelize the download → decompress → store pipeline so stages can overlap
- Process end-to-end per record rather than batching all downloads, then all decompress, etc.
- Consider a worker per record for further parallelism (if browser supports multiple workers)

---

## Local file upload support

Support uploading local Archive II files for viewing without requiring network access. This enables offline analysis and examination of data from non-standard sources.

- File picker or drag-and-drop for Archive II volume files
- Parse and render uploaded files using the same pipeline as downloaded data
- Uploaded data should appear on the timeline and be navigable like any other scan

---

## Multi-site support

Extend the application from a single-site viewer to support multiple simultaneous radar sites.

### Multi-Site Selection

Site selection modal with checkboxes for multi-selection. Number of simultaneous sites limited (initially ~3) to manage resource consumption. All active sites listed in the top bar.

### Overlapping Rendering

Multiple sites render as overlapping polar projections on a single shared canvas. A mosaic algorithm governs compositing of overlapping regions. Each site's data is rendered independently according to the active accumulation strategy and product/elevation selection.

### Stacked Timeline Tracks

Each active site receives its own timeline track, stacked vertically in the bottom dock. All tracks share the same temporal axis (synchronized pan/zoom) and a single playback position line spans all tracks. Each track independently displays data availability, VCP color-coding, scan/sweep decomposition, and acquisition progress.

### Shared Playback Semantics

A single playback position governs all sites. Per-track data availability makes gaps clear without special handling.

---

## Timeline enhancements

Improvements to the timeline display and playback behavior.

### Segment Boundaries

Show start/end boundaries of distinct data segments with timestamps. For recent data, show live age (e.g. "2m ago") that updates in real time.

### Uniform Playback Mode

Explore a second playback mode alongside the current real-time mode:

- **Real-time** (current): playback matches collection time; may stutter depending on VCP
- **Uniform**: frames shown as equidistant blocks regardless of collection time; simpler, smoother playback

---

## Cell detection and analysis improvements

The current storm cell detection is functional but basic. Several improvements would make it more useful.

- Improve cell detection quality (currently too simplistic for real analysis)
- Optimize cell detection performance
- Distance tool that locks to detected cells or specific positions, showing distance and storm-relative timing

---

## Initial load experience improvements

Improvements to the first-load experience after site selection.

- After site selection, download current volume data without automatically starting real-time streaming
- Add short timeout to acquisition to avoid indefinite search when data is unavailable
- Fix known bug in acquisition logic; add invariants/safeguards

---

## Additional data products

Expand supported data products beyond the current base moments.

- Confirm no assumptions that sweeps start at 0° (some products/VCPs may not)
- Add Level 3 products
- Support custom derived products
