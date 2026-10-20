//! Where the map's chrome sits, and which labels the chrome leaves room for.
//!
//! Everything drawn over the 2D/3D canvas that is not data — the map toolbar
//! (top left), the colour scale (top right), and the bottom tray holding the
//! scale bar (or the 3D compass) and the layer legends — is placed by one
//! pure function, [`layout_map_chrome`], from the canvas size and each
//! overlay's measured size. The shell measures text, asks for the layout, and
//! paints each overlay in the rectangle it gets back.
//!
//! **The tray rule.** The tray runs along the canvas's bottom edge, one row
//! at a time from the bottom up. The scale bar (or compass) comes first and
//! sits at the row's left end; the legends follow, packed from the row's right
//! end inward in the order given (the first legend takes the corner). A legend
//! that does not fit in what is left of a row starts a new row above it, again
//! from the right. A row that would reach up beside the colour scale ends
//! short of it. Nothing overlaps and nothing leaves the canvas: an item that
//! would rise into the toolbar or past the top margin is left out.
//!
//! **Labels.** Map labels (city, state and site labels) never print under
//! chrome: a label whose bounds meet any chrome rectangle, or that runs off
//! the canvas, is hidden rather than nudged ([`label_clear_of_chrome`]). Site
//! labels also give way to city labels: a site's label is hidden when a city
//! label or another site's label sits within [`SITE_LABEL_CLEARANCE`] of it
//! ([`select_site_labels`]); the site's dot always stays.

use crate::geo::{GeoLabelClass, ScreenBounds};

/// Gap between the canvas edge and the colour scale / the tray.
pub(crate) const CHROME_MARGIN: f32 = 12.0;

/// Gap between the map toolbar and the canvas's top-left corner.
pub(crate) const TOOLBAR_INSET: f32 = 10.0;

/// Clear space between two tray items, between tray rows, and between the
/// tray and the colour scale.
pub(crate) const TRAY_GAP: f32 = 6.0;

/// How close (points) a city label or another site label may come to a site
/// label before the site label is hidden.
pub(crate) const SITE_LABEL_CLEARANCE: f32 = 12.0;

/// One item of the bottom tray.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TrayItem {
    /// The 2D km/mi scale bar: the tray's left end.
    ScaleBar,
    /// The 3D compass dial: the scale bar's place in 3D.
    Compass,
    /// The national mosaic stamp with its palette.
    MosaicLegend,
    /// The mPING report categories.
    MpingLegend,
}

impl TrayItem {
    /// Whether the item sits at the row's left end (the scale bar and its 3D
    /// stand-in) rather than packed from the right with the legends.
    fn anchors_left(self) -> bool {
        matches!(self, TrayItem::ScaleBar | TrayItem::Compass)
    }
}

/// A measured overlay size, in points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ChromeSize {
    pub w: f32,
    pub h: f32,
}

/// What is on the canvas this frame, measured.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ChromeInputs<'a> {
    /// Canvas width, points.
    pub width: f32,
    /// Canvas height, points.
    pub height: f32,
    /// The map toolbar (2D only), pinned at the top-left corner.
    pub toolbar: Option<ChromeSize>,
    /// The colour scale, pinned at the top-right corner.
    pub color_scale: Option<ChromeSize>,
    /// The tray's items in order: the scale bar (or compass) first, then the
    /// legends, the first of which takes the bottom-right corner.
    pub tray: &'a [(TrayItem, ChromeSize)],
}

/// Every chrome rectangle, canvas-local (origin at the canvas's top left).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ChromeLayout {
    pub toolbar: Option<ScreenBounds>,
    pub color_scale: Option<ScreenBounds>,
    /// The tray items that fit, in input order.
    pub tray: Vec<(TrayItem, ScreenBounds)>,
}

impl ChromeLayout {
    /// The rectangle of one tray item, if it is on and fit.
    pub(crate) fn tray_rect(&self, item: TrayItem) -> Option<ScreenBounds> {
        self.tray.iter().find(|(i, _)| *i == item).map(|(_, r)| *r)
    }

    /// Every rectangle chrome covers, for labels to keep clear of.
    pub(crate) fn keep_out(&self) -> Vec<ScreenBounds> {
        self.toolbar
            .iter()
            .chain(self.color_scale.iter())
            .copied()
            .chain(self.tray.iter().map(|(_, r)| *r))
            .collect()
    }

