//! Media files: probing (ffprobe), still images (image crate), streaming video
//! decode (ffmpeg), thumbnails, the project library, and web sources for
//! images (Openverse) and PBR textures (Poly Haven).

pub mod web;

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};

use anyhow::{Context, Result, anyhow, ensure};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaKind {
    Image,
    Video,
    Audio,
}

impl MediaKind {
    pub fn name(self) -> &'static str {
        match self {
            MediaKind::Image => "image",
            MediaKind::Video => "video",
            MediaKind::Audio => "audio",
        }
    }
}

const IMAGE_EXT: [&str; 8] = ["png", "jpg", "jpeg", "webp", "gif", "bmp", "tif", "tiff"];
const VIDEO_EXT: [&str; 7] = ["mp4", "mov", "m4v", "mkv", "webm", "avi", "mxf"];
const AUDIO_EXT: [&str; 8] = ["mp3", "wav", "flac", "m4a", "aac", "ogg", "aiff", "opus"];

pub fn kind_of(path: &Path) -> Option<MediaKind> {
    let ext = path.extension()?.to_str()?.to_lowercase();
    if IMAGE_EXT.contains(&ext.as_str()) {
        Some(MediaKind::Image)
    } else if VIDEO_EXT.contains(&ext.as_str()) {
        Some(MediaKind::Video)
    } else if AUDIO_EXT.contains(&ext.as_str()) {
        Some(MediaKind::Audio)
    } else {
        None
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Info {
    pub w: u32,
    pub h: u32,
    pub seconds: f32,
    pub fps: f32,
    pub has_audio: bool,
}

/// Size, length and frame rate from ffprobe.
pub fn probe(path: &Path) -> Result<Info> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-print_format", "json", "-show_streams", "-show_format"])
        .arg(path)
        .output()
        .context("could not run ffprobe (brew install ffmpeg)")?;
    ensure!(out.status.success(), "ffprobe failed on {}", path.display());
    let d: Value = serde_json::from_slice(&out.stdout)?;
    let mut info = Info { seconds: d["format"]["duration"].as_str().and_then(|s| s.parse().ok()).unwrap_or(0.0), ..Default::default() };
    info.has_audio = d["streams"].as_array().into_iter().flatten().any(|s| s["codec_type"] == "audio");
    if let Some(v) = d["streams"].as_array().into_iter().flatten().find(|s| s["codec_type"] == "video") {
        info.w = v["width"].as_u64().unwrap_or(0) as u32;
        info.h = v["height"].as_u64().unwrap_or(0) as u32;
        let rate = v["avg_frame_rate"].as_str().unwrap_or("0/1");
        if let Some((a, b)) = rate.split_once('/') {
            let (a, b): (f32, f32) = (a.parse().unwrap_or(0.0), b.parse().unwrap_or(1.0));
            info.fps = if b > 0.0 { a / b } else { 0.0 };
        }
    }
    Ok(info)
}

/// RGBA8 pixels (straight alpha).
pub struct Picture {
    pub rgba: Vec<u8>,
    pub w: u32,
    pub h: u32,
}

pub fn load_image(path: &Path) -> Result<Picture> {
    let img = image::open(path).with_context(|| format!("opening {}", path.display()))?.to_rgba8();
    let (w, h) = img.dimensions();
    Ok(Picture { rgba: img.into_raw(), w, h })
}

/// Small preview of any image or video (first second), at most `max` px wide.
pub fn thumbnail(path: &Path, max: u32) -> Result<Picture> {
    match kind_of(path) {
        Some(MediaKind::Image) => {
            let img = image::open(path)?.thumbnail(max, max).to_rgba8();
            let (w, h) = img.dimensions();
            Ok(Picture { rgba: img.into_raw(), w, h })
        }
        Some(MediaKind::Video) => {
            let info = probe(path)?;
            let (w, h) = fit_width(info.w, info.h, max);
            let out = Command::new("ffmpeg")
                .args(["-v", "error", "-ss", &format!("{:.2}", (info.seconds * 0.1).min(1.0)), "-i"])
                .arg(path)
                .args(["-frames:v", "1", "-vf", &format!("scale={w}:{h}"), "-f", "rawvideo", "-pix_fmt", "rgba", "-"])
                .output()?;
            ensure!(out.stdout.len() == (w * h * 4) as usize, "no frame decoded");
            Ok(Picture { rgba: out.stdout, w, h })
        }
        _ => Err(anyhow!("no picture in {}", path.display())),
    }
}

/// Even dimensions no wider than `max`, keeping aspect.
fn fit_width(w: u32, h: u32, max: u32) -> (u32, u32) {
    let (w, h) = (w.max(2), h.max(2));
    if w <= max {
        return (w & !1, h & !1);
    }
    (max & !1, ((h as f32 * max as f32 / w as f32).round() as u32).max(2) & !1)
}

/// Sequential video decoder. Reading frames in order is cheap; jumping
/// restarts ffmpeg at the new time (fast seek to the nearest keyframe, then
/// exact decode).
pub struct VideoReader {
    path: PathBuf,
    pub w: u32,
    pub h: u32,
    pub fps: f32,
    pub seconds: f32,
    child: Option<(Child, ChildStdout)>,
    next: i64,
    pub frame: Vec<u8>,
    current: i64,
}

impl VideoReader {
    pub fn open(path: &Path, max_w: u32) -> Result<Self> {
        let info = probe(path)?;
        ensure!(info.w > 0 && info.fps > 0.0, "{} has no video stream", path.display());
        let (w, h) = fit_width(info.w, info.h, max_w);
        Ok(Self {
            path: path.to_path_buf(),
            w,
            h,
            fps: info.fps,
            seconds: info.seconds,
            child: None,
            next: 0,
            frame: vec![0; (w * h * 4) as usize],
            current: -1,
        })
    }

    fn start(&mut self, index: i64) -> Result<()> {
        if let Some((mut c, _)) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        let mut child = Command::new("ffmpeg")
            .args(["-v", "error", "-ss", &format!("{:.4}", index as f32 / self.fps), "-i"])
            .arg(&self.path)
            .args(["-an", "-vf", &format!("scale={}:{}", self.w, self.h), "-f", "rawvideo", "-pix_fmt", "rgba", "-"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let out = child.stdout.take().unwrap();
        self.child = Some((child, out));
        self.next = index;
        Ok(())
    }

    /// Updates `self.frame` to the frame shown at `seconds`. Returns whether it changed.
    pub fn seek(&mut self, seconds: f32) -> Result<bool> {
        let last = ((self.seconds * self.fps) as i64 - 1).max(0);
        let index = ((seconds.max(0.0) * self.fps) as i64).min(last);
        if index == self.current {
            return Ok(false);
        }
        // Skip ahead by reading if close, otherwise restart at the target.
        if self.child.is_none() || index < self.next || index > self.next + 48 {
            self.start(index)?;
        }
        while self.next <= index {
            let (_, out) = self.child.as_mut().unwrap();
            if out.read_exact(&mut self.frame).is_err() {
                self.child = None;
                break;
            }
            self.next += 1;
        }
        self.current = index;
        Ok(true)
    }
}

impl Drop for VideoReader {
    fn drop(&mut self) {
        if let Some((mut c, _)) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// Library folder for a kind of media, relative to the project.
pub fn folder(kind: MediaKind) -> &'static str {
    match kind {
        MediaKind::Audio => "audio",
        _ => "media",
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    /// Project-relative path, usable as `src`.
    pub src: String,
    pub kind: MediaKind,
}

/// Everything in the project's media and audio folders, subfolders included
/// (hidden files and folders skipped).
pub fn list(project: &Path) -> Vec<Item> {
    let mut out = Vec::new();
    for dir in ["media", "audio"] {
        let root = project.join(dir);
        let mut files = Vec::new();
        let mut stack = vec![root.clone()];
        while let Some(d) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&d) else { continue };
            for p in entries.flatten().map(|e| e.path()) {
                if p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')) {
                    continue;
                }
                if p.is_dir() {
                    stack.push(p);
                } else if p.is_file() {
                    files.push(p);
                }
            }
        }
        files.sort();
        for p in files {
            if let (Some(kind), Ok(rel)) = (kind_of(&p), p.strip_prefix(&root)) {
                out.push(Item { src: format!("{dir}/{}", rel.to_string_lossy()), kind });
            }
        }
    }
    out
}

fn unique(dir: &Path, name: &str) -> PathBuf {
    let p = dir.join(name);
    if !p.exists() {
        return p;
    }
    let (stem, ext) = name.rsplit_once('.').unwrap_or((name, ""));
    (2..).map(|i| dir.join(format!("{stem}_{i}.{ext}"))).find(|p| !p.exists()).unwrap()
}

/// Copies a file into the project library; returns its `src`.
pub fn import(project: &Path, file: &Path) -> Result<String> {
    let kind = kind_of(file).ok_or_else(|| anyhow!("{} is not an image, video or audio file", file.display()))?;
    let dir = project.join(folder(kind));
    std::fs::create_dir_all(&dir)?;
    let name = file.file_name().unwrap().to_string_lossy().replace(' ', "_");
    if file.starts_with(&dir) {
        return Ok(format!("{}/{name}", folder(kind)));
    }
    let dest = unique(&dir, &name);
    std::fs::copy(file, &dest)?;
    Ok(format!("{}/{}", folder(kind), dest.file_name().unwrap().to_string_lossy()))
}

/// One compact line per library item, for the agent.
pub fn describe(project: &Path, item: &Item) -> String {
    let info = probe(&project.join(&item.src)).unwrap_or_default();
    let secs = |s: f32| format!("{}:{:04.1}", (s / 60.0) as u32, s % 60.0);
    match item.kind {
        MediaKind::Image => format!("{} image {}x{}", item.src, info.w, info.h),
        MediaKind::Video => format!("{} video {}x{} {}fps {}", item.src, info.w, info.h, info.fps.round(), secs(info.seconds)),
        MediaKind::Audio => format!("{} audio {}", item.src, secs(info.seconds)),
    }
}
