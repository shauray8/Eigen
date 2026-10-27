//! "Video skill catalog" on the Plugins & Skills page: video skills from the
//! awesome-claude-video-skills list (repo tables + the "Tested end to end"
//! table) and the video-related entries of the claude-skill-registry index.
//!
//! Fetched with `curl` on a background thread when the cache
//! (`~/.editor/skill-catalog.json`) is older than a day. Installing clones into
//! `~/.editor/skills-src/` (sparse checkout for skills inside a bigger repo) and
//! ticks every SKILL.md skill found, i.e. links it into the project.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use eframe::egui;
use serde_json::{Value, json};

use crate::app::Panels;
use crate::extensions::{frontmatter, git_clone, home, is_skill_name, project_skills, set_skill, skill_dirs, skills_src};

const AWESOME_README: &str = "https://raw.githubusercontent.com/zhuyansen/awesome-claude-video-skills/main/README.md";
/// The registry's README points here: its deduplicated catalog index (top
/// entries with quality/security signals). The full registry is ~300k records
/// in 256 shards, far too much to pull for a filter.
const REGISTRY_INDEX: &str = "https://majiayu000.github.io/claude-skill-registry/search-index-lite.json";
const MAX_AGE: Duration = Duration::from_secs(24 * 3600);
const PAGE: usize = 40;

const AWESOME: &str = "awesome";
const REGISTRY: &str = "registry";

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Entry {
    pub owner: String,
    pub repo: String,
    /// Skill folder inside the repo; empty = the whole repo.
    pub path: String,
    /// Branch for `path` entries (empty = default branch).
    pub branch: String,
    pub name: String,
    pub stars: u64,
    pub description: String,
    pub category: String,
    /// In the awesome list's "Tested end to end" table.
    pub tested: bool,
    /// Time / look / sound notes from the test.
    pub note: String,
    /// Security grade (`SAFE`, `CAUTION`, registry `passed`, ...).
    pub security: String,
    /// `awesome` or `registry`.
    pub source: String,
}

impl Entry {
    fn key(&self) -> String {
        format!("{}/{}/{}", self.owner, self.repo, self.path).to_lowercase()
    }

    fn repo_key(&self) -> String {
        format!("{}/{}", self.owner, self.repo).to_lowercase()
    }

    fn url(&self) -> String {
        format!("https://github.com/{}/{}", self.owner, self.repo)
    }

    fn web(&self) -> String {
        if self.path.is_empty() {
            self.url()
        } else {
            let branch = if self.branch.is_empty() { "HEAD" } else { &self.branch };
            format!("{}/tree/{branch}/{}", self.url(), self.path)
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "owner": self.owner, "repo": self.repo, "path": self.path, "branch": self.branch,
            "name": self.name, "stars": self.stars, "description": self.description,
            "category": self.category, "tested": self.tested, "note": self.note,
            "security": self.security, "source": self.source,
        })
    }

    fn from_json(v: &Value) -> Option<Entry> {
        let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
        let e = Entry {
            owner: s("owner"),
            repo: s("repo"),
            path: s("path"),
            branch: s("branch"),
            name: s("name"),
            stars: v.get("stars").and_then(Value::as_u64).unwrap_or(0),
            description: s("description"),
            category: s("category"),
            tested: v.get("tested").and_then(Value::as_bool).unwrap_or(false),
            note: s("note"),
            security: s("security"),
            source: s("source"),
        };
        (!e.owner.is_empty() && !e.repo.is_empty()).then_some(e)
    }
}

// ---------------------------------------------------------------- awesome list

/// Table cells of a markdown row (`\|` unescaped; outer empties dropped).
fn cells(line: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut chars = line.trim().chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'|') => {
                chars.next();
                out.last_mut().unwrap().push('|');
            }
            '|' => out.push(String::new()),
            c => out.last_mut().unwrap().push(c),
        }
    }
    if out.first().is_some_and(|c| c.trim().is_empty()) {
        out.remove(0);
    }
    if out.last().is_some_and(|c| c.trim().is_empty()) {
        out.pop();
    }
    out.into_iter().map(|c| c.trim().to_string()).collect()
}

/// `[text](https://github.com/owner/repo[/tree/branch/path])` ->
/// (text, owner, repo, branch, path).
fn github_link(cell: &str) -> Option<(String, String, String, String, String)> {
    let open = cell.find('[')?;
    let close = open + cell[open..].find("](")?;
    let text = cell[open + 1..close].to_string();
    let rest = cell[close + 2..].strip_prefix("https://github.com/")?;
    let url = &rest[..rest.find(')')?];
    let mut segs = url.split('/').filter(|s| !s.is_empty());
    let owner = segs.next()?.to_string();
    let repo = segs.next()?.trim_end_matches(".git").to_string();
    let (mut branch, mut path) = (String::new(), String::new());
    if segs.next() == Some("tree") {
        branch = segs.next().filter(|b| *b != "HEAD").unwrap_or_default().to_string();
        path = segs.collect::<Vec<_>>().join("/");
    }
    Some((text, owner, repo, branch, path))
}

