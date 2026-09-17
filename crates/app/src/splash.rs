//! The welcome screen: the mark, the wordmark, and where to find people.
//!
//! Printed into a fresh tab as terminal *output*, not typed into the shell —
//! it leaves no line in anyone's history, and the links can be real OSC 8
//! hyperlinks, which Giverny already resolves on click.
//!
//! The flower is read from the same pixels the window icon is built from, so
//! the two can never drift apart. Two cells wide and one row tall per pixel
//! is square on a terminal grid, where a cell is twice as tall as it is wide.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::icon::{GIVERNY_H, GIVERNY_RGBA, GIVERNY_W};
use crate::update::{self, CURRENT};

const CHANGELOG: &str = include_str!("../../../CHANGELOG.md");

pub const SUPPORT: &str = "https://t.me/givernysupport";
pub const NEWS: &str = "https://t.me/givernyapp";

/// Where the right-hand column starts: the mark is 16 pixels at two cells
/// each, then a gap.
const RIGHT: usize = (GIVERNY_W * 2) as usize + 2;
/// How wide the whole thing is allowed to get. An 80-column tab is the
/// narrowest anyone is likely to open, and this has to fit inside one.
const WIDTH: usize = 78;

/// Why the welcome tab is opening.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Welcome {
    /// Nobody has run this Giverny before.
    First,
    /// A version changed under them. `from` is unknown for an install that
    /// predates this screen existing.
    Updated { from: Option<String> },
}

#[derive(Serialize, Deserialize, Default)]
struct Seen {
    version: String,
}

fn seen_path(base: &Path) -> PathBuf {
    base.join("state").join("welcome.json")
}

/// Is a welcome due, and which one?
///
/// `had_workspace` separates a first run from an upgrade by someone who has
/// been here for versions already: they have tabs saved, they have just never
/// seen this screen.
pub fn due(base: &Path, had_workspace: bool) -> Option<Welcome> {
    let seen: Seen = std::fs::read(seen_path(base))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    match seen.version.as_str() {
        "" if had_workspace => Some(Welcome::Updated { from: None }),
        "" => Some(Welcome::First),
        v if v != CURRENT => Some(Welcome::Updated {
            from: Some(v.to_string()),
        }),
        _ => None,
    }
}

/// Remember that this version said hello, so it only says it once.
pub fn mark_seen(base: &Path) {
    let path = seen_path(base);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let body = serde_json::to_vec(&Seen {
        version: CURRENT.to_string(),
    })
    .unwrap_or_default();
    if let Err(err) = std::fs::write(&path, body) {
        tracing::info!("welcome marker not saved: {err}");
    }
}

/// 5×7 pixels a letter, in the same idiom as the mark. Only the seven letters
/// the name needs; a font is not the point.
fn glyph(c: char) -> [&'static str; 7] {
    match c {
        'G' => [
            ".###.", "#...#", "#....", "#.###", "#...#", "#...#", ".###.",
        ],
        'I' => [
            "#####", "..#..", "..#..", "..#..", "..#..", "..#..", "#####",
        ],
        'V' => [
            "#...#", "#...#", "#...#", "#...#", "#...#", ".#.#.", "..#..",
        ],
        'E' => [
            "#####", "#....", "#....", "####.", "#....", "#....", "#####",
        ],
        'R' => [
            "####.", "#...#", "#...#", "####.", "#.#..", "#..#.", "#...#",
        ],
        'N' => [
            "#...#", "##..#", "##..#", "#.#.#", "#..##", "#..##", "#...#",
        ],
        'Y' => [
            "#...#", "#...#", ".#.#.", "..#..", "..#..", "..#..", "..#..",
        ],
        _ => [
            "     ", "     ", "     ", "     ", "     ", "     ", "     ",
        ],
    }
}

/// The wordmark washes from wisteria to cream down the letters, which is the
/// mark's own palette read top to bottom.
const WASH: [&str; 7] = [
    "135;116;164",
    "154;134;184",
    "168;151;194",
    "182;166;206",
    "195;182;216",
    "213;203;226",
    "231;224;238",
];

fn fg(rgb: &str) -> String {
    format!("\x1b[38;2;{rgb}m")
}

const RESET: &str = "\x1b[0m";
const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const CYAN: &str = "\x1b[36m";
const GOLD: &str = "\x1b[33m";

/// A clickable link, as an OSC 8 hyperlink with the URL as its own label.
fn hyperlink(url: &str) -> String {
    format!("\x1b]8;;{url}\x1b\\{CYAN}{url}{RESET}\x1b]8;;\x1b\\")
}

