//! What the storm cell overlay shows and how the user talks to it.
//!
//! Detection (when it runs, which result is current) is
//! [`crate::core::storm_cells`]; this module is the presentation half:
//!
//! - **Intents.** The Layers checkbox, the envelope slider, the desktop
//!   pointer and the phone tap arrive as [`StormCellIntent`]s and are reduced
//!   by [`reduce`]. Nothing under `src/ui/` writes the storm cell state.
//! - **Which cells draw.** 2D only. In Rays the canvas shows the previous
//!   sweep ahead of the sweep line and the current one behind it, and the
//!   cells belong to the current sweep, so a cell draws only once its whole
//!   footprint is behind the line ([`footprint_behind_sweep_line`]).
//! - **Grammar.** Every envelope draws as one faint, thin solid ring: a
//!   group's around its members, a lone cell's around itself. Inside it
//!   each cell draws its core (`StormCell::core_outline`, [`drawn_outline`])
//!   at full stroke, so the strongest part of each storm stands out and no
//!   borders run between the footprints that tile a group's envelope. (A
//!   cell without a core ring, which detection should not produce, draws
//!   its footprint at full stroke and no faint ring.) Cell rings are one
//!   light stroke over a dark halo, intensity left out of the color. The
//!   peak dBZ sits in a chip at the drawn outline's top-right; chips
//!   are placed strongest first and skipped when they would crowd one
//!   already placed or when the outline is too small on screen to carry one.
//!   Cells from a live sweep that is still arriving are provisional: drawn
//!   muted, with a hollow chip.
//! - **Hit test.** Point in polygon on the footprint rings in lat/lon (the
//!   whole share of the envelope, not just the drawn core, so a tap anywhere
//!   in a group lands on a cell); when two footprints contain the point the
//!   strongest cell wins ([`cell_at`]). It
//!   drives the desktop hover card, the phone's tap-to-select card and the
//!   data probe's cell line.
//!
//! - **Tracks** (the "Tracks" checkbox, [`crate::core::storm_tracks`]): the
//!   chip leads with the track's letter and carries a trend mark when the
//!   peak moved more than [`TREND_MARK_DB`] over the last three volumes; the
//!   storm's path (every centroid the store holds, up to 24) draws as small
//!   dots on a thin line that fades with age ([`path_fade`]), a track that
//!   split off another joins the parent's centroid with a short dashed line,
//!   and a dashed arrow points to where the cell will be in 30 minutes once
//!   its motion is confident. Everything is anchored on centroids, never on
//!   the outline.
//!   The card adds motion, age, trend and, with "My location" on, where the
//!   cell is from you and its closest approach. A coasting track (no cell
//!   this volume) draws nothing.
//!
//! Screen math takes the projection as a closure (`lat, lon -> [x, y]`) so
//! the module stays free of egui; text widths assume the monospace chip
//! font's 0.6 em advance (IBM Plex Mono).

use crate::core::canvas::{geo_to_polar, polar_in_prev_region};
use crate::core::storm_cells::clamp_threshold;
use crate::core::storm_tracks::{TrackVm, TracksView};
use crate::core::{StormCell, StormCellsResult};
use crate::geo::ViewMode;

// ───────────────────────────────────────────────────────────────────────────
// Intents
// ───────────────────────────────────────────────────────────────────────────

/// What the user did to the storm cell overlay.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum StormCellIntent {
    /// The Layers checkbox. Carries the new value so a repeat is idempotent.
    SetVisible(bool),
    /// The envelope slider, in dBZ (clamped to the slider's range).
    SetThreshold(f32),
    /// The "Tracks" checkbox under it: trails, forecast arrows and track
    /// letters.
    SetTracks(bool),
    /// The desktop pointer over the 2D map, as `(lat, lon)`, or `None` when
    /// it leaves.
    Hover(Option<(f64, f64)>),
    /// A phone tap: `Some((lat, lon))` selects the cell under it, `None`
    /// dismisses the card.
    Select(Option<(f64, f64)>),
}

/// Where the pointer and the phone's selection are, in `(lat, lon)`. The
/// cell under each is resolved per frame by [`cell_at`], so a point that no
/// longer falls in a cell (a new volume, a new threshold) shows no card.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct StormCellPointer {
    pub hover: Option<(f64, f64)>,
    pub selected: Option<(f64, f64)>,
}

/// The state [`reduce`] writes.
pub(crate) struct StormCellControls<'a> {
    /// `viz_state.storm_cells_visible`.
    pub visible: &'a mut bool,
    /// `viz_state.storm_cell_threshold_dbz`.
    pub threshold_dbz: &'a mut f32,
    /// `viz_state.storm_tracks_visible`.
    pub tracks: &'a mut bool,
    /// `viz_state.storm_cell_pointer`.
    pub pointer: &'a mut StormCellPointer,
}

/// Apply one intent. Turning the overlay off drops the hover and the
/// selection; a point that is not finite is ignored.
pub(crate) fn reduce(c: StormCellControls<'_>, intent: StormCellIntent) {
    let finite = |p: Option<(f64, f64)>| p.filter(|(a, b)| a.is_finite() && b.is_finite());
    match intent {
        StormCellIntent::SetVisible(on) => {
            *c.visible = on;
            if !on {
                *c.pointer = StormCellPointer::default();
            }
        }
        StormCellIntent::SetThreshold(dbz) => *c.threshold_dbz = clamp_threshold(dbz),
        StormCellIntent::SetTracks(on) => *c.tracks = on,
        StormCellIntent::Hover(p) => c.pointer.hover = finite(p),
        StormCellIntent::Select(p) => c.pointer.selected = finite(p),
    }
}

/// The extra intent a phone tap on the map sends beside its pick: a tap
/// that lands anywhere but on a cell dismisses an open cell card.
pub(crate) fn tap_dismisses_card(
    tapped_cell: bool,
    pointer: &StormCellPointer,
) -> Option<StormCellIntent> {
    (!tapped_cell && pointer.selected.is_some()).then_some(StormCellIntent::Select(None))
}

// ───────────────────────────────────────────────────────────────────────────
// Geometry: hit test and the Rays rule
// ───────────────────────────────────────────────────────────────────────────

