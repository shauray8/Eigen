//! Embedded terminal running the agent (omp), so it lives inside the app.
//! PTY via portable-pty, screen state via vt100, drawn with egui.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui::{self, Color32, FontId, text::LayoutJob};
use parking_lot::Mutex;
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};

/// The terminal's focus id, so the app can move keyboard focus to and from it.
#[allow(non_snake_case)]
pub fn TERMINAL_ID() -> egui::Id {
    egui::Id::new("agent-terminal")
}

pub struct Terminal {
    parser: Arc<Mutex<vt100::Parser>>,
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
    exited: Arc<AtomicBool>,
    size: (u16, u16),
}

impl Terminal {
    /// Runs `command` through the user's login shell (so PATH matches their terminal).
    pub fn spawn(command: &str, cwd: &Path, ctx: egui::Context) -> anyhow::Result<Self> {
        let size = (40, 120);
        let pair = native_pty_system().openpty(PtySize { rows: size.0, cols: size.1, pixel_width: 0, pixel_height: 0 })?;
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
        let mut cmd = CommandBuilder::new(shell);
        cmd.args(["-lc", command]);
        cmd.cwd(cwd);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        let mut child = pair.slave.spawn_command(cmd)?;
        drop(pair.slave);

        let parser = Arc::new(Mutex::new(vt100::Parser::new(size.0, size.1, 2000)));
        let exited = Arc::new(AtomicBool::new(false));
        let mut reader = pair.master.try_clone_reader()?;
        let (p, e, c) = (parser.clone(), exited.clone(), ctx.clone());
        std::thread::spawn(move || {
            let mut buf = [0u8; 16384];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                p.lock().process(&buf[..n]);
                c.request_repaint();
            }
            let _ = child.wait();
            e.store(true, Ordering::Relaxed);
            c.request_repaint();
        });
        let writer = pair.master.take_writer()?;
        Ok(Self { parser, writer, master: pair.master, exited, size })
    }

    pub fn exited(&self) -> bool {
        self.exited.load(Ordering::Relaxed)
    }

    /// Types `text` into the agent's prompt without sending it (as a paste,
    /// so newlines stay part of the message); the human finishes and sends.
    pub fn insert(&mut self, text: &str) {
        if self.parser.lock().screen().bracketed_paste() {
            self.send(format!("\x1b[200~{text}\x1b[201~").as_bytes());
        } else {
            self.send(text.as_bytes());
        }
    }

    /// Sends raw bytes (e.g. a literal Ctrl+B, 0x02) to the agent.
    pub fn send_raw(&mut self, bytes: &[u8]) {
        self.send(bytes);
    }

    fn send(&mut self, bytes: &[u8]) {
        let _ = self.writer.write_all(bytes);
        let _ = self.writer.flush();
    }

    fn key_bytes(&self, key: egui::Key, m: egui::Modifiers) -> Option<Vec<u8>> {
        use egui::Key::*;
        let app_cursor = self.parser.lock().screen().application_cursor();
        let arrow = |c: char| if app_cursor { format!("\x1bO{c}") } else { format!("\x1b[{c}") };
        Some(match key {
            Enter if m.shift => b"\x1b\r".to_vec(),
            Enter => b"\r".to_vec(),
            Backspace if m.alt => b"\x1b\x7f".to_vec(),
            Backspace => b"\x7f".to_vec(),
            Tab if m.shift => b"\x1b[Z".to_vec(),
            Tab => b"\t".to_vec(),
            Escape => b"\x1b".to_vec(),
            ArrowUp => arrow('A').into_bytes(),
            ArrowDown => arrow('B').into_bytes(),
            ArrowRight => arrow('C').into_bytes(),
            ArrowLeft => arrow('D').into_bytes(),
            Home => b"\x1b[H".to_vec(),
            End => b"\x1b[F".to_vec(),
            Delete => b"\x1b[3~".to_vec(),
            PageUp => b"\x1b[5~".to_vec(),
            PageDown => b"\x1b[6~".to_vec(),
            k if m.ctrl && !m.command_only() => {
                let name = k.name();
                let c = name.chars().next().filter(|_| name.len() == 1)?.to_ascii_lowercase();
                if !c.is_ascii_lowercase() {
                    return None;
                }
                vec![(c as u8) & 0x1f]
            }
            _ => return None,
        })
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let font = FontId::monospace(13.0);
        let (cw, ch) = ui.fonts_mut(|f| (f.glyph_width(&font, 'M'), f.row_height(&font)));
        let rect = ui.available_rect_before_wrap();
        let rows = ((rect.height() / ch).floor() as u16).max(2);
        let cols = ((rect.width() / cw).floor() as u16).max(10);
        if (rows, cols) != self.size {
            self.size = (rows, cols);
            let _ = self.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
            self.parser.lock().screen_mut().set_size(rows, cols);
        }

        let id = TERMINAL_ID();
        let response = ui.interact(rect, id, egui::Sense::click());
        if response.clicked() {
            ui.memory_mut(|m| m.request_focus(id));
        }
        let focused = ui.memory(|m| m.has_focus(id));
        if focused {
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    id,
                    egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true },
                )
            });
            let events = ui.input(|i| i.events.clone());
            for event in events {
                match event {
                    egui::Event::Text(t) => self.send(t.as_bytes()),
                    egui::Event::Paste(t) => {
                        if self.parser.lock().screen().bracketed_paste() {
                            self.send(format!("\x1b[200~{t}\x1b[201~").as_bytes());
                        } else {
                            self.send(t.as_bytes());
                        }
                    }
                    egui::Event::Key { key, pressed: true, modifiers, .. } => {
                        if let Some(b) = self.key_bytes(key, modifiers) {
                            self.send(&b);
                        }
                    }
                    _ => {}
                }
            }
        }

        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, Color32::from_rgb(16, 16, 18));
        let parser = self.parser.lock();
        let screen = parser.screen();
        let (srows, scols) = screen.size();
        for r in 0..srows {
            let y = rect.top() + r as f32 * ch;
            let mut job = LayoutJob::default();
            let mut c = 0;
            while c < scols {
                let Some(cell) = screen.cell(r, c) else { break };
                let (mut fg, mut bg) = (color(cell.fgcolor(), true), color(cell.bgcolor(), false));
                if cell.inverse() {
                    std::mem::swap(&mut fg, &mut bg);
                    fg = fg.or(Some(Color32::from_rgb(16, 16, 18)));
                    bg = bg.or(Some(Color32::from_gray(220)));
                }
                if let Some(bg) = bg {
                    let x = rect.left() + c as f32 * cw;
                    let w = if cell.is_wide() { 2.0 * cw } else { cw };
                    painter.rect_filled(egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(w, ch)), 0.0, bg);
                }
                let text = if cell.has_contents() { cell.contents() } else { " " };
                let mut fmt = egui::TextFormat::simple(font.clone(), fg.unwrap_or(Color32::from_gray(220)));
                if cell.bold() {
                    fmt.color = fmt.color.gamma_multiply(1.15);
                }
                job.append(text, 0.0, fmt);
                c += if cell.is_wide() { 2 } else { 1 };
            }
            let galley = ui.fonts_mut(|f| f.layout_job(job));
            painter.galley(egui::pos2(rect.left(), y), galley, Color32::WHITE);
        }
        if !screen.hide_cursor() {
            let (cr, cc) = screen.cursor_position();
            let cur = egui::Rect::from_min_size(
                egui::pos2(rect.left() + cc as f32 * cw, rect.top() + cr as f32 * ch),
                egui::vec2(cw, ch),
            );
            if focused {
                painter.rect_filled(cur, 0.0, Color32::from_white_alpha(120));
            } else {
                painter.rect_stroke(cur, 0.0, egui::Stroke::new(1.0, Color32::GRAY), egui::StrokeKind::Inside);
            }
        }
    }
}