/// One row of the mark: one pixel row, two cells per pixel.
fn mark_row(y: u32) -> String {
    let mut out = String::new();
    let mut last: Option<[u8; 4]> = None;
    for x in 0..GIVERNY_W {
        let i = ((y * GIVERNY_W + x) * 4) as usize;
        let px: [u8; 4] = [
            GIVERNY_RGBA[i],
            GIVERNY_RGBA[i + 1],
            GIVERNY_RGBA[i + 2],
            GIVERNY_RGBA[i + 3],
        ];
        if px[3] == 0 {
            if last.is_some() {
                out.push_str(RESET);
                last = None;
            }
            out.push_str("  ");
            continue;
        }
        // One escape per run of colour, not per cell: the mark is bands.
        if last != Some(px) {
            out.push_str(&fg(&format!("{};{};{}", px[0], px[1], px[2])));
            last = Some(px);
        }
        out.push_str("██");
    }
    if last.is_some() {
        out.push_str(RESET);
    }
    out
}

/// One row of the wordmark, blank where the letters have no pixels.
fn word_row(y: usize) -> String {
    let mut out = fg(WASH[y]);
    for (i, c) in "GIVERNY".chars().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        for px in glyph(c)[y].chars() {
            out.push(if px == '#' { '█' } else { ' ' });
        }
    }
    out.push_str(RESET);
    out
}

/// Pad to a column, counting only what the terminal will show.
fn at_column(line: &mut String, col: usize) {
    let width = visible_width(line);
    if width < col {
        line.push_str(&" ".repeat(col - width));
    }
}

/// Printable width, ignoring escape sequences.
///
/// The two kinds end differently, and guessing costs the whole measurement: a
/// CSI runs to its first letter, while an OSC runs to a bell or a string
/// terminator — and an OSC 8 carries a URL, which is nothing but letters.
fn visible_width(s: &str) -> usize {
    let mut width = 0;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            width += 1;
            continue;
        }
        match chars.next() {
            Some('[') => {
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\x07' {
                        break;
                    }
                    if c == '\x1b' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            // `ESC \` on its own is a string terminator; anything else is a
            // two-character sequence either way.
            _ => {}
        }
    }
    width
}

/// The whole screen, ready to advance into a terminal.
pub fn render(welcome: &Welcome) -> String {
    let mut rows: Vec<String> = (0..GIVERNY_H).map(mark_row).collect();

    // The wordmark sits against the middle of the flower.
    let top = (GIVERNY_H as usize - 7) / 2;
    for (i, row) in rows.iter_mut().enumerate() {
        if (top..top + 7).contains(&i) {
            at_column(row, RIGHT);
            row.push_str(&word_row(i - top));
        }
    }
    let mut say = |row: usize, text: String| {
        if let Some(line) = rows.get_mut(row) {
            at_column(line, RIGHT);
            line.push_str(&text);
        }
    };
    match welcome {
        Welcome::First => say(
            top + 8,
            format!("{DIM}a terminal built around Claude Code{RESET}"),
        ),
        Welcome::Updated { from } => say(
            top + 8,
            match from {
                Some(from) => format!("{DIM}updated {from} → {RESET}{GOLD}{BOLD}{CURRENT}{RESET}"),
                None => format!("{DIM}updated to {RESET}{GOLD}{BOLD}{CURRENT}{RESET}"),
            },
        ),
    }
    say(
        top + 10,
        format!("{DIM}support  {RESET}{}", hyperlink(SUPPORT)),
    );
    say(
        top + 11,
        format!("{DIM}news     {RESET}{}", hyperlink(NEWS)),
    );

    let mut out = String::from("\r\n");
    for row in &rows {
        out.push_str(row);
        out.push_str("\r\n");
    }
    match welcome {
        Welcome::First => {
            out.push_str(&format!(
                "\r\n  {GOLD}ctrl+shift+t{RESET}{DIM} new tab     {RESET}\
                 {GOLD}ctrl+tab{RESET}{DIM} last tab     {RESET}\
                 {GOLD}ctrl+,{RESET}{DIM} settings{RESET}\r\n\r\n"
            ));
        }
        Welcome::Updated { from } => {
            let entries = changes_since(from.as_deref());
            if !entries.is_empty() {
                out.push_str(&format!("\r\n  {DIM}what changed{RESET}\r\n\r\n"));
                for line in entries {
                    out.push_str(&line);
                    out.push_str("\r\n");
                }
            }
            out.push_str("\r\n");
        }
    }
    out
}

