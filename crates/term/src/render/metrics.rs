//! Font discovery (fontdb) and integer physical-pixel cell metrics.
//!
//! Cell metrics are always whole pixels: every glyph origin lands on the
//! pixel grid, which removes subpixel positioning from the atlas key and
//! avoids background-seam shimmer between cells.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use fontdb::{Database, Family, Query, Source as DbSource, Stretch, Style, Weight};
use swash::FontRef;

/// Synthetic style bits carried into the atlas key.
pub const SYNTH_BOLD: u8 = 1 << 0;
pub const SYNTH_ITALIC: u8 = 1 << 1;

/// Default primary family candidates, tried in order before generic monospace.
const PRIMARY_CANDIDATES: &[&str] = &[
    "JetBrainsMono Nerd Font Mono",
    "JetBrainsMono Nerd Font",
    "JetBrains Mono",
    "Fira Code",
    "Cascadia Code",
    "DejaVu Sans Mono",
];

/// Fallback faces appended after the primary family styles: symbols first,
/// then — after the bundled Hebrew faces — scripts and the generic faces.
const SYMBOL_FALLBACKS: &[&str] = &[
    "Symbols Nerd Font Mono",
    "Symbols Nerd Font",
    "PowerlineSymbols",
    "Noto Color Emoji",
    "Noto Emoji",
];
const FALLBACK_CANDIDATES: &[&str] = &[
    // Arabic before the generic faces below: DejaVu Sans *Mono* has none, so
    // it fell through to the proportional DejaVu Sans, drawn into monospace
    // cells — legible in the sense that a shape appeared. These are
    // script-specific, so they can only ever serve the script they cover.
    "Noto Naskh Arabic",
    "Noto Sans Arabic",
    "DejaVu Sans Mono",
    "DejaVu Sans",
];

/// Hebrew comes with Giverny: Cascadia Mono's, drawn by Liron Lavi Turkenich
/// for a coding font. It is monospace — every letter one cell wide, where a
/// proportional face like Noto Sans Hebrew leaves `ו` in a gap and squeezes
/// `ש` — and weighted to sit beside a coding font's Latin, where the Miriam
/// Mono that Linux usually has is a thin typewriter face. Bundled rather
/// than looked up because few systems have a good monospace Hebrew at all.
/// SIL Open Font License; see `fonts/CascadiaMono-OFL.txt`.
pub(crate) const HEBREW_REGULAR: &[u8] = include_bytes!("../../fonts/CascadiaMono-Regular.ttf");
const HEBREW_BOLD: &[u8] = include_bytes!("../../fonts/CascadiaMono-Bold.ttf");

/// Hebrew letters, points and punctuation, and the Hebrew presentation
/// forms: what the bundled faces are used for, and all they are used for —
/// they cover Latin, Arabic and symbols too, and taking those over would
/// change how text that looked fine yesterday looks today.
pub fn is_hebrew(c: char) -> bool {
    matches!(c, '\u{0590}'..='\u{05FF}' | '\u{FB1D}'..='\u{FB4F}')
}

/// One loaded font face (owned bytes + collection index).
pub struct LoadedFont {
    pub family: String,
    data: Arc<Vec<u8>>,
    index: u32,
}

impl LoadedFont {
    pub fn as_font(&self) -> Option<FontRef<'_>> {
        FontRef::from_index(&self.data, self.index as usize)
    }
}

/// A glyph resolution: which loaded face, which glyph, which synthesis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resolved {
    pub slot: u16,
    pub glyph: u16,
    pub synth: u8,
}

/// Resolved set of faces. Slot 0 is the primary regular face; real
/// bold/italic/bold-italic faces (when the family has them) plus fallbacks
/// follow. Not `Sync`: lives on the UI thread.
pub struct FontSet {
    fonts: Vec<LoadedFont>,
    bold: Option<u16>,
    italic: Option<u16>,
    bold_italic: Option<u16>,
    /// The bundled Hebrew faces: regular and bold.
    hebrew: Option<(u16, Option<u16>)>,
    /// Their letters' width, in ems: one for every letter, since they are
    /// monospace.
    hebrew_advance: f32,
    /// Lazy charmap memo: (slot, char) → glyph id (0 = absent).
    charmap_cache: RefCell<HashMap<(u16, char), u16>>,
}

impl FontSet {
    /// Load fonts from the system database. `preferred` (from config) is
    /// tried before the built-in candidates.
    pub fn load(preferred: Option<&str>) -> anyhow::Result<FontSet> {
        let mut db = Database::new();
        db.load_system_fonts();
        Self::load_from_db(&db, preferred)
    }

