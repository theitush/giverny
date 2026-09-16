//! The theme, applied to Giverny's own chrome.
//!
//! The rail, the settings screen and the overlays used to be painted in
//! hardcoded Monet colours, so picking Gruvbox recoloured the grid and left
//! everything around it unchanged. The accents here are taken from the
//! theme's own ANSI palette — a terminal theme already says what its red,
//! yellow and cyan are, and using them is what makes the chrome belong to it.

use eframe::egui::{self, Color32};
use giverny_term::render::theme::Theme;

#[derive(Debug, Clone, Copy)]
pub struct Chrome {
    /// Panel background: the theme's background, lifted slightly so the rail
    /// reads as a separate surface from the terminal beside it.
    pub panel: Color32,
    pub fg: Color32,
    /// Secondary text — paths, keys, hints.
    pub dim: Color32,
    /// Cyan: selection, headings, "live".
    pub accent: Color32,
    /// Yellow: attention, changed-from-default, warnings.
    pub amber: Color32,
    /// Red: degraded, critical.
    pub poppy: Color32,
    /// Green: healthy.
    pub green: Color32,
    /// Category colours, read from the theme in the order the Monet ones
    /// were picked: wisteria, teal, sunlight, poppy, pond, garden, rose,
    /// water. A fixed palette painted every theme's rail in Monet pastels.
    pub cats: [Color32; 8],
}

