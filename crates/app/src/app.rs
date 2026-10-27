//! App shell and panels. Every edit made here is an op-language line run
//! through the session, exactly like the agent's edits.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use eframe::egui::{self, Color32, RichText};
use ed_core::Source;
use ed_core::data::Kind;
use ed_core::paths::{self, Value, fmt_num};
use ed_mcp::Shared;
use egui_dock::{DockArea, DockState, NodeIndex};
use parking_lot::Mutex;

use crate::pool::Pool;
use crate::terminal::Terminal;
use crate::timeline::Timeline;
use crate::viewport::{Action, Viewport};

/// Panels docked on the Edit page.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Tab {
    Viewport,
    Layers,
    Properties,
    Timeline,
    Pool,
    Source,
    Log,
}

/// Whole-window pages, picked from the bar at the bottom (as an NLE's pages).
/// The agent stays on the right on every page.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Page {
    Edit,
    Storyboard,
    Moodboard,
    References,
    Extensions,
}

impl Page {
    const ALL: [Page; 5] = [Page::Edit, Page::Storyboard, Page::Moodboard, Page::References, Page::Extensions];

    fn label(self) -> &'static str {
        match self {
            Page::Edit => "Edit",
            Page::Storyboard => "Storyboard",
            Page::Moodboard => "Moodboard",
            Page::References => "References",
            Page::Extensions => "Plugins & Skills",
        }
    }
}

pub struct App {
    dock: DockState<Tab>,
    panels: Panels,
}

pub struct Panels {
    pub shared: Arc<Shared>,
    /// The open project (None: only the Project Manager is usable).
    project: Option<PathBuf>,
    /// MCP port, written into each project's agent config.
    port: u16,
    manager: crate::projects::Manager,
    rs: eframe::egui_wgpu::RenderState,
    viewport: Viewport,
    timeline: Timeline,
    pool: Pool,
    board: crate::storyboard::Board,
    refs_page: crate::refs_page::RefsPage,
    moodboard: crate::moodboard::Moodboard,
    extensions: crate::extensions::ExtensionsPage,
    pub page: Page,
    /// Native menu bar commands (`crate::menu`).
    menu: std::sync::mpsc::Receiver<String>,
    /// Session version and open comp the player's clips were built for.
    audio_synced: Option<(u64, Option<String>)>,
    terminal: Option<Terminal>,
    terminal_error: Option<String>,
    pub playing: bool,
    last_tick: Instant,
    console: String,
    /// Groups one continuous edit (a drag, typing into a field) into one undo step.
    edit_counter: u64,
    last_edit: String,
    saved_version: u64,
    last_save: Instant,
    export_status: Arc<Mutex<Option<String>>>,
    pub player: ed_audio::player::Player,
    was_playing: bool,
    /// Waveform peaks per audio file (`PEAKS_PER_SEC` buckets per second).
    pub peaks: std::collections::HashMap<String, Arc<Vec<f32>>>,
    /// Refs pointed at recently: outlined in the viewer while in range.
    pub pointed: Vec<String>,
    /// Marked in/out frames (I/O keys); refs made while marked span them.
    pub marks: [Option<f32>; 2],
    /// Latest note from the agent or the app, with when it was shown.
    note: Option<(String, Instant)>,
    /// tmux-style keys (Ctrl+A prefix) and their state.
    prefix: crate::tmux::Prefix,
    /// The focused pane fills the window (Ctrl+A z).
    zoomed: bool,
    /// Ctrl+A ? help is open.
    show_keys: bool,
    /// Where the agent pane and the page content were drawn last frame.
    agent_rect: egui::Rect,
    content_rect: egui::Rect,
}

pub const PEAKS_PER_SEC: u32 = 100;

