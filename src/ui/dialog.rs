//! The one dialog component ("One Instrument" §12), and its popover sibling.
//!
//! Every window is a [`Dialog`]: a left-aligned title with a quiet subtitle,
//! a close mark at the right, the body, and a [`footer`] only when there is a
//! primary action. It opens anchored in the upper third and grows downward,
//! so it does not move as its content changes. A [`Popover`] is the same
//! frame anchored to the control that opened it (the site control, the scan
//! that was right-clicked).
//!
//! Neither closes itself. Escape is handled once, centrally, against the
//! [`DialogStack`]; a close mark or a click outside comes back as
//! [`DialogOut::close`] and the caller emits
//! `DialogIntent::Close(id)`. Stacking follows the stack: [`restack`] puts
//! the windows in the stack's order whenever it changes, so a confirmation
//! opened over the event dialog sits on top of it.

use crate::core::dialogs::{DialogId, DialogStack};
use crate::ui::theme;
use eframe::egui::{self, Align2, Id, LayerId, Order, Pos2, Rect, RichText, Stroke, Vec2};

/// The four dialog widths. Each shrinks to the viewport less a gutter.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Width {
    /// Confirmations and short forms.
    Narrow,
    /// Most dialogs.
    Regular,
    /// Lists with long rows (alerts).
    Wide,
    /// Developer tables.
    Full,
}

impl Width {
    fn px(self) -> f32 {
        match self {
            Width::Narrow => 360.0,
            Width::Regular => 440.0,
            Width::Wide => 560.0,
            Width::Full => 940.0,
        }
    }
}

/// Space kept between a dialog and the viewport's edges.
const GUTTER: f32 = 8.0;
/// Inner margin of the frame.
const MARGIN: f32 = 16.0;
/// Where a dialog's top sits, as a fraction of the viewport height (upper
/// third), clamped to [`TOP_MIN`, `TOP_MAX`] pixels.
const TOP_FRACTION: f32 = 0.12;
const TOP_MIN: f32 = 16.0;
const TOP_MAX: f32 = 120.0;
/// Gap between a popover and its anchor.
const POPOVER_GAP: f32 = 6.0;

/// The egui id of an entry's window.
fn window_id(id: DialogId) -> Id {
    Id::unique(("dialog_window", id.key()))
}

/// The egui id of an entry's backdrop.
fn backdrop_id(id: DialogId) -> Id {
    Id::unique(("dialog_backdrop", id.key()))
}

/// What a dialog or popover reports back.
pub(crate) struct DialogOut {
    /// The close mark or a click outside asked to close.
    pub close: bool,
}

/// The whole screen.
fn screen(ctx: &egui::Context) -> Rect {
    ctx.input(|i| i.viewport_rect())
}

/// The screen less the phone's safe-area insets (status bar, home
/// indicator) — where windows may sit.
fn viewport(ctx: &egui::Context) -> Rect {
    let (top, right, bottom, left) = super::mobile::safe_area_insets();
    let s = screen(ctx);
    Rect::from_min_max(
        Pos2::new(s.left() + left.max(0.0), s.top() + top.max(0.0)),
        Pos2::new(s.right() - right.max(0.0), s.bottom() - bottom.max(0.0)),
    )
}

fn width_in(ctx: &egui::Context, width: Width) -> f32 {
    (viewport(ctx).width() - 2.0 * GUTTER)
        .min(width.px())
        .max(240.0)
}

/// The shared frame: the raised surface, no border on every edge, a soft
/// shadow.
fn frame() -> egui::Frame {
    egui::Frame::new()
        .fill(theme::dialog::SURFACE)
        .corner_radius(theme::dialog::RADIUS)
        .inner_margin(MARGIN)
        .stroke(Stroke::new(1.0, theme::dialog::EDGE))
        .shadow(egui::epaint::Shadow {
            offset: [0, 6],
            blur: 18,
            spread: 0,
            color: theme::alpha(theme::MAP, 160),
        })
}

