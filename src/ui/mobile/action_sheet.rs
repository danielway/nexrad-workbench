//! One row of a phone action sheet (the chip sheet, the long-press sheet):
//! a toned glyph, the verb, and quiet words or inline choices at the right.
//! A thin painter: what the row says and does is the core's.

use crate::core::phone_dock::ChipTone;
use crate::core::phone_range::SHEET_ROW_H;
use crate::ui::theme;
use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, Vec2};

/// The line between rows.
const DIVIDER: Color32 = Color32::from_rgb(31, 36, 46);

/// What a row draws.
pub(super) struct ActionRow<'a> {
    pub icon: &'a str,
    pub tone: ChipTone,
    pub label: &'a str,
    pub sub: Option<&'a str>,
    /// Inline choices (label, in use) and the word after them.
    pub choices: Vec<(&'a str, bool)>,
    pub unit: Option<&'a str>,
    pub current: bool,
    pub enabled: bool,
    pub last: bool,
}

/// What a tap on a row hit.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum RowHit {
    Row,
    Choice(usize),
}

fn tone_color(tone: ChipTone) -> Color32 {
    match tone {
        ChipTone::Live => theme::phone::NOW,
        ChipTone::Loop => theme::LOOP,
        ChipTone::Point => theme::phone::POINT,
    }
}

/// Draw one row across the sheet; returns what a tap hit.
pub(super) fn action_row(ui: &mut egui::Ui, row: &ActionRow) -> Option<RowHit> {
    let w = ui.available_width();
    let sense = if row.enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, SHEET_ROW_H), sense);
    let p = ui.painter();
    if row.current {
        p.rect_filled(
            rect.shrink2(Vec2::new(0.0, 3.0)),
            10.0,
            theme::phone::CONTROL_ON,
        );
    } else if resp.is_pointer_button_down_on() {
        p.rect_filled(
            rect.shrink2(Vec2::new(0.0, 3.0)),
            10.0,
            theme::phone::CONTROL,
        );
    }
    let cy = rect.center().y;
    let dim = |c: Color32| {
        if row.enabled {
            c
        } else {
            c.gamma_multiply(0.45)
        }
    };
    p.text(
        Pos2::new(rect.left() + 10.0, cy),
        Align2::LEFT_CENTER,
        row.icon,
        FontId::proportional(20.0),
        dim(tone_color(row.tone)),
    );
    p.text(
        Pos2::new(rect.left() + 42.0, cy),
        Align2::LEFT_CENTER,
        row.label,
        FontId::proportional(16.0),
        if row.enabled {
            theme::TEXT
        } else {
            theme::TEXT_DIM
        },
    );

    // The right side, laid out from the right edge inwards.
    let small = FontId::proportional(13.0);
    let mut x = rect.right() - 10.0;
    let mut put = |text: &str, color: Color32, font: &FontId| -> Rect {
        let r = p.text(
            Pos2::new(x, cy),
            Align2::RIGHT_CENTER,
            text,
            font.clone(),
            color,
        );
        x = r.left();
        r
    };
    let mut choice_rects = Vec::new();
    if !row.choices.is_empty() {
        if let Some(unit) = row.unit {
            put(unit, theme::TEXT_DIM, &small);
            put(" ", theme::TEXT_DIM, &small);
        }
        let n = row.choices.len();
        for (i, (label, on)) in row.choices.iter().enumerate().rev() {
            let (color, font) = if *on {
                (theme::TEXT, FontId::proportional(14.0))
            } else {
                (theme::TEXT_DIM, small.clone())
            };
            let r = put(label, color, &font);
            choice_rects.push((i, r));
            if i > 0 && i < n {
                put(" \u{00B7} ", theme::TEXT_DIM, &small);
            }
        }
    } else if let Some(sub) = row.sub {
        put(sub, theme::TEXT_DIM, &small);
    }
    if !row.last {
        p.line_segment(
            [
                Pos2::new(rect.left() + 4.0, rect.bottom()),
                Pos2::new(rect.right() - 4.0, rect.bottom()),
            ],
            Stroke::new(1.0, DIVIDER),
        );
    }

    if !resp.clicked() {
        return None;
    }
    let at = resp.interact_pointer_pos();
    let choice = at.and_then(|pos| {
        choice_rects
            .iter()
            .find(|(_, r)| {
                let hit = Rect::from_x_y_ranges(r.left() - 8.0..=r.right() + 8.0, rect.y_range());
                hit.contains(pos)
            })
            .map(|(i, _)| *i)
    });
    Some(choice.map_or(RowHit::Row, RowHit::Choice))
}