/// WCAG contrast ratio between two colours: 1.0 for identical, 21.0 for
/// black on white.
fn contrast(a: Color32, b: Color32) -> f32 {
    let lum = |c: Color32| {
        let f = |v: u8| {
            let v = v as f32 / 255.0;
            if v <= 0.039_28 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * f(c.r()) + 0.7152 * f(c.g()) + 0.0722 * f(c.b())
    };
    let (x, y) = (lum(a).max(lum(b)), lum(a).min(lum(b)));
    (x + 0.05) / (y + 0.05)
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let f = |x: u8, y: u8| {
        (x as f32 + (y as f32 - x as f32) * t)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    Color32::from_rgb(f(a.r(), b.r()), f(a.g(), b.g()), f(a.b(), b.b()))
}

impl Chrome {
    pub fn from_theme(theme: &Theme) -> Self {
        // Bright variants read better as UI accents on either polarity.
        let pick = |dark: usize, light: usize| {
            if theme.is_light() {
                theme.ansi[light]
            } else {
                theme.ansi[dark]
            }
        };
        let panel = mix(
            theme.bg,
            theme.fg,
            if theme.is_light() { 0.05 } else { 0.06 },
        );
        // Secondary text sits 45% of the way back toward the background, or
        // nearer the text where that would leave it too faint on the rail —
        // a background that is itself a colour (Workbench's blue) or a light
        // one pulls a fixed mix below what can be read.
        let dim = [0.45, 0.40, 0.35, 0.30, 0.25]
            .into_iter()
            .map(|t| mix(theme.fg, theme.bg, t))
            .find(|d| contrast(*d, panel) >= 3.0)
            .unwrap_or(theme.fg);
        Chrome {
            panel,
            fg: theme.fg,
            dim,
            accent: theme.accent.unwrap_or_else(|| pick(14, 6)),
            amber: pick(11, 3),
            poppy: pick(9, 1),
            green: pick(10, 2),
            cats: {
                let a = &theme.ansi;
                [
                    a[5],
                    a[6],
                    a[3],
                    a[1],
                    a[4],
                    a[2],
                    mix(a[9], a[13], 0.5),
                    a[14],
                ]
            },
        }
    }

    /// The colour a category with this index wears.
    pub fn category(&self, index: usize) -> Color32 {
        self.cats[index % self.cats.len()]
    }

    /// Push it into egui, so panels, text fields and buttons follow too.
    pub fn apply(&self, ctx: &egui::Context, theme: &Theme) {
        let mut v = if theme.is_light() {
            egui::Visuals::light()
        } else {
            egui::Visuals::dark()
        };
        v.panel_fill = self.panel;
        v.window_fill = self.panel;
        v.faint_bg_color = mix(self.panel, self.fg, 0.05);
        v.extreme_bg_color = theme.bg;
        // Per-widget strokes rather than `override_text_color`, which is a
        // blunt instrument: it also overrides the hyperlink colour, so links
        // come out looking like plain text.
        v.widgets.noninteractive.fg_stroke.color = self.fg;
        v.widgets.inactive.fg_stroke.color = self.fg;
        v.widgets.hovered.fg_stroke.color = self.fg;
        v.widgets.active.fg_stroke.color = self.fg;
        v.widgets.open.fg_stroke.color = self.fg;
        v.hyperlink_color = self.accent;
        v.selection.bg_fill = mix(self.panel, self.accent, 0.55);
        v.selection.stroke.color = theme.bg;
        v.widgets.noninteractive.bg_fill = self.panel;
        v.widgets.inactive.bg_fill = mix(self.panel, self.fg, 0.10);
        v.widgets.inactive.weak_bg_fill = mix(self.panel, self.fg, 0.07);
        v.widgets.hovered.bg_fill = mix(self.panel, self.fg, 0.18);
        v.widgets.hovered.weak_bg_fill = mix(self.panel, self.fg, 0.14);
        v.widgets.active.bg_fill = mix(self.panel, self.accent, 0.35);
        v.widgets.active.weak_bg_fill = mix(self.panel, self.accent, 0.28);
        ctx.set_visuals(v);
        // egui's floating scrollbars are drawn *over* the last ~10px of the
        // content, so as soon as the rail has enough tabs to scroll, the "+"
        // and close buttons at the right edge end up underneath the bar.
        // Reserve the width instead — only when a bar is actually shown, so
        // short rails keep the full width.
        ctx.all_styles_mut(|s| {
            s.spacing.scroll.floating_allocated_width = s.spacing.scroll.bar_width;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chrome_follows_the_theme_rather_than_one_palette() {
        let monet = Chrome::from_theme(&Theme::monet_dark());
        let gruvbox = Chrome::from_theme(&Theme::gruvbox());
        assert_ne!(monet.panel, gruvbox.panel, "rail background is themed");
        assert_ne!(monet.accent, gruvbox.accent, "accents are themed");
    }

    /// The house theme keeps the categories it always had; only rose, which
    /// no ANSI slot holds, comes out a shade different.
    #[test]
    fn monet_keeps_its_category_colours() {
        let c = Chrome::from_theme(&Theme::monet_dark());
        let rgb = |v: u32| Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8);
        for (i, want) in [0x9a86b8, 0x5fa3a3, 0xd9b55f, 0xc35b4e, 0x5b7fa6, 0x7ba25a]
            .into_iter()
            .enumerate()
        {
            assert_eq!(c.cats[i], rgb(want), "category {i}");
        }
    }

    /// Every built-in can be read: text against the background, and the rail's
    /// secondary text distinct from both the rail and the primary text.
    #[test]
    fn every_theme_is_readable() {
        for name in Theme::NAMES {
            let theme = Theme::by_name(name);
            let c = Chrome::from_theme(&theme);
            assert!(
                contrast(theme.fg, theme.bg) >= 7.0,
                "{name}: text on background"
            );
            assert!(contrast(c.dim, c.panel) >= 3.0, "{name}: hints on the rail");
            assert!(contrast(c.dim, c.fg) >= 1.5, "{name}: hints look like text");
            assert_ne!(c.accent, c.amber, "{name}: selection looks like attention");
        }
    }

    #[test]
    fn a_theme_can_name_its_own_accent() {
        let c = Chrome::from_theme(&Theme::workbench());
        assert_eq!(c.accent, Color32::from_rgb(0xff, 0x88, 0x00));
    }

    #[test]
    fn light_themes_get_readable_text() {
        for theme in [Theme::monet_light(), Theme::monet_dark()] {
            let c = Chrome::from_theme(&theme);
            let lum = |x: Color32| x.r() as i32 + x.g() as i32 + x.b() as i32;
            // Text must contrast with the panel it sits on, and `dim` must
            // land between the two rather than vanishing into either.
            assert!(
                (lum(c.fg) - lum(c.panel)).abs() > 150,
                "text too close to the panel"
            );
            let between =
                (lum(c.dim) - lum(c.panel)).abs() > 40 && (lum(c.dim) - lum(c.fg)).abs() > 40;
            assert!(between, "dim text is indistinguishable");
        }
    }
}
