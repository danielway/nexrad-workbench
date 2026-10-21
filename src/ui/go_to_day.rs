//! The calendar, "Go to a day": a thin projection of
//! [`crate::core::calendar::calendar_vm`].
//!
//! On the phone, a bottom sheet in portrait and a right sheet in landscape
//! (phone timeline §2); on the desktop, a popover above the transport clock's
//! calendar button, with the clock's time field under the grid for an exact
//! time. Both are on the dialog component and the Esc stack. The month grid shades each day by its storm
//! activity, underlines the days held on device, outlines today in the now
//! red and dims the days after it. Tapping the month name opens the years,
//! a year its months, a month its grid; a horizontal swipe on the grid moves
//! a month. A tapped day emits [`Intent::GoToDay`]. Every decision (levels,
//! flags, clamping, the swipe rule) is the core's.

use super::layout::{Layer, LayerKind, LayoutCtx};
use crate::core::calendar::{
    calendar_sheet_rect, calendar_vm, swipe_nav, CalendarBody, CalendarFacts, CalendarNav, DayCell,
    MonthVm, YearVm, YearsVm, WEEKDAYS,
};
use crate::core::dialogs::{DialogId, DialogIntent};
use crate::core::Intent;
use crate::data::SiteId;
use crate::state::AppState;
use crate::ui::dialog::{Popover, Sheet, Width};
use crate::ui::theme;
use crate::ui::time_format::ZoneClock;
use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, Vec2};

/// A day cell, as the design draws it.
const CELL_W: f32 = 44.0;
const CELL_H: f32 = 32.0;
/// A grid row: the cell and the room for its underline.
const ROW_H: f32 = 40.0;
/// The tightest a grid row gets (a six-week month in a landscape sheet).
const MIN_ROW_H: f32 = 34.0;
/// The legend row and the space above it.
const LEGEND_H: f32 = 24.0;
/// The on-device underline under a cell.
const UNDERLINE_H: f32 = 2.0;
/// The header row (arrows and the month name).
const HEADER_H: f32 = 32.0;
/// A month or year cell in the year views.
const BIG_CELL_H: f32 = 44.0;
/// The strongest a day's shade gets, so its number stays legible.
const MAX_SHADE: f32 = 0.95;

pub(in crate::ui) struct GoToDayLayer;

/// Where the desktop's calendar button stores its rect for the popover.
pub(in crate::ui) fn anchor_id() -> egui::Id {
    egui::Id::new("go_to_day_anchor")
}

impl Layer for GoToDayLayer {
    fn kind(&self) -> LayerKind {
        LayerKind::Modal
    }
    fn z_order(&self) -> i32 {
        95
    }
    fn visible(&self, ctx: &LayoutCtx) -> bool {
        ctx.chrome.go_to_day.is_some()
    }
    fn render(&self, ctx: &mut LayoutCtx) {
        draw(ctx);
    }
}

fn draw(ctx: &mut LayoutCtx) {
    let Some(sheet) = ctx.chrome.go_to_day else {
        return;
    };
    let site = SiteId::new(ctx.state.viz_state.site_id.clone());
    let held: Vec<(f64, f64)> = ctx
        .timeline
        .scans
        .time_ranges()
        .iter()
        .map(|r| (r.start, r.end))
        .collect();
    let facts = CalendarFacts {
        book: &ctx.timeline.activity,
        site: &site,
        held: &held,
        now: ctx.state.frame_now.secs(),
    };
    let zone = ZoneClock {
        use_local: ctx.state.use_local_time,
    };
    let vm = calendar_vm(
        &sheet,
        ctx.playback.state.playback_position(),
        &facts,
        &zone,
    );

    let egui_ctx = ctx.ctx;
    let state = &mut *ctx.state;
    let body = |ui: &mut egui::Ui, room: f32, state: &mut AppState| match &vm.body {
        CalendarBody::Month(m) => month(ui, m, &vm.legend, state),
        CalendarBody::Year(y) => year(ui, y, state),
        CalendarBody::Years(y) => years(ui, y, room, state),
    };
    let out = if state.is_mobile {
        let viewport = egui_ctx.input(|i| i.viewport_rect());
        let r = calendar_sheet_rect(
            viewport.width(),
            viewport.height(),
            ctx.chrome.device_layout.is_landscape_phone(),
            super::mobile::safe_area_insets(),
        );
        let rect = Rect::from_min_size(Pos2::new(r.x, r.y), Vec2::new(r.w, r.h));
        Sheet::new(DialogId::GoToDay, vm.title, rect)
            .trailing(&vm.site)
            .show(egui_ctx, |ui, room| body(ui, room, state))
    } else {
        let anchor = egui_ctx
            .data(|d| d.get_temp::<Rect>(anchor_id()))
            .unwrap_or_else(|| {
                let vp = egui_ctx.input(|i| i.viewport_rect());
                Rect::from_min_size(Pos2::new(vp.left() + 12.0, vp.bottom() - 80.0), Vec2::ZERO)
            });
        let playhead = ctx.playback.state.playback_position();
        let now = state.frame_now.secs();
        Popover::new(DialogId::GoToDay, anchor)
            .title(vm.title)
            .subtitle(vm.site.clone())
            .width(Width::Narrow)
            .show(egui_ctx, |ui, room| {
                body(ui, room - TIME_ROW_H, state);
                time_row(ui, playhead, now, state);
            })
    };
    if out.close {
        state.push_command(Intent::Dialog(DialogIntent::Close(DialogId::GoToDay)));
    }
}

