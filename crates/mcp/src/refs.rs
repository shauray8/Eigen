//! Refs: things the human points at, so a message can say "this" and "here".
//! An object or a screen region over a frame range (`@R<n>`), or a review
//! note drawn on a frame (`@N<n>`). Annotations, not film content: they live
//! beside the document in `<project>/refs.json`, never in the op log.

use std::path::{Path, PathBuf};

use ed_core::data::{Document, Kind};
use ed_core::eval;
use serde::{Deserialize, Serialize};

const FILE: &str = "refs.json";

/// Who wrote a review note: the human, or the director agent reviewing renders.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Author {
    #[default]
    Human,
    Director,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Target {
    Object {
        name: String,
    },
    /// Canvas rectangle in scene px: x, y (top-left), w, h.
    Region {
        rect: [f32; 4],
    },
    /// Strokes drawn over the frame (scene px) and what the note says.
    Note {
        text: String,
        strokes: Vec<Vec<[f32; 2]>>,
        #[serde(default)]
        done: bool,
        #[serde(default)]
        reply: String,
        #[serde(default)]
        by: Author,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Ref {
    pub id: String,
    /// Composition the frames and pixels belong to (None: the film).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comp: Option<String>,
    /// First and last frame (inclusive; equal for a single frame).
    pub frames: [f32; 2],
    #[serde(flatten)]
    pub target: Target,
}

impl Ref {
    pub fn token(&self) -> String {
        format!("@{}", self.id)
    }

    pub fn is_open_note(&self) -> bool {
        matches!(self.target, Target::Note { done: false, .. })
    }

    /// One line a person or the agent can read without looking anything up.
    pub fn describe(&self, fps: f32) -> String {
        let [a, b] = self.frames;
        let mut when = if a == b {
            format!("frame {a}")
        } else {
            format!("frames {a}-{b} ({:.2}s)", (b - a + 1.0) / fps)
        };
        if let Some(c) = &self.comp {
            when += &format!(" of comp `{c}`");
        }
        match &self.target {
            Target::Object { name } => format!("{} = object `{name}` at {when}", self.token()),
            Target::Region { rect: [x, y, w, h] } => {
                format!("{} = canvas region x={x:.0} y={y:.0} w={w:.0} h={h:.0} px (center {:.0},{:.0}) at {when}", self.token(), x + w / 2.0, y + h / 2.0)
            }
            Target::Note { text, done, by, .. } => {
                let state = if *done { "resolved" } else { "open" };
                let who = match by {
                    Author::Human => "human",
                    Author::Director => "director",
                };
                format!("{} = {state} {who} note at {when}: \"{text}\"", self.token())
            }
        }
    }
}

#[derive(Default)]
pub struct Refs {
    path: PathBuf,
    list: Vec<Ref>,
}

impl Refs {
    pub fn load(dir: &Path) -> Self {
        let path = dir.join(FILE);
        let list = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Self { path, list }
    }

    fn save(&self) {
        if let Ok(json) = serde_json::to_vec_pretty(&self.list) {
            let _ = std::fs::write(&self.path, json);
        }
    }

    /// Stores a new ref and returns its id (`R<n>`, or `N<n>` for notes).
    pub fn add(&mut self, comp: Option<String>, frames: [f32; 2], target: Target) -> String {
        let prefix = if matches!(target, Target::Note { .. }) { "N" } else { "R" };
        let n = self.list.iter().filter_map(|r| r.id.strip_prefix(prefix)?.parse::<u32>().ok()).max().unwrap_or(0) + 1;
        let id = format!("{prefix}{n}");
        let [a, b] = frames;
        self.list.push(Ref { id: id.clone(), comp, frames: [a.min(b).round(), a.max(b).round()], target });
        self.save();
        id
    }

    pub fn get(&self, id: &str) -> Option<&Ref> {
        let id = id.trim().trim_start_matches('@');
        self.list.iter().find(|r| r.id.eq_ignore_ascii_case(id))
    }

    pub fn all(&self) -> &[Ref] {
        &self.list
    }

    /// Marks a note done, with the agent's (or human's) answer.
    pub fn resolve(&mut self, id: &str, answer: &str) -> anyhow::Result<()> {
        let id = id.trim().trim_start_matches('@');
        let r = self.list.iter_mut().find(|r| r.id.eq_ignore_ascii_case(id)).ok_or_else(|| anyhow::anyhow!("no ref {id}"))?;
        let Target::Note { done, reply, .. } = &mut r.target else { anyhow::bail!("{id} is not a note") };
        *done = true;
        *reply = answer.to_string();
        self.save();
        Ok(())
    }

    /// Opens a resolved note again (the human disagrees with the fix).
    pub fn reopen(&mut self, id: &str) {
        if let Some(Target::Note { done, .. }) = self.list.iter_mut().find(|r| r.id.eq_ignore_ascii_case(id)).map(|r| &mut r.target) {
            *done = false;
            self.save();
        }
    }

    pub fn remove(&mut self, id: &str) {
        self.list.retain(|r| !r.id.eq_ignore_ascii_case(id));
        self.save();
    }
}

/// 2D layers of composition `comp` visible at `frame` whose box overlaps
/// `rect`, top first: `name (kind) x,y wxh` lines.
pub fn layers_in(doc: &Document, comp: Option<&str>, frame: f32, rect: [f32; 4]) -> Vec<String> {
    let mut hits: Vec<(f32, String)> = doc
        .objects
        .iter()
        .filter(|(_, o)| o.comp.as_deref() == comp && o.kind.is_2d() && o.kind != Kind::Fx && o.visible && o.alive(frame))
        .filter_map(|(n, o)| {
            let o = eval::animated(o, n, frame, doc.scene.fps);
            let corners = eval::box_corners(eval::layer_box(&o)?);
            let (x0, x1) = corners.iter().fold((f32::MAX, f32::MIN), |(a, b), p| (a.min(p[0]), b.max(p[0])));
            let (y0, y1) = corners.iter().fold((f32::MAX, f32::MIN), |(a, b), p| (a.min(p[1]), b.max(p[1])));
            let [x, y, w, h] = rect;
            let overlaps = x0 < x + w && x1 > x && y0 < y + h && y1 > y;
            overlaps.then(|| (o.z, format!("{n} ({}) {:.0},{:.0} {:.0}x{:.0}", o.kind.name(), x0, y0, x1 - x0, y1 - y0)))
        })
        .collect();
    hits.sort_by(|a, b| b.0.total_cmp(&a.0));
    hits.into_iter().map(|(_, s)| s).collect()
}

/// Color refs are marked in, on screen and in the agent's images: magenta,
/// rare in footage so the mark never reads as part of the picture.
pub const MARK: [u8; 3] = [255, 43, 214];

/// What a ref points at on `frame`, as polylines in scene px (closed shapes
/// repeat their first point). Shared by the agent's images and the viewport.
pub fn marks(r: &Ref, doc: &Document, frame: f32) -> Vec<Vec<[f32; 2]>> {
    match &r.target {
        Target::Region { rect: [x, y, w, h] } => vec![vec![[*x, *y], [x + w, *y], [x + w, y + h], [*x, y + h], [*x, *y]]],
        Target::Object { name } => {
            let b = doc.objects.get(name).map(|o| eval::animated(o, name, frame, doc.scene.fps));
            b.as_ref().and_then(eval::layer_box).map(eval::box_corners).map(|c| vec![vec![c[0], c[1], c[2], c[3], c[0]]]).unwrap_or_default()
        }
        Target::Note { strokes, .. } => strokes.clone(),
    }
}

/// Draws `marks` (scene px) onto a rendered RGBA frame of `size` px for a scene of `res` px.
pub fn draw(rgba: &mut [u8], size: [u32; 2], res: [u32; 2], marks: &[Vec<[f32; 2]>]) {
    let k = size[0] as f32 / res[0] as f32;
    let mut pen = crate::raster::Pen { rgba, size, width: (size[0] as f32 / 220.0).max(2.0), color: MARK };
    for line in marks {
        pen.polyline(line.iter().map(|p| [p[0] * k, p[1] * k]));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_count_per_kind_and_survive_reload() {
        let dir = std::env::temp_dir().join(format!("ed-refs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut refs = Refs::load(&dir);
        let note = Target::Note { text: "too dark".into(), strokes: vec![], done: false, reply: String::new(), by: Author::Human };
        assert_eq!(refs.add(None, [10.0, 4.0], Target::Region { rect: [0.0, 0.0, 10.0, 10.0] }), "R1");
        assert_eq!(refs.add(Some("shot1".into()), [5.0, 5.0], note), "N1");
        assert_eq!(refs.add(None, [5.0, 5.0], Target::Object { name: "a".into() }), "R2");
        refs.resolve("@n1", "lifted the shadows").unwrap();
        assert!(refs.resolve("R1", "").is_err(), "only notes resolve");

        let back = Refs::load(&dir);
        assert_eq!(back.get("R1").unwrap().frames, [4.0, 10.0], "range is ordered");
        assert!(!back.get("N1").unwrap().is_open_note());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
