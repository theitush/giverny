//! The user's last prompt, pinned over the top of a Claude tab's terminal
//! once it has scrolled out of view.
//!
//! A long answer scrolls the question that started it off the screen, and
//! Claude Code has no way to keep it in view. One line here, cut to fit, and
//! the whole prompt below it on a click. While the prompt is still on screen
//! there is nothing to pin, and no bar. The terminal never moves for it:
//! the bar and the full prompt both float over the grid, so neither showing
//! them nor hiding them resizes the PTY and makes Claude redraw.

use eframe::egui::{self, Color32, FontId, Rect, Sense, Stroke, Vec2};
use giverny_core::tabs::TabId;

use crate::chrome::{Chrome, mix};

/// The most of a prompt the one line ever lays out. It is cut to the width
/// anyway; this only spares laying out a pasted log to show its first words.
const LINE_CHARS: usize = 400;

/// A prompt as one line: line breaks and runs of whitespace become single
/// spaces, and anything past `max` characters becomes an ellipsis.
pub fn one_line(prompt: &str, max: usize) -> String {
    one_line_cut(prompt, max).0
}

/// [`one_line`], and whether it had to cut.
fn one_line_cut(prompt: &str, max: usize) -> (String, bool) {
    let mut out = String::new();
    let mut count = 0;
    for word in prompt.split_whitespace() {
        if count > 0 {
            if count == max {
                return (cut(out), true);
            }
            out.push(' ');
            count += 1;
        }
        for ch in word.chars() {
            if count == max {
                return (cut(out), true);
            }
            out.push(ch);
            count += 1;
        }
    }
    (out, false)
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
///
/// The top row does not count: it is the one the bar covers. Scrolled back,
/// Claude Code pins the turn's prompt there itself, in its own colours, and
/// counting it hid the bar and let that row take its place, a different
/// grey, every time the view moved off the bottom.
pub fn on_screen(prompt: &str, rows: &[(String, bool)]) -> bool {
    let Some(first) = prompt.lines().map(str::trim).find(|l| !l.is_empty()) else {
        return false;
    };
    let want: Vec<char> = one_line(first, usize::MAX).chars().collect();
    let k = want.len().min(MATCH_CHARS);
    let mut under_rule = false;
    for (text, shaded) in rows.iter().skip(1) {
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

/// Is there more to the prompt than the bar shows? Only then does a click
/// open it: a prompt of one line that fits has nothing more to show.
/// `cut` is whether the line shown was cut short, by the width or the cap.
pub fn expandable(prompt: &str, cut: bool) -> bool {
    cut || prompt.lines().filter(|l| !l.trim().is_empty()).count() > 1
}

/// The bar's one colour: the same closed or open, hovered or not, at the
/// bottom or scrolled back. Opaque, so the grid scrolling under it never
/// shows through.
pub fn fill(chrome: &Chrome) -> Color32 {
    mix(chrome.panel, chrome.fg, 0.10)
}

/// Draw the bar for `tab` over the top row of the terminal at `over`, `row`
/// points high. Returns true when it was clicked, so the caller can hand the
/// keyboard back to the terminal.
///
/// Over the grid rather than above it: the bar comes and goes as the prompt
/// scrolls in and out of view, and a bar that took a row of the layout would
/// resize the terminal each time, and make Claude redraw. The row it covers
/// is never the prompt's: the bar is only up while the prompt is off screen.
///
/// Open, the bar *becomes* the whole prompt, one panel from the same top
/// edge: the line it showed is the panel's first, not a second copy above it.
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
    let fill = fill(chrome);
    let rule = mix(chrome.panel, chrome.fg, 0.25);
    let font = FontId::monospace(12.0);
    // Where the one line's text starts, and the room the marker keeps. The
    // room is kept whether or not there is a marker, so a prompt that fits
    // is decided at the same width either way.
    const LEFT: f32 = 10.0;
    const RIGHT: f32 = 26.0;

    // The one line, laid out at this width: whether it was cut is what
    // says there is more to see, and a resize can change the answer.
    let (line, capped) = one_line_cut(prompt, LINE_CHARS);
    let mut job = egui::text::LayoutJob::single_section(
        line,
        egui::TextFormat::simple(font.clone(), chrome.fg),
    );
    job.wrap = egui::text::TextWrapping {
        max_width: (over.width() - LEFT - RIGHT).max(0.0),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    let galley = ctx.fonts_mut(|f| f.layout_job(job));
    let more = expandable(prompt, capped || galley.elided);
    if !more {
        open = false;
    }

    let shown = egui::Area::new(open_id.with("bar"))
        .order(if open {
            egui::Order::Foreground
        } else {
            egui::Order::Middle
        })
        .fixed_pos(over.min)
        .constrain(false)
        // egui fades an area in each time it reappears, and this one
        // reappears every time the prompt scrolls out of view: mid-scroll the
        // bar was half see-through, a different colour each frame.
        .fade_in(false)
        .show(ctx, |ui| {
            let rect = if open {
                // The prompt in full, its first line where the bar's was.
                let max_height = (over.height() * 0.6).max(60.0);
                let pad = ((height - 14.0) / 2.0).round().clamp(1.0, 8.0) as i8;
                egui::Frame::new()
                    .fill(fill)
                    .inner_margin(egui::Margin {
                        left: LEFT as i8,
                        right: RIGHT as i8,
                        top: pad,
                        bottom: pad.max(6),
                    })
                    .show(ui, |ui| {
                        ui.set_width(over.width() - LEFT - RIGHT);
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
                                    .selectable(false),
                                );
                            });
                    })
                    .response
                    .rect
            } else {
                let (rect, _) =
                    ui.allocate_exact_size(Vec2::new(over.width(), height), Sense::hover());
                let p = ui.painter_at(rect);
                p.rect_filled(rect, 0.0, fill);
                let y = rect.center().y - galley.size().y / 2.0;
                p.galley(egui::pos2(rect.min.x + LEFT, y), galley, chrome.fg);
                rect
            };
            let p = ui.painter();
            // A hairline under it keeps it apart from the row below.
            p.hline(rect.x_range(), rect.max.y - 0.5, Stroke::new(1.0, rule));
            // Clicks only, anywhere on it: a focusable bar would take the
            // keyboard from the terminal under it. Sensed even with nothing to
            // open, so the click hands the keyboard back to the terminal; it
            // just does nothing else, and does not look like it would.
            let response = ui.interact(rect, open_id.with("click"), Sense::CLICK);
            if !more {
                return response;
            }
            p.text(
                egui::pos2(rect.max.x - 10.0, rect.min.y + height / 2.0),
                egui::Align2::RIGHT_CENTER,
                if open { "▴" } else { "▾" },
                font.clone(),
                chrome.dim,
            );
            response.on_hover_cursor(egui::CursorIcon::PointingHand)
        });
    let rect = shown.response.rect;
    let clicked = shown.inner.clicked();
    if clicked && more {
        open = !open;
    } else if open {
        // A click anywhere else puts it away, as a menu would.
        let elsewhere = ctx.input(|i| i.pointer.any_click())
            && ctx
                .input(|i| i.pointer.interact_pos())
                .is_some_and(|pos| !rect.contains(pos));
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
        let mut rows = vec![("", false), ("❯ count down from 5", true)];
        rows.extend(INPUT_BOX);
        assert!(!on_screen("List the numbers 1 to 60", &screen(&rows)));
    }

    #[test]
    fn a_wrapped_prompt_is_found_by_its_first_row() {
        let prompt = "List the numbers 1 to 60, one per line, each followed by its English name.";
        // Wrapped at the terminal's width, mid-sentence.
        let rows = screen(&[
            ("● earlier", false),
            ("❯ List the numbers 1 to 60, one per line, each", true),
            ("  followed by its English name.", true),
        ]);
        assert!(on_screen(prompt, &rows));
        // Wrapped early, at a word, in a narrow terminal.
        let rows = screen(&[
            ("", false),
            ("❯ List the numbers 1 to", true),
            ("  60, one per", true),
        ]);
        assert!(on_screen(prompt, &rows));
        // Only its tail on screen: the start scrolled off.
        let rows = screen(&[("  followed by its English name.", true)]);
        assert!(!on_screen(prompt, &rows));
    }

    #[test]
    fn the_top_row_is_under_the_bar() {
        // Scrolled back in Claude Code: it pins the turn's prompt on the top
        // row itself. The bar covers that row, in its own colour.
        let mut rows = vec![("❯ List the numbers 1 to 80", true), ("20 400", false)];
        rows.extend(INPUT_BOX);
        assert!(!on_screen("List the numbers 1 to 80", &screen(&rows)));
        // One row lower, it is the prompt itself, in view.
        let mut rows = vec![
            ("❯ an older prompt", true),
            ("❯ List the numbers 1 to 80", true),
        ];
        rows.extend(INPUT_BOX);
        assert!(on_screen("List the numbers 1 to 80", &screen(&rows)));
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
            ("● output", false),
            ("────────────────────────────────────────", false),
            ("❯ fix the build", true),
        ]);
        assert!(on_screen("fix the build", &rows));
        assert!(!on_screen("", &rows), "no prompt, nothing to find");
    }

    /// One opaque colour, in every theme: nothing under the bar shows
    /// through it as the grid scrolls.
    #[test]
    fn the_bar_is_one_opaque_colour() {
        use giverny_term::render::theme::Theme;
        for name in Theme::NAMES {
            let chrome = Chrome::from_theme(&Theme::by_name(name));
            assert_eq!(fill(&chrome).a(), 255, "{name}");
            assert_ne!(fill(&chrome), chrome.panel, "{name}: apart from the panel");
        }
    }

    #[test]
    fn only_a_prompt_with_more_to_show_opens() {
        assert!(!expandable("fix the build", false), "fits: nothing to open");
        assert!(
            !expandable("  fix the build \n\n", false),
            "blank lines are not more"
        );
        assert!(expandable("fix the build", true), "cut to the width");
        assert!(
            expandable("fix the build\nthen test", false),
            "a second line"
        );
        assert_eq!(one_line_cut("abcdefg", 6), ("abcdef…".to_string(), true));
        assert_eq!(one_line_cut("abc", 6), ("abc".to_string(), false));
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
