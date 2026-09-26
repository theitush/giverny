//! The header over a worker's view (giverny#82).
//!
//! While a tab's Claude Code shows a worker's view, a slim band at the top
//! of the terminal says what that worker is on: its task id and full title,
//! wrapped rather than cut, then its elapsed time, ETA, current activity and
//! tokens — the cells of its agents-pane row, as the pane drew them this
//! frame, so both change together. It is drawn over the terminal rather
//! than beside it, so it never resizes the grid (a resize would have Claude
//! Code repaint under it). A small `×` closes it for that view; the
//! keyboard stays with the terminal throughout.

use egui::text::{LayoutJob, TextFormat};
use egui::{FontId, Rect, Sense, Ui};

use giverny_core::tabs::TabId;

use crate::agents_pane::Line;
use crate::chrome::Chrome;

/// Room kept free at the band's right end for its `×`.
const CLOSE_W: f32 = 22.0;
const PAD_X: f32 = 10.0;
const PAD_Y: f32 = 5.0;
const GAP_Y: f32 = 2.0;
const TITLE_SIZE: f32 = 14.0;
const FACTS_SIZE: f32 = 12.0;

/// The facts line: the row's cells, labelled, the empty ones left out.
pub fn facts(line: &Line) -> String {
    let mut out: Vec<String> = Vec::new();
    if !line.elapsed.is_empty() {
        out.push(format!("{} elapsed", line.elapsed));
    }
    if !line.eta.is_empty() {
        out.push(format!("ETA {}", line.eta));
    }
    if !line.now.is_empty() {
        out.push(line.now.clone());
    }
    if !line.tokens.is_empty() {
        out.push(format!("{} tokens", line.tokens));
    }
    out.join("  ·  ")
}

/// Draw the band over the top of `term` (the terminal's rect). Whether its
/// `×` was clicked this frame, and where the `×` is.
pub fn show(ui: &mut Ui, term: Rect, line: &Line, chrome: &Chrome, tab: TabId) -> (bool, Rect) {
    let wrap = (term.width() - 2.0 * PAD_X - CLOSE_W).max(40.0);
    let mut job = LayoutJob::default();
    job.wrap.max_width = wrap;
    let font = FontId::proportional(TITLE_SIZE);
    if !line.id.is_empty() {
        job.append(
            &line.id,
            0.0,
            TextFormat::simple(font.clone(), chrome.accent),
        );
        job.append("  ", 0.0, TextFormat::simple(font.clone(), chrome.fg));
    }
    job.append(&line.title, 0.0, TextFormat::simple(font, chrome.fg));
    let title = ui.ctx().fonts_mut(|f| f.layout_job(job));
    let facts_text = facts(line);
    let facts = (!facts_text.is_empty()).then(|| {
        let mut job = LayoutJob::default();
        job.wrap.max_width = wrap;
        job.append(
            &facts_text,
            0.0,
            TextFormat::simple(FontId::proportional(FACTS_SIZE), chrome.dim),
        );
        ui.ctx().fonts_mut(|f| f.layout_job(job))
    });
    let facts_h = facts.as_ref().map_or(0.0, |g| GAP_Y + g.size().y);
    let height = (PAD_Y * 2.0 + title.size().y + facts_h).min(term.height() * 0.5);
    let band = Rect::from_min_size(term.min, egui::vec2(term.width(), height));

    // Registered after the terminal, so a click on the band is the band's
    // and never reaches the program under it.
    let id = egui::Id::new(("view_header", tab));
    ui.interact(band, id, Sense::click());
    let close = Rect::from_min_size(
        egui::pos2(band.max.x - CLOSE_W - 4.0, band.min.y + 3.0),
        egui::vec2(CLOSE_W, CLOSE_W - 4.0),
    );
    let close_resp = ui
        .interact(close, id.with("close"), Sense::click())
        .on_hover_text("Hide for this view")
        .on_hover_cursor(egui::CursorIcon::PointingHand);

    let painter = ui.painter_at(band);
    painter.rect_filled(band, 0.0, chrome.panel);
    painter.hline(
        band.x_range(),
        band.max.y - 0.5,
        egui::Stroke::new(1.0, chrome.dim.gamma_multiply(0.4)),
    );
    let text_at = egui::pos2(band.min.x + PAD_X, band.min.y + PAD_Y);
    painter.galley(text_at, title.clone(), chrome.fg);
    if let Some(g) = facts {
        painter.galley(
            text_at + egui::vec2(0.0, title.size().y + GAP_Y),
            g,
            chrome.dim,
        );
    }
    if close_resp.hovered() {
        painter.rect_filled(close, 3.0, chrome.dim.gamma_multiply(0.25));
    }
    painter.text(
        close.center(),
        egui::Align2::CENTER_CENTER,
        "×",
        FontId::proportional(15.0),
        if close_resp.hovered() {
            chrome.fg
        } else {
            chrome.dim
        },
    );
    (close_resp.clicked(), close)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents_pane::RowClick;
    use giverny_claude::feed::Stage;

    fn line(elapsed: &str, eta: &str, now: &str, tokens: &str) -> Line {
        Line {
            stage: Stage::Running,
            id: "giverny#82".into(),
            title: "a title".into(),
            elapsed: elapsed.into(),
            eta: eta.into(),
            now: now.into(),
            tokens: tokens.into(),
            click: RowClick {
                stage: Stage::Running,
                key: String::new(),
                agent_id: None,
                name: String::new(),
                transcript: None,
                open: None,
                brief: None,
                note: None,
                facts: Vec::new(),
            },
        }
    }

    #[test]
    fn facts_label_the_cells_and_skip_the_empty_ones() {
        assert_eq!(
            facts(&line("3:21", "12m", "Running tests", "25k")),
            "3:21 elapsed  ·  ETA 12m  ·  Running tests  ·  25k tokens"
        );
        assert_eq!(facts(&line("0:04", "", "", "")), "0:04 elapsed");
        assert_eq!(facts(&line("", "", "", "")), "");
    }
}
