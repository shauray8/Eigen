//! The agent's seat in the editor: an MCP server inside the app process.
//! Small tools over the same session the UI edits. The agent speaks
//! operators, exactly like the human's clicks do, and grows the editor with
//! plugins at runtime.

pub mod plugins;
mod raster;
pub mod moodboard;
pub mod refs;
pub mod sandbox;
pub mod storyboard;

use std::sync::Arc;

use base64::Engine;
use ed_core::{Session, Source, eval};
use ed_render::Renderer;
use parking_lot::Mutex;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, ServerCapabilities, ServerConfig};
use rmcp::transport::streamable_http_server::{StreamableHttpService, session::local::LocalSessionManager};
use rmcp::{ServerHandler, schemars, tool, tool_handler, tool_router};

pub const INSTRUCTIONS: &str = include_str!("instructions.md");

/// State shared between the UI thread and the agent. One per app; the
/// project inside it can be switched (see [`Shared::open`]).
pub struct Shared {
    /// Project directory (scene file, media, exports).
    dir: Mutex<std::path::PathBuf>,
    pub session: Mutex<Session>,
    pub renderer: Mutex<Renderer>,
    /// Last log entry the agent has been shown by `log`.
    agent_seen: Mutex<u64>,
    /// Called after the agent changes anything (wakes the UI).
    notify: Box<dyn Fn() + Send + Sync>,
    /// Requests from the agent to the app's UI, consumed each frame.
    pub ui: Mutex<UiRequest>,
    /// What the human pointed at (`@R<n>`) and review notes (`@N<n>`).
    pub refs: Mutex<refs::Refs>,
    /// The planned film as sketch panels (`@P<n>`).
    pub storyboard: Mutex<storyboard::Storyboard>,
}

#[derive(Default)]
pub struct UiRequest {
    pub play: Option<bool>,
    /// `canvas` or `3d`.
    pub mode: Option<String>,
    /// A note for the human, shown in the app.
    pub say: Option<String>,
    /// Library changed (reload thumbnails / media).
    pub library_changed: bool,
    /// A running tool plugin's latest progress line.
    pub progress: Option<String>,
}

impl Shared {
    pub fn new(
        dir: std::path::PathBuf,
        session: Session,
        renderer: Renderer,
        notify: impl Fn() + Send + Sync + 'static,
    ) -> Arc<Self> {
        renderer.set_root(dir.clone());
        Arc::new(Self {
            refs: Mutex::new(refs::Refs::load(&dir)),
            storyboard: Mutex::new(storyboard::Storyboard::load(&dir)),
            dir: Mutex::new(dir),
            session: Mutex::new(session),
            renderer: Mutex::new(renderer),
            agent_seen: Mutex::new(0),
            notify: Box::new(notify),
            ui: Mutex::new(UiRequest::default()),
        })
    }

    /// The open project's directory.
    pub fn dir(&self) -> std::path::PathBuf {
        self.dir.lock().clone()
    }

