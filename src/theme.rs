use crate::proxy::Direction;
use crate::session::Status;
use eframe::egui::{self, Color32};

pub const DOWNLOAD: Color32 = Color32::from_rgb(71, 211, 181);
pub const UPLOAD: Color32 = Color32::from_rgb(126, 164, 255);
pub const WARNING: Color32 = Color32::from_rgb(240, 196, 90);
pub const DANGER: Color32 = Color32::from_rgb(235, 110, 110);
pub const CARD: Color32 = Color32::from_rgb(24, 30, 41);
pub const BORDER: Color32 = Color32::from_rgb(40, 48, 62);
pub const BUTTON_TEXT: Color32 = Color32::from_rgb(10, 18, 22);

pub fn direction_color(direction: Direction) -> Color32 {
    match direction {
        Direction::Download => DOWNLOAD,
        Direction::Upload => UPLOAD,
    }
}

pub fn status_color(status: Status) -> Color32 {
    match status {
        Status::Offline => Color32::from_gray(140),
        Status::Starting | Status::Stopping => WARNING,
        Status::Listening => DOWNLOAD,
    }
}

pub fn apply_style(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = Color32::from_rgb(19, 24, 33);
    visuals.window_fill = Color32::from_rgb(26, 32, 43);
    visuals.selection.bg_fill = Color32::from_rgb(30, 100, 90);
    ctx.set_visuals(visuals);
    ctx.style_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(8.0, 3.0);
        style.spacing.interact_size.y = 22.0;
        style.spacing.scroll = egui::style::ScrollStyle::solid();
        for widget in [
            &mut style.visuals.widgets.inactive,
            &mut style.visuals.widgets.hovered,
            &mut style.visuals.widgets.active,
            &mut style.visuals.widgets.noninteractive,
        ] {
            widget.corner_radius = egui::CornerRadius::same(2);
        }
    });
}