/// The changelog's bullets for every version after `from`, up to this one.
///
/// One line each: the first sentence is what a bullet leads with, and the
/// rest is for whoever opens the file.
fn changes_since(from: Option<&str>) -> Vec<String> {
    const MAX: usize = 9;
    let mut out = Vec::new();
    let mut counted = 0usize;
    for (version, bullets) in sections() {
        let wanted = match from {
            // Newer than what they had, and no newer than what they have now.
            Some(from) => {
                update::is_newer(&version, from) == Some(true)
                    && update::is_newer(&version, CURRENT) != Some(true)
            }
            None => version == CURRENT,
        };
        if !wanted {
            continue;
        }
        for bullet in bullets {
            counted += 1;
            if out.len() < MAX {
                out.push(format!("  {DIM}·{RESET} {}", one_line(&bullet)));
            }
        }
    }
    if counted > out.len() {
        out.push(format!(
            "  {DIM}and {} more in the changelog{RESET}",
            counted - out.len()
        ));
    }
    out
}

/// `(version, bullets)` for each release in the changelog, newest first.
fn sections() -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    for line in CHANGELOG.lines() {
        if let Some(rest) = line.strip_prefix("## v") {
            let version = rest
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .to_string();
            out.push((version, Vec::new()));
            continue;
        }
        let Some((_, bullets)) = out.last_mut() else {
            continue;
        };
        if let Some(text) = line.strip_prefix("- ") {
            bullets.push(text.to_string());
        } else if let Some(text) = line.strip_prefix("  ")
            && let Some(last) = bullets.last_mut()
        {
            // A bullet wrapped onto the next line.
            last.push(' ');
            last.push_str(text.trim());
        }
    }
    out
}

/// A bullet's first sentence, trimmed of markdown and cut to the width.
fn one_line(bullet: &str) -> String {
    let plain = bullet.replace(['*', '`'], "");
    let mut text = plain.trim();
    if let Some(end) = text.find(". ") {
        text = &text[..end + 1];
    }
    let room = WIDTH - 4;
    if text.chars().count() <= room {
        return text.to_string();
    }
    let cut: String = text.chars().take(room - 1).collect();
    let cut = cut.rsplit_once(' ').map(|(head, _)| head).unwrap_or(&cut);
    format!("{cut}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mark_is_square_and_fits_a_narrow_tab() {
        let rendered = render(&Welcome::First);
        let rows: Vec<&str> = rendered.lines().collect();
        // Every pixel row is a text row, so the flower is as tall as it is
        // wide once each pixel is two cells across.
        let mark_rows = rows
            .iter()
            .filter(|r| r.contains("\u{2588}\u{2588}"))
            .count();
        assert!(mark_rows >= GIVERNY_H as usize, "{mark_rows} rows of mark");
        for row in &rows {
            assert!(
                visible_width(row) <= WIDTH,
                "{} columns: {row:?}",
                visible_width(row)
            );
        }
    }

    #[test]
    fn both_links_are_clickable() {
        let rendered = render(&Welcome::First);
        for url in [SUPPORT, NEWS] {
            assert!(
                rendered.contains(&format!("\x1b]8;;{url}\x1b\\")),
                "{url} is not a hyperlink"
            );
        }
    }

    /// Escape sequences take no columns; the width test above depends on it.
    #[test]
    fn width_ignores_escapes() {
        assert_eq!(visible_width("abc"), 3);
        assert_eq!(visible_width(&format!("{DIM}abc{RESET}")), 3);
        assert_eq!(visible_width(&hyperlink("https://x.example")), 17);
    }

    #[test]
    fn an_update_lists_what_changed_since_the_version_they_had() {
        let rendered = render(&Welcome::Updated {
            from: Some("0.0.1".into()),
        });
        assert!(rendered.contains("what changed"), "{rendered}");
        assert!(
            rendered.contains("more in the changelog"),
            "a long list is cut"
        );
        // The list is this release's own notes, not the whole file.
        let only_current = render(&Welcome::Updated { from: None });
        let bullets = |s: &str| s.matches('·').count();
        assert!(
            bullets(&only_current) < bullets(&rendered),
            "an unknown previous version shows this release alone"
        );
    }

    #[test]
    fn the_changelog_parses_into_versions_and_bullets() {
        let all = sections();
        assert!(all.len() > 5, "found {} releases", all.len());
        let (version, bullets) = &all[0];
        assert_eq!(version, CURRENT, "the newest section is this build");
        assert!(!bullets.is_empty(), "this release has notes");
    }

    /// A first run has never seen a version; an upgrade has seen an older one.
    #[test]
    fn the_marker_says_who_is_looking() {
        let base = std::env::temp_dir().join(format!("giverny-welcome-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("state")).unwrap();
        assert_eq!(due(&base, false), Some(Welcome::First));
        assert_eq!(
            due(&base, true),
            Some(Welcome::Updated { from: None }),
            "someone with tabs already is not new here"
        );
        mark_seen(&base);
        assert_eq!(due(&base, false), None, "said once");
        std::fs::write(
            seen_path(&base),
            serde_json::to_vec(&Seen {
                version: "0.0.1".into(),
            })
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            due(&base, true),
            Some(Welcome::Updated {
                from: Some("0.0.1".into())
            })
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}