    /// The same layout moved by `(dx, dy)` (canvas-local to screen).
    pub(crate) fn offset(&self, dx: f32, dy: f32) -> Self {
        let mv = |r: ScreenBounds| ScreenBounds {
            min_x: r.min_x + dx,
            min_y: r.min_y + dy,
            max_x: r.max_x + dx,
            max_y: r.max_y + dy,
        };
        Self {
            toolbar: self.toolbar.map(mv),
            color_scale: self.color_scale.map(mv),
            tray: self.tray.iter().map(|(i, r)| (*i, mv(*r))).collect(),
        }
    }
}

fn rect(x: f32, y: f32, w: f32, h: f32) -> ScreenBounds {
    ScreenBounds {
        min_x: x,
        min_y: y,
        max_x: x + w,
        max_y: y + h,
    }
}

/// One tray row under construction.
struct Row {
    bottom: f32,
    items: Vec<(usize, TrayItem, ChromeSize)>,
}

impl Row {
    fn height_with(&self, extra: Option<ChromeSize>) -> f32 {
        self.items
            .iter()
            .map(|(_, _, s)| s.h)
            .chain(extra.map(|s| s.h))
            .fold(0.0, f32::max)
    }

    fn width_with(&self, extra: Option<ChromeSize>) -> f32 {
        let widths: Vec<f32> = self
            .items
            .iter()
            .map(|(_, _, s)| s.w)
            .chain(extra.map(|s| s.w))
            .collect();
        widths.iter().sum::<f32>() + TRAY_GAP * widths.len().saturating_sub(1) as f32
    }
}

/// Lay out the map's chrome (see the module doc for the rule).
pub(crate) fn layout_map_chrome(i: &ChromeInputs<'_>) -> ChromeLayout {
    let (w, h) = (i.width.max(0.0), i.height.max(0.0));
    let toolbar = i
        .toolbar
        .map(|s| rect(TOOLBAR_INSET, TOOLBAR_INSET, s.w, s.h));
    let color_scale = i.color_scale.map(|s| {
        let sw = s.w.min((w - 2.0 * CHROME_MARGIN).max(0.0));
        let sh = s.h.min((h - 2.0 * CHROME_MARGIN).max(0.0));
        rect(w - CHROME_MARGIN - sw, CHROME_MARGIN, sw, sh)
    });

    // The right end of a row spanning `top..bottom`: the canvas margin, or
    // short of the colour scale when the row reaches up beside it.
    let right_limit = |top: f32, bottom: f32| -> f32 {
        match color_scale {
            Some(cs) if top < cs.max_y + TRAY_GAP && bottom > cs.min_y - TRAY_GAP => {
                cs.min_x - TRAY_GAP
            }
            _ => w - CHROME_MARGIN,
        }
    };

    let mut rows: Vec<Row> = vec![Row {
        bottom: h - CHROME_MARGIN,
        items: Vec::new(),
    }];
    for (index, &(item, size)) in i.tray.iter().enumerate() {
        let row = rows.last().expect("a row");
        let fits = {
            let top = row.bottom - row.height_with(Some(size));
            CHROME_MARGIN + row.width_with(Some(size)) <= right_limit(top, row.bottom)
        };
        if !fits && !row.items.is_empty() {
            let bottom = row.bottom - row.height_with(None) - TRAY_GAP;
            rows.push(Row {
                bottom,
                items: Vec::new(),
            });
        }
        rows.last_mut()
            .expect("a row")
            .items
            .push((index, item, size));
    }

    let mut placed: Vec<(usize, TrayItem, ScreenBounds)> = Vec::new();
    for row in &rows {
        let row_h = row.height_with(None);
        let limit = right_limit(row.bottom - row_h, row.bottom);
        let span = (limit - CHROME_MARGIN).max(0.0);
        let mut left_x = CHROME_MARGIN;
        let mut right_x = limit;
        for &(index, item, size) in &row.items {
            // An item wider than the whole row (a very narrow canvas) is
            // clipped to it rather than run off the canvas.
            let iw = size.w.min(span);
            let r = if item.anchors_left() {
                let r = rect(left_x, row.bottom - size.h, iw, size.h);
                left_x += iw + TRAY_GAP;
                r
            } else {
                let r = rect(right_x - iw, row.bottom - size.h, iw, size.h);
                right_x -= iw + TRAY_GAP;
                r
            };
            placed.push((index, item, r));
        }
    }

    // Nothing rises past the top margin or into the toolbar or the scale.
    let blocked = |r: ScreenBounds| {
        r.min_y < CHROME_MARGIN
            || r.min_x < 0.0
            || r.max_x > w
            || toolbar.is_some_and(|t| t.expanded(TRAY_GAP).intersects(r))
            || color_scale.is_some_and(|c| c.intersects(r))
    };
    placed.sort_by_key(|(index, _, _)| *index);
    let tray = placed
        .into_iter()
        .filter(|(_, _, r)| r.is_valid() && !blocked(*r))
        .map(|(_, item, r)| (item, r))
        .collect();

    ChromeLayout {
        toolbar,
        color_scale,
        tray,
    }
}

