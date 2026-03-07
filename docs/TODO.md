# TODO

## Timeline Improvements

### Usability & Clarity

- Visually separate matching vs non-matching sweeps based on user filters at sufficient
  zoom, while preserving other sweep indications
- Include products in sweep indicators on the timeline
- Tone down the green background color when zoomed to individual sweeps
- Show overview VCP information on the timeline (e.g. clear-air vs storm mode
  transitions via color coding, a separate track, or another indicator)
- When a range is selected, display the boundaries and length of the selection
- Can we visually show the full scan container despite missing many sweeps?
- Can we improve how sweeps are shown out of the scan? Hard to distinguish currently, unclear.
- Can we estimate scan completion time, possibly based on VCP?
- Remove inaccurate timeline status info ("Reflectivity 0.5deg")
- Add a persistent "now" marker in the timeline, separate from the playback marker

### Debugging & Data Accuracy

- Why are there gaps between sweeps and scans?
- Show start/end of distinct segments
- Show times for start/end
  - If within X, also show age live
- Always stamp presented data clearly with collection time range
  - If particularly old but confusable with live, stamp loudly
- Show pipeline status (active phases, expected load-in points on timeline, ETA)

### Playback Behavior

- Jogging should be by frame or based on zoom level, not playback speed
- Hide azimuth indicator when playing back above 30s/s
- Investigate azimuth line speed variation — is it animated incorrectly based on
  resolution, or does the instrument actually change rotation speed? Should animate
  based on actual radial data's azimuths by-radial.
- Explore two distinct timeline modes:
  1. Low zoom: playback is instantaneous time (current behavior), showing imagery as
     collected in real-time, which may stutter depending on VCP.
  2. High zoom: matching frames shown as uniform equidistant blocks, sequencing through
     frames regardless of relative collection time. Less accurate for scan pattern
     understanding but more convenient for simple playback.

## Rendering

- Confirm we're rendering at the maximum available resolution in the data
- Stop rendering when we can't find a sweep for the current filter params
- Drop the 0.5 pre-render; instead use a double buffer or just prefetch without rendering

## Cell Detection & Analysis

- Improve quality of cell detection (currently too simplistic)
- Optimize cell detection performance
- Distance tool that locks cells or position with distance and storm-relative timing
- Iterate on non-thunderstorm experience

## Data Pipeline & Performance

- Parallelize download -> decompress -> store pipeline (currently serial?)
  - This could be moved to per-record, even though we'll download scans as a whole, so that data starts to flow-in before the whole record is ready
- Is our pipeline coupled to the access pattern, or does it generically turn "compressed record" into "sweeps" that are cached?
- Can the pipeline operate at a record level rather than a scan level? e.g. enqueuing and pushing individual records' data
- Can the pipeline take things end-to-end rather than batching all DL, all decompress, etc?
- Can the pipeline be multi-threaded further? Perhaps a worker per record end-to-end.

### Streaming

- Update streaming API to allow fetching all the intermediate chunks from the partial volume up to now
- Can we begin processing as chunks are streamed rather than waiting?

### Partial Scan Edge Cases

- When we load partial scan data, have we considered all edge cases?
  - What gets stored in cache if we start mid-scan?
  - Can I restart streaming twice in the same scan?
  - Can I "fill in the gaps" with either an archive or reconstructing the volume from the real-time store?

### Storage

- Reset the IDB version; no backwards compatibility

## Interaction & Downloads

- Clicking download with a non-range selection doesn't trigger a download as expected

## Map & Visuals

- Can we add major streets, cities, etc at a low zoom level?
- We should have iconography brought in

## Data Products

- Confirm we don't have any assumptions of the sweep starting at 0 deg
- Adding level 3 products, and maybe my own derived products too

## MORE TODOS

- Separating live streaming from scrubbing around?

- Clean up timeline interactions (jumping is screwy)

- We should clear the render if there's no data
  - No data might include "really old data that is not relevant or visible in the timeline"

- How do we prevent double-downloading data?
  e.g. real-time partial scan, then archive full scan
