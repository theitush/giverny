//! The user's last prompt, pinned over the top of a Claude tab's terminal
//! once it has scrolled out of view.
//!
//! A long answer scrolls the question that started it off the screen, and
//! Claude Code has no way to keep it in view. One line here, cut to fit, and
//! the whole prompt below it on a click. While the prompt is still on screen
//! there is nothing to pin, and no bar. The terminal never moves for it:
//! the bar and the full prompt both float over the grid, so neither showing
//! them nor hiding them resizes the PTY and makes Claude redraw.

use eframe::egui::{self, FontId, Rect, Sense, Stroke, Vec2};
use giverny_core::tabs::TabId;

use crate::chrome::{Chrome, mix};

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

/// How much of the prompt's first line has to be found on a row to call the
/// prompt on screen: enough to tell it from another, short enough to fit on
/// the first row of a wrapped one.
const MATCH_CHARS: usize = 40;
/// A wrapped row can end early, at a word; this much of a prompt's start on
/// it still counts.
const MATCH_MIN: usize = 8;

/// Is this prompt on screen? `rows` are the terminal's visible rows, top to
/// bottom, each with whether its first cell is shaded.
///
/// Claude Code shows a sent prompt as `❯ <prompt>` on a shaded row, wrapped
/// over as many rows as it takes, so the first row holds the start of its
/// first line. The input box at the bottom starts with `❯` too, right under a
/// rule and unshaded: whatever is being typed there is not the prompt sent.
pub fn on_screen(prompt: &str, rows: &[(String, bool)]) -> bool {
    let Some(first) = prompt.lines().map(str::trim).find(|l| !l.is_empty()) else {
        return false;
    };
    let want: Vec<char> = one_line(first, usize::MAX).chars().collect();
    let k = want.len().min(MATCH_CHARS);
    let mut under_rule = false;
    for (text, shaded) in rows {
        let row = text.trim();
        let in_input = under_rule && !shaded;
        under_rule = !row.is_empty() && row.chars().all(|c| c == '─');
        if in_input {
            continue;
        }
        let Some(rest) = row.strip_prefix('❯').or_else(|| row.strip_prefix('>')) else {
            continue;
        };
        let got: Vec<char> = one_line(rest, usize::MAX).chars().collect();
        let found = if got.len() >= k {
            got[..k] == want[..k]
        } else {
            got.len() >= MATCH_MIN && want.starts_with(&got)
        };
        if found {
            return true;
        }
    }
    false
}

fn open_id(tab: TabId) -> egui::Id {
    egui::Id::new(("giverny-prompt-bar", tab.0))
}

/// The bar is not shown: the full prompt it opens goes with it, and the bar
/// comes back closed.
pub fn hide(ctx: &egui::Context, tab: TabId) {
    ctx.data_mut(|d| d.remove::<bool>(open_id(tab)));
}

