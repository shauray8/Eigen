//! Text: system font lookup (fontdb), shaping (rustybuzz, a HarfBuzz port),
//! and glyph outlines tessellated into triangles (lyon). Glyphs are geometry,
//! so they stay sharp under any scale or rotation, and the same outlines can
//! be extruded for 3D type later.

use std::collections::HashMap;
use std::sync::Arc;

use lyon_tessellation::path::Path;
use lyon_tessellation::{BuffersBuilder, FillOptions, FillRule, FillTessellator, FillVertex, VertexBuffers};
use rustybuzz::ttf_parser::{self, GlyphId, Tag};

/// Triangles of one glyph in em units, y up.
pub struct GlyphMesh {
    pub vertices: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
}

/// A glyph placed relative to the text anchor, in pixels, y down.
pub struct PlacedGlyph {
    pub mesh: Arc<GlyphMesh>,
    pub origin: [f32; 2],
    /// Pixels per em.
    pub scale: f32,
    /// Point the per-letter animator rotates and scales around.
    pub pivot: [f32; 2],
    /// Index among the text's non-whitespace characters.
    pub letter: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

pub struct Spec<'a> {
    pub text: &'a str,
    pub font: &'a str,
    pub weight: f32,
    pub size: f32,
    pub align: Align,
    pub tracking: f32,
    pub leading: f32,
}

struct FaceData {
    data: Vec<u8>,
    index: u32,
}

#[derive(Default)]
pub struct Fonts {
    db: Option<fontdb::Database>,
    faces: HashMap<fontdb::ID, Arc<FaceData>>,
    resolved: HashMap<(String, u16), Option<fontdb::ID>>,
    glyphs: HashMap<(fontdb::ID, u16, u16), Arc<GlyphMesh>>,
}

const WGHT: Tag = Tag::from_bytes(b"wght");

struct PathSink(lyon_tessellation::path::path::Builder, bool);

impl ttf_parser::OutlineBuilder for PathSink {
    fn move_to(&mut self, x: f32, y: f32) {
        if self.1 {
            self.0.end(false);
        }
        self.0.begin(lyon_tessellation::math::point(x, y));
        self.1 = true;
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.0.line_to(lyon_tessellation::math::point(x, y));
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.0.quadratic_bezier_to(lyon_tessellation::math::point(x1, y1), lyon_tessellation::math::point(x, y));
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.0.cubic_bezier_to(
            lyon_tessellation::math::point(x1, y1),
            lyon_tessellation::math::point(x2, y2),
            lyon_tessellation::math::point(x, y),
        );
    }
    fn close(&mut self) {
        self.0.end(true);
        self.1 = false;
    }
}

fn tessellate(face: &ttf_parser::Face, glyph: GlyphId) -> GlyphMesh {
    let upem = face.units_per_em() as f32;
    let mut sink = PathSink(Path::builder(), false);
    if face.outline_glyph(glyph, &mut sink).is_none() {
        return GlyphMesh { vertices: Vec::new(), indices: Vec::new() };
    }
    if sink.1 {
        sink.0.end(true);
    }
    let path = sink.0.build();
    let mut buffers: VertexBuffers<[f32; 2], u32> = VertexBuffers::new();
    let options = FillOptions::tolerance(upem * 0.0002).with_fill_rule(FillRule::NonZero);
    let _ = FillTessellator::new().tessellate_path(
        &path,
        &options,
        &mut BuffersBuilder::new(&mut buffers, |v: FillVertex| {
            let p = v.position();
            [p.x / upem, p.y / upem]
        }),
    );
    GlyphMesh { vertices: buffers.vertices, indices: buffers.indices }
}

impl Fonts {
    fn db(&mut self) -> &fontdb::Database {
        self.db.get_or_insert_with(|| {
            let mut db = fontdb::Database::new();
            db.load_system_fonts();
            db.set_sans_serif_family("Helvetica Neue");
            db
        })
    }

    /// Resolves a family and weight to a face, falling back to sans-serif.
    fn resolve(&mut self, family: &str, weight: u16) -> Option<fontdb::ID> {
        if let Some(id) = self.resolved.get(&(family.to_string(), weight)) {
            return *id;
        }
        let db = self.db();
        let query = |families: &[fontdb::Family]| {
            db.query(&fontdb::Query { families, weight: fontdb::Weight(weight), ..Default::default() })
        };
        let id = query(&[fontdb::Family::Name(family)]).or_else(|| query(&[fontdb::Family::SansSerif]));
        self.resolved.insert((family.to_string(), weight), id);
        id
    }

