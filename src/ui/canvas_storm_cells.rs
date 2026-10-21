//! Paint the storm cell overlay: a 1:1 projection of
//! [`crate::core::storm_cell_view::StormCellsVm`]. Which cells draw, where,
//! and how thick is the core's; this file only turns the view-model into
//! egui shapes with the theme's storm cell tokens.

use crate::core::storm_cell_view::StormCellsVm;
use crate::ui::theme::storm_cells as tokens;
use eframe::egui::{Align2, FontId, Painter, Pos2, Rect, Shape, Stroke, StrokeKind, Vec2};

fn pos(p: [f32; 2]) -> Pos2 {
    Pos2::new(p[0], p[1])
}

/// Group outlines (dashed), cell footprints (light stroke over a dark halo)
/// and the peak dBZ chips.
pub(crate) fn paint_storm_cells(painter: &Painter, vm: &StormCellsVm) {
    if vm.is_empty() {
        return;
    }
    let style = vm.style;
    for ring in &vm.groups {
        let mut path: Vec<Pos2> = ring.iter().copied().map(pos).collect();
        if let Some(first) = path.first().copied() {
            path.push(first);
        }
        painter.extend(Shape::dashed_line(
            &path,
            Stroke::new(style.group_px + 2.0, tokens::group_halo()),
            3.0,
            4.0,
        ));
        painter.extend(Shape::dashed_line(
            &path,
            Stroke::new(style.group_px, tokens::group()),
            3.0,
            4.0,
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