    fn load_from_db(db: &Database, preferred: Option<&str>) -> anyhow::Result<FontSet> {
        let mut families: Vec<Family<'_>> = Vec::new();
        if let Some(name) = preferred {
            families.push(Family::Name(name));
        }
        families.extend(PRIMARY_CANDIDATES.iter().map(|n| Family::Name(n)));
        families.push(Family::Monospace);

        let query = |weight: Weight, style: Style, fams: &[Family<'_>]| {
            db.query(&Query {
                families: fams,
                weight,
                stretch: Stretch::Normal,
                style,
            })
        };

        let regular_id = query(Weight::NORMAL, Style::Normal, &families)
            .ok_or_else(|| anyhow::anyhow!("no usable monospace font found on this system"))?;

        let mut seen: HashSet<fontdb::ID> = HashSet::new();
        let mut fonts: Vec<LoadedFont> = Vec::new();

        let regular = push_face(db, regular_id, &mut seen, &mut fonts)
            .ok_or_else(|| anyhow::anyhow!("failed to load font data for primary face"))?;
        debug_assert_eq!(regular, 0);

        // Real styled faces of the *resolved* primary family only.
        let primary_family = fonts[0].family.clone();
        let prim = [Family::Name(primary_family.as_str())];
        let bold = query(Weight::BOLD, Style::Normal, &prim)
            .and_then(|id| push_face(db, id, &mut seen, &mut fonts));
        let italic = query(Weight::NORMAL, Style::Italic, &prim)
            .and_then(|id| push_face(db, id, &mut seen, &mut fonts));
        let bold_italic = query(Weight::BOLD, Style::Italic, &prim)
            .and_then(|id| push_face(db, id, &mut seen, &mut fonts));

        let mut fallbacks = |names: &[&str], fonts: &mut Vec<LoadedFont>| {
            for name in names {
                let fams = [Family::Name(name)];
                if let Some(id) = query(Weight::NORMAL, Style::Normal, &fams) {
                    push_face(db, id, &mut seen, fonts);
                }
            }
        };
        fallbacks(SYMBOL_FALLBACKS, &mut fonts);
        // Ahead of every system face with Hebrew in it, so that the rail and
        // prompt bar — which take faces in this order — draw Hebrew with it
        // too.
        let hebrew = push_bundled(HEBREW_REGULAR, &mut fonts)
            .map(|regular| (regular, push_bundled(HEBREW_BOLD, &mut fonts)));
        let hebrew_advance = hebrew
            .and_then(|(regular, _)| fonts[regular as usize].as_font())
            .map_or(0.0, |font| {
                let alef = font.charmap().map('\u{05D0}');
                let units = font.metrics(&[]).units_per_em as f32;
                font.glyph_metrics(&[]).advance_width(alef) / units
            });
        fallbacks(FALLBACK_CANDIDATES, &mut fonts);

        Ok(FontSet {
            fonts,
            bold,
            italic,
            bold_italic,
            hebrew,
            hebrew_advance,
            charmap_cache: RefCell::new(HashMap::new()),
        })
    }

    pub fn font(&self, slot: u16) -> &LoadedFont {
        &self.fonts[slot as usize]
    }

    pub fn primary(&self) -> &LoadedFont {
        &self.fonts[0]
    }

    /// Raw bytes of every loaded face, primary first — so the UI layer can
    /// install the same fonts egui uses for chrome. Without this the rail
    /// cannot draw braille spinners, ⚑, ✓ or other symbols: egui's built-in
    /// fonts don't cover them and they render as tofu boxes.
    pub fn face_bytes(&self) -> Vec<(String, Vec<u8>)> {
        self.fonts
            .iter()
            .enumerate()
            .map(|(i, f)| {
                (
                    format!("giverny-face-{i}-{}", f.family),
                    f.data.as_ref().clone(),
                )
            })
            .collect()
    }

    fn glyph_in(&self, slot: u16, ch: char) -> u16 {
        if let Some(&g) = self.charmap_cache.borrow().get(&(slot, ch)) {
            return g;
        }
        let g = self.fonts[slot as usize]
            .as_font()
            .map(|f| f.charmap().map(ch))
            .unwrap_or(0);
        self.charmap_cache.borrow_mut().insert((slot, ch), g);
        g
    }

    /// How far right to draw a glyph from `slot` so it sits in the middle of
    /// its cell. Only the bundled Hebrew faces are moved: their letters are
    /// a shade narrower than most coding fonts' cells, and left where they
    /// fall they lean against the next letter. Every other face draws where
    /// it always has.
    pub fn centring(&self, slot: u16, ppem: f32, cell_w: u32) -> f32 {
        let Some((regular, bold)) = self.hebrew else {
            return 0.0;
        };
        if slot != regular && Some(slot) != bold {
            return 0.0;
        }
        ((cell_w as f32 - self.hebrew_advance * ppem) / 2.0).round()
    }