/// The full-screen layer under a window. `dim` paints the dialog backdrop;
/// a popover's is invisible but still catches the click outside. Returns
/// whether it was clicked.
fn backdrop(ctx: &egui::Context, id: DialogId, dim: bool) -> bool {
    // A press on the backdrop (the click outside) asks egui to move it to
    // the top of the foreground, over its own window; the next open would
    // then draw a window that gets no hover or clicks. Keeping the window
    // a sublayer of the backdrop pins it directly above, whatever moved.
    ctx.set_sublayer(
        LayerId::new(Order::Foreground, backdrop_id(id)),
        LayerId::new(Order::Foreground, window_id(id)),
    );
    let mut clicked = false;
    egui::Area::new(backdrop_id(id))
        .fixed_pos(Pos2::ZERO)
        .order(Order::Foreground)
        .show(ctx, |ui| {
            let full = screen(ctx);
            let (response, painter) = ui.allocate_painter(full.size(), egui::Sense::click());
            if dim {
                painter.rect_filled(full, 0.0, theme::dialog::backdrop());
            }
            clicked = response.clicked();
        });
    clicked
}

/// The title row: title left, close mark right (when dismissible), then
/// the quiet subtitle.
fn header(ui: &mut egui::Ui, title: &str, subtitle: Option<&str>, dismissible: bool) -> bool {
    let mut close = false;
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).size(theme::TITLE).color(theme::TEXT));
        if dismissible {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                close = ui
                    .add(
                        egui::Button::new(
                            RichText::new(crate::ui::icons::X)
                                .size(theme::BODY)
                                .color(theme::TEXT_DIM),
                        )
                        .frame(false),
                    )
                    .on_hover_text("Close (Esc)")
                    .clicked();
            });
        }
    });
    if let Some(sub) = subtitle {
        ui.label(RichText::new(sub).size(theme::LABEL).color(theme::TEXT_DIM));
    }
    ui.add_space(10.0);
    close
}

/// A dialog: one frame, title, body, optional footer.
pub(crate) struct Dialog<'a> {
    id: DialogId,
    title: &'a str,
    subtitle: Option<String>,
    width: Width,
    dismissible: bool,
}

impl<'a> Dialog<'a> {
    /// A dismissible, regular-width dialog for stack entry `id`.
    pub(crate) fn new(id: DialogId, title: &'a str) -> Self {
        Self {
            id,
            title,
            subtitle: None,
            width: Width::Regular,
            dismissible: true,
        }
    }

    /// The quiet line under the title.
    pub(crate) fn subtitle(mut self, subtitle: impl Into<String>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }

    pub(crate) fn width(mut self, width: Width) -> Self {
        self.width = width;
        self
    }

    /// Draw the dialog. The body gets the content width; the window never
    /// grows past the viewport's bottom (bodies with long lists scroll).
    pub(crate) fn show(self, ctx: &egui::Context, body: impl FnOnce(&mut egui::Ui)) -> DialogOut {
        let mut close = backdrop(ctx, self.id, true) && self.dismissible;
        let vp = viewport(ctx);
        let w = width_in(ctx, self.width);
        let top = (vp.height() * TOP_FRACTION).clamp(TOP_MIN, TOP_MAX);
        let max_h = (vp.height() - top - GUTTER).max(120.0);
        let offset = Vec2::new(vp.center().x - screen(ctx).center().x, vp.top() + top);
        egui::Window::new(self.title)
            .id(window_id(self.id))
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_TOP, offset)
            .min_width(w - 2.0 * MARGIN)
            .max_width(w - 2.0 * MARGIN)
            .max_height(max_h)
            .frame(frame())
            .order(Order::Foreground)
            .show(ctx, |ui| {
                close |= header(ui, self.title, self.subtitle.as_deref(), self.dismissible);
                body(ui);
            });
        DialogOut { close }
    }
}

