//! Moodboards: folders of reference images shared across projects
//! (`~/.editor/moodboards/<folder>/`, or `$ED_MOODBOARDS`). Each project picks
//! the folders that are active for it in `<project>/.editor/moodboard.json`
//! (`{"active": [...]}`); the agent reads those as one contact sheet plus a
//! palette and luma summary per folder.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};

/// Image extensions a moodboard holds.
pub const EXTS: [&str; 5] = ["png", "jpg", "jpeg", "webp", "gif"];

/// Where moodboard folders live.
pub fn root() -> PathBuf {
    if let Some(dir) = std::env::var_os("ED_MOODBOARDS") {
        return PathBuf::from(dir);
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    home.join(".editor").join("moodboards")
}

fn is_image(p: &Path) -> bool {
    p.is_file() && p.extension().and_then(|e| e.to_str()).is_some_and(|e| EXTS.contains(&e.to_ascii_lowercase().as_str()))
}

/// Folder names under `root`, sorted.
pub fn folders(root: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().to_str().map(String::from))
        .filter(|n| !n.starts_with('.'))
        .collect();
    out.sort_by_key(|n| n.to_lowercase());
    out
}

/// Images in one folder, sorted by file name.
pub fn images(root: &Path, folder: &str) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(root.join(folder)).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| is_image(p)).collect();
    out.sort();
    out
}

fn check_name(name: &str) -> Result<&str> {
    let name = name.trim();
    ensure!(!name.is_empty() && !name.starts_with('.') && !name.contains(['/', '\\', ':']), "bad moodboard name `{name}`");
    Ok(name)
}

fn config(project: &Path) -> PathBuf {
    project.join(".editor").join("moodboard.json")
}

/// The folders active for `project`, in the order they were activated.
pub fn active(project: &Path) -> Vec<String> {
    std::fs::read_to_string(config(project))
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v["active"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()))
        .unwrap_or_default()
}

fn save_active(project: &Path, list: &[String]) -> Result<()> {
    let path = config(project);
    std::fs::create_dir_all(path.parent().unwrap())?;
    std::fs::write(&path, serde_json::to_string_pretty(&serde_json::json!({ "active": list }))?)?;
    Ok(())
}

/// Turns `folder` on or off for `project`.
pub fn set_active(project: &Path, folder: &str, on: bool) -> Result<()> {
    let mut list = active(project);
    let had = list.iter().any(|f| f == folder);
    if on && !had {
        list.push(folder.to_string());
    } else if !on && had {
        list.retain(|f| f != folder);
    } else {
        return Ok(());
    }
    save_active(project, &list)
}

fn unique(dir: &Path, name: &str) -> PathBuf {
    let p = dir.join(name);
    if !p.exists() {
        return p;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s, format!(".{e}")),
        _ => (name, String::new()),
    };
    (2..).map(|i| dir.join(format!("{stem}-{i}{ext}"))).find(|p| !p.exists()).unwrap()
}

/// Creates folder `name` (made unique if taken); returns its final name.
pub fn create(root: &Path, name: &str) -> Result<String> {
    let name = check_name(name)?;
    std::fs::create_dir_all(root)?;
    let dir = unique(root, name);
    std::fs::create_dir(&dir).with_context(|| format!("creating {}", dir.display()))?;
    Ok(dir.file_name().unwrap().to_string_lossy().into_owned())
}

/// Renames a folder; `project`'s active list follows it.
pub fn rename(root: &Path, project: &Path, old: &str, new: &str) -> Result<()> {
    let new = check_name(new)?;
    if old == new {
        return Ok(());
    }
    ensure!(!root.join(new).exists(), "`{new}` already exists");
    std::fs::rename(root.join(old), root.join(new)).with_context(|| format!("renaming {old}"))?;
    let mut list = active(project);
    if list.iter().any(|f| f == old) {
        list.iter_mut().filter(|f| *f == old).for_each(|f| *f = new.to_string());
        save_active(project, &list)?;
    }
    Ok(())
}

