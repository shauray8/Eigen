//! Timeline of the open composition (the film, or a comp opened from the media
//! pool), laid out like an NLE: video tracks V1..Vn (one per `z` level, V1 at
//! the bottom, stacking upward), a draggable divider, audio tracks A1..An,
//! then the 3D world's objects. A press captures what it grabbed; a drag only
//! previews and commits ONE op on release (`set name in=.. out=.. z=..`, or
//! `dup` with Alt held); Esc cancels. Double-clicking a comp clip opens it.
//!
//! Look and mechanics are ported from FilmCraft (github.com/storytold/filmcraft,
//! MIT OR Apache-2.0), `crates/ui-egui/src/panels/`:
//! - `timeline.rs`: per-frame `Layout`, `ruler_steps`, timecode ruler, In/Out
//!   band, playhead, track headers (lock, eye, mute/solo, height drag,
//!   video/audio divider, separate vertical scroll), clip drawing, waveform
//!   column mesh, snapping, raw wheel input, eased zoom, zoom scroll bar;
//! - `timeline_hit.rs`: hit testing, edge zones, `Reach::beats`;
//! - `timeline_volume.rs`: the volume rubber band.
//!
//! See ATTRIBUTION.md at the repository root.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use eframe::egui::{self, Align2, Color32, CursorIcon, FontId, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, pos2, vec2};
use ed_core::anim::{FCurve, Key};
use ed_core::data::{Document, Kind, Object};
use ed_core::paths::fmt_num;
use ed_mcp::refs::{Author, Target};

use crate::app::{PEAKS_PER_SEC, Panels, kind_icon};

const HEADER_W: f32 = 168.0;
/// Timecode + transport (left) / ruler (right).
const TOP_H: f32 = 46.0;
const SCROLLBAR: f32 = 13.0;
const DIVIDER_H: f32 = 5.0;
const NAME_H: f32 = 16.0;
const WORLD_H: f32 = 24.0;
/// Widest an edge zone gets (px), on each side of a clip edge.
const EDGE_PX: f32 = 7.0;
const SNAP_PX: f32 = 9.0;
/// How far the pointer moves before a press becomes a drag.
const DRAG_PX: f32 = 3.0;
const MIN_THUMB_W: f32 = 34.0;
const MIN_PPF: f32 = 0.005;
const MAX_PPF: f32 = 400.0;
/// The volume line spans -48 dB (bottom; below it, silence) to +6 dB (top).
const VOL_MIN_DB: f32 = -48.0;
const VOL_MAX_DB: f32 = 6.0;

const BG: Color32 = Color32::from_gray(30);
const LANE_BG: Color32 = Color32::from_gray(24);
const TRACK_BG: Color32 = Color32::from_gray(35);
const TRACK_BG_ALT: Color32 = Color32::from_gray(32);
const HEADER_BG: Color32 = Color32::from_gray(40);
const SEPARATOR: Color32 = Color32::from_gray(19);
const TEXT: Color32 = Color32::from_gray(205);
const TEXT_DIM: Color32 = Color32::from_gray(140);
const TICK: Color32 = Color32::from_gray(115);
const PLAYHEAD: Color32 = Color32::from_rgb(0x3d, 0x9b, 0xe9);
const TIMECODE: Color32 = Color32::from_rgb(0x4d, 0xa6, 0xf5);
const BEAT: Color32 = Color32::from_rgb(70, 200, 210);
const KEY_COL: Color32 = Color32::from_rgb(0xc8, 0xc8, 0xc8);
const BRACKET: Color32 = Color32::from_rgb(0xf2, 0xc2, 0x2e);
const THUMB: Color32 = Color32::from_gray(78);
const THUMB_HOVER: Color32 = Color32::from_gray(105);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Edge {
    In,
    Out,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Lane {
    /// A canvas track: every 2D layer whose `z` floors to this level.
    Video(i64),
    Audio,
    World,
}

#[derive(Clone)]
struct Clip {
    name: String,
    kind: Kind,
    a: f32,
    b: f32,
    z: f32,
    /// No `out`: the clip lasts to the end (or, for a comp clip, its comp's end).
    open_out: bool,
    /// The media starts at `in` + `trim` seconds (video, audio, comp clips).
    trims: bool,
    trim: f32,
    /// Last frame the media reaches (the Out edge stops there), when known.
    max_b: Option<f32>,
    keys: Vec<f32>,
    expr: bool,
    visible: bool,
    src: String,
    volume: f32,
    /// Keyed `volume`.
    vol_keys: Option<FCurve>,
}

struct Track {
    lane: Lane,
    label: String,
    name: String,
    clips: Vec<Clip>,
}

/// Geometry of one track row, rebuilt every frame.
struct Row {
    track: usize,
    rect: Rect,
    /// `rect` cut to its area (video or audio): the part that is on screen.
    clip: Rect,
}

struct Layout {
    content: Rect,
    ruler: Rect,
    rows: Vec<Row>,
    /// Points per frame.
    ppf: f32,
    /// Frame at the left edge of `content`.
    scroll: f32,
    split_y: f32,
    video: Rect,
    lower: Rect,
}

impl Layout {
    fn x_of(&self, f: f32) -> f32 {
        self.content.min.x + (f - self.scroll) * self.ppf
    }
    fn frame_at(&self, x: f32) -> f32 {
        self.scroll + (x - self.content.min.x) / self.ppf
    }
    fn row_at(&self, y: f32) -> Option<&Row> {
        self.rows.iter().find(|r| r.clip.min.y <= y && y < r.clip.max.y)
    }
    fn row_of(&self, track: usize) -> Option<&Row> {
        self.rows.iter().find(|r| r.track == track)
    }
    fn body(&self, row: Rect, c: &Clip) -> Rect {
        let x0 = self.x_of(c.a);
        Rect::from_min_max(pos2(x0, row.min.y + 1.0), pos2(self.x_of(c.b).max(x0 + 1.0), row.max.y - 1.0))
    }
    /// Top of the highest video row (the split when there is none).
    fn top_video(&self, tracks: &[Track]) -> f32 {
        self.rows.iter().filter(|r| matches!(tracks[r.track].lane, Lane::Video(_))).map(|r| r.rect.min.y).fold(self.split_y, f32::min)
    }
}

/// What is at a screen point.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    Ruler,
    Clip { track: usize, clip: usize, edge: Option<Edge> },
    /// An audio clip's volume line, or one of its keys.
    Volume { track: usize, clip: usize, key: Option<usize> },
    Empty,
    None,
}

/// What the primary button went down on, worked out on the press frame.
#[derive(Clone, Copy)]
struct Pressed {
    pos: Pos2,
    hit: Hit,
}

/// Where a moved video clip goes.
#[derive(Clone, Copy, PartialEq)]
enum Dest {
    Home,
    Track(usize),
    /// The empty space above the top video track: a new track.
    New,
}

enum Drag {
    Scrub,
    Pan { last: Pos2 },
    Move { clip: Clip, track: usize, from: f32, delta: f32, dest: Dest, copy: bool },
    Trim { clip: Clip, track: usize, edge: Edge, from: f32, value: f32 },
    /// `keys`: the keys that move with the line (none: `volume` is not keyed).
    Volume { clip: Clip, y0: f32, h: f32, keys: Vec<Key>, last: Option<f32> },
}

#[derive(Default)]
pub struct Timeline {
    ready: bool,
    ppf: f32,
    target_ppf: f32,
    scroll: f32,
    target_scroll: f32,
    /// (frame, screen x) kept under each other while the zoom eases.
    zoom_anchor: Option<(f32, f32)>,
    video_h: f32,
    audio_h: f32,
    /// Share of the track area above the divider.
    split: f32,
    v_scroll: f32,
    a_scroll: f32,
    /// Track keys (`track_key`) refusing drags.
    locked: HashSet<String>,
    muted: HashSet<String>,
    solo: HashSet<String>,
    /// Audio objects silenced by mute/solo, with the volume to restore.
    silenced: HashMap<String, f32>,
    no_snap: bool,
    pressed: Option<Pressed>,
    drag: Option<Drag>,
    /// A drag aborted with Escape: ignore the rest of the press.
    cancelled: bool,
    /// Zoom bar drag: (press x, scroll, ppf, mode 0 = scroll, 1/2 = left/right handle).
    zoom_drag: Option<(f32, f32, f32, u8)>,
    snap_x: Option<f32>,
    /// Comp and playhead seen last frame (a new comp fits, a jump reveals).
    seen: Option<(Option<String>, f32)>,
    /// Loudest peak of each peak list (by address), for the waveform gain.
    peak_max: HashMap<usize, f32>,
}

fn kind_color(k: Kind) -> Color32 {
    let c = match k {
        Kind::Text => Color32::from_rgb(0xe3, 0x84, 0xe3),
        Kind::View => Color32::from_rgb(0x19, 0xa7, 0xda),
        Kind::Clip => Color32::from_rgb(0xf2, 0xa6, 0x50),
        Kind::Audio => Color32::from_rgb(0x4c, 0xa3, 0x4c),
        Kind::Fx => Color32::from_rgb(0xf8, 0x8b, 0xa5),
        Kind::Rect | Kind::Ellipse | Kind::Shader => Color32::from_rgb(0x36, 0xb3, 0xb0),
        Kind::Image | Kind::Video => Color32::from_rgb(0x8f, 0x8f, 0xd9),
        Kind::Camera | Kind::Light => Color32::from_rgb(0xc8, 0xaf, 0x8c),
        _ => Color32::from_rgb(0x4a, 0x63, 0xc9),
    };
    scale(c, 0.72)
}

fn scale(c: Color32, f: f32) -> Color32 {
    let s = |v: u8| (v as f32 * f).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgb(s(c.r()), s(c.g()), s(c.b()))
}

fn lighten(c: Color32, f: f32) -> Color32 {
    let l = |v: u8| (v as f32 + (255.0 - v as f32) * f).round() as u8;
    Color32::from_rgb(l(c.r()), l(c.g()), l(c.b()))
}