/// The desktop's time row under the grid.
const TIME_ROW_H: f32 = 40.0;

/// "Or type a time": the transport clock's field, so a full date and time
/// (or a time on the playhead's day) lands exactly there and closes the
/// calendar.
fn time_row(ui: &mut egui::Ui, playhead: f64, now: f64, state: &mut AppState) {
    ui.add_space(8.0);
    ui.separator();
    ui.horizontal(|ui| {
        ui.add(
            egui::Label::new(
                egui::RichText::new("Or type a time")
                    .size(theme::CAPTION)
                    .color(theme::TEXT_DIM),
            )
            .selectable(false),
        );
        let look = crate::ui::time_field::TimeFieldLook {
            color: theme::SEEN,
            size: theme::BODY,
            hover: "Type or paste a date and time, then Enter",
        };
        let typed = crate::ui::time_field::time_field(
            ui,
            "go_to_day_time",
            playhead,
            now,
            state.use_local_time,
            &look,
        );
        if let Some(ts) = typed {
            state.push_command(Intent::GoToTime(ts));
        }
    });
}

fn nav(state: &mut AppState, to: CalendarNav) {
    state.push_command(Intent::Calendar(to));
}

/// The header: ‹ title ›, the title tappable when `title_to` is set.
fn header(
    ui: &mut egui::Ui,
    title: &str,
    prev: Option<CalendarNav>,
    next: Option<CalendarNav>,
    title_to: Option<CalendarNav>,
    state: &mut AppState,
) {
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, HEADER_H), Sense::hover());
    let arrow = |ui: &mut egui::Ui, r: Rect, glyph: &str, to: Option<CalendarNav>| {
        let resp = ui.interact(r, ui.id().with(glyph), Sense::click());
        let color = if to.is_some() {
            theme::TEXT
        } else {
            theme::TEXT_FAINT
        };
        ui.painter().text(
            r.center(),
            Align2::CENTER_CENTER,
            glyph,
            FontId::proportional(theme::TITLE),
            color,
        );
        resp.clicked().then_some(to).flatten()
    };
    let left = Rect::from_min_size(rect.min, Vec2::new(HEADER_H + 8.0, HEADER_H));
    let right = Rect::from_min_size(
        Pos2::new(rect.right() - HEADER_H - 8.0, rect.top()),
        Vec2::new(HEADER_H + 8.0, HEADER_H),
    );
    let mid = Rect::from_min_max(
        Pos2::new(left.right(), rect.top()),
        Pos2::new(right.left(), rect.bottom()),
    );
    let mut picked = arrow(ui, left, "\u{2039}", prev);
    picked = picked.or(arrow(ui, right, "\u{203A}", next));
    let resp = ui.interact(mid, ui.id().with("calendar_title"), Sense::click());
    ui.painter().text(
        mid.center(),
        Align2::CENTER_CENTER,
        title,
        FontId::proportional(theme::BODY),
        theme::TEXT,
    );
    if resp.clicked() {
        picked = picked.or(title_to);
    }
    if let Some(to) = picked {
        nav(state, to);
    }
}

