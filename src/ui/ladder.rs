//! The one painter of the tilt ladder (one-instrument §6, §8): a 1:1
//! projection of [`VolumeLadder`] on the volume clock.
//!
//! Rows are angles under Latest; each row's track is the volume's clock.
//! Received cuts are slate, the filling cut periwinkle with the caret on its
//! row, expected cuts dashed grey, archive cuts not on this device hollow
//! slate; the cut on screen gets a white outline; the red now line (pinned)
//! or the white playhead line (at a time) crosses every angle row. A row
//! click is the row's [`crate::core::ladder::TiltPick`]; a hollow bar's
//! click is its fetch. Nothing is decided here.

use crate::core::ladder::{BarMark, Caret, LadderBar, LineKind, MoveState, VolumeLadder};
use crate::core::top_bar::TopBarIntent;
use crate::core::Intent;
use crate::state::AppState;
use crate::ui::theme;
use eframe::egui::{self, Align2, FontId, Pos2, Rect, RichText, Sense, Shape, Stroke, Vec2};

/// The ladder's sizes: the desktop panel's, or the phone sheet's touch size.
pub(super) struct LadderLook {
    /// One row's height (the row is the tap target).
    pub row_h: f32,
    /// A bar's height inside its row.
    pub bar_h: f32,
    /// The least height of the product buttons drawn above the ladder (0 is
    /// the widget's own).
    pub product_h: f32,
}

/// The desktop panel's ladder.
pub(super) const DESKTOP: LadderLook = LadderLook {
    row_h: 19.0,
    bar_h: 8.0,
    product_h: 0.0,
};

/// The phone sheet's ladder: rows tall enough for a finger.
pub(super) const TOUCH: LadderLook = LadderLook {
    row_h: 28.0,
    bar_h: 11.0,
    product_h: 34.0,
};

/// The angle-label column.
const LABEL_W: f32 = 44.0;
/// Room under the rows for the axis labels.
const AXIS_H: f32 = 16.0;

/// The ladder's header lines.
pub(super) fn header(ui: &mut egui::Ui, vm: &VolumeLadder) {
    if let Some(summary) = &vm.summary {
        ui.label(
            RichText::new(summary)
                .size(theme::CAPTION)
                .monospace()
                .color(theme::TEXT_DIM),
        );
    }
    if let Some(status) = &vm.status {
        ui.label(
            RichText::new(status)
                .size(theme::CAPTION)
                .color(theme::TEXT_DIM),
        );
    }
}

