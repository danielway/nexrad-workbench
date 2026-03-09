# Backlog

Active bugs, improvements, and feature ideas. For the phased product roadmap, see [PRODUCT.md](PRODUCT.md) and the individual phase documents.

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

## Data Pipeline

- Parallelize download → decompress → store pipeline (currently serial within the single WASM thread)
- Take pipeline end-to-end per record rather than batching all downloads, then all decompress, etc.
- Consider a worker per record for further parallelism (if browser supports multiple workers)

## Real-Time Streaming

- Allow fetching all intermediate chunks from the partial volume up to now
- Begin processing as chunks stream in rather than waiting for completion
- Add fallback to restart streaming after X consecutive chunk failures

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

## Not Yet Started (from Phase Specs)

These are tracked in the phase documents but called out here for visibility:

- **Local file upload** (Phase 1) — Upload local Archive II files for offline analysis
- **Progressive sweep animation** (Phase 2) — Wiper and sweep-isolated accumulation modes
- **Data age visualization** (Phase 2) — Sweep boundary lines, age labels, age attenuation
- **Acquisition queue** (Phase 3) — Expandable queue with pause/cancel/reprioritize
- **Multi-site support** (Phase 4) — Multiple simultaneous sites with mosaic compositing