/// A phone sheet: the dialog's frame and header at a fixed rect (bottom in
/// portrait, right in landscape, placed by the core), over the dimmed
/// backdrop. `trailing` is a quiet word at the title's right (the site).
pub(crate) struct Sheet<'a> {
    id: DialogId,
    title: &'a str,
    trailing: Option<&'a str>,
    rect: Rect,
}

impl<'a> Sheet<'a> {
    /// A dismissible sheet for stack entry `id` filling `rect`.
    pub(crate) fn new(id: DialogId, title: &'a str, rect: Rect) -> Self {
        Self {
            id,
            title,
            trailing: None,
            rect,
        }
    }

    pub(crate) fn trailing(mut self, text: &'a str) -> Self {
        self.trailing = Some(text);
        self
    }

    /// Draw the sheet. `body` gets the content width and the height left.
    pub(crate) fn show(
        self,
        ctx: &egui::Context,
        body: impl FnOnce(&mut egui::Ui, f32),
    ) -> DialogOut {
        let mut close = backdrop(ctx, self.id, true);
        let inner = Vec2::new(
            (self.rect.width() - 2.0 * MARGIN).max(0.0),
            (self.rect.height() - 2.0 * MARGIN).max(0.0),
        );
        egui::Area::new(window_id(self.id))
            .order(Order::Foreground)
            .fixed_pos(self.rect.min)
            .show(ctx, |ui| {
                frame().show(ui, |ui| {
                    ui.set_width(inner.x);
                    ui.set_height(inner.y);
                    let top = ui.cursor().top();
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(self.title)
                                .size(theme::TITLE)
                                .color(theme::TEXT),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            close |= ui
                                .add(
                                    egui::Button::new(
                                        RichText::new(crate::ui::icons::X)
                                            .size(theme::BODY)
                                            .color(theme::TEXT_DIM),
                                    )
                                    .frame(false)
                                    .min_size(Vec2::splat(28.0)),
                                )
                                .clicked();
                            if let Some(t) = self.trailing {
                                ui.label(
                                    RichText::new(t).size(theme::LABEL).color(theme::TEXT_DIM),
                                );
                            }
                        });
                    });
                    ui.add_space(4.0);
                    let used = ui.cursor().top() - top;
                    body(ui, (inner.y - used).max(0.0));
                });
            });
        DialogOut { close }
    }
}

/// A popover anchored to the control that opened it.
pub(crate) struct Popover<'a> {
    id: DialogId,
    anchor: Rect,
    title: Option<&'a str>,
    subtitle: Option<String>,
    width: Width,
    dismissible: bool,
}

impl<'a> Popover<'a> {
    /// A dismissible popover for stack entry `id`, anchored to `anchor`
    /// (the trigger's screen rect; a point is a zero-size rect).
    pub(crate) fn new(id: DialogId, anchor: Rect) -> Self {
        Self {
            id,
            anchor,
            title: None,
            subtitle: None,
            width: Width::Regular,
            dismissible: true,
        }
    }

    pub(crate) fn title(mut self, title: &'a str) -> Self {
        self.title = Some(title);
        self
    }

    pub(crate) fn subtitle(mut self, subtitle: impl Into<String>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }

    pub(crate) fn width(mut self, width: Width) -> Self {
        self.width = width;
        self
    }

    pub(crate) fn dismissible(mut self, dismissible: bool) -> Self {
        self.dismissible = dismissible;
        self
    }