/// Draw the ladder at the available width and emit what a click asks for.
pub(super) fn ladder(
    ui: &mut egui::Ui,
    vm: &VolumeLadder,
    state: &mut AppState,
    look: &LadderLook,
) {
    let (row_h, bar_h) = (look.row_h, look.bar_h);
    let width = ui.available_width().max(160.0);
    let height = vm.rows.len() as f32 * row_h + AXIS_H;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
    let painter = ui.painter_at(rect.expand(2.0));

    let track = Rect::from_min_max(
        Pos2::new(rect.left() + LABEL_W, rect.top()),
        Pos2::new(
            rect.right() - 4.0,
            rect.top() + vm.rows.len() as f32 * row_h,
        ),
    );
    let len = vm.length_secs.max(1.0);
    let x_at = |secs: f64| track.left() + (secs / len).clamp(0.0, 1.0) as f32 * track.width();
    let row_y = |i: usize| rect.top() + (i as f32 + 0.5) * row_h;

    // Minute ticks under the angle rows.
    if vm.length_secs > 0.0 {
        let axis_y = track.bottom();
        painter.line_segment(
            [
                Pos2::new(track.left(), axis_y),
                Pos2::new(track.right(), axis_y),
            ],
            Stroke::new(1.0, theme::LINE),
        );
        let mut m = 60.0;
        while m < vm.length_secs {
            let x = x_at(m);
            painter.line_segment(
                [Pos2::new(x, axis_y), Pos2::new(x, axis_y + 3.0)],
                Stroke::new(1.0, theme::LINE),
            );
            m += 60.0;
        }
        let font = FontId::monospace(theme::CAPTION - 2.0);
        painter.text(
            Pos2::new(track.left(), axis_y + 3.0),
            Align2::LEFT_TOP,
            "0:00",
            font.clone(),
            theme::TEXT_FAINT,
        );
        painter.text(
            Pos2::new(track.right(), axis_y + 3.0),
            Align2::RIGHT_TOP,
            &vm.length_label,
            font,
            theme::TEXT_FAINT,
        );
    }

    // Rows: label, click target, hint.
    for (i, row) in vm.rows.iter().enumerate() {
        let r = Rect::from_min_size(
            Pos2::new(rect.left(), rect.top() + i as f32 * row_h),
            Vec2::new(rect.width(), row_h),
        );
        let resp = ui
            .interact(r, ui.id().with(("ladder_row", i)), Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        if resp.hovered() {
            painter.rect_filled(r, 2.0, theme::alpha(theme::HOVER, 160));
        }
        let color = if row.tilt.selected {
            theme::SELECTED
        } else if row.tilt.angle.is_none() || !row.tilt.reaches_product {
            theme::TEXT_FAINT
        } else {
            theme::TEXT_DIM
        };
        let font = if row.tilt.angle.is_some() {
            FontId::monospace(theme::CAPTION)
        } else {
            FontId::proportional(theme::CAPTION)
        };
        painter.text(
            Pos2::new(r.left() + 6.0, r.center().y),
            Align2::LEFT_CENTER,
            &row.tilt.label,
            font,
            color,
        );
        if row.tilt.selected {
            painter.rect_filled(
                Rect::from_min_size(
                    r.left_top() + Vec2::new(0.0, 4.0),
                    Vec2::new(2.0, row_h - 8.0),
                ),
                1.0,
                theme::SELECTED,
            );
        }
        if let Some(repeats) = &row.tilt.repeats {
            painter.text(
                Pos2::new(track.right(), r.center().y),
                Align2::RIGHT_CENTER,
                repeats,
                FontId::proportional(theme::CAPTION - 1.0),
                theme::TEXT_FAINT,
            );
        }
        if let Some(hint) = row.hint {
            painter.text(
                Pos2::new(track.left(), r.center().y),
                Align2::LEFT_CENTER,
                hint,
                FontId::proportional(theme::CAPTION - 1.0),
                theme::TEXT_FAINT,
            );
        }
        let hover = match &row.tilt.note {
            Some(note) => format!("{} \u{00B7} {note}", row.tilt.hover),
            None => row.tilt.hover.clone(),
        };
        if resp.on_hover_text(hover).clicked() {
            state.push_command(Intent::TopBar(TopBarIntent::SetTilt(row.tilt.pick)));
        }
    }

    // Moves: thin connectors from one cut's end to the next cut's start.
    for m in &vm.moves {
        let a = Pos2::new(x_at(m.start), row_y(m.from_row));
        let b = Pos2::new(x_at(m.end), row_y(m.to_row));
        let stroke = match m.state {
            MoveState::Done => Stroke::new(1.0, theme::alpha(theme::ON_DEVICE, 170)),
            MoveState::Active => Stroke::new(1.5, theme::SELECTED),
            MoveState::Ahead => Stroke::new(1.0, theme::alpha(theme::TEXT_FAINT, 70)),
        };
        painter.line_segment([a, b], stroke);
    }

    // Bars.
    for (i, row) in vm.rows.iter().enumerate() {
        for bar in &row.bars {
            paint_bar(
                ui,
                &painter,
                bar,
                x_at(bar.start),
                x_at(bar.end),
                row_y(i),
                bar_h,
                state,
            );
        }
    }

    // The caret on the antenna's row (or between two rows while it moves).
    if let Some(caret) = vm.caret {
        let y = match caret {
            Caret::OnRow(r) => row_y(r),
            Caret::Between { from, to } => (row_y(from) + row_y(to)) / 2.0,
        };
        let x = track.left() - 6.0;
        painter.add(Shape::convex_polygon(
            vec![
                Pos2::new(x - 4.0, y - 4.0),
                Pos2::new(x + 1.0, y),
                Pos2::new(x - 4.0, y + 4.0),
            ],
            theme::SELECTED,
            Stroke::NONE,
        ));
    }

    // Now (red) or the playhead (white), across the angle rows.
    if let Some(line) = &vm.line {
        let x = x_at(line.at);
        let color = match line.kind {
            LineKind::Now => theme::NOW,
            LineKind::Playhead => theme::SEEN,
        };
        painter.line_segment(
            [
                Pos2::new(x, rect.top() + row_h),
                Pos2::new(x, track.bottom()),
            ],
            Stroke::new(1.5, color),
        );
        if let Some(label) = &line.label {
            let font = FontId::monospace(theme::CAPTION - 1.5);
            let galley = painter.layout_no_wrap(label.clone(), font, color);
            let w = galley.size().x;
            // Keep the label off the axis end labels' edges.
            let lx = (x - w / 2.0).clamp(track.left() + 26.0, track.right() - 30.0 - w);
            painter.galley(Pos2::new(lx, track.bottom() + 3.0), galley, color);
        }
    }

    if let Some(empty) = vm.empty {
        ui.label(
            RichText::new(empty)
                .size(theme::CAPTION)
                .color(theme::TEXT_FAINT),
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_bar(
    ui: &mut egui::Ui,
    painter: &egui::Painter,
    bar: &LadderBar,
    x0: f32,
    x1: f32,
    y: f32,
    bar_h: f32,
    state: &mut AppState,
) {
    let r = Rect::from_min_max(
        Pos2::new(x0, y - bar_h / 2.0),
        Pos2::new(x1.max(x0 + 2.0), y + bar_h / 2.0),
    );
    match bar.mark {
        BarMark::Received | BarMark::OnDevice => {
            painter.rect_filled(r, 1.5, theme::ON_DEVICE);
        }
        BarMark::Filling { progress } => {
            painter.rect_filled(r, 1.5, theme::alpha(theme::SELECTED, 50));
            let filled = Rect::from_min_max(
                r.min,
                Pos2::new(r.left() + r.width() * progress, r.bottom()),
            );
            painter.rect_filled(filled, 1.5, theme::SELECTED);
            painter.rect_stroke(
                r,
                1.5,
                Stroke::new(1.0, theme::SELECTED),
                egui::StrokeKind::Inside,
            );
        }
        BarMark::Expected => {
            let s = Stroke::new(1.0, theme::alpha(theme::TEXT_DIM, 150));
            let corners = [
                r.left_top(),
                r.right_top(),
                r.right_bottom(),
                r.left_bottom(),
                r.left_top(),
            ];
            painter.extend(Shape::dashed_line(&corners, s, 3.0, 2.0));
        }
        BarMark::InArchive => {
            painter.rect_stroke(
                r,
                1.5,
                Stroke::new(1.0, theme::ON_DEVICE),
                egui::StrokeKind::Inside,
            );
        }
    }
    if bar.on_screen {
        painter.rect_stroke(
            r.expand(1.5),
            2.0,
            Stroke::new(1.5, theme::SEEN),
            egui::StrokeKind::Outside,
        );
    }
    let id = ui.id().with(("ladder_bar", bar.elevation_number));
    let sense = if bar.fetch.is_some() {
        Sense::click()
    } else {
        Sense::hover()
    };
    let mut resp = ui.interact(r.expand2(Vec2::new(0.0, 3.0)), id, sense);
    if bar.fetch.is_some() {
        resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    }
    let resp = resp.on_hover_text(&bar.hover);
    if resp.clicked() {
        if let Some(fetch) = &bar.fetch {
            state.push_command(fetch.clone());
        }
    }
}