/// `71.8k`, `1,315`, `in EverMind-AI/Raven: 5.8k`.
fn parse_stars(cell: &str) -> u64 {
    let s = cell.rsplit(':').next().unwrap_or("").trim().replace(',', "").to_lowercase();
    let (num, mul) = match s.strip_suffix('k') {
        Some(n) => (n, 1e3),
        None => match s.strip_suffix('m') {
            Some(n) => (n, 1e6),
            None => (s.as_str(), 1.0),
        },
    };
    num.trim().parse::<f64>().map(|n| (n * mul).round() as u64).unwrap_or(0)
}

/// Link text of `[SAFE](...)`, or the plain cell.
fn link_text(cell: &str) -> String {
    match (cell.find('['), cell.find(']')) {
        (Some(a), Some(b)) if a < b => cell[a + 1..b].to_string(),
        _ => cell.to_string(),
    }
}

/// `🧱 Frameworks & toolkits` -> `Frameworks`.
fn category_name(heading: &str) -> String {
    let t = heading.trim_start_matches(|c: char| !c.is_alphanumeric()).trim();
    t.split(" & ").next().unwrap_or(t).trim().to_string()
}

/// Every repo row of the awesome list, with its section as category; rows of
/// the "Tested end to end" table mark the matching entry as tested.
pub fn parse_awesome(readme: &str) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    let mut tested: Vec<(Entry, String)> = Vec::new();
    let mut section = String::new();
    for line in readme.lines() {
        if let Some(h) = line.strip_prefix("## ") {
            section = category_name(h);
            continue;
        }
        if !line.trim_start().starts_with('|') {
            continue;
        }
        let c = cells(line);
        if section.starts_with("Tested") {
            // | # | [skill](url) | ★ | checks | lines | sound | look | time | evidence |
            if c.len() < 8 {
                continue;
            }
            let Some((text, owner, repo, branch, path)) = github_link(&c[1]) else { continue };
            let sound = if c[5].eq_ignore_ascii_case("yes") { " · sound" } else { "" };
            let note = format!("{} · {}{sound}", c[7], c[6]);
            let e = Entry { owner, repo, path, branch, name: text, stars: parse_stars(&c[2]), source: AWESOME.into(), ..Default::default() };
            tested.push((e, note));
            continue;
        }
        // | [owner/repo](url) | stars | what it does | security |
        if c.len() < 4 {
            continue;
        }
        let Some((text, owner, repo, branch, path)) = github_link(&c[0]) else { continue };
        let e = Entry {
            owner,
            repo,
            path,
            branch,
            name: text,
            stars: parse_stars(&c[1]),
            description: c[2].clone(),
            category: section.clone(),
            security: link_text(&c[3]),
            source: AWESOME.into(),
            ..Default::default()
        };
        // Repos repeat across sections ("Made with ..." then their type): keep the type.
        match out.iter_mut().find(|o| o.key() == e.key()) {
            Some(o) if o.category.starts_with("Made with") => o.category = e.category,
            Some(_) => {}
            None => out.push(e),
        }
    }
    for (t, note) in tested {
        match out.iter_mut().find(|o| o.key() == t.key()) {
            Some(o) => {
                o.tested = true;
                o.note = note;
            }
            None => out.push(Entry { tested: true, note, category: "Tested".into(), name: format!("{}/{}", t.owner, t.repo), ..t }),
        }
    }
    out
}

// ---------------------------------------------------------------- registry

/// Words that make an entry a video skill on their own.
const VIDEO_WORDS: &[&str] = &[
    "video", "film", "movie", "motion", "animation", "animate", "remotion", "hyperframes", "ffmpeg", "subtitle", "caption",
    "storyboard", "cinematic", "youtube", "tiktok", "reel", "shorts", "blender", "music video", "explainer", "trailer",
    "montage", "b-roll", "vfx", "lottie",
];
/// Words that also mean code ("edit the file", "render the component"); they
/// count only next to a footage word.
const WEAK_WORDS: &[&str] = &["edit", "editing", "render", "3d"];
const FOOTAGE_WORDS: &[&str] = &["footage", "clip", "timeline", "scene", "frame", "shot", "audio", "media"];

