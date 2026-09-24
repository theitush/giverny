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

/// Draw the caption across the top of the window.
pub fn show(ui: &mut egui::Ui, title: &str, chrome: &Chrome) {
    let ctx = ui.ctx().clone();
    let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
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
        ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized));
    }
    if button(Button::Minimize) {
        ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
    }

    // The rest of the bar moves the window; a double click maximises, as
    // everywhere on Windows. A press on a resize edge is not a move.
    if bar.double_clicked() {
        ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized));
    } else if bar.drag_started_by(egui::PointerButton::Primary)
        && edge_under_pointer(&ctx).is_none()
    {
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
fn edge_under_pointer(ctx: &egui::Context) -> Option<ResizeDirection> {
    if ctx.input(|i| i.viewport().maximized.unwrap_or(false)) {
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
pub fn outline(ctx: &egui::Context, chrome: &Chrome) {
    if ctx.input(|i| i.viewport().maximized.unwrap_or(false)) {
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
pub fn resize_edges(ctx: &egui::Context) {
    let Some(dir) = edge_under_pointer(ctx) else {
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
}
