//! Paint the storm cell overlay: a 1:1 projection of
//! [`crate::core::storm_cell_view::StormCellsVm`]. Which cells draw, where,
//! and how thick is the core's; this file only turns the view-model into
//! egui shapes with the theme's storm cell tokens.

use crate::core::storm_cell_view::{CardAnchor, CardVm, StormCellsVm};
use crate::ui::theme::storm_cells as tokens;
use eframe::egui::{self, Align2, FontId, Painter, Pos2, Rect, Shape, Stroke, StrokeKind, Vec2};

fn pos(p: [f32; 2]) -> Pos2 {
    Pos2::new(p[0], p[1])
}

/// Envelope rings (faint, thin, solid: each group's and each lone cell's),
/// cell cores (light stroke over a dark halo) and the peak dBZ chips.
pub(crate) fn paint_storm_cells(painter: &Painter, vm: &StormCellsVm) {
    if vm.is_empty() {
        return;
    }
    let style = vm.style;
    for ring in &vm.groups {
        let pts: Vec<Pos2> = ring.iter().copied().map(pos).collect();
        painter.add(Shape::closed_line(
            pts.clone(),
            Stroke::new(style.group_halo_px, tokens::group_halo()),
        ));
        painter.add(Shape::closed_line(
            pts,
            Stroke::new(style.group_px, tokens::group()),
        ));
    }

    let stroke = if vm.provisional {
        tokens::outline_provisional()
    } else {
        tokens::OUTLINE
    };
    for cell in &vm.cells {
        let path: Vec<Pos2> = cell.ring.iter().copied().map(pos).collect();
        painter.add(Shape::closed_line(
            path.clone(),
            Stroke::new(style.halo_px, tokens::halo()),
        ));
        painter.add(Shape::closed_line(
            path,
            Stroke::new(style.outline_px, stroke),
        ));
    }

    let font = FontId::monospace(style.chip_font_px);
    for chip in &vm.chips {
        let rect = Rect::from_min_size(pos(chip.min), Vec2::new(chip.size[0], chip.size[1]));
        let center = rect.center();
        if chip.hollow {
            painter.rect_stroke(
                rect,
                3.0,
                Stroke::new(1.0, tokens::chip_edge_hollow()),
                StrokeKind::Inside,
            );
            for d in [
                Vec2::new(-1.0, 0.0),
                Vec2::new(1.0, 0.0),
                Vec2::new(0.0, -1.0),
                Vec2::new(0.0, 1.0),
            ] {
                painter.text(
                    center + d,
                    Align2::CENTER_CENTER,
                    &chip.text,
                    font.clone(),
                    tokens::text_halo(),
                );
            }
            painter.text(
                center,
                Align2::CENTER_CENTER,
                &chip.text,
                font.clone(),
                tokens::OUTLINE,
            );
        } else {
            painter.rect(
                rect,
                3.0,
                tokens::chip_fill(),
                Stroke::new(1.0, tokens::chip_edge()),
                StrokeKind::Inside,
            );
            painter.text(
                center,
                Align2::CENTER_CENTER,
                &chip.text,
                font.clone(),
                tokens::CHIP_VALUE,
            );
        }
    }
}

/// The hover card (desktop, beside the pointer) or the selected cell's
/// compact card (phone, above the bottom of the map, clear of the corner
/// scrims). Painted on a foreground layer so it sits over the map chrome.
pub(crate) fn paint_card(
    ctx: &egui::Context,
    canvas: Rect,
    card: &CardVm,
    phone: bool,
    bottom_scrims: &[crate::core::canvas::BottomScrim],
) {
    let painter = ctx
        .layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("storm_cell_card"),
        ))
        .with_clip_rect(canvas);
    let body = FontId::monospace(if phone { 12.0 } else { 11.5 });
    let title_font = FontId::proportional(if phone { 13.0 } else { 12.5 });
    let title = painter.layout_no_wrap(card.title.clone(), title_font, tokens::OUTLINE);
    let lines: Vec<_> = card
        .lines
        .iter()
        .map(|l| {
            let color = if l.dim {
                tokens::CARD_DIM
            } else {
                tokens::OUTLINE
            };
            painter.layout_no_wrap(l.text.clone(), body.clone(), color)
        })
        .collect();
    let pad = Vec2::new(10.0, 8.0);
    let gap = 3.0;
    let width = lines
        .iter()
        .map(|g| g.size().x)
        .fold(title.size().x, f32::max);
    let height = title.size().y + lines.iter().map(|g| g.size().y + gap).sum::<f32>();
    let size = [width + 2.0 * pad.x, height + 2.0 * pad.y];
    let bounds = [canvas.min.x, canvas.min.y, canvas.max.x, canvas.max.y];
    let min = match card.anchor {
        CardAnchor::Pointer(at) => {
            crate::core::storm_cell_view::place_card_at_pointer(at, size, bounds)
        }
        CardAnchor::Bottom => {
            let inset =
                crate::core::canvas::caption_bottom_inset(canvas.width(), size[0], bottom_scrims);
            crate::core::storm_cell_view::place_card_at_bottom(size, bounds, inset)
        }
    };
    let rect = Rect::from_min_size(pos(min), Vec2::new(size[0], size[1]));
    painter.rect(
        rect,
        4.0,
        tokens::card_fill(),
        Stroke::new(1.0, tokens::card_edge()),
        StrokeKind::Inside,
    );
    let mut y = rect.min.y + pad.y;
    let x = rect.min.x + pad.x;
    let title_h = title.size().y;
    painter.galley(Pos2::new(x, y), title, tokens::OUTLINE);
    y += title_h + gap;
    for g in lines {
        let h = g.size().y;
        painter.galley(Pos2::new(x, y), g, tokens::OUTLINE);
        y += h + gap;
    }
}
