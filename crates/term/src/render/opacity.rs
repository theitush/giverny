//! See-through backgrounds (`window.opacity`).
//!
//! Only the surfaces the window is made of get the alpha: the terminal's own
//! background and the panels around it. Everything drawn on them — glyphs,
//! the cursor, selection, cells a program coloured — is painted at full
//! alpha, and egui's premultiplied blending then gives a glyph's edge the
//! right mix of text and desktop with no dark fringe. That is also why
//! nothing here multiplies a colour that is not opaque to begin with.

use egui::Color32;

/// `colour` as a background at `opacity` (0.0-1.0), premultiplied the way
/// egui expects. At 1.0 it is `colour` itself, bit for bit, so a window that
/// never asked for this paints exactly what it always did.
pub fn see_through(colour: Color32, opacity: f32) -> Color32 {
    if opacity >= 1.0 || opacity.is_nan() {
        return colour;
    }
    let alpha = (opacity.max(0.0) * colour.a() as f32).round() as u8;
    let [r, g, b, _] = colour.to_srgba_unmultiplied();
    Color32::from_rgba_unmultiplied(r, g, b, alpha)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::theme::Theme;

    #[test]
    fn solid_is_exactly_todays_colour() {
        for name in Theme::NAMES {
            let bg = Theme::by_name(name).bg;
            assert_eq!(see_through(bg, 1.0), bg, "{name}");
            assert_eq!(see_through(bg, 1.5), bg, "{name}: above 1.0");
            assert_eq!(see_through(bg, f32::NAN), bg, "{name}: NaN");
        }
    }

    #[test]
    fn below_one_only_the_alpha_changes() {
        for name in Theme::NAMES {
            let bg = Theme::by_name(name).bg;
            for opacity in [0.5, 0.9, 0.92, 0.95] {
                let c = see_through(bg, opacity);
                assert_eq!(
                    c.a(),
                    (opacity * 255.0).round() as u8,
                    "{name} at {opacity}"
                );
                // Premultiplied storage loses a little precision; the hue the
                // compositor sees must still be the theme's, within a step.
                let [r, g, b, _] = c.to_srgba_unmultiplied();
                for (got, want) in [(r, bg.r()), (g, bg.g()), (b, bg.b())] {
                    assert!(
                        got.abs_diff(want) <= 1,
                        "{name} at {opacity}: {got} vs {want}"
                    );
                }
            }
        }
    }

    #[test]
    fn premultiplied_never_exceeds_its_alpha() {
        // A premultiplied channel above alpha is additive light: on the
        // compositor it shows up as a glow, not a tint.
        for opacity in [0.5, 0.92] {
            let c = see_through(Color32::WHITE, opacity);
            assert!(c.r() <= c.a() && c.g() <= c.a() && c.b() <= c.a());
        }
    }
}