/// Draw the bar for `tab` over the top row of the terminal at `over`, `row`
/// points high. Returns true when it was clicked, so the caller can hand the
/// keyboard back to the terminal.
///
/// Over the grid rather than above it: the bar comes and goes as the prompt
/// scrolls in and out of view, and a bar that took a row of the layout would
/// resize the terminal each time, and make Claude redraw. The row it covers
/// is never the prompt's: the bar is only up while the prompt is off screen.
pub fn show(
    ctx: &egui::Context,
    chrome: &Chrome,
    over: Rect,
    row: f32,
    tab: TabId,
    prompt: &str,
) -> bool {
    let open_id = open_id(tab);
    let mut open = ctx.data(|d| d.get_temp::<bool>(open_id).unwrap_or(false));
    let height = row.max(16.0);
    let size = Vec2::new(over.width(), height);
    let fill = mix(chrome.panel, chrome.fg, 0.10);

    let bar = egui::Area::new(open_id.with("bar"))
        .order(egui::Order::Middle)
        .fixed_pos(over.min)
        .constrain(false)
        .show(ctx, |ui| {
            // Clicks only: a focusable bar would take the keyboard from the
            // terminal under it.
            let (rect, response) = ui.allocate_exact_size(size, Sense::CLICK);
            let hovered = response.hovered();
            let p = ui.painter_at(rect);
            let fill = if hovered || open {
                mix(chrome.panel, chrome.fg, 0.16)
            } else {
                fill
            };
            p.rect_filled(rect, 0.0, fill);
            // An accent edge, as the rail marks its active tab: this is
            // yours, not Claude's output. A hairline under it keeps it apart
            // from the row below.
            p.rect_filled(
                Rect::from_min_size(rect.min, Vec2::new(3.0, rect.height())),
                0.0,
                chrome.accent,
            );
            p.hline(
                rect.x_range(),
                rect.max.y - 0.5,
                Stroke::new(1.0, mix(chrome.panel, chrome.fg, 0.25)),
            );

            let font = FontId::monospace(12.0);
            let marker = if open { "▴" } else { "▾" };
            let marker_rect = p.text(
                egui::pos2(rect.max.x - 10.0, rect.center().y),
                egui::Align2::RIGHT_CENTER,
                marker,
                font.clone(),
                chrome.dim,
            );
            let text_left = rect.min.x + 10.0;
            let width = (marker_rect.min.x - 10.0 - text_left).max(0.0);
            let mut job = egui::text::LayoutJob::single_section(
                one_line(prompt, LINE_CHARS),
                egui::TextFormat::simple(font, chrome.fg),
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
            response.on_hover_cursor(egui::CursorIcon::PointingHand)
        });
    let rect = bar.response.rect;
    let clicked = bar.inner.clicked();
    if clicked {
        open = !open;
    }

    if open {
        // Never taller than most of the terminal: the answer is still there
        // to be read under it.
        let max_height = (over.height() * 0.6).max(60.0);
        let font = FontId::monospace(12.0);
        let popup = egui::Area::new(open_id.with("full"))
            .order(egui::Order::Foreground)
            .fixed_pos(rect.left_bottom())
            .show(ctx, |ui| {
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
                        ui.set_width(rect.width() - 25.0);
                        egui::ScrollArea::vertical()
                            .max_height(max_height)
                            .show(ui, |ui| {
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(prompt).font(font).color(chrome.fg),
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
    ctx.data_mut(|d| d.insert_temp(open_id, open));
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

    fn screen(rows: &[(&str, bool)]) -> Vec<(String, bool)> {
        rows.iter().map(|(t, s)| (format!("{t:<60}"), *s)).collect()
    }

    const INPUT_BOX: [(&str, bool); 3] = [
        ("────────────────────────────────────────", false),
        ("❯ ", false),
        ("────────────────────────────────────────", false),
    ];

    #[test]
    fn a_prompt_on_screen_needs_no_bar() {
        let prompt = "fix the build\nthen run the tests";
        let mut rows = vec![
            ("● earlier output", false),
            ("❯ fix the build", true),
            ("  then run the tests", true),
            ("", false),
            ("● Done.", false),
        ];
        rows.extend(INPUT_BOX);
        assert!(on_screen(prompt, &screen(&rows)));
    }

    #[test]
    fn a_prompt_scrolled_off_wants_the_bar() {
        let mut rows = vec![("27 twenty-seven", false), ("28 twenty-eight", false)];
        rows.extend(INPUT_BOX);
        assert!(!on_screen("List the numbers 1 to 60", &screen(&rows)));
        // Another prompt on screen is not this one.
        let mut rows = vec![("❯ count down from 5", true)];
        rows.extend(INPUT_BOX);
        assert!(!on_screen("List the numbers 1 to 60", &screen(&rows)));
    }

    #[test]
    fn a_wrapped_prompt_is_found_by_its_first_row() {
        let prompt = "List the numbers 1 to 60, one per line, each followed by its English name.";
        // Wrapped at the terminal's width, mid-sentence.
        let rows = screen(&[
            ("❯ List the numbers 1 to 60, one per line, each", true),
            ("  followed by its English name.", true),
        ]);
        assert!(on_screen(prompt, &rows));
        // Wrapped early, at a word, in a narrow terminal.
        let rows = screen(&[("❯ List the numbers 1 to", true), ("  60, one per", true)]);
        assert!(on_screen(prompt, &rows));
        // Only its tail on screen: the start scrolled off.
        let rows = screen(&[("  followed by its English name.", true)]);
        assert!(!on_screen(prompt, &rows));
    }

    #[test]
    fn the_same_text_in_the_input_box_is_not_the_prompt() {
        let rows = screen(&[
            ("● output", false),
            ("────────────────────────────────────────", false),
            ("❯ fix the build", false),
            ("────────────────────────────────────────", false),
        ]);
        assert!(!on_screen("fix the build", &rows));
        // A sent prompt right under a rule is still shaded, and still counts.
        let rows = screen(&[
            ("────────────────────────────────────────", false),
            ("❯ fix the build", true),
        ]);
        assert!(on_screen("fix the build", &rows));
        assert!(!on_screen("", &rows), "no prompt, nothing to find");
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