    /// Whether `family` exists on this system (no fallback).
    pub fn has_family(&mut self, family: &str) -> bool {
        let db = self.db();
        db.query(&fontdb::Query { families: &[fontdb::Family::Name(family)], ..Default::default() }).is_some()
    }

    fn face_data(&mut self, id: fontdb::ID) -> Option<Arc<FaceData>> {
        if let Some(f) = self.faces.get(&id) {
            return Some(f.clone());
        }
        let face = self.db().with_face_data(id, |data, index| FaceData { data: data.to_vec(), index })?;
        let face = Arc::new(face);
        self.faces.insert(id, face.clone());
        Some(face)
    }

    pub fn layout(&mut self, spec: &Spec) -> Vec<PlacedGlyph> {
        let weight = spec.weight.round().clamp(1.0, 1000.0) as u16;
        let Some(id) = self.resolve(spec.font, weight) else { return Vec::new() };
        let Some(data) = self.face_data(id) else { return Vec::new() };
        let Some(mut face) = rustybuzz::Face::from_slice(&data.data, data.index) else { return Vec::new() };
        if face.variation_axes().into_iter().any(|a| a.tag == WGHT) {
            face.set_variations(&[rustybuzz::Variation { tag: WGHT, value: weight as f32 }]);
        }
        let upem = face.units_per_em() as f32;
        let s = spec.size / upem;
        let cap = face.capital_height().map_or(0.7 * upem, |c| c as f32) * s;
        let line_h = spec.size * spec.leading;
        let lines: Vec<&str> = spec.text.split('\n').collect();
        let mut baseline = (cap - (lines.len() as f32 - 1.0) * line_h) / 2.0;

        let mut out = Vec::new();
        let mut letter_base = 0;
        for line in lines {
            // Letter index for each char start (whitespace gets none).
            let mut letter_at: HashMap<usize, usize> = HashMap::new();
            let mut n = 0;
            for (byte, c) in line.char_indices() {
                if !c.is_whitespace() {
                    letter_at.insert(byte, letter_base + n);
                    n += 1;
                }
            }
            let mut buf = rustybuzz::UnicodeBuffer::new();
            buf.push_str(line);
            buf.guess_segment_properties();
            let shaped = rustybuzz::shape(&face, &[], buf);
            let mut pen = 0.0;
            let mut placed = Vec::new();
            for (info, pos) in shaped.glyph_infos().iter().zip(shaped.glyph_positions()) {
                let advance = pos.x_advance as f32 * s;
                if let Some(&letter) = letter_at.get(&(info.cluster as usize)) {
                    let gid = info.glyph_id as u16;
                    let mesh = self
                        .glyphs
                        .entry((id, gid, weight))
                        .or_insert_with(|| Arc::new(tessellate(&face, GlyphId(gid))))
                        .clone();
                    let origin = [pen + pos.x_offset as f32 * s, baseline - pos.y_offset as f32 * s];
                    placed.push(PlacedGlyph {
                        mesh,
                        origin,
                        scale: s * upem,
                        pivot: [pen + advance / 2.0, baseline - cap / 2.0],
                        letter,
                    });
                }
                pen += advance + spec.tracking;
            }
            let width = (pen - spec.tracking).max(0.0);
            let shift = match spec.align {
                Align::Left => 0.0,
                Align::Center => -width / 2.0,
                Align::Right => -width,
            };
            for g in &mut placed {
                g.origin[0] += shift;
                g.pivot[0] += shift;
            }
            out.extend(placed);
            letter_base += n;
            baseline += line_h;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lays_out_letters_with_geometry() {
        let mut fonts = Fonts::default();
        let spec = Spec {
            text: "Hi there\nok",
            font: "Helvetica Neue",
            weight: 700.0,
            size: 100.0,
            align: Align::Center,
            tracking: 0.0,
            leading: 1.1,
        };
        let glyphs = fonts.layout(&spec);
        assert_eq!(glyphs.len(), 9);
        assert!(glyphs.iter().all(|g| !g.mesh.indices.is_empty()));
        let letters: Vec<usize> = glyphs.iter().map(|g| g.letter).collect();
        assert_eq!(letters, (0..9).collect::<Vec<_>>());
        // Centered: first line spans both sides of the anchor.
        assert!(glyphs[0].origin[0] < 0.0 && glyphs[6].origin[0] > 0.0);
    }
}
