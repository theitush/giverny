//! Giverny's own title bar, for WSLg (#69).
//!
//! Under WSLg the window is an X11 one (see `wslg::avoid_wayland`), and an
//! X11 window's frame there is drawn by WSLg's Weston: a 1×-scale caption a
//! few pixels tall with Windows-95 buttons, inside a thick black band where
//! Weston's shadow would be. Windows never draws a caption for it, and there
//! is no setting that asks it to. So on WSLg the window opens without
//! decorations and draws this instead — a Windows 11 caption in the theme's
//! colours, at the interface's own scale — and the edges it no longer has are
//! handed back as resize handles.
//!
//! Moving and resizing go to the window manager as `_NET_WM_MOVERESIZE`
//! (winit's `drag_window` / `drag_resize_window`), which WSLg passes on to
//! Windows, so a drag feels like dragging any other window.
//!
//! Maximising is the one thing the window manager cannot be left to do
//! (#78): see [`Maximize`].

use crate::chrome::Chrome;
use eframe::egui::{self, Color32, CursorIcon, ResizeDirection, Sense, Stroke, ViewportCommand};

/// Caption height, in points: Windows 11's 32 px at 100 %.
pub const HEIGHT: f32 = 32.0;

/// Width of a caption button: Windows 11's 46 px.
const BUTTON_W: f32 = 46.0;

/// How far in from the window's edge a press resizes rather than clicks, in
/// points. Windows' own invisible resize border is about this wide.
const EDGE: f32 = 5.0;

/// Windows 11's close-button red, and the white its glyph turns on it.
const CLOSE_RED: Color32 = Color32::from_rgb(0xc4, 0x2b, 0x1c);

/// Maximising a frameless window by hand, because WSLg gets it wrong (#78).
///
/// Weston's X window manager keeps a 32-px shadow margin round every
/// undecorated window, maximised or not, and WSLg sends the whole surface,
/// margin included, to Windows. Windows puts a maximised window's top-left
/// at the work area's corner — the margin's corner, not the window's — so a
/// maximised Giverny sat 32 px down and to the right, its right and bottom
/// 32 px past the screen. Weston's own frame drops the margin when
/// maximised, which is why a decorated window never showed it.
///
/// So the window manager maximises once, which is how the work area of
/// whichever monitor the window is on gets learned, and the window is then
/// un-maximised and laid over that work area by hand. An ordinary window is
/// placed exactly (its margin falls off-screen), so this one is flush.
/// Windows-side maximising — Win+↑, dragging to the top — arrives the same
/// way and is converted the same way.
#[derive(Default)]
pub struct Maximize {
    /// Laid over the work area by hand.
    on: bool,
    /// The work area, in physical pixels, until the window is laid over it.
    pending: Option<egui::Rect>,
    /// Frames since the placement was asked for; the window manager takes a
    /// few to act on it, and until then the window is not yet where it goes.
    settling: u8,
    /// The window manager's maximised rect as last seen: acted on only once
    /// two frames agree, because it resizes before it moves.
    seen: Option<egui::Rect>,
    /// Where it was laid, in physical pixels, once it got there.
    placed: Option<egui::Rect>,
    /// The inner rect, in physical pixels, to go back to on restore.
    restore: Option<egui::Rect>,
}

impl Maximize {
    /// Whether the window is maximised, by the window manager or by hand.
    pub fn is_on(&self, ctx: &egui::Context) -> bool {
        self.on || ctx.input(|i| i.viewport().maximized.unwrap_or(false))
    }

