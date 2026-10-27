//! tmux-style keys: press the prefix (Ctrl+A), then one key. The prefix and
//! the key after it are taken out of the input, so the agent's terminal never
//! sees them; Ctrl+A twice sends a literal Ctrl+A to the terminal.
//!
//! | after Ctrl+A | does |
//! |---|---|
//! | 1-5 | page (Edit, Storyboard, Moodboard, References, Plugins & Skills) |
//! | n / p | next / previous page |
//! | ←↑↓→ or h j k l | focus the pane in that direction (agent included) |
//! | o | cycle focus through the panes |
//! | a | focus the agent |
//! | z | zoom the focused pane to the whole window (again: back) |
//! | s | save |
//! | w | Project Manager |
//! | ? | these keys |

use std::time::{Duration, Instant};

use eframe::egui::{self, Event, Key, Modifiers};

const ARMED_FOR: Duration = Duration::from_millis(2500);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cmd {
    Page(usize),
    NextPage,
    PrevPage,
    Focus(Dir),
    Cycle,
    Agent,
    Zoom,
    Save,
    Manager,
    Help,
    /// Ctrl+A Ctrl+A: the terminal gets one Ctrl+A.
    Literal,
}

pub const HELP: &[(&str, &str)] = &[
    ("1 – 5", "pages: Edit, Storyboard, Moodboard, References, Plugins & Skills"),
    ("n  p", "next / previous page"),
    ("← ↑ ↓ →  h j k l", "move focus between panes (the agent too)"),
    ("o", "cycle focus through the panes"),
    ("a", "focus the agent"),
    ("z", "zoom the focused pane (again to unzoom)"),
    ("s", "save"),
    ("w", "Project Manager"),
    ("Ctrl+A", "send Ctrl+A to the agent's terminal"),
    ("?", "this list"),
];

#[derive(Default)]
pub struct Prefix {
    armed: Option<Instant>,
}

impl Prefix {
    pub fn armed(&self) -> bool {
        self.armed.is_some_and(|t| t.elapsed() < ARMED_FOR)
    }

    /// Reads (and removes) the prefix and its key from this frame's input.
    pub fn input(&mut self, ctx: &egui::Context) -> Option<Cmd> {
        let mut cmd = None;
        ctx.input_mut(|i| {
            let mut keep = Vec::with_capacity(i.events.len());
            // The text a command key types arrives after it in the same frame.
            let mut swallow_text = false;
            for ev in std::mem::take(&mut i.events) {
                let key = match &ev {
                    Event::Key { key, pressed: true, modifiers, .. } => Some((*key, *modifiers)),
                    _ => None,
                };
                if self.armed() {
                    match key {
                        Some((Key::A, m)) if m.ctrl => {
                            cmd = Some(Cmd::Literal);
                            self.armed = None;
                        }
                        Some((k, m)) => {
                            cmd = command(k, m);
                            self.armed = None;
                            swallow_text = true;
                        }
                        None if matches!(ev, Event::Text(_)) => {}
                        None => keep.push(ev),
                    }
                    continue;
                }
                match key {
                    Some((Key::A, m)) if m.ctrl && !m.alt && !m.shift => self.armed = Some(Instant::now()),
                    None if swallow_text && matches!(ev, Event::Text(_)) => swallow_text = false,
                    _ => keep.push(ev),
                }
            }
            i.events = keep;
        });
        if self.armed() {
            ctx.request_repaint_after(ARMED_FOR);
        }
        cmd
    }
}