impl App {
    /// `project`: open it at once; None shows the Project Manager first.
    pub fn new(cc: &eframe::CreationContext, shared: Arc<Shared>, project: Option<PathBuf>, port: u16) -> Self {
        // Edit page: viewers on top, timeline below, pool and inspectors on the right.
        let mut dock = DockState::new(vec![Tab::Viewport]);
        let surface = dock.main_surface_mut();
        let [main, _] = surface.split_right(NodeIndex::root(), 0.74, vec![Tab::Pool, Tab::Properties, Tab::Layers, Tab::Log]);
        let [viewers, _] = surface.split_below(main, 0.58, vec![Tab::Timeline]);
        let [_, _] = surface.split_left(viewers, 0.38, vec![Tab::Source]);

        let ctx = cc.egui_ctx.clone();
        crate::theme::apply(&ctx);
        let mut panels = Panels {
            shared,
            project: None,
            port,
            manager: Default::default(),
            rs: cc.wgpu_render_state.clone().expect("wgpu"),
            viewport: Viewport::new(),
            timeline: Timeline::default(),
            pool: Pool::default(),
            board: Default::default(),
            refs_page: Default::default(),
            moodboard: Default::default(),
            extensions: Default::default(),
            page: Page::Edit,
            menu: crate::menu::install(&ctx),
            audio_synced: None,
            terminal: None,
            terminal_error: None,
            playing: false,
            last_tick: Instant::now(),
            console: String::new(),
            edit_counter: 0,
            last_edit: String::new(),
            saved_version: 0,
            last_save: Instant::now(),
            export_status: Arc::new(Mutex::new(None)),
            player: ed_audio::player::Player::new(),
            was_playing: false,
            peaks: Default::default(),
            pointed: Vec::new(),
            marks: [None, None],
            note: None,
            prefix: Default::default(),
            zoomed: false,
            show_keys: false,
            agent_rect: egui::Rect::NOTHING,
            content_rect: egui::Rect::NOTHING,
        };
        match project {
            Some(dir) => panels.open_project(&ctx, dir),
            None => panels.manager.show(),
        }
        Self { dock, panels }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        while let Ok(id) = self.panels.menu.try_recv() {
            self.panels.menu_command(&ctx, &id);
        }
        if let Some(crate::projects::Choice::Open(dir)) = self.panels.manager.ui(&ctx, self.panels.project.as_deref()) {
            self.panels.open_project(&ctx, dir);
        }
        if self.panels.project.is_none() {
            return;
        }
        if let Some(cmd) = self.panels.prefix.input(&ctx) {
            self.tmux(&ctx, cmd);
        }
        // The Moodboard page takes dropped files into its folder itself.
        let dropped = crate::pool::dropped(&ctx);
        if !dropped.is_empty() && self.panels.page != Page::Moodboard {
            self.panels.import_files(&dropped);
        }
        self.panels.global_input(&ctx);
        self.panels.tick(&ctx);

        let agent_focused = ctx.memory(|m| m.has_focus(crate::terminal::TERMINAL_ID()));
        egui::Panel::bottom("status").exact_size(26.0).frame(egui::Frame::NONE.fill(crate::theme::BAR)).show(ui, |ui| self.panels.status_bar(ui));
        let zoom = self.panels.zoomed;
        if zoom && agent_focused {
            // The agent fills the window.
            let r = egui::CentralPanel::default().show(ui, |ui| self.panels.agent(ui)).response.rect;
            self.panels.agent_rect = r;
        } else {
            if !zoom {
                let r = egui::Panel::right("agent").resizable(true).default_size(560.0).size_range(320.0..=1200.0).show(ui, |ui| self.panels.agent(ui)).response.rect;
                self.panels.agent_rect = r;
            }
            let r = egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| self.page(ui, zoom)).response.rect;
            self.panels.content_rect = r;
        }
        // The focused pane, outlined as tmux marks the active pane.
        let focused = if agent_focused {
            Some(self.panels.agent_rect)
        } else if self.panels.page == Page::Edit && !zoom {
            self.dock.focused_leaf().and_then(|p| self.dock.iter_leaves().find(|(q, _)| *q == p).map(|(_, l)| l.rect))
        } else {
            None
        };
        if let Some(r) = focused {
            ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("pane-focus")))
                .rect_stroke(r.shrink(1.0), 4.0, egui::Stroke::new(1.5, crate::theme::ACCENT), egui::StrokeKind::Inside);
        }
        if self.panels.show_keys {
            let mut open = true;
            egui::Window::new("Keys  ·  Ctrl+A, then…").open(&mut open).collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).show(&ctx, |ui| {
                egui::Grid::new("keys").num_columns(2).spacing([24.0, 6.0]).show(ui, |ui| {
                    for (k, what) in crate::tmux::HELP {
                        ui.label(RichText::new(*k).monospace().color(crate::theme::ACCENT));
                        ui.label(*what);
                        ui.end_row();
                    }
                });
            });
            self.panels.show_keys = open && !ctx.input(|i| i.key_pressed(egui::Key::Escape));
        }
    }

    fn on_exit(&mut self) {
        self.panels.close_project();
    }
}

impl App {
    /// The open page's content; zoomed on the Edit page shows only the focused pane.
    fn page(&mut self, ui: &mut egui::Ui, zoom: bool) {
        let p = &mut self.panels;
        match p.page {
            Page::Edit if zoom => {
                let tab = self.dock.focused_leaf().and_then(|f| self.dock.iter_leaves().find(|(q, _)| *q == f).map(|(_, l)| l.tabs[l.active.0]));
                let mut tab = tab.unwrap_or(Tab::Viewport);
                egui::Frame::NONE.inner_margin(6.0).show(ui, |ui| egui_dock::TabViewer::ui(p, ui, &mut tab));
            }
            Page::Edit => {
                DockArea::new(&mut self.dock).style(crate::theme::dock_style(ui.style())).show_inside(ui, p);
            }
            Page::Storyboard => {
                let mut board = std::mem::take(&mut p.board);
                egui::Frame::NONE.inner_margin(12.0).show(ui, |ui| board.ui(ui, p));
                p.board = board;
            }
            Page::Moodboard => {
                let mut page = std::mem::take(&mut p.moodboard);
                egui::Frame::NONE.inner_margin(12.0).show(ui, |ui| page.ui(ui, p));
                p.moodboard = page;
            }
            Page::References => {
                let mut page = std::mem::take(&mut p.refs_page);
                egui::Frame::NONE.inner_margin(12.0).show(ui, |ui| page.ui(ui, p));
                p.refs_page = page;
            }
            Page::Extensions => {
                let mut page = std::mem::take(&mut p.extensions);
                egui::Frame::NONE.inner_margin(12.0).show(ui, |ui| page.ui(ui, p));
                p.extensions = page;
            }
        }
    }

    /// One Ctrl+A command.
    fn tmux(&mut self, ctx: &egui::Context, cmd: crate::tmux::Cmd) {
        use crate::tmux::Cmd;
        let term = crate::terminal::TERMINAL_ID();
        let focus_agent = |ctx: &egui::Context| ctx.memory_mut(|m| m.request_focus(term));
        let p = &mut self.panels;
        let n = Page::ALL.len();
        let at = Page::ALL.iter().position(|x| *x == p.page).unwrap_or(0);
        match cmd {
            Cmd::Page(i) => p.page = Page::ALL[i.min(n - 1)],
            Cmd::NextPage => p.page = Page::ALL[(at + 1) % n],
            Cmd::PrevPage => p.page = Page::ALL[(at + n - 1) % n],
            Cmd::Agent => focus_agent(ctx),
            Cmd::Zoom => p.zoomed = !p.zoomed,
            Cmd::Save => {
                p.save();
                p.notify("saved");
            }
            Cmd::Manager => p.manager.show(),
            Cmd::Help => p.show_keys = !p.show_keys,
            Cmd::Literal => {
                if let Some(t) = &mut p.terminal {
                    t.send_raw(&[0x01]);
                }
            }
            Cmd::Focus(_) | Cmd::Cycle => {
                // Panes: the Edit page's docked leaves (or the page as one pane), then the agent.
                const AGENT: usize = usize::MAX;
                let leaves: Vec<_> = if p.page == Page::Edit && !p.zoomed {
                    self.dock.iter_leaves().map(|(path, l)| (path, l.rect)).collect()
                } else {
                    Vec::new()
                };
                let mut panes: Vec<(usize, egui::Rect)> = leaves.iter().enumerate().map(|(i, (_, r))| (i, *r)).collect();
                if leaves.is_empty() {
                    panes.push((0, p.content_rect));
                }
                if !p.zoomed {
                    panes.push((AGENT, p.agent_rect));
                }
                let agent_has = ctx.memory(|m| m.has_focus(term));
                let current = if agent_has {
                    AGENT
                } else {
                    let f = self.dock.focused_leaf();
                    leaves.iter().position(|(path, _)| Some(*path) == f).unwrap_or(0)
                };
                let from = panes.iter().find(|(i, _)| *i == current).map_or(p.content_rect, |(_, r)| *r);
                let next = match cmd {
                    Cmd::Focus(dir) => crate::tmux::neighbour(from, dir, &panes),
                    _ => {
                        let k = panes.iter().position(|(i, _)| *i == current).unwrap_or(0);
                        panes.get((k + 1) % panes.len()).map(|(i, _)| *i)
                    }
                };
                match next {
                    Some(AGENT) => focus_agent(ctx),
                    Some(i) => {
                        ctx.memory_mut(|m| m.surrender_focus(term));
                        if let Some((path, _)) = leaves.get(i) {
                            self.dock.set_focused_node_and_surface(*path);
                        }
                    }
                    None => {}
                }
            }
        }
    }
}