    /// Once a frame, before anything reads [`Maximize::is_on`].
    pub fn update(&mut self, ctx: &egui::Context) {
        let ppp = ctx.pixels_per_point();
        let (maximized, fullscreen, inner) = ctx.input(|i| {
            let vp = i.viewport();
            (
                vp.maximized.unwrap_or(false),
                vp.fullscreen.unwrap_or(false),
                vp.inner_rect,
            )
        });
        let Some(inner) = inner else {
            return;
        };
        if fullscreen {
            return;
        }
        let px = |r: egui::Rect| egui::Rect::from_min_max(r.min * ppp, r.max * ppp);
        if maximized {
            // The window manager's maximise: its size is the work area's,
            // and its position the work area's corner. Take both, and undo it.
            let area = px(inner);
            if !self.seen.is_some_and(|s| near(s, area)) {
                self.seen = Some(area);
                ctx.request_repaint_after(std::time::Duration::from_millis(16));
                return;
            }
            self.seen = None;
            // Opened maximised, it has no size of its own yet: give it
            // one, as Windows would, rather than restore to the screen.
            self.restore.get_or_insert_with(|| {
                egui::Rect::from_center_size(area.center(), area.size() * 0.75)
            });
            self.pending = Some(area);
            self.on = true;
            self.settling = 0;
            ctx.send_viewport_cmd(ViewportCommand::Maximized(false));
            return;
        }
        if let Some(area) = self.pending {
            // Asked once, and once more if the first was lost in the
            // un-maximise; given up on after that, where it stands.
            if self.settling == 0 || self.settling == 10 {
                place(ctx, area.min / ppp, area.size() / ppp);
            }
            self.settling = self.settling.saturating_add(1);
            if near(px(inner), area) || self.settling > 40 {
                self.pending = None;
                self.placed = Some(px(inner));
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
            return;
        }
        if self.on {
            // Moved or resized since (a Snap, a drag off the taskbar): not
            // maximised any more.
            if self.placed.is_some_and(|p| !near(px(inner), p)) {
                self.on = false;
                self.placed = None;
            }
        } else {
            self.restore = Some(px(inner));
        }
    }

    /// The caption's □, or a double click on it.
    pub fn toggle(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().maximized.unwrap_or(false)) {
            ctx.send_viewport_cmd(ViewportCommand::Maximized(false));
        } else if self.on {
            self.restore_to(ctx, None);
        } else {
            // Through the window manager, for the work area; `update` takes
            // it from there.
            ctx.send_viewport_cmd(ViewportCommand::Maximized(true));
        }
    }

    /// Back to the size it had, at `corner` (in points) or where it was.
    fn restore_to(&mut self, ctx: &egui::Context, corner: Option<egui::Pos2>) {
        self.on = false;
        self.pending = None;
        self.placed = None;
        let Some(r) = self.restore else {
            return;
        };
        let ppp = ctx.pixels_per_point();
        place(ctx, corner.unwrap_or(r.min / ppp), r.size() / ppp);
    }

    /// A drag on the caption of a window maximised by hand: back to its own
    /// size first, under the pointer where it was along the caption, as
    /// Windows does, and then the move.
    fn unmaximize_for_drag(&mut self, ctx: &egui::Context) {
        let ppp = ctx.pixels_per_point();
        let (pointer, inner) = ctx.input(|i| (i.pointer.interact_pos(), i.viewport().inner_rect));
        let (Some(p), Some(inner), Some(r)) = (pointer, inner, self.restore) else {
            self.restore_to(ctx, None);
            return;
        };
        let w = r.width() / ppp;
        let along = (p.x / inner.width().max(1.0)).clamp(0.0, 1.0);
        let corner = egui::pos2(inner.min.x + p.x - w * along, inner.min.y);
        self.restore_to(ctx, Some(corner));
    }
}

/// Put the window's inner area at `corner`, `size` big, both in points.
///
/// `OuterPosition` though it is the *inner* corner: winit asks X to move
/// the client window there, and WSLg's Weston takes that as where the
/// window's content goes, whatever frame extents winit reports for it.
fn place(ctx: &egui::Context, corner: egui::Pos2, size: egui::Vec2) {
    ctx.send_viewport_cmd(ViewportCommand::InnerSize(size));
    ctx.send_viewport_cmd(ViewportCommand::OuterPosition(corner));
}

/// Two rects within a pixel or two of each other.
fn near(a: egui::Rect, b: egui::Rect) -> bool {
    (a.min - b.min).length() < 2.5 && (a.max - b.max).length() < 2.5
}