    /// Switches to the project in `dir`, whose document is `session`: its
    /// refs, storyboard and media root come with it.
    pub fn open(&self, dir: std::path::PathBuf, session: Session) {
        *self.session.lock() = session;
        *self.refs.lock() = refs::Refs::load(&dir);
        *self.storyboard.lock() = storyboard::Storyboard::load(&dir);
        {
            let r = self.renderer.lock();
            r.set_root(dir.clone());
            r.reload_media();
        }
        *self.agent_seen.lock() = 0;
        *self.dir.lock() = dir;
        (self.notify)();
    }
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct OpsArgs {
    /// Op-language script. Runs as one undo step; all or nothing.
    pub script: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct QueryArgs {
    /// Selector: `*`, `dump`, `scene`, `<name>`, `<name>.<prop>[.<c>]`, `kind=<kind>`, `comp=<comp>`; optional `@frame`.
    #[serde(default)]
    pub sel: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct RenderArgs {
    /// Frames to render into one contact sheet, left-to-right then down. Default: the human's current frame.
    #[serde(default)]
    pub frames: Vec<f32>,
    /// Width of each frame in pixels (default 384). Keep small; it costs tokens.
    pub width: Option<u32>,
    /// Columns in the sheet (default 3).
    pub cols: Option<u32>,
    /// A composition to render on its own (its frames start at 0). Default: the film.
    pub comp: Option<String>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct UndoArgs {
    /// Number of steps (default 1).
    pub steps: Option<u32>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct LogArgs {
    /// Show entries after this sequence number. Default: since your last `log` call.
    pub since: Option<u64>,
    /// Include your own entries too (default: only the human's).
    #[serde(default)]
    pub all: bool,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct ExportArgs {
    /// Output file, relative to the project (default `exports/<project>-NNN.mp4`; `.mov` with codec prores).
    pub path: Option<String>,
    /// `h264` (default) or `prores` (422 HQ, for grading/finishing).
    pub codec: Option<String>,
    /// Output width in pixels (default: scene res).
    pub width: Option<u32>,
}

/// Export motion blur: a frame is the average of this many sub-frames over
/// the open shutter. The shutter opens at the frame and stays open
/// `scene.shutter` of it, so a cut on a whole frame never blends two shots.
const BLUR_SAMPLES: u32 = 8;

/// Where the next export of the project in `dir` goes: `exports/<project>-NNN.mp4`
/// (`.mov` for ProRes), numbered so no export overwrites another.
pub fn next_export_path(dir: &std::path::Path, codec: &str) -> std::path::PathBuf {
    let name = dir.file_name().map_or_else(|| "film".to_string(), |n| n.to_string_lossy().into_owned());
    let ext = if codec == "prores" { "mov" } else { "mp4" };
    let exports = dir.join("exports");
    (1..).map(|n| exports.join(format!("{name}-{n:03}.{ext}"))).find(|p| !p.exists()).unwrap()
}

/// Renders scene.start..=scene.end and encodes it with ffmpeg (hardware
/// VideoToolbox encoders on macOS). Returns a one-line summary.
pub fn export_video(shared: &Shared, path: &std::path::Path, codec: &str, width: Option<u32>) -> anyhow::Result<String> {
    use std::io::Write;
    let t0 = std::time::Instant::now();
    let doc = shared.session.lock().doc().clone();
    let sc = &doc.scene;
    let w = width.unwrap_or(sc.res[0]) & !1;
    let h = ((w as f32 * sc.res[1] as f32 / sc.res[0] as f32).round() as u32) & !1;
    let codec_args: &[&str] = match codec {
        "prores" => &["-c:v", "prores_videotoolbox", "-profile:v", "hq", "-pix_fmt", "p210le"],
        _ => &["-c:v", "h264_videotoolbox", "-b:v", "40M", "-pix_fmt", "yuv420p", "-movflags", "+faststart"],
    };
    let clips: Vec<_> = audio_clips(&doc, &shared.dir()).into_iter().filter(|c| c.0.exists()).collect();
    let duration = (sc.end - sc.start + 1) as f32 / sc.fps;
    let mut cmd = std::process::Command::new("ffmpeg");
    cmd.args(["-y", "-loglevel", "error", "-f", "rawvideo", "-pix_fmt", "rgba", "-s", &format!("{w}x{h}")])
        .args(["-r", &sc.fps.to_string(), "-i", "-"]);
    if !clips.is_empty() {
        // Each clip: trim, place at its start, set volume; then mix.
        let mut graph = String::new();
        for (i, (file, start, trim, vol, stop)) in clips.iter().enumerate() {
            cmd.arg("-i").arg(file);
            let len = (stop - start).max(0.0);
            let delay = ((start - sc.start as f32 / sc.fps).max(0.0) * 1000.0) as u32;
            graph += &format!(
                "[{}:a]atrim=start={trim}:duration={len},asetpts=PTS-STARTPTS,volume={vol},adelay={delay}|{delay}[a{i}];",
                i + 1
            );
        }
        let inputs: String = (0..clips.len()).map(|i| format!("[a{i}]")).collect();
        graph += &format!("{inputs}amix=inputs={}:normalize=0,atrim=duration={duration}[aout]", clips.len());
        cmd.args(["-filter_complex", &graph, "-map", "0:v", "-map", "[aout]"]);
        cmd.args(if codec == "prores" { ["-c:a", "pcm_s24le", "-ar", "48000"] } else { ["-c:a", "aac", "-b:a", "320k"] });
    }
    let mut ff = cmd
        .args(codec_args)
        .arg(path)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| anyhow::anyhow!("could not start ffmpeg ({e}); install it with `brew install ffmpeg`"))?;
    let mut stdin = ff.stdin.take().unwrap();
    let frames = sc.start..=sc.end;
    let count = frames.clone().count();
    // ffmpeg reads on its own thread while the GPU renders the next frames;
    // frame buffers travel back for reuse instead of being reallocated.
    let (to_ffmpeg, encoded) = std::sync::mpsc::sync_channel::<Vec<u8>>(2);
    let (recycle, spare) = std::sync::mpsc::channel::<Vec<u8>>();
    let writer = std::thread::spawn(move || -> std::io::Result<()> {
        for px in encoded {
            stdin.write_all(&px)?;
            let _ = recycle.send(px);
        }
        Ok(())
    });
    let samples = if sc.shutter > 0.0 { BLUR_SAMPLES } else { 1 };
    let mut stream = shared.renderer.lock().frame_stream([w, h]);
    let mut buf = Vec::new();
    let mut result = Ok(());
    for f in frames {
        let f = f as f32;
        let at = |k: u32| if samples == 1 { f } else { f + sc.shutter * (k as f32 + 0.5) / samples as f32 };
        let scenes: Vec<eval::EvalScene> = (0..samples).map(|k| eval::evaluate(&doc, at(k))).collect();
        match stream.push(&shared.renderer.lock(), &scenes, &mut buf) {
            Ok(true) => {
                let next = spare.try_recv().unwrap_or_default();
                if to_ffmpeg.send(std::mem::replace(&mut buf, next)).is_err() {
                    break; // the writer failed; its error is reported below
                }
            }
            Ok(false) => {}
            Err(e) => {
                result = Err(e);
                break;
            }
        }
    }
    if result.is_ok() && stream.finish(&shared.renderer.lock(), &mut buf)? {
        let _ = to_ffmpeg.send(buf);
    }
    drop(to_ffmpeg);
    let written = writer.join().map_err(|_| anyhow::anyhow!("ffmpeg writer panicked"))?;
    result?;
    written.map_err(|e| anyhow::anyhow!("writing frames to ffmpeg: {e}"))?;
    let status = ff.wait()?;
    anyhow::ensure!(status.success(), "ffmpeg failed ({status})");
    let audio = if clips.is_empty() { String::new() } else { format!(" with {} audio clip(s)", clips.len()) };
    Ok(format!("exported {count} frames {w}x{h}{audio} to {} in {:.1}s", path.display(), t0.elapsed().as_secs_f32()))
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct MusicArgs {
    /// Mood/genre/tempo words, e.g. "dark cinematic pulse", "upbeat funk 120 bpm". Local files match by name.
    pub q: String,
    /// Minimum length in seconds (e.g. the film's length).
    pub min_seconds: Option<f32>,
    pub max_seconds: Option<f32>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct TrackArgs {
    /// A result id from `music` (ov:/cc:/ia:/local:) to download into the project, or a project-relative file (audio/x.mp3) to analyze.
    pub id: String,
}

/// Audio clips of the film (sound inside compositions is not mixed yet), for
/// export mixing: (file, start s, trim s, volume, end s).
fn audio_clips(doc: &ed_core::data::Document, dir: &std::path::Path) -> Vec<(std::path::PathBuf, f32, f32, f32, f32)> {
    let fps = doc.scene.fps;
    let end = (doc.scene.end + 1) as f32 / fps;
    doc.objects
        .values()
        .filter(|o| o.comp.is_none() && o.visible && !o.media.src.is_empty())
        .filter(|o| match o.kind {
            ed_core::data::Kind::Audio => true,
            ed_core::data::Kind::Video => {
                o.media.volume > 0.0 && ed_media::probe(&dir.join(&o.media.src)).is_ok_and(|i| i.has_audio)
            }
            _ => false,
        })
        .map(|o| {
            let start = o.t_in.unwrap_or(0.0) / fps;
            let stop = o.t_out.map_or(end, |f| f / fps);
            (dir.join(&o.media.src), start, o.media.trim, o.media.volume, stop)
        })
        .collect()
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct FetchArgs {
    /// A URL, an `ovi:` id from `images`, or a local file path. Lands in the project library.
    pub what: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct QArgs {
    pub q: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct TexturesArgs {
    /// texture (default), hdri or model.
    #[serde(rename = "type")]
    pub kind: Option<String>,
    /// Search words, e.g. "blue fabric", "brushed metal", "studio", "sunset", "plant", "chair".
    pub q: Option<String>,
    /// An id from the search to download.
    pub get: Option<String>,
    /// 1k (default), 2k or 4k.
    pub res: Option<String>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct UiArgs {
    /// Move the human's playhead to this frame.
    pub seek: Option<f32>,
    /// Select an object in the human's panels.
    pub select: Option<String>,
    pub play: Option<bool>,
    /// Viewport: `canvas` (the film) or `3d` (orbit the world).
    pub mode: Option<String>,
    /// A short note shown to the human in the app.
    pub say: Option<String>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct RefArgs {
    /// A ref from the human's message, e.g. `R3` or `N1`.
    pub id: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct NotesArgs {
    /// Post a director note with this text (what is wrong and what it should become).
    pub add: Option<String>,
    /// Frames the new note covers: [frame] or [first, last]. Default: the human's current frame.
    #[serde(default)]
    pub frames: Vec<f32>,
    /// Canvas region (px) the new note is about: [x, y, w, h].
    pub rect: Option<[f32; 4]>,
    /// Composition the new note's frames and region belong to. Default: the film.
    pub comp: Option<String>,
    /// Mark this note done (e.g. `N2`) once you have addressed it.
    pub resolve: Option<String>,
    /// One line for the human: what you changed for the note.
    pub reply: Option<String>,
    /// List resolved notes too.
    #[serde(default)]
    pub all: bool,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct PluginArgs {
    /// list (default) | help (the plugin contract) | show (read a plugin's files) | write (create/replace + check) | run (a tool plugin).
    #[serde(default)]
    pub action: String,
    pub name: Option<String>,
    /// write: fx | layer | tool.
    pub kind: Option<String>,
    /// write: one line, what it does.
    pub about: Option<String>,
    /// write: what params.x..w mean (shaders) or the args a tool takes.
    #[serde(default)]
    pub params: Vec<String>,
    /// write: tool command, run in the plugin folder, e.g. "python3 run.py".
    pub run: Option<String>,
    /// write: files by relative path, e.g. {"shader.wgsl": "..."} or {"run.py": "..."}.
    #[serde(default)]
    pub files: std::collections::BTreeMap<String, String>,
    /// write: "library" to share with every project (default: this project).
    pub scope: Option<String>,
    /// run: arguments for the tool (any JSON object).
    #[serde(default)]
    pub args: serde_json::Value,
    /// run: seconds before the tool is stopped (default 600).
    pub timeout: Option<u64>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct RequestArgs {
    /// What the editor itself should gain, in one line.
    pub title: String,
    /// Why a plugin cannot do it.
    pub why: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema, Default)]
pub struct PanelArgs {
    pub caption: Option<String>,
    pub seconds: Option<f32>,
    /// A picture under the sketch: a project path (render, reference, generated draft).
    pub image: Option<String>,
    /// The comp built for this shot.
    pub comp: Option<String>,
    /// Rough lines over it, points 0..1 across the panel (y down), drawn in blue ink.
    pub strokes: Option<Vec<Vec<[f32; 2]>>>,
    /// Insert before this 1-based position (default: at the end).
    pub at: Option<usize>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct StoryboardArgs {
    /// One panel to look at or change, e.g. `P2`. Without it: the whole board as one image.
    pub panel: Option<String>,
    /// Changes to `panel` (strokes replace the agent's blue lines; the human's sketch stays).
    pub set: Option<PanelArgs>,
    /// New panels, in order: draft a whole board in one call.
    #[serde(default)]
    pub add: Vec<PanelArgs>,
    /// Delete this panel.
    pub del: Option<String>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct MoodboardArgs {
    /// Look at this one folder (active or not). Without it: every active folder.
    pub folder: Option<String>,
    /// A URL or local image path to copy into `folder` (created if missing).
    pub add: Option<String>,
    /// Make this folder active for the project.
    pub activate: Option<String>,
    /// Stop using this folder for the project.
    pub deactivate: Option<String>,
}

#[derive(Clone)]
pub struct Editor {
    shared: Arc<Shared>,
}

fn text(s: impl Into<String>) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(s)])
}

fn fail(s: impl Into<String>) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(s)])
}

impl Editor {
    /// Renders `frames` of composition `comp` (None: the film) into one
    /// contact-sheet PNG, optionally marking what `mark` points at.
    fn sheet(&self, comp: Option<&str>, frames: &[f32], width: u32, cols: u32, mark: Option<&refs::Ref>) -> anyhow::Result<ContentBlock> {
        anyhow::ensure!(frames.len() <= 24, "at most 24 frames per call");
        // Evaluate (and find the marks) under the lock; render after releasing it.
        let (frames, res): (Vec<_>, _) = {
            let s = self.shared.session.lock();
            let doc = s.doc();
            if let Some(c) = comp {
                anyhow::ensure!(doc.objects.get(c).is_some_and(|o| o.kind == ed_core::data::Kind::Comp), "no comp `{c}`");
            }
            let frames = frames.iter().map(|&f| (eval::evaluate_comp(doc, comp, f), mark.map(|r| refs::marks(r, doc, f)))).collect();
            (frames, doc.comp_canvas(comp).0)
        };
        let w = width.clamp(64, 1920);
        let size = [w, ((w as f32) * res[1] as f32 / res[0] as f32).round().max(1.0) as u32];
        let mut out = Vec::with_capacity(frames.len());
        for (scene, marks) in &frames {
            let mut px = self.shared.renderer.lock().render_rgba(scene, size)?;
            if let Some(m) = marks {
                refs::draw(&mut px, size, res, m);
            }
            out.push(px);
        }
        let (sheet, sheet_size) = ed_render::contact_sheet(&out, size, cols);
        let png = ed_render::encode_png(&sheet, sheet_size)?;
        Ok(ContentBlock::image(base64::engine::general_purpose::STANDARD.encode(png), "image/png"))
    }
}

#[tool_router]
impl Editor {
    pub fn new(shared: Arc<Shared>) -> Self {
        Self { shared }
    }

    #[tool(description = "Apply an op-language script to the document (see server instructions).")]
    fn ops(&self, Parameters(OpsArgs { script }): Parameters<OpsArgs>) -> CallToolResult {
        let result = self.shared.session.lock().run(&script, Source::Agent);
        (self.shared.notify)();
        match result {
            Ok(n) => text(format!("ok {n}")),
            Err(e) => fail(format!("{e:#}")),
        }
    }

    #[tool(description = "Read document state as op-language text.")]
    fn q(&self, Parameters(QueryArgs { sel }): Parameters<QueryArgs>) -> CallToolResult {
        match self.shared.session.lock().query(&sel) {
            Ok(s) => text(s),
            Err(e) => fail(format!("{e:#}")),
        }
    }

    #[tool(description = "Render frames through the scene camera as one contact-sheet image.")]
    fn render(&self, Parameters(args): Parameters<RenderArgs>) -> CallToolResult {
        let frames = if args.frames.is_empty() { vec![self.shared.session.lock().frame] } else { args.frames };
        match self.sheet(args.comp.as_deref(), &frames, args.width.unwrap_or(384), args.cols.unwrap_or(3), None) {
            Ok(img) => {
                let mut out = vec![img];
                // An active moodboard is binding: every look at the picture carries it.
                let active = moodboard::active(&self.shared.dir());
                if !active.is_empty() {
                    out.push(ContentBlock::text(format!(
                        "moodboard active: {} (binding: match its palette, contrast and light; `moodboard` shows it)",
                        active.join(", ")
                    )));
                }
                CallToolResult::success(out)
            }
            Err(e) => fail(format!("render failed: {e:#}")),
        }
    }

    #[tool(
        name = "ref",
        description = "Look at something the human pointed at (`@R<n>` object/region, `@N<n>` review note): what and when, the layers there, and the frames with it marked in magenta."
    )]
    fn look(&self, Parameters(RefArgs { id }): Parameters<RefArgs>) -> CallToolResult {
        let Some(r) = self.shared.refs.lock().get(&id).cloned() else {
            return fail(format!("no ref {id}"));
        };
        let (fps, mut info) = {
            let s = self.shared.session.lock();
            let doc = s.doc();
            let mut info = r.describe(doc.scene.fps);
            let rect = match &r.target {
                refs::Target::Region { rect } => Some(*rect),
                _ => None,
            };
            if let refs::Target::Object { name } = &r.target {
                info += &format!("\n{}", s.query(name).unwrap_or_else(|e| format!("({e:#})")));
            }
            if let refs::Target::Note { reply, .. } = &r.target
                && !reply.is_empty()
            {
                info += &format!("\nreply: {reply}");
            }
            if let Some(rect) = rect {
                let layers = refs::layers_in(doc, r.comp.as_deref(), r.frames[0], rect);
                info += &format!("\nlayers there at frame {}: {}", r.frames[0], if layers.is_empty() { "none".into() } else { layers.join("; ") });
            }
            (doc.scene.fps, info)
        };
        let [a, b] = r.frames;
        let frames = if a == b { vec![a] } else { vec![a, ((a + b) / 2.0).round(), b] };
        if frames.len() > 1 {
            info += &format!("\nimage: frames {frames:?} at {fps}fps");
        }
        match self.sheet(r.comp.as_deref(), &frames, if frames.len() == 1 { 768 } else { 512 }, 3, Some(&r)) {
            Ok(img) => CallToolResult::success(vec![ContentBlock::text(info), img]),
            Err(e) => fail(format!("render failed: {e:#}")),
        }
    }

    #[tool(
        description = "Review notes on frames, from the human and the director: open ones by default. Post one with `add` (director), resolve one with a one-line `reply` once addressed."
    )]
    fn notes(&self, Parameters(args): Parameters<NotesArgs>) -> CallToolResult {
        // Lock order: `refs` may be taken while holding `session`, never the reverse.
        let (fps, now) = {
            let s = self.shared.session.lock();
            (s.doc().scene.fps, s.frame.round())
        };
        let mut refs = self.shared.refs.lock();
        let mut head = String::new();
        if let Some(text) = args.add {
            let frames = match args.frames[..] {
                [] => [now, now],
                [a] => [a, a],
                [a, b, ..] => [a, b],
            };
            let strokes = args.rect.map(|[x, y, w, h]| vec![vec![[x, y], [x + w, y], [x + w, y + h], [x, y + h], [x, y]]]).unwrap_or_default();
            let note = refs::Target::Note { text, strokes, done: false, reply: String::new(), by: refs::Author::Director };
            let id = refs.add(args.comp.clone(), frames, note);
            head = format!("posted @{id}\n");
            (self.shared.notify)();
        }
        if let Some(id) = &args.resolve {
            if let Err(e) = refs.resolve(id, args.reply.as_deref().unwrap_or("")) {
                return fail(format!("{e:#}"));
            }
            (self.shared.notify)();
        }
        let lines: Vec<String> = refs
            .all()
            .iter()
            .filter(|r| matches!(r.target, refs::Target::Note { .. }) && (args.all || r.is_open_note()))
            .map(|r| r.describe(fps))
            .collect();
        text(format!("{head}{}", if lines.is_empty() { "(no open notes)".into() } else { lines.join("\n") }))
    }

    #[tool(
        description = "Grow the editor live: list | help (contract) | show | write (create+check a shader fx, shader layer, or tool) | run (a tool; its ops apply). No rebuild ever."
    )]
    fn plugin(&self, Parameters(a): Parameters<PluginArgs>) -> CallToolResult {
        let dir = &self.shared.dir();
        let named = || a.name.clone().ok_or_else(|| "name is required".to_string());
        let result: Result<String, String> = match a.action.as_str() {
            "" | "list" => {
                let renderer = self.shared.renderer.lock();
                let lines: Vec<String> = plugins::list(dir)
                    .iter()
                    .map(|p| {
                        let kind = serde_json::to_value(&p.manifest.kind).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default();
                        let params = if p.manifest.params.is_empty() { String::new() } else { format!(" · params: {}", p.manifest.params.join("; ")) };
                        let err = renderer.plugin_error(&p.name).map(|e| format!(" · BROKEN: {}", e.lines().next().unwrap_or(""))).unwrap_or_default();
                        let shared = if p.shared { " (library)" } else { "" };
                        format!("{} [{kind}]{shared} {}{params}{err}", p.name, p.manifest.about)
                    })
                    .collect();
                Ok(if lines.is_empty() { "(no plugins yet; `plugin action=help` shows how to write one)".into() } else { lines.join("\n") })
            }
            "help" => Ok(plugins::HELP.into()),
            "show" => named().and_then(|n| plugins::show(dir, &n).map_err(|e| format!("{e:#}"))),
            "write" => named().and_then(|n| {
                let kind = match a.kind.as_deref() {
                    Some("fx") => plugins::PluginKind::Fx,
                    Some("layer") => plugins::PluginKind::Layer,
                    Some("tool") => plugins::PluginKind::Tool,
                    other => return Err(format!("kind must be fx, layer or tool, got {other:?}")),
                };
                let manifest = plugins::Manifest { kind, about: a.about.clone().unwrap_or_default(), params: a.params.clone(), run: a.run.clone() };
                let library = a.scope.as_deref() == Some("library");
                plugins::write(dir, &n, &manifest, &a.files, library)
                    .map(|p| {
                        (self.shared.notify)();
                        let checked = if kind == plugins::PluginKind::Tool { "" } else { " (compiles)" };
                        format!("wrote {}{checked}; use it now", p.display())
                    })
                    .map_err(|e| format!("{e:#}"))
            }),
            "run" => named().and_then(|n| {
                let input = {
                    let s = self.shared.session.lock();
                    let doc = s.doc();
                    serde_json::json!({
                        "args": if a.args.is_null() { serde_json::json!({}) } else { a.args.clone() },
                        "project": dir,
                        "frame": s.frame,
                        "comp": s.comp,
                        "selected": s.selected,
                        "res": doc.comp_canvas(s.comp.as_deref()).0,
                        "fps": doc.scene.fps,
                    })
                };
                // Progress lines show live, and the pool picks up each file as it lands.
                let (ui, notify) = (&self.shared.ui, &self.shared.notify);
                let mut on_progress = |p: &str| {
                    let mut u = ui.lock();
                    u.progress = Some(format!("{n}: {p}"));
                    u.library_changed = true;
                    drop(u);
                    notify();
                };
                let out = plugins::run(dir, &n, &input, std::time::Duration::from_secs(a.timeout.unwrap_or(600)), &mut on_progress).map_err(|e| format!("{e:#}"))?;
                self.shared.ui.lock().library_changed = true;
                let mut msg = out.text;
                if !out.ops.trim().is_empty() {
                    let applied = self.shared.session.lock().run(&out.ops, Source::Agent);
                    match applied {
                        Ok(k) => msg = format!("{msg}\napplied {k} ops").trim().to_string(),
                        Err(e) => return Err(format!("{msg}\nits ops failed: {e:#}")),
                    }
                }
                (self.shared.notify)();
                Ok(if msg.is_empty() { "done".into() } else { msg })
            }),
            other => Err(format!("unknown action `{other}` (list, help, show, write, run)")),
        };
        match result {
            Ok(s) => text(s),
            Err(e) => fail(e),
        }
    }

    #[tool(
        description = "The storyboard (`@P<n>` panels the human sketched or you drafted; they play in order, each `seconds` long). No args: every panel as one image + a line each. panel=P2: that panel large. add=[...]: draft panels (caption, seconds, image, comp, strokes). panel+set: change one. del: remove one."
    )]
    fn storyboard(&self, Parameters(a): Parameters<StoryboardArgs>) -> CallToolResult {
        let (fps, start, res) = {
            let s = self.shared.session.lock();
            (s.doc().scene.fps, s.doc().scene.start as f32, s.doc().scene.res)
        };
        let mut board = self.shared.storyboard.lock();
        let mut changed = Vec::new();
        let blue = |lines: Vec<Vec<[f32; 2]>>| -> Vec<storyboard::Stroke> {
            lines.into_iter().map(|points| storyboard::Stroke { color: storyboard::INKS[2], width: 0.006, points }).collect()
        };
        for p in a.add {
            let id = board.add(p.at.map(|i| i.saturating_sub(1)), p.caption.unwrap_or_default(), p.seconds.unwrap_or(2.0));
            let panel = board.get_mut(&id).unwrap();
            panel.image = p.image;
            panel.comp = p.comp;
            panel.strokes = p.strokes.map(blue).unwrap_or_default();
            changed.push(id);
        }
        if let (Some(id), Some(set)) = (&a.panel, a.set) {
            let Some(panel) = board.get_mut(id) else { return fail(format!("no panel {id}")) };
            if let Some(c) = set.caption {
                panel.caption = c;
            }
            if let Some(s) = set.seconds {
                panel.seconds = s.max(0.1);
            }
            if let Some(i) = set.image {
                panel.image = Some(i).filter(|i| !i.is_empty());
            }
            if let Some(c) = set.comp {
                panel.comp = Some(c).filter(|c| !c.is_empty());
            }
            if let Some(lines) = set.strokes {
                panel.strokes.retain(|s| s.color != storyboard::INKS[2]);
                panel.strokes.extend(blue(lines));
            }
            changed.push(panel.id.clone());
        }
        if let Some(id) = &a.del {
            let Some(i) = board.index(id) else { return fail(format!("no panel {id}")) };
            board.panels.remove(i);
            changed.push(format!("deleted {id}"));
        }
        if !changed.is_empty() {
            board.save();
            (self.shared.notify)();
        }
        if board.panels.is_empty() {
            return text("(the storyboard is empty; draft panels with add=[{caption, seconds, image, strokes}])");
        }
        // Look: one panel large, or the whole board as one sheet.
        let shown: Vec<usize> = match a.panel.as_deref().and_then(|id| board.index(id)) {
            Some(i) => vec![i],
            None => (0..board.panels.len()).collect(),
        };
        let w = if shown.len() == 1 { 768 } else { 320 };
        let size = [w, ((w as f32) * res[1] as f32 / res[0] as f32).round().max(1.0) as u32];
        let frames: Vec<Vec<u8>> = shown.iter().map(|&i| storyboard::rasterize(&board.panels[i], &self.shared.dir(), size)).collect();
        let mut lines: Vec<String> = shown.iter().map(|&i| board.describe(i, start, fps)).collect();
        if !changed.is_empty() {
            lines.insert(0, format!("changed: {}", changed.join(", ")));
        }
        let (sheet, sheet_size) = ed_render::contact_sheet(&frames, size, 4);
        match ed_render::encode_png(&sheet, sheet_size) {
            Ok(png) => CallToolResult::success(vec![
                ContentBlock::text(lines.join("\n")),
                ContentBlock::image(base64::engine::general_purpose::STANDARD.encode(png), "image/png"),
            ]),
            Err(e) => fail(format!("encode failed: {e:#}")),
        }
    }

    #[tool(
        description = "Moodboards: folders of reference images the human collects (shared across projects); the project's active ones set the look. No args: active folders as one image + per folder a 5-colour palette and luma stats. folder=x: just that one. add=url|path (with folder): collect an image. activate/deactivate=folder."
    )]
    fn moodboard(&self, Parameters(a): Parameters<MoodboardArgs>) -> CallToolResult {
        let (root, project) = (moodboard::root(), self.shared.dir());
        let mut head = Vec::new();
        if let Some(what) = &a.add {
            let Some(folder) = &a.folder else { return fail("add needs folder=<name>") };
            match moodboard::add(&root, folder, what) {
                Ok(p) => head.push(format!("added {}", p.file_name().unwrap_or_default().to_string_lossy())),
                Err(e) => return fail(format!("{e:#}")),
            }
        }
        for (name, on) in [(&a.activate, true), (&a.deactivate, false)] {
            let Some(name) = name else { continue };
            if on && !root.join(name).is_dir() {
                return fail(format!("no moodboard `{name}` (have: {})", moodboard::folders(&root).join(", ")));
            }
            if let Err(e) = moodboard::set_active(&project, name, on) {
                return fail(format!("{e:#}"));
            }
            head.push(format!("{} {name}", if on { "activated" } else { "deactivated" }));
        }
        if !head.is_empty() {
            (self.shared.notify)();
        }
        let shown = match &a.folder {
            Some(f) => vec![f.clone()],
            None => moodboard::active(&project).into_iter().filter(|f| root.join(f).is_dir()).collect(),
        };
        if shown.is_empty() {
            let all: Vec<String> = moodboard::folders(&root).iter().map(|f| format!("{f} ({})", moodboard::images(&root, f).len())).collect();
            head.push(format!(
                "no active moodboards; folders: {}",
                if all.is_empty() { "none yet (add=url folder=name starts one)".into() } else { all.join(", ") }
            ));
            return text(head.join("\n"));
        }
        let per = if shown.len() == 1 { 24 } else { (32 / shown.len()).max(4) };
        let mut cells = Vec::new();
        for f in &shown {
            let read = moodboard::read(&root, f, per);
            let first = cells.len() + 1;
            cells.extend(read.cells);
            let at = if cells.len() >= first { format!(" [sheet {first}-{}]", cells.len()) } else { String::new() };
            head.push(format!("{}{at}", read.line));
        }
        if cells.is_empty() {
            return text(head.join("\n"));
        }
        let (sheet, size) = ed_render::contact_sheet(&cells, moodboard::CELL, 6);
        match ed_render::encode_png(&sheet, size) {
            Ok(png) => CallToolResult::success(vec![
                ContentBlock::text(head.join("\n")),
                ContentBlock::image(base64::engine::general_purpose::STANDARD.encode(png), "image/png"),
            ]),
            Err(e) => fail(format!("encode failed: {e:#}")),
        }
    }