/// Lowercase words separated by single spaces, padded: ` b roll video `.
fn words(text: &str) -> String {
    let mut s = String::from(" ");
    for w in text.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()) {
        s.push_str(w);
        s.push(' ');
    }
    s
}

fn has_word(hay: &str, word: &str) -> bool {
    let w = words(word);
    let w = w.trim();
    hay.contains(&format!(" {w} ")) || hay.contains(&format!(" {w}s ")) || hay.contains(&format!(" {w}es "))
}

/// Is a skill with this name/description/tags about video?
pub fn is_video(name: &str, description: &str, tags: &[String]) -> bool {
    let hay = words(&format!("{name} {description} {}", tags.join(" ")));
    VIDEO_WORDS.iter().any(|w| has_word(&hay, w))
        || (WEAK_WORDS.iter().any(|w| has_word(&hay, w)) && FOOTAGE_WORDS.iter().any(|w| has_word(&hay, w)))
}

/// Video entries of the registry's `search-index-lite.json` (`{"skills": [...]}`).
pub fn parse_registry(index: &Value) -> Vec<Entry> {
    let Some(skills) = index.get("skills").and_then(Value::as_array) else { return Vec::new() };
    let mut out: Vec<Entry> = Vec::new();
    for s in skills {
        let str_of = |k: &str| s.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
        let name = str_of("name");
        let description = str_of("description");
        let tags: Vec<String> = s.get("tags").and_then(Value::as_array).map(|t| t.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default();
        if !is_video(&name, &description, &tags) {
            continue;
        }
        let repo_full = str_of("repo");
        let Some((owner, repo)) = repo_full.split_once('/') else { continue };
        let path = str_of("path").trim_matches('/').to_string();
        let path = if path == "." { String::new() } else { path };
        let e = Entry {
            owner: owner.to_string(),
            repo: repo.to_string(),
            path,
            branch: str_of("branch"),
            name,
            stars: s.get("stars").and_then(Value::as_u64).unwrap_or(0),
            description,
            category: "Registry".into(),
            security: str_of("security_status"),
            source: REGISTRY.into(),
            ..Default::default()
        };
        // The index lists copies of one skill at several paths of the same repo.
        if !out.iter().any(|o| o.key() == e.key() || (o.repo_key() == e.repo_key() && o.name == e.name)) {
            out.push(e);
        }
    }
    out
}

// ---------------------------------------------------------------- fetch + cache

fn cache_path() -> PathBuf {
    home().join(".editor/skill-catalog.json")
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn load_cache() -> Option<(u64, Vec<Entry>)> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(cache_path()).ok()?).ok()?;
    let fetched = v.get("fetched").and_then(Value::as_u64).unwrap_or(0);
    let entries = v.get("entries").and_then(Value::as_array)?.iter().filter_map(Entry::from_json).collect();
    Some((fetched, entries))
}

fn save_cache(fetched: u64, entries: &[Entry]) {
    let v = json!({ "fetched": fetched, "entries": entries.iter().map(Entry::to_json).collect::<Vec<_>>() });
    let path = cache_path();
    if let Some(p) = path.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    let _ = std::fs::write(path, v.to_string());
}