/// Moves a folder to `~/.Trash`.
pub fn trash(root: &Path, folder: &str) -> Result<()> {
    trash_path(&root.join(check_name(folder)?))
}

/// Moves a file or folder to `~/.Trash`.
pub fn trash_path(path: &Path) -> Result<()> {
    let home = std::env::var_os("HOME").map(PathBuf::from).context("no HOME")?;
    let bin = home.join(".Trash");
    std::fs::create_dir_all(&bin)?;
    let name = path.file_name().context("nothing to trash")?.to_string_lossy();
    std::fs::rename(path, unique(&bin, &name)).with_context(|| format!("trashing {}", path.display()))
}

/// Copies a local image or downloads a URL into `folder` (created if
/// missing); returns the new file.
pub fn add(root: &Path, folder: &str, what: &str) -> Result<PathBuf> {
    let folder = check_name(folder)?;
    let dir = root.join(folder);
    std::fs::create_dir_all(&dir)?;
    let what = what.trim();
    if what.starts_with("http://") || what.starts_with("https://") {
        let path_part = what.split(['?', '#']).next().unwrap_or(what);
        let name = path_part.rsplit('/').next().filter(|n| !n.is_empty()).unwrap_or("image");
        let mut name: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' }).collect();
        if !is_image_name(&name) {
            name.push_str(".jpg");
        }
        let dest = unique(&dir, &name);
        ed_media::web::download(what, &dest)?;
        if let Err(e) = ed_media::load_image(&dest) {
            let _ = std::fs::remove_file(&dest);
            bail!("{what} is not an image: {e:#}");
        }
        Ok(dest)
    } else {
        let src = PathBuf::from(what.strip_prefix("file://").unwrap_or(what));
        ensure!(is_image(&src), "not an image file: {}", src.display());
        let dest = unique(&dir, &src.file_name().unwrap().to_string_lossy());
        std::fs::copy(&src, &dest).with_context(|| format!("copying {}", src.display()))?;
        Ok(dest)
    }
}

fn is_image_name(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(_, e)| EXTS.contains(&e.to_ascii_lowercase().as_str()))
}

/// Box-filtered copy of `pic` that fits in `max` (never enlarged).
pub fn fit(pic: &ed_media::Picture, max: [u32; 2]) -> ed_media::Picture {
    let k = (max[0] as f32 / pic.w.max(1) as f32).min(max[1] as f32 / pic.h.max(1) as f32).min(1.0);
    let w = ((pic.w as f32 * k).round() as u32).max(1);
    let h = ((pic.h as f32 * k).round() as u32).max(1);
    if w == pic.w && h == pic.h {
        return ed_media::Picture { rgba: pic.rgba.clone(), w, h };
    }
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        let (y0, y1) = (y * pic.h / h, ((y + 1) * pic.h / h).max(y * pic.h / h + 1));
        for x in 0..w {
            let (x0, x1) = (x * pic.w / w, ((x + 1) * pic.w / w).max(x * pic.w / w + 1));
            let mut sum = [0u32; 4];
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let i = ((sy * pic.w + sx) * 4) as usize;
                    for c in 0..4 {
                        sum[c] += pic.rgba[i + c] as u32;
                    }
                }
            }
            let n = (y1 - y0) * (x1 - x0);
            let o = ((y * w + x) * 4) as usize;
            for c in 0..4 {
                rgba[o + c] = (sum[c] / n) as u8;
            }
        }
    }
    ed_media::Picture { rgba, w, h }
}

/// Colour and tone summary of a set of pixels.
#[derive(Debug, Clone, PartialEq)]
pub struct Look {
    /// Dominant colours, most common first (up to 5).
    pub palette: Vec<[u8; 3]>,
    /// Mean luma, 0..1.
    pub mean: f32,
    /// Luma standard deviation (contrast), 0..0.5.
    pub std: f32,
    /// Share of pixels with luma < 0.2 / > 0.8.
    pub shadows: f32,
    pub highlights: f32,
}

