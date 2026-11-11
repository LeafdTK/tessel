//! one font database for both measuring labels and drawing them.
//!
//! the diagram layout sizes every box from text widths. if those widths came
//! from a different font than the one that ends up drawn, labels spill out of
//! their boxes, so both sides resolve faces through the same database.

use std::{
    collections::HashMap,
    sync::{Arc, LazyLock, Mutex},
};

use usvg::fontdb::{Database, Family, ID, Query, Stretch, Style, Weight};

// present on every mac; the first one found becomes the label font.
const LABEL_FAMILIES: [&str; 3] = ["Helvetica Neue", "Helvetica", "Arial"];
const EMOJI_FAMILIES: [&str; 2] = ["Apple Color Emoji", "Noto Color Emoji"];

// how wide a character counts when no face could be resolved at all.
const UNKNOWN_ADVANCE_EM: f32 = 0.55;
const WIDTH_CACHE_ENTRIES: usize = 8192;

pub(crate) static FONTS: LazyLock<Fonts> = LazyLock::new(Fonts::load);

type FaceKey = (String, bool, bool);

pub(crate) struct Fonts {
    db: Arc<Database>,
    family: String,
    options: usvg::Options<'static>,
    faces: Mutex<HashMap<FaceKey, Option<ID>>>,
    // advance in em per (face, text); wrapping asks for the same strings repeatedly.
    widths: Mutex<HashMap<(ID, String), f32>>,
}

impl Fonts {
    fn load() -> Self {
        let mut db = Database::new();
        db.load_system_fonts();
        let family = label_family(&db);
        // generic names in a diagram's own css should land on the label font too.
        db.set_sans_serif_family(family.clone());
        let db = Arc::new(db);

        let fallbacks: Mutex<HashMap<char, Option<ID>>> = Mutex::default();
        let options = usvg::Options {
            fontdb: db.clone(),
            font_family: family.clone(),
            font_resolver: usvg::FontResolver {
                select_font: usvg::FontResolver::default_font_selector(),
                // the stock selector takes the first face in the database that
                // has any glyph for the character. on a mac that can be
                // LastResort, which draws a placeholder box for everything.
                select_fallback: Box::new(move |c, used, db| {
                    let mut fallbacks = fallbacks.lock().unwrap_or_else(|e| e.into_inner());
                    let cached = *fallbacks.entry(c).or_insert_with(|| fallback_face(db, c, &[]));
                    match cached {
                        Some(id) if used.contains(&id) => fallback_face(db, c, used),
                        other => other,
                    }
                }),
            },
            ..Default::default()
        };

        Self {
            db,
            family,
            options,
            faces: Mutex::default(),
            widths: Mutex::default(),
        }
    }

    /// the family diagrams are told to set their labels in.
    pub(crate) fn family(&self) -> &str {
        &self.family
    }

    pub(crate) fn options(&self) -> &usvg::Options<'static> {
        &self.options
    }

    /// width of one line of text in px, shaped the way it will be drawn.
    pub(crate) fn width(
        &self,
        text: &str,
        css_family: Option<&str>,
        px: f64,
        bold: bool,
        italic: bool,
    ) -> f64 {
        if text.is_empty() || !px.is_finite() || px <= 0.0 {
            return 0.0;
        }
        let em = match self.face(css_family, bold, italic) {
            Some(face) => self.advance_em(face, text),
            None => text.chars().count() as f32 * UNKNOWN_ADVANCE_EM,
        };
        f64::from(em) * px
    }

    fn face(&self, css_family: Option<&str>, bold: bool, italic: bool) -> Option<ID> {
        let key = (css_family.unwrap_or_default().to_owned(), bold, italic);
        let mut faces = self.faces.lock().unwrap_or_else(|e| e.into_inner());
        *faces.entry(key).or_insert_with(|| {
            let names = css_family.map(family_names).unwrap_or_default();
            let mut families: Vec<Family> = names.iter().map(|name| family(name)).collect();
            families.push(Family::Name(&self.family));
            self.db.query(&Query {
                families: &families,
                weight: if bold { Weight::BOLD } else { Weight::NORMAL },
                stretch: Stretch::Normal,
                style: if italic { Style::Italic } else { Style::Normal },
            })
        })
    }

    fn advance_em(&self, face: ID, text: &str) -> f32 {
        let key = (face, text.to_owned());
        let mut widths = self.widths.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(em) = widths.get(&key) {
            return *em;
        }
        let em = self
            .db
            .with_face_data(face, |data, index| shape_em(data, index, text))
            .flatten()
            .unwrap_or(text.chars().count() as f32 * UNKNOWN_ADVANCE_EM);
        if widths.len() >= WIDTH_CACHE_ENTRIES {
            widths.clear();
        }
        widths.insert(key, em);
        em
    }
}

