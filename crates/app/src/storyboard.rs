//! Storyboard page: the film as rough panels. A dark board you sketch on in
//! white or red marker (the agent drafts in blue), with a text tool for
//! words on the frame, a details column for the selected shot, and a strip
//! of numbered cards underneath. Panels play in order through the film.

use std::collections::HashMap;

use eframe::egui::{self, Color32, CornerRadius, Pos2, Rect, RichText, Stroke, TextureHandle, vec2};
use ed_mcp::storyboard::{INKS, Label, PAPER, Panel, Stroke as Line};

use crate::app::Panels;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Tool {
    #[default]
    Pen,
    Eraser,
    Text,
}

#[derive(Default)]
pub struct Board {
    selected: Option<String>,
    tool: Tool,
    ink: usize,
    /// Pen width as a fraction of the panel's width (0: not set yet).
    width: f32,
    /// Text tool: where the label goes (0..1) and what is typed so far.
    typing: Option<([f32; 2], String)>,
    /// Pictures under panels, by project path.
    pictures: HashMap<String, Option<TextureHandle>>,
}

fn color(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

const CARD_H: f32 = 92.0;

impl Board {
    fn picture(&mut self, ctx: &egui::Context, dir: &std::path::Path, src: &str) -> Option<TextureHandle> {
        self.pictures
            .entry(src.to_string())
            .or_insert_with(|| {
                let pic = ed_media::load_image(&dir.join(src)).ok()?;
                let img = egui::ColorImage::from_rgba_unmultiplied([pic.w as usize, pic.h as usize], &pic.rgba);
                Some(ctx.load_texture(src, img, Default::default()))
            })
            .clone()
    }

    /// Board, picture, strokes and words of `p` inside `rect`.
    fn paint(&mut self, ui: &egui::Ui, dir: &std::path::Path, p: &Panel, rect: Rect) {
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, CornerRadius::same(6), color(PAPER));
        if let Some(t) = p.image.as_ref().and_then(|src| self.picture(ui.ctx(), dir, src)) {
            let s = t.size_vec2();
            let k = (rect.width() / s.x).min(rect.height() / s.y);
            let r = Rect::from_center_size(rect.center(), s * k);
            painter.image(t.id(), r, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::from_white_alpha(170));
        }
        let at = |q: [f32; 2]| rect.min + vec2(q[0] * rect.width(), q[1] * rect.height());
        for s in &p.strokes {
            let pts: Vec<Pos2> = s.points.iter().map(|q| at(*q)).collect();
            let w = (s.width * rect.width()).max(1.0);
            if let [one] = pts.as_slice() {
                painter.circle_filled(*one, w / 2.0, color(s.color));
            } else {
                painter.add(egui::Shape::line(pts, Stroke::new(w, color(s.color))));
            }
        }
        for l in &p.labels {
            let size = (l.size * rect.height()).max(6.0);
            painter.text(at(l.pos), egui::Align2::LEFT_TOP, &l.text, egui::FontId::proportional(size), color(l.color));
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, panels: &mut Panels) {
        if self.width == 0.0 {
            self.width = 0.005;
        }
        let dir = panels.dir();
        let (fps, start, res, frame, in_film) = {
            let s = panels.shared.session.lock();
            let sc = &s.doc().scene;
            (sc.fps, sc.start as f32, sc.res, s.frame, s.comp.is_none())
        };
        let aspect = res[0] as f32 / res[1].max(1) as f32;
        let mut board = panels.shared.storyboard.lock();
        if self.selected.as_ref().is_none_or(|id| board.index(id).is_none()) {
            self.selected = board.panels.first().map(|p| p.id.clone());
        }
        let mut attach = Vec::new();
        let mut seek = None;
        let mut dirty = false;

        // ---- Header: title, length, tools.
        let total: f32 = board.panels.iter().map(|p| p.seconds).sum();
        ui.horizontal(|ui| {
            ui.heading("Storyboard");
            ui.label(RichText::new(format!("{} shots · {:.1}s", board.panels.len(), total)).color(Color32::from_gray(140)));
            ui.add_space(20.0);
            for (t, label, tip) in [(Tool::Pen, "✏ Pen", "draw (P)"), (Tool::Eraser, "⌫ Eraser", "drag over lines to remove them (E)"), (Tool::Text, "T Text", "click the board to write (T)")] {
                if ui.selectable_label(self.tool == t, label).on_hover_text(tip).clicked() {
                    self.tool = t;
                    self.typing = None;
                }
            }
            ui.separator();
            for (i, ink) in INKS[..2].iter().enumerate() {
                let (r, resp) = ui.allocate_exact_size(vec2(22.0, 22.0), egui::Sense::click());
                ui.painter().circle_filled(r.center(), 8.0, color(*ink));
                if self.ink == i {
                    ui.painter().circle_stroke(r.center(), 10.5, Stroke::new(1.5, crate::theme::ACCENT));
                }
                if resp.on_hover_text(if i == 0 { "white marker" } else { "red marker" }).clicked() {
                    self.ink = i;
                }
            }
            ui.add(egui::Slider::new(&mut self.width, 0.002..=0.02).show_value(false)).on_hover_text("marker size");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Send board to agent").on_hover_text("every shot, one line each, into the agent's prompt").clicked() {
                    attach.extend(board.panels.iter().map(|p| p.id.clone()));
                }
            });
        });
        let (p_key, e_key, t_key) = if ui.ctx().memory(|m| m.focused().is_some()) {
            (false, false, false)
        } else {
            ui.input(|i| (i.key_pressed(egui::Key::P), i.key_pressed(egui::Key::E), i.key_pressed(egui::Key::T)))
        };
        if p_key {
            self.tool = Tool::Pen;
        }
        if e_key {
            self.tool = Tool::Eraser;
        }
        if t_key {
            self.tool = Tool::Text;
        }
        ui.add_space(8.0);

        // ---- Strip of shots at the bottom; board and details above it.
        let strip_h = CARD_H + 34.0;
        let avail = ui.available_rect_before_wrap();
        let top = Rect::from_min_max(avail.min, Pos2::new(avail.right(), avail.bottom() - strip_h));
        let strip = Rect::from_min_max(Pos2::new(avail.left(), avail.bottom() - strip_h + 8.0), avail.max);
        ui.allocate_rect(avail, egui::Sense::hover());

        let selected = self.selected.as_ref().and_then(|id| board.index(id));
        match selected {
            None => {
                let mut c = ui.new_child(egui::UiBuilder::new().max_rect(top));
                c.vertical_centered(|ui| {
                    ui.add_space(top.height() * 0.3);
                    ui.label(RichText::new("No shots yet").size(20.0).color(Color32::from_gray(200)));
                    ui.label(RichText::new("Sketch the film shot by shot, or ask the agent to draft a storyboard.").color(Color32::from_gray(130)));
                    ui.add_space(10.0);
                    if ui.button("＋ First shot").clicked() {
                        self.selected = Some(board.add(None, String::new(), 2.0));
                    }
                });
            }
            Some(i) => {
                // Details column on the right.
                let side_w = 260.0_f32.min(top.width() * 0.35);
                let side = Rect::from_min_max(Pos2::new(top.right() - side_w, top.top()), top.max);
                let canvas_area = Rect::from_min_max(top.min, Pos2::new(side.left() - 16.0, top.bottom()));
                let mut size = canvas_area.size();
                if size.x / size.y > aspect {
                    size.x = size.y * aspect;
                } else {
                    size.y = size.x / aspect;
                }
                let rect = Rect::from_center_size(canvas_area.center(), size);

                let p = board.panels[i].clone();
                let resp = ui.interact(rect, ui.id().with("board-canvas"), egui::Sense::click_and_drag());
                self.paint(ui, &dir, &p, rect);
                ui.painter().rect_stroke(rect, CornerRadius::same(6), Stroke::new(1.0, Color32::from_gray(55)), egui::StrokeKind::Outside);
                ui.painter().text(
                    rect.left_top() + vec2(10.0, 8.0),
                    egui::Align2::LEFT_TOP,
                    format!("{}", i + 1),
                    egui::FontId::proportional(14.0),
                    Color32::from_gray(110),
                );
                let norm = |pos: Pos2| [((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0), ((pos.y - rect.top()) / rect.height()).clamp(0.0, 1.0)];
                let panel = &mut board.panels[i];
                match self.tool {
                    Tool::Pen => {
                        if resp.drag_started() {
                            panel.strokes.push(Line { color: INKS[self.ink], width: self.width, points: Vec::new() });
                        }
                        if resp.dragged()
                            && let (Some(s), Some(pos)) = (panel.strokes.last_mut(), resp.interact_pointer_pos())
                        {
                            s.points.push(norm(pos));
                        }
                        dirty |= resp.drag_stopped();
                        if resp.hovered() {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
                        }
                    }
                    Tool::Eraser => {
                        if (resp.dragged() || resp.clicked())
                            && let Some(pos) = resp.interact_pointer_pos()
                        {
                            let q = norm(pos);
                            let reach = 0.02_f32;
                            let before = panel.strokes.len() + panel.labels.len();
                            panel.strokes.retain(|s| !s.points.iter().any(|p| (p[0] - q[0]).hypot((p[1] - q[1]) / aspect) < reach));
                            panel.labels.retain(|l| !(q[0] >= l.pos[0] && q[0] <= l.pos[0] + 0.02 * l.text.len() as f32 && q[1] >= l.pos[1] && q[1] <= l.pos[1] + l.size));
                            dirty |= before != panel.strokes.len() + panel.labels.len();
                        }
                        if let Some(h) = resp.hover_pos() {
                            ui.painter().circle_stroke(h, 0.02 * rect.width(), Stroke::new(1.0, Color32::from_gray(160)));
                        }
                    }
                    Tool::Text => {
                        if resp.clicked()
                            && let Some(pos) = resp.interact_pointer_pos()
                        {
                            self.typing = Some((norm(pos), String::new()));
                        }
                        if resp.hovered() {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
                        }
                    }
                }
                // The label being typed, in place on the board.
                let mut commit = None;
                if let Some((at, text)) = &mut self.typing {
                    let pos = rect.min + vec2(at[0] * rect.width(), at[1] * rect.height());
                    let font = egui::FontId::proportional((0.06 * rect.height()).max(10.0));
                    egui::Area::new(ui.id().with("board-typing")).fixed_pos(pos).order(egui::Order::Foreground).show(ui.ctx(), |ui| {
                        let r = ui.add(
                            egui::TextEdit::singleline(text)
                                .font(font)
                                .text_color(color(INKS[self.ink]))
                                .background_color(Color32::from_black_alpha(160))
                                .desired_width(rect.right() - pos.x - 8.0)
                                .hint_text("type, Enter"),
                        );
                        r.request_focus();
                        if r.lost_focus() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            commit = Some(ui.input(|i| !i.key_pressed(egui::Key::Escape)));
                        }
                    });
                }
                if let Some(keep) = commit {
                    if let Some((at, text)) = self.typing.take()
                        && keep
                        && !text.trim().is_empty()
                    {
                        panel.labels.push(Label { pos: at, text: text.trim().to_string(), color: INKS[self.ink], size: 0.06 });
                        dirty = true;
                    }
                }

                // Details. Buttons that reorder or remove shots act after the frame.
                let n = board.panels.len();
                let at_s = (board.starts(start, fps)[i] - start) / fps;
                let mut shot_act: Option<&'static str> = None;
                let mut side_ui = ui.new_child(egui::UiBuilder::new().max_rect(side));
                egui::Frame::new().fill(Color32::from_rgb(34, 35, 40)).corner_radius(8).inner_margin(12).show(&mut side_ui, |ui| {
                    ui.set_width(side.width() - 24.0);
                    let p = &mut board.panels[i];
                    ui.label(RichText::new(format!("Shot {} of {n}", i + 1)).strong().size(15.0));
                    ui.label(RichText::new(format!("@{} · starts at {at_s:.1}s", p.id)).small().color(Color32::from_gray(120)));
                    ui.add_space(6.0);
                    ui.label(RichText::new("What happens").small().color(Color32::from_gray(150)));
                    let r = ui.add(egui::TextEdit::multiline(&mut p.caption).hint_text("e.g. Wide: city at dusk, title fades in").desired_rows(4).desired_width(f32::INFINITY));
                    dirty |= r.lost_focus();
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Length").small().color(Color32::from_gray(150)));
                        dirty |= ui.add(egui::DragValue::new(&mut p.seconds).speed(0.05).range(0.1..=600.0).suffix(" s")).changed();
                    });
                    if let Some(c) = &p.comp {
                        ui.label(RichText::new(format!("built as comp `{c}`")).small().color(crate::theme::ACCENT_SOFT));
                    }
                    ui.add_space(10.0);
                    if ui.button("Send shot to agent").clicked() {
                        attach.push(p.id.clone());
                    }
                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
                        if ui.small_button("Undo stroke").clicked()
                            && let Some(k) = p.strokes.iter().rposition(|s| s.color != INKS[2])
                        {
                            p.strokes.remove(k);
                            dirty = true;
                        }
                        if ui.small_button("Clear sketch").clicked() {
                            p.strokes.clear();
                            p.labels.clear();
                            dirty = true;
                        }
                    });
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        if ui.add_enabled(i > 0, egui::Button::new("◀ Move").small()).clicked() {
                            shot_act = Some("left");
                        }
                        if ui.add_enabled(i + 1 < n, egui::Button::new("Move ▶").small()).clicked() {
                            shot_act = Some("right");
                        }
                        if ui.small_button("Duplicate").clicked() {
                            shot_act = Some("dup");
                        }
                    });
                    ui.add_space(8.0);
                    if ui.add(egui::Button::new(RichText::new("Delete shot").color(Color32::from_rgb(255, 120, 110))).small()).clicked() {
                        shot_act = Some("delete");
                    }
                });
                match shot_act {
                    Some("left") => board.panels.swap(i, i - 1),
                    Some("right") => board.panels.swap(i, i + 1),
                    Some("dup") => {
                        let copy = board.panels[i].clone();
                        let id = board.add(Some(i + 1), copy.caption.clone(), copy.seconds);
                        let new = board.get_mut(&id).unwrap();
                        (new.strokes, new.labels, new.image) = (copy.strokes, copy.labels, copy.image);
                        self.selected = Some(id);
                    }
                    Some(_) => {
                        board.panels.remove(i);
                        self.selected = None;
                    }
                    None => {}
                }
                dirty |= shot_act.is_some();
            }
        }

        // ---- Strip.
        let starts = board.starts(start, fps);
        let mut strip_ui = ui.new_child(egui::UiBuilder::new().max_rect(strip));
        egui::ScrollArea::horizontal().id_salt("board-strip").show(&mut strip_ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                let ids: Vec<String> = board.panels.iter().map(|p| p.id.clone()).collect();
                for (i, id) in ids.iter().enumerate() {
                    let p = board.panels[i].clone();
                    ui.vertical(|ui| {
                        let (rect, resp) = ui.allocate_exact_size(vec2(CARD_H * aspect.min(2.4), CARD_H), egui::Sense::click());
                        self.paint(ui, &dir, &p, rect);
                        let playing = in_film && frame >= starts[i] && frame < starts[i] + (p.seconds * fps).round();
                        let (w, border) = if self.selected.as_deref() == Some(id.as_str()) {
                            (2.0, crate::theme::ACCENT)
                        } else if playing {
                            (1.5, Color32::from_rgb(255, 80, 70))
                        } else if resp.hovered() {
                            (1.0, Color32::from_gray(120))
                        } else {
                            (1.0, Color32::from_gray(50))
                        };
                        ui.painter().rect_stroke(rect, CornerRadius::same(6), Stroke::new(w, border), egui::StrokeKind::Outside);
                        let num = Rect::from_min_size(rect.left_top() + vec2(5.0, 5.0), vec2(20.0, 16.0));
                        ui.painter().rect_filled(num, CornerRadius::same(4), Color32::from_black_alpha(170));
                        ui.painter().text(num.center(), egui::Align2::CENTER_CENTER, format!("{}", i + 1), egui::FontId::proportional(11.0), Color32::WHITE);
                        ui.horizontal(|ui| {
                            ui.set_width(rect.width());
                            let cap = if p.caption.is_empty() { "untitled" } else { p.caption.lines().next().unwrap_or("") };
                            ui.add(egui::Label::new(RichText::new(cap).small().color(Color32::from_gray(170))).truncate());
                        });
                        ui.label(RichText::new(format!("{:.1}s", p.seconds)).small().color(Color32::from_gray(110)));
                        if resp.clicked() {
                            self.selected = Some(id.clone());
                            self.typing = None;
                            seek = Some(starts[i]);
                        }
                    });
                }
                // The add card.
                let (rect, resp) = ui.allocate_exact_size(vec2(CARD_H * 0.8, CARD_H), egui::Sense::click());
                let c = if resp.hovered() { Color32::from_gray(150) } else { Color32::from_gray(80) };
                ui.painter().rect_stroke(rect, CornerRadius::same(6), Stroke::new(1.0, c), egui::StrokeKind::Inside);
                ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, "＋", egui::FontId::proportional(24.0), c);
                if resp.on_hover_text("new shot after the selected one").clicked() {
                    let at = self.selected.as_ref().and_then(|id| board.index(id)).map(|i| i + 1);
                    self.selected = Some(board.add(at, String::new(), 2.0));
                }
            });
        });

        if dirty {
            board.save();
        }
        drop(board);
        for id in attach {
            panels.attach_ref(&id);
        }
        if let Some(f) = seek.filter(|_| in_film) {
            panels.shared.session.lock().frame = f;
        }
    }
}
