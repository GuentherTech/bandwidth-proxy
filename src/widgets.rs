use crate::session::Status;
use crate::theme::{BORDER, BUTTON_TEXT, CARD, DANGER, status_color};
use eframe::egui::{self, Color32, RichText, Stroke};

pub fn section_heading(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).size(16.0).strong());
}

pub fn card(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(CARD)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(2.0)
        .inner_margin(12.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_min_height(180.0);
            add_contents(ui);
        });
}

pub fn primary_button(
    ui: &mut egui::Ui,
    text: &str,
    fill: Color32,
    enabled: bool,
) -> egui::Response {
    let label = RichText::new(text).strong().color(BUTTON_TEXT);
    ui.add_enabled(
        enabled,
        egui::Button::new(label)
            .fill(fill)
            .min_size(egui::vec2(112.0, 30.0)),
    )
}

pub fn status_badge(ui: &mut egui::Ui, status: Status) {
    let color = status_color(status);
    let galley = ui.painter().layout_no_wrap(
        status.label().to_owned(),
        egui::FontId::proportional(13.0),
        color,
    );
    let size = egui::vec2(galley.size().x + 32.0, 26.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
    response
        .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, status.label()));
    let painter = ui.painter();
    painter.rect(
        rect,
        13.0,
        color.gamma_multiply(0.14),
        Stroke::new(1.0, color.gamma_multiply(0.6)),
        egui::StrokeKind::Inside,
    );
    painter.circle_filled(egui::pos2(rect.left() + 14.0, rect.center().y), 4.0, color);
    let text_top = rect.center().y - galley.size().y / 2.0;
    painter.galley(egui::pos2(rect.left() + 24.0, text_top), galley, color);
}

pub fn legend_swatch(ui: &mut egui::Ui, color: Color32, dashed: bool) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(16.0, 10.0), egui::Sense::hover());
    let line = [rect.left_center(), rect.right_center()];
    if dashed {
        ui.painter().extend(egui::Shape::dashed_line(
            &line,
            Stroke::new(1.5, color),
            4.0,
            3.0,
        ));
    } else {
        ui.painter().line_segment(line, Stroke::new(2.0, color));
    }
}

pub fn profile_row(ui: &mut egui::Ui, name: &str, selected: bool) -> bool {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 26.0), egui::Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::SelectableLabel,
            ui.is_enabled(),
            selected,
            name,
        )
    });
    if selected || response.has_focus() {
        ui.painter()
            .rect_filled(rect, 0.0, ui.visuals().selection.bg_fill);
    } else if response.hovered() {
        ui.painter()
            .rect_filled(rect, 0.0, ui.visuals().widgets.hovered.bg_fill);
    }
    let text_rect = rect.shrink2(egui::vec2(8.0, 0.0));
    let text_color = if selected {
        ui.visuals().selection.stroke.color
    } else {
        ui.visuals().text_color()
    };
    ui.painter()
        .with_clip_rect(text_rect.intersect(ui.clip_rect()))
        .text(
            egui::pos2(text_rect.left(), rect.center().y),
            egui::Align2::LEFT_CENTER,
            name,
            egui::TextStyle::Body.resolve(ui.style()),
            text_color,
        );
    let activate = response.clicked()
        || (response.has_focus()
            && ui.input(|input| {
                input.key_pressed(egui::Key::Enter) || input.key_pressed(egui::Key::Space)
            }));
    if activate {
        response.request_focus();
    }
    activate
}

/// Returns `Some(true)` to confirm, `Some(false)` to cancel, or `None` while open.
pub fn confirm_dialog(ctx: &egui::Context, title: &str, body: &str, confirm: &str) -> Option<bool> {
    let mut choice = None;
    let response = egui::Modal::new(egui::Id::new("confirm")).show(ctx, |ui| {
        ui.set_max_width(380.0);
        ui.heading(title);
        ui.add_space(4.0);
        ui.label(body);
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if primary_button(ui, confirm, DANGER, true).clicked() {
                choice = Some(true);
            }
            if ui.button("Cancel").clicked() {
                choice = Some(false);
            }
        });
    });
    if choice.is_none() && response.should_close() {
        choice = Some(false);
    }
    choice
}
