# TODO

## Timeline

- Show start/end boundaries of distinct segments with timestamps
  - If recent, also show live age
- Explore two playback modes:
  1. **Real-time**: playback matches collection time (current behavior); may stutter depending on VCP
  2. **Uniform**: frames shown as equidistant blocks regardless of collection time; simpler playback
- Clean up timeline interactions (jumping is erratic)
- Sweep line not matching actual data (jerks back on chunk receive)

## Rendering

- Replace 0.5 pre-render with double buffering or prefetch-only
- Scan line pattern is non-parallel at edges — investigate
- Sweep/chunks from real-time not displaying at closer zoom levels

## Cell Detection & Analysis

- Improve cell detection quality (currently too simplistic)
- Optimize cell detection performance
- Distance tool that locks to cells or positions with distance and storm-relative timing
- Iterate on non-thunderstorm experience

## Data Pipeline & Performance

- Parallelize download → decompress → store pipeline (currently serial)
  - Move to per-record granularity so data flows in before the full scan is ready
- Decouple pipeline from access pattern — generically turn compressed records into cached sweeps
- Operate at record level rather than scan level (enqueue and push individual records)
- Take pipeline end-to-end per record rather than batching all downloads, all decompress, etc.
- Consider a worker per record for further parallelism

## Real-Time Streaming

- Allow fetching all intermediate chunks from the partial volume up to now
- Begin processing as chunks stream in rather than waiting for completion
- Separate live streaming mode from scrub/playback mode
- Surface timing metrics/stats from the streaming iterator
- Scan VCP info and sweep animation not always shown during real-time streaming
  (more reliable when streaming across scan boundaries)
- Real-time VCP not appearing correctly (panel showing fewer elevations than sweeps received)
- Real-time scan estimated lengths are short — review calculation
- Add fallback to restart streaming after X consecutive chunk failures
- Why does a streamed scan's length/end change when streaming ends?

## Data Management

- Ensure new archives are picked up even if a previous listing didn't include them
- Ensure real-time data overwrites existing IDB cache entries
- How is the scan ghost size determined? Sometimes shorter than expected.

## Data Products

- Confirm no assumptions that sweeps start at 0°
- Add Level 3 products and custom derived products

## Initial Load Experience

- Prompt for location (site, zip code, or device location)
- Download current volume data without starting real-time streaming
- Add short timeout to acquisition to avoid indefinite search
  - Fix known bug in acquisition logic; add invariants/safeguards

## Open Questions

- What do the timeline scan lines represent?
- Why don't elevation angles over timeline sweeps match tooltip values?