fn command(k: Key, m: Modifiers) -> Option<Cmd> {
    Some(match k {
        Key::Num1 => Cmd::Page(0),
        Key::Num2 => Cmd::Page(1),
        Key::Num3 => Cmd::Page(2),
        Key::Num4 => Cmd::Page(3),
        Key::Num5 => Cmd::Page(4),
        Key::N => Cmd::NextPage,
        Key::P => Cmd::PrevPage,
        Key::ArrowLeft | Key::H => Cmd::Focus(Dir::Left),
        Key::ArrowRight | Key::L => Cmd::Focus(Dir::Right),
        Key::ArrowUp | Key::K => Cmd::Focus(Dir::Up),
        Key::ArrowDown | Key::J => Cmd::Focus(Dir::Down),
        Key::O => Cmd::Cycle,
        Key::A => Cmd::Agent,
        Key::Z => Cmd::Zoom,
        Key::S => Cmd::Save,
        Key::W => Cmd::Manager,
        Key::Slash if m.shift => Cmd::Help,
        Key::Questionmark => Cmd::Help,
        _ => return None,
    })
}

/// The pane nearest to `from` in direction `dir`, among `panes` (index, rect).
pub fn neighbour(from: egui::Rect, dir: Dir, panes: &[(usize, egui::Rect)]) -> Option<usize> {
    let c = from.center();
    panes
        .iter()
        .filter_map(|(i, r)| {
            let d = r.center() - c;
            let (along, across) = match dir {
                Dir::Left => (-d.x, d.y),
                Dir::Right => (d.x, d.y),
                Dir::Up => (-d.y, d.x),
                Dir::Down => (d.y, d.x),
            };
            (along > 1.0).then_some((*i, along + across.abs() * 2.0))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::{Rect, pos2};

    #[test]
    fn neighbour_prefers_the_pane_in_line() {
        let r = |x0, y0, x1, y1| Rect::from_min_max(pos2(x0, y0), pos2(x1, y1));
        // viewer top-left, source top-right, timeline below both, agent far right
        let panes = [(0, r(0., 0., 100., 50.)), (1, r(100., 0., 200., 50.)), (2, r(0., 50., 200., 100.)), (3, r(200., 0., 300., 100.))];
        assert_eq!(neighbour(panes[0].1, Dir::Right, &panes), Some(1));
        assert_eq!(neighbour(panes[0].1, Dir::Down, &panes), Some(2));
        assert_eq!(neighbour(panes[1].1, Dir::Right, &panes), Some(3));
        assert_eq!(neighbour(panes[2].1, Dir::Up, &panes), Some(0));
        assert_eq!(neighbour(panes[0].1, Dir::Left, &panes), None);
    }

    fn key(key: Key, modifiers: Modifiers) -> Event {
        Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers }
    }

    /// Feeds one frame of events; returns the command and what the rest of the app still sees.
    fn frame(ctx: &egui::Context, p: &mut Prefix, events: Vec<Event>) -> (Option<Cmd>, usize) {
        let mut out = (None, 0);
        let mut output = ctx.run_ui(egui::RawInput { events, ..Default::default() }, |ui| {
            out.0 = p.input(ui.ctx());
            out.1 = ui.input(|i| i.events.len());
        });
        output.textures_delta.clear();
        out
    }

    #[test]
    fn prefix_then_key_is_a_command_the_terminal_never_sees() {
        let ctx = egui::Context::default();
        let mut p = Prefix::default();
        assert_eq!(frame(&ctx, &mut p, vec![key(Key::A, Modifiers::CTRL)]), (None, 0));
        assert!(p.armed());
        let (cmd, left) = frame(&ctx, &mut p, vec![key(Key::Num2, Modifiers::NONE), Event::Text("2".into())]);
        assert_eq!((cmd, left), (Some(Cmd::Page(1)), 0));
        assert!(!p.armed());
        // Unarmed keys pass through untouched.
        assert_eq!(frame(&ctx, &mut p, vec![key(Key::Num2, Modifiers::NONE), Event::Text("2".into())]), (None, 2));
        // Ctrl+A twice: a literal Ctrl+A for the terminal.
        frame(&ctx, &mut p, vec![key(Key::A, Modifiers::CTRL)]);
        assert_eq!(frame(&ctx, &mut p, vec![key(Key::A, Modifiers::CTRL)]).0, Some(Cmd::Literal));
    }
}