fn curl(url: &str) -> Result<String, String> {
    let out = std::process::Command::new("curl")
        .args(["-fsSL", "--max-time", "90", url])
        .output()
        .map_err(|e| format!("curl: {e}"))?;
    if !out.status.success() {
        return Err(format!("{url}: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    String::from_utf8(out.stdout).map_err(|e| e.to_string())
}

fn fetch_awesome() -> Result<Vec<Entry>, String> {
    let e = parse_awesome(&curl(AWESOME_README)?);
    if e.is_empty() { Err("awesome list: no rows parsed".into()) } else { Ok(e) }
}

fn fetch_registry() -> Result<Vec<Entry>, String> {
    let v: Value = serde_json::from_str(&curl(REGISTRY_INDEX)?).map_err(|e| format!("registry index: {e}"))?;
    if v.get("skills").is_none() {
        return Err("registry index: no `skills` array".into());
    }
    Ok(parse_registry(&v))
}

/// Both sources; a failed source keeps its cached entries.
fn fetch_all(cached: &[Entry]) -> (Vec<Entry>, Vec<String>) {
    let mut errors = Vec::new();
    let mut pick = |source: &str, r: Result<Vec<Entry>, String>| match r {
        Ok(e) => e,
        Err(e) => {
            errors.push(e);
            cached.iter().filter(|c| c.source == source).cloned().collect()
        }
    };
    let mut entries = pick(AWESOME, fetch_awesome());
    let registry = pick(REGISTRY, fetch_registry());
    for r in registry {
        if !entries.iter().any(|e| e.key() == r.key()) {
            entries.push(r);
        }
    }
    (entries, errors)
}

// ---------------------------------------------------------------- install

/// Clones on disk, keyed by lowercase `owner/repo`: (folder, sparse checkout).
fn scan_clones() -> HashMap<String, Vec<(PathBuf, bool)>> {
    let mut out: HashMap<String, Vec<(PathBuf, bool)>> = HashMap::new();
    let Ok(dirs) = std::fs::read_dir(skills_src()) else { return out };
    for d in dirs.flatten() {
        let config = std::fs::read_to_string(d.path().join(".git/config")).unwrap_or_default();
        let lower = config.to_lowercase();
        let Some(url) = lower.lines().find_map(|l| l.trim().strip_prefix("url = ").map(str::to_string)) else { continue };
        let Some(rest) = url.split("github.com").nth(1) else { continue };
        let key = rest.trim_start_matches([':', '/']).trim_end_matches('/').trim_end_matches(".git").to_string();
        let sparse = lower.contains("sparsecheckout = true");
        out.entry(key).or_default().push((d.path(), sparse));
    }
    out
}

/// The clone holding this entry, if any.
fn installed_dir(clones: &HashMap<String, Vec<(PathBuf, bool)>>, e: &Entry) -> Option<PathBuf> {
    let dirs = clones.get(&e.repo_key())?;
    if e.path.is_empty() {
        dirs.iter().find(|(_, sparse)| !sparse).map(|(d, _)| d.clone())
    } else {
        dirs.iter().find(|(d, _)| d.join(&e.path).is_dir()).map(|(d, _)| d.clone())
    }
}

/// A free folder under skills-src: the repo (or skill folder) name, prefixed
/// with the owner when taken (many repos are called `skills`).
fn free_dest(e: &Entry) -> Result<PathBuf, String> {
    let leaf = e.path.rsplit('/').next().filter(|l| !l.is_empty()).unwrap_or(&e.repo).to_string();
    let root = skills_src();
    let mut candidates = vec![leaf.clone(), format!("{}-{leaf}", e.owner)];
    candidates.extend((2..50).map(|n| format!("{}-{leaf}-{n}", e.owner)));
    candidates
        .into_iter()
        .filter(|n| is_skill_name(n))
        .map(|n| root.join(n))
        .find(|p| std::fs::symlink_metadata(p).is_err())
        .ok_or_else(|| format!("no usable folder name for {}", e.repo_key()))
}

fn git(dir: Option<&Path>, args: &[&str]) -> Result<(), String> {
    let mut cmd = std::process::Command::new("git");
    if let Some(d) = dir {
        cmd.arg("-C").arg(d);
    }
    let out = cmd.args(args).env("GIT_TERMINAL_PROMPT", "0").output().map_err(|e| e.to_string())?;
    if out.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&out.stderr).trim().to_string()) }
}

/// Clones the entry (sparse for a skill inside a repo); returns the folder to
/// look for skills in.
fn install(e: &Entry) -> Result<PathBuf, String> {
    let dest = free_dest(e)?;
    if e.path.is_empty() {
        return git_clone(&e.url(), &dest);
    }
    std::fs::create_dir_all(skills_src()).map_err(|err| err.to_string())?;
    let d = dest.to_string_lossy().into_owned();
    let url = e.url();
    let mut args = vec!["clone", "--depth", "1", "--filter=blob:none", "--sparse"];
    if !e.branch.is_empty() {
        args.extend(["--branch", e.branch.as_str()]);
    }
    args.extend(["--", url.as_str(), d.as_str()]);
    git(None, &args)?;
    if let Err(err) = git(Some(&dest), &["sparse-checkout", "set", &e.path]) {
        let _ = std::fs::remove_dir_all(&dest);
        return Err(err);
    }
    Ok(dest.join(&e.path))
}

fn update(dir: &Path) -> Result<PathBuf, String> {
    git(Some(dir), &["pull", "--ff-only"]).map(|()| dir.to_path_buf())
}

/// Links every SKILL.md skill under `root` (including `.claude/skills`) into
/// the project; returns how many.
pub fn tick_all(root: &Path, links: &Path) -> Result<usize, String> {
    let mut dirs = skill_dirs(root, 4);
    dirs.extend(skill_dirs(&root.join(".claude/skills"), 2));
    let mut n = 0;
    for d in dirs {
        let real = std::fs::canonicalize(&d).unwrap_or(d);
        let text = std::fs::read_to_string(real.join("SKILL.md")).unwrap_or_default();
        let fallback = real.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let name = frontmatter(&text).0.filter(|n| is_skill_name(n)).unwrap_or(fallback);
        set_skill(links, &name, &real, true)?;
        n += 1;
    }
    Ok(n)
}

