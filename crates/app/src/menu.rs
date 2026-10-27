//! The native macOS menu bar (muda, as FilmCraft does): File, Edit, View,
//! Window live in the system menu bar, not inside the window. Items send
//! their ids to the app, which runs them on its next frame.

use std::sync::mpsc::{Receiver, channel};

use eframe::egui;
use muda::accelerator::Accelerator;
use muda::{AboutMetadata, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};

fn item(id: &str, label: &str, keys: Option<&str>) -> MenuItem {
    MenuItem::with_id(id, label, true, keys.and_then(|k| k.parse::<Accelerator>().ok()))
}

/// Builds the menu bar and returns the stream of chosen item ids.
pub fn install(ctx: &egui::Context) -> Receiver<String> {
    let (tx, rx) = channel();
    let ctx = ctx.clone();
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        let _ = tx.send(e.id().0.clone());
        ctx.request_repaint();
    }));

    let bar = Menu::new();
    let app = Submenu::new("Eigen", true);
    let _ = app.append_items(&[
        &PredefinedMenuItem::about(None, Some(AboutMetadata { name: Some("Eigen".into()), ..Default::default() })),
        &PredefinedMenuItem::separator(),
        &PredefinedMenuItem::hide(None),
        &PredefinedMenuItem::hide_others(None),
        &PredefinedMenuItem::show_all(None),
        &PredefinedMenuItem::separator(),
        &PredefinedMenuItem::quit(None),
    ]);

    let recent = Submenu::new("Open Recent", true);
    for dir in crate::projects::recent().into_iter().filter(|d| d.is_dir()).take(15) {
        let name = dir.file_name().map_or_else(|| dir.display().to_string(), |n| n.to_string_lossy().into_owned());
        let _ = recent.append(&item(&format!("recent:{}", dir.display()), &name, None));
    }
    let file = Submenu::new("File", true);
    let _ = file.append_items(&[
        &item("project.manager", "Project Manager…", Some("CmdOrCtrl+Shift+O")),
        &recent,
        &PredefinedMenuItem::separator(),
        &item("file.save", "Save", Some("CmdOrCtrl+S")),
        &item("file.import", "Import Media…", Some("CmdOrCtrl+I")),
        &item("file.export", "Export MP4", Some("CmdOrCtrl+E")),
        &PredefinedMenuItem::separator(),
        &item("file.finder", "Show Project in Finder", None),
    ]);

    // Undo/Redo/Delete stay in-app shortcuts (no key equivalents here), so
    // ⌘Z in a text field or the agent's terminal still edits that text.
    let edit = Submenu::new("Edit", true);
    let _ = edit.append_items(&[
        &item("edit.undo", "Undo  (⌘Z)", None),
        &item("edit.redo", "Redo  (⌘⇧Z)", None),
        &PredefinedMenuItem::separator(),
        &item("edit.delete", "Delete Selected  (⌫)", None),
        &item("edit.clearMarks", "Clear In/Out  (⌥X)", None),
    ]);

    let view = Submenu::new("View", true);
    let _ = view.append_items(&[
        &item("page.edit", "Edit Page", Some("CmdOrCtrl+1")),
        &item("page.storyboard", "Storyboard Page", Some("CmdOrCtrl+2")),
        &item("page.moodboard", "Moodboard Page", Some("CmdOrCtrl+3")),
        &item("page.references", "References Page", Some("CmdOrCtrl+4")),
        &item("page.extensions", "Plugins & Skills Page", Some("CmdOrCtrl+5")),
        &PredefinedMenuItem::separator(),
        &item("view.canvas", "Canvas", None),
        &item("view.world", "3D World", None),
        &item("view.film", "Back to Film  (Esc)", None),
    ]);

    let agent = Submenu::new("Agent", true);
    let _ = agent.append_items(&[&item("agent.restart", "Restart Agent", None)]);

    let window = Submenu::new("Window", true);
    let _ = window.append_items(&[&PredefinedMenuItem::minimize(None), &PredefinedMenuItem::fullscreen(None)]);

    let _ = bar.append_items(&[&app, &file, &edit, &view, &agent, &window]);
    #[cfg(target_os = "macos")]
    {
        bar.init_for_nsapp();
        window.set_as_windows_menu_for_nsapp();
    }
    // The menu must outlive the app; it is the process's menu bar from now on.
    std::mem::forget(bar);
    rx
}
