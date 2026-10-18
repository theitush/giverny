//! Right-to-left text, reordered for display.
//!
//! The grid stores characters in *logical* order — the order they were typed
//! or written — which is correct and is what the program on the other end
//! expects to read back. Drawing them in that order puts Hebrew and Arabic on
//! screen backwards, because RTL script runs right to left. VTE (and so GNOME
//! Terminal) applies the Unicode Bidi Algorithm per line before drawing; this
//! is the same idea, with the paragraph direction worked out per row.
//!
//! Display only: nothing here touches the grid. A screen with no RTL character
//! never reaches the algorithm, so ordinary Latin output costs one cheap scan.

use std::borrow::Cow;

use unicode_bidi::{BidiClass, Level, ParagraphBidiInfo, bidi_class};

/// Could this character start an RTL run?
///
/// A range test, not a table lookup: this runs per cell on every frame, and
/// its only job is to decide whether the real algorithm is worth running.
/// Covers Hebrew, Arabic, Syriac, Thaana, N'Ko, Samaritan, Mandaic and the
/// Arabic presentation forms.
pub fn is_rtl(c: char) -> bool {
    matches!(c,
        '\u{0590}'..='\u{08FF}'
        | '\u{FB1D}'..='\u{FDFF}'
        | '\u{FE70}'..='\u{FEFF}'
        | '\u{10800}'..='\u{10FFF}'
        | '\u{1E800}'..='\u{1EFFF}'
    )
}

pub fn row_has_rtl(chars: &[char]) -> bool {
    chars.iter().copied().any(is_rtl)
}

/// Which way a paragraph runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Ltr,
    Rtl,
}

/// The direction of the first letter, which is how Unicode (rule P2) and a
/// browser's `dir="auto"` decide a paragraph's direction. `None` with no
/// letter at all.
pub fn first_strong(chars: impl IntoIterator<Item = char>) -> Option<Dir> {
    chars.into_iter().find_map(strong)
}

fn strong(c: char) -> Option<Dir> {
    match bidi_class(c) {
        BidiClass::L => Some(Dir::Ltr),
        BidiClass::R | BidiClass::AL => Some(Dir::Rtl),
        _ => None,
    }
}

/// How one row is drawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowOrder {
    /// `to_visual[logical]`: the column a cell is drawn at.
    pub to_visual: Vec<u16>,
    /// `to_logical[visual]`: the cell drawn at a column, which is what a
    /// click on that column means.
    pub to_logical: Vec<u16>,
    /// Cells in a right-to-left run. Their brackets are drawn mirrored, and
    /// the left half of one is its logical end.
    pub rtl: Vec<bool>,
}

/// One row of the screen, as the grid holds it.
pub struct ScreenRow {
    pub chars: Vec<char>,
    /// The terminal wrapped this row onto the next (`WRAPLINE`).
    pub wraps: bool,
}

/// Order every row of a screen; `None` for a row drawn as stored.
///
/// A row runs in the direction of its first letter, so a Hebrew line is laid
/// out right to left — right-aligned, its words in reading order, its full
/// stop at the left end — and an English line with a Hebrew word in it stays
/// left to right with only that word turned round. A row that carries on the
/// paragraph above it takes that paragraph's direction instead: Claude Code
/// wraps its own text, and a Hebrew sentence whose second row happens to
/// start with `parse_args` is still a Hebrew sentence.
///
/// `rows[0]` may be the row just above the screen, so that the top row can
/// carry on its paragraph; the caller drops its entry.
pub fn order_screen(rows: &[ScreenRow]) -> Vec<Option<RowOrder>> {
    let mut out = Vec::with_capacity(rows.len());
    let mut prev_dir = None;
    for (i, row) in rows.iter().enumerate() {
        let own = first_strong(row.chars.iter().copied());
        let dir = match i.checked_sub(1) {
            Some(p) if continues(&rows[p], &row.chars) => prev_dir.or(own),
            _ => own,
        };
        prev_dir = dir;
        let reorder = dir == Some(Dir::Rtl) || row_has_rtl(&row.chars);
        out.push(reorder.then(|| order_row(&row.chars, dir.unwrap_or(Dir::Ltr))));
    }
    out
}

