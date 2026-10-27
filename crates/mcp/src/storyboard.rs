//! Storyboard: the film planned as a row of rough panels before (and while)
//! it is built. The human sketches; the agent reads the sketches as images,
//! and can draft panels itself (caption + image + rough strokes). Panels play
//! in order: each one's `seconds` gives it a frame range of the film, and it
//! can name the comp built for it. Like refs, the board is a plan beside the
//! film (`<project>/storyboard.json`), not film content.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const FILE: &str = "storyboard.json";
/// Sketch paper (near black) and inks: white marker, red marker, and the
/// agent's blue (the agent always draws in blue so its lines are told apart).
pub const PAPER: [u8; 3] = [18, 18, 21];
pub const INKS: [[u8; 3]; 3] = [[245, 245, 245], [255, 84, 72], [90, 170, 255]];

/// A word or line written on a panel.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Label {
    /// Top-left, 0..1 across the panel.
    pub pos: [f32; 2],
    pub text: String,
    pub color: [u8; 3],
    /// Letter height as a fraction of the panel's height.
    pub size: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Stroke {
    pub color: [u8; 3],
    /// Pen width as a fraction of the panel's width.
    pub width: f32,
    /// Points in 0..1 across the panel (y down).
    pub points: Vec<[f32; 2]>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Panel {
    pub id: String,
    #[serde(default)]
    pub caption: String,
    pub seconds: f32,
    #[serde(default)]
    pub strokes: Vec<Stroke>,
    #[serde(default)]
    pub labels: Vec<Label>,
    /// A picture under the sketch (project path): a reference, a render, a draft.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// The comp built for this shot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comp: Option<String>,
}

#[derive(Default)]
pub struct Storyboard {
    path: PathBuf,
    pub panels: Vec<Panel>,
}

impl Storyboard {
    pub fn load(dir: &Path) -> Self {
        let path = dir.join(FILE);
        let panels = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Self { path, panels }
    }

    pub fn save(&self) {
        if let Ok(json) = serde_json::to_vec_pretty(&self.panels) {
            let _ = std::fs::write(&self.path, json);
        }
    }

    /// Appends a panel (or inserts it at `at`) and returns its id (`P<n>`).
    pub fn add(&mut self, at: Option<usize>, caption: String, seconds: f32) -> String {
        let n = self.panels.iter().filter_map(|p| p.id.strip_prefix('P')?.parse::<u32>().ok()).max().unwrap_or(0) + 1;
        let id = format!("P{n}");
        let panel = Panel { id: id.clone(), caption, seconds: seconds.max(0.1), strokes: Vec::new(), labels: Vec::new(), image: None, comp: None };
        let at = at.unwrap_or(self.panels.len()).min(self.panels.len());
        self.panels.insert(at, panel);
        self.save();
        id
    }

    pub fn index(&self, id: &str) -> Option<usize> {
        let id = id.trim().trim_start_matches('@');
        self.panels.iter().position(|p| p.id.eq_ignore_ascii_case(id))
    }

    pub fn get(&self, id: &str) -> Option<&Panel> {
        self.index(id).map(|i| &self.panels[i])
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut Panel> {
        self.index(id).map(|i| &mut self.panels[i])
    }

    /// First frame of each panel when the board plays from frame `start` at `fps`.
    pub fn starts(&self, start: f32, fps: f32) -> Vec<f32> {
        let mut t = start;
        self.panels
            .iter()
            .map(|p| {
                let s = t;
                t += (p.seconds * fps).round();
                s
            })
            .collect()
    }

    /// One line per panel, or for one panel: what and when.
    pub fn describe(&self, i: usize, start: f32, fps: f32) -> String {
        let p = &self.panels[i];
        let a = self.starts(start, fps)[i];
        let b = a + (p.seconds * fps).round() - 1.0;
        let mut s = format!("@{} = storyboard panel {} of {}, {:.1}s (film frames {a}-{b})", p.id, i + 1, self.panels.len(), p.seconds);
        if !p.caption.is_empty() {
            s += &format!(": \"{}\"", p.caption);
        }
        if let Some(c) = &p.comp {
            s += &format!(" · comp `{c}`");
        }
        if !p.labels.is_empty() {
            let words: Vec<String> = p.labels.iter().map(|l| format!("\"{}\" at {:.0}%,{:.0}%", l.text, l.pos[0] * 100.0, l.pos[1] * 100.0)).collect();
            s += &format!(" · written on it: {}", words.join(", "));
        }
        s
    }
}

/// The panel as an image of `size` px: paper, its picture (fitted), its sketch.
pub fn rasterize(p: &Panel, project: &Path, size: [u32; 2]) -> Vec<u8> {
    let [w, h] = size;
    let mut rgba: Vec<u8> = PAPER.iter().copied().chain([255]).cycle().take((w * h * 4) as usize).collect();
    if let Some(pic) = p.image.as_ref().and_then(|src| ed_media::load_image(&project.join(src)).ok()) {
        // Fit inside, centered; nearest sample is plenty for a storyboard.
        let k = (w as f32 / pic.w as f32).min(h as f32 / pic.h as f32);
        let (dw, dh) = ((pic.w as f32 * k) as u32, (pic.h as f32 * k) as u32);
        let (ox, oy) = ((w - dw.min(w)) / 2, (h - dh.min(h)) / 2);
        for y in 0..dh.min(h) {
            let sy = ((y as f32 / k) as u32).min(pic.h - 1);
            for x in 0..dw.min(w) {
                let sx = ((x as f32 / k) as u32).min(pic.w - 1);
                let s = ((sy * pic.w + sx) * 4) as usize;
                let d = (((y + oy) * w + x + ox) * 4) as usize;
                let a = pic.rgba[s + 3] as u32;
                for c in 0..3 {
                    rgba[d + c] = ((pic.rgba[s + c] as u32 * a + rgba[d + c] as u32 * (255 - a)) / 255) as u8;
                }
            }
        }
    }
    for s in &p.strokes {
        let mut pen = crate::raster::Pen { rgba: &mut rgba, size, width: (s.width * w as f32).max(1.5), color: s.color };
        pen.polyline(s.points.iter().map(|q| [q[0] * w as f32, q[1] * h as f32]));
    }
    rgba
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panels_play_in_order_and_draw() {
        let dir = std::env::temp_dir().join(format!("ed_board_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut b = Storyboard::load(&dir);
        let p1 = b.add(None, "wide".into(), 2.0);
        let p0 = b.add(Some(0), "cold open".into(), 1.0);
        assert_eq!((p1.as_str(), p0.as_str()), ("P1", "P2"));
        assert_eq!(b.starts(0.0, 24.0), vec![0.0, 24.0], "the inserted panel plays first");
        assert!(b.describe(1, 0.0, 24.0).contains("film frames 24-71"));
        b.get_mut("p1").unwrap().strokes.push(Stroke { color: INKS[1], width: 0.05, points: vec![[0.0, 0.5], [1.0, 0.5]] });
        b.save();

        let back = Storyboard::load(&dir);
        let px = rasterize(back.get("P1").unwrap(), &dir, [40, 20]);
        let at = |x: usize, y: usize| <[u8; 3]>::try_from(&px[(y * 40 + x) * 4..][..3]).unwrap();
        assert_eq!((at(20, 10), at(20, 2)), (INKS[1], PAPER), "the stroke is drawn across the middle on paper");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