// ---------------------------------------------------------------------------
// The colour scale's size
// ---------------------------------------------------------------------------

/// The colour scale's gradient bar width.
pub(crate) const COLOR_SCALE_BAR_W: f32 = 14.0;
/// The colour scale's inner padding.
pub(crate) const COLOR_SCALE_PAD: f32 = 6.0;
/// Room for the unit caption over the bar.
pub(crate) const COLOR_SCALE_TITLE_H: f32 = 16.0;
/// Room for the tick labels left of the bar.
pub(crate) const COLOR_SCALE_TICK_W: f32 = 30.0;

/// The colour scale's scrim size on a canvas `canvas_h` tall: the bar runs
/// 100–300 pt, leaving room under it for the tray.
pub(crate) fn color_scale_size(canvas_h: f32) -> ChromeSize {
    let pad = COLOR_SCALE_PAD;
    let bar = (canvas_h - 2.0 * CHROME_MARGIN - COLOR_SCALE_TITLE_H - 2.0 * pad - 20.0)
        .clamp(100.0, 300.0);
    ChromeSize {
        w: COLOR_SCALE_TICK_W + 4.0 + COLOR_SCALE_BAR_W + pad * 2.0,
        h: pad + COLOR_SCALE_TITLE_H + bar + pad + 4.0,
    }
}

/// The gradient bar's height inside a colour-scale scrim `scrim_h` tall.
pub(crate) fn color_scale_bar_height(scrim_h: f32) -> f32 {
    (scrim_h - COLOR_SCALE_PAD * 2.0 - COLOR_SCALE_TITLE_H - 4.0).max(0.0)
}

// ---------------------------------------------------------------------------
// Labels around the chrome
// ---------------------------------------------------------------------------

/// Whether a map label at `bounds` may print: wholly on the canvas
/// (`viewport`) and under no chrome rectangle. Labels that fail are hidden,
/// not nudged — a nudged city label no longer marks its city.
pub(crate) fn label_clear_of_chrome(
    bounds: ScreenBounds,
    keep_out: &[ScreenBounds],
    viewport: ScreenBounds,
) -> bool {
    bounds.is_valid() && viewport.contains(bounds) && !keep_out.iter().any(|k| k.intersects(bounds))
}

/// A radar site's label, measured where it would print.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SiteLabelCandidate {
    pub bounds: ScreenBounds,
    /// The site the radar is showing: its label always prints.
    pub current: bool,
}