/// How far short of the last column a program may wrap its text and still
/// be recognised as having wrapped it: Claude Code keeps margins.
const WRAP_SLACK: usize = 6;

/// Is `this` the rest of the paragraph `prev` started?
///
/// Either the terminal wrapped `prev`, or a program did: `this` starts where
/// `prev`'s text does, opens no list item or message of its own, and its
/// first word would not have fitted at the end of `prev` — which is why it
/// is on this row. Lines that merely follow each other (code, a list) fail
/// the last test, so they keep their own directions.
fn continues(prev: &ScreenRow, this: &[char]) -> bool {
    if prev.wraps {
        return true;
    }
    if has_walls(&prev.chars) || has_walls(this) {
        return false;
    }
    let text = |c: &char| !c.is_whitespace();
    let (Some(prev_start), Some(prev_letter), Some(prev_end), Some(start)) = (
        prev.chars.iter().position(text),
        prev.chars.iter().position(|&c| strong(c).is_some()),
        prev.chars.iter().rposition(text),
        this.iter().position(text),
    ) else {
        return false;
    };
    // `⏺ text` carries on at the text, not at the bullet.
    if start < prev_start || start > prev_letter || opens_block(&this[start..]) {
        return false;
    }
    let word = this[start..].iter().take_while(|&c| text(c)).count();
    prev_end + 2 + word > this.len().saturating_sub(WRAP_SLACK)
}

/// Does a row start a block of its own: a bullet, a numbered item, or one of
/// Claude Code's message and prompt marks?
fn opens_block(text: &[char]) -> bool {
    if let [mark, ' ', ..] = text
        && matches!(mark, '-' | '*' | '•' | '⏺' | '●' | '❯' | '>' | '⎿')
    {
        return true;
    }
    let digits = text.iter().take_while(|c| c.is_ascii_digit()).count();
    digits > 0 && matches!(text.get(digits), Some('.' | ')')) && text.get(digits + 1) == Some(&' ')
}

/// Box drawing: a table's or a box's walls. They stay where the program put
/// them, and the text between each pair is ordered on its own — so a table
/// with a Hebrew row keeps its columns lined up with the rows around it.
fn is_wall(c: char) -> bool {
    matches!(c, '\u{2500}'..='\u{257F}')
}

fn has_walls(chars: &[char]) -> bool {
    chars.iter().copied().any(is_wall)
}

/// Order one row that runs in `dir`, or between its walls if it has any.
pub fn order_row(chars: &[char], dir: Dir) -> RowOrder {
    let n = chars.len();
    let mut to_visual: Vec<u16> = (0..n as u16).collect();
    let mut rtl = vec![false; n];
    if has_walls(chars) {
        let mut from = 0;
        for end in (0..n).filter(|&i| is_wall(chars[i])).chain([n]) {
            let span = &chars[from..end];
            let dir = first_strong(span.iter().copied()).unwrap_or(Dir::Ltr);
            order_span(span, dir, from, &mut to_visual, &mut rtl);
            from = end + 1;
        }
    } else {
        order_span(chars, dir, 0, &mut to_visual, &mut rtl);
    }

    let mut to_logical: Vec<u16> = (0..n as u16).collect();
    for (logical, &visual) in to_visual.iter().enumerate() {
        if let Some(slot) = to_logical.get_mut(visual as usize) {
            *slot = logical as u16;
        }
    }
    RowOrder {
        to_visual,
        to_logical,
        rtl,
    }
}

