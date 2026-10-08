//! The user's last prompt, pinned above a Claude tab's terminal.
//!
//! A long answer scrolls the question that started it off the screen, and
//! Claude Code has no way to keep it in view. One line here, cut to fit, and
//! the whole prompt below it on a click. The terminal never moves for it: the
//! line is part of the layout, the full prompt floats over the grid, so
//! opening it does not resize the PTY and make Claude redraw.

use eframe::egui::{self, FontId, Rect, Sense, Stroke, Vec2};
use giverny_core::tabs::TabId;
use giverny_term::render::opacity::see_through;

use crate::chrome::{Chrome, mix};

/// Height of the collapsed bar.
pub const HEIGHT: f32 = 22.0;
/// The most of a prompt the one line ever lays out. It is cut to the width
/// anyway; this only spares laying out a pasted log to show its first words.
const LINE_CHARS: usize = 400;

/// A prompt as one line: line breaks and runs of whitespace become single
/// spaces, and anything past `max` characters becomes an ellipsis.
pub fn one_line(prompt: &str, max: usize) -> String {
    let mut out = String::new();
    let mut count = 0;
    for word in prompt.split_whitespace() {
        if count > 0 {
            if count == max {
                return cut(out);
            }
            out.push(' ');
            count += 1;
        }
        for ch in word.chars() {
            if count == max {
                return cut(out);
            }
            out.push(ch);
            count += 1;
        }
    }
    out
}

fn cut(mut text: String) -> String {
    text.truncate(text.trim_end().len());
    text.push('…');
    text
}

/// Draw the bar for `tab`. Returns true when it was clicked, so the caller
/// can hand the keyboard back to the terminal.
pub fn show(ui: &mut egui::Ui, chrome: &Chrome, opacity: f32, tab: TabId, prompt: &str) -> bool {
    let open_id = egui::Id::new(("giverny-prompt-bar", tab.0));
    let mut open = ui.data(|d| d.get_temp::<bool>(open_id).unwrap_or(false));

    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), HEIGHT),
        // Clicks only: a focusable bar would take the keyboard from the
        // terminal under it.
        Sense::CLICK,
    );
    let hovered = response.hovered();
    let p = ui.painter_at(rect);
    let lift = if hovered || open { 0.12 } else { 0.06 };
    let fill = mix(chrome.panel, chrome.fg, lift);
    p.rect_filled(rect, 0.0, see_through(fill, opacity));
    // An accent edge, as the rail marks its active tab: this is yours, not
    // Claude's output.
    p.rect_filled(
        Rect::from_min_size(rect.min, Vec2::new(3.0, rect.height())),
        0.0,
        chrome.accent,
    );

    let font = FontId::monospace(12.0);
    let left = rect.min.x + 10.0;
    let label = p.text(
        egui::pos2(left, rect.center().y),
        egui::Align2::LEFT_CENTER,
        "you ›",
        font.clone(),
        chrome.dim,
    );
    let marker = if open { "▴" } else { "▾" };
    let marker_rect = p.text(
        egui::pos2(rect.max.x - 10.0, rect.center().y),
        egui::Align2::RIGHT_CENTER,
        marker,
        font.clone(),
        chrome.dim,
    );

    let text_left = label.max.x + 8.0;
    let width = (marker_rect.min.x - 10.0 - text_left).max(0.0);
    let mut job = egui::text::LayoutJob::single_section(
        one_line(prompt, LINE_CHARS),
        egui::TextFormat::simple(font.clone(), chrome.fg),
    );
    job.wrap = egui::text::TextWrapping {
        max_width: width,
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    let galley = p.layout_job(job);
    let y = rect.center().y - galley.size().y / 2.0;
    p.galley(egui::pos2(text_left, y), galley, chrome.fg);

    let clicked = response.clicked();
    if clicked {
        open = !open;
    }
    let response = response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text_at_pointer(if open {
            "hide the full prompt"
        } else {
            "show the full prompt"
        });
    let _ = response;

    if open {
        let ctx = ui.ctx().clone();
        // Never taller than most of what is left: the answer is still there
        // to be read under it.
        let below = ui.available_rect_before_wrap();
        let max_height = (below.height() * 0.6).max(60.0);
        let popup = egui::Area::new(open_id.with("full"))
            .order(egui::Order::Foreground)
            .fixed_pos(rect.left_bottom())
            .show(&ctx, |ui| {
                egui::Frame::new()
                    .fill(fill)
                    .stroke(Stroke::new(1.0, mix(chrome.panel, chrome.fg, 0.25)))
                    .inner_margin(egui::Margin {
                        left: 13,
                        right: 10,
                        top: 6,
                        bottom: 8,
                    })
                    .show(ui, |ui| {
                        let inner = rect.width() - 23.0;
                        ui.set_width(inner);
                        egui::ScrollArea::vertical()
                            .max_height(max_height)
                            .show(ui, |ui| {
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(prompt)
                                            .font(font.clone())
                                            .color(chrome.fg),
                                    )
                                    .wrap()
                                    .selectable(true),
                                );
                            });
                    });
            });
        // A click anywhere else puts it away, as a menu would.
        let elsewhere = ctx.input(|i| i.pointer.any_click())
            && !clicked
            && ctx
                .input(|i| i.pointer.interact_pos())
                .is_some_and(|pos| !popup.response.rect.contains(pos) && !rect.contains(pos));
        if elsewhere {
            open = false;
        }
    }
    ui.data_mut(|d| d.insert_temp(open_id, open));
    clicked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_prompt_becomes_one_line() {
        assert_eq!(
            one_line("  fix the build\n\nthen  run\tthe tests \n", 100),
            "fix the build then run the tests"
        );
    }

    #[test]
    fn a_long_prompt_is_cut_with_an_ellipsis() {
        assert_eq!(one_line("abcdef", 6), "abcdef", "exactly max is not cut");
        assert_eq!(one_line("abcdefg", 6), "abcdef…");
        assert_eq!(one_line("abc def", 3), "abc…", "cut at the space");
        assert_eq!(
            one_line("abc def", 4),
            "abc…",
            "no space before the ellipsis"
        );
        assert_eq!(one_line("אבג דהו", 5), "אבג ד…", "characters, not bytes");
        assert_eq!(one_line("", 5), "");
    }
}