/// Draw the caption across the top of the window.
pub fn show(ui: &mut egui::Ui, title: &str, chrome: &Chrome, max: &mut Maximize) {
    let ctx = ui.ctx().clone();
    let maximized = max.is_on(&ctx);
    let focused = ctx.input(|i| i.viewport().focused.unwrap_or(true));

    let rect = ui.max_rect();
    let bar = ui.interact(rect, ui.id().with("caption"), Sense::click_and_drag());
    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, chrome.panel);
    painter.hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        Stroke::new(1.0, chrome.dim.gamma_multiply(0.25)),
    );

    let fg = if focused { chrome.fg } else { chrome.dim };
    // The title where Windows puts it: at the left, after a small mark.
    let mark = egui::Rect::from_center_size(
        egui::pos2(rect.left() + 20.0, rect.center().y),
        egui::vec2(8.0, 8.0),
    );
    painter.circle_filled(
        mark.center(),
        4.0,
        if focused { chrome.accent } else { chrome.dim },
    );
    painter.text(
        egui::pos2(rect.left() + 34.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        title,
        egui::FontId::proportional(12.5),
        fg,
    );

    // Minimise, maximise/restore and close, right to left.
    let mut x = rect.right();
    let mut button = |kind: Button| -> bool {
        let r = egui::Rect::from_min_max(
            egui::pos2(x - BUTTON_W, rect.top()),
            egui::pos2(x, rect.bottom()),
        );
        x -= BUTTON_W;
        let resp = ui.interact(r, ui.id().with(kind as u8), Sense::click());
        let (bg, glyph) = match (kind, resp.hovered(), resp.is_pointer_button_down_on()) {
            (Button::Close, true, _) => (Some(CLOSE_RED), Color32::WHITE),
            (_, true, true) => (Some(chrome.fg.gamma_multiply(0.14)), chrome.fg),
            (_, true, false) => (Some(chrome.fg.gamma_multiply(0.08)), chrome.fg),
            _ => (None, fg),
        };
        let painter = ui.painter();
        if let Some(bg) = bg {
            painter.rect_filled(r, 0.0, bg);
        }
        draw_glyph(painter, r.center(), kind, maximized, glyph);
        resp.clicked()
    };
    if button(Button::Close) {
        ctx.send_viewport_cmd(ViewportCommand::Close);
    }
    if button(Button::Maximize) {
        max.toggle(&ctx);
    }
    if button(Button::Minimize) {
        ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
    }

    // The rest of the bar moves the window; a double click maximises, as
    // everywhere on Windows. A press on a resize edge is not a move.
    if bar.double_clicked() {
        max.toggle(&ctx);
    } else if bar.drag_started_by(egui::PointerButton::Primary)
        && edge_under_pointer(&ctx, maximized).is_none()
    {
        if max.on {
            max.unmaximize_for_drag(&ctx);
        }
        ctx.send_viewport_cmd(ViewportCommand::StartDrag);
    }
}

#[derive(Clone, Copy, Hash, PartialEq, Eq)]
enum Button {
    Minimize,
    Maximize,
    Close,
}

/// The Segoe Fluent caption glyphs, drawn: 10 px, 1 px strokes.
fn draw_glyph(p: &egui::Painter, c: egui::Pos2, kind: Button, maximized: bool, color: Color32) {
    let s = Stroke::new(1.0, color);
    let h = 5.0;
    match kind {
        Button::Minimize => {
            p.hline(c.x - h..=c.x + h, c.y, s);
        }
        Button::Maximize if maximized => {
            // Restore: a square with another behind it.
            let front = egui::Rect::from_min_max(
                egui::pos2(c.x - h, c.y - h + 2.0),
                egui::pos2(c.x + h - 2.0, c.y + h),
            );
            p.rect_stroke(front, 1.0, s, egui::StrokeKind::Middle);
            p.hline(c.x - h + 2.0..=c.x + h, c.y - h, s);
            p.vline(c.x + h, c.y - h..=c.y + h - 2.0, s);
        }
        Button::Maximize => {
            let r = egui::Rect::from_center_size(c, egui::vec2(2.0 * h, 2.0 * h));
            p.rect_stroke(r, 1.0, s, egui::StrokeKind::Middle);
        }
        Button::Close => {
            p.line_segment([c + egui::vec2(-h, -h), c + egui::vec2(h, h)], s);
            p.line_segment([c + egui::vec2(-h, h), c + egui::vec2(h, -h)], s);
        }
    }
}

/// The window edge the pointer is on, if it is within [`EDGE`] of one.
fn edge_under_pointer(ctx: &egui::Context, maximized: bool) -> Option<ResizeDirection> {
    if maximized {
        return None;
    }
    let pos = ctx.input(|i| i.pointer.latest_pos())?;
    edge_at(ctx.content_rect(), pos, EDGE)
}