/// Which site labels print, by index into `sites`.
///
/// The current site's label always prints. Every other site's label prints
/// when it is clear of the chrome and on the canvas
/// ([`label_clear_of_chrome`]) and no city label (from `map_labels`, the
/// labels actually painted this frame) or already-shown site label lies
/// within [`SITE_LABEL_CLEARANCE`] of it. Sites are taken in input order, so
/// the result is deterministic.
pub(crate) fn select_site_labels(
    sites: &[SiteLabelCandidate],
    map_labels: &[(GeoLabelClass, ScreenBounds)],
    keep_out: &[ScreenBounds],
    viewport: ScreenBounds,
) -> Vec<bool> {
    let city_labels: Vec<ScreenBounds> = map_labels
        .iter()
        .filter(|(class, _)| {
            matches!(
                class,
                GeoLabelClass::CityMajor | GeoLabelClass::CityMedium | GeoLabelClass::CitySmall
            )
        })
        .map(|(_, b)| *b)
        .collect();
    let mut shown = vec![false; sites.len()];
    let mut occupied: Vec<ScreenBounds> = Vec::new();
    for (i, s) in sites.iter().enumerate() {
        if s.current {
            shown[i] = true;
            occupied.push(s.bounds);
        }
    }
    for (i, s) in sites.iter().enumerate() {
        if s.current {
            continue;
        }
        let near = s.bounds.expanded(SITE_LABEL_CLEARANCE);
        let crowded = city_labels
            .iter()
            .chain(occupied.iter())
            .any(|o| near.intersects(*o));
        if !crowded && label_clear_of_chrome(s.bounds, keep_out, viewport) {
            shown[i] = true;
            occupied.push(s.bounds);
        }
    }
    shown
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    // Measured sizes from the running app (points).
    const SCALE_BAR: ChromeSize = ChromeSize { w: 112.0, h: 44.0 };
    const MOSAIC: ChromeSize = ChromeSize { w: 196.0, h: 41.0 };
    const MPING: ChromeSize = ChromeSize { w: 104.0, h: 48.0 };
    const TOOLBAR: ChromeSize = ChromeSize { w: 52.0, h: 26.0 };

    /// The phone canvas (390 pt portrait), a phone in landscape (844 pt
    /// wide, short), and a desktop canvas, with their heights.
    const CANVASES: [(f32, f32); 3] = [(390.0, 590.0), (844.0, 300.0), (1600.0, 860.0)];

    fn inputs<'a>(w: f32, h: f32, tray: &'a [(TrayItem, ChromeSize)]) -> ChromeInputs<'a> {
        ChromeInputs {
            width: w,
            height: h,
            toolbar: Some(TOOLBAR),
            color_scale: Some(color_scale_size(h)),
            tray,
        }
    }

    /// Every rectangle on the canvas, inside the margin, none overlapping.
    fn assert_sound(layout: &ChromeLayout, w: f32, h: f32) {
        let all = layout.keep_out();
        for r in &all {
            assert!(r.is_valid(), "{r:?}");
            assert!(
                r.min_x >= 0.0 && r.min_y >= 0.0 && r.max_x <= w && r.max_y <= h,
                "{r:?} leaves the {w}x{h} canvas"
            );
        }
        for (a, ra) in all.iter().enumerate() {
            for rb in all.iter().skip(a + 1) {
                assert!(!ra.intersects(*rb), "{ra:?} overlaps {rb:?} at {w}x{h}");
            }
        }
    }

    fn legends(n: usize) -> Vec<(TrayItem, ChromeSize)> {
        // Three legends: the two real ones and a second mPING-sized one to
        // stand for a third layer.
        let all = [
            (TrayItem::MosaicLegend, MOSAIC),
            (TrayItem::MpingLegend, MPING),
            (TrayItem::MpingLegend, ChromeSize { w: 150.0, h: 30.0 }),
        ];
        all[..n].to_vec()
    }

    #[wasm_bindgen_test]
    fn nothing_overlaps_or_leaves_the_canvas_at_any_width() {
        for (w, h) in CANVASES {
            for n in 0..=3 {
                let mut tray = vec![(TrayItem::ScaleBar, SCALE_BAR)];
                tray.extend(legends(n));
                let layout = layout_map_chrome(&inputs(w, h, &tray));
                assert_sound(&layout, w, h);
                assert_eq!(
                    layout.tray.len(),
                    n + 1,
                    "every item fits at {w}x{h} with {n} legends: {layout:?}"
                );
            }
        }
    }

    #[wasm_bindgen_test]
    fn the_scale_bar_takes_the_bottom_left_corner() {
        for (w, h) in CANVASES {
            let tray = [(TrayItem::ScaleBar, SCALE_BAR)];
            let r = layout_map_chrome(&inputs(w, h, &tray))
                .tray_rect(TrayItem::ScaleBar)
                .unwrap();
            assert_eq!((r.min_x, r.max_y), (CHROME_MARGIN, h - CHROME_MARGIN));
        }
    }

    #[wasm_bindgen_test]
    fn the_first_legend_takes_the_bottom_right_corner() {
        for (w, h) in CANVASES {
            let mut tray = vec![(TrayItem::ScaleBar, SCALE_BAR)];
            tray.extend(legends(1));
            let layout = layout_map_chrome(&inputs(w, h, &tray));
            let r = layout.tray_rect(TrayItem::MosaicLegend).unwrap();
            // On the short 844 pt (landscape) canvas the colour scale reaches
            // down beside the tray, so the corner is just left of it.
            let cs = layout.color_scale.unwrap();
            let right = if cs.max_y + TRAY_GAP > r.min_y {
                cs.min_x - TRAY_GAP
            } else {
                w - CHROME_MARGIN
            };
            assert_eq!((r.max_x, r.max_y), (right, h - CHROME_MARGIN));
        }
        // The phone and desktop canvases have the true corner.
        for (w, h) in [CANVASES[0], CANVASES[2]] {
            let tray = legends(1);
            let r = layout_map_chrome(&inputs(w, h, &tray))
                .tray_rect(TrayItem::MosaicLegend)
                .unwrap();
            assert_eq!(r.max_x, w - CHROME_MARGIN);
        }
    }

    #[wasm_bindgen_test]
    fn on_a_phone_the_second_legend_wraps_above_the_first() {
        let (w, h) = CANVASES[0];
        let mut tray = vec![(TrayItem::ScaleBar, SCALE_BAR)];
        tray.extend(legends(2));
        let layout = layout_map_chrome(&inputs(w, h, &tray));
        let mosaic = layout.tray_rect(TrayItem::MosaicLegend).unwrap();
        let mping = layout.tray_rect(TrayItem::MpingLegend).unwrap();
        let scale = layout.tray_rect(TrayItem::ScaleBar).unwrap();
        // Scale bar and mosaic share the bottom row; mPING sits a row up,
        // right-aligned, a gap above the taller of the two.
        assert_eq!(scale.max_y, mosaic.max_y);
        assert_eq!(mping.max_x, w - CHROME_MARGIN);
        assert_eq!(mping.max_y, h - CHROME_MARGIN - SCALE_BAR.h - TRAY_GAP);
    }

    #[wasm_bindgen_test]
    fn on_a_desktop_the_legends_share_one_row() {
        let (w, h) = CANVASES[2];
        let mut tray = vec![(TrayItem::ScaleBar, SCALE_BAR)];
        tray.extend(legends(3));
        let layout = layout_map_chrome(&inputs(w, h, &tray));
        assert!(layout
            .tray
            .iter()
            .all(|(_, r)| r.max_y == h - CHROME_MARGIN));
        // Packed from the right in order: mosaic in the corner, mPING left of it.
        let mosaic = layout.tray_rect(TrayItem::MosaicLegend).unwrap();
        let mping = layout.tray_rect(TrayItem::MpingLegend).unwrap();
        assert_eq!(mping.max_x, mosaic.min_x - TRAY_GAP);
    }

    #[wasm_bindgen_test]
    fn a_row_beside_the_colour_scale_ends_short_of_it() {
        // A short canvas: the scale reaches down to the tray's second row.
        let (w, h) = (390.0, 260.0);
        let mut tray = vec![(TrayItem::ScaleBar, SCALE_BAR)];
        tray.extend(legends(2));
        let layout = layout_map_chrome(&inputs(w, h, &tray));
        assert_sound(&layout, w, h);
        let cs = layout.color_scale.unwrap();
        for (_, r) in &layout.tray {
            if r.min_y < cs.max_y + TRAY_GAP {
                assert!(r.max_x <= cs.min_x - TRAY_GAP, "{r:?} beside {cs:?}");
            }
        }
    }

    #[wasm_bindgen_test]
    fn items_that_would_rise_into_the_toolbar_are_left_out() {
        let (w, h) = (200.0, 120.0);
        let mut tray = vec![(TrayItem::ScaleBar, SCALE_BAR)];
        tray.extend(legends(3));
        let layout = layout_map_chrome(&ChromeInputs {
            color_scale: None,
            ..inputs(w, h, &tray)
        });
        assert_sound(&layout, w, h);
        assert!(layout.tray.len() < tray.len());
        assert_eq!(layout.tray[0].0, TrayItem::ScaleBar);
    }

    #[wasm_bindgen_test]
    fn an_item_wider_than_the_canvas_is_clipped_to_it() {
        let tray = [(TrayItem::MosaicLegend, ChromeSize { w: 500.0, h: 40.0 })];
        let layout = layout_map_chrome(&ChromeInputs {
            width: 300.0,
            height: 400.0,
            toolbar: None,
            color_scale: None,
            tray: &tray,
        });
        assert_sound(&layout, 300.0, 400.0);
        let r = layout.tray_rect(TrayItem::MosaicLegend).unwrap();
        assert_eq!((r.min_x, r.max_x), (CHROME_MARGIN, 300.0 - CHROME_MARGIN));
    }

    #[wasm_bindgen_test]
    fn the_compass_takes_the_scale_bars_place_in_3d() {
        let (w, h) = CANVASES[2];
        let tray = [
            (TrayItem::Compass, ChromeSize { w: 64.0, h: 64.0 }),
            (TrayItem::MosaicLegend, MOSAIC),
        ];
        let layout = layout_map_chrome(&ChromeInputs {
            toolbar: None,
            ..inputs(w, h, &tray)
        });
        let r = layout.tray_rect(TrayItem::Compass).unwrap();
        assert_eq!((r.min_x, r.max_y), (CHROME_MARGIN, h - CHROME_MARGIN));
    }

    #[wasm_bindgen_test]
    fn the_colour_scale_keeps_its_corner() {
        for (w, h) in CANVASES {
            let cs = layout_map_chrome(&inputs(w, h, &[])).color_scale.unwrap();
            assert_eq!((cs.max_x, cs.min_y), (w - CHROME_MARGIN, CHROME_MARGIN));
            assert_eq!(color_scale_bar_height(cs.max_y - cs.min_y), {
                let s = color_scale_size(h);
                s.h - 2.0 * COLOR_SCALE_PAD - COLOR_SCALE_TITLE_H - 4.0
            });
        }
    }

    // ---- labels ----

    fn label(x: f32, y: f32, w: f32, h: f32) -> ScreenBounds {
        rect(x, y, w, h)
    }

    const PHONE: ScreenBounds = ScreenBounds {
        min_x: 0.0,
        min_y: 0.0,
        max_x: 390.0,
        max_y: 590.0,
    };

    /// A site label as the shell measures it: 10 pt text 6 pt right of the
    /// dot, centred 2 pt above it.
    fn site_at(x: f32, y: f32, current: bool) -> SiteLabelCandidate {
        SiteLabelCandidate {
            bounds: label(x + 6.0, y - 2.0 - 6.5, 26.0, 13.0),
            current,
        }
    }

    /// A city label as the geo pass measures it: 11 pt text 4.5 pt right of
    /// the dot, centred 2 pt above it.
    fn city_at(x: f32, y: f32, w: f32) -> (GeoLabelClass, ScreenBounds) {
        (
            GeoLabelClass::CityMajor,
            label(x + 4.5, y - 2.0 - 7.5, w, 15.0),
        )
    }

    #[wasm_bindgen_test]
    fn kfsd_gives_way_to_sioux_falls() {
        // From the Iowa screenshot (390 pt phone): KFSD's dot at (22, 172),
        // Sioux Falls's dot just under it at (22, 175).
        let sioux_falls = city_at(22.0, 175.0, 70.0);
        let shown = select_site_labels(&[site_at(22.0, 172.0, false)], &[sioux_falls], &[], PHONE);
        assert_eq!(shown, vec![false]);
    }

    #[wasm_bindgen_test]
    fn koax_gives_way_to_omaha() {
        // KOAX's dot at (45, 359), Omaha's at (72, 362): KOAX's label ran
        // into "Omaha".
        let omaha = city_at(72.0, 362.0, 46.0);
        let shown = select_site_labels(&[site_at(45.0, 359.0, false)], &[omaha], &[], PHONE);
        assert_eq!(shown, vec![false]);
    }

    #[wasm_bindgen_test]
    fn a_site_label_with_room_prints() {
        // KMPX at the top, nothing within 12 pt.
        let lincoln = city_at(24.0, 400.0, 50.0);
        let shown = select_site_labels(&[site_at(218.0, 68.0, false)], &[lincoln], &[], PHONE);
        assert_eq!(shown, vec![true]);
    }

    #[wasm_bindgen_test]
    fn twelve_points_is_the_line() {
        let city = (GeoLabelClass::CityMedium, label(100.0, 100.0, 40.0, 13.0));
        // Label 13 pt to the right of the city label: prints.
        let clear = SiteLabelCandidate {
            bounds: label(153.0, 100.0, 26.0, 13.0),
            current: false,
        };
        // 11 pt: hidden.
        let close = SiteLabelCandidate {
            bounds: label(151.0, 100.0, 26.0, 13.0),
            current: false,
        };
        assert_eq!(
            select_site_labels(&[clear], &[city], &[], PHONE),
            vec![true]
        );
        assert_eq!(
            select_site_labels(&[close], &[city], &[], PHONE),
            vec![false]
        );
    }

    #[wasm_bindgen_test]
    fn only_city_labels_crowd_a_site_label() {
        // "Iowa" across KDMX's neighbour does not hide it; a city would.
        let site = site_at(200.0, 300.0, false);
        let state = (GeoLabelClass::State, label(190.0, 290.0, 60.0, 18.0));
        assert_eq!(
            select_site_labels(&[site], &[state], &[], PHONE),
            vec![true]
        );
        let city = (GeoLabelClass::CitySmall, state.1);
        assert_eq!(
            select_site_labels(&[site], &[city], &[], PHONE),
            vec![false]
        );
    }

    #[wasm_bindgen_test]
    fn crowded_site_labels_keep_the_first_and_the_current() {
        // KMPX and TMSP side by side at the top of the screenshot.
        let kmpx = site_at(218.0, 68.0, false);
        let tmsp = site_at(250.0, 62.0, false);
        assert_eq!(
            select_site_labels(&[kmpx, tmsp], &[], &[], PHONE),
            vec![true, false]
        );
        // The current site's label prints even beside a city label, and
        // pushes the other one out.
        let kdmx = site_at(214.0, 330.0, true);
        let des_moines = city_at(218.0, 342.0, 70.0);
        let near = site_at(230.0, 320.0, false);
        assert_eq!(
            select_site_labels(&[near, kdmx], &[des_moines], &[], PHONE),
            vec![false, true]
        );
    }

    #[wasm_bindgen_test]
    fn labels_under_the_colour_scale_or_off_the_canvas_are_hidden() {
        let layout = layout_map_chrome(&inputs(390.0, 590.0, &[]));
        let keep_out = layout.keep_out();
        // The "KA…" label in the screenshot: a site just inside the right
        // edge, its label under the scale and off the canvas.
        let karx = site_at(372.0, 152.0, false);
        assert_eq!(
            select_site_labels(&[karx], &[], &keep_out, PHONE),
            vec![false]
        );
        assert!(!label_clear_of_chrome(karx.bounds, &[], PHONE));
        // A city label under the scale.
        let cs = layout.color_scale.unwrap();
        let under = label(cs.min_x - 10.0, 200.0, 40.0, 13.0);
        assert!(!label_clear_of_chrome(under, &keep_out, PHONE));
        // A city label in the open.
        assert!(label_clear_of_chrome(
            label(100.0, 200.0, 40.0, 13.0),
            &keep_out,
            PHONE
        ));
    }

    #[wasm_bindgen_test]
    fn labels_under_the_tray_are_hidden() {
        let tray = [
            (TrayItem::ScaleBar, SCALE_BAR),
            (TrayItem::MosaicLegend, MOSAIC),
        ];
        let layout = layout_map_chrome(&inputs(390.0, 590.0, &tray));
        let mosaic = layout.tray_rect(TrayItem::MosaicLegend).unwrap();
        // "Topeka" under the mosaic legend.
        let topeka = label(mosaic.min_x + 4.0, mosaic.min_y + 6.0, 46.0, 15.0);
        assert!(!label_clear_of_chrome(topeka, &layout.keep_out(), PHONE));
    }
}