    #[tool(description = "Ask for a change to the editor itself, only when a plugin cannot do it. Files a task for its developers; keep working around it.")]
    fn request(&self, Parameters(RequestArgs { title, why }): Parameters<RequestArgs>) -> CallToolResult {
        let repo = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
        let out = std::process::Command::new("tt")
            .args(["new", &format!("{title} (agent request: {why})"), "-t", "request"])
            .env("TT_DIR", repo.join("tasks"))
            .output();
        match out {
            Ok(o) if o.status.success() => text(format!("filed {}", String::from_utf8_lossy(&o.stdout).trim())),
            Ok(o) => fail(format!("tt failed: {}", String::from_utf8_lossy(&o.stderr).trim())),
            Err(e) => fail(format!("could not run tt: {e}")),
        }
    }

    #[tool(description = "Render scene.start..end to a video file with ffmpeg. Default: the project's exports/<project>-NNN.mp4, never overwriting.")]
    fn export(&self, Parameters(args): Parameters<ExportArgs>) -> CallToolResult {
        let codec = args.codec.unwrap_or_else(|| "h264".into());
        let dir = self.shared.dir();
        let path = match args.path.as_deref() {
            Some(p) => dir.join(p),
            None => next_export_path(&dir, &codec),
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match export_video(&self.shared, &path, &codec, args.width) {
            Ok(s) => text(s),
            Err(e) => fail(format!("{e:#}")),
        }
    }

    #[tool(description = "Search music: open catalogs (Openverse/Jamendo, ccMixter, archive.org) and local folders. One line per track: id | title — artist | length | tags | source.")]
    fn music(&self, Parameters(args): Parameters<MusicArgs>) -> CallToolResult {
        let mut roots = vec![self.shared.dir().join("audio")];
        if let Some(home) = std::env::var_os("HOME") {
            roots.push(std::path::PathBuf::from(home).join("Music"));
        }
        let (tracks, errors) = ed_audio::library::search(&args.q, args.min_seconds, args.max_seconds, &roots);
        let mut out: Vec<String> = tracks.iter().take(30).map(|t| t.line()).collect();
        if out.is_empty() {
            out.push("(no matches; try broader mood/genre words)".into());
        }
        out.extend(errors.into_iter().map(|e| format!("// source failed: {e}")));
        text(out.join("\n"))
    }

    #[tool(description = "Get a track: downloads a `music` result into the project (or takes a project audio file) and analyzes it: tempo, beat grid, energy per bar, loud sections.")]
    fn track(&self, Parameters(TrackArgs { id }): Parameters<TrackArgs>) -> CallToolResult {
        let src = if id.contains(':') {
            match ed_audio::library::fetch(&id, &self.shared.dir()) {
                Ok(s) => s,
                Err(e) => return fail(format!("fetch failed: {e:#}")),
            }
        } else {
            id
        };
        match ed_audio::analyze(&self.shared.dir().join(&src)) {
            Ok(a) => text(a.summary(&src)),
            Err(e) => fail(format!("{src}: analysis failed: {e:#}")),
        }
    }

    #[tool(description = "The project's media library: one line per image/video/audio file (src, size, length). Use src in `add image|video|audio <name> src=...`.")]
    fn library(&self) -> CallToolResult {
        let items = ed_media::list(&self.shared.dir());
        if items.is_empty() {
            return text("(library empty; use fetch, images, music+track)");
        }
        text(items.iter().map(|i| ed_media::describe(&self.shared.dir(), i)).collect::<Vec<_>>().join("\n"))
    }

    #[tool(description = "Bring a file into the library: any URL, an `ovi:` result from `images`, or a local path.")]
    fn fetch(&self, Parameters(FetchArgs { what }): Parameters<FetchArgs>) -> CallToolResult {
        let dir = &self.shared.dir();
        let src = if what.contains("://") || what.starts_with("ovi:") {
            ed_media::web::fetch(dir, &what)
        } else {
            ed_media::import(dir, std::path::Path::new(&what))
        };
        self.shared.ui.lock().library_changed = true;
        (self.shared.notify)();
        match src {
            Ok(src) => {
                let kind = ed_media::kind_of(std::path::Path::new(&src)).unwrap_or(ed_media::MediaKind::Image);
                text(ed_media::describe(dir, &ed_media::Item { src, kind }))
            }
            Err(e) => fail(format!("{e:#}")),
        }
    }

    #[tool(description = "Search openly licensed images (Openverse): id | title | size | source. Get one with fetch.")]
    fn images(&self, Parameters(QArgs { q }): Parameters<QArgs>) -> CallToolResult {
        match ed_media::web::image_search(&q) {
            Ok(lines) if lines.is_empty() => text("(no matches)"),
            Ok(lines) => text(lines.join("\n")),
            Err(e) => fail(format!("{e:#}")),
        }
    }

    #[tool(
        description = "Poly Haven CC0 assets: type=texture (default) | hdri | model; search with q, download with get=<id>. Texture: `set obj tex=<id> uvscale=<repeats>`; hdri: `set scene.env <path>`; model: `add model <name> src=<path>` (its own maps)."
    )]
    fn textures(&self, Parameters(args): Parameters<TexturesArgs>) -> CallToolResult {
        let kind = args.kind.as_deref().unwrap_or("texture");
        let Some(kind) = ed_media::web::PolyHaven::parse(kind) else {
            return fail(format!("unknown type `{kind}`: texture, hdri or model"));
        };
        if let Some(id) = args.get {
            let res = args.res.unwrap_or_else(|| "1k".into());
            return match ed_media::web::polyhaven_fetch(&self.shared.dir(), kind, &id, &res) {
                Ok(src) => {
                    self.shared.renderer.lock().reload_media();
                    text(match kind {
                        ed_media::web::PolyHaven::Texture => format!("ready: set <obj> tex={src} uvscale=<repeats> (color/normal/rough maps)"),
                        ed_media::web::PolyHaven::Hdri => format!("ready: set scene.env {src}"),
                        ed_media::web::PolyHaven::Model => format!("ready: add model <name> src={src} (real-world meters, own color/normal/rough maps)"),
                    })
                }
                Err(e) => fail(format!("{e:#}")),
            };
        }
        match ed_media::web::polyhaven_search(kind, args.q.as_deref().unwrap_or("")) {
            Ok(lines) if lines.is_empty() => text("(no matches)"),
            Ok(lines) => text(lines.join("\n")),
            Err(e) => fail(format!("{e:#}")),
        }
    }

    #[tool(description = "Drive the human's view: seek the playhead, select an object, play/stop, switch canvas/3d, or say a short note to them.")]
    fn ui(&self, Parameters(args): Parameters<UiArgs>) -> CallToolResult {
        {
            let mut s = self.shared.session.lock();
            if let Some(f) = args.seek {
                s.frame = f;
            }
            if let Some(n) = args.select {
                s.selected = Some(n).filter(|n| !n.is_empty() && n != "none");
            }
        }
        {
            let mut ui = self.shared.ui.lock();
            ui.play = args.play.or(ui.play);
            ui.mode = args.mode.or(ui.mode.take());
            ui.say = args.say.or(ui.say.take());
        }
        (self.shared.notify)();
        text("ok")
    }

    #[tool(description = "Undo your (or the human's) last steps.")]
    fn undo(&self, Parameters(UndoArgs { steps }): Parameters<UndoArgs>) -> CallToolResult {
        let mut s = self.shared.session.lock();
        let n = (0..steps.unwrap_or(1)).take_while(|_| s.undo(Source::Agent)).count();
        drop(s);
        (self.shared.notify)();
        text(format!("undid {n}"))
    }

    #[tool(description = "Operator log: what the human changed since you last looked.")]
    fn log(&self, Parameters(LogArgs { since, all }): Parameters<LogArgs>) -> CallToolResult {
        let s = self.shared.session.lock();
        let mut seen = self.shared.agent_seen.lock();
        let out = s.log_since(since.unwrap_or(*seen), (!all).then_some(Source::Human));
        *seen = s.log.last().map_or(0, |l| l.seq);
        text(if out.is_empty() { "(nothing new)".into() } else { out })
    }
}