    /// Resolve a character + style to a face slot, glyph id and synthesis
    /// flags. Walks: styled primary face → primary regular (with synthesis)
    /// → the bundled Hebrew faces, for Hebrew only → fallbacks (with
    /// synthesis).
    pub fn resolve(&self, ch: char, bold: bool, italic: bool) -> Option<Resolved> {
        let styled_slot = match (bold, italic) {
            (true, true) => self.bold_italic.or(self.bold).or(self.italic),
            (true, false) => self.bold,
            (false, true) => self.italic,
            (false, false) => None,
        };
        if let Some(slot) = styled_slot {
            let glyph = self.glyph_in(slot, ch);
            if glyph != 0 {
                // A real bold face still needs italic synth when we only had
                // bold (and vice versa) — compute what the face lacks.
                let mut synth = 0;
                if bold && Some(slot) == self.italic {
                    synth |= SYNTH_BOLD;
                }
                if italic && Some(slot) == self.bold {
                    synth |= SYNTH_ITALIC;
                }
                return Some(Resolved { slot, glyph, synth });
            }
        }

        let mut synth = 0;
        if bold {
            synth |= SYNTH_BOLD;
        }
        if italic {
            synth |= SYNTH_ITALIC;
        }
        // The primary first: a family with Hebrew of its own keeps it, since
        // that is the family that was chosen.
        let glyph = self.glyph_in(0, ch);
        if glyph != 0 {
            return Some(Resolved {
                slot: 0,
                glyph,
                synth,
            });
        }

        let hebrew = [self.hebrew.map(|h| h.0), self.hebrew.and_then(|h| h.1)];
        if let Some((regular, bold_face)) = self.hebrew
            && is_hebrew(ch)
        {
            let slot = bold_face.filter(|_| bold).unwrap_or(regular);
            let glyph = self.glyph_in(slot, ch);
            if glyph != 0 {
                let synth = if Some(slot) == bold_face {
                    synth & !SYNTH_BOLD
                } else {
                    synth
                };
                return Some(Resolved { slot, glyph, synth });
            }
        }

        for slot in (1..self.fonts.len() as u16).filter(|s| !hebrew.contains(&Some(*s))) {
            let glyph = self.glyph_in(slot, ch);
            if glyph != 0 {
                return Some(Resolved { slot, glyph, synth });
            }
        }
        None
    }
}

fn push_face(
    db: &Database,
    id: fontdb::ID,
    seen: &mut HashSet<fontdb::ID>,
    fonts: &mut Vec<LoadedFont>,
) -> Option<u16> {
    if !seen.insert(id) {
        return None;
    }
    let face = db.face(id)?;
    let family = face
        .families
        .first()
        .map(|(n, _)| n.clone())
        .unwrap_or_default();
    let font = load_face(db, id, family)?;
    fonts.push(font);
    Some((fonts.len() - 1) as u16)
}

/// A face compiled into the binary; `None` only if it fails to parse.
fn push_bundled(data: &'static [u8], fonts: &mut Vec<LoadedFont>) -> Option<u16> {
    let font = FontRef::from_index(data, 0)?;
    let family = font
        .localized_strings()
        .find_by_id(swash::StringId::Family, None)
        .map(|name| name.to_string())
        .unwrap_or_default();
    fonts.push(LoadedFont {
        family,
        data: Arc::new(data.to_vec()),
        index: 0,
    });
    Some((fonts.len() - 1) as u16)
}

fn load_face(db: &Database, id: fontdb::ID, family: String) -> Option<LoadedFont> {
    let (source, index) = db.face_source(id)?;
    let data: Arc<Vec<u8>> = match source {
        DbSource::Binary(bin) => Arc::new(bin.as_ref().as_ref().to_vec()),
        DbSource::File(path) => Arc::new(std::fs::read(path).ok()?),
        DbSource::SharedFile(_, bin) => Arc::new(bin.as_ref().as_ref().to_vec()),
    };
    // Validate the face parses before accepting it.
    FontRef::from_index(&data, index as usize)?;
    Some(LoadedFont {
        family,
        data,
        index,
    })
}

/// Integer physical-pixel cell geometry for a font at a pixel size.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CellMetrics {
    pub cell_w: u32,
    pub cell_h: u32,
    /// Baseline distance from the cell top, in pixels.
    pub baseline: u32,
    /// The exact ppem used for rasterization.
    pub ppem: f32,
}