/// Which edge or corner of `rect` a point within `edge` of it is on.
fn edge_at(rect: egui::Rect, pos: egui::Pos2, edge: f32) -> Option<ResizeDirection> {
    // Corners are generous, as on Windows: a corner's grip runs a little way
    // along both edges, or it would be a few pixels nobody can hit.
    let corner = edge * 3.0;
    let left = pos.x - rect.left() < edge;
    let right = rect.right() - pos.x < edge;
    let top = pos.y - rect.top() < edge;
    let bottom = rect.bottom() - pos.y < edge;
    let near_left = pos.x - rect.left() < corner;
    let near_right = rect.right() - pos.x < corner;
    let near_top = pos.y - rect.top() < corner;
    let near_bottom = rect.bottom() - pos.y < corner;
    Some(match () {
        _ if (top && near_left) || (left && near_top) => ResizeDirection::NorthWest,
        _ if (top && near_right) || (right && near_top) => ResizeDirection::NorthEast,
        _ if (bottom && near_left) || (left && near_bottom) => ResizeDirection::SouthWest,
        _ if (bottom && near_right) || (right && near_bottom) => ResizeDirection::SouthEast,
        _ if top => ResizeDirection::North,
        _ if bottom => ResizeDirection::South,
        _ if left => ResizeDirection::West,
        _ if right => ResizeDirection::East,
        _ => return None,
    })
}

/// A one-pixel outline round the window, as Windows 11 draws one: without a
/// frame a dark window on a dark desktop has no edge at all. Not when
/// maximised, where there is nothing to be told apart from.
pub fn outline(ctx: &egui::Context, chrome: &Chrome, maximized: bool) {
    if maximized {
        return;
    }
    let px = 1.0 / ctx.pixels_per_point();
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("window-outline"),
    ));
    painter.rect_stroke(
        ctx.content_rect(),
        0.0,
        Stroke::new(px, chrome.dim.gamma_multiply(0.45)),
        egui::StrokeKind::Inside,
    );
}

/// Turn the window's outermost few points into resize handles. Runs before
/// anything else is drawn, so a press on an edge is the resize's and not the
/// widget's underneath; the cursor shows which way it will go.
pub fn resize_edges(ctx: &egui::Context, maximized: bool) {
    let Some(dir) = edge_under_pointer(ctx, maximized) else {
        return;
    };
    ctx.set_cursor_icon(match dir {
        ResizeDirection::North | ResizeDirection::South => CursorIcon::ResizeVertical,
        ResizeDirection::East | ResizeDirection::West => CursorIcon::ResizeHorizontal,
        ResizeDirection::NorthWest | ResizeDirection::SouthEast => CursorIcon::ResizeNwSe,
        ResizeDirection::NorthEast | ResizeDirection::SouthWest => CursorIcon::ResizeNeSw,
    });
    if ctx.input(|i| i.pointer.primary_pressed()) {
        ctx.send_viewport_cmd(ViewportCommand::BeginResize(dir));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect() -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0))
    }

    #[test]
    fn the_middle_is_no_edge() {
        assert_eq!(edge_at(rect(), egui::pos2(400.0, 300.0), 5.0), None);
        assert_eq!(edge_at(rect(), egui::pos2(6.0, 300.0), 5.0), None);
    }

    #[test]
    fn edges_and_corners() {
        let r = rect();
        assert_eq!(
            edge_at(r, egui::pos2(2.0, 300.0), 5.0),
            Some(ResizeDirection::West)
        );
        assert_eq!(
            edge_at(r, egui::pos2(798.0, 300.0), 5.0),
            Some(ResizeDirection::East)
        );
        assert_eq!(
            edge_at(r, egui::pos2(400.0, 1.0), 5.0),
            Some(ResizeDirection::North)
        );
        assert_eq!(
            edge_at(r, egui::pos2(400.0, 599.0), 5.0),
            Some(ResizeDirection::South)
        );
        // A corner's grip runs a little way along each edge.
        assert_eq!(
            edge_at(r, egui::pos2(10.0, 1.0), 5.0),
            Some(ResizeDirection::NorthWest)
        );
        assert_eq!(
            edge_at(r, egui::pos2(1.0, 10.0), 5.0),
            Some(ResizeDirection::NorthWest)
        );
        assert_eq!(
            edge_at(r, egui::pos2(790.0, 598.0), 5.0),
            Some(ResizeDirection::SouthEast)
        );
        assert_eq!(
            edge_at(r, egui::pos2(799.0, 2.0), 5.0),
            Some(ResizeDirection::NorthEast)
        );
        assert_eq!(
            edge_at(r, egui::pos2(3.0, 595.0), 5.0),
            Some(ResizeDirection::SouthWest)
        );
    }

    #[test]
    fn near_is_a_pixel_or_two() {
        let r = rect();
        assert!(near(r, r.translate(egui::vec2(1.0, -1.0))));
        assert!(!near(r, r.translate(egui::vec2(32.0, 32.0))));
        assert!(!near(r, r.expand(4.0)));
    }
}