    /// Draw the popover below its anchor when the anchor is in the upper
    /// half of the screen, above it otherwise; clamped inside the viewport.
    /// `body` receives the height left for content.
    pub(crate) fn show(
        self,
        ctx: &egui::Context,
        body: impl FnOnce(&mut egui::Ui, f32),
    ) -> DialogOut {
        let mut close = backdrop(ctx, self.id, !self.dismissible) && self.dismissible;
        let vp = viewport(ctx);
        let w = width_in(ctx, self.width);
        let x = self
            .anchor
            .left()
            .clamp(vp.left() + GUTTER, (vp.right() - w - GUTTER).max(GUTTER));
        let below = self.anchor.center().y < vp.center().y;
        let (pivot, pos, room) = if below {
            let y = self.anchor.bottom() + POPOVER_GAP;
            (Align2::LEFT_TOP, Pos2::new(x, y), vp.bottom() - y - GUTTER)
        } else {
            let y = self.anchor.top() - POPOVER_GAP;
            (Align2::LEFT_BOTTOM, Pos2::new(x, y), y - vp.top() - GUTTER)
        };
        let content_h = (room - 2.0 * MARGIN).max(120.0);
        egui::Area::new(window_id(self.id))
            .order(Order::Foreground)
            .pivot(pivot)
            .fixed_pos(pos)
            .constrain(true)
            .show(ctx, |ui| {
                frame().show(ui, |ui| {
                    ui.set_width(w - 2.0 * MARGIN);
                    let mut used = 0.0;
                    if let Some(title) = self.title {
                        let before = ui.cursor().top();
                        close |= header(ui, title, self.subtitle.as_deref(), self.dismissible);
                        used = ui.cursor().top() - before;
                    }
                    body(ui, (content_h - used).max(80.0));
                });
            });
        DialogOut { close }
    }
}

/// The footer: a hairline, then the buttons right-aligned. Add the primary
/// (or destructive) button first; it lands rightmost.
pub(crate) fn footer(ui: &mut egui::Ui, buttons: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(12.0);
    ui.separator();
    ui.add_space(6.0);
    // One row high: a bare right-to-left layout takes the window's whole
    // remaining height, which stretched short dialogs to the viewport.
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), buttons);
    });
}

fn sized(button: egui::Button<'_>) -> egui::Button<'_> {
    button.min_size(Vec2::new(72.0, 28.0))
}

/// The one primary button: filled, neutral (chrome is grey).
pub(crate) fn primary(ui: &mut egui::Ui, label: &str) -> egui::Response {
    ui.add(sized(
        egui::Button::new(RichText::new(label).color(theme::dialog::PRIMARY_TEXT))
            .fill(theme::dialog::PRIMARY_FILL),
    ))
}

/// A primary button that can be disabled (with the reason on hover).
pub(crate) fn primary_enabled(
    ui: &mut egui::Ui,
    enabled: bool,
    label: &str,
    disabled_reason: &str,
) -> egui::Response {
    let r = ui.add_enabled(
        enabled,
        sized(
            egui::Button::new(RichText::new(label).color(theme::dialog::PRIMARY_TEXT))
                .fill(theme::dialog::PRIMARY_FILL),
        ),
    );
    if enabled {
        r
    } else {
        r.on_disabled_hover_text(disabled_reason)
    }
}

/// A secondary button (Cancel, Close).
pub(crate) fn secondary(ui: &mut egui::Ui, label: &str) -> egui::Response {
    ui.add(sized(
        egui::Button::new(RichText::new(label).color(theme::TEXT))
            .fill(theme::dialog::SECONDARY_FILL),
    ))
}

/// The destructive button: outlined, not filled, and not red (red means
/// now). The label says what goes.
pub(crate) fn destructive(ui: &mut egui::Ui, label: &str) -> egui::Response {
    ui.add(sized(
        egui::Button::new(
            RichText::new(format!("{} {label}", crate::ui::icons::TRASH))
                .color(theme::dialog::DESTRUCTIVE),
        )
        .fill(egui::Color32::TRANSPARENT)
        .stroke(Stroke::new(1.5, theme::dialog::DESTRUCTIVE)),
    ))
}

/// A bullet list line in body text.
pub(crate) fn bullet(ui: &mut egui::Ui, text: &str, color: egui::Color32) {
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("\u{2022}").color(theme::TEXT_FAINT));
        ui.label(RichText::new(text).color(color));
    });
}