#[tool_handler]
impl ServerHandler for Editor {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(INSTRUCTIONS)
    }
}

pub fn url(port: u16) -> String {
    format!("http://127.0.0.1:{port}/mcp")
}

/// Names of the agent's tools, as the server lists them.
pub fn tool_names() -> Vec<String> {
    Editor::tool_router().list_all().into_iter().map(|t| t.name.into_owned()).collect()
}

/// Serves the editor's MCP endpoint on a background thread.
pub fn spawn(shared: Arc<Shared>, port: u16) -> std::thread::JoinHandle<anyhow::Result<()>> {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
        rt.block_on(async move {
            let service: StreamableHttpService<Editor, LocalSessionManager> = StreamableHttpService::new(
                move || Ok(Editor::new(shared.clone())),
                Default::default(),
                Default::default(),
            );
            let app = axum::Router::new().nest_service("/mcp", service);
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
            axum::serve(listener, app).await?;
            Ok(())
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Switching projects swaps everything that belongs to one: document,
    /// refs, storyboard and the folder the tools read and write.
    #[test]
    fn open_switches_the_whole_project() {
        let Some(renderer) = Renderer::headless() else { return eprintln!("no GPU adapter; skipped") };
        let root = std::env::temp_dir().join(format!("ed_switch_{}", std::process::id()));
        let (a, b) = (root.join("a"), root.join("b"));
        for d in [&a, &b] {
            std::fs::create_dir_all(d).unwrap();
        }
        let shared = Shared::new(a.clone(), Session::new(), renderer, || {});
        shared.session.lock().run("add rect only_in_a", Source::Agent).unwrap();
        shared.refs.lock().add(None, [0.0, 0.0], refs::Target::Region { rect: [0.0, 0.0, 1.0, 1.0] });
        shared.storyboard.lock().add(None, "a's plan".into(), 1.0);

        let mut s = Session::new();
        s.run("add ellipse only_in_b", Source::Human).unwrap();
        shared.open(b.clone(), s);
        assert_eq!(shared.dir(), b);
        assert!(shared.session.lock().doc().objects.contains_key("only_in_b"));
        assert!(shared.refs.lock().all().is_empty(), "b has no refs");
        assert!(shared.storyboard.lock().panels.is_empty(), "b has no storyboard");

        shared.open(a.clone(), Session::new());
        assert_eq!(shared.refs.lock().all().len(), 1, "a's refs come back from disk");
        assert_eq!(shared.storyboard.lock().panels[0].caption, "a's plan");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
