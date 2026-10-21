//! Paint the storm cell overlay: a 1:1 projection of
//! [`crate::core::storm_cell_view::StormCellsVm`]. Which cells draw, where,
//! and how thick is the core's; this file only turns the view-model into
//! egui shapes with the theme's storm cell tokens.

use crate::core::storm_cell_view::StormCellsVm;
use crate::ui::theme::storm_cells as tokens;
use eframe::egui::{Painter, Pos2, Shape, Stroke};

fn pos(p: [f32; 2]) -> Pos2 {
    Pos2::new(p[0], p[1])
}

/// Group outlines (dashed) and cell footprints (light stroke over a dark
/// halo).
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
}