fn clip_of(doc: &Document, name: &str, o: &Object, end: f32, peaks: &HashMap<String, Arc<Vec<f32>>>) -> Clip {
    let fps = doc.scene.fps.max(1.0);
    let a = o.t_in.unwrap_or(0.0);
    let comp_end = (o.kind == Kind::Clip && !o.media.src.is_empty()).then(|| a + doc.comp_canvas(Some(&o.media.src)).1 - o.media.trim * fps);
    let b = o.t_out.or(comp_end).unwrap_or(end + 1.0);
    let max_b = match o.kind {
        Kind::Clip => comp_end,
        Kind::Audio | Kind::Video => {
            peaks.get(&o.media.src).filter(|p| !p.is_empty()).map(|p| a + (p.len() as f32 / PEAKS_PER_SEC as f32 - o.media.trim) * fps)
        }
        _ => None,
    };
    let audio = o.kind == Kind::Audio;
    let mut keys: Vec<f32> =
        o.anim.iter().filter(|(p, _)| !(audio && p.as_str() == "volume")).flat_map(|(_, c)| c.keys.iter().map(|k| k.frame)).collect();
    keys.sort_by(f32::total_cmp);
    keys.dedup();
    Clip {
        name: name.to_string(),
        kind: o.kind,
        a,
        b,
        z: o.z,
        open_out: o.t_out.is_none(),
        trims: matches!(o.kind, Kind::Video | Kind::Audio | Kind::Clip),
        trim: o.media.trim,
        max_b,
        keys,
        expr: !o.expr.is_empty(),
        visible: o.visible,
        src: o.media.src.clone(),
        volume: o.media.volume,
        vol_keys: o.anim.get("volume").filter(|c| !c.keys.is_empty()).cloned(),
    }
}

/// The open timeline's tracks: video V1..Vn (by `z` level), audio, 3D world.
fn build_tracks(doc: &Document, comp: Option<&str>, end: f32, peaks: &HashMap<String, Arc<Vec<f32>>>) -> (Vec<Track>, usize) {
    let mut video: BTreeMap<i64, Vec<Clip>> = BTreeMap::new();
    let mut audio = Vec::new();
    let mut world = Vec::new();
    for (n, o) in doc.objects.iter().filter(|(_, o)| o.comp.as_deref() == comp && o.kind != Kind::Comp) {
        let c = clip_of(doc, n, o, end, peaks);
        match o.kind {
            k if k.is_2d() => video.entry(o.z.floor() as i64).or_default().push(c),
            Kind::Audio => audio.push(c),
            _ => world.push(c),
        }
    }
    let nv = video.len();
    let mut out: Vec<Track> = video
        .into_iter()
        .enumerate()
        .map(|(i, (z, mut clips))| {
            clips.sort_by(|a, b| a.z.total_cmp(&b.z).then(a.a.total_cmp(&b.a)));
            let name = if clips.len() == 1 { "1 layer".to_string() } else { format!("{} layers", clips.len()) };
            Track { lane: Lane::Video(z), label: format!("V{}", i + 1), name, clips }
        })
        .collect();
    out.extend(audio.into_iter().enumerate().map(|(i, c)| Track { lane: Lane::Audio, label: format!("A{}", i + 1), name: c.name.clone(), clips: vec![c] }));
    out.extend(world.into_iter().map(|c| Track { lane: Lane::World, label: kind_icon(c.kind).to_string(), name: c.name.clone(), clips: vec![c] }));
    (out, nv)
}

/// Identity of a track across frames (for lock, mute, solo and widget ids).
fn track_key(t: &Track) -> String {
    match t.lane {
        Lane::Video(z) => format!("v{z}"),
        _ => format!("o{}", t.name),
    }
}

/// The ruler's two strides, in frames: `(tick, label)`. `label` is the first
/// ladder step whose labels stay 110 pt apart; `tick` the finest divisor of it
/// (at most 9) that keeps ticks 12 pt apart, so every label sits on a tick.
fn ruler_steps(frame_px: f64, base: i64) -> (i64, i64) {
    let big = |k: i64| base.saturating_mul(k);
    let steps = [1, 2, 5, 10, base / 2, base, big(2), big(5), big(10), big(15), big(30), big(60), big(120), big(300), big(600), big(1800), big(3600)];
    let label_step = (*steps.iter().find(|s| **s as f64 * frame_px >= 110.0).unwrap_or(&big(3600))).max(1);
    let minor = (1..=9)
        .rev()
        .filter(|k| label_step.rem_euclid(*k) == 0)
        .map(|k| label_step / k)
        .find(|m| *m as f64 * frame_px >= 12.0)
        .unwrap_or(label_step)
        .max(1);
    (minor, label_step)
}