/// Even-odd point in polygon on a ring of `(lat, lon)` points. The ring may
/// repeat its first point at the end.
pub(crate) fn point_in_ring(ring: &[(f64, f64)], lat: f64, lon: f64) -> bool {
    let n = ring.len();
    if n < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = n - 1;
    for i in 0..n {
        let (yi, xi) = ring[i];
        let (yj, xj) = ring[j];
        if (yi > lat) != (yj > lat) {
            let x_cross = xi + (lat - yi) / (yj - yi) * (xj - xi);
            if lon < x_cross {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

/// The ring the overlay draws at full stroke for a cell: its core (the
/// faint envelope ring stands for the rest of its footprint), or its
/// footprint when it has no core ring.
pub(crate) fn drawn_outline(cell: &StormCell) -> &[(f64, f64)] {
    if cell.core_outline.len() >= 4 {
        &cell.core_outline
    } else {
        &cell.outline
    }
}

/// The cell whose footprint contains `(lat, lon)`, among the `shown` ids.
/// When footprints nest or overlap, the strongest (highest peak, then
/// largest) wins.
pub(crate) fn cell_at(result: &StormCellsResult, shown: &[u32], lat: f64, lon: f64) -> Option<u32> {
    shown
        .iter()
        .filter_map(|id| result.cells.get(*id as usize))
        .filter(|c| {
            let (min_lat, min_lon, max_lat, max_lon) = c.bounds;
            (min_lat..=max_lat).contains(&lat)
                && (min_lon..=max_lon).contains(&lon)
                && point_in_ring(&c.outline, lat, lon)
        })
        .max_by(|a, b| {
            a.peak_dbz
                .total_cmp(&b.peak_dbz)
                .then(a.area_km2.total_cmp(&b.area_km2))
                .then(b.id.cmp(&a.id))
        })
        .map(|c| c.id)
}

/// Whether a footprint lies wholly behind the Rays sweep line, i.e. on
/// pixels of the sweep the cells were detected on. `sweep` is the canvas's
/// `(sweep azimuth, sweep start)`; `None` means the whole current sweep is
/// on screen. Every ring vertex and the centroid must be in the swept arc.
pub(crate) fn footprint_behind_sweep_line(
    ring: &[(f64, f64)],
    centroid: (f64, f64),
    radar: (f64, f64),
    sweep: Option<(f32, f32)>,
) -> bool {
    if sweep.is_none() {
        return true;
    }
    std::iter::once(&centroid)
        .chain(ring.iter())
        .all(|&(lat, lon)| {
            let (az, _) = geo_to_polar(lat, lon, radar.0, radar.1);
            !polar_in_prev_region(az as f32, sweep)
        })
}

// ───────────────────────────────────────────────────────────────────────────
// View-model
// ───────────────────────────────────────────────────────────────────────────

/// Everything the overlay's view-model is built from.
pub(crate) struct StormCellViewInputs<'a> {
    pub result: &'a StormCellsResult,
    /// The Layers checkbox.
    pub visible: bool,
    pub view_mode: ViewMode,
    /// Radar site `(lat, lon)`.
    pub radar: (f64, f64),
    /// The site id, for the card's "from KDMX".
    pub site_id: &'a str,
    /// The canvas's Rays sweep `(azimuth, start)`, `None` when the whole
    /// current sweep is on screen.
    pub sweep: Option<(f32, f32)>,
    /// The cells come from a live sweep that is still arriving.
    pub provisional: bool,
    /// Phone layout: thicker strokes, larger chips, tap instead of hover.
    pub phone: bool,
    pub pointer: &'a StormCellPointer,
    /// The data probe is on: it names the cell under the pointer itself, so
    /// the desktop hover card stays away.
    pub probe_on: bool,
    /// The tracks on these cells, when "Tracks" is on and the volume is
    /// tracked ([`crate::core::storm_tracks::StormTracks::view`]).
    pub tracks: Option<&'a TracksView>,
}

/// Stroke widths and chip type for the layout, in screen px.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct OverlayStyle {
    /// The light footprint stroke.
    pub outline_px: f32,
    /// The dark halo under it.
    pub halo_px: f32,
    /// The group ring: solid, thinner and fainter than a cell's.
    pub group_px: f32,
    /// The thin halo under the group ring.
    pub group_halo_px: f32,
    /// Chip font size (monospace).
    pub chip_font_px: f32,
    /// Minimum gap between two chips.
    pub chip_spacing_px: f32,
    /// A track's trail line and forecast arrow.
    pub trail_px: f32,
    /// Radius of a trail dot.
    pub trail_dot_px: f32,
    /// Length of the forecast arrow's head.
    pub arrow_head_px: f32,
}

/// The style for the layout: 1.6 px outlines and 11 px chips on the
/// desktop, 2 px and 12 px with wider chip spacing on the phone.
pub(crate) fn overlay_style(phone: bool) -> OverlayStyle {
    if phone {
        OverlayStyle {
            outline_px: 2.0,
            halo_px: 4.5,
            group_px: 1.5,
            group_halo_px: 2.5,
            chip_font_px: 12.0,
            chip_spacing_px: 6.0,
            trail_px: 1.6,
            trail_dot_px: 2.6,
            arrow_head_px: 9.0,
        }
    } else {
        OverlayStyle {
            outline_px: 1.6,
            halo_px: 4.0,
            group_px: 1.0,
            group_halo_px: 2.0,
            chip_font_px: 11.0,
            chip_spacing_px: 2.0,
            trail_px: 1.2,
            trail_dot_px: 2.2,
            arrow_head_px: 7.0,
        }
    }
}

/// An outline whose projected extent is below this draws no chip.
pub(crate) const CHIP_MIN_OUTLINE_PX: f32 = 14.0;
/// Advance of one monospace glyph, in em (IBM Plex Mono: 600/1000).
const MONO_ADVANCE_EM: f32 = 0.6;
/// Horizontal padding inside a chip, each side.
const CHIP_PAD_X: f32 = 4.0;
/// Chip height beyond the font size.
const CHIP_PAD_Y: f32 = 5.0;
/// Gap between the outline's right edge and its chip.
const CHIP_GAP_X: f32 = 3.0;

/// One cell's footprint on screen.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CellOutlineVm {
    pub id: u32,
    /// Closed ring in screen px (first point not repeated).
    pub ring: Vec<[f32; 2]>,
    /// Hovered (desktop) or selected (phone): drawn a little heavier.
    pub highlighted: bool,
}

/// A peak dBZ chip.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ChipVm {
    pub cell_id: u32,
    /// Top-left corner in screen px.
    pub min: [f32; 2],
    pub size: [f32; 2],
    pub text: String,
    /// Provisional cell: outline only, no fill.
    pub hollow: bool,
}

/// A track's path and forecast arrow on screen.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TrailVm {
    pub cell_id: u32,
    /// The storm's path: its centroids (up to 24), oldest first; the last
    /// is the cell's own.
    pub dots: Vec<[f32; 2]>,
    /// Opacity (0..1) of each dot, rising with recency: the newest is 1.
    pub dot_alpha: Vec<f32>,
    /// Opacity of each segment `dots[i] → dots[i + 1]`: its newer end's.
    pub segment_alpha: Vec<f32>,
    /// A track that split off another: a dashed join from the parent's
    /// centroid at the split to the path's first dot.
    pub split_join: Option<([f32; 2], [f32; 2])>,
    /// The forecast arrow, from the cell's centroid to its position in 30
    /// minutes, when the motion is confident.
    pub arrow: Option<ArrowVm>,
    /// The hovered or selected cell's trail.
    pub highlighted: bool,
}

/// A dashed forecast arrow on screen.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ArrowVm {
    pub from: [f32; 2],
    pub to: [f32; 2],
    /// The head: a filled triangle with its tip at `to`.
    pub head: [[f32; 2]; 3],
}

/// Opacity of the oldest point of a path.
pub(crate) const PATH_OLDEST_ALPHA: f32 = 0.2;

/// Opacity of each of `n` path points, oldest first: rising evenly from
/// [`PATH_OLDEST_ALPHA`] to 1 at the newest.
pub(crate) fn path_fade(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| {
            if n <= 1 {
                1.0
            } else {
                PATH_OLDEST_ALPHA + (1.0 - PATH_OLDEST_ALPHA) * i as f32 / (n - 1) as f32
            }
        })
        .collect()
}

/// An arrow from `from` to `to` with a head `head_px` long, or `None` when
/// the two points are under a pixel apart (a storm barely moving at this
/// zoom).
pub(crate) fn arrow(from: [f32; 2], to: [f32; 2], head_px: f32) -> Option<ArrowVm> {
    let (dx, dy) = (to[0] - from[0], to[1] - from[1]);
    let len = dx.hypot(dy);
    if len < 1.0 {
        return None;
    }
    let (ux, uy) = (dx / len, dy / len);
    let head = head_px.min(len);
    let base = [to[0] - ux * head, to[1] - uy * head];
    let half = head * 0.45;
    Some(ArrowVm {
        from,
        to,
        head: [
            to,
            [base[0] - uy * half, base[1] + ux * half],
            [base[0] + uy * half, base[1] - ux * half],
        ],
    })
}

/// One line of the cell card.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CardLine {
    pub text: String,
    /// Secondary (position, provisional note).
    pub dim: bool,
}

/// Where the cell card goes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum CardAnchor {
    /// Desktop: beside this screen point (the pointer's place).
    Pointer([f32; 2]),
    /// Phone: centered above the bottom of the map.
    Bottom,
}

/// The hover card (desktop) or the selected cell's card (phone).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CardVm {
    pub cell_id: u32,
    pub title: String,
    pub lines: Vec<CardLine>,
    pub anchor: CardAnchor,
}

/// What the overlay draws this frame. Empty when off, in 3D, or with no
/// cells.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StormCellsVm {
    pub style: OverlayStyle,
    /// Faint envelope rings, closed rings in screen px: each group's, and
    /// each lone cell's that draws its core.
    pub groups: Vec<Vec<[f32; 2]>>,
    pub cells: Vec<CellOutlineVm>,
    pub chips: Vec<ChipVm>,
    /// Track trails and forecast arrows (Tracks on).
    pub trails: Vec<TrailVm>,
    pub card: Option<CardVm>,
    /// Ids of the cells that draw, for hit tests ([`cell_at`]).
    pub shown: Vec<u32>,
    /// Cells come from a live sweep still arriving: drawn muted.
    pub provisional: bool,
}

impl StormCellsVm {
    fn empty(phone: bool) -> Self {
        Self {
            style: overlay_style(phone),
            groups: Vec::new(),
            cells: Vec::new(),
            chips: Vec::new(),
            trails: Vec::new(),
            card: None,
            shown: Vec::new(),
            provisional: false,
        }
    }