fn month(ui: &mut egui::Ui, vm: &MonthVm, legend: &[&str; 3], state: &mut AppState) {
    header(
        ui,
        &vm.title,
        vm.prev.map(CalendarNav::Month),
        vm.next.map(CalendarNav::Month),
        Some(CalendarNav::Years),
        state,
    );

    let w = ui.available_width();
    let col_w = w / 7.0;
    let (days_row, _) = ui.allocate_exact_size(Vec2::new(w, 18.0), Sense::hover());
    for (i, d) in WEEKDAYS.iter().enumerate() {
        ui.painter().text(
            Pos2::new(
                days_row.left() + col_w * (i as f32 + 0.5),
                days_row.center().y,
            ),
            Align2::CENTER_CENTER,
            *d,
            FontId::proportional(theme::CAPTION),
            theme::TEXT_FAINT,
        );
    }

    // Six weeks in a landscape sheet: rows give up their gap before the
    // legend leaves the sheet.
    let rows = vm.weeks.len() as f32;
    let row_h = ((ui.available_height() - LEGEND_H) / rows).clamp(MIN_ROW_H, ROW_H);
    let cell_h = CELL_H.min(row_h - 6.0);
    let (grid, resp) = ui.allocate_exact_size(Vec2::new(w, row_h * rows), Sense::click_and_drag());
    let cell_w = CELL_W.min(col_w - 4.0).max(16.0);
    let cell_rect = |r: usize, c: usize| {
        let center = Pos2::new(
            grid.left() + col_w * (c as f32 + 0.5),
            grid.top() + row_h * r as f32 + cell_h / 2.0 + 2.0,
        );
        Rect::from_center_size(center, Vec2::new(cell_w, cell_h))
    };
    let painter = ui.painter_at(grid);
    for (r, week) in vm.weeks.iter().enumerate() {
        for (c, cell) in week.iter().enumerate() {
            paint_day(&painter, cell_rect(r, c), cell);
        }
    }

    // A tap goes to a day; a horizontal swipe moves a month.
    if resp.clicked() {
        if let Some(p) = resp.interact_pointer_pos() {
            let hit = vm.weeks.iter().enumerate().find_map(|(r, week)| {
                week.iter()
                    .enumerate()
                    .find(|(c, _)| cell_rect(r, *c).expand(2.0).contains(p))
                    .map(|(_, cell)| cell)
            });
            if let Some(cell) = hit.filter(|c| c.tappable()) {
                state.push_command(Intent::GoToDay(cell.day));
            }
        }
    }
    if resp.drag_stopped() {
        let delta = ui.input(|i| {
            i.pointer
                .press_origin()
                .zip(i.pointer.interact_pos())
                .map(|(a, b)| b - a)
        });
        if let Some(to) = delta.and_then(|d| swipe_nav(vm, d.x, d.y)) {
            nav(state, to);
        }
    }

    ui.add_space(6.0);
    legend_row(ui, legend);
}

fn paint_day(painter: &egui::Painter, rect: Rect, cell: &DayCell) {
    if !cell.in_month {
        return;
    }
    let text_color = if cell.is_future {
        theme::TEXT_FAINT
    } else if cell.unknown || cell.outage {
        theme::TEXT_DIM
    } else {
        theme::TEXT
    };
    if cell.level > 0.0 {
        painter.rect_filled(rect, 6.0, shade(cell.level));
    } else if cell.unknown {
        // Not known yet (newer than the index): a hairline, never a quiet
        // fill.
        painter.rect_stroke(
            rect,
            6.0,
            Stroke::new(1.0, theme::alpha(theme::LINE, 200)),
            egui::StrokeKind::Inside,
        );
    }
    if cell.is_today {
        painter.rect_stroke(
            rect,
            6.0,
            Stroke::new(1.5, theme::NOW),
            egui::StrokeKind::Inside,
        );
    }
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        &cell.label,
        FontId::proportional(theme::BODY),
        text_color,
    );
    if cell.on_device {
        let y = rect.bottom() + 2.0;
        painter.rect_filled(
            Rect::from_min_max(
                Pos2::new(rect.left() + 6.0, y),
                Pos2::new(rect.right() - 6.0, y + UNDERLINE_H),
            ),
            0.0,
            theme::ON_DEVICE,
        );
    }
}

fn shade(level: f32) -> Color32 {
    theme::alpha(
        theme::STORM,
        (level.clamp(0.0, 1.0) * MAX_SHADE * 255.0).round() as u8,
    )
}

fn legend_row(ui: &mut egui::Ui, legend: &[&str; 3]) {
    let w = ui.available_width();
    let (row, _) = ui.allocate_exact_size(Vec2::new(w, 16.0), Sense::hover());
    let painter = ui.painter_at(row);
    let font = FontId::proportional(theme::CAPTION);
    let y = row.center().y;
    let mut x = row.left();
    for (i, word) in legend.iter().enumerate() {
        match i {
            0 => painter.rect_filled(
                Rect::from_center_size(Pos2::new(x + 5.0, y), Vec2::splat(10.0)),
                2.0,
                shade(1.0),
            ),
            1 => painter.rect_filled(
                Rect::from_min_size(Pos2::new(x, y - 1.0), Vec2::new(14.0, UNDERLINE_H)),
                0.0,
                theme::ON_DEVICE,
            ),
            _ => painter.rect_stroke(
                Rect::from_center_size(Pos2::new(x + 5.0, y), Vec2::splat(10.0)),
                2.0,
                Stroke::new(1.5, theme::NOW),
                egui::StrokeKind::Inside,
            ),
        };
        let mark_w = if i == 1 { 14.0 } else { 10.0 };
        let galley = painter.layout_no_wrap(word.to_string(), font.clone(), theme::TEXT_DIM);
        let text_w = galley.size().x;
        painter.galley(
            Pos2::new(x + mark_w + 6.0, y - galley.size().y / 2.0),
            galley,
            theme::TEXT_DIM,
        );
        x += mark_w + 6.0 + text_w + 18.0;
    }
}