impl CellMetrics {
    pub fn compute(font: FontRef<'_>, px: f32) -> CellMetrics {
        let m = font.metrics(&[]).scale(px);
        let gm = font.glyph_metrics(&[]).scale(px);
        let reference = font.charmap().map('M');
        let advance = if reference != 0 {
            gm.advance_width(reference)
        } else {
            px * 0.6
        };
        let cell_w = advance.round().max(1.0) as u32;
        let cell_h = (m.ascent + m.descent + m.leading).ceil().max(1.0) as u32;
        let baseline = (m.ascent + m.leading / 2.0).round().min(cell_h as f32) as u32;
        CellMetrics {
            cell_w,
            cell_h,
            baseline,
            ppem: px,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::FALLBACK_CANDIDATES;

    #[test]
    fn arabic_faces_are_tried_before_the_generic_ones() {
        // DejaVu Sans Mono covers no Arabic, so it used to land on the
        // proportional DejaVu Sans. Order is the whole fix: a
        // script-specific face has to be reached first.
        let at = |name: &str| FALLBACK_CANDIDATES.iter().position(|f| *f == name);
        let generic = at("DejaVu Sans Mono").expect("generic fallback present");
        for face in ["Noto Naskh Arabic", "Noto Sans Arabic"] {
            let pos = at(face).unwrap_or_else(|| panic!("{face} missing from fallbacks"));
            assert!(pos < generic, "{face} must be tried before DejaVu");
        }
    }

    use super::*;

    fn set() -> FontSet {
        FontSet::load(None).expect("system should have a monospace font")
    }

    #[test]
    fn loads_primary_and_metrics_are_integer() {
        let s = set();
        let font = s.primary().as_font().expect("parse primary");
        let m = CellMetrics::compute(font, 15.0);
        assert!(m.cell_w >= 4 && m.cell_w <= 40, "cell_w sane: {}", m.cell_w);
        assert!(m.cell_h >= 8 && m.cell_h <= 60, "cell_h sane: {}", m.cell_h);
        assert!(m.baseline > 0 && m.baseline <= m.cell_h);
    }

    #[test]
    fn resolves_ascii_and_caches() {
        let s = set();
        let r = s.resolve('A', false, false).expect("A must resolve");
        assert_eq!(r.slot, 0);
        assert_ne!(r.glyph, 0);
        assert_eq!(r.synth, 0);
        let r2 = s.resolve('A', false, false).unwrap();
        assert_eq!(r, r2);
    }

    #[test]
    fn bold_resolves_with_face_or_synth() {
        let s = set();
        let r = s.resolve('B', true, false).expect("bold B must resolve");
        assert_ne!(r.glyph, 0);
        if r.slot == 0 {
            assert_eq!(
                r.synth & SYNTH_BOLD,
                SYNTH_BOLD,
                "regular face ⇒ synthetic bold"
            );
        }
    }

    #[test]
    fn hebrew_is_drawn_with_the_bundled_face_and_only_hebrew_is() {
        let s = set();
        let (regular, bold) = s.hebrew.expect("the bundled faces parse");
        assert_eq!(s.font(regular).family, "Cascadia Mono");
        if s.glyph_in(0, 'ש') != 0 {
            return; // a primary with Hebrew of its own keeps it
        }
        let r = s.resolve('ש', false, false).expect("Hebrew resolves");
        assert_eq!((r.slot, r.synth), (regular, 0));
        // Bold Hebrew is the real bold face, not the regular one smeared.
        let b = s.resolve('ש', true, false).unwrap();
        assert_eq!((Some(b.slot), b.synth), (bold, 0));
        let bi = s.resolve('ש', true, true).unwrap();
        assert_eq!(bi.synth, SYNTH_ITALIC, "italic is still synthesised");
        // Everything else resolves as it did before the faces were bundled,
        // even what Cascadia Mono happens to cover.
        for ch in ['A', '─', '⠋', 'ب', 'Ж'] {
            if let Some(r) = s.resolve(ch, false, false) {
                assert!(
                    r.slot != regular && Some(r.slot) != bold,
                    "{ch:?} went to Cascadia"
                );
            }
        }
    }

    #[test]
    fn hebrew_sits_in_the_middle_of_its_cell() {
        let s = set();
        let (regular, _) = s.hebrew.unwrap();
        // A cell two pixels wider than the letter: one pixel either side.
        let font = s.font(regular).as_font().unwrap();
        let advance = font
            .glyph_metrics(&[])
            .scale(20.0)
            .advance_width(font.charmap().map('א'));
        let cell = advance.round() as u32 + 2;
        assert_eq!(s.centring(regular, 20.0, cell), 1.0);
        assert_eq!(s.centring(0, 20.0, cell), 0.0, "other faces stay put");
    }

    #[test]
    fn box_drawing_resolves_somewhere() {
        let s = set();
        // Claude Code UI uses box drawing + braille spinners heavily.
        for ch in ['─', '│', '╭', '⠋'] {
            assert!(s.resolve(ch, false, false).is_some(), "no font for {ch:?}");
        }
    }
}
