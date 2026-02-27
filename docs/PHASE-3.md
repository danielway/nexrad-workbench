# Phase 3 — Acquisition Depth

Adds full acquisition transparency and rich real-time feedback to the single-site viewer. The simple progress indicator from Phase 1 is replaced by a detailed acquisition queue, and real-time streaming gains predictive visualization.

## Acquisition Queue

Data acquisition is managed via an explicit queue visible in the bottom dock's expandable acquisition drawer. When a time range is selected or the playback position targets uncached data, the required scans are enumerated and enqueued for download.

The drawer shows individual requests with their status, progress, and controls. Users can pause, cancel, or reprioritize queued items. If the user changes their selection, in-progress downloads may be canceled or deprioritized.

### Network Activity Detail

For streaming, distinct phases are visible: acquisition/polling phase and chunk download phase. Expected delays between chunks are apparent, and retry attempts are observable.

For archive downloads, queued, active, and completed downloads are individually enumerable. Each download shows its target (site, scan, record), status, progress, and timing.

### Latency Metrics

For each chunk, the system surfaces latency measurements: time since the first and last radial in the chunk were collected, and latency between chunk availability in S3 and download completion. These metrics provide insight into radar collection delay, distribution latency, and client-side acquisition performance.

### Error Handling

When a download or streaming request fails, the error is displayed in the acquisition drawer with diagnostic information available on click or hover. A failure pauses the entire acquisition queue — both archive downloads and active streaming — to prevent cascading failures and give the user a clear moment to assess the situation. The user can retry the failed request, skip it, or resume the queue to continue with remaining items.

## Real-Time Locked Visualization

When streaming real-time data with playback locked to real time, the viewer extends the Phase 1 behavior with predictive visual cues:

- Renders a shaded future region indicating data currently being collected or expected in the next chunk
- Displays an overlay showing estimated time until the next chunk
- Allows the sweep line to continue beyond received data into the shaded region

These cues communicate the expected cadence of incoming data and help distinguish "waiting for data" from "no data available."

When streaming but not locked to real time, visualization behavior remains identical to archive playback.