impl Panels {
    pub fn dir(&self) -> PathBuf {
        self.shared.dir()
    }

    /// Saves the open project and leaves a thumbnail for the Project Manager.
    fn close_project(&mut self) {
        let Some(dir) = self.project.clone() else { return };
        self.save();
        let scene = {
            let s = self.shared.session.lock();
            ed_core::eval::evaluate(s.doc(), s.frame)
        };
        let res = scene.res;
        let size = [320, ((320.0 * res[1] as f32 / res[0].max(1) as f32).round() as u32).max(1)];
        if let Ok(px) = self.shared.renderer.lock().render_rgba(&scene, size)
            && let Ok(png) = ed_render::encode_png(&px, size)
        {
            let path = crate::projects::thumb_path(&dir);
            let _ = std::fs::create_dir_all(path.parent().unwrap());
            let _ = std::fs::write(path, png);
        }
    }

    /// Opens the project in `dir` (switching from the open one): its document,
    /// refs, storyboard and media, and a fresh agent session in that folder.
    pub fn open_project(&mut self, ctx: &egui::Context, dir: PathBuf) {
        let dir = dir.canonicalize().unwrap_or(dir);
        let mut session = ed_core::Session::new();
        let scene = dir.join("scene.ops");
        if scene.exists()
            && let Err(e) = session.load(&scene)
        {
            self.notify(format!("could not open {}: {e:#}", scene.display()));
            self.manager.show();
            return;
        }
        if let Err(e) = crate::agent::setup(&dir, self.port) {
            self.notify(format!("agent setup failed: {e:#}"));
        }
        self.close_project();
        self.terminal = None; // closing the PTY ends the old agent
        self.shared.open(dir.clone(), session);
        crate::projects::touch(&dir);

        // Everything that belonged to the old project starts over.
        self.viewport = Viewport::new();
        self.timeline = Timeline::default();
        self.pool = Pool::default();
        self.board = Default::default();
        self.audio_synced = None;
        self.peaks.clear();
        self.player.stop();
        self.playing = false;
        self.was_playing = false;
        self.pointed.clear();
        self.marks = [None, None];
        self.edit_counter = 0;
        self.last_edit.clear();
        *self.export_status.lock() = None;
        self.saved_version = self.shared.session.lock().version;
        (self.terminal, self.terminal_error) = match Terminal::spawn(&crate::agent::command(&dir), &dir, ctx.clone()) {
            Ok(t) => (Some(t), None),
            Err(e) => (None, Some(format!("{e:#}"))),
        };
        let name = dir.file_name().map_or_else(|| dir.display().to_string(), |n| n.to_string_lossy().into_owned());
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!("{name} — editor")));
        self.notify(format!("opened {name}"));
        self.project = Some(dir);
    }

    pub fn notify(&mut self, msg: impl Into<String>) {
        self.note = Some((msg.into(), Instant::now()));
    }

    /// One line about a ref (`R`/`N`) or storyboard panel (`P`).
    pub fn describe_ref(&self, id: &str) -> String {
        let fps = self.shared.session.lock().doc().scene.fps;
        if let Some(r) = self.shared.refs.lock().get(id) {
            return r.describe(fps);
        }
        let start = self.shared.session.lock().doc().scene.start as f32;
        let board = self.shared.storyboard.lock();
        board.index(id).map(|i| board.describe(i, start, fps)).unwrap_or_default()
    }

    /// Types `text` into the agent's prompt (not sent: the human finishes the
    /// sentence and presses Enter).
    pub fn attach(&mut self, text: &str) {
        match &mut self.terminal {
            Some(t) if !t.exited() => {
                t.insert(&format!("{} ", text.trim()));
                self.notify("added to the agent's prompt");
            }
            _ => self.notify("the agent is not running (Restart it on the right)"),
        }
    }

    /// Attaches a ref or panel by id, described in one line.
    pub fn attach_ref(&mut self, id: &str) {
        let line = self.describe_ref(id);
        if !line.is_empty() {
            self.attach(&line);
        }
    }

    /// Stores what the human pointed at as a ref over the marked frames (else
    /// the current frame) and types it into the agent's prompt.
    pub fn point(&mut self, target: ed_mcp::refs::Target) {
        let (frame, comp) = {
            let s = self.shared.session.lock();
            (s.frame.round(), s.comp.clone())
        };
        let a = self.marks[0].unwrap_or(frame);
        let b = self.marks[1].unwrap_or(a);
        let id = self.shared.refs.lock().add(comp, [a, b], target);
        // Keep how the frame looked right now, for the References page.
        if let Some(r) = self.shared.refs.lock().get(&id).cloned() {
            crate::refs_page::snapshot(&self.shared, &r);
        }
        self.attach_ref(&id);
        self.pointed.push(id);
        if self.pointed.len() > 8 {
            self.pointed.remove(0);
        }
    }

    /// Copies files into the project's media pool.
    pub fn import_files(&mut self, files: &[PathBuf]) {
        let mut ok = 0;
        for f in files {
            match ed_media::import(&self.dir(), f) {
                Ok(_) => ok += 1,
                Err(e) => self.notify(format!("{e:#}")),
            }
        }
        if ok > 0 {
            self.pool.refresh();
            self.notify(format!("imported {ok} file(s) into the media pool"));
        }
    }

    /// Opens a composition in the timeline and viewer (None: back to the film).
    pub fn open_comp(&mut self, comp: Option<String>) {
        let mut s = self.shared.session.lock();
        if s.comp != comp {
            s.comp = comp;
            s.frame = 0.0;
            s.selected = None;
            drop(s);
            self.marks = [None, None];
            self.playing = false;
        }
    }

    /// A name like `base`, `base2`... that is free, starting with a letter.
    fn free_name(&self, stem: &str) -> String {
        let mut base: String = stem.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' }).collect();
        if !base.starts_with(|c: char| c.is_ascii_alphabetic()) {
            base = format!("m_{base}");
        }
        base.truncate(24);
        let s = self.shared.session.lock();
        (1..).map(|i| if i == 1 { base.clone() } else { format!("{base}{i}") }).find(|n| !s.doc().objects.contains_key(n)).unwrap()
    }

    /// Playhead, top canvas layer `z`, and ` comp=<open comp>` for adding into the open timeline.
    fn placement(&self) -> (f32, f32, String) {
        let s = self.shared.session.lock();
        let comp = s.comp.clone();
        let top = s.doc().objects.values().filter(|o| o.comp == comp && o.kind.is_2d()).map(|o| o.z).fold(0.0, f32::max);
        let inside = comp.map(|c| format!(" comp={c}")).unwrap_or_default();
        (s.frame.round(), top, inside)
    }

    /// Puts a media file on the open timeline at the playhead, on top of the stack.
    pub fn add_media(&mut self, item: &ed_media::Item) {
        let stem = item.src.rsplit('/').next().unwrap_or("clip").split('.').next().unwrap_or("clip");
        let name = self.free_name(stem);
        let (frame, top, inside) = self.placement();
        let kind = item.kind.name();
        let src = Value::Str(item.src.clone());
        let script = match item.kind {
            ed_media::MediaKind::Audio => format!("add audio {name} src={src} in={}{inside}", fmt_num(frame)),
            _ => format!("add {kind} {name} src={src} in={} z={}{inside}", fmt_num(frame), fmt_num(top + 1.0)),
        };
        self.run_reporting(&script);
        self.shared.session.lock().selected = Some(name);
    }

    /// Puts a clip of composition `comp` on the open timeline at the playhead, on top.
    pub fn place_comp(&mut self, comp: &str) {
        let name = self.free_name(&format!("{comp}_clip"));
        let (frame, top, inside) = self.placement();
        self.run_reporting(&format!("add clip {name} src={comp} in={} z={}{inside}", fmt_num(frame), fmt_num(top + 1.0)));
        self.shared.session.lock().selected = Some(name);
    }

    /// Runs a script, showing its error (e.g. a comp placed inside itself) as a note.
    fn run_reporting(&mut self, script: &str) {
        if let Err(e) = self.shared.session.lock().run(script, Source::Human) {
            self.note = Some((format!("{e:#}"), Instant::now()));
        }
    }

    pub fn run(&mut self, script: &str) {
        let _ = self.shared.session.lock().run(script, Source::Human);
    }

    /// Runs as part of a continuous edit of `prop` (merged into one undo step).
    pub fn run_edit(&mut self, script: &str, prop: &str, continuing: bool) {
        if !continuing || self.last_edit != prop {
            self.edit_counter += 1;
            self.last_edit = prop.into();
        }
        let key = format!("{prop}#{}", self.edit_counter);
        let _ = self.shared.session.lock().run_merged(script, Source::Human, Some(&key));
    }

    fn global_input(&mut self, ctx: &egui::Context) {
        if ctx.memory(|m| m.focused().is_some()) {
            return; // typing into the terminal or a text field
        }
        let (manager, import, export) = ctx.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::O),
                i.consume_key(egui::Modifiers::COMMAND, egui::Key::I),
                i.consume_key(egui::Modifiers::COMMAND, egui::Key::E),
            )
        });
        if manager {
            self.manager.show();
        }
        if import {
            self.import_dialog();
        }
        if export {
            self.start_export(ctx);
        }
        let (space, undo, redo, save, delete, dup, mark_in, mark_out, unmark, back) = ctx.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::NONE, egui::Key::Space),
                i.consume_key(egui::Modifiers::COMMAND, egui::Key::Z),
                i.consume_key(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::Z),
                i.consume_key(egui::Modifiers::COMMAND, egui::Key::S),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Delete)
                    || i.consume_key(egui::Modifiers::NONE, egui::Key::Backspace),
                i.consume_key(egui::Modifiers::COMMAND, egui::Key::D),
                i.consume_key(egui::Modifiers::NONE, egui::Key::I),
                i.consume_key(egui::Modifiers::NONE, egui::Key::O),
                i.consume_key(egui::Modifiers::ALT, egui::Key::X),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
            )
        });
        let frame = self.shared.session.lock().frame.round();
        if mark_in {
            self.marks[0] = Some(frame);
            self.marks[1] = self.marks[1].filter(|o| *o >= frame);
        }
        if mark_out {
            self.marks[1] = Some(frame);
            self.marks[0] = self.marks[0].filter(|i| *i <= frame);
        }
        if unmark {
            self.marks = [None, None];
        }
        // Escape during a timeline drag only cancels the drag.
        if back && !self.timeline.dragging() {
            self.open_comp(None);
        }
        let selected = self.shared.session.lock().selected.clone();
        if let (true, Some(n)) = (delete, &selected) {
            self.run(&format!("del {n}"));
        }
        if let (true, Some(n)) = (dup, &selected) {
            let (new, offset) = {
                let s = self.shared.session.lock();
                let new = (2..).map(|i| format!("{n}_{i}")).find(|c| !s.doc().objects.contains_key(c)).unwrap();
                let o = s.doc().objects.get(n).map(|o| (o.loc, o.kind.is_2d()));
                (new, o)
            };
            if let Some((loc, is_2d)) = offset {
                let d = if is_2d { 24.0 } else { 0.4 };
                self.run(&format!("dup {n} {new} loc={},{},{}", fmt_num(loc.x + d), fmt_num(loc.y + d), fmt_num(loc.z)));
                self.shared.session.lock().selected = Some(new);
            }
        }
        if space {
            self.playing = !self.playing;
        }
        if redo {
            self.shared.session.lock().redo(Source::Human);
        } else if undo {
            self.shared.session.lock().undo(Source::Human);
        }
        if save {
            self.save();
        }
    }

    fn save(&mut self) {
        let Some(dir) = &self.project else { return };
        let s = self.shared.session.lock();
        if s.save(&dir.join("scene.ops")).is_ok() {
            self.saved_version = s.version;
        }
        self.last_save = Instant::now();
    }

    fn tick(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_tick).as_secs_f64();
        self.last_tick = now;
        self.sync_audio();
        // The open comp may have been deleted or undone away.
        let stale = {
            let s = self.shared.session.lock();
            s.comp.as_ref().is_some_and(|c| !s.doc().objects.get(c).is_some_and(|o| o.kind == Kind::Comp))
        };
        if stale {
            self.open_comp(None);
        }
        let req = std::mem::take(&mut *self.shared.ui.lock());
        if let Some(p) = req.play {
            self.playing = p;
        }
        if let Some(m) = req.mode {
            self.viewport.mode = if m == "3d" { crate::viewport::Mode::World } else { crate::viewport::Mode::Canvas };
        }
        if let Some(say) = req.say {
            self.notify(format!("Agent: {say}"));
        }
        if let Some(p) = req.progress {
            self.notify(p);
        }
        if req.library_changed {
            self.pool.refresh();
            self.shared.renderer.lock().reload_media();
        }
        let (start, end, fps) = {
            let s = self.shared.session.lock();
            let sc = &s.doc().scene;
            match &s.comp {
                Some(c) => (0.0, s.doc().comp_canvas(Some(c)).1 - 1.0, sc.fps),
                None => (sc.start as f32, sc.end as f32, sc.fps),
            }
        };
        if self.playing != self.was_playing {
            if self.playing {
                let f = self.shared.session.lock().frame;
                self.player.play((f / fps) as f64);
            } else {
                self.player.stop();
            }
            self.was_playing = self.playing;
        }
        if self.playing {
            // The audio clock drives the picture.
            self.player.advance(dt);
            let mut f = (self.player.position() * fps as f64) as f32;
            if f > end {
                f = start;
                self.player.play((start / fps) as f64);
            }
            self.shared.session.lock().frame = f;
            ctx.request_repaint();
        }
        // Autosave: the project file is always the current op script.
        let version = self.shared.session.lock().version;
        if version != self.saved_version && self.last_save.elapsed().as_secs_f32() > 1.0 {
            self.save();
        } else if version != self.saved_version {
            ctx.request_repaint_after(std::time::Duration::from_millis(1100));
        }
    }

    /// Hands the film's audio clips to the player (decoding on first use).
    /// Rebuilt only when the document or the open comp changed; silent inside a comp.
    fn sync_audio(&mut self) {
        let (clips, fps, end) = {
            let s = self.shared.session.lock();
            let key = (s.version, s.comp.clone());
            if self.audio_synced.as_ref() == Some(&key) {
                return;
            }
            self.audio_synced = Some(key);
            let doc = s.doc();
            let sounding = |o: &&ed_core::data::Object| match o.kind {
                Kind::Audio => true,
                Kind::Video => o.media.volume > 0.0,
                _ => false,
            };
            let clips: Vec<(String, f32, Option<f32>, f32, f32)> = if s.comp.is_some() {
                Vec::new()
            } else {
                doc.objects
                    .values()
                    .filter(|o| o.comp.is_none() && o.visible && !o.media.src.is_empty())
                    .filter(sounding)
                    .map(|o| (o.media.src.clone(), o.t_in.unwrap_or(0.0), o.t_out, o.media.trim, o.media.volume))
                    .collect()
            };
            (clips, doc.scene.fps as f64, (doc.scene.end + 1) as f64 / doc.scene.fps as f64)
        };
        let mut out = Vec::with_capacity(clips.len());
        for (src, t_in, t_out, trim, volume) in clips {
            let path = self.dir().join(&src);
            let Some(pcm) = self.player.pcm(&path) else { continue };
            self.peaks.entry(src).or_insert_with(|| Arc::new(ed_audio::peaks(&pcm, PEAKS_PER_SEC)));
            out.push(ed_audio::player::Clip {
                pcm,
                start: t_in as f64 / fps,
                trim: trim as f64,
                volume,
                end: t_out.map_or(end, |f| f as f64 / fps),
            });
        }
        self.player.set_clips(out);
    }

    /// tmux's status line: session (project) on the left, then the pages as
    /// windows `1:edit*`, status on the right. Pages are clickable too.
    fn status_bar(&mut self, ui: &mut egui::Ui) {
        use crate::theme::{ACCENT, BAR};
        let mono = |s: &str, size: f32| RichText::new(s).monospace().size(size);
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            let segment = |ui: &mut egui::Ui, text: RichText, fill: Color32| {
                egui::Frame::NONE.fill(fill).inner_margin(egui::Margin::symmetric(8, 4)).show(ui, |ui| ui.label(text)).response
            };
            if self.prefix.armed() {
                segment(ui, mono("^A", 12.0).color(Color32::BLACK).strong(), Color32::from_rgb(255, 214, 90));
            }
            let name = self.project.as_ref().and_then(|d| d.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            segment(ui, mono(&format!("[{name}]"), 12.0).color(Color32::BLACK).strong(), ACCENT);
            ui.add_space(6.0);
            for (i, p) in Page::ALL.into_iter().enumerate() {
                let on = self.page == p;
                let flag = if on && self.zoomed { "*Z" } else if on { "*" } else { " " };
                let text = mono(&format!("{}:{}{flag}", i + 1, p.label().to_lowercase()), 12.0);
                let text = if on { text.color(ACCENT).strong() } else { text.color(Color32::from_gray(150)) };
                let r = egui::Frame::NONE
                    .fill(if on { Color32::from_rgb(40, 41, 46) } else { BAR })
                    .inner_margin(egui::Margin::symmetric(8, 4))
                    .show(ui, |ui| ui.label(text))
                    .response
                    .interact(egui::Sense::click());
                if r.clicked() {
                    self.page = p;
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                let alive = self.terminal.as_ref().is_some_and(|t| !t.exited());
                let (dot, c) = if alive { ("● agent", Color32::from_rgb(110, 210, 130)) } else { ("○ agent", Color32::from_rgb(230, 100, 90)) };
                segment(ui, mono(dot, 12.0).color(c), Color32::from_rgb(36, 37, 42));
                segment(ui, mono("^A ?", 12.0).color(Color32::from_gray(120)), BAR);
                if let Some(s) = self.export_status.lock().clone() {
                    segment(ui, mono(&s, 11.0).color(Color32::from_gray(160)), BAR);
                }
                if let Some((note, at)) = &self.note
                    && at.elapsed().as_secs() < 8
                {
                    segment(ui, mono(note, 11.0).color(crate::theme::ACCENT_SOFT), BAR);
                    ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));
                }
            });
        });
    }

    fn exporting(&self) -> bool {
        self.export_status.lock().as_deref() == Some("exporting…")
    }

    /// Exports the film to `out.mp4` in the project, in the background.
    fn start_export(&mut self, ctx: &egui::Context) {
        if self.exporting() {
            return;
        }
        *self.export_status.lock() = Some("exporting…".into());
        let (shared, status, ctx) = (self.shared.clone(), self.export_status.clone(), ctx.clone());
        let path = ed_mcp::next_export_path(&self.dir(), "h264");
        let _ = std::fs::create_dir_all(path.parent().unwrap());
        std::thread::spawn(move || {
            let msg = match ed_mcp::export_video(&shared, &path, "h264", None) {
                Ok(m) => m,
                Err(e) => format!("export failed: {e:#}"),
            };
            *status.lock() = Some(msg);
            ctx.request_repaint();
        });
    }

    /// Picks files to copy into the project's media pool.
    pub fn import_dialog(&mut self) {
        let exts = ["png", "jpg", "jpeg", "webp", "gif", "mp4", "mov", "m4v", "webm", "mkv", "mp3", "wav", "flac", "m4a", "aiff", "ogg"];
        if let Some(files) = rfd::FileDialog::new().add_filter("media", &exts).pick_files() {
            self.import_files(&files);
        }
    }

    /// A command from the native menu bar (`crate::menu`).
    fn menu_command(&mut self, ctx: &egui::Context, id: &str) {
        if self.project.is_none() && id != "project.manager" {
            return;
        }
        match id {
            "project.manager" => self.manager.show(),
            "file.save" => {
                self.save();
                self.notify("saved");
            }
            "file.import" => self.import_dialog(),
            "file.export" => self.start_export(ctx),
            "file.finder" => {
                let _ = std::process::Command::new("open").arg(self.dir()).spawn();
            }
            "edit.undo" => _ = self.shared.session.lock().undo(Source::Human),
            "edit.redo" => _ = self.shared.session.lock().redo(Source::Human),
            "edit.delete" => {
                let selected = self.shared.session.lock().selected.clone();
                if let Some(n) = selected {
                    self.run(&format!("del {n}"));
                }
            }
            "edit.clearMarks" => self.marks = [None, None],
            "view.canvas" => self.viewport.mode = crate::viewport::Mode::Canvas,
            "view.world" => self.viewport.mode = crate::viewport::Mode::World,
            "view.film" => self.open_comp(None),
            "page.edit" => self.page = Page::Edit,
            "page.storyboard" => self.page = Page::Storyboard,
            "page.moodboard" => self.page = Page::Moodboard,
            "page.references" => self.page = Page::References,
            "page.extensions" => self.page = Page::Extensions,
            "agent.restart" => self.terminal = Terminal::spawn(&crate::agent::command(&self.dir()), &self.dir(), ctx.clone()).ok(),
            other => {
                if let Some(dir) = other.strip_prefix("recent:") {
                    self.open_project(ctx, PathBuf::from(dir));
                }
            }
        }
    }

    fn layers(&mut self, ui: &mut egui::Ui) {
        let mut s = self.shared.session.lock();
        let comp = s.comp.clone();
        let rows: Vec<(String, Kind, usize)> = s
            .doc()
            .objects
            .iter()
            .filter(|(_, o)| o.comp == comp && o.kind != Kind::Comp)
            .map(|(n, o)| {
                let mut depth = 0;
                let mut p = o.parent.clone();
                while let Some(n) = p {
                    depth += 1;
                    p = s.doc().objects.get(&n).and_then(|o| o.parent.clone());
                }
                (n.clone(), o.kind, depth)
            })
            .collect();
        egui::ScrollArea::vertical().max_height(ui.available_height() - 60.0).show(ui, |ui| {
            // The timeline's own settings: the film's scene, or the comp object.
            let (label, target) = match &comp {
                Some(c) => (format!("{} {c}", kind_icon(Kind::Comp)), Some(c.clone())),
                None => ("◆ scene".to_string(), None),
            };
            if ui.selectable_label(s.selected == target, label).clicked() {
                s.selected = target;
            }
            for (name, kind, depth) in rows {
                let label = format!("{}{} {name}", "   ".repeat(depth), kind_icon(kind));
                if ui.selectable_label(s.selected.as_deref() == Some(&name), label).clicked() {
                    s.selected = Some(name);
                }
            }
        });
        drop(s);
        ui.separator();
        ui.horizontal_wrapped(|ui| {
            ui.label("Add:");
            for kind in Kind::ALL.into_iter().filter(|k| !matches!(k, Kind::Comp | Kind::Clip)) {
                if ui.small_button(kind.name()).clicked() {
                    let n = self.free_name(kind.name());
                    let inside = comp.as_ref().map(|c| format!(" comp={c}")).unwrap_or_default();
                    self.run(&format!("add {} {n}{inside}", kind.name()));
                    self.shared.session.lock().selected = Some(n);
                }
            }
        });
    }

    /// One editor per property, chosen by value type. Covers every property
    /// of every kind, so the UI can never fall behind the agent's abilities.
    fn prop_editor(&mut self, ui: &mut egui::Ui, owner: &str, prop: &str, value: Value) {
        let path = format!("{owner}.{prop}");
        let speed = match prop {
            "opacity" | "galpha" | "leading" | "ambient" | "aperture" | "bloom" | "shutter" | "shadow" | "grain" | "vignette" => 0.01,
            "haze" => 0.001,
            "rot" | "grot" | "fov" | "weight" | "energy" => 0.5,
            "loc" | "scale" | "size" | "tracking" | "gloc" | "radius" | "z" | "in" | "out" | "stagger" => 1.0,
            _ => 0.05,
        };
        match value {
            Value::Num(mut v) => {
                let r = ui.add(egui::DragValue::new(&mut v).speed(speed).max_decimals(3));
                if r.changed() {
                    self.run_edit(&format!("set {path} {}", fmt_num(v)), &path, r.dragged());
                }
            }
            Value::Vec(v) if prop == "color" || prop == "bg" => {
                let mut c = egui::Rgba::from_rgba_premultiplied(v[0], v[1], v[2], v[3]);
                let alpha = egui::color_picker::Alpha::OnlyBlend;
                if egui::widgets::color_picker::color_edit_button_rgba(ui, &mut c, alpha).changed() {
                    let v = Value::Vec(c.to_array().to_vec());
                    self.run_edit(&format!("set {path} {v}"), &path, true);
                }
            }
            Value::Vec(mut v) => {
                let mut changed = None;
                ui.horizontal(|ui| {
                    for x in v.iter_mut() {
                        let r = ui.add(egui::DragValue::new(x).speed(speed).max_decimals(3));
                        if r.changed() {
                            changed = Some(r.dragged());
                        }
                    }
                });
                if let Some(dragging) = changed {
                    self.run_edit(&format!("set {path} {}", Value::Vec(v)), &path, dragging);
                }
            }
            Value::Bool(mut b) => {
                if ui.checkbox(&mut b, "").changed() {
                    self.run(&format!("set {path} {b}"));
                }
            }
            Value::Str(mut s) if prop == "text" => {
                let r = ui.add(egui::TextEdit::multiline(&mut s).desired_rows(2).desired_width(f32::INFINITY));
                if r.changed() {
                    self.run_edit(&format!("set {path} {}", Value::Str(s)), &path, true);
                }
            }
            Value::Str(s) => {
                let id = ui.id().with(&path);
                let mut buf = ui.data_mut(|d| d.get_temp::<String>(id)).unwrap_or(s.clone());
                let r = ui.add(egui::TextEdit::singleline(&mut buf).desired_width(160.0));
                if r.has_focus() {
                    ui.data_mut(|d| d.insert_temp(id, buf.clone()));
                } else {
                    ui.data_mut(|d| d.remove::<String>(id));
                }
                if r.lost_focus() && buf != s {
                    self.run(&format!("set {path} {}", if buf.is_empty() { "none".into() } else { Value::Str(buf).to_string() }));
                }
            }
            Value::None if matches!(prop, "in" | "out") => {
                if ui.small_button("set to playhead").clicked() {
                    let f = self.shared.session.lock().frame.round();
                    self.run(&format!("set {path} {}", fmt_num(f)));
                }
            }
            Value::None => {
                let id = ui.id().with(&path);
                let mut buf = ui.data_mut(|d| d.get_temp::<String>(id)).unwrap_or_default();
                let r = ui.add(egui::TextEdit::singleline(&mut buf).hint_text("none").desired_width(160.0));
                ui.data_mut(|d| d.insert_temp(id, buf.clone()));
                if r.lost_focus() && !buf.is_empty() {
                    ui.data_mut(|d| d.remove::<String>(id));
                    self.run(&format!("set {path} {buf}"));
                }
            }
        }
    }

    fn properties(&mut self, ui: &mut egui::Ui) {
        let (selected, frame) = {
            let s = self.shared.session.lock();
            (s.selected.clone().filter(|n| s.doc().objects.contains_key(n)), s.frame)
        };
        let Some(name) = selected else {
            return self.scene_properties(ui);
        };
        let o = {
            let s = self.shared.session.lock();
            ed_core::eval::animated(&s.doc().objects[&name], &name, frame, s.doc().scene.fps)
        };
        ui.heading(format!("{} {name}", kind_icon(o.kind)));
        egui::ScrollArea::vertical().id_salt("props").show(ui, |ui| {
            egui::Grid::new("props").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
                for prop in paths::props_for(o.kind) {
                    let keyed = o.anim.keys().any(|c| c == prop || c.starts_with(&format!("{prop}.")));
                    let label = if keyed { RichText::new(prop).color(Color32::from_rgb(240, 190, 60)) } else { prop.into() };
                    ui.label(label);
                    if let Ok(v) = paths::get_object(&o, prop) {
                        self.prop_editor(ui, &name, prop, v);
                    }
                    ui.end_row();
                }
            });
            ui.separator();
            ui.horizontal(|ui| {
                let f = fmt_num(frame.round());
                if ui.button("Key transform").clicked() {
                    let props: String = ["loc", "rot", "scale"]
                        .iter()
                        .map(|p| format!(" {p}={}", paths::get_object(&o, p).unwrap()))
                        .collect();
                    self.run(&format!("key {name}{props} @{f}"));
                }
                if o.kind == Kind::Camera && ui.button("Make scene camera").clicked() {
                    self.run(&format!("set scene.camera {name}"));
                }
                if ui.button("Delete").clicked() {
                    self.run(&format!("del {name}"));
                }
            });
            let keys = {
                let s = self.shared.session.lock();
                ed_core::lang::fmt_keys(&name, &s.doc().objects[&name])
            };
            for k in keys {
                ui.monospace(k);
            }
        });
    }

    fn scene_properties(&mut self, ui: &mut egui::Ui) {
        let scene = self.shared.session.lock().doc().scene.clone();
        ui.heading("◆ scene");
        egui::Grid::new("scene").num_columns(2).show(ui, |ui| {
            for prop in paths::SCENE_PROPS {
                ui.label(prop);
                if let Ok(v) = paths::get_scene(&scene, prop) {
                    self.prop_editor(ui, "scene", prop, v);
                }
                ui.end_row();
            }
        });
    }

    fn log(&mut self, ui: &mut egui::Ui) {
        let entries: Vec<_> = {
            let s = self.shared.session.lock();
            s.log.iter().rev().take(300).rev().cloned().collect()
        };
        let console_h = 28.0;
        egui::ScrollArea::vertical()
            .max_height(ui.available_height() - console_h)
            .stick_to_bottom(true)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for e in entries {
                    let color = match e.source {
                        Source::Agent => Color32::from_rgb(140, 180, 255),
                        Source::Human => Color32::from_rgb(200, 200, 200),
                    };
                    let mut line = format!("{} › {}", e.source.name(), e.text.replace('\n', " ; "));
                    if let Some(err) = &e.error {
                        line = format!("{line}   ✗ {err}");
                    }
                    let color = if e.error.is_some() { Color32::LIGHT_RED } else { color };
                    ui.label(RichText::new(line).monospace().color(color));
                }
            });
        ui.horizontal(|ui| {
            ui.monospace("›");
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.console)
                    .font(egui::TextStyle::Monospace)
                    .hint_text("op console, e.g.  add text title text=\"Hello\"")
                    .desired_width(f32::INFINITY),
            );
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                let cmd = std::mem::take(&mut self.console);
                self.run(&cmd);
                r.request_focus();
            }
        });
    }

    /// The agent's terminal: the whole right side, on every page.
    fn agent(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("✦ Agent").strong());
            ui.label(RichText::new("director · omp").small().color(Color32::from_gray(130)));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("Restart").on_hover_text("a fresh agent session in this project").clicked() {
                    self.terminal = Terminal::spawn(&crate::agent::command(&self.dir()), &self.dir(), ui.ctx().clone()).ok();
                }
            });
        });
        ui.add_space(4.0);
        match &mut self.terminal {
            Some(t) if t.exited() => {
                ui.label("The agent (omp) exited. Restart it above.");
            }
            Some(t) => t.ui(ui),
            None => {
                ui.colored_label(Color32::LIGHT_RED, self.terminal_error.as_deref().unwrap_or("no terminal"));
            }
        }
    }
}