    /// Nothing to draw.
    pub(crate) fn is_empty(&self) -> bool {
        self.cells.is_empty() && self.groups.is_empty()
    }
}

/// The ids of the cells that draw: on, 2D, and behind the sweep line.
pub(crate) fn shown_cells(i: &StormCellViewInputs<'_>) -> Vec<u32> {
    if !i.visible || i.view_mode != ViewMode::Flat2D {
        return Vec::new();
    }
    i.result
        .cells
        .iter()
        .filter(|c| footprint_behind_sweep_line(&c.outline, (c.lat, c.lon), i.radar, i.sweep))
        .map(|c| c.id)
        .collect()
}

/// Build the overlay's view-model. `project` maps `(lat, lon)` to screen px.
pub(crate) fn build(
    i: &StormCellViewInputs<'_>,
    project: impl Fn(f64, f64) -> [f32; 2],
) -> StormCellsVm {
    let mut vm = StormCellsVm::empty(i.phone);
    let shown = shown_cells(i);
    if shown.is_empty() {
        return vm;
    }
    vm.provisional = i.provisional;
    let style = vm.style;

    let ring_px = |ring: &[(f64, f64)]| -> Vec<[f32; 2]> {
        let open = match ring.split_last() {
            Some((last, rest)) if !rest.is_empty() && Some(last) == rest.first() => rest,
            _ => ring,
        };
        stroke_ready(open.iter().map(|&(lat, lon)| project(lat, lon)).collect())
    };

    // The cell the card is about: the phone's selection, or the desktop
    // pointer when the probe is not already naming it.
    let pointed = if i.phone {
        i.pointer.selected
    } else if i.probe_on {
        None
    } else {
        i.pointer.hover
    };
    let focus = pointed.and_then(|(lat, lon)| cell_at(i.result, &shown, lat, lon));

    // Groups that have at least one member on screen, each wholly behind
    // the sweep line like its cells.
    for g in &i.result.groups {
        let member_shown = g.cell_ids.iter().any(|id| shown.contains(id));
        if member_shown
            && g.outline.len() >= 3
            && footprint_behind_sweep_line(&g.outline, (g.lat, g.lon), i.radar, i.sweep)
        {
            vm.groups.push(ring_px(&g.outline));
        }
    }

    // A lone cell drawn as its core gets its envelope as a faint ring too.
    for &id in &shown {
        if let Some(cell) = i.result.cells.get(id as usize) {
            if cell.group_id.is_none() && cell.core_outline.len() >= 4 && cell.outline.len() >= 3 {
                vm.groups.push(ring_px(&cell.outline));
            }
        }
    }

    // Shown cells are in result order: strongest first.
    let mut placed: Vec<([f32; 2], [f32; 2])> = Vec::new();
    for &id in &shown {
        let Some(cell) = i.result.cells.get(id as usize) else {
            continue;
        };
        let ring = ring_px(drawn_outline(cell));
        if ring.len() < 3 {
            continue;
        }
        let track = i.tracks.and_then(|t| t.for_cell(id));
        if let Some(t) = track.filter(|t| t.trail.len() >= 2 || t.split_origin.is_some()) {
            let dots: Vec<[f32; 2]> = t.trail.iter().map(|&(a, b)| project(a, b)).collect();
            let from = dots[dots.len() - 1];
            let dot_alpha = path_fade(dots.len());
            vm.trails.push(TrailVm {
                cell_id: id,
                segment_alpha: dot_alpha.iter().skip(1).copied().collect(),
                dot_alpha,
                split_join: t.split_origin.map(|(a, b)| (project(a, b), dots[0])),
                arrow: t
                    .forecast
                    .and_then(|(a, b)| arrow(from, project(a, b), style.arrow_head_px)),
                dots,
                highlighted: focus == Some(id),
            });
        }
        if let Some(chip) = place_chip(cell, track, &ring, &style, &placed) {
            placed.push((chip.min, chip.size));
            vm.chips.push(ChipVm {
                hollow: i.provisional,
                ..chip
            });
        }
        vm.cells.push(CellOutlineVm {
            id,
            ring,
            highlighted: focus == Some(id),
        });
    }

    vm.card = focus.zip(pointed).and_then(|(id, (lat, lon))| {
        let cell = i.result.cells.get(id as usize)?;
        let anchor = if i.phone {
            CardAnchor::Bottom
        } else {
            CardAnchor::Pointer(project(lat, lon))
        };
        Some(card(i, cell, anchor))
    });
    vm.shown = shown;
    vm
}

/// Screen points closer than this are merged before stroking.
const RING_MIN_STEP_PX: f32 = 1.0;
/// A vertex where the ring turns back by more than this (degrees) is cut.
const RING_MAX_TURN_DEG: f32 = 150.0;

/// Make a projected ring safe to stroke: merge points less than a pixel
/// apart and cut vertices where the ring doubles back on itself. A stroke
/// joins segments with a miter, and at a near-reversal (a one-gate spike, or
/// a whole small outline collapsing at a low zoom) the miter runs off for
/// hundreds of pixels. Returns fewer than three points when nothing
/// drawable is left.
pub(crate) fn stroke_ready(mut ring: Vec<[f32; 2]>) -> Vec<[f32; 2]> {
    let dist = |a: [f32; 2], b: [f32; 2]| ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt();
    let cos_limit = (180.0 - RING_MAX_TURN_DEG).to_radians().cos();
    loop {
        let before = ring.len();
        ring.dedup_by(|b, a| dist(*a, *b) < RING_MIN_STEP_PX);
        while ring.len() > 1 && dist(ring[0], ring[ring.len() - 1]) < RING_MIN_STEP_PX {
            ring.pop();
        }
        let n = ring.len();
        if n < 3 {
            return ring;
        }
        // Cut every vertex whose incoming and outgoing edges are nearly
        // opposite (the angle at the vertex is under 30°).
        let keep: Vec<bool> = (0..n)
            .map(|i| {
                let (p, c, q) = (ring[(i + n - 1) % n], ring[i], ring[(i + 1) % n]);
                let (ax, ay) = (p[0] - c[0], p[1] - c[1]);
                let (bx, by) = (q[0] - c[0], q[1] - c[1]);
                let cos = (ax * bx + ay * by) / ((ax.hypot(ay)) * (bx.hypot(by)));
                cos < cos_limit
            })
            .collect();
        let mut i = 0;
        ring.retain(|_| {
            i += 1;
            keep[i - 1]
        });
        if ring.len() == before {
            return ring;
        }
    }
}

/// A peak change beyond this (dB, over the last three volumes) gets a
/// trend mark on the chip.
pub(crate) const TREND_MARK_DB: f32 = 3.0;

/// The trend mark for a peak change: up, down, or none.
pub(crate) fn trend_mark(trend_db: Option<f32>) -> Option<char> {
    match trend_db {
        Some(d) if d > TREND_MARK_DB => Some('\u{2191}'),
        Some(d) if d < -TREND_MARK_DB => Some('\u{2193}'),
        _ => None,
    }
}

/// The chip text for a cell: its peak, whole dBZ, led by its track's letter
/// and followed by a trend mark when tracked (`B 58\u{2191}`).
pub(crate) fn chip_text(cell: &StormCell, track: Option<&TrackVm>) -> String {
    let peak = format!("{:.0}", cell.peak_dbz);
    match track {
        Some(t) => {
            let mark = trend_mark(t.peak_trend_db)
                .map(String::from)
                .unwrap_or_default();
            format!("{} {peak}{mark}", t.label)
        }
        None => peak,
    }
}

/// Size of a chip carrying `text` at the style's font.
fn chip_size(text: &str, style: &OverlayStyle) -> [f32; 2] {
    let glyphs = text.chars().count() as f32;
    [
        glyphs * MONO_ADVANCE_EM * style.chip_font_px + 2.0 * CHIP_PAD_X,
        style.chip_font_px + CHIP_PAD_Y,
    ]
}

