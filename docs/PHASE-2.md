# Phase 2 — Rich Rendering

Adds progressive sweep animation and full data age visualization to the single-site viewer. The rendering model gains two additional accumulation strategies and the visual tools needed to communicate temporal structure when data from multiple sweeps coexists on the canvas.

## Accumulation Strategies

Phase 1 provides only complete sweeps (discrete full-circle updates). Phase 2 adds two strategies that progressively paint radials as the playback position advances, synchronized with the sweep's azimuthal progression.

### Continuous (Wiper)

The default strategy once available. At each azimuth/range, the most recent eligible value is shown. As the radar sweeps, new data progressively overwrites older data at each azimuth. The canvas always shows maximum spatial coverage — a full 360° once initially populated. Data from different times coexists on the canvas simultaneously.

Lookback is bounded to a reasonable horizon to prevent rendering arbitrarily old data without clear indication of its age.

### Sweep-Isolated

When a new sweep begins for the active product/elevation filter, the canvas clears entirely. Data paints in fresh from the sweep's starting azimuth. Only data from the current sweep is ever visible — a growing wedge until the sweep completes, then a full circle until the next matching sweep clears it.

This guarantees temporal purity: all visible data comes from a single sweep. This strategy also inherently prevents mixing data from different elevations.

## Data Age Visualization

Phase 1 provides periodic timestamp markers. Phase 2 adds three mechanisms that make the temporal structure of rendered data visible when data from multiple sweeps coexists on the canvas (primarily relevant to continuous/wiper mode).

### Sweep Boundary Lines

Thin radial lines on the canvas at azimuths where data from different sweeps meets. These make the temporal structure of the rendered data visible — where one sweep's data ends and another's begins. Always present when data from multiple sweeps is on the canvas.

### Age Labels at Sweep Boundaries

At each sweep boundary line, the age of the data on each side is annotated (e.g. "3m12s"). These provide precise temporal context at the transitions between sweeps.

### Age Attenuation

A configurable visual effect where older data is progressively dimmed, desaturated, or otherwise attenuated relative to the newest data. When enabled, this provides an at-a-glance sense of data freshness across the entire canvas without reading labels. Configurable in the rendering parameters sidebar. Optional — off by default.
