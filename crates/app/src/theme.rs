//! The look: the system UI font, roomier spacing, a dark neutral palette
//! with one warm accent, rounded corners.

use eframe::egui::{self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, Stroke, Visuals};

pub const ACCENT: Color32 = Color32::from_rgb(255, 154, 61);
pub const ACCENT_SOFT: Color32 = Color32::from_rgb(140, 180, 255);
const BG: Color32 = Color32::from_rgb(22, 23, 26);
const PANEL: Color32 = Color32::from_rgb(28, 29, 33);
/// The status line at the bottom.
pub const BAR: Color32 = Color32::from_rgb(17, 18, 20);
const RAISED: Color32 = Color32::from_rgb(38, 40, 45);

/// The macOS system font, when it can be read; egui's own font otherwise.
fn fonts() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    let candidates = ["/System/Library/Fonts/SFNS.ttf", "/System/Library/Fonts/HelveticaNeue.ttc", "/System/Library/Fonts/Helvetica.ttc"];
    if let Some(bytes) = candidates.iter().find_map(|p| std::fs::read(p).ok()) {
        fonts.font_data.insert("system".into(), FontData::from_owned(bytes).into());
        fonts.families.get_mut(&FontFamily::Proportional).unwrap().insert(0, "system".into());
    }
    if let Ok(bytes) = std::fs::read("/System/Library/Fonts/SFNSMono.ttf").or_else(|_| std::fs::read("/System/Library/Fonts/Menlo.ttc")) {
        fonts.font_data.insert("mono".into(), FontData::from_owned(bytes).into());
        fonts.families.get_mut(&FontFamily::Monospace).unwrap().insert(0, "mono".into());
    }
    fonts
}

pub fn apply(ctx: &egui::Context) {
    ctx.set_fonts(fonts());
    let mut v = Visuals::dark();
    v.panel_fill = PANEL;
    v.window_fill = PANEL;
    v.extreme_bg_color = BG;
    v.faint_bg_color = Color32::from_rgb(32, 33, 37);
    v.window_corner_radius = CornerRadius::same(10);
    v.menu_corner_radius = CornerRadius::same(8);
    v.window_stroke = Stroke::new(1.0, Color32::from_gray(48));
    v.selection.bg_fill = ACCENT.gamma_multiply(0.45);
    v.selection.stroke = Stroke::new(1.0, ACCENT);
    v.hyperlink_color = ACCENT_SOFT;
    for (w, fill) in [
        (&mut v.widgets.noninteractive, PANEL),
        (&mut v.widgets.inactive, RAISED),
        (&mut v.widgets.hovered, Color32::from_rgb(50, 52, 58)),
        (&mut v.widgets.active, Color32::from_rgb(60, 62, 70)),
        (&mut v.widgets.open, RAISED),
    ] {
        w.corner_radius = CornerRadius::same(6);
        w.bg_fill = fill;
        w.weak_bg_fill = fill;
    }
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, Color32::from_gray(40));
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_gray(80));
    ctx.set_visuals(v);
    ctx.all_styles_mut(|s| {
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.spacing.button_padding = egui::vec2(10.0, 4.0);
        s.spacing.interact_size.y = 22.0;
        s.spacing.window_margin = egui::Margin::same(12);
        s.spacing.menu_margin = egui::Margin::same(8);
        for (style, size) in [
            (egui::TextStyle::Body, 13.5),
            (egui::TextStyle::Button, 13.5),
            (egui::TextStyle::Small, 11.0),
            (egui::TextStyle::Heading, 18.0),
            (egui::TextStyle::Monospace, 12.5),
        ] {
            if let Some(f) = s.text_styles.get_mut(&style) {
                f.size = size;
            }
        }
    });
}

/// Dock tabs that match the theme: quiet tab bar, accent underline on the focused tab.
pub fn dock_style(style: &egui::Style) -> egui_dock::Style {
    let mut d = egui_dock::Style::from_egui(style);
    d.tab_bar.bg_fill = BG;
    d.tab_bar.height = 28.0;
    d.tab_bar.hline_color = Color32::from_gray(40);
    d.tab.active.bg_fill = PANEL;
    d.tab.inactive.bg_fill = BG;
    d.tab.focused.bg_fill = PANEL;
    d.tab.active.text_color = Color32::from_gray(230);
    d.tab.inactive.text_color = Color32::from_gray(140);
    d.tab.focused.text_color = Color32::WHITE;
    d.tab.tab_body.bg_fill = PANEL;
    d.separator.width = 3.0;
    d.separator.color_idle = BG;
    d.separator.color_hovered = ACCENT.gamma_multiply(0.6);
    d.separator.color_dragged = ACCENT;
    d
}