pub fn kind_icon(k: Kind) -> &'static str {
    match k {
        Kind::Cube => "▣",
        Kind::Sphere => "●",
        Kind::Plane => "▱",
        Kind::Cylinder => "◫",
        Kind::Model => "♜",
        Kind::Camera => "🎥",
        Kind::Light => "💡",
        Kind::Empty => "✛",
        Kind::Points => "⁘",
        Kind::Rect => "▭",
        Kind::Ellipse => "◯",
        Kind::Text => "T",
        Kind::View => "▶",
        Kind::Fx => "✨",
        Kind::Audio => "♪",
        Kind::Image => "🖼",
        Kind::Video => "🎞",
        Kind::Comp => "▦",
        Kind::Clip => "🎬",
        Kind::Shader => "✺",
    }
}

impl egui_dock::TabViewer for Panels {
    type Tab = Tab;

    fn id(&mut self, tab: &mut Tab) -> egui::Id {
        egui::Id::new(*tab)
    }

    fn title(&mut self, tab: &mut Tab) -> egui::WidgetText {
        match tab {
            Tab::Viewport => "Timeline viewer",
            Tab::Source => "Source viewer",
            Tab::Pool => "Media pool",
            other => return format!("{other:?}").into(),
        }
        .into()
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Tab) {
        match tab {
            Tab::Viewport => {
                let rs = self.rs.clone();
                match self.viewport.ui(ui, &self.shared, &rs, &self.pointed) {
                    Action::MoveLayer { name, delta, continuing } => {
                        let loc = self.shared.session.lock().doc().objects[&name].loc;
                        let v = Value::Vec(vec![loc.x + delta[0], loc.y + delta[1], loc.z]);
                        self.run_edit(&format!("set {name}.loc {v}"), &format!("{name}.loc"), continuing);
                    }
                    Action::Select(name) => self.shared.session.lock().selected = name,
                    Action::Point(target) => self.point(target),
                    Action::None => {}
                }
            }
            Tab::Layers => self.layers(ui),
            Tab::Properties => self.properties(ui),
            Tab::Timeline => {
                let mut timeline = std::mem::take(&mut self.timeline);
                timeline.ui(ui, self);
                self.timeline = timeline;
            }
            Tab::Pool => {
                let mut pool = std::mem::take(&mut self.pool);
                pool.ui(ui, self);
                self.pool = pool;
            }
            Tab::Source => {
                let rs = self.rs.clone();
                let mut pool = std::mem::take(&mut self.pool);
                pool.source_ui(ui, self, &rs);
                self.pool = pool;
            }
            Tab::Log => self.log(ui),
        }
    }
}