/// Place a cell's chip at its outline's top-right, or `None` when the
/// outline is too small on screen or the chip would crowd one already
/// placed.
fn place_chip(
    cell: &StormCell,
    track: Option<&TrackVm>,
    ring: &[[f32; 2]],
    style: &OverlayStyle,
    placed: &[([f32; 2], [f32; 2])],
) -> Option<ChipVm> {
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for p in ring {
        min_x = min_x.min(p[0]);
        min_y = min_y.min(p[1]);
        max_x = max_x.max(p[0]);
        max_y = max_y.max(p[1]);
    }
    if (max_x - min_x).max(max_y - min_y) < CHIP_MIN_OUTLINE_PX {
        return None;
    }
    let text = chip_text(cell, track);
    let size = chip_size(&text, style);
    let min = [max_x + CHIP_GAP_X, min_y - size[1] * 0.4];
    let gap = style.chip_spacing_px;
    let crowds = placed.iter().any(|(m, s)| {
        min[0] < m[0] + s[0] + gap
            && m[0] < min[0] + size[0] + gap
            && min[1] < m[1] + s[1] + gap
            && m[1] < min[1] + size[1] + gap
    });
    (!crowds).then_some(ChipVm {
        cell_id: cell.id,
        min,
        size,
        text,
        hollow: false,
    })
}

/// The card's words for a cell. The phone's card is compact: three lines.
fn card(i: &StormCellViewInputs<'_>, cell: &StormCell, anchor: CardAnchor) -> CardVm {
    let group = cell.group_id.and_then(|g| i.result.groups.get(g as usize));
    let membership = group.and_then(|g| {
        let pos = g.cell_ids.iter().position(|&c| c == cell.id)?;
        Some(format!("{} of {} in this group", pos + 1, g.cell_ids.len()))
    });
    let place = format!(
        "{:.0} km at {:03.0}\u{00B0} from {}",
        cell.range_from_radar_km,
        cell.bearing_from_radar_deg.rem_euclid(360.0),
        i.site_id
    );
    let track = i.tracks.and_then(|t| t.for_cell(cell.id));
    let line = |text: String, dim: bool| CardLine { text, dim };
    let mut lines = Vec::new();
    if i.phone {
        lines.push(line(
            format!(
                "{:.0} km\u{00B2} \u{00B7} mean {:.0} dBZ \u{00B7} distinct at {:.0}",
                cell.area_km2, cell.mean_dbz, cell.level_dbz
            ),
            false,
        ));
        if let Some(t) = track {
            lines.push(line(
                format!("{} \u{00B7} {}", motion_text(t), short_age_text(t)),
                false,
            ));
        } else if let Some(m) = membership {
            lines.push(line(m, false));
        }
        lines.push(line(place, true));
    } else {
        lines.push(line(
            format!(
                "Area {:.0} km\u{00B2} \u{00B7} mean {:.0} dBZ",
                cell.area_km2, cell.mean_dbz
            ),
            false,
        ));
        lines.push(line(
            format!("Distinct at {:.0} dBZ", cell.level_dbz),
            false,
        ));
        if let Some(m) = membership {
            lines.push(line(m, false));
        }
        if let Some(t) = track {
            lines.push(line(motion_text(t), false));
            lines.push(line(age_text(t), false));
        }
        lines.push(line(place, true));
    }
    if let Some(t) = track {
        if let Some((bearing, km)) = t.from_reference {
            lines.push(line(
                format!("{km:.0} km at {bearing:03.0}\u{00B0} from you"),
                true,
            ));
        }
        if let Some(text) = approach_text(t) {
            lines.push(line(text, false));
        }
    }
    if i.provisional {
        lines.push(line("Provisional: sweep still arriving".to_string(), true));
    }
    let title = match track {
        Some(t) => {
            let trend = trend_mark(t.peak_trend_db)
                .zip(t.peak_trend_db)
                .map(|(mark, d)| format!(" {mark} from {:.0}", cell.peak_dbz - d))
                .unwrap_or_default();
            format!("{} \u{00B7} {:.0} dBZ peak{trend}", t.label, cell.peak_dbz)
        }
        None => format!("Cell \u{00B7} {:.0} dBZ peak", cell.peak_dbz),
    };
    CardVm {
        cell_id: cell.id,
        title,
        lines,
        anchor,
    }
}

/// Eight-point compass name for a direction in degrees.
fn compass(deg: f64) -> &'static str {
    const NAMES: [&str; 8] = ["N", "NE", "E", "SE", "S", "SW", "W", "NW"];
    NAMES[((deg.rem_euclid(360.0) + 22.5) / 45.0) as usize % 8]
}

/// Motion confidence in a word: high from 0.75, medium from the forecast
/// threshold, low below it.
fn confidence_word(confidence: f64) -> &'static str {
    if confidence >= 0.75 {
        "high"
    } else if confidence >= crate::storm_cells::track::MIN_FORECAST_CONFIDENCE {
        "medium"
    } else {
        "low"
    }
}

/// `Moving NE 47 km/h, high confidence`, `Stationary`, or, for a track
/// seen once, `Motion not known yet`.
fn motion_text(t: &TrackVm) -> String {
    if t.stationary {
        "Stationary".to_string()
    } else if t.confidence <= 0.0 {
        "Motion not known yet".to_string()
    } else {
        format!(
            "Moving {} {:.0} km/h, {} confidence",
            compass(t.direction_deg),
            t.speed_kmh,
            confidence_word(t.confidence)
        )
    }
}

/// `Tracked 6 volumes · 25 min`, or `New this volume`.
fn age_text(t: &TrackVm) -> String {
    if t.age_volumes <= 1 {
        "New this volume".to_string()
    } else {
        format!(
            "Tracked {} volumes \u{00B7} {:.0} min",
            t.age_volumes, t.age_minutes
        )
    }
}

/// The phone card's age, short enough to share a line with the motion on
/// a 390 px screen.
fn short_age_text(t: &TrackVm) -> String {
    if t.age_volumes <= 1 {
        "new".to_string()
    } else {
        format!("{:.0} min", t.age_minutes)
    }
}

/// Closest approach beyond this many minutes is withheld: extrapolation
/// cannot see growth or decay that far out.
pub(crate) const APPROACH_HORIZON_MIN: f64 = 60.0;

/// The closest-approach line, when "My location" is on and the motion is
/// confident: `Closest to you: 14 km in 22 min`.
fn approach_text(t: &TrackVm) -> Option<String> {
    let a = t.approach.filter(|_| !t.stationary)?;
    Some(if a.minutes < 1.0 {
        "Moving away from you".to_string()
    } else if a.minutes > APPROACH_HORIZON_MIN {
        "Closest to you: more than an hour out".to_string()
    } else {
        format!(
            "Closest to you: {:.0} km in {:.0} min",
            a.distance_km, a.minutes
        )
    })
}

/// The data probe's extra line when the pointer is over a cell that draws:
/// `Cell 62 dBZ · 48 km²` (`Cell B 62 dBZ · 48 km²` when tracked).
pub(crate) fn probe_line(i: &StormCellViewInputs<'_>, lat: f64, lon: f64) -> Option<String> {
    let shown = shown_cells(i);
    let id = cell_at(i.result, &shown, lat, lon)?;
    let cell = i.result.cells.get(id as usize)?;
    let name = match i.tracks.and_then(|t| t.for_cell(id)) {
        Some(t) => format!("Cell {}", t.label),
        None => "Cell".to_string(),
    };
    Some(format!(
        "{name} {:.0} dBZ \u{00B7} {:.0} km\u{00B2}",
        cell.peak_dbz, cell.area_km2
    ))
}

/// Gap between the desktop pointer and its card.
const CARD_POINTER_GAP: f32 = 16.0;
/// Margin the card keeps from the map's edges.
const CARD_EDGE_MARGIN: f32 = 8.0;

/// Top-left of a card of `size` beside the pointer at `anchor`, inside
/// `canvas` (`[min_x, min_y, max_x, max_y]`): below-right of the pointer,
/// flipped left or up when it would leave the map.
pub(crate) fn place_card_at_pointer(
    anchor: [f32; 2],
    size: [f32; 2],
    canvas: [f32; 4],
) -> [f32; 2] {
    let mut x = anchor[0] + CARD_POINTER_GAP;
    if x + size[0] > canvas[2] - CARD_EDGE_MARGIN {
        x = anchor[0] - CARD_POINTER_GAP - size[0];
    }
    let mut y = anchor[1] + CARD_POINTER_GAP;
    if y + size[1] > canvas[3] - CARD_EDGE_MARGIN {
        y = anchor[1] - CARD_POINTER_GAP - size[1];
    }
    [
        x.max(canvas[0] + CARD_EDGE_MARGIN),
        y.max(canvas[1] + CARD_EDGE_MARGIN),
    ]
}