const ANSI: [(u8, u8, u8); 16] = [
    (0, 0, 0),
    (205, 49, 49),
    (13, 188, 121),
    (229, 229, 16),
    (36, 114, 200),
    (188, 63, 188),
    (17, 168, 205),
    (229, 229, 229),
    (102, 102, 102),
    (241, 76, 76),
    (35, 209, 139),
    (245, 245, 67),
    (59, 142, 234),
    (214, 112, 214),
    (41, 184, 219),
    (255, 255, 255),
];

fn color(c: vt100::Color, _fg: bool) -> Option<Color32> {
    match c {
        vt100::Color::Default => None,
        vt100::Color::Rgb(r, g, b) => Some(Color32::from_rgb(r, g, b)),
        vt100::Color::Idx(i) if i < 16 => {
            let (r, g, b) = ANSI[i as usize];
            Some(Color32::from_rgb(r, g, b))
        }
        vt100::Color::Idx(i) if i < 232 => {
            let i = i - 16;
            let v = |x: u8| if x == 0 { 0 } else { 55 + x * 40 };
            Some(Color32::from_rgb(v(i / 36), v((i / 6) % 6), v(i % 6)))
        }
        vt100::Color::Idx(i) => Some(Color32::from_gray(8 + (i - 232) * 10)),
    }
}