/// HH:MM:SS:FF of frame `f` at `base` frames a second.
fn timecode(f: i64, base: i64) -> String {
    let sign = if f < 0 { "-" } else { "" };
    let n = f.abs();
    let s = n / base;
    format!("{sign}{:02}:{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60, n % base)
}

/// The top of each of `n` video rows `vh` high, V1 first: V1 sits at the
/// bottom of `area` and the tracks stack upward; `v_scroll` (clamped here)
/// shifts them up. One empty track's height stays free above the top track
/// as a drop zone for a new track. Also gives the largest `v_scroll`.
fn video_row_tops(area: Rect, n: usize, vh: f32, v_scroll: &mut f32) -> (Vec<f32>, f32) {
    let total = (n + 1) as f32 * vh;
    let v_off = (area.height() - total).max(0.0);
    let max_vs = (total - area.height()).max(0.0);
    *v_scroll = v_scroll.clamp(0.0, max_vs);
    let tops = (0..n).map(|i| area.min.y + v_off + (n - i) as f32 * vh - (max_vs - *v_scroll)).collect();
    (tops, max_vs)
}

// ---- volume line (timeline_volume.rs)

fn gain_to_pos(g: f32) -> f32 {
    if g <= 0.0 { 0.0 } else { ((20.0 * g.log10() - VOL_MIN_DB) / (VOL_MAX_DB - VOL_MIN_DB)).clamp(0.0, 1.0) }
}

fn pos_to_gain(p: f32) -> f32 {
    if p <= 0.002 { 0.0 } else { 10f32.powf((p.min(1.0) * (VOL_MAX_DB - VOL_MIN_DB) + VOL_MIN_DB) / 20.0) }
}

fn db_text(g: f32) -> String {
    if g <= 0.0 { "-∞ dB".into() } else { format!("{:+.2} dB", 20.0 * g.log10()) }
}

/// Where the line goes in a clip drawn at `body`: under the name, down to the bottom edge.
fn band(body: Rect) -> Rect {
    Rect::from_min_max(pos2(body.min.x, body.min.y + NAME_H + 1.0), pos2(body.max.x, body.max.y - 3.0))
}

fn y_of(body: Rect, g: f32) -> f32 {
    let b = band(body);
    b.max.y - gain_to_pos(g) * b.height()
}

fn gain_at(c: &Clip, f: f32) -> f32 {
    c.vol_keys.as_ref().and_then(|k| k.eval(f)).unwrap_or(c.volume)
}

/// The op that puts the volume line `dpos` (share of the band) away from where
/// the drag started: the static `volume`, or the keys that move with it.
fn volume_script(c: &Clip, keys: &[Key], dpos: f32) -> String {
    if keys.is_empty() {
        return format!("set {} volume={}", c.name, fmt_num(pos_to_gain(gain_to_pos(c.volume) + dpos)));
    }
    keys.iter()
        .map(|k| format!("key {}.volume {} @{} {}", c.name, fmt_num(pos_to_gain(gain_to_pos(k.value) + dpos)), fmt_num(k.frame), k.ease.name()))
        .collect::<Vec<_>>()
        .join("; ")
}

// ---- hit testing (timeline_hit.rs)

/// A clip edge within reach of the pointer.
#[derive(Clone, Copy)]
struct Reach {
    clip: usize,
    edge: Edge,
    /// The pointer is inside the clip (or exactly on the edge).
    inside: bool,
    dist: f32,
}

impl Reach {
    /// Inside a clip beats outside one (at a cut the side picks the clip),
    /// then the nearer edge wins; an exact tie goes to the later clip's In.
    fn beats(&self, other: &Reach) -> bool {
        if self.inside != other.inside {
            return self.inside;
        }
        match self.dist.partial_cmp(&other.dist) {
            Some(Ordering::Less) => true,
            Some(Ordering::Greater) => false,
            _ => self.edge == Edge::In && other.edge != Edge::In,
        }
    }
}

/// The edge zone (px, each side) of a clip `w` px wide: a third of it, at
/// most `EDGE_PX`, so the middle third of a narrow clip still moves it.
fn zone(w: f32) -> Option<f32> {
    if w.is_finite() && w > 0.0 { Some(EDGE_PX.min(w / 3.0)) } else { None }
}

fn edge_at(tr: &Track, layout: &Layout, x: f32) -> Option<(usize, Edge)> {
    let mut best: Option<Reach> = None;
    for (i, c) in tr.clips.iter().enumerate() {
        let (x0, x1) = (layout.x_of(c.a), layout.x_of(c.b));
        let Some(e) = zone(x1 - x0) else { continue };
        for (edge, d) in [(Edge::In, x - x0), (Edge::Out, x1 - x)] {
            if !(-e..=e).contains(&d) {
                continue;
            }
            let r = Reach { clip: i, edge, inside: d >= 0.0, dist: d.abs() };
            if best.is_none_or(|b| r.beats(&b)) {
                best = Some(r);
            }
        }
    }
    best.map(|r| (r.clip, r.edge))
}

/// The topmost clip of `tr` at frame `f`.
fn item_at(tr: &Track, f: f32) -> Option<usize> {
    tr.clips.iter().rposition(|c| c.a <= f && f < c.b)
}

fn hit(layout: &Layout, tracks: &[Track], pos: Pos2) -> Hit {
    if layout.ruler.contains(pos) {
        return Hit::Ruler;
    }
    if !layout.content.contains(pos) {
        return Hit::None;
    }
    let Some(row) = layout.row_at(pos.y) else { return Hit::None };
    let tr = &tracks[row.track];
    let audio = tr.lane == Lane::Audio;
    let on_band = |c: &Clip| {
        let body = layout.body(row.rect, c);
        (band(body).height() >= 6.0 && pos.x >= body.min.x && pos.x <= body.max.x).then_some(body)
    };
    if audio {
        for (ci, c) in tr.clips.iter().enumerate() {
            if let (Some(body), Some(k)) = (on_band(c), &c.vol_keys)
                && let Some(i) = k.keys.iter().position(|k| pos2(layout.x_of(k.frame), y_of(body, k.value)).distance(pos) <= 5.0)
            {
                return Hit::Volume { track: row.track, clip: ci, key: Some(i) };
            }
        }
    }
    if let Some((clip, edge)) = edge_at(tr, layout, pos.x) {
        return Hit::Clip { track: row.track, clip, edge: Some(edge) };
    }
    if audio {
        for (ci, c) in tr.clips.iter().enumerate() {
            if let Some(body) = on_band(c)
                && (pos.y - y_of(body, gain_at(c, layout.frame_at(pos.x)))).abs() <= 4.0
            {
                return Hit::Volume { track: row.track, clip: ci, key: None };
            }
        }
    }
    match item_at(tr, layout.frame_at(pos.x)) {
        Some(clip) => Hit::Clip { track: row.track, clip, edge: None },
        None => Hit::Empty,
    }
}

/// The clip a drag shows instead of the document's, its home track, where it
/// goes, and whether it is a copy.
fn preview(drag: &Drag, fps: f32) -> Option<(Clip, usize, Dest, bool)> {
    match drag {
        Drag::Move { clip, track, delta, dest, copy, .. } => {
            let mut c = clip.clone();
            c.a += delta;
            c.b += delta;
            Some((c, *track, *dest, *copy))
        }
        Drag::Trim { clip, track, edge, value, .. } => {
            let mut c = clip.clone();
            match edge {
                Edge::In => {
                    if c.trims {
                        c.trim = clip.trim + (value - clip.a) / fps;
                    }
                    c.a = *value;
                }
                Edge::Out => c.b = *value,
            }
            Some((c, *track, Dest::Home, false))
        }
        _ => None,
    }
}

// ---- painting

fn paint_lock(p: &egui::Painter, c: Pos2, locked: bool, col: Color32) {
    p.rect_filled(Rect::from_center_size(c + vec2(0.0, 2.5), vec2(9.0, 7.0)), 1.0, col);
    let (l, r, top, base) = (c.x - 2.5, c.x + 2.5, c.y - 5.0, c.y - 1.0);
    let right_end = if locked { base } else { top + 2.5 };
    let pts = vec![pos2(l, base), pos2(l, top + 1.5), pos2(l + 1.0, top), pos2(r - 1.0, top), pos2(r, top + 1.5), pos2(r, right_end)];
    p.add(Shape::line(pts, Stroke::new(1.4, col)));
}

fn paint_eye(p: &egui::Painter, c: Pos2, on: bool, col: Color32) {
    let (w, h) = (7.0, 4.0);
    let mut pts: Vec<Pos2> = (0..=8).map(|i| -1.0 + i as f32 / 4.0).map(|t| pos2(c.x + t * w, c.y - h * (1.0 - t * t))).collect();
    pts.extend((1..8).rev().map(|i| -1.0 + i as f32 / 4.0).map(|t| pos2(c.x + t * w, c.y + h * (1.0 - t * t))));
    p.add(Shape::closed_line(pts, Stroke::new(1.2, col)));
    p.circle_filled(c, 1.8, col);
    if !on {
        p.line_segment([pos2(c.x - w, c.y + h + 1.0), pos2(c.x + w, c.y - h - 1.0)], Stroke::new(1.4, col));
    }
}

fn paint_letter(p: &egui::Painter, r: Rect, letter: &str, on: bool, on_col: Color32, hovered: bool) {
    let fill = if on { on_col } else if hovered { Color32::from_gray(70) } else { Color32::from_gray(55) };
    p.rect_filled(r, 2.0, fill);
    p.text(r.center(), Align2::CENTER_CENTER, letter, FontId::proportional(10.0), if on { Color32::BLACK } else { TEXT });
}

fn paint_hatch(p: &egui::Painter, r: Rect) {
    let col = Color32::from_white_alpha(28);
    let mut x = r.min.x - r.height();
    while x < r.max.x {
        p.line_segment([pos2(x, r.max.y), pos2(x + r.height(), r.min.y)], Stroke::new(1.0, col));
        x += 8.0;
    }
}

fn paint_volume(p: &egui::Painter, layout: &Layout, body: Rect, c: &Clip) {
    let vis = p.clip_rect().intersect(body);
    if band(body).height() < 6.0 || vis.width() <= 0.0 {
        return;
    }
    let p = p.with_clip_rect(vis);
    let (x0, x1) = (vis.min.x, vis.max.x);
    // keys bend the line: then it gets a point every 2 px
    let n = if c.vol_keys.is_some() { (((x1 - x0) / 2.0).ceil() as usize).clamp(1, 4096) } else { 1 };
    let pts: Vec<Pos2> = (0..=n)
        .map(|i| {
            let x = x0 + (x1 - x0) * i as f32 / n as f32;
            pos2(x, y_of(body, gain_at(c, layout.frame_at(x))))
        })
        .collect();
    p.add(Shape::line(pts.iter().map(|q| *q + vec2(0.0, 1.0)).collect(), Stroke::new(1.0, Color32::BLACK)));
    p.add(Shape::line(pts, Stroke::new(1.0, Color32::WHITE)));
    for k in c.vol_keys.iter().flat_map(|k| &k.keys) {
        let q = pos2(layout.x_of(k.frame), y_of(body, k.value));
        if vis.expand(6.0).contains(q) {
            let d = 4.0;
            p.add(Shape::convex_polygon(vec![q + vec2(0.0, -d), q + vec2(d, 0.0), q + vec2(0.0, d), q + vec2(-d, 0.0)], KEY_COL, Stroke::new(1.0, Color32::BLACK)));
        }
    }
}

/// One column per screen pixel in a single mesh, rectified into the lower
/// 42% of the clip on a dB scale (-48 dB to 0), normalised to the source peak
/// (at most +12 dB) and multiplied by the clip's volume.
fn paint_waveform(p: &egui::Painter, layout: &Layout, body: Rect, c: &Clip, fps: f32, peaks: &[f32], peak: f32, col: Color32) {
    let zone_h = (body.height() * 0.42).max(8.0);
    let area = Rect::from_min_max(pos2(body.min.x + 1.0, body.max.y - zone_h - 1.0), pos2(body.max.x - 1.0, body.max.y - 1.0));
    let clip = p.clip_rect().intersect(area);
    if clip.width() <= 0.0 || peaks.is_empty() {
        return;
    }
    let gain = (1.0 / peak.max(1e-9)).min(3.981_072);
    let pps = PEAKS_PER_SEC as f32;
    let bucket = |x: f32| ((layout.frame_at(x) - c.a) / fps + c.trim) * pps;
    let mut mesh = egui::Mesh::default();
    for x in (clip.min.x.floor() as i32)..(clip.max.x.ceil() as i32) {
        let x = x as f32;
        let (s0, s1) = (bucket(x), bucket(x + 1.0));
        if s1 < 0.0 {
            continue;
        }
        let i0 = s0.max(0.0) as usize;
        if i0 >= peaks.len() {
            break;
        }
        let i1 = (s1.max(0.0) as usize).max(i0 + 1).min(peaks.len());
        let m = peaks[i0..i1].iter().fold(0f32, |m, v| m.max(v.abs())) * gain_at(c, layout.frame_at(x));
        let db = 20.0 * (m * gain).max(1e-5).log10();
        let h = ((db + 48.0) / 48.0).clamp(0.0, 1.0) * area.height();
        if h > 0.3 {
            mesh.add_colored_rect(Rect::from_min_max(pos2(x, area.max.y - h), pos2(x + 1.0, area.max.y)), col);
        }
    }
    p.add(mesh);
}

fn paint_diamond(p: &egui::Painter, c: Pos2, d: f32, col: Color32) {
    p.add(Shape::convex_polygon(vec![c + vec2(0.0, -d), c + vec2(d, 0.0), c + vec2(0.0, d), c + vec2(-d, 0.0)], col, Stroke::new(0.8, Color32::from_black_alpha(160))));
}

impl Timeline {
    /// Whether a drag is in progress (Escape then cancels it, nothing else).
    pub fn dragging(&self) -> bool {
        self.drag.is_some() || self.pressed.is_some()
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, panels: &mut Panels) {
        let (start, end, fps, frame0, selected, comp, tracks, nv, beats) = {
            let s = panels.shared.session.lock();
            let doc = s.doc();
            let comp = s.comp.clone();
            let (start, end) = match &comp {
                Some(c) => (0.0, doc.comp_canvas(Some(c)).1 - 1.0),
                None => (doc.scene.start as f32, doc.scene.end as f32),
            };
            let (tracks, nv) = build_tracks(doc, comp.as_deref(), end, &panels.peaks);
            // the film's music beat grid: (first downbeat, frames a beat)
            let beats = match (&comp, doc.beat_frame(0.0), doc.beat_frame(4.0)) {
                (None, Ok(b0), Ok(b4)) if b4 > b0 => Some((b0, (b4 - b0) / 4.0)),
                _ => None,
            };
            (start, end, doc.scene.fps.max(1.0), s.frame, s.selected.clone(), comp, tracks, nv, beats)
        };
        // review notes of this timeline: (frame, colour, text)
        let notes: Vec<(f32, Color32, String)> = panels
            .shared
            .refs
            .lock()
            .all()
            .iter()
            .filter(|r| r.comp == comp)
            .filter_map(|r| match &r.target {
                Target::Note { done: true, text, .. } => Some((r.frames[0], Color32::from_gray(110), format!("{} ✓ {text}", r.id))),
                Target::Note { by: Author::Director, text, .. } => Some((r.frames[0], Color32::from_rgb(90, 220, 255), format!("{} {text}", r.id))),
                Target::Note { text, .. } => Some((r.frames[0], Color32::from_rgb(255, 225, 70), format!("{} {text}", r.id))),
                _ => None,
            })
            .collect();
        let marks = panels.marks;

        let rect = ui.available_rect_before_wrap();
        ui.allocate_rect(rect, Sense::hover());
        if rect.width() < HEADER_W + 80.0 || rect.height() < TOP_H + 40.0 {
            return;
        }
        let ctx = ui.ctx().clone();
        let id = ui.id().with("timeline");
        let content = Rect::from_min_max(pos2(rect.min.x + HEADER_W, rect.min.y + TOP_H), pos2(rect.max.x - SCROLLBAR, rect.max.y - SCROLLBAR));
        let ruler = Rect::from_min_max(pos2(content.min.x, rect.min.y), pos2(content.max.x, content.min.y));
        let width = content.width();
        let ext = end + 1.0 + ((end - start + 1.0) * 0.25).max(fps * 5.0);

        // ---- view: first use, a new comp, eased zoom / scroll, following the playhead
        if !self.ready {
            self.ready = true;
            (self.video_h, self.audio_h, self.split) = (40.0, 44.0, 0.55);
        }
        if self.seen.as_ref().is_none_or(|(c, _)| *c != comp) {
            self.fit(width, start, end);
            (self.ppf, self.scroll) = (self.target_ppf, self.target_scroll);
        }
        let dt = ctx.input(|i| i.stable_dt).min(0.05);
        self.animate(dt, content, start, ext, &ctx);
        if panels.playing {
            let px = (frame0 - self.scroll) * self.ppf;
            if px > width * 0.95 || px < 0.0 {
                self.target_scroll = (frame0 - width * 0.05 / self.ppf).clamp(start, max_scroll(start, ext, width, self.ppf));
                self.scroll = self.target_scroll;
            }
        } else if self.seen.as_ref().is_some_and(|(c, f)| *c == comp && *f != frame0) && !matches!(self.drag, Some(Drag::Scrub)) {
            // a jump while stopped brings the playhead into view
            let px = (frame0 - self.target_scroll) * self.target_ppf;
            if px < 0.0 || px > width {
                self.target_scroll = (frame0 - width * 0.05 / self.target_ppf).clamp(start, max_scroll(start, ext, width, self.target_ppf));
                self.scroll = self.target_scroll;
                self.zoom_anchor = None;
            }
        }

        // ---- rows
        let tracks_area = content;
        let split_y = tracks_area.min.y + (tracks_area.height() - DIVIDER_H) * self.split;
        let video = Rect::from_min_max(pos2(rect.min.x, tracks_area.min.y), pos2(content.max.x, split_y));
        let lower = Rect::from_min_max(pos2(rect.min.x, split_y + DIVIDER_H), pos2(content.max.x, tracks_area.max.y));
        let (tops, max_vs) = video_row_tops(video, nv, self.video_h, &mut self.v_scroll);
        let mut rows = Vec::with_capacity(tracks.len());
        for (i, top) in tops.into_iter().enumerate() {
            let r = Rect::from_min_max(pos2(content.min.x, top), pos2(content.max.x, top + self.video_h));
            rows.push(Row { track: i, rect: r, clip: r.intersect(video) });
        }
        let row_h = |t: &Track| if t.lane == Lane::Audio { self.audio_h } else { WORLD_H };
        let lower_total: f32 = tracks[nv..].iter().map(row_h).sum();
        let max_as = (lower_total - lower.height()).max(0.0);
        self.a_scroll = self.a_scroll.clamp(0.0, max_as);
        let mut y = lower.min.y - self.a_scroll;
        for (i, t) in tracks.iter().enumerate().skip(nv) {
            let h = row_h(t);
            let r = Rect::from_min_max(pos2(content.min.x, y), pos2(content.max.x, y + h));
            rows.push(Row { track: i, rect: r, clip: r.intersect(lower) });
            y += h;
        }
        let layout = Layout { content, ruler, rows, ppf: self.ppf, scroll: self.scroll, split_y, video, lower };

        // ---- input (the area is registered first: headers, divider and bars win over it)
        let area = Rect::from_min_max(ruler.min, content.max);
        let resp = ui.interact(area, id.with("area"), Sense::click_and_drag());
        let mut frame = frame0;
        let mut select: Option<Option<String>> = None;
        let mut open: Option<String> = None;
        let mut scripts: Vec<String> = Vec::new();
        let mut edit: Option<(String, String, bool)> = None;
        self.snap_x = None;
        self.wheel(ui, &ctx, rect, &layout, start, ext);
        self.keys(ui, &ctx, rect, width, frame0, start, end);
        let (mods, primary_pressed, middle_pressed, any_down, pointer, press_origin, esc) = ctx.input(|i| {
            (
                i.modifiers,
                i.pointer.primary_pressed(),
                i.pointer.button_pressed(egui::PointerButton::Middle),
                i.pointer.any_down(),
                i.pointer.latest_pos(),
                i.pointer.press_origin(),
                i.key_pressed(egui::Key::Escape) || i.keys_down.contains(&egui::Key::Escape),
            )
        });
        let cands = |me: &Self, exclude: Option<&str>, playhead: bool| -> Vec<f32> {
            if me.no_snap {
                return Vec::new();
            }
            let mut c: Vec<f32> = tracks.iter().flat_map(|t| &t.clips).filter(|c| Some(c.name.as_str()) != exclude).flat_map(|c| [c.a, c.b]).collect();
            if playhead {
                c.push(frame0);
            }
            c.extend(marks.iter().flatten().copied());
            c.extend(marks[1].map(|o| o + 1.0));
            c.extend(notes.iter().map(|n| n.0));
            if let Some((b0, pb)) = beats
                && pb * layout.ppf >= 4.0
            {
                let n0 = ((layout.frame_at(content.min.x) - b0) / pb).floor() as i64;
                let n1 = ((layout.frame_at(content.max.x) - b0) / pb).ceil() as i64;
                c.extend((n0..=n1).map(|n| b0 + n as f32 * pb));
            }
            c
        };
        if !any_down {
            self.pressed = None;
            self.cancelled = false;
        }
        // press: act on what is under the pointer now, not once a drag is recognised
        if resp.is_pointer_button_down_on()
            && primary_pressed
            && let Some(p) = press_origin
        {
            self.cancelled = false;
            let h = hit(&layout, &tracks, p);
            match h {
                Hit::Ruler => {
                    self.drag = Some(Drag::Scrub);
                    frame = self.scrub(&layout, p, start, end, &notes, mods.shift, &cands(self, None, false));
                }
                Hit::Clip { track, clip, .. } | Hit::Volume { track, clip, .. } => select = Some(Some(tracks[track].clips[clip].name.clone())),
                _ => {}
            }
            self.pressed = Some(Pressed { pos: p, hit: h });
        }
        if resp.is_pointer_button_down_on()
            && middle_pressed
            && let Some(p) = pointer
        {
            self.drag = Some(Drag::Pan { last: p });
        }
        // a press that moved far enough becomes a drag of what it pressed
        if self.drag.is_none()
            && !self.cancelled
            && any_down
            && let (Some(pr), Some(p)) = (self.pressed, pointer)
            && p.distance(pr.pos) > DRAG_PX
        {
            self.pressed = None;
            let from = layout.frame_at(pr.pos.x).round();
            self.drag = match pr.hit {
                Hit::Clip { track, clip, edge } if !self.locked.contains(&track_key(&tracks[track])) => {
                    let c = tracks[track].clips[clip].clone();
                    Some(match edge {
                        Some(e) => Drag::Trim { value: if e == Edge::In { c.a } else { c.b }, clip: c, track, edge: e, from },
                        None => Drag::Move { clip: c, track, from, delta: 0.0, dest: Dest::Home, copy: mods.alt },
                    })
                }
                Hit::Volume { track, clip, key } if !self.locked.contains(&track_key(&tracks[track])) => {
                    let c = tracks[track].clips[clip].clone();
                    let h = layout.row_of(track).map_or(1.0, |r| band(layout.body(r.rect, &c)).height().max(1.0));
                    let keys = match (&c.vol_keys, key) {
                        (Some(k), Some(i)) => k.keys.get(i).copied().into_iter().collect(),
                        (Some(k), None) => {
                            let f = layout.frame_at(pr.pos.x);
                            let i = k.keys.partition_point(|k| k.frame <= f);
                            [i.checked_sub(1), Some(i)].into_iter().flatten().filter_map(|j| k.keys.get(j).copied()).collect()
                        }
                        (None, _) => Vec::new(),
                    };
                    Some(Drag::Volume { clip: c, y0: pr.pos.y, h, keys, last: None })
                }
                _ => None,
            };
        }
        // Escape cancels the drag: nothing is committed (a volume drag goes back)
        if esc && self.drag.is_some() {
            if let Some(Drag::Volume { clip, keys, last: Some(_), .. }) = &self.drag {
                edit = Some((volume_script(clip, keys, 0.0), format!("{}.volume", clip.name), true));
            }
            self.drag = None;
            self.cancelled = true;
        }
        // follow the pointer
        if let (Some(mut drag), Some(p)) = (self.drag.take(), pointer) {
            match &mut drag {
                Drag::Scrub => frame = self.scrub(&layout, p, start, end, &notes, mods.shift, &cands(self, None, false)),
                Drag::Pan { last } => {
                    let d = p - *last;
                    *last = p;
                    self.target_scroll = (self.target_scroll - d.x / self.ppf).clamp(start, max_scroll(start, ext, width, self.ppf));
                    self.scroll = self.target_scroll;
                    if p.y < split_y {
                        self.v_scroll += d.y;
                    } else {
                        self.a_scroll -= d.y;
                    }
                }
                Drag::Move { clip, track, from, delta, dest, copy } => {
                    let cs = cands(self, Some(&clip.name), true);
                    let mut d = (layout.frame_at(p.x) - *from).round();
                    if let Some(s) = self.snap(&layout, &cs, clip.a + d) {
                        d = s - clip.a;
                    } else if let Some(s) = self.snap(&layout, &cs, clip.b + d) {
                        d = s - clip.b;
                    }
                    *delta = d;
                    *copy = mods.alt;
                    *dest = match tracks[*track].lane {
                        Lane::Video(_) => match layout.row_at(p.y) {
                            Some(r) if r.track != *track && matches!(tracks[r.track].lane, Lane::Video(_)) && !self.locked.contains(&track_key(&tracks[r.track])) => {
                                Dest::Track(r.track)
                            }
                            None if p.y >= video.min.y && p.y < layout.top_video(&tracks) => Dest::New,
                            _ => Dest::Home,
                        },
                        _ => Dest::Home,
                    };
                    ctx.set_cursor_icon(if *copy { CursorIcon::Copy } else { CursorIcon::Grabbing });
                    self.auto_scroll(p, content, start, ext, &ctx);
                }
                Drag::Trim { clip, edge, from, value, .. } => {
                    let cs = cands(self, Some(&clip.name), true);
                    let base = if *edge == Edge::In { clip.a } else { clip.b };
                    let mut v = base + (layout.frame_at(p.x) - *from).round();
                    if let Some(s) = self.snap(&layout, &cs, v) {
                        v = s;
                    }
                    *value = match edge {
                        Edge::In => {
                            let lo = if clip.trims { clip.a - clip.trim * fps } else { f32::NEG_INFINITY };
                            v.max(lo).min(clip.b - 1.0)
                        }
                        Edge::Out => v.min(clip.max_b.unwrap_or(f32::INFINITY)).max(clip.a + 1.0),
                    };
                    ctx.set_cursor_icon(CursorIcon::ResizeColumn);
                    self.auto_scroll(p, content, start, ext, &ctx);
                }
                Drag::Volume { clip, y0, h, keys, last } => {
                    let dpos = (*y0 - p.y) / *h;
                    if *last != Some(dpos) {
                        edit = Some((volume_script(clip, keys, dpos), format!("{}.volume", clip.name), last.is_some()));
                        *last = Some(dpos);
                    }
                    let g = pos_to_gain(gain_to_pos(keys.first().map_or(clip.volume, |k| k.value)) + dpos);
                    resp.show_tooltip_text(db_text(g));
                    ctx.set_cursor_icon(CursorIcon::ResizeVertical);
                }
            }
            if any_down {
                self.drag = Some(drag);
            } else if let Some((script, sel)) = self.commit(drag, &tracks, nv, fps, panels) {
                // release: one op, one undo step
                scripts.push(script);
                if let Some(s) = sel {
                    select = Some(Some(s));
                }
            }
        }
        // a click (no drag) on an empty lane drops the selection; double-clicks
        if resp.clicked() && matches!(self.pressed.map(|p| p.hit), Some(Hit::Empty) | Some(Hit::None)) && self.drag.is_none() {
            select = Some(None);
        }
        if resp.double_clicked()
            && let Some(p) = resp.interact_pointer_pos()
        {
            match hit(&layout, &tracks, p) {
                Hit::Ruler => self.fit(width, start, end),
                Hit::Clip { track, clip, .. } if tracks[track].clips[clip].kind == Kind::Clip => open = Some(tracks[track].clips[clip].src.clone()),
                _ => {}
            }
        }

        // ---- paint
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, BG);
        painter.rect_filled(content, 0.0, LANE_BG);
        for r in &layout.rows {
            if r.clip.height() <= 0.0 {
                continue;
            }
            painter.rect_filled(r.clip, 0.0, if r.track % 2 == 0 { TRACK_BG } else { TRACK_BG_ALT });
            painter.line_segment([pos2(r.clip.min.x, r.rect.max.y - 0.5), pos2(r.clip.max.x, r.rect.max.y - 0.5)], Stroke::new(1.0, SEPARATOR));
            if r.track + 1 == nv {
                painter.line_segment([pos2(r.clip.min.x, r.rect.min.y + 0.5), pos2(r.clip.max.x, r.rect.min.y + 0.5)], Stroke::new(1.0, SEPARATOR));
            }
        }
        let cp = painter.with_clip_rect(content);
        // bars of the music across the tracks
        if let Some((b0, pb)) = beats
            && pb * 4.0 * self.ppf >= 8.0
        {
            let n0 = ((layout.frame_at(content.min.x) - b0) / (pb * 4.0)).floor() as i64;
            let n1 = ((layout.frame_at(content.max.x) - b0) / (pb * 4.0)).ceil() as i64;
            for n in n0..=n1 {
                let x = layout.x_of(b0 + n as f32 * pb * 4.0);
                cp.line_segment([pos2(x, content.min.y), pos2(x, content.max.y)], Stroke::new(1.0, BEAT.gamma_multiply(0.12)));
            }
        }
        // in/out shading across the tracks
        let in_out = (marks[0].is_some() || marks[1].is_some()).then(|| (marks[0].unwrap_or(start), marks[1].map_or(end + 1.0, |o| o + 1.0)));
        if let Some((a, b)) = in_out {
            let (xa, xb) = (layout.x_of(a).max(content.min.x), layout.x_of(b).min(content.max.x));
            if xb > xa {
                cp.rect_filled(Rect::from_min_max(pos2(xa, content.min.y), pos2(xb, content.max.y)), 0.0, Color32::from_white_alpha(9));
            }
        }
        // clips (culled to the visible frames), the dragged one where the drag puts it
        let (vis0, vis1) = (layout.frame_at(content.min.x - 2.0), layout.frame_at(content.max.x + 2.0));
        let pv = self.drag.as_ref().and_then(|d| preview(d, fps));
        for r in &layout.rows {
            if r.clip.height() <= 0.0 {
                continue;
            }
            let tr = &tracks[r.track];
            let p = painter.with_clip_rect(Rect::from_min_max(pos2(content.min.x, r.clip.min.y), pos2(content.max.x, r.clip.max.y)));
            for c in &tr.clips {
                let mut shown = c;
                if let Some((pc, _, dest, false)) = &pv
                    && pc.name == c.name
                {
                    if *dest != Dest::Home {
                        continue;
                    }
                    shown = pc;
                }
                if shown.b < vis0 || shown.a > vis1 {
                    continue;
                }
                let sel = selected.as_deref() == Some(&c.name);
                let inr = in_out.is_some_and(|(a, b)| shown.a < b && shown.b > a);
                self.paint_clip(&p, &layout, layout.body(r.rect, shown), shown, sel, inr, fps, &panels.peaks);
            }
            if let Some((pc, home, dest, copy)) = &pv
                && (*dest == Dest::Track(r.track) || (*copy && *dest == Dest::Home && *home == r.track))
            {
                self.paint_clip(&p, &layout, layout.body(r.rect, pc), pc, true, false, fps, &panels.peaks);
            }
            if self.locked.contains(&track_key(tr)) {
                paint_hatch(&p, r.clip);
            }
        }
        // a clip dragged above the top video track: its new track
        if let Some((pc, _, Dest::New, _)) = &pv {
            let top = layout.top_video(&tracks);
            let band = Rect::from_min_max(pos2(content.min.x, (top - self.video_h).max(video.min.y)), pos2(content.max.x, top));
            let body = Rect::from_min_max(pos2(layout.x_of(pc.a), band.min.y + 1.0), pos2(layout.x_of(pc.b), band.max.y - 1.0));
            let p = painter.with_clip_rect(band.intersect(content));
            p.rect_filled(body, 3.0, PLAYHEAD.gamma_multiply(0.4));
            p.rect_stroke(body, 3.0, Stroke::new(1.5, PLAYHEAD), StrokeKind::Inside);
            p.text(body.left_top() + vec2(4.0, 2.0), Align2::LEFT_TOP, &pc.name, FontId::proportional(11.0), TEXT);
        }

        // ---- hover: cursor and the edge a press would grab
        if self.drag.is_none()
            && let Some(p) = resp.hover_pos()
        {
            let h = hit(&layout, &tracks, p);
            let locked = match h {
                Hit::Clip { track, .. } | Hit::Volume { track, .. } => self.locked.contains(&track_key(&tracks[track])),
                _ => false,
            };
            let cur = match h {
                _ if locked => CursorIcon::NotAllowed,
                Hit::Clip { edge: Some(_), .. } => CursorIcon::ResizeColumn,
                Hit::Volume { .. } => CursorIcon::ResizeVertical,
                _ => CursorIcon::Default,
            };
            ctx.set_cursor_icon(cur);
            if let (Hit::Clip { track, clip, edge: Some(e) }, false) = (h, locked)
                && let Some(r) = layout.row_of(track)
            {
                let c = &tracks[track].clips[clip];
                let (x, d) = if e == Edge::In { (layout.x_of(c.a), 5.0) } else { (layout.x_of(c.b), -5.0) };
                let (y0, y1) = (r.rect.min.y + 2.0, r.rect.max.y - 2.0);
                let b = painter.with_clip_rect(r.clip.intersect(content));
                b.add(Shape::line(vec![pos2(x + d, y0), pos2(x, y0), pos2(x, y1), pos2(x + d, y1)], Stroke::new(2.0, BRACKET)));
            }
            if let Some((_, _, text)) = notes.iter().find(|n| Rect::from_center_size(pos2(layout.x_of(n.0), ruler.min.y + 8.0), vec2(10.0, 13.0)).contains(p)) {
                resp.show_tooltip_text(text);
            }
        }

        // ---- headers, divider, top block, playhead, bars
        self.headers(ui, &painter, id, rect, &layout, &tracks, &mut scripts);
        let div = Rect::from_min_max(pos2(rect.min.x, split_y), pos2(content.max.x, split_y + DIVIDER_H));
        painter.rect_filled(div, 0.0, SEPARATOR);
        let dr = ui.interact(div, id.with("divider"), Sense::drag());
        if dr.hovered() || dr.dragged() {
            ctx.set_cursor_icon(CursorIcon::ResizeVertical);
        }
        if dr.dragged() {
            self.split = (self.split + dr.drag_delta().y / (tracks_area.height() - DIVIDER_H).max(1.0)).clamp(0.1, 0.9);
        }
        self.top(ui, &painter, id, rect, &layout, frame, fps, start, end, &comp, marks, &notes, beats, panels);
        let px = layout.x_of(frame);
        if px >= content.min.x - 1.0 && px <= content.max.x + 1.0 {
            let (top, tip) = (ruler.max.y - 13.0, ruler.max.y);
            let head = vec![pos2(px - 5.0, top), pos2(px + 5.0, top), pos2(px + 5.0, tip - 5.0), pos2(px, tip), pos2(px - 5.0, tip - 5.0)];
            painter.add(Shape::convex_polygon(head, PLAYHEAD, Stroke::NONE));
            painter.line_segment([pos2(px, tip), pos2(px, content.max.y)], Stroke::new(1.0, PLAYHEAD));
        }
        if let Some(sx) = self.snap_x {
            painter.line_segment([pos2(sx, ruler.max.y), pos2(sx, content.max.y)], Stroke::new(1.0, Color32::WHITE));
            for (y, d) in [(ruler.max.y, 1.0), (content.max.y, -1.0)] {
                painter.add(Shape::convex_polygon(vec![pos2(sx - 4.0, y), pos2(sx + 4.0, y), pos2(sx, y + 5.0 * d)], Color32::WHITE, Stroke::NONE));
            }
        }
        let zbar = Rect::from_min_max(pos2(content.min.x, rect.max.y - SCROLLBAR + 2.0), pos2(content.max.x, rect.max.y - 2.0));
        self.zoom_scrollbar(ui, &painter, id, zbar, start, ext);
        let vbar = Rect::from_min_max(pos2(content.max.x, video.min.y), pos2(rect.max.x, video.max.y));
        vertical_scrollbar(ui, &painter, id.with("vs"), vbar, &mut self.v_scroll, max_vs, true);
        let abar = Rect::from_min_max(pos2(content.max.x, lower.min.y), pos2(rect.max.x, lower.max.y));
        vertical_scrollbar(ui, &painter, id.with("as"), abar, &mut self.a_scroll, max_as, false);

        // ---- apply
        self.seen = Some((comp, frame));
        if frame != frame0 {
            panels.shared.session.lock().frame = frame;
        }
        if let Some(s) = select {
            panels.shared.session.lock().selected = s;
        }
        if let Some((script, key, continuing)) = edit {
            panels.run_edit(&script, &key, continuing);
        }
        for s in scripts {
            panels.run(&s);
        }
        if let Some(src) = open.filter(|s| !s.is_empty()) {
            panels.open_comp(Some(src));
        }
    }

    /// Zoom to show the whole timeline.
    fn fit(&mut self, width: f32, start: f32, end: f32) {
        self.target_ppf = (width * 0.94 / (end - start + 1.0).max(1.0)).clamp(MIN_PPF, MAX_PPF);
        self.target_scroll = start;
        self.zoom_anchor = None;
    }

    /// Zoom by `factor` about frame `anchor` (kept under the same x when in view).
    fn zoom_about(&mut self, factor: f32, anchor: f32, width: f32) {
        let old = self.target_ppf;
        let new = (old * factor).clamp(MIN_PPF, MAX_PPF);
        let x = (anchor - self.target_scroll) * old;
        let x = if (0.0..=width).contains(&x) { x } else { width / 2.0 };
        self.target_ppf = new;
        self.target_scroll = anchor - x / new;
        self.zoom_anchor = None;
    }

    /// Critically damped easing of zoom and scroll, anchored under the pointer.
    fn animate(&mut self, dt: f32, content: Rect, start: f32, ext: f32, ctx: &egui::Context) {
        let w = content.width();
        let k = 1.0 - (-dt * 20.0).exp();
        let zooming = (self.ppf - self.target_ppf).abs() / self.target_ppf > 0.001;
        self.ppf += (self.target_ppf - self.ppf) * k;
        if let Some((t, x)) = self.zoom_anchor.filter(|_| zooming) {
            self.scroll = t - (x - content.min.x) / self.ppf;
            self.target_scroll = t - (x - content.min.x) / self.target_ppf;
        } else {
            self.scroll += (self.target_scroll - self.scroll) * k;
            self.zoom_anchor = None;
        }
        if (self.ppf - self.target_ppf).abs() / self.target_ppf < 0.0005 {
            self.ppf = self.target_ppf;
        }
        if (self.scroll - self.target_scroll).abs() * self.ppf < 0.05 {
            self.scroll = self.target_scroll;
        }
        if !self.scroll.is_finite() || !self.target_scroll.is_finite() {
            (self.scroll, self.target_scroll) = (start, start);
        }
        self.scroll = self.scroll.clamp(start, max_scroll(start, ext, w, self.ppf));
        self.target_scroll = self.target_scroll.clamp(start, max_scroll(start, ext, w, self.target_ppf));
        if self.ppf != self.target_ppf || self.scroll != self.target_scroll {
            ctx.request_repaint();
        }
    }

    /// Wheel, as in Premiere Pro: the wheel scrolls the tracks under the
    /// pointer, Shift or a sideways swipe scrolls the time, Option/Cmd + wheel
    /// or a pinch zooms about the pointer. Read from the raw events: egui has
    /// its own meaning for Cmd + wheel.
    fn wheel(&mut self, ui: &egui::Ui, ctx: &egui::Context, rect: Rect, layout: &Layout, start: f32, ext: f32) {
        let Some(p) = ctx.pointer_hover_pos().filter(|_| ui.rect_contains_pointer(rect)) else { return };
        let (mut delta, mut pinch, mut zoom_mod, mut shift) = (egui::Vec2::ZERO, 1.0f32, false, false);
        ctx.input(|i| {
            for e in &i.events {
                match e {
                    egui::Event::MouseWheel { unit, delta: d, modifiers, .. } => {
                        let points = match unit {
                            egui::MouseWheelUnit::Point => 1.0,
                            egui::MouseWheelUnit::Line => 40.0,
                            egui::MouseWheelUnit::Page => 400.0,
                        };
                        if d.x.is_finite() && d.y.is_finite() {
                            delta += *d * points;
                        }
                        zoom_mod |= modifiers.alt || modifiers.command || modifiers.mac_cmd || modifiers.ctrl;
                        shift |= modifiers.shift;
                    }
                    egui::Event::Zoom(z) if z.is_finite() && *z > 0.0 => pinch *= *z,
                    _ => {}
                }
            }
        });
        let width = layout.content.width();
        if (pinch - 1.0).abs() > 1e-4 || (zoom_mod && delta.y != 0.0) {
            let f = if (pinch - 1.0).abs() > 1e-4 { pinch } else { (1.0 + delta.y * 0.01).clamp(0.5, 2.0) };
            let x = p.x.clamp(layout.content.min.x, layout.content.max.x);
            self.zoom_anchor = Some((layout.frame_at(x), x));
            self.target_ppf = (self.target_ppf * f).clamp(MIN_PPF, MAX_PPF);
        } else if delta != egui::Vec2::ZERO {
            let sideways = if delta.x.abs() > delta.y.abs() {
                Some(delta.x)
            } else if shift {
                Some(delta.y)
            } else {
                None
            };
            match sideways {
                Some(d) => {
                    self.target_scroll = (self.target_scroll - d / self.ppf).clamp(start, max_scroll(start, ext, width, self.ppf));
                    self.scroll = self.target_scroll;
                }
                None if p.y < layout.split_y => self.v_scroll += delta.y,
                None => self.a_scroll -= delta.y,
            }
        }
    }

    /// `=` / `-` zoom about the playhead, `\` fits, `S` toggles snapping (while
    /// the pointer is over the timeline and nothing has keyboard focus).
    #[allow(clippy::too_many_arguments)]
    fn keys(&mut self, ui: &egui::Ui, ctx: &egui::Context, rect: Rect, width: f32, frame: f32, start: f32, end: f32) {
        if !ui.rect_contains_pointer(rect) || ctx.memory(|m| m.focused().is_some()) {
            return;
        }
        let none = egui::Modifiers::NONE;
        let (zin, zout, fit, snap) = ctx.input_mut(|i| {
            (
                i.consume_key(none, egui::Key::Equals) || i.consume_key(none, egui::Key::Plus),
                i.consume_key(none, egui::Key::Minus),
                i.consume_key(none, egui::Key::Backslash),
                i.consume_key(none, egui::Key::S),
            )
        });
        if zin {
            self.zoom_about(1.6, frame, width);
        }
        if zout {
            self.zoom_about(1.0 / 1.6, frame, width);
        }
        if fit {
            self.fit(width, start, end);
        }
        if snap {
            self.no_snap = !self.no_snap;
        }
    }

    /// The candidate within `SNAP_PX` of frame `t` nearest to it (and the snap line there).
    fn snap(&mut self, layout: &Layout, cands: &[f32], t: f32) -> Option<f32> {
        let x = layout.x_of(t);
        let best = cands.iter().map(|c| ((layout.x_of(*c) - x).abs(), *c)).filter(|(d, _)| *d < SNAP_PX).min_by(|a, b| a.0.total_cmp(&b.0))?;
        self.snap_x = Some(layout.x_of(best.1));
        Some(best.1)
    }

    /// The playhead where the pointer is on the ruler: onto a note flag under
    /// it, snapped with Shift, whole frames inside the timeline.
    #[allow(clippy::too_many_arguments)]
    fn scrub(&mut self, layout: &Layout, p: Pos2, start: f32, end: f32, notes: &[(f32, Color32, String)], shift: bool, cands: &[f32]) -> f32 {
        if let Some(n) = notes.iter().find(|n| Rect::from_center_size(pos2(layout.x_of(n.0), layout.ruler.min.y + 8.0), vec2(10.0, 13.0)).contains(p)) {
            return n.0;
        }
        let mut f = layout.frame_at(p.x).round();
        if shift && let Some(s) = self.snap(layout, cands, f) {
            f = s;
        }
        f.clamp(start, end)
    }

    /// Scroll while a clip is dragged near the left or right edge.
    fn auto_scroll(&mut self, p: Pos2, content: Rect, start: f32, ext: f32, ctx: &egui::Context) {
        let speed = if p.x > content.max.x - 20.0 {
            (p.x - (content.max.x - 20.0)) / 20.0 * 12.0
        } else if p.x < content.min.x + 10.0 {
            -(content.min.x + 10.0 - p.x) / 10.0 * 12.0
        } else {
            return;
        };
        self.target_scroll = (self.target_scroll + speed / self.ppf).clamp(start, max_scroll(start, ext, content.width(), self.ppf));
        self.scroll = self.target_scroll;
        ctx.request_repaint();
    }

    /// The one op a released drag commits, and what it selects.
    fn commit(&mut self, drag: Drag, tracks: &[Track], nv: usize, fps: f32, panels: &Panels) -> Option<(String, Option<String>)> {
        match drag {
            Drag::Move { clip: c, delta, dest, copy, .. } => {
                let frac = c.z - c.z.floor();
                let z = match dest {
                    Dest::Home => c.z,
                    Dest::Track(t) => match tracks.get(t).map(|t| t.lane) {
                        Some(Lane::Video(z)) => z as f32 + frac,
                        _ => c.z,
                    },
                    Dest::New => match tracks[..nv].last().map(|t| t.lane) {
                        Some(Lane::Video(z)) => (z + 1) as f32 + frac,
                        _ => c.z,
                    },
                };
                if delta == 0.0 && z == c.z && !copy {
                    return None;
                }
                let mut props = format!("in={}", fmt_num(c.a + delta));
                if !c.open_out {
                    props += &format!(" out={}", fmt_num(c.b + delta));
                }
                if c.kind.is_2d() && z != c.z {
                    props += &format!(" z={}", fmt_num(z));
                }
                if copy {
                    let s = panels.shared.session.lock();
                    let new = (2..).map(|i| format!("{}_{i}", c.name)).find(|n| !s.doc().objects.contains_key(n))?;
                    Some((format!("dup {} {new} {props}", c.name), Some(new)))
                } else {
                    Some((format!("set {} {props}", c.name), None))
                }
            }
            Drag::Trim { clip: c, edge, value, .. } => match edge {
                Edge::In if value != c.a => {
                    let trim = if c.trims { format!(" trim={}", fmt_num((c.trim + (value - c.a) / fps).max(0.0))) } else { String::new() };
                    Some((format!("set {} in={}{trim}", c.name, fmt_num(value)), None))
                }
                Edge::Out if value != c.b => Some((format!("set {} out={}", c.name, fmt_num(value)), None)),
                _ => None,
            },
            _ => None,
        }
    }

    /// Draw one clip: label-colour body, name band with `fx` badge, waveform
    /// and volume line (audio), keyframe diamonds, selection border.
    #[allow(clippy::too_many_arguments)]
    fn paint_clip(&mut self, p: &egui::Painter, layout: &Layout, body: Rect, c: &Clip, selected: bool, in_range: bool, fps: f32, peaks: &HashMap<String, Arc<Vec<f32>>>) {
        let base = kind_color(c.kind);
        let fill = if !c.visible {
            scale(base, 0.5)
        } else if in_range {
            lighten(base, 0.12)
        } else if selected {
            lighten(base, 0.06)
        } else {
            base
        };
        p.rect_filled(body, 0.0, fill);
        p.line_segment([pos2(body.min.x, body.min.y + 0.5), pos2(body.max.x, body.min.y + 0.5)], Stroke::new(1.0, lighten(fill, 0.15)));
        let w = body.width();
        if c.kind == Kind::Audio
            && w > 3.0
            && let Some(pk) = peaks.get(&c.src)
        {
            let key = Arc::as_ptr(pk) as usize;
            let peak = *self.peak_max.entry(key).or_insert_with(|| pk.iter().fold(0f32, |m, v| m.max(v.abs())));
            paint_waveform(p, layout, body, c, fps, pk, peak, scale(fill, 0.5));
        }
        let cp = p.with_clip_rect(body.shrink2(vec2(1.0, 0.0)).intersect(p.clip_rect()));
        if w > 24.0 {
            let col = if c.visible { Color32::from_gray(0xe6) } else { TEXT_DIM };
            cp.text(pos2(body.min.x + 6.0, body.min.y + NAME_H / 2.0 + 0.5), Align2::LEFT_CENTER, format!("{} {}", kind_icon(c.kind), c.name), FontId::proportional(11.5), col);
        }
        if c.expr && w > 40.0 {
            cp.text(pos2(body.max.x - 6.0, body.min.y + NAME_H / 2.0 + 0.5), Align2::RIGHT_CENTER, "fx", FontId::proportional(11.0), Color32::from_rgb(0xe8, 0xc5, 0x4a));
        }
        if body.height() > NAME_H + 6.0 {
            let y = body.max.y - 5.0;
            for k in &c.keys {
                let x = layout.x_of(*k);
                if x >= body.min.x && x <= body.max.x {
                    paint_diamond(&cp, pos2(x, y), 3.5, BRACKET);
                }
            }
        }
        if c.kind == Kind::Audio {
            paint_volume(p, layout, body, c);
        }
        p.line_segment([pos2(body.min.x + 0.5, body.min.y), pos2(body.min.x + 0.5, body.max.y)], Stroke::new(1.0, Color32::BLACK));
        if selected {
            p.rect_stroke(body.shrink(1.0), 0.0, Stroke::new(1.0, Color32::BLACK), StrokeKind::Inside);
            p.rect_stroke(body, 0.0, Stroke::new(2.0, Color32::from_gray(240)), StrokeKind::Inside);
        }
    }

    /// Track headers: V/A label, lock, eye (video, 3D) or mute/solo (audio),
    /// name, and the bottom edge that resizes the track height.
    fn headers(&mut self, ui: &mut egui::Ui, painter: &egui::Painter, id: egui::Id, rect: Rect, layout: &Layout, tracks: &[Track], scripts: &mut Vec<String>) {
        let mut mute_changed = false;
        for r in &layout.rows {
            let tr = &tracks[r.track];
            let is_video = matches!(tr.lane, Lane::Video(_));
            let area = if is_video { layout.video } else { layout.lower };
            let column = Rect::from_min_max(pos2(rect.min.x, area.min.y), pos2(rect.min.x + HEADER_W, area.max.y));
            let h = Rect::from_min_max(pos2(rect.min.x, r.rect.min.y), pos2(rect.min.x + HEADER_W, r.rect.max.y));
            let vis = h.intersect(column);
            if vis.height() <= 0.0 {
                continue;
            }
            let key = track_key(tr);
            let tid = id.with(("track", &key));
            let p = painter.with_clip_rect(vis);
            p.rect_filled(h, 0.0, HEADER_BG);
            p.line_segment([pos2(h.min.x, h.max.y - 0.5), pos2(h.max.x, h.max.y - 0.5)], Stroke::new(1.0, SEPARATOR));
            // the header itself first, so its buttons (added later) win
            let bg = ui.interact(vis, tid, Sense::click());
            if bg.double_clicked() && tr.lane != Lane::World {
                let th = if is_video { &mut self.video_h } else { &mut self.audio_h };
                *th = if *th < 50.0 { 64.0 } else { 30.0 };
            }
            let locked = self.locked.contains(&key);
            if locked {
                p.rect_filled(Rect::from_min_max(pos2(h.max.x - 4.0, h.min.y), h.max), 0.0, Color32::from_gray(75));
            }
            let cy = if h.height() >= 40.0 { h.min.y + 14.0 } else { h.center().y };
            let lb = Rect::from_center_size(pos2(h.min.x + 22.0, cy), vec2(28.0, 18.0f32.min(h.height() - 4.0)));
            p.rect_filled(lb, 3.0, Color32::from_gray(62));
            p.text(lb.center(), Align2::CENTER_CENTER, &tr.label, FontId::proportional(11.0), TEXT);
            let button = |x: f32, w: f32, what: &str, tip: &str| {
                let br = Rect::from_center_size(pos2(h.min.x + x, cy), vec2(w, w));
                (br, ui.interact(br.intersect(vis), tid.with(what), Sense::click()).on_hover_text(tip))
            };
            let (lr, lresp) = button(52.0, 18.0, "lock", "Lock track (refuses drags)");
            paint_lock(&p, lr.center(), locked, if locked { Color32::from_gray(235) } else if lresp.hovered() { TEXT } else { TEXT_DIM });
            if lresp.clicked() && !self.locked.remove(&key) {
                self.locked.insert(key.clone());
            }
            let mut text_x = 86.0;
            match tr.lane {
                Lane::Audio => {
                    let name = &tr.name;
                    let (mr, mresp) = button(76.0, 14.0, "mute", "Mute track");
                    paint_letter(&p, mr, "M", self.muted.contains(name), Color32::from_rgb(0x2d, 0x9d, 0x78), mresp.hovered());
                    if mresp.clicked() {
                        if !self.muted.remove(name) {
                            self.muted.insert(name.clone());
                        }
                        mute_changed = true;
                    }
                    let (sr, sresp) = button(96.0, 14.0, "solo", "Solo track");
                    paint_letter(&p, sr, "S", self.solo.contains(name), Color32::from_rgb(0xf0, 0xf0, 0x4f), sresp.hovered());
                    if sresp.clicked() {
                        if !self.solo.remove(name) {
                            self.solo.insert(name.clone());
                        }
                        mute_changed = true;
                    }
                    text_x = 110.0;
                }
                _ => {
                    let on = tr.clips.iter().any(|c| c.visible);
                    let tip = if is_video { "Show or hide every layer on this track" } else { "Show or hide this object" };
                    let (er, eresp) = button(76.0, 18.0, "eye", tip);
                    paint_eye(&p, er.center(), on, if eresp.hovered() { TEXT } else { TEXT_DIM });
                    if eresp.clicked() && !tr.clips.is_empty() {
                        scripts.push(tr.clips.iter().map(|c| format!("set {} visible={}", c.name, !on)).collect::<Vec<_>>().join("\n"));
                    }
                }
            }
            if h.height() >= 40.0 {
                p.text(pos2(h.min.x + 9.0, h.min.y + 32.0), Align2::LEFT_CENTER, &tr.name, FontId::proportional(11.0), TEXT_DIM);
            } else {
                p.with_clip_rect(vis.intersect(Rect::from_min_max(pos2(h.min.x + text_x, h.min.y), pos2(h.max.x - 6.0, h.max.y))))
                    .text(pos2(h.min.x + text_x, cy), Align2::LEFT_CENTER, &tr.name, FontId::proportional(11.0), TEXT_DIM);
            }
            // height: drag the bottom edge (one height for video, one for audio)
            if tr.lane != Lane::World {
                let edge = Rect::from_min_max(pos2(h.min.x, h.max.y - 3.0), pos2(h.max.x, h.max.y + 2.0)).intersect(column);
                if edge.height() > 0.0 {
                    let eresp = ui.interact(edge, tid.with("height"), Sense::drag());
                    if eresp.hovered() || eresp.dragged() {
                        ui.ctx().set_cursor_icon(CursorIcon::ResizeVertical);
                    }
                    if eresp.dragged() {
                        let d = eresp.drag_delta().y;
                        let th = if is_video { &mut self.video_h } else { &mut self.audio_h };
                        *th = (*th + if is_video { -d } else { d }).clamp(22.0, 220.0);
                    }
                }
            }
        }
        painter.line_segment(
            [pos2(rect.min.x + HEADER_W - 0.5, layout.video.min.y), pos2(rect.min.x + HEADER_W - 0.5, layout.lower.max.y)],
            Stroke::new(1.0, SEPARATOR),
        );
        if mute_changed && let Some(s) = self.apply_mute(tracks) {
            scripts.push(s);
        }
    }

    /// Mute and solo as volume ops: silenced audio goes to `volume=0` and gets
    /// its volume back when it sounds again (one op for all of them).
    fn apply_mute(&mut self, tracks: &[Track]) -> Option<String> {
        let audio: Vec<&Clip> = tracks.iter().filter(|t| t.lane == Lane::Audio).flat_map(|t| &t.clips).collect();
        let solo_on = audio.iter().any(|c| self.solo.contains(&c.name));
        let mut lines = Vec::new();
        for c in audio {
            let silent = self.muted.contains(&c.name) || (solo_on && !self.solo.contains(&c.name));
            match (silent, self.silenced.contains_key(&c.name)) {
                (true, false) => {
                    self.silenced.insert(c.name.clone(), c.volume);
                    lines.push(format!("set {} volume=0", c.name));
                }
                (false, true) => {
                    let v = self.silenced.remove(&c.name).unwrap_or(1.0);
                    lines.push(format!("set {} volume={}", c.name, fmt_num(v)));
                }
                _ => {}
            }
        }
        (!lines.is_empty()).then(|| lines.join("\n"))
    }

    /// Top block: timecode, transport, snapping, where we are (left); the
    /// timecode ruler with note flags, In/Out band and beat grid (right).
    #[allow(clippy::too_many_arguments)]
    fn top(
        &mut self,
        ui: &mut egui::Ui,
        painter: &egui::Painter,
        id: egui::Id,
        rect: Rect,
        layout: &Layout,
        frame: f32,
        fps: f32,
        start: f32,
        end: f32,
        comp: &Option<String>,
        marks: [Option<f32>; 2],
        notes: &[(f32, Color32, String)],
        beats: Option<(f32, f32)>,
        panels: &mut Panels,
    ) {
        let base = (fps.round() as i64).max(1);
        let left = Rect::from_min_max(rect.min, pos2(rect.min.x + HEADER_W, rect.min.y + TOP_H));
        let lp = painter.with_clip_rect(left);
        lp.rect_filled(left, 0.0, BG);
        let tc = Rect::from_min_size(pos2(left.min.x + 10.0, left.min.y + 3.0), vec2(HEADER_W - 16.0, 20.0));
        lp.text(tc.left_center(), Align2::LEFT_CENTER, timecode(frame.round() as i64, base), FontId::monospace(16.0), TIMECODE);
        ui.interact(tc, id.with("tc"), Sense::hover()).on_hover_text(format!(
            "frame {} of {}–{}\nspace: play · I/O: mark · ⌥/⌘ wheel or pinch: zoom · = − \\: zoom · S: snap\n⌥ drag: copy · Esc: cancel · double-click a comp clip: open it",
            frame.round(),
            start,
            end
        ));
        let mut x = left.min.x + 8.0;
        let y = left.min.y + 25.0;
        let mut button = |w: f32, what: &str, label: &str, on: bool, tip: &str| {
            let r = Rect::from_min_size(pos2(x, y), vec2(w, 18.0));
            x += w + 3.0;
            let resp = ui.interact(r, id.with(what), Sense::click()).on_hover_text(tip);
            let fill = if on { Color32::from_gray(75) } else if resp.hovered() { Color32::from_gray(55) } else { Color32::from_gray(42) };
            lp.rect_filled(r, 3.0, fill);
            lp.text(r.center(), Align2::CENTER_CENTER, label, FontId::proportional(11.5), if on { TIMECODE } else { TEXT });
            resp.clicked()
        };
        if button(22.0, "home", "⏮", false, "to the start") {
            panels.shared.session.lock().frame = start;
        }
        let playing = panels.playing;
        if button(22.0, "play", if playing { "⏸" } else { "▶" }, false, "play / pause (space)") {
            panels.playing = !playing;
        }
        if button(22.0, "snap", "S", !self.no_snap, "Snap in timeline (S)") {
            self.no_snap = !self.no_snap;
        }
        match comp {
            Some(c) => {
                if button(42.0, "film", "◀ Film", false, "back to the film (Esc)") {
                    panels.open_comp(None);
                }
                lp.text(pos2(x + 2.0, y + 9.0), Align2::LEFT_CENTER, format!("▦ {c}"), FontId::proportional(11.0), TEXT);
            }
            None => {
                lp.text(pos2(x + 2.0, y + 9.0), Align2::LEFT_CENTER, "◆ Film", FontId::proportional(11.0), TEXT_DIM);
            }
        }

        // ruler: note flags · timecodes · ticks (beats at the bottom)
        let ruler = layout.ruler;
        let clip = painter.with_clip_rect(ruler);
        clip.rect_filled(ruler, 0.0, BG);
        let label_y = ruler.min.y + 22.0;
        let tick_base = ruler.max.y - 3.0;
        if marks.iter().any(Option::is_some) {
            let a = layout.x_of(marks[0].unwrap_or(start));
            let b = layout.x_of(marks[1].map_or(end + 1.0, |o| o + 1.0));
            clip.rect_filled(Rect::from_min_max(pos2(a, label_y + 7.0), pos2(b, tick_base)), 0.0, Color32::from_gray(0x5c));
        }
        let (minor, label) = ruler_steps(layout.ppf as f64, base);
        let f0 = layout.frame_at(ruler.min.x).floor() as i64;
        let f1 = layout.frame_at(ruler.max.x).ceil() as i64 + 1;
        let mut f = f0.div_euclid(minor) * minor;
        while f <= f1 {
            let x = layout.x_of(f as f32);
            let major = f.rem_euclid(label) == 0;
            clip.line_segment([pos2(x, tick_base - if major { 10.0 } else { 4.0 }), pos2(x, tick_base)], Stroke::new(1.0, TICK));
            if major {
                clip.text(pos2(x + 1.0, label_y), Align2::CENTER_CENTER, timecode(f, base), FontId::proportional(11.0), Color32::from_gray(170));
            }
            f += minor;
        }
        if let Some((b0, pb)) = beats
            && pb * layout.ppf >= 4.0
        {
            let n0 = ((layout.frame_at(ruler.min.x) - b0) / pb).floor() as i64;
            let n1 = ((layout.frame_at(ruler.max.x) - b0) / pb).ceil() as i64;
            for n in n0..=n1 {
                let x = layout.x_of(b0 + n as f32 * pb);
                let bar = n.rem_euclid(4) == 0;
                let c = BEAT.gamma_multiply(if bar { 1.0 } else { 0.5 });
                clip.line_segment([pos2(x, ruler.max.y - if bar { 7.0 } else { 3.0 }), pos2(x, ruler.max.y)], Stroke::new(1.0, c));
            }
        }
        let marker_y = ruler.min.y + 2.0;
        for (nf, c, _) in notes {
            let x = layout.x_of(*nf);
            let shape = vec![pos2(x - 4.0, marker_y), pos2(x + 4.0, marker_y), pos2(x + 4.0, marker_y + 8.0), pos2(x, marker_y + 12.0), pos2(x - 4.0, marker_y + 8.0)];
            clip.add(Shape::convex_polygon(shape, *c, Stroke::NONE));
        }
        if self.no_snap {
            clip.text(pos2(ruler.max.x - 4.0, ruler.min.y + 2.0), Align2::RIGHT_TOP, "snap off", FontId::proportional(10.0), TEXT_DIM);
        }
    }

    /// Premiere's zoom scroll bar: the thumb is the share in view; drag it to
    /// scroll, drag either end handle to zoom with the other end pinned.
    fn zoom_scrollbar(&mut self, ui: &mut egui::Ui, painter: &egui::Painter, id: egui::Id, bar: Rect, start: f32, ext: f32) {
        painter.rect_filled(bar, bar.height() / 2.0, Color32::from_gray(22));
        let vis = bar.width() / self.ppf;
        let total = (ext - start).max(vis);
        let w = ((vis / total) * bar.width()).clamp(MIN_THUMB_W.min(bar.width()), bar.width());
        let travel = (bar.width() - w).max(0.0);
        let range = (total - vis).max(0.0);
        let at = if range > 0.0 { ((self.scroll - start) / range).clamp(0.0, 1.0) * travel } else { 0.0 };
        let per_point = if travel > 0.5 { range / travel } else { 0.0 };
        let thumb = Rect::from_min_max(pos2(bar.min.x + at, bar.min.y), pos2(bar.min.x + at + w, bar.max.y));
        let hover = ui.rect_contains_pointer(thumb);
        painter.rect_filled(thumb, thumb.height() / 2.0, if hover { THUMB_HOVER } else { THUMB });
        for x in [thumb.min.x + thumb.height() / 2.0, thumb.max.x - thumb.height() / 2.0] {
            painter.circle_filled(pos2(x, thumb.center().y), 3.5, BG);
            painter.circle_stroke(pos2(x, thumb.center().y), 3.5, Stroke::new(1.3, Color32::from_gray(200)));
        }
        let resp = ui.interact(bar, id.with("zoombar"), Sense::click_and_drag());
        let jump = |me: &mut Self, x: f32| {
            if per_point > 0.0 {
                me.target_scroll = start + ((x - bar.min.x - w / 2.0) * per_point).clamp(0.0, range);
                me.scroll = me.target_scroll;
            }
        };
        if resp.clicked()
            && let Some(p) = resp.interact_pointer_pos().filter(|p| !thumb.contains(*p))
        {
            jump(self, p.x);
        }
        if resp.drag_started()
            && let Some(p) = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos())
        {
            let mode = if !thumb.expand2(vec2(0.0, 4.0)).contains(p) {
                jump(self, p.x);
                0
            } else if (p.x - thumb.min.x).abs() < 9.0 {
                1
            } else if (p.x - thumb.max.x).abs() < 9.0 {
                2
            } else {
                0
            };
            self.zoom_drag = Some((p.x, self.target_scroll, self.target_ppf, mode));
        }
        if let Some((_, s0, ppf0, _)) = self.zoom_drag
            && ui.input(|i| i.key_pressed(egui::Key::Escape) || i.keys_down.contains(&egui::Key::Escape))
        {
            (self.scroll, self.target_scroll, self.ppf, self.target_ppf) = (s0, s0, ppf0, ppf0);
            self.zoom_drag = None;
        }
        if let Some((grab, s0, _, mode)) = self.zoom_drag
            && resp.dragged()
        {
            match mode {
                0 => {
                    if let Some(p) = resp.interact_pointer_pos() {
                        self.target_scroll = (s0 + (p.x - grab) * per_point).clamp(start, start + range);
                        self.scroll = self.target_scroll;
                    }
                }
                _ => {
                    let dx = resp.drag_delta().x / bar.width().max(1.0) * total;
                    if mode == 1 {
                        let end = self.scroll + vis;
                        let ns = (self.scroll + dx).clamp(start, (end - 1.0).max(start));
                        self.ppf = (bar.width() / (end - ns).max(1.0)).clamp(MIN_PPF, MAX_PPF);
                        self.scroll = ns;
                        self.target_scroll = ns;
                    } else {
                        let ne = (self.scroll + vis + dx).max(self.scroll + 1.0);
                        self.ppf = (bar.width() / (ne - self.scroll)).clamp(MIN_PPF, MAX_PPF);
                    }
                    self.target_ppf = self.ppf;
                    self.zoom_anchor = None;
                }
            }
        }
        if resp.drag_stopped() {
            self.zoom_drag = None;
        }
    }
}