/// Top-left of the phone's card: centered, its bottom `bottom_inset` px
/// above the map's bottom edge (clear of the corner scrims).
pub(crate) fn place_card_at_bottom(
    size: [f32; 2],
    canvas: [f32; 4],
    bottom_inset: f32,
) -> [f32; 2] {
    let cx = (canvas[0] + canvas[2]) / 2.0;
    [
        (cx - size[0] / 2.0).max(canvas[0] + CARD_EDGE_MARGIN),
        (canvas[3] - bottom_inset - size[1]).max(canvas[1] + CARD_EDGE_MARGIN),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storm_cells::StormGroup;
    use wasm_bindgen_test::wasm_bindgen_test;

    const RADAR: (f64, f64) = (41.73, -93.72);

    /// A square footprint of half-side `h` degrees around `(lat, lon)`.
    fn square(lat: f64, lon: f64, h: f64) -> Vec<(f64, f64)> {
        vec![
            (lat - h, lon - h),
            (lat - h, lon + h),
            (lat + h, lon + h),
            (lat + h, lon - h),
            (lat - h, lon - h),
        ]
    }

    fn cell(id: u32, lat: f64, lon: f64, h: f64, peak: f32) -> StormCell {
        StormCell {
            id,
            group_id: None,
            level_dbz: 35.0,
            peak_dbz: peak,
            mean_dbz: peak - 12.0,
            area_km2: (2.0 * h * 111.0).powi(2) as f32,
            gate_count: 100,
            lat,
            lon,
            bounds: (lat - h, lon - h, lat + h, lon + h),
            bearing_from_radar_deg: 239.4,
            range_from_radar_km: 98.2,
            orientation_deg: 0.0,
            elongation: 1.0,
            outline: square(lat, lon, h),
            core_outline: Vec::new(),
        }
    }

    /// Two cells NE of the radar, strongest first, in one group.
    fn result() -> StormCellsResult {
        let mut a = cell(0, 42.2, -93.2, 0.1, 62.0);
        let mut b = cell(1, 42.2, -92.6, 0.1, 55.0);
        a.group_id = Some(0);
        b.group_id = Some(0);
        b.level_dbz = 50.0;
        StormCellsResult {
            cells: vec![a, b],
            groups: vec![StormGroup {
                id: 0,
                level_dbz: 35.0,
                peak_dbz: 62.0,
                area_km2: 900.0,
                cell_ids: vec![0, 1],
                lat: 42.2,
                lon: -92.9,
                outline: square(42.2, -92.9, 0.45),
            }],
            ..StormCellsResult::default()
        }
    }

    fn inputs<'a>(r: &'a StormCellsResult, p: &'a StormCellPointer) -> StormCellViewInputs<'a> {
        StormCellViewInputs {
            result: r,
            visible: true,
            view_mode: ViewMode::Flat2D,
            radar: RADAR,
            site_id: "KDMX",
            sweep: None,
            provisional: false,
            phone: false,
            pointer: p,
            probe_on: false,
            tracks: None,
        }
    }

    /// 100 px per degree, north up, origin at (43, -94).
    fn project(lat: f64, lon: f64) -> [f32; 2] {
        [((lon + 94.0) * 100.0) as f32, ((43.0 - lat) * 100.0) as f32]
    }

    fn controls<'a>(
        v: &'a mut bool,
        t: &'a mut f32,
        p: &'a mut StormCellPointer,
    ) -> StormCellControls<'a> {
        StormCellControls {
            visible: v,
            threshold_dbz: t,
            tracks: Box::leak(Box::new(false)),
            pointer: p,
        }
    }

    // ── Intents ────────────────────────────────────────────────────────────

    #[wasm_bindgen_test]
    fn intents_set_visibility_threshold_and_pointer() {
        let (mut v, mut t, mut p) = (false, 35.0, StormCellPointer::default());
        reduce(
            controls(&mut v, &mut t, &mut p),
            StormCellIntent::SetVisible(true),
        );
        assert!(v);
        reduce(
            controls(&mut v, &mut t, &mut p),
            StormCellIntent::SetThreshold(45.0),
        );
        assert_eq!(t, 45.0);
        reduce(
            controls(&mut v, &mut t, &mut p),
            StormCellIntent::SetThreshold(5.0),
        );
        assert_eq!(t, crate::core::storm_cells::THRESHOLD_MIN_DBZ);
        reduce(
            controls(&mut v, &mut t, &mut p),
            StormCellIntent::Hover(Some((42.0, -93.0))),
        );
        reduce(
            controls(&mut v, &mut t, &mut p),
            StormCellIntent::Select(Some((42.1, -93.1))),
        );
        assert_eq!(p.hover, Some((42.0, -93.0)));
        assert_eq!(p.selected, Some((42.1, -93.1)));
        reduce(
            controls(&mut v, &mut t, &mut p),
            StormCellIntent::Hover(Some((f64::NAN, -93.0))),
        );
        assert_eq!(p.hover, None);
        // Off drops the pointer state.
        reduce(
            controls(&mut v, &mut t, &mut p),
            StormCellIntent::SetVisible(false),
        );
        assert!(!v);
        assert_eq!(p, StormCellPointer::default());
    }

    #[wasm_bindgen_test]
    fn a_tap_off_a_cell_dismisses_an_open_card() {
        let open = StormCellPointer {
            selected: Some((42.2, -93.2)),
            ..Default::default()
        };
        assert_eq!(
            tap_dismisses_card(false, &open),
            Some(StormCellIntent::Select(None))
        );
        assert_eq!(tap_dismisses_card(true, &open), None);
        assert_eq!(
            tap_dismisses_card(false, &StormCellPointer::default()),
            None
        );
    }

    // ── Hit test ───────────────────────────────────────────────────────────

    #[wasm_bindgen_test]
    fn point_in_ring_handles_concave_rings() {
        // A "C" opening east: the notch is outside.
        let c = vec![
            (0.0, 0.0),
            (0.0, 3.0),
            (1.0, 3.0),
            (1.0, 1.0),
            (2.0, 1.0),
            (2.0, 3.0),
            (3.0, 3.0),
            (3.0, 0.0),
            (0.0, 0.0),
        ];
        assert!(point_in_ring(&c, 0.5, 2.0));
        assert!(point_in_ring(&c, 1.5, 0.5));
        assert!(!point_in_ring(&c, 1.5, 2.0), "the notch");
        assert!(!point_in_ring(&c, 4.0, 1.0));
        assert!(!point_in_ring(&c[..2], 0.0, 0.0), "degenerate ring");
    }

    #[wasm_bindgen_test]
    fn strongest_cell_wins_when_footprints_nest() {
        let mut r = result();
        // A weak, large cell around the strong one.
        let mut big = cell(2, 42.2, -93.2, 0.3, 48.0);
        big.area_km2 = 4000.0;
        r.cells.push(big);
        let all = [0, 1, 2];
        assert_eq!(cell_at(&r, &all, 42.2, -93.2), Some(0));
        assert_eq!(cell_at(&r, &all, 42.4, -93.2), Some(2));
        assert_eq!(cell_at(&r, &all, 42.2, -92.6), Some(1));
        assert_eq!(cell_at(&r, &all, 41.0, -95.0), None);
        // A cell that is not shown is not hit.
        assert_eq!(cell_at(&r, &[1, 2], 42.2, -93.2), Some(2));
    }

    // ── Rays rule ──────────────────────────────────────────────────────────

    #[wasm_bindgen_test]
    fn rays_draws_a_cell_only_behind_the_sweep_line() {
        let r = result();
        let p = StormCellPointer::default();
        let mut i = inputs(&r, &p);
        // Cell 0 spans azimuths of about 29° to 52°, cell 1 about 53° to 68°.
        i.sweep = Some((20.0, 0.0));
        assert!(shown_cells(&i).is_empty(), "line has not reached them");
        i.sweep = Some((60.0, 0.0));
        assert_eq!(shown_cells(&i), vec![0], "only the nearer-north cell");
        i.sweep = Some((90.0, 0.0));
        assert_eq!(shown_cells(&i), vec![0, 1]);
        // Line just past them but the sweep started inside them: their
        // footprint straddles the start, so part is previous-sweep pixels.
        i.sweep = Some((90.0, 50.0));
        assert!(!shown_cells(&i).contains(&0));
        // Wrapping through north: started at 350°, now at 60°.
        i.sweep = Some((60.0, 350.0));
        assert_eq!(shown_cells(&i), vec![0]);
        // No sweep line: everything on screen is the current sweep.
        i.sweep = None;
        assert_eq!(shown_cells(&i), vec![0, 1]);
    }

    #[wasm_bindgen_test]
    fn nothing_draws_off_or_in_3d() {
        let r = result();
        let p = StormCellPointer {
            hover: Some((42.2, -93.2)),
            selected: None,
        };
        let mut i = inputs(&r, &p);
        i.view_mode = ViewMode::Globe3D;
        let vm = build(&i, project);
        assert!(vm.is_empty() && vm.card.is_none() && vm.chips.is_empty());
        assert_eq!(probe_line(&i, 42.2, -93.2), None);
        i.view_mode = ViewMode::Flat2D;
        i.visible = false;
        assert!(build(&i, project).is_empty());
    }

    // ── View-model ─────────────────────────────────────────────────────────

    #[wasm_bindgen_test]
    fn outlines_groups_and_chips_project_to_screen() {
        let r = result();
        let p = StormCellPointer::default();
        let vm = build(&inputs(&r, &p), project);
        assert_eq!(vm.cells.len(), 2);
        assert_eq!(vm.groups.len(), 1);
        // The closing point is dropped: four corners.
        assert_eq!(vm.cells[0].ring.len(), 4);
        assert_eq!(vm.chips.len(), 2);
        let chip = &vm.chips[0];
        assert_eq!(chip.text, "62");
        assert!(!chip.hollow);
        // Top-right of cell 0's outline: x right of its east edge (-93.1),
        // y straddling its north edge (42.3).
        let east = project(42.3, -93.1);
        assert!(chip.min[0] > east[0] && chip.min[0] < east[0] + 6.0);
        assert!(chip.min[1] < east[1] && chip.min[1] + chip.size[1] > east[1]);
        // Two mono glyphs at 11 px plus padding; 16 px tall.
        assert!((chip.size[0] - (2.0 * 0.6 * 11.0 + 8.0)).abs() < 1e-4);
        assert_eq!(chip.size[1], 16.0);
        // The closing point is dropped: four corners.
        assert_eq!(vm.cells[0].ring[0], project(42.1, -93.3));
        assert_eq!(vm.style, overlay_style(false));
    }

    #[wasm_bindgen_test]
    fn chips_go_strongest_first_and_skip_overlaps() {
        let mut r = result();
        // Move cell 1 right next to cell 0 so their chips collide.
        r.cells[1] = cell(1, 42.2, -93.15, 0.1, 55.0);
        let p = StormCellPointer::default();
        let vm = build(&inputs(&r, &p), project);
        assert_eq!(vm.cells.len(), 2, "both outlines still draw");
        assert_eq!(vm.chips.len(), 1);
        assert_eq!(vm.chips[0].cell_id, 0, "the stronger cell keeps its chip");
    }

    #[wasm_bindgen_test]
    fn small_outlines_carry_no_chip() {
        let r = result();
        let p = StormCellPointer::default();
        // 20 px per degree: cells are 4 px across.
        let vm = build(&inputs(&r, &p), |lat, lon| {
            [((lon + 94.0) * 20.0) as f32, ((43.0 - lat) * 20.0) as f32]
        });
        assert_eq!(vm.cells.len(), 2);
        assert!(vm.chips.is_empty());
    }

    #[wasm_bindgen_test]
    fn provisional_cells_get_hollow_chips_and_say_so() {
        let r = result();
        let p = StormCellPointer {
            hover: Some((42.2, -93.2)),
            selected: None,
        };
        let mut i = inputs(&r, &p);
        i.provisional = true;
        let vm = build(&i, project);
        assert!(vm.provisional);
        assert!(vm.chips.iter().all(|c| c.hollow));
        let card = vm.card.expect("hover card");
        assert!(card.lines.iter().any(|l| l.text.starts_with("Provisional")));
    }

    #[wasm_bindgen_test]
    fn desktop_hover_card_reads_the_cell() {
        let r = result();
        let p = StormCellPointer {
            hover: Some((42.2, -92.6)),
            selected: None,
        };
        let vm = build(&inputs(&r, &p), project);
        let card = vm.card.expect("hover card");
        assert_eq!(card.cell_id, 1);
        assert_eq!(card.title, "Cell \u{00B7} 55 dBZ peak");
        let text: Vec<&str> = card.lines.iter().map(|l| l.text.as_str()).collect();
        assert!(text.contains(&"Distinct at 50 dBZ"), "{text:?}");
        assert!(text.contains(&"2 of 2 in this group"), "{text:?}");
        assert!(text.contains(&"98 km at 239\u{00B0} from KDMX"), "{text:?}");
        assert!(text[0].starts_with("Area ") && text[0].contains("mean 43 dBZ"));
        assert_eq!(card.anchor, CardAnchor::Pointer(project(42.2, -92.6)));
        assert!(vm.cells[1].highlighted && !vm.cells[0].highlighted);

        // Off any cell: no card. With the probe on: no card either.
        let off = StormCellPointer {
            hover: Some((41.0, -95.0)),
            selected: None,
        };
        assert_eq!(build(&inputs(&r, &off), project).card, None);
        let mut i = inputs(&r, &p);
        i.probe_on = true;
        assert_eq!(build(&i, project).card, None);
    }

    #[wasm_bindgen_test]
    fn phone_card_follows_the_selection_not_the_pointer() {
        let r = result();
        let p = StormCellPointer {
            hover: Some((42.2, -92.6)),
            selected: Some((42.2, -93.2)),
        };
        let mut i = inputs(&r, &p);
        i.phone = true;
        let vm = build(&i, project);
        assert_eq!(vm.style, overlay_style(true));
        let card = vm.card.expect("selected card");
        assert_eq!(card.cell_id, 0);
        assert_eq!(card.anchor, CardAnchor::Bottom);
        assert!(card.lines.len() <= 4, "compact");
        assert!(overlay_style(true).outline_px > overlay_style(false).outline_px);
        assert_eq!(overlay_style(true).chip_font_px, 12.0);
        assert!(overlay_style(true).chip_spacing_px > overlay_style(false).chip_spacing_px);
    }

    #[wasm_bindgen_test]
    fn probe_line_names_the_cell_under_the_pointer() {
        let r = result();
        let p = StormCellPointer::default();
        let i = inputs(&r, &p);
        let line = probe_line(&i, 42.2, -93.2).expect("over cell 0");
        assert!(line.starts_with("Cell 62 dBZ \u{00B7} "), "{line}");
        assert!(line.ends_with("km\u{00B2}"), "{line}");
        assert_eq!(probe_line(&i, 41.0, -95.0), None);
    }

    #[wasm_bindgen_test]
    fn cells_draw_their_core_inside_a_faint_envelope_ring() {
        let mut r = result();
        // Cell 0's core: most of its footprint, big enough for a chip.
        r.cells[0].core_outline = square(42.2, -93.2, 0.08);
        // A lone cell (no group) with its core: core at full stroke, its
        // envelope as a faint ring.
        let mut lone = cell(2, 42.8, -92.0, 0.1, 50.0);
        lone.core_outline = square(42.8, -92.0, 0.08);
        r.cells.push(lone);
        // A lone cell without a core ring: its footprint, no faint ring.
        r.cells.push(cell(3, 41.0, -92.0, 0.1, 48.0));
        let p = StormCellPointer::default();
        let vm = build(&inputs(&r, &p), project);
        assert_eq!(vm.cells.len(), 4);
        let open = |ring: Vec<(f64, f64)>| -> Vec<[f32; 2]> {
            ring[..4].iter().map(|&(la, lo)| project(la, lo)).collect()
        };
        assert_eq!(vm.cells[0].ring, open(square(42.2, -93.2, 0.08)));
        assert_eq!(vm.cells[2].ring, open(square(42.8, -92.0, 0.08)));
        assert_eq!(vm.cells[3].ring, open(square(41.0, -92.0, 0.1)));
        // A grouped cell without a core ring falls back to its footprint.
        assert_eq!(vm.cells[1].ring, open(square(42.2, -92.6, 0.1)));
        // Faint rings: the group's envelope and the lone cell's, not the
        // coreless cell's.
        assert_eq!(
            vm.groups,
            vec![
                open(square(42.2, -92.9, 0.45)),
                open(square(42.8, -92.0, 0.1))
            ]
        );
        // Chips sit at the drawn core's top-right.
        for (id, (lat, lon)) in [(0u32, (42.28, -93.12)), (2, (42.88, -91.92))] {
            let chip = vm.chips.iter().find(|c| c.cell_id == id).expect("chip");
            let east = project(lat, lon);
            assert!(chip.min[0] > east[0] && chip.min[0] < east[0] + 6.0, "{id}");
            assert!(
                chip.min[1] < east[1] && chip.min[1] + chip.size[1] > east[1],
                "{id}"
            );
        }

        // The hit test still takes the whole footprint: a point outside the
        // core but inside cell 0's share of the envelope opens its card.
        let p = StormCellPointer {
            hover: Some((42.2, -93.105)),
            selected: None,
        };
        let vm = build(&inputs(&r, &p), project);
        assert_eq!(vm.card.map(|c| c.cell_id), Some(0));
    }

    #[wasm_bindgen_test]
    fn group_ring_is_solid_thinner_and_lighter_than_a_cell() {
        for phone in [false, true] {
            let s = overlay_style(phone);
            assert!(s.group_px < s.outline_px, "{s:?}");
            assert!(s.group_halo_px < s.halo_px, "{s:?}");
            assert!(s.group_halo_px > s.group_px, "{s:?}");
        }
        assert_eq!(overlay_style(false).group_px, 1.0);
        assert_eq!(overlay_style(true).group_px, 1.5);
        // The group ring is the envelope itself, not dashed pieces: one
        // closed ring per group.
        let r = result();
        let p = StormCellPointer::default();
        let vm = build(&inputs(&r, &p), project);
        assert_eq!(vm.groups.len(), 1);
        assert_eq!(vm.groups[0].len(), 4);
    }

    #[wasm_bindgen_test]
    fn group_outline_draws_only_with_a_member_on_screen() {
        let r = result();
        let p = StormCellPointer::default();
        let mut i = inputs(&r, &p);
        // The group's envelope spans past both cells; with the line short
        // of the far cell neither the far cell nor the group draws.
        i.sweep = Some((60.0, 0.0));
        let vm = build(&i, project);
        assert_eq!(vm.cells.len(), 1);
        assert!(vm.groups.is_empty());
    }

    #[wasm_bindgen_test]
    fn rings_lose_spikes_and_collapsed_points_before_stroking() {
        // A square with a one-pixel-wide spike out of its top edge.
        let spiky = vec![
            [0.0, 0.0],
            [4.0, 0.0],
            [5.0, -40.0],
            [5.5, 0.0],
            [10.0, 0.0],
            [10.0, 10.0],
            [0.0, 10.0],
        ];
        let clean = stroke_ready(spiky);
        assert!(!clean.contains(&[5.0, -40.0]), "{clean:?}");
        assert!(clean.iter().all(|p| p[1] >= 0.0));
        assert!(clean.len() >= 4);
        // A square's right angles stay.
        let square = vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]];
        assert_eq!(stroke_ready(square.clone()), square);
        // An outline that collapses under a pixel leaves nothing to draw.
        let tiny = vec![[0.0, 0.0], [0.3, 0.0], [0.3, 0.4], [0.0, 0.4]];
        assert!(stroke_ready(tiny).len() < 3);
    }

    #[wasm_bindgen_test]
    fn cards_stay_on_the_map() {
        let canvas = [0.0, 0.0, 800.0, 600.0];
        assert_eq!(
            place_card_at_pointer([100.0, 100.0], [200.0, 80.0], canvas),
            [116.0, 116.0]
        );
        // Near the right and bottom edges it flips to the other side.
        assert_eq!(
            place_card_at_pointer([700.0, 560.0], [200.0, 80.0], canvas),
            [484.0, 464.0]
        );
        let at = place_card_at_bottom([300.0, 70.0], [0.0, 0.0, 390.0, 600.0], 40.0);
        assert_eq!(at, [45.0, 490.0]);
    }

    // ── Tracks ─────────────────────────────────────────────────────────────

    #[wasm_bindgen_test]
    fn the_tracks_checkbox_is_an_intent() {
        let (mut v, mut t, mut tracks, mut p) = (true, 35.0, false, StormCellPointer::default());
        for on in [true, false] {
            reduce(
                StormCellControls {
                    visible: &mut v,
                    threshold_dbz: &mut t,
                    tracks: &mut tracks,
                    pointer: &mut p,
                },
                StormCellIntent::SetTracks(on),
            );
            assert_eq!(tracks, on);
        }
    }

    /// A track on cell `cell_id` with three trail points, the last at
    /// `(lat, lon)`, moving NE at 47 km/h.
    fn track(cell_id: u32, label: &str, lat: f64, lon: f64, confidence: f64) -> TrackVm {
        TrackVm {
            id: cell_id,
            label: label.to_string(),
            cell_id: Some(cell_id),
            lat,
            lon,
            trail: vec![
                (lat - 0.06, lon - 0.08),
                (lat - 0.03, lon - 0.04),
                (lat, lon),
            ],
            forecast: (confidence >= crate::storm_cells::track::MIN_FORECAST_CONFIDENCE)
                .then_some((lat + 0.15, lon + 0.2)),
            speed_kmh: 47.0,
            direction_deg: 47.0,
            confidence,
            age_volumes: 6,
            age_minutes: 25.0,
            peak_trend_db: Some(4.0),
            stationary: false,
            coasting: false,
            from_reference: None,
            approach: None,
            split_origin: None,
        }
    }

    fn tracks_view() -> TracksView {
        let mut b = track(1, "B", 42.2, -92.6, 0.25);
        b.peak_trend_db = Some(1.0);
        b.age_volumes = 2;
        b.age_minutes = 5.0;
        b.trail.drain(..1);
        TracksView {
            provisional: false,
            tracks: vec![track(0, "D", 42.2, -93.2, 0.9), b],
        }
    }

    #[wasm_bindgen_test]
    fn chips_lead_with_the_track_letter_and_mark_the_trend() {
        let (r, p) = (result(), StormCellPointer::default());
        let tv = tracks_view();
        let mut i = inputs(&r, &p);
        assert_eq!(chip_text(&r.cells[0], None), "62");
        i.tracks = Some(&tv);
        let vm = build(&i, project);
        let texts: Vec<&str> = vm.chips.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, vec!["D 62\u{2191}", "B 55"]);
        // Wider text, wider chip.
        let plain = build(&inputs(&r, &p), project);
        assert!(vm.chips[0].size[0] > plain.chips[0].size[0]);
        assert_eq!(trend_mark(Some(-3.5)), Some('\u{2193}'));
        assert_eq!(trend_mark(Some(3.0)), None);
        assert_eq!(trend_mark(None), None);
    }

    #[wasm_bindgen_test]
    fn trails_draw_with_an_arrow_only_when_motion_is_confident() {
        let (r, p) = (result(), StormCellPointer::default());
        let tv = tracks_view();
        let mut i = inputs(&r, &p);
        assert!(build(&i, project).trails.is_empty(), "tracks off");
        i.tracks = Some(&tv);
        let vm = build(&i, project);
        assert_eq!(vm.trails.len(), 2);
        let d = &vm.trails[0];
        assert_eq!(d.cell_id, 0);
        assert_eq!(d.dots.len(), 3);
        assert_eq!(d.dots[2], project(42.2, -93.2));
        let a = d.arrow.as_ref().expect("confident");
        assert_eq!(a.from, d.dots[2]);
        assert_eq!(a.to, project(42.35, -93.0));
        assert_eq!(a.head[0], a.to);
        // B's track is two volumes old: trail, no arrow.
        assert_eq!(vm.trails[1].dots.len(), 2);
        assert!(vm.trails[1].arrow.is_none());
        // A one-point trail or a coasting track draws nothing.
        let mut lone = tracks_view();
        lone.tracks[1].trail.truncate(1);
        lone.tracks[0].cell_id = None;
        i.tracks = Some(&lone);
        assert!(build(&i, project).trails.is_empty());
    }

    #[wasm_bindgen_test]
    fn with_tracks_the_chip_sits_on_the_core_and_the_path_on_the_centroid() {
        let p = StormCellPointer::default();
        let tv = tracks_view();
        // Cell 0's core sits off-centre in its footprint (north-east).
        let mut r = result();
        r.cells[0].core_outline = square(42.22, -93.18, 0.07);
        let mut i = inputs(&r, &p);
        i.tracks = Some(&tv);
        let vm = build(&i, project);
        let chip = vm.chips.iter().find(|c| c.cell_id == 0).expect("chip");
        let core_ne = project(42.29, -93.11);
        assert!(chip.min[0] > core_ne[0] && chip.min[0] < core_ne[0] + 6.0);
        assert!(chip.min[1] < core_ne[1] && chip.min[1] + chip.size[1] > core_ne[1]);
        // The path, its join and the arrow start from the centroid, not
        // from the core.
        let d = vm.trails.iter().find(|t| t.cell_id == 0).expect("trail");
        let centroid = project(r.cells[0].lat, r.cells[0].lon);
        assert_eq!(d.dots.last(), Some(&centroid));
        assert_eq!(d.arrow.as_ref().map(|a| a.from), Some(centroid));
        // And they do not depend on which outline is drawn.
        let mut bare = result();
        bare.cells[0].core_outline.clear();
        let mut j = inputs(&bare, &p);
        j.tracks = Some(&tv);
        assert_eq!(build(&j, project).trails, vm.trails);
    }

    #[wasm_bindgen_test]
    fn the_path_fades_with_age_newest_strongest() {
        assert_eq!(path_fade(1), vec![1.0]);
        let f = path_fade(5);
        assert_eq!(f.len(), 5);
        assert_eq!(f[0], PATH_OLDEST_ALPHA);
        assert_eq!(f[4], 1.0);
        assert!(f.windows(2).all(|w| w[0] < w[1]), "{f:?}");

        // A 24-point path draws every point, fading toward the oldest.
        let (r, p) = (result(), StormCellPointer::default());
        let mut tv = tracks_view();
        tv.tracks[0].trail = (0..24)
            .map(|k| {
                (
                    42.2 - 0.01 * (23 - k) as f64,
                    -93.2 - 0.01 * (23 - k) as f64,
                )
            })
            .collect();
        let mut i = inputs(&r, &p);
        i.tracks = Some(&tv);
        let vm = build(&i, project);
        let d = &vm.trails[0];
        assert_eq!(d.dots.len(), 24);
        assert_eq!(d.dot_alpha.len(), 24);
        assert_eq!(d.segment_alpha.len(), 23);
        assert_eq!(d.segment_alpha[22], 1.0);
        assert_eq!(d.segment_alpha[0], d.dot_alpha[1]);
        assert!(d.segment_alpha.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(d.dots[23], project(42.2, -93.2));
        assert!(d.split_join.is_none());
    }

    #[wasm_bindgen_test]
    fn a_split_track_joins_its_parent_with_a_dashed_line() {
        let (r, p) = (result(), StormCellPointer::default());
        let mut tv = tracks_view();
        // B split off D one volume ago: one point, plus the join.
        tv.tracks[1].trail = vec![(42.2, -92.6)];
        tv.tracks[1].split_origin = Some((42.15, -92.9));
        let mut i = inputs(&r, &p);
        i.tracks = Some(&tv);
        let vm = build(&i, project);
        let b = vm.trails.iter().find(|t| t.cell_id == 1).expect("drawn");
        assert_eq!(b.dots, vec![project(42.2, -92.6)]);
        assert_eq!(
            b.split_join,
            Some((project(42.15, -92.9), project(42.2, -92.6)))
        );
    }

    #[wasm_bindgen_test]
    fn arrow_heads_point_along_the_arrow() {
        let a = arrow([0.0, 0.0], [100.0, 0.0], 10.0).unwrap();
        assert_eq!(a.head[0], [100.0, 0.0]);
        assert_eq!(a.head[1], [90.0, 4.5]);
        assert_eq!(a.head[2], [90.0, -4.5]);
        // A short arrow keeps its head inside it; a dot has none.
        let a = arrow([0.0, 0.0], [0.0, 4.0], 10.0).unwrap();
        assert_eq!(a.head[1][1], 0.0);
        assert!(arrow([5.0, 5.0], [5.4, 5.0], 10.0).is_none());
    }

    #[wasm_bindgen_test]
    fn rays_hides_trails_of_cells_ahead_of_the_sweep_line() {
        let (r, p) = (result(), StormCellPointer::default());
        let tv = tracks_view();
        let mut i = inputs(&r, &p);
        i.tracks = Some(&tv);
        // Swept from north round to 60°: neither cell (both near 60-70°)
        // is wholly behind the line.
        i.sweep = Some((40.0, 0.0));
        let vm = build(&i, project);
        assert!(vm.trails.is_empty() && vm.chips.is_empty());
        i.view_mode = ViewMode::Globe3D;
        i.sweep = None;
        assert!(build(&i, project).trails.is_empty());
    }

    #[wasm_bindgen_test]
    fn the_card_tells_motion_age_trend_and_closest_approach() {
        let r = result();
        let p = StormCellPointer {
            hover: Some((42.2, -93.2)),
            selected: None,
        };
        let tv = tracks_view();
        let mut i = inputs(&r, &p);
        i.tracks = Some(&tv);
        let card = build(&i, project).card.unwrap();
        assert_eq!(card.title, "D \u{00B7} 62 dBZ peak \u{2191} from 58");
        let texts: Vec<&str> = card.lines.iter().map(|l| l.text.as_str()).collect();
        assert!(
            texts.contains(&"Moving NE 47 km/h, high confidence"),
            "{texts:?}"
        );
        assert!(
            texts.contains(&"Tracked 6 volumes \u{00B7} 25 min"),
            "{texts:?}"
        );
        assert!(
            !texts.iter().any(|t| t.contains("you")),
            "no location: {texts:?}"
        );

        // My location on.
        let mut tv2 = tracks_view();
        tv2.tracks[0].from_reference = Some((262.4, 30.3));
        tv2.tracks[0].approach = Some(crate::storm_cells::track::Approach {
            minutes: 22.4,
            distance_km: 14.2,
        });
        i.tracks = Some(&tv2);
        let card = build(&i, project).card.unwrap();
        let texts: Vec<&str> = card.lines.iter().map(|l| l.text.as_str()).collect();
        assert!(
            texts.contains(&"30 km at 262\u{00B0} from you"),
            "{texts:?}"
        );
        assert!(
            texts.contains(&"Closest to you: 14 km in 22 min"),
            "{texts:?}"
        );
        // Moving away, too far out, or stationary.
        let mut t = tv2.tracks[0].clone();
        t.approach = Some(crate::storm_cells::track::Approach {
            minutes: 0.0,
            distance_km: 30.0,
        });
        assert_eq!(approach_text(&t).as_deref(), Some("Moving away from you"));
        t.approach = Some(crate::storm_cells::track::Approach {
            minutes: 95.0,
            distance_km: 3.0,
        });
        assert_eq!(
            approach_text(&t).as_deref(),
            Some("Closest to you: more than an hour out")
        );
        t.stationary = true;
        assert_eq!(approach_text(&t), None);
        assert_eq!(motion_text(&t), "Stationary");
        t.stationary = false;
        t.confidence = 0.0;
        t.age_volumes = 1;
        assert_eq!(motion_text(&t), "Motion not known yet");
        assert_eq!(age_text(&t), "New this volume");
        t.confidence = 0.25;
        assert_eq!(motion_text(&t), "Moving NE 47 km/h, low confidence");
        t.direction_deg = 350.0;
        t.confidence = 0.5;
        assert_eq!(motion_text(&t), "Moving N 47 km/h, medium confidence");
    }

    #[wasm_bindgen_test]
    fn the_phone_card_folds_motion_into_one_line() {
        let r = result();
        let p = StormCellPointer {
            hover: None,
            selected: Some((42.2, -92.6)),
        };
        let tv = tracks_view();
        let mut i = inputs(&r, &p);
        i.phone = true;
        i.tracks = Some(&tv);
        let card = build(&i, project).card.unwrap();
        assert_eq!(card.title, "B \u{00B7} 55 dBZ peak");
        assert_eq!(
            card.lines[1].text,
            "Moving NE 47 km/h, low confidence \u{00B7} 5 min"
        );
        assert_eq!(card.lines.len(), 3);
        // The longest motion line still fits a 390 px card (it cuts off
        // past about 50 characters).
        let mut t = tv.tracks[1].clone();
        t.confidence = 0.5;
        t.speed_kmh = 128.0;
        t.direction_deg = 315.0;
        t.age_minutes = 118.0;
        let longest = format!("{} \u{00B7} {}", motion_text(&t), short_age_text(&t));
        assert!(longest.chars().count() <= 48, "{longest}");
        t.age_volumes = 1;
        assert_eq!(short_age_text(&t), "new");
    }

    #[wasm_bindgen_test]
    fn the_probe_names_the_track() {
        let (r, p) = (result(), StormCellPointer::default());
        let tv = tracks_view();
        let mut i = inputs(&r, &p);
        i.tracks = Some(&tv);
        assert_eq!(
            probe_line(&i, 42.2, -93.2).as_deref(),
            Some("Cell D 62 dBZ \u{00B7} 493 km\u{00B2}")
        );
    }
}
