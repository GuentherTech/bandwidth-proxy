use crate::proxy::{Direction, PerDirection};
use crate::theme::direction_color;
use eframe::egui::{self, Color32, Stroke};
use std::collections::VecDeque;

const BACKGROUND: Color32 = Color32::from_rgb(14, 19, 27);
const PIXELS_PER_SECOND: f32 = 12.0;
const TICK_SECONDS: u64 = 10;
pub const SAMPLE_SECONDS: f64 = 0.5;
// Two hours of history. Older samples are dropped so the scroll width stays bounded.
pub const MAX_SAMPLES: usize = 14_400;

pub struct Sample {
    pub at: f64,
    pub kib_per_second: PerDirection<f64>,
}

pub fn format_rate(kib_per_second: f64) -> String {
    if kib_per_second >= 1024.0 {
        format!("{:.1} MiB/s", kib_per_second / 1024.0)
    } else {
        format!("{kib_per_second:.1} KiB/s")
    }
}

pub fn format_clock(seconds: f64) -> String {
    let seconds = seconds as u64;
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// Rounds up to 1, 2, or 5 times a power of ten, so graph gridlines land on round values.
fn nice_ceiling(value: f64) -> f64 {
    let magnitude = 10_f64.powf(value.log10().floor());
    [1.0, 2.0, 5.0, 10.0]
        .into_iter()
        .map(|step| step * magnitude)
        .find(|candidate| *candidate >= value)
        .unwrap_or(10.0 * magnitude)
}

pub fn push_sample(samples: &mut VecDeque<Sample>, sample: Sample) {
    if samples.len() == MAX_SAMPLES {
        samples.pop_front();
    }
    samples.push_back(sample);
}

/// Draws the traffic history. `limits` holds the active caps in KiB/s, if any.
pub fn show(
    ui: &mut egui::Ui,
    id: impl std::hash::Hash,
    samples: &VecDeque<Sample>,
    limits: Option<PerDirection<f64>>,
    running: bool,
) {
    const MARGIN: egui::Vec2 = egui::vec2(12.0, 22.0);
    let height = (ui.available_height() - 18.0).max(150.0);
    let viewport_width = ui.available_width();
    let origin = samples.front().map_or(0.0, |sample| sample.at);
    let duration = (samples.back().map_or(0.0, |sample| sample.at) - origin) as f32;
    egui::ScrollArea::horizontal()
        .scroll_source(
            egui::scroll_area::ScrollSource::SCROLL_BAR
                | egui::scroll_area::ScrollSource::MOUSE_WHEEL,
        )
        .id_salt(id)
        .auto_shrink([false, true])
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
        .stick_to_right(true)
        .show_viewport(ui, |ui, viewport| {
            let width = viewport_width.max(duration * PIXELS_PER_SECOND + 2.0 * MARGIN.x);
            let (rect, response) =
                ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
            let painter = ui.painter_at(rect);
            painter.rect_filled(rect, 2.0, BACKGROUND);
            let plot = rect.shrink2(MARGIN);
            let visible = egui::Rangef::new(
                rect.left() + viewport.left(),
                (rect.left() + viewport.right()).min(rect.right()),
            );
            let x_of = |at: f64| plot.left() + (at - origin) as f32 * PIXELS_PER_SECOND;
            let at_of = |x: f32| origin + f64::from((x - plot.left()) / PIXELS_PER_SECOND);
            let (from, to) = (at_of(visible.min), at_of(visible.max));
            let first = samples
                .partition_point(|sample| sample.at < from)
                .saturating_sub(1);
            let end = (samples.partition_point(|sample| sample.at <= to) + 1).min(samples.len());
            let shown = samples.range(first..end);

            let peak = shown
                .clone()
                .map(|sample| sample.kib_per_second)
                .chain(limits)
                .flat_map(|rates| [rates.download, rates.upload])
                .fold(0.0_f64, f64::max);
            let max = 4.0 * nice_ceiling((peak * 1.1).max(1.0) / 4.0);
            let y_of = |kib: f64| plot.bottom() - (kib / max) as f32 * plot.height();

            let label_font = egui::FontId::monospace(10.0);
            let label_color = Color32::from_gray(120);
            for step in 0..=4 {
                let value = max * f64::from(step) / 4.0;
                let y = y_of(value);
                painter.hline(visible, y, Stroke::new(1.0, Color32::from_gray(38)));
                painter.text(
                    egui::pos2(visible.min + 6.0, y - 2.0),
                    egui::Align2::LEFT_BOTTOM,
                    format_rate(value),
                    label_font.clone(),
                    label_color,
                );
            }

            if samples.is_empty() {
                let text = if running {
                    "Waiting for traffic samples..."
                } else {
                    "Start the proxy to see traffic."
                };
                painter.text(
                    egui::pos2(visible.center(), plot.center().y),
                    egui::Align2::CENTER_CENTER,
                    text,
                    egui::FontId::proportional(14.0),
                    Color32::from_gray(110),
                );
                return;
            }

            let tick = TICK_SECONDS as f64;
            let first_tick = (from.max(0.0) / tick).ceil() as u64;
            let last_tick = (to.max(0.0) / tick).floor() as u64;
            for index in first_tick..=last_tick {
                let at = (index * TICK_SECONDS) as f64;
                painter.text(
                    egui::pos2(x_of(at), rect.bottom() - 4.0),
                    egui::Align2::CENTER_BOTTOM,
                    format_clock(at),
                    label_font.clone(),
                    label_color,
                );
            }

            if let Some(limits) = limits {
                for direction in Direction::ALL {
                    let y = y_of(*limits.get(direction));
                    painter.extend(egui::Shape::dashed_line(
                        &[egui::pos2(visible.min, y), egui::pos2(visible.max, y)],
                        Stroke::new(1.0, direction_color(direction).gamma_multiply(0.7)),
                        6.0,
                        5.0,
                    ));
                }
            }

            for direction in Direction::ALL {
                let points: Vec<_> = shown
                    .clone()
                    .map(|sample| {
                        egui::pos2(x_of(sample.at), y_of(*sample.kib_per_second.get(direction)))
                    })
                    .collect();
                if points.len() > 1 {
                    painter.add(egui::Shape::line(
                        points,
                        Stroke::new(2.0, direction_color(direction)),
                    ));
                }
            }

            if let Some(pointer) = response.hover_pos() {
                let at = at_of(pointer.x);
                let after = samples.partition_point(|sample| sample.at < at);
                let sample = [after.saturating_sub(1), after.min(samples.len() - 1)]
                    .into_iter()
                    .map(|index| &samples[index])
                    .min_by(|a, b| (a.at - at).abs().total_cmp(&(b.at - at).abs()))
                    .expect("samples is not empty");
                painter.vline(
                    x_of(sample.at),
                    plot.y_range(),
                    Stroke::new(1.0, Color32::from_gray(200)),
                );
                egui::Tooltip::for_widget(&response)
                    .at_pointer()
                    .show(|ui| {
                        ui.label(format!("Session {}", format_clock(sample.at)));
                        for direction in Direction::ALL {
                            let rate = format_rate(*sample.kib_per_second.get(direction));
                            ui.colored_label(
                                direction_color(direction),
                                format!("{}: {rate}", direction.label()),
                            );
                        }
                    });
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_is_bounded() {
        let mut samples = VecDeque::new();
        for index in 0..MAX_SAMPLES + 10 {
            push_sample(
                &mut samples,
                Sample {
                    at: index as f64,
                    kib_per_second: PerDirection::default(),
                },
            );
        }
        assert_eq!(samples.len(), MAX_SAMPLES);
        assert_eq!(samples.front().unwrap().at, 10.0);
    }

    #[test]
    fn gridlines_round_up() {
        assert_eq!(nice_ceiling(3.0), 5.0);
        assert_eq!(nice_ceiling(12.0), 20.0);
        assert_eq!(format_clock(3725.0), "1:02:05");
    }
}