/// A month or year in the year views, as its view-model says.
#[derive(Clone, Copy)]
struct BigCell<'a> {
    label: &'a str,
    level: f32,
    on_device: bool,
    current: bool,
    enabled: bool,
}

/// A shaded, tappable cell in the year views. Returns whether it was
/// tapped (never when disabled).
fn big_cell(ui: &mut egui::Ui, rect: Rect, id: egui::Id, c: &BigCell<'_>) -> bool {
    let BigCell {
        label,
        level,
        on_device,
        current,
        enabled,
    } = *c;
    let resp = ui.interact(rect, id, Sense::click());
    let painter = ui.painter();
    let cell = rect.shrink2(Vec2::new(4.0, 6.0));
    if level > 0.0 && enabled {
        painter.rect_filled(cell, 6.0, shade(level));
    }
    if current {
        painter.rect_stroke(
            cell,
            6.0,
            Stroke::new(1.5, theme::NOW),
            egui::StrokeKind::Inside,
        );
    }
    painter.text(
        cell.center(),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(theme::BODY),
        if enabled {
            theme::TEXT
        } else {
            theme::TEXT_FAINT
        },
    );
    if on_device && enabled {
        let y = cell.bottom() + 2.0;
        painter.rect_filled(
            Rect::from_min_max(
                Pos2::new(cell.left() + 8.0, y),
                Pos2::new(cell.right() - 8.0, y + UNDERLINE_H),
            ),
            0.0,
            theme::ON_DEVICE,
        );
    }
    enabled && resp.clicked()
}

fn year(ui: &mut egui::Ui, vm: &YearVm, state: &mut AppState) {
    header(
        ui,
        &vm.title,
        vm.prev.map(CalendarNav::Year),
        vm.next.map(CalendarNav::Year),
        Some(CalendarNav::Years),
        state,
    );
    ui.add_space(6.0);
    const COLS: usize = 3;
    let w = ui.available_width();
    let rows = vm.months.len().div_ceil(COLS);
    let (grid, _) = ui.allocate_exact_size(Vec2::new(w, BIG_CELL_H * rows as f32), Sense::hover());
    let col_w = w / COLS as f32;
    let mut picked = None;
    for (i, m) in vm.months.iter().enumerate() {
        let rect = Rect::from_min_size(
            Pos2::new(
                grid.left() + col_w * (i % COLS) as f32,
                grid.top() + BIG_CELL_H * (i / COLS) as f32,
            ),
            Vec2::new(col_w, BIG_CELL_H),
        );
        if big_cell(
            ui,
            rect,
            ui.id().with(("calendar_month", i)),
            &BigCell {
                label: m.label,
                level: m.level,
                on_device: m.on_device,
                current: m.is_current,
                enabled: m.enabled,
            },
        ) {
            picked = Some(CalendarNav::Month(m.month));
        }
    }
    if let Some(to) = picked {
        nav(state, to);
    }
}

fn years(ui: &mut egui::Ui, vm: &YearsVm, room: f32, state: &mut AppState) {
    header(ui, &vm.title, None, None, None, state);
    ui.add_space(6.0);
    const COLS: usize = 4;
    let mut picked = None;
    egui::ScrollArea::vertical()
        .id_salt("calendar_years")
        .max_height((room - HEADER_H - 6.0).max(BIG_CELL_H))
        .stick_to_bottom(true)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let w = ui.available_width();
            let rows = vm.years.len().div_ceil(COLS);
            let (grid, _) =
                ui.allocate_exact_size(Vec2::new(w, BIG_CELL_H * rows as f32), Sense::hover());
            let col_w = w / COLS as f32;
            for (i, y) in vm.years.iter().enumerate() {
                let rect = Rect::from_min_size(
                    Pos2::new(
                        grid.left() + col_w * (i % COLS) as f32,
                        grid.top() + BIG_CELL_H * (i / COLS) as f32,
                    ),
                    Vec2::new(col_w, BIG_CELL_H),
                );
                if big_cell(
                    ui,
                    rect,
                    ui.id().with(("calendar_year", y.year)),
                    &BigCell {
                        label: &y.label,
                        level: y.level,
                        on_device: y.on_device,
                        current: y.is_current,
                        enabled: true,
                    },
                ) {
                    picked = Some(CalendarNav::Year(y.year));
                }
            }
        });
    if let Some(to) = picked {
        nav(state, to);
    }
}