enum Job {
    Install(Entry),
    Update(Entry, PathBuf),
}

enum Msg {
    Start { index: usize, name: String },
    Done { name: String, update: bool, result: Result<PathBuf, String> },
}

struct Installing {
    rx: Receiver<Msg>,
    total: usize,
    index: usize,
    current: String,
}

// ---------------------------------------------------------------- UI

#[derive(Default)]
pub struct Catalog {
    entries: Vec<Entry>,
    fetched: u64,
    loaded: bool,
    fetching: Option<Receiver<(Vec<Entry>, Vec<String>)>>,
    error: Option<String>,
    search: String,
    category: Option<String>,
    by_stars: bool,
    show_all: bool,
    clones: HashMap<String, Vec<(PathBuf, bool)>>,
    installing: Option<Installing>,
}

impl Catalog {
    /// Called by the page's periodic rescan.
    pub fn rescan(&mut self) {
        self.clones = scan_clones();
    }

    fn refresh(&mut self, ctx: &egui::Context) {
        let cached = self.entries.clone();
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let (entries, errors) = fetch_all(&cached);
            if errors.is_empty() || !entries.is_empty() {
                save_cache(if errors.is_empty() { now() } else { 0 }, &entries);
            }
            let _ = tx.send((entries, errors));
            ctx.request_repaint();
        });
        self.fetching = Some(rx);
        self.error = None;
    }

    fn start(&mut self, jobs: Vec<Job>, ctx: &egui::Context) {
        if jobs.is_empty() || self.installing.is_some() {
            return;
        }
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        let total = jobs.len();
        std::thread::spawn(move || {
            for (index, job) in jobs.into_iter().enumerate() {
                let (name, update, run): (String, bool, Box<dyn FnOnce() -> Result<PathBuf, String>>) = match job {
                    Job::Install(e) => (e.name.clone(), false, Box::new(move || install(&e))),
                    Job::Update(e, dir) => (e.name.clone(), true, Box::new(move || update(&dir))),
                };
                let _ = tx.send(Msg::Start { index, name: name.clone() });
                ctx.request_repaint();
                let result = run();
                if tx.send(Msg::Done { name, update, result }).is_err() {
                    return;
                }
                ctx.request_repaint();
            }
        });
        self.installing = Some(Installing { rx, total, index: 0, current: String::new() });
    }

    /// Returns true when skills on disk changed (the page should rescan).
    pub fn ui(&mut self, ui: &mut egui::Ui, panels: &mut Panels, project: &Path) -> bool {
        let mut changed = false;
        if !self.loaded {
            self.loaded = true;
            self.by_stars = true;
            if let Some((fetched, entries)) = load_cache() {
                self.fetched = fetched;
                self.entries = entries;
            }
            self.clones = scan_clones();
            if self.entries.is_empty() || now().saturating_sub(self.fetched) > MAX_AGE.as_secs() {
                self.refresh(ui.ctx());
            }
        }
        if let Some(rx) = &self.fetching {
            match rx.try_recv() {
                Ok((entries, errors)) => {
                    if errors.is_empty() {
                        self.fetched = now();
                    }
                    if !entries.is_empty() {
                        self.entries = entries;
                    }
                    self.error = (!errors.is_empty()).then(|| errors.join("; "));
                    self.fetching = None;
                }
                Err(TryRecvError::Disconnected) => self.fetching = None,
                Err(TryRecvError::Empty) => {}
            }
        }
        if let Some(inst) = &mut self.installing {
            let links = project_skills(project);
            loop {
                match inst.rx.try_recv() {
                    Ok(Msg::Start { index, name }) => {
                        inst.index = index;
                        inst.current = name;
                    }
                    Ok(Msg::Done { name, update, result }) => {
                        changed = true;
                        match result {
                            Ok(_) if update => panels.notify(format!("updated {name}")),
                            Ok(dir) => match tick_all(&dir, &links) {
                                Ok(n) => panels.notify(format!("installed {name}: {n} skill(s) enabled (agent picks them up on its next session)")),
                                Err(e) => panels.notify(format!("installed {name}, but enabling its skills failed: {e}")),
                            },
                            Err(e) => panels.notify(format!("{} {name} failed: {e}", if update { "update" } else { "install" })),
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        self.installing = None;
                        break;
                    }
                }
            }
        }
        if changed {
            self.clones = scan_clones();
        }

        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.search).hint_text("Search video skills").desired_width(240.0));
            ui.checkbox(&mut self.by_stars, "Sort by stars");
            let busy = self.fetching.is_some();
            if ui.add_enabled(!busy, egui::Button::new("Refresh catalog")).clicked() {
                self.refresh(ui.ctx());
            }
            if busy {
                ui.spinner();
                ui.weak("fetching catalog…");
            }
        });
        if let Some(e) = &self.error {
            ui.weak(format!("catalog fetch failed (showing cached): {e}"));
        }
        if self.entries.is_empty() {
            if self.fetching.is_none() {
                ui.label("Catalog empty. Press Refresh catalog.");
            }
            return changed;
        }

        self.recommended_ui(ui);
        ui.add_space(6.0);

        let categories: BTreeSet<&str> = self.entries.iter().map(|e| e.category.as_str()).collect();
        let mut pick = None;
        ui.horizontal_wrapped(|ui| {
            if ui.selectable_label(self.category.is_none(), "All").clicked() {
                pick = Some(None);
            }
            for c in &categories {
                let n = self.entries.iter().filter(|e| e.category == *c).count();
                if ui.selectable_label(self.category.as_deref() == Some(*c), format!("{c} ({n})")).clicked() {
                    pick = Some(Some(c.to_string()));
                }
            }
        });
        if let Some(p) = pick {
            self.category = p;
        }

        let query = self.search.trim().to_lowercase();
        let mut shown: Vec<&Entry> = self
            .entries
            .iter()
            .filter(|e| self.category.as_ref().is_none_or(|c| *c == e.category))
            .filter(|e| {
                query.is_empty()
                    || format!("{} {}/{} {} {}", e.name, e.owner, e.repo, e.description, e.category).to_lowercase().contains(&query)
            })
            .collect();
        if self.by_stars {
            shown.sort_by(|a, b| b.stars.cmp(&a.stars));
        }
        let total = shown.len();
        if !self.show_all {
            shown.truncate(PAGE);
        }
        let mut jobs = Vec::new();
        let busy = self.installing.is_some();
        for e in shown {
            if let Some(job) = card(ui, e, installed_dir(&self.clones, e), busy) {
                jobs.push(job);
            }
        }
        if total > PAGE {
            let label = if self.show_all { "Show fewer".to_string() } else { format!("Show all {total}") };
            if ui.button(label).clicked() {
                self.show_all = !self.show_all;
            }
        }
        if total == 0 {
            ui.weak("No skills match.");
        }
        self.start(jobs, ui.ctx());
        changed
    }

    fn recommended_ui(&mut self, ui: &mut egui::Ui) {
        let tested: Vec<&Entry> = self.entries.iter().filter(|e| e.tested).collect();
        if tested.is_empty() {
            return;
        }
        let missing: Vec<Entry> = tested.iter().filter(|e| installed_dir(&self.clones, e).is_none()).map(|e| (*e).clone()).collect();
        let mut go = false;
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.label(egui::RichText::new("Recommended starter set").strong());
                ui.weak(format!("{} skills tested end to end by the awesome list", tested.len()));
            });
            ui.horizontal_wrapped(|ui| {
                for e in &tested {
                    let done = installed_dir(&self.clones, e).is_some();
                    let text = egui::RichText::new(format!("{}{}", if done { "✔ " } else { "" }, e.name));
                    ui.label(if done { text.weak() } else { text }).on_hover_text(format!("{}/{} · ★{} · {}", e.owner, e.repo, e.stars, e.note));
                }
            });
            ui.horizontal(|ui| {
                match &self.installing {
                    Some(inst) => {
                        ui.spinner();
                        ui.label(format!("Installing {}/{}: {}", inst.index + 1, inst.total, inst.current));
                    }
                    None if missing.is_empty() => {
                        ui.weak("All recommended skills installed.");
                    }
                    None => go = ui.button(format!("Install recommended ({})", missing.len())).clicked(),
                }
            });
        });
        if go {
            self.start(missing.into_iter().map(Job::Install).collect(), ui.ctx());
        }
    }
}