fn luma(p: [u8; 3]) -> f32 {
    (0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32) / 255.0
}

/// Palette (k-means, k=5, deterministic) and luma stats of RGBA8 `rgba`.
/// Transparent pixels are ignored.
pub fn analyze(rgba: &[u8]) -> Look {
    let all: Vec<[u8; 3]> = rgba.chunks_exact(4).filter(|p| p[3] >= 128).map(|p| [p[0], p[1], p[2]]).collect();
    if all.is_empty() {
        return Look { palette: Vec::new(), mean: 0.0, std: 0.0, shadows: 0.0, highlights: 0.0 };
    }
    // Stats over every pixel; clustering over an even sample.
    let n = all.len() as f32;
    let ls: Vec<f32> = all.iter().map(|&p| luma(p)).collect();
    let mean = ls.iter().sum::<f32>() / n;
    let std = (ls.iter().map(|l| (l - mean).powi(2)).sum::<f32>() / n).sqrt();
    let shadows = ls.iter().filter(|&&l| l < 0.2).count() as f32 / n;
    let highlights = ls.iter().filter(|&&l| l > 0.8).count() as f32 / n;

    let step = all.len().div_ceil(8192);
    let mut sample: Vec<[f32; 3]> = all.iter().step_by(step).map(|p| [p[0] as f32, p[1] as f32, p[2] as f32]).collect();
    sample.sort_by(|a, b| luma3(a).total_cmp(&luma3(b)));
    let k = 5.min(sample.len());
    // Seeds spread across the luma-sorted sample.
    let mut centers: Vec<[f32; 3]> = (0..k).map(|i| sample[(2 * i + 1) * sample.len() / (2 * k)]).collect();
    let mut assign = vec![0usize; sample.len()];
    for _ in 0..16 {
        for (a, p) in assign.iter_mut().zip(&sample) {
            *a = (0..centers.len()).min_by(|&i, &j| dist(p, &centers[i]).total_cmp(&dist(p, &centers[j]))).unwrap();
        }
        let mut sums = vec![[0f32; 4]; centers.len()];
        for (&a, p) in assign.iter().zip(&sample) {
            sums[a] = [sums[a][0] + p[0], sums[a][1] + p[1], sums[a][2] + p[2], sums[a][3] + 1.0];
        }
        let mut moved = false;
        for (c, s) in centers.iter_mut().zip(&sums) {
            if s[3] > 0.0 {
                let next = [s[0] / s[3], s[1] / s[3], s[2] / s[3]];
                moved |= dist(c, &next) > 0.25;
                *c = next;
            }
        }
        if !moved {
            break;
        }
    }
    let mut counts = vec![0usize; centers.len()];
    for &a in &assign {
        counts[a] += 1;
    }
    let mut order: Vec<usize> = (0..centers.len()).filter(|&i| counts[i] > 0).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(counts[i]));
    let mut palette: Vec<[u8; 3]> = Vec::new();
    for i in order {
        let c = centers[i].map(|v| v.round().clamp(0.0, 255.0) as u8);
        if !palette.contains(&c) {
            palette.push(c);
        }
    }
    Look { palette, mean, std, shadows, highlights }
}

fn luma3(p: &[f32; 3]) -> f32 {
    0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]
}

fn dist(a: &[f32; 3], b: &[f32; 3]) -> f32 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)
}