/// The latest frame the left edge of a view `width` points wide can show.
fn max_scroll(start: f32, ext: f32, width: f32, ppf: f32) -> f32 {
    if !(ppf > 0.0 && ppf.is_finite()) {
        return start;
    }
    (ext - width / ppf).max(start)
}

/// A vertical scroll bar for one track area (`invert`: the video area, whose
/// scroll grows upward).
fn vertical_scrollbar(ui: &mut egui::Ui, painter: &egui::Painter, id: egui::Id, bar: Rect, value: &mut f32, max: f32, invert: bool) {
    if max <= 0.5 || bar.height() < 20.0 {
        return;
    }
    let h = (bar.height() * bar.height() / (bar.height() + max)).max(18.0);
    let pos = if invert { 1.0 - *value / max } else { *value / max };
    let track = bar.shrink2(vec2(3.0, 0.0));
    let thumb = Rect::from_min_size(pos2(track.min.x, bar.min.y + (bar.height() - h) * pos), vec2(track.width(), h));
    painter.rect_filled(track, track.width() / 2.0, Color32::from_gray(22));
    painter.rect_filled(thumb, thumb.width() / 2.0, if ui.rect_contains_pointer(thumb) { THUMB_HOVER } else { THUMB });
    let resp = ui.interact(bar, id, Sense::drag());
    if resp.dragged() {
        let d = resp.drag_delta().y / (bar.height() - h).max(1.0) * max;
        *value = (*value + if invert { -d } else { d }).clamp(0.0, max);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ruler_labels_sit_on_ticks() {
        for ppf in [0.05, 0.5, 2.0, 9.0, 40.0] {
            let (minor, label) = ruler_steps(ppf, 24);
            assert_eq!(label % minor, 0);
            assert!(label as f64 * ppf >= 110.0 || label == 24 * 3600);
        }
    }

    #[test]
    fn timecode_formats() {
        assert_eq!(timecode(0, 24), "00:00:00:00");
        assert_eq!(timecode(24 * 61 + 5, 24), "00:01:01:05");
        assert_eq!(timecode(-1, 24), "-00:00:00:01");
    }

    #[test]
    fn volume_scale_round_trips() {
        for g in [0.1, 0.5, 1.0, 1.8] {
            assert!((pos_to_gain(gain_to_pos(g)) - g).abs() < 1e-3);
        }
        assert_eq!(pos_to_gain(0.0), 0.0);
        assert_eq!(gain_to_pos(0.0), 0.0);
    }

    #[test]
    fn inside_beats_outside_at_a_cut() {
        let inside = Reach { clip: 1, edge: Edge::In, inside: true, dist: 3.0 };
        let outside = Reach { clip: 0, edge: Edge::Out, inside: false, dist: 1.0 };
        assert!(inside.beats(&outside));
        let tie_in = Reach { clip: 1, edge: Edge::In, inside: true, dist: 0.0 };
        let tie_out = Reach { clip: 0, edge: Edge::Out, inside: true, dist: 0.0 };
        assert!(tie_in.beats(&tie_out) && !tie_out.beats(&tie_in));
    }
}