fn card(ui: &mut egui::Ui, e: &Entry, installed: Option<PathBuf>, busy: bool) -> Option<Job> {
    let mut job = None;
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new(&e.name).strong());
            ui.weak(format!("★ {}", e.stars));
            ui.weak(&e.category);
            if e.tested {
                ui.label(egui::RichText::new("tested").small().color(egui::Color32::from_rgb(90, 190, 110)))
                    .on_hover_text(&e.note);
            }
            if !e.security.is_empty() {
                let color = match e.security.to_lowercase().as_str() {
                    "safe" | "passed" => egui::Color32::from_rgb(90, 190, 110),
                    "caution" | "warning" => egui::Color32::from_rgb(220, 170, 60),
                    "unsafe" | "failed" | "danger" => ui.visuals().error_fg_color,
                    _ => ui.visuals().weak_text_color(),
                };
                ui.label(egui::RichText::new(&e.security).small().color(color));
            }
            if e.source == REGISTRY {
                ui.weak(format!("{}/{}", e.owner, e.repo));
            }
        });
        if !e.description.is_empty() {
            let line: String = e.description.lines().next().unwrap_or_default().chars().take(160).collect();
            ui.add(egui::Label::new(line).truncate()).on_hover_text(&e.description);
        }
        if e.tested && !e.note.is_empty() {
            ui.weak(format!("Tested: {}", e.note));
        }
        ui.horizontal(|ui| {
            match &installed {
                Some(dir) => {
                    ui.weak("Installed").on_hover_text(dir.display().to_string());
                    if ui.add_enabled(!busy, egui::Button::new("Update")).on_hover_text("git pull").clicked() {
                        job = Some(Job::Update(e.clone(), dir.clone()));
                    }
                }
                None => {
                    let hover = format!("Clone into {} and enable its skills in this project", skills_src().display());
                    if ui.add_enabled(!busy, egui::Button::new("Install")).on_hover_text(hover).clicked() {
                        job = Some(Job::Install(e.clone()));
                    }
                }
            }
            if ui.button("GitHub").clicked() {
                let _ = std::process::Command::new("open").arg(e.web()).spawn();
            }
        });
    });
    job
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rows copied from the live README (2026-10-26).
    const EXCERPT: &str = r#"
