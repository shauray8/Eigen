//! Viewport: the canvas (the film frame, exactly what exports) or the 3D
//! world from a free orbit camera. Re-renders only when something changed.
//! On the canvas the human can also point: at a layer (right-click), at a
//! region (Region tool) or with a drawn review note (Note tool).

use std::hash::{Hash, Hasher};

use eframe::egui::{self, Color32, Pos2, Stroke};
use ed_core::data::{Document, Kind, Object};
use ed_core::eval::{self, box_corners, layer_box};
use ed_mcp::Shared;
use ed_mcp::refs::{Author, Target};
use ed_render::{Target as RenderTarget, ViewCam};
use glam::Vec3;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Mode {
    Canvas,
    World,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tool {
    Select,
    Region,
    Note,
}

const MARK: Color32 = Color32::from_rgb(ed_mcp::refs::MARK[0], ed_mcp::refs::MARK[1], ed_mcp::refs::MARK[2]);
const NOTE: Color32 = Color32::from_rgb(255, 225, 70);
const DIRECTOR: Color32 = Color32::from_rgb(90, 220, 255);

/// An offscreen render shown as an egui image, redrawn only when its key
/// (or its pixel size) changes. The viewport and the source viewer use one each.
#[derive(Default)]
pub struct Canvas {
    target: Option<RenderTarget>,
    texture: Option<egui::TextureId>,
    key: Option<u64>,
}

impl Canvas {
    /// The largest rect of `res`'s shape centered in `avail`.
    pub fn fit(avail: egui::Rect, res: [u32; 2]) -> egui::Rect {
        let aspect = res[0] as f32 / res[1].max(1) as f32;
        let mut size = avail.size();
        if size.x / size.y > aspect {
            size.x = size.y * aspect;
        } else {
            size.y = size.x / aspect;
        }
        egui::Rect::from_center_size(avail.center(), size)
    }

    /// Calls `draw` into a target sized for `rect` when `key` changed, then paints it.
    pub fn show(&mut self, ui: &egui::Ui, rs: &eframe::egui_wgpu::RenderState, rect: egui::Rect, key: impl Hash, draw: impl FnOnce(&RenderTarget)) {
        let ppp = ui.ctx().pixels_per_point();
        let px = [(rect.width() * ppp).round().max(1.0) as u32, (rect.height() * ppp).round().max(1.0) as u32];
        let mut h = std::hash::DefaultHasher::new();
        (key, px).hash(&mut h);
        let key = h.finish();
        if self.key != Some(key) {
            if self.target.as_ref().is_none_or(|t| t.size != px) {
                let target = RenderTarget::new(&rs.device, px);
                let view = target.ui_view();
                let mut r = rs.renderer.write();
                match self.texture {
                    Some(id) => r.update_egui_texture_from_wgpu_texture(&rs.device, &view, wgpu::FilterMode::Linear, id),
                    None => self.texture = Some(r.register_native_texture(&rs.device, &view, wgpu::FilterMode::Linear)),
                }
                self.target = Some(target);
            }
            draw(self.target.as_ref().unwrap());
            self.key = Some(key);
        }
        if let Some(id) = self.texture {
            let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
            ui.painter().image(id, rect, uv, Color32::WHITE);
        }
        ui.painter().rect_stroke(rect, 0.0, Stroke::new(1.0, Color32::from_gray(70)), egui::StrokeKind::Outside);
    }
}

pub struct Viewport {
    canvas: Canvas,
    orbit: ViewCam,
    pub mode: Mode,
    pub tool: Tool,
    /// Region tool: where the drag started (scene px).
    drag_from: Option<[f32; 2]>,
    /// Note tool: strokes drawn so far and the text being written.
    draft: Vec<Vec<[f32; 2]>>,
    note: String,
    /// Layer under the pointer when the context menu opened.
    menu_hit: Option<String>,
}

/// What the viewport wants the app to do (edits go through ops).
pub enum Action {
    None,
    /// Move a canvas layer by a delta in scene pixels; `continuing` is false on the first frame of a drag.
    MoveLayer { name: String, delta: [f32; 2], continuing: bool },
    /// Click on the canvas: the topmost layer under the pointer (None: empty space).
    Select(Option<String>),
    /// The human pointed at something; it becomes a ref attached to the Ask bar.
    Point(Target),
}

/// Topmost layer of composition `comp` under `p` (scene px).
fn hit(doc: &Document, comp: Option<&str>, frame: f32, p: [f32; 2]) -> Option<String> {
    let mut layers: Vec<(f32, usize, &String, Object)> = doc
        .objects
        .iter()
        .enumerate()
        .filter(|(_, (_, o))| o.comp.as_deref() == comp && o.kind.is_2d() && o.kind != Kind::Fx && o.visible && o.alive(frame))
        .map(|(i, (n, o))| (o.z, i, n, eval::animated(o, n, frame, doc.scene.fps)))
        .collect();
    layers.sort_by(|a, b| b.0.total_cmp(&a.0).then(b.1.cmp(&a.1)));
    layers.into_iter().find_map(|(_, _, name, o)| {
        let (c, h, rot) = layer_box(&o)?;
        let (sn, cs) = (-rot.to_radians()).sin_cos();
        let (dx, dy) = (p[0] - c[0], p[1] - c[1]);
        let (lx, ly) = (dx * cs - dy * sn, dx * sn + dy * cs);
        (lx.abs() <= h[0] && ly.abs() <= h[1]).then(|| name.clone())
    })
}

fn rect_of(a: [f32; 2], b: [f32; 2]) -> [f32; 4] {
    [a[0].min(b[0]), a[1].min(b[1]), (a[0] - b[0]).abs(), (a[1] - b[1]).abs()]
}

impl Viewport {
    pub fn new() -> Self {
        Self {
            canvas: Canvas::default(),
            orbit: ViewCam::default_orbit(),
            mode: Mode::Canvas,
            tool: Tool::Select,
            drag_from: None,
            draft: Vec::new(),
            note: String::new(),
            menu_hit: None,
        }
    }

    fn orbit_key(&self) -> [u32; 5] {
        match self.orbit {
            ViewCam::Orbit { target, yaw, pitch, dist } => {
                [target.x.to_bits() ^ target.z.to_bits(), target.y.to_bits(), yaw.to_bits(), pitch.to_bits(), dist.to_bits()]
            }
            ViewCam::Camera { .. } => [0; 5],
        }
    }

    /// `attached`: ids of the refs waiting in the Ask bar (outlined while in range).
    pub fn ui(&mut self, ui: &mut egui::Ui, shared: &Shared, rs: &eframe::egui_wgpu::RenderState, attached: &[String]) -> Action {
        let mut action = Action::None;
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.mode, Mode::Canvas, "Canvas");
            ui.selectable_value(&mut self.mode, Mode::World, "3D");
            ui.separator();
            match self.mode {
                Mode::Canvas => {
                    ui.selectable_value(&mut self.tool, Tool::Select, "⬉ Select")
                        .on_hover_text("click: select · drag: move the selected layer · right-click: point at a layer");
                    ui.selectable_value(&mut self.tool, Tool::Region, "⬚ Region")
                        .on_hover_text("drag a box to point at a region, over the I/O marked frames (else this frame)");
                    ui.selectable_value(&mut self.tool, Tool::Note, "✎ Note").on_hover_text("draw on the frame, write a note, Post");
                    if self.tool == Tool::Note {
                        let r = ui.add(egui::TextEdit::singleline(&mut self.note).hint_text("note for the agent…").desired_width(240.0));
                        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        let ready = !self.note.trim().is_empty() || !self.draft.is_empty();
                        if (ui.add_enabled(ready, egui::Button::new("Post")).clicked() || enter) && ready {
                            action = Action::Point(Target::Note {
                                text: std::mem::take(&mut self.note).trim().to_string(),
                                strokes: std::mem::take(&mut self.draft),
                                done: false,
                                reply: String::new(),
                                by: ed_mcp::refs::Author::Human,
                            });
                        }
                        if !self.draft.is_empty() && ui.small_button("Clear").clicked() {
                            self.draft.clear();
                        }
                    }
                }
                Mode::World => {
                    if ui.small_button("Reset view").clicked() {
                        self.orbit = ViewCam::default_orbit();
                    }
                    ui.weak("drag: orbit · shift+drag: pan · scroll: zoom");
                }
            }
        });

        let (res, selected, version, frame, comp) = {
            let s = shared.session.lock();
            (s.doc().comp_canvas(s.comp.as_deref()).0, s.selected.clone(), s.version, s.frame, s.comp.clone())
        };
        let comp = comp.as_deref();
        let avail = ui.available_rect_before_wrap();
        let rect = Canvas::fit(avail, res);
        let size = rect.size();
        let response = ui.allocate_rect(avail, egui::Sense::click_and_drag());
        let k = size.x / res[0] as f32;
        let to_scene = |p: Pos2| [((p.x - rect.left()) / k).clamp(0.0, res[0] as f32), ((p.y - rect.top()) / k).clamp(0.0, res[1] as f32)];
        let to_screen = |p: [f32; 2]| Pos2::new(rect.left() + p[0] * k, rect.top() + p[1] * k);

        match self.mode {
            Mode::Canvas => match self.tool {
                Tool::Select => {
                    let movable = selected.filter(|n| shared.session.lock().doc().objects.get(n).is_some_and(|o| o.kind.is_2d()));
                    if response.clicked() {
                        if let Some(p) = response.interact_pointer_pos() {
                            action = Action::Select(hit(shared.session.lock().doc(), comp, frame, to_scene(p)));
                        }
                    } else if let (true, Some(name)) = (response.dragged(), movable) {
                        let d = response.drag_delta() / k;
                        action = Action::MoveLayer { name, delta: [d.x, d.y], continuing: !response.drag_started() };
                    }
                    if response.secondary_clicked() {
                        self.menu_hit = response.interact_pointer_pos().and_then(|p| hit(shared.session.lock().doc(), comp, frame, to_scene(p)));
                    }
                    response.context_menu(|ui| {
                        if let Some(name) = &self.menu_hit
                            && ui.button(format!("Point at `{name}`")).clicked()
                        {
                            action = Action::Point(Target::Object { name: name.clone() });
                            ui.close();
                        }
                        if ui.button("Point at the whole frame").clicked() {
                            action = Action::Point(Target::Region { rect: [0.0, 0.0, res[0] as f32, res[1] as f32] });
                            ui.close();
                        }
                    });
                }
                Tool::Region => {
                    if response.drag_started() {
                        self.drag_from = response.interact_pointer_pos().map(to_scene);
                    }
                    if response.drag_stopped()
                        && let (Some(a), Some(p)) = (self.drag_from.take(), response.interact_pointer_pos())
                    {
                        let r = rect_of(a, to_scene(p));
                        if r[2] >= 4.0 && r[3] >= 4.0 {
                            action = Action::Point(Target::Region { rect: r });
                        }
                    }
                }
                Tool::Note => {
                    if response.drag_started() {
                        self.draft.push(Vec::new());
                    }
                    if response.dragged()
                        && let (Some(stroke), Some(p)) = (self.draft.last_mut(), response.interact_pointer_pos())
                    {
                        stroke.push(to_scene(p));
                    }
                }
            },
            Mode::World => {
                if let ViewCam::Orbit { target, yaw, pitch, dist } = &mut self.orbit {
                    if response.dragged() {
                        let d = response.drag_delta();
                        if ui.input(|i| i.modifiers.shift) {
                            let right = Vec3::new(yaw.cos(), 0.0, -yaw.sin());
                            *target += (-right * d.x + Vec3::Y * d.y) * *dist * 0.002;
                        } else {
                            *yaw -= d.x * 0.01;
                            *pitch = (*pitch + d.y * 0.01).clamp(-1.5, 1.5);
                        }
                    }
                    if response.hovered() {
                        let scroll = ui.input(|i| i.smooth_scroll_delta.y + (i.zoom_delta() - 1.0) * 200.0);
                        if scroll != 0.0 {
                            *dist = (*dist * (1.0 - scroll * 0.002)).clamp(0.2, 500.0);
                        }
                    }
                }
            }
        }

        let mode = self.mode;
        let orbit = self.orbit;
        self.canvas.show(ui, rs, rect, (version, frame.to_bits(), mode, self.orbit_key(), comp), |target| {
            let scene = eval::evaluate_comp(shared.session.lock().doc(), comp, frame);
            let renderer = shared.renderer.lock();
            match mode {
                Mode::Canvas => renderer.render_canvas(&scene, target),
                Mode::World => {
                    let bg = scene.bg;
                    renderer.render_world(&scene, orbit, target, [bg[0], bg[1], bg[2], 1.0]);
                }
            }
        });
        let painter = ui.painter_at(avail);
        if self.mode != Mode::Canvas {
            return action;
        }

        // Refs in range of this frame: open notes, and what the Ask bar points at.
        let f = frame.round();
        let s = shared.session.lock();
        let shown: Vec<(Vec<Vec<[f32; 2]>>, String, Color32)> = shared
            .refs
            .lock()
            .all()
            .iter()
            .filter(|r| r.comp.as_deref() == comp && r.frames[0] <= f && f <= r.frames[1] && (r.is_open_note() || attached.contains(&r.id)))
            .map(|r| {
                let (label, color) = match &r.target {
                    Target::Note { text, by, .. } => (format!("{} {text}", r.token()), if *by == Author::Director { DIRECTOR } else { NOTE }),
                    _ => (r.token(), MARK),
                };
                (ed_mcp::refs::marks(r, s.doc(), frame), label, color)
            })
            .collect();
        let line = |pts: &[[f32; 2]], stroke: Stroke| {
            painter.add(egui::Shape::line(pts.iter().map(|p| to_screen(*p)).collect(), stroke));
        };
        let outline = |c: [[f32; 2]; 4], stroke: Stroke| line(&[c[0], c[1], c[2], c[3], c[0]], stroke);
        for (lines, label, color) in &shown {
            for l in lines {
                line(l, Stroke::new(2.0, *color));
            }
            let at = lines.first().and_then(|l| l.first()).copied().unwrap_or([16.0, 40.0]);
            let galley = painter.layout_no_wrap(label.clone(), egui::FontId::proportional(12.0), Color32::BLACK);
            let pos = to_screen(at) - egui::vec2(0.0, galley.size().y + 4.0);
            painter.rect_filled(egui::Rect::from_min_size(pos, galley.size() + egui::vec2(6.0, 2.0)), 2.0, *color);
            painter.galley(pos + egui::vec2(3.0, 1.0), galley, Color32::BLACK);
        }

        // Selection outline.
        let sel = s.selected.as_ref().and_then(|n| s.doc().objects.get(n).map(|o| eval::animated(o, n, frame, s.doc().scene.fps)));
        if let Some(b) = sel.as_ref().and_then(layer_box) {
            outline(box_corners(b), Stroke::new(1.5, Color32::from_rgb(255, 170, 60)));
        }
        // Drafts: the region being dragged, the note being drawn.
        if let (Some(a), Some(p)) = (self.drag_from, response.interact_pointer_pos()) {
            let [x, y, w, h] = rect_of(a, to_scene(p));
            outline([[x, y], [x + w, y], [x + w, y + h], [x, y + h]], Stroke::new(1.5, MARK));
        }
        for st in &self.draft {
            line(st, Stroke::new(2.5, NOTE));
        }
        if s.doc().objects.is_empty() {
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "Describe your film in the Ask bar above,\nor drop images, video and music onto the window.",
                egui::FontId::proportional(18.0),
                Color32::from_gray(120),
            );
        }
        action
    }
}