/// A small group heading inside a dialog body ("Deletes", "Near you").
pub(crate) fn group_heading(ui: &mut egui::Ui, text: &str) {
    ui.label(
        RichText::new(text)
            .size(theme::CAPTION)
            .color(theme::TEXT_FAINT),
    );
}

/// Put the windows in the stack's order (bottom to top) after the stack
/// changed: each entry's backdrop, then its window, over everything below.
pub(crate) fn restack(ctx: &egui::Context, stack: &DialogStack) {
    for id in stack.ids().filter(|d| d.has_window()) {
        ctx.move_to_top(LayerId::new(Order::Foreground, backdrop_id(id)));
        ctx.move_to_top(LayerId::new(Order::Foreground, window_id(id)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn widths_ascend() {
        let w = [Width::Narrow, Width::Regular, Width::Wide, Width::Full];
        for p in w.windows(2) {
            assert!(p[0].px() < p[1].px());
        }
    }

    #[wasm_bindgen_test]
    fn each_entry_has_its_own_layers() {
        assert!(window_id(DialogId::Event) != window_id(DialogId::Confirm));
        assert!(window_id(DialogId::Event) != backdrop_id(DialogId::Event));
    }

    /// A dialog and a popover lay out headlessly, and a click outside a
    /// dismissible popover asks to close it.
    #[wasm_bindgen_test]
    fn dialog_and_popover_draw_headlessly() {
        let ctx = egui::Context::default();
        let input = || egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1200.0, 800.0))),
            ..Default::default()
        };
        let mut drew = (false, false);
        ctx.begin_pass(input());
        {
            let ctx = &ctx;
            let out = Dialog::new(DialogId::Event, "Save event")
                .subtitle("KTLX")
                .show(ctx, |ui| {
                    footer(ui, |ui| {
                        primary(ui, "Save");
                        secondary(ui, "Cancel");
                    });
                    drew.0 = true;
                });
            assert!(!out.close);
            let anchor = Rect::from_min_size(Pos2::new(100.0, 10.0), Vec2::new(80.0, 24.0));
            let out = Popover::new(DialogId::SitePicker, anchor)
                .title("Site")
                .show(ctx, |_ui, room| drew.1 = room > 0.0);
            assert!(!out.close);
        }
        ctx.end_pass().textures_delta.clear();
        assert!(drew.0 && drew.1);
    }

    /// The popover's window stays above its backdrop after a click outside
    /// moved the backdrop to the top (the picker's dead reopen).
    #[wasm_bindgen_test]
    fn window_stays_above_its_backdrop_after_a_click_outside() {
        let ctx = egui::Context::default();
        let input = || egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1200.0, 800.0))),
            ..Default::default()
        };
        let anchor = Rect::from_min_size(Pos2::new(100.0, 10.0), Vec2::new(80.0, 24.0));
        let show = |ctx: &egui::Context| {
            Popover::new(DialogId::SitePicker, anchor)
                .title("Site")
                .show(ctx, |ui, _| {
                    ui.label("row");
                });
        };
        let window = LayerId::new(Order::Foreground, window_id(DialogId::SitePicker));
        let backdrop = LayerId::new(Order::Foreground, backdrop_id(DialogId::SitePicker));

        ctx.begin_pass(input());
        show(&ctx);
        ctx.end_pass().textures_delta.clear();

        // The click outside: the press on the backdrop asks for the top.
        ctx.begin_pass(input());
        show(&ctx);
        ctx.move_to_top(backdrop);
        ctx.end_pass().textures_delta.clear();

        // Reopened: a point inside the window must still hit the window.
        ctx.begin_pass(input());
        show(&ctx);
        let inside = anchor.left_bottom() + Vec2::new(20.0, 40.0);
        assert_eq!(ctx.layer_id_at(inside), Some(window));
        ctx.end_pass().textures_delta.clear();
    }
}