fn shape_em(data: &[u8], index: u32, text: &str) -> Option<f32> {
    let face = rustybuzz::Face::from_slice(data, index)?;
    let units_per_em = face.units_per_em() as f32;
    let mut buffer = rustybuzz::UnicodeBuffer::new();
    buffer.push_str(text);
    let shaped = rustybuzz::shape(&face, &[], buffer);
    let units: f32 = shaped
        .glyph_infos()
        .iter()
        .zip(shaped.glyph_positions())
        .map(|(glyph, position)| {
            // a glyph this face lacks is drawn from a fallback face. those are
            // overwhelmingly emoji and cjk, which are one em wide.
            if glyph.glyph_id == 0 {
                units_per_em
            } else {
                position.x_advance as f32
            }
        })
        .sum();
    Some(units / units_per_em)
}

fn label_family(db: &Database) -> String {
    let installed = |name: &str| {
        db.query(&Query {
            families: &[Family::Name(name)],
            ..Default::default()
        })
        .is_some()
    };
    if let Some(name) = LABEL_FAMILIES.into_iter().find(|name| installed(name)) {
        return name.to_owned();
    }
    let generic = db.query(&Query {
        families: &[Family::SansSerif],
        ..Default::default()
    });
    generic
        .and_then(|id| db.face(id))
        .or_else(|| db.faces().next())
        .and_then(|face| face.families.first())
        .map(|(name, _)| name.clone())
        .unwrap_or_else(|| LABEL_FAMILIES[0].to_owned())
}

/// a face other than `used` that can draw `c`, preferring the colour emoji
/// font for pictographs.
fn fallback_face(db: &Database, c: char, used: &[ID]) -> Option<ID> {
    let usable = |id: ID| !used.contains(&id) && has_glyph(db, id, c);
    if is_emoji(c) {
        let emoji = EMOJI_FAMILIES.iter().find_map(|name| {
            db.query(&Query {
                families: &[Family::Name(name)],
                ..Default::default()
            })
        });
        if let Some(id) = emoji.filter(|id| usable(*id)) {
            return Some(id);
        }
    }
    db.faces()
        .filter(|face| !face.families.iter().any(|(name, _)| name.contains("LastResort")))
        .map(|face| face.id)
        .find(|id| usable(*id))
}

fn has_glyph(db: &Database, id: ID, c: char) -> bool {
    db.with_face_data(id, |data, index| {
        rustybuzz::Face::from_slice(data, index).is_some_and(|face| face.glyph_index(c).is_some())
    })
    .unwrap_or(false)
}

// the blocks pictographs live in; text symbols below them have ordinary glyphs.
fn is_emoji(c: char) -> bool {
    matches!(u32::from(c), 0x2600..=0x27bf | 0x2b00..=0x2bff | 0x1f000..=0x1faff)
}

/// splits a css `font-family` value into its entries, quotes removed.
fn family_names(css: &str) -> Vec<String> {
    css.split(',')
        .map(|name| name.trim().trim_matches(['"', '\'']).trim().to_owned())
        .filter(|name| !name.is_empty())
        .collect()
}

fn family(name: &str) -> Family<'_> {
    match name.to_ascii_lowercase().as_str() {
        "serif" => Family::Serif,
        "sans-serif" | "system-ui" | "ui-sans-serif" | "-apple-system" => Family::SansSerif,
        "monospace" | "ui-monospace" => Family::Monospace,
        "cursive" => Family::Cursive,
        "fantasy" => Family::Fantasy,
        _ => Family::Name(name),
    }
}