/// Reorder the cells `at..at + span.len()` within those same columns.
fn order_span(span: &[char], dir: Dir, at: usize, to_visual: &mut [u16], rtl: &mut [bool]) {
    if span.is_empty() || dir == Dir::Ltr && !row_has_rtl(span) {
        return;
    }
    // A paragraph separator inside a row would start a second paragraph;
    // a cell is a cell.
    let text: String = span
        .iter()
        .map(|&c| {
            if bidi_class(c) == BidiClass::B {
                ' '
            } else {
                c
            }
        })
        .collect();
    let level = match dir {
        Dir::Ltr => Level::ltr(),
        Dir::Rtl => Level::rtl(),
    };
    let info = ParagraphBidiInfo::new(&text, Some(level));
    // Per char, and with trailing blanks put back at the paragraph's level
    // (rule L1): a right-to-left row's padding goes to its left, so the text
    // sits against the right edge.
    let levels = info.reordered_levels_per_char(0..text.len());
    for (visual, logical) in ParagraphBidiInfo::reorder_visual(&levels)
        .into_iter()
        .enumerate()
    {
        // Defensive: a mismatch would otherwise panic on a user's screenful
        // of text. Leaving a cell in place is wrong-looking, not fatal.
        if logical < span.len() {
            to_visual[at + logical] = (at + visual) as u16;
        }
    }
    for (i, level) in levels.iter().enumerate().take(span.len()) {
        rtl[at + i] = level.is_rtl();
    }
}

/// The glyph a character is drawn with inside a right-to-left run (rule L4):
/// an opening bracket there opens to the left, so `(` takes `)`'s shape.
pub fn mirrored(c: char) -> char {
    match c {
        '(' => ')',
        ')' => '(',
        '[' => ']',
        ']' => '[',
        '{' => '}',
        '}' => '{',
        '<' => '>',
        '>' => '<',
        '«' => '»',
        '»' => '«',
        '‹' => '›',
        '›' => '‹',
        '≤' => '≥',
        '≥' => '≤',
        '⟨' => '⟩',
        '⟩' => '⟨',
        '〈' => '〉',
        '〉' => '〈',
        _ => c,
    }
}