pub fn hex(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

impl Look {
    /// One line: palette, then tone.
    pub fn describe(&self) -> String {
        let palette: Vec<String> = self.palette.iter().map(|&c| hex(c)).collect();
        format!(
            "palette {} · luma mean {:.2} contrast(std) {:.2} · shadows {:.0}% highlights {:.0}%",
            palette.join(" "),
            self.mean,
            self.std,
            self.shadows * 100.0,
            self.highlights * 100.0
        )
    }
}

/// Size of one contact-sheet cell.
pub const CELL: [u32; 2] = [256, 192];
const BACKDROP: [u8; 4] = [24, 24, 24, 255];

/// `pic` letterboxed into a `CELL`-sized RGBA frame.
pub fn cell(pic: &ed_media::Picture) -> Vec<u8> {
    let small = fit(pic, CELL);
    let mut out: Vec<u8> = BACKDROP.iter().copied().cycle().take((CELL[0] * CELL[1] * 4) as usize).collect();
    let (ox, oy) = ((CELL[0] - small.w) / 2, (CELL[1] - small.h) / 2);
    for y in 0..small.h {
        for x in 0..small.w {
            let s = ((y * small.w + x) * 4) as usize;
            let o = (((y + oy) * CELL[0] + x + ox) * 4) as usize;
            let a = small.rgba[s + 3] as u32;
            for c in 0..3 {
                out[o + c] = ((small.rgba[s + c] as u32 * a + BACKDROP[c] as u32 * (255 - a)) / 255) as u8;
            }
        }
    }
    out
}

/// One folder read for the agent: its cells (at most `limit` images) and a
/// summary line over all of its images.
pub struct Read {
    pub cells: Vec<Vec<u8>>,
    pub line: String,
}

pub fn read(root: &Path, folder: &str, limit: usize) -> Read {
    let files = images(root, folder);
    let mut cells = Vec::new();
    let mut pixels = Vec::new();
    let mut names = Vec::new();
    for f in &files {
        let Ok(pic) = ed_media::load_image(f) else { continue };
        let small = fit(&pic, [128, 128]);
        pixels.extend_from_slice(&small.rgba);
        if cells.len() < limit {
            cells.push(cell(&pic));
            names.push(f.file_name().unwrap().to_string_lossy().into_owned());
        }
    }
    let look = if pixels.is_empty() { "(no readable images)".into() } else { analyze(&pixels).describe() };
    let more = if files.len() > cells.len() { format!(" (showing {})", cells.len()) } else { String::new() };
    Read { cells, line: format!("`{folder}` ({} images{more}): {look}\n  {}", files.len(), names.join(", ")) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_and_stats_of_a_split_image() {
        // 3/4 near-black, 1/4 pure red, 100 pixels.
        let mut rgba = Vec::new();
        for i in 0..100 {
            let p = if i % 4 == 0 { [255, 0, 0, 255] } else { [10, 10, 10, 255] };
            rgba.extend_from_slice(&p);
        }
        let look = analyze(&rgba);
        assert_eq!(look.palette[0], [10, 10, 10], "most common first");
        assert!(look.palette.contains(&[255, 0, 0]));
        assert_eq!(look.palette.len(), 2, "duplicate centres collapse");
        assert!((look.shadows - 0.75).abs() < 1e-6, "red's luma (0.21) is not a shadow: {look:?}");
        let red = 0.2126;
        let dark = 10.0 / 255.0;
        assert!((look.mean - (0.25 * red + 0.75 * dark)).abs() < 1e-3, "{look:?}");
        assert!(look.std > 0.05 && look.highlights == 0.0, "{look:?}");
        assert!(look.describe().contains("#0a0a0a #ff0000"));
    }

    #[test]
    fn folders_and_active_list() {
        let base = std::env::temp_dir().join(format!("ed-moodboard-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (root, project) = (base.join("boards"), base.join("project"));
        assert_eq!(create(&root, "neon").unwrap(), "neon");
        assert_eq!(create(&root, "neon").unwrap(), "neon-2");
        assert!(create(&root, "../x").is_err());
        set_active(&project, "neon", true).unwrap();
        rename(&root, &project, "neon", "noir").unwrap();
        assert_eq!(folders(&root), ["neon-2", "noir"]);
        assert_eq!(active(&project), ["noir"]);
        set_active(&project, "noir", false).unwrap();
        assert!(active(&project).is_empty());
        let _ = std::fs::remove_dir_all(&base);
    }
}
