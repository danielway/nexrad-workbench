# Phase 4 — Multi-Site

> **Status: Not started.**

Extends the application from a single-site viewer to support multiple simultaneous radar sites. All prior phases operate in a single-site context; this phase adds the selection, rendering, and timeline infrastructure for viewing several sites at once.

## Multi-Site Selection

The site selection modal presents all NEXRAD sites with checkboxes for multi-selection. The number of simultaneously active sites is limited (initially ~3) to manage resource consumption. All active sites are listed in the top bar. Site management (adding, removing, reordering) is handled through the selection modal.

## Overlapping Rendering

Multiple sites render as overlapping polar projections on a single shared canvas. A mosaic algorithm governs how overlapping regions are composited. Each site's data is rendered independently according to the active accumulation strategy and product/elevation selection; the mosaic determines which site's data is displayed where coverage overlaps.

## Stacked Timeline Tracks

Each active site receives its own timeline track, stacked vertically within the bottom dock. All tracks share the same temporal axis — pan and zoom are synchronized — and a single playback position line spans all tracks.

Each track independently displays:

- Data availability segments for that site
- VCP color-coding and transition markers for that site
- Scan and sweep decomposition at close zoom levels
- Acquisition progress (cached, downloading, or missing scans)

## Shared Playback Semantics

A single playback position governs all sites. All sites render data at the same moment. Some sites may have data at the playback position while others do not; per-track data availability makes this clear without requiring any special handling.