## 🧪 Tested end to end

| # | Skill | ★ | Checks passed (of 4) | Lines of copy readable (of 5) | Sound | Look, by eye | Time to make |  |
|---|---|---|---|---|---|---|---|---|
| 4 | [skills](https://github.com/remotion-dev/skills) | 5,005 | 4/4 | 5/5 | No | Close to the no-skill film | 4.5 min | [evidence](https://agentskillshub.top/best-runs/video/remotion-dev__skills.html) |

## Made with Claude Opus 5.5

| Repo | Stars | What it does | Security |
|---|---:|---|---|
| [ledbetterljoshua/functional-emotions-video](https://github.com/ledbetterljoshua/functional-emotions-video) | 71 | A painted music video for "Functional Emotions", made by Claude Opus 5.5: custom GPU brushstroke renderer, storyboard, and 7 parallel chapter agents. | [SAFE](https://agentskillshub.top/skill/ledbetterljoshua/functional-emotions-video/?utm_source=github&utm_medium=awesome-list) |

<a id="type-general"></a>
## 🧱 Frameworks & toolkits

| Repo | Stars | What it does | Security |
|---|---:|---|---|
| [remotion-dev/skills](https://github.com/remotion-dev/skills) | 5.7k | Agent Skills | [SAFE](https://agentskillshub.top/skill/remotion-dev/skills/?utm_source=github&utm_medium=awesome-list) |
| [kangarooking/director-skills](https://github.com/kangarooking/director-skills) | 239 | 导演Skill：面向 AI 视频创作的开源 Agent Skills \| Director Skills: Open-source Agent Skills for AI video creation. | [SAFE](https://agentskillshub.top/skill/kangarooking/director-skills/?utm_source=github&utm_medium=awesome-list) |
| [argus-metis/hermes-video-editing](https://github.com/argus-metis/hermes-video-editing) | 7 | Professional video editing skill for Hermes Agent — AI captioning, silence removal, scene detection, stabilisation, smart reframing, and more. All fr… | [CAUTION](https://agentskillshub.top/skill/argus-metis/hermes-video-editing/?utm_source=github&utm_medium=awesome-list) |

## 🎓 Explainers

| Repo | Stars | What it does | Security |
|---|---:|---|---|
| [git-story-film](https://github.com/EverMind-AI/Raven/tree/HEAD/skills/git-story-film) | in EverMind-AI/Raven: 5.8k | Direct a two-minute hand-drawn animated film from a repository's Git history: story, character, storyboard, HTML review studio, then a 4K/60fps MP4. | [SAFE](https://agentskillshub.top/skill/EverMind-AI/Raven/?utm_source=github&utm_medium=awesome-list) |

## 🎵 Music videos

| Repo | Stars | What it does | Security |
|---|---:|---|---|
| [ledbetterljoshua/functional-emotions-video](https://github.com/ledbetterljoshua/functional-emotions-video) | 71 | A painted music video | [SAFE](https://agentskillshub.top/skill/ledbetterljoshua/functional-emotions-video/) |
"#;

    #[test]
    fn parses_awesome_rows() {
        let e = parse_awesome(EXCERPT);
        assert_eq!(e.len(), 5, "{e:#?}");
        let remotion = &e[1];
        assert_eq!((remotion.owner.as_str(), remotion.repo.as_str(), remotion.path.as_str()), ("remotion-dev", "skills", ""));
        assert_eq!(remotion.stars, 5700);
        assert_eq!(remotion.category, "Frameworks");
        assert_eq!(remotion.security, "SAFE");
        assert!(remotion.tested);
        assert_eq!(remotion.note, "4.5 min · Close to the no-skill film");
        let director = &e[2];
        assert!(director.description.contains("Agent Skills | Director Skills"), "{}", director.description);
        assert!(!director.tested);
        assert_eq!(e[3].security, "CAUTION");
        let film = &e[4];
        assert_eq!((film.owner.as_str(), film.repo.as_str(), film.path.as_str()), ("EverMind-AI", "Raven", "skills/git-story-film"));
        assert_eq!(film.name, "git-story-film");
        assert_eq!(film.stars, 5800);
        assert_eq!(film.category, "Explainers");
        // Listed under "Made with" first, then its type wins.
        assert_eq!(e[0].category, "Music videos");
        assert_eq!(e.iter().filter(|x| x.tested).count(), 1);
    }

    #[test]
    fn parses_stars() {
        assert_eq!(parse_stars("71.8k"), 71800);
        assert_eq!(parse_stars("1,315"), 1315);
        assert_eq!(parse_stars("998"), 998);
        assert_eq!(parse_stars(""), 0);
    }

    #[test]
    fn video_filter() {
        let t = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert!(is_video("remotion-best-practices", "Best practices for Remotion", &[]));
        assert!(is_video("yt-summary", "Summarize YouTube videos", &[]));
        assert!(is_video("broll-finder", "Find b-roll for a cut", &[]));
        assert!(is_video("x", "Generate subtitles with whisper", &[]));
        assert!(is_video("x", "y", &t(&["lottie"])));
        assert!(is_video("clip-trim", "Edit footage on a timeline", &[]));
        assert!(is_video("music-video", "", &[]));
        assert!(!is_video("docx", "Create and edit Word documents", &t(&["documents"])));
        assert!(!is_video("react-perf", "Avoid unnecessary re-renders and render work in React", &[]));
        assert!(!is_video("pdf", "Extract text from PDFs", &t(&["pdf"])));
        assert!(!is_video("promotion-planner", "Plan a sales promotion", &[]), "word match, not substring");

        let index = json!({"skills": [
            {"name": "video-editor", "description": "Cut clips with ffmpeg", "tags": [], "repo": "a/b", "path": "skills/video-editor", "branch": "main", "stars": 10, "security_status": "passed"},
            {"name": "add-uint-support", "description": "Add uint types to PyTorch operators", "tags": ["add"], "repo": "pytorch/pytorch", "path": ".claude/skills/add-uint-support", "stars": 9},
        ]});
        let e = parse_registry(&index);
        assert_eq!(e.len(), 1);
        assert_eq!((e[0].owner.as_str(), e[0].path.as_str(), e[0].security.as_str()), ("a", "skills/video-editor", "passed"));
        assert_eq!(e[0].category, "Registry");
    }

    #[test]
    fn cache_roundtrip() {
        let e = parse_awesome(EXCERPT);
        let back: Vec<Entry> = e.iter().map(|x| Entry::from_json(&x.to_json()).unwrap()).collect();
        assert_eq!(back, e);
    }

    /// `cargo test -p ed-app --bin eigen live_catalog -- --ignored --nocapture`
    #[test]
    #[ignore = "network"]
    fn live_catalog() {
        let awesome = fetch_awesome().unwrap();
        let registry = fetch_registry().unwrap();
        let mut by_cat: Vec<(String, usize)> = Vec::new();
        for e in &awesome {
            match by_cat.iter_mut().find(|(c, _)| *c == e.category) {
                Some((_, n)) => *n += 1,
                None => by_cat.push((e.category.clone(), 1)),
            }
        }
        println!("awesome: {} entries, {} tested; by category {by_cat:?}", awesome.len(), awesome.iter().filter(|e| e.tested).count());
        println!("registry: {} video entries", registry.len());
        for e in registry.iter().take(15) {
            println!("  {} ({}/{}) {}", e.name, e.owner, e.repo, e.description.chars().take(80).collect::<String>());
        }
    }
}