/// A line of text for egui to draw, in the order that comes out right.
///
/// egui lays text out left to right with no bidi of its own, but it shapes
/// each run of one font with HarfBuzz's guess at the run's direction — so a
/// Hebrew word comes out letter-perfect while a Hebrew sentence comes out
/// with its words in the wrong order. So: put the whole line in drawn order,
/// then put each run of RTL letters back the way it was written, for egui's
/// shaper to turn round. `dir` is the paragraph's direction; `None` takes
/// the line's own. Text with no RTL character comes back as it was.
pub fn egui_line(text: &str, dir: Option<Dir>) -> Cow<'_, str> {
    let dir = dir.or_else(|| first_strong(text.chars()));
    if dir != Some(Dir::Rtl) && !text.chars().any(is_rtl) {
        return Cow::Borrowed(text);
    }
    let chars: Vec<char> = text.chars().collect();
    let order = order_row(&chars, dir.unwrap_or(Dir::Ltr));
    let mut out = vec![' '; chars.len()];
    for (logical, &c) in chars.iter().enumerate() {
        let c = if order.rtl[logical] { mirrored(c) } else { c };
        out[order.to_visual[logical] as usize] = c;
    }
    let mut i = 0;
    while i < out.len() {
        let run = out[i..].iter().take_while(|&&c| is_rtl(c)).count();
        out[i..i + run].reverse();
        i += run.max(1);
    }
    Cow::Owned(out.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    /// A row the width of `s`, read back the way it is drawn.
    fn drawn_as(s: &str, dir: Dir) -> String {
        let cs = chars(s);
        let order = order_row(&cs, dir);
        let mut out = vec![' '; cs.len()];
        for (logical, &visual) in order.to_visual.iter().enumerate() {
            let c = cs[logical];
            out[visual as usize] = if order.rtl[logical] { mirrored(c) } else { c };
        }
        out.into_iter().collect()
    }

    /// Rows of a screen, each padded to `cols`, read back as drawn.
    fn screen(rows: &[&str], cols: usize) -> Vec<String> {
        let rows: Vec<ScreenRow> = rows
            .iter()
            .map(|r| {
                let mut chars = chars(r);
                chars.resize(cols, ' ');
                ScreenRow {
                    chars,
                    wraps: false,
                }
            })
            .collect();
        order_screen(&rows)
            .into_iter()
            .zip(&rows)
            .map(|(order, row)| match order {
                None => row.chars.iter().collect(),
                Some(order) => {
                    let mut out = vec![' '; cols];
                    for (logical, &visual) in order.to_visual.iter().enumerate() {
                        let c = row.chars[logical];
                        out[visual as usize] = if order.rtl[logical] { mirrored(c) } else { c };
                    }
                    out.into_iter().collect()
                }
            })
            .collect()
    }

    fn rev(s: &str) -> String {
        s.chars().rev().collect()
    }

    #[test]
    fn latin_is_left_exactly_as_it_was() {
        assert_eq!(
            screen(&["cargo build --release"], 30),
            ["cargo build --release         "]
        );
        assert!(!row_has_rtl(&chars("cargo build")), "no algorithm needed");
    }

    #[test]
    fn a_hebrew_row_reads_right_to_left_from_the_right_edge() {
        let word = "שלום";
        assert!(row_has_rtl(&chars(word)));
        let got = &screen(&[word], 10)[0];
        assert_eq!(got, &format!("      {}", rev(word)));
    }

    #[test]
    fn a_hebrew_sentence_keeps_its_word_order_around_english() {
        // The case a left-to-right row gets wrong: the runs come out in
        // left-to-right order, so a Hebrew reader meets the last words first.
        // Read from the right edge, the sentence has to come out as written.
        let got = drawn_as("בדקתי את main.rs ומצאתי באג.", Dir::Rtl);
        assert_eq!(
            got,
            format!(
                ".{} {} main.rs {}",
                rev("באג"),
                rev("ומצאתי"),
                rev("בדקתי את")
            )
        );
    }

    #[test]
    fn an_english_row_with_a_hebrew_word_stays_left_to_right() {
        let got = &screen(&["git commit שלום now"], 19)[0];
        assert_eq!(got, &format!("git commit {} now", rev("שלום")));
    }

    #[test]
    fn numbers_inside_hebrew_stay_readable() {
        // Digits are left-to-right even inside an RTL run: "12", not "21".
        let d = drawn_as("שלום 12 עולם", Dir::Rtl);
        assert!(d.contains("12"), "digits reversed: {d}");
    }

    #[test]
    fn brackets_in_hebrew_face_the_text_they_enclose() {
        // Read from the right: an opening bracket, then the word.
        let d = drawn_as("(הערה)", Dir::Rtl);
        assert_eq!(d, format!("({})", rev("הערה")));
    }

    #[test]
    fn claudes_bullet_and_indent_go_to_the_right_with_the_text() {
        // `⏺ ` opens Claude's message; on a Hebrew message it is the first
        // thing read, so it sits at the right edge, and the continuation
        // row's indent becomes a margin on the right.
        let got = screen(&["⏺ שלום עולם", "  וזה המשך"], 14);
        assert_eq!(got[0], format!("   {} ⏺", rev("שלום עולם")));
        assert_eq!(got[1], format!("    {}  ", rev("וזה המשך")));
    }

    #[test]
    fn a_wrapped_row_starting_in_english_stays_in_its_hebrew_paragraph() {
        // Claude Code wraps its own text, so the second row of a Hebrew
        // sentence can start with an English word. On its own that row would
        // be left to right; it carries on a Hebrew paragraph, so it isn't.
        let cols = 24;
        let first = "⏺ בדקתי את הפונקציה";
        let second = "  parse_args והיא תקינה";
        assert!(first.chars().count() + 1 + "parse_args".len() > cols - WRAP_SLACK);
        let got = screen(&[first, second], cols);
        assert!(
            got[1].trim_start().starts_with(&rev("והיא תקינה")),
            "{:?}",
            got[1]
        );
        assert!(got[1].trim_end().ends_with("parse_args"), "{:?}", got[1]);
    }

    #[test]
    fn lines_that_merely_follow_each_other_keep_their_own_direction() {
        // Code: short lines, each its own. The English one stays put even
        // under a Hebrew comment.
        let got = screen(&["// בודק את הקלט", "check(input)"], 40);
        assert_eq!(got[1].trim_end(), "check(input)");
        // A list: the next item opens a block of its own.
        let wide = "- פריט ראשון שתופס כמעט את כל השורה";
        let cols = wide.chars().count() + 1;
        let got = screen(&[wide, "- second item"], cols);
        assert!(got[1].starts_with("- second item"), "{:?}", got[1]);
    }

    #[test]
    fn a_row_the_terminal_wrapped_carries_on_its_paragraph() {
        let mut a = chars("שלום עולם");
        a.resize(9, ' ');
        let rows = [
            ScreenRow {
                chars: a,
                wraps: true,
            },
            ScreenRow {
                chars: chars("abc def  "),
                wraps: false,
            },
        ];
        let orders = order_screen(&rows);
        let second = orders[1]
            .as_ref()
            .expect("an RTL paragraph's row is ordered");
        // Right-aligned: the padding went left.
        assert_eq!(second.to_visual[0], 2);
    }

    #[test]
    fn a_table_keeps_its_walls_where_they_are() {
        // Header and body rows in different directions: the walls must not
        // move, or the columns stop lining up.
        let row = "│ שם │ name │";
        let got = drawn_as(row, Dir::Ltr);
        let walls = |s: &str| {
            s.char_indices()
                .filter(|(_, c)| *c == '│')
                .map(|(i, _)| i)
                .collect::<Vec<_>>()
        };
        assert_eq!(walls(&got), walls(row));
        assert!(got.contains(&rev("שם")), "{got}");
    }

    #[test]
    fn every_cell_gets_exactly_one_place() {
        // A permutation, or cells would overwrite each other on screen.
        for (s, dir) in [
            ("שלום", Dir::Rtl),
            ("abc שלום def", Dir::Ltr),
            ("abc שלום def", Dir::Rtl),
            ("a", Dir::Rtl),
            ("", Dir::Rtl),
            ("│ שלום 12 │ abc │", Dir::Ltr),
            ("שלום 12 עולם abc   ", Dir::Rtl),
        ] {
            let cs = chars(s);
            let order = order_row(&cs, dir);
            assert_eq!(order.to_visual.len(), cs.len());
            let mut seen = order.to_visual.clone();
            seen.sort_unstable();
            seen.dedup();
            assert_eq!(seen.len(), cs.len(), "not a permutation for {s:?}");
            for (logical, &visual) in order.to_visual.iter().enumerate() {
                assert_eq!(order.to_logical[visual as usize] as usize, logical, "{s:?}");
            }
        }
    }

    #[test]
    fn egui_draws_a_hebrew_sentence_in_reading_order() {
        // Pins egui's side of the bargain as well as ours: if egui ever
        // grows a bidi pass of its own, this fails rather than every Hebrew
        // title on the rail quietly coming out backwards.
        use egui::epaint::text::{FontData, FontInsert, FontPriority, InsertFontFamily};
        let ctx = egui::Context::default();
        ctx.add_font(FontInsert {
            name: "hebrew".into(),
            data: FontData::from_static(super::super::metrics::HEBREW_REGULAR),
            families: vec![InsertFontFamily {
                family: egui::FontFamily::Monospace,
                priority: FontPriority::Lowest,
            }],
        });
        let _ = ctx.run_ui(Default::default(), |_| {});
        let on_screen = |text: &str| {
            let text = egui_line(text, None).into_owned();
            let galley = ctx.fonts_mut(|f| {
                f.layout_no_wrap(text, egui::FontId::monospace(14.0), egui::Color32::WHITE)
            });
            let mut glyphs: Vec<(f32, char)> = galley.rows[0]
                .glyphs
                .iter()
                .filter(|g| g.advance_width > 0.0)
                .map(|g| (g.pos.x, g.chr))
                .collect();
            glyphs.sort_by(|a, b| a.0.total_cmp(&b.0));
            glyphs.into_iter().map(|(_, c)| c).collect::<String>()
        };
        // Left to right on screen, so reading from the right gives the text.
        assert_eq!(on_screen("שלום עולם"), rev("שלום עולם"));
        assert_eq!(
            on_screen("תיקון (באג) ב-main"),
            format!("main-ב ({}) {}", rev("באג"), rev("תיקון"))
        );
        assert_eq!(
            on_screen("fix שלום bug"),
            format!("fix {} bug", rev("שלום"))
        );
        assert!(matches!(egui_line("plain title", None), Cow::Borrowed(_)));
    }
}
