#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod editor;
mod graph;
mod proxy;
mod session;
mod settings;
mod theme;
mod widgets;

use editor::{Choice, Editor, Invalid};
use eframe::egui::{self, Color32, RichText};
use graph::{format_clock, format_rate};
use proxy::{Direction, Level, PerDirection};
use session::{Session, Status};
use settings::{PORT_ERROR, Settings, parse_port, parse_rate};
use std::{
    sync::atomic::Ordering,
    time::{Duration, Instant},
};
use theme::{DANGER, DOWNLOAD, WARNING, apply_style, direction_color};
use widgets::{
    card, confirm_dialog, legend_swatch, primary_button, profile_row, section_heading, status_badge,
};

const PRESETS: [u64; 8] = [1, 4, 16, 64, 128, 512, 1024, 4096];
const NOTICE_SECONDS: u64 = 5;
const DISCONNECT_WARNING: &str = "This disconnects clients and may interrupt a save or \
    transaction. To remove throttling without disconnecting, turn off Limit traffic instead.";

fn tint(text: RichText, level: Level) -> RichText {
    match level {
        Level::Info => text,
        Level::Error => text.color(DANGER),
    }
}

enum Modal {
    None,
    Stop,
    Close,
    Delete(String),
    Discard(Choice),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Place {
    Header,
    Profiles,
    Connection,
    Bandwidth,
}

struct Notice {
    place: Place,
    text: String,
    error: bool,
    shown_at: Instant,
}

struct App {
    editor: Editor,
    // The current or most recent run, kept after it stops so its log and graph stay visible.
    session: Option<Session>,
    modal: Modal,
    allow_close: bool,
    notice: Option<Notice>,
    title: String,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        apply_style(&cc.egui_ctx);
        let (settings, problem) = Settings::load();
        let mut app = Self {
            editor: Editor::new(settings),
            session: None,
            modal: Modal::None,
            allow_close: false,
            notice: None,
            title: String::new(),
        };
        if let Some(problem) = problem {
            app.notify(Place::Profiles, true, problem);
        }
        app
    }

    fn running(&self) -> bool {
        self.session.as_ref().is_some_and(Session::running)
    }

    fn status(&self) -> Status {
        self.session
            .as_ref()
            .map_or(Status::Offline, Session::status)
    }

    fn notify(&mut self, place: Place, error: bool, text: impl Into<String>) {
        self.notice = Some(Notice {
            place,
            text: text.into(),
            error,
            shown_at: Instant::now(),
        });
    }

    fn clear_notice(&mut self, place: Place) {
        if self
            .notice
            .as_ref()
            .is_some_and(|notice| notice.place == place)
        {
            self.notice = None;
        }
    }

    fn report(&mut self, invalid: Invalid) {
        match invalid {
            Invalid::Rates(text) => self.notify(Place::Bandwidth, true, text),
            Invalid::Profile(text) => self.notify(Place::Header, true, text),
        }
    }

    /// Clears confirmations after a few seconds. Errors stay until replaced.
    fn expire_notice(&mut self, ctx: &egui::Context) {
        let Some(notice) = self.notice.as_ref().filter(|notice| !notice.error) else {
            return;
        };
        let lifetime = Duration::from_secs(NOTICE_SECONDS);
        match lifetime.checked_sub(notice.shown_at.elapsed()) {
            Some(remaining) => ctx.request_repaint_after(remaining),
            None => self.notice = None,
        }
    }

    fn rates_applied(&mut self, result: Result<(), String>) {
        match result {
            Ok(()) => self.clear_notice(Place::Bandwidth),
            Err(text) => self.notify(Place::Bandwidth, true, text),
        }
    }

    fn request_open(&mut self, choice: Choice) {
        if self.editor.has_unsaved_changes() {
            self.modal = Modal::Discard(choice);
        } else {
            self.open_profile(&choice);
        }
    }

    fn open_profile(&mut self, choice: &Choice) {
        self.editor.open(choice);
        if self
            .notice
            .as_ref()
            .is_some_and(|notice| notice.place != Place::Profiles)
        {
            self.notice = None;
        }
    }

    fn start(&mut self) {
        match self.editor.commit() {
            Ok(profile) => {
                self.notice = None;
                let limits = self.editor.limits();
                self.session = Some(Session::start(profile.local_port, profile.target(), limits));
            }
            Err(invalid) => self.report(invalid),
        }
    }

    fn stop(&mut self) {
        if let Some(session) = &mut self.session {
            session.stop();
        }
    }

    fn request_stop(&mut self) {
        let connections = self.session.as_ref().map_or(0, |session| {
            session.stats.connections.load(Ordering::Relaxed)
        });
        if connections > 0 {
            self.modal = Modal::Stop;
        } else {
            self.stop();
        }
    }

    fn save_profile(&mut self) {
        match self.editor.save() {
            Ok(name) => self.notify(Place::Header, false, format!("Saved \"{name}\".")),
            Err(invalid) => self.report(invalid),
        }
    }

    fn delete_profile(&mut self, name: &str) {
        match self.editor.delete(name) {
            Ok(()) => {
                let text = format!("Deleted \"{name}\". Its values stay in the form, unsaved.");
                self.notify(Place::Profiles, false, text);
            }
            Err(error) => {
                let text = format!("Could not delete \"{name}\": {error}");
                self.notify(Place::Profiles, true, text);
            }
        }
    }

    fn update_title(&mut self, ctx: &egui::Context) {
        let title = match self.status() {
            Status::Offline => "Bandwidth Proxy".to_owned(),
            status => format!("{} - Bandwidth Proxy", status.label()),
        };
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }
    }

    fn show_notice(&self, ui: &mut egui::Ui, place: Place) {
        if let Some(notice) = self.notice.as_ref().filter(|notice| notice.place == place) {
            let text = RichText::new(&notice.text);
            ui.label(if notice.error {
                text.color(DANGER)
            } else {
                text.weak()
            });
        }
    }

    fn header(&mut self, ui: &mut egui::Ui) {
        let status = self.status();
        let running = self.running();
        let unsaved = self.editor.has_unsaved_changes();
        let saved = self.editor.selected().is_some();
        let (save, toggle) = egui::Sides::new().height(32.0).show(
            ui,
            |ui| {
                ui.add_enabled(
                    !running,
                    egui::TextEdit::singleline(&mut self.editor.form.name)
                        .hint_text("Profile name")
                        .font(egui::FontId::proportional(20.0))
                        .desired_width(260.0),
                );
                let save = ui.button("Save profile").clicked();
                if !saved {
                    ui.weak("Not saved yet");
                } else if unsaved {
                    ui.colored_label(WARNING, "Unsaved changes");
                }
                save
            },
            |ui| {
                let toggle = if running {
                    primary_button(ui, "Stop proxy", DANGER, status != Status::Stopping)
                } else {
                    primary_button(ui, "Start proxy", DOWNLOAD, true)
                }
                .clicked();
                status_badge(ui, status);
                toggle
            },
        );
        if save {
            self.save_profile();
        }
        if toggle {
            if running {
                self.request_stop();
            } else {
                self.start();
            }
        }
        self.show_notice(ui, Place::Header);
    }

    fn profiles_panel(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        section_heading(ui, "Profiles");
        let running = self.running();
        let profiles = self.editor.profiles();
        let mut chosen = None;
        let mut delete = false;
        ui.add_enabled_ui(!running, |ui| {
            egui::Frame::group(ui.style())
                .inner_margin(2.0)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("profile_list")
                        .scroll_source(
                            egui::scroll_area::ScrollSource::SCROLL_BAR
                                | egui::scroll_area::ScrollSource::MOUSE_WHEEL,
                        )
                        .auto_shrink([false, false])
                        .max_height((profiles.len() as f32 * 26.0).clamp(104.0, 260.0))
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            for profile in profiles {
                                let selected = self.editor.selected() == Some(&*profile.name);
                                if profile_row(ui, &profile.name, selected) {
                                    chosen = Some(Choice::Saved(profile.name.clone()));
                                }
                            }
                            if profiles.is_empty() {
                                ui.weak("No saved profiles");
                            }
                        });
                });
            ui.horizontal(|ui| {
                if ui.button("+ New").clicked() {
                    chosen = Some(Choice::New);
                }
                delete = ui
                    .add_enabled(
                        self.editor.selected().is_some(),
                        egui::Button::new("Delete..."),
                    )
                    .clicked();
            });
        });
        if running {
            ui.weak("Stop the proxy to switch profiles.");
        }
        if delete && let Some(name) = self.editor.selected() {
            self.modal = Modal::Delete(name.to_owned());
        }
        if let Some(choice) = chosen {
            self.request_open(choice);
        }
        self.show_notice(ui, Place::Profiles);
    }

    fn connection_section(&mut self, ui: &mut egui::Ui) {
        section_heading(ui, "Connection");
        let editor = &mut self.editor;
        ui.add_enabled_ui(!self.session.as_ref().is_some_and(Session::running), |ui| {
            egui::Grid::new("endpoint")
                .num_columns(2)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    ui.label("Destination host");
                    ui.add(egui::TextEdit::singleline(&mut editor.form.host).desired_width(180.0));
                    ui.end_row();
                    ui.label("Destination port");
                    ui.add(
                        egui::TextEdit::singleline(&mut editor.form.upstream_port)
                            .desired_width(80.0),
                    );
                    ui.end_row();
                    ui.label("Local port");
                    ui.add(
                        egui::TextEdit::singleline(&mut editor.form.local_port).desired_width(80.0),
                    );
                    ui.end_row();
                });
        });
        ui.add_space(6.0);
        let local_port = parse_port(&editor.form.local_port);
        let mut copied = None;
        ui.horizontal(|ui| {
            ui.add_enabled_ui(local_port.is_some(), |ui| {
                ui.menu_button("Copy endpoint", |ui| {
                    let Some(port) = local_port else { return };
                    egui::Grid::new("copy_endpoint")
                        .num_columns(2)
                        .show(ui, |ui| {
                            for (kind, text) in [
                                ("Address", format!("127.0.0.1:{port}")),
                                ("SQL Server", format!("tcp:127.0.0.1,{port}")),
                            ] {
                                ui.weak(kind);
                                if ui.button(&text).clicked() {
                                    ui.ctx().copy_text(text.clone());
                                    copied = Some(text);
                                }
                                ui.end_row();
                            }
                        });
                })
                .response
                .on_disabled_hover_text(PORT_ERROR);
            });
            ui.weak(format!(
                "127.0.0.1:{} -> {}:{}",
                editor.form.local_port.trim(),
                editor.form.host.trim(),
                editor.form.upstream_port.trim()
            ));
        });
        if let Some(text) = copied {
            self.notify(Place::Connection, false, format!("Copied {text}"));
        }
        self.show_notice(ui, Place::Connection);
    }

    fn rate_row(&mut self, ui: &mut egui::Ui, direction: Direction) {
        let active = *self.editor.rates().get(direction);
        let text = self.editor.form.rate_mut(direction);
        ui.label(direction.label());
        let response = ui.add(egui::TextEdit::singleline(text).desired_width(80.0));
        ui.label("KiB/s");
        let shown = parse_rate(text).unwrap_or(active);
        let megabits = shown as f64 * 1024.0 * 8.0 / 1_000_000.0;
        ui.weak(format!("{megabits:.2} Mbit/s"));
        ui.end_row();
        if response.lost_focus() {
            let result = self.editor.commit_rates();
            self.rates_applied(result);
        }
    }

    fn bandwidth_section(&mut self, ui: &mut egui::Ui) {
        egui::Sides::new().show(
            ui,
            |ui| section_heading(ui, "Bandwidth"),
            |ui| ui.checkbox(&mut self.editor.form.limited, "Limit traffic"),
        );
        let limited = self.editor.form.limited;
        ui.scope(|ui| {
            if !limited {
                ui.multiply_opacity(0.5);
            }
            egui::Grid::new("rates")
                .num_columns(4)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    for direction in Direction::ALL {
                        self.rate_row(ui, direction);
                    }
                });
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.label("Presets");
                for rate in PRESETS {
                    let rates = self.editor.rates();
                    let selected = rates.download == rate && rates.upload == rate;
                    let button = egui::Button::new(rate.to_string()).selected(selected);
                    if ui
                        .add(button)
                        .on_hover_text(format!("{rate} KiB/s in both directions"))
                        .clicked()
                    {
                        let result = self.editor.apply_preset(rate);
                        self.rates_applied(result);
                    }
                }
            });
        });
        if self.editor.rates_pending() {
            ui.colored_label(WARNING, "Press Enter to apply.");
        } else if !limited {
            ui.weak("Limits are off. Traffic passes unrestricted.");
        }
        self.show_notice(ui, Place::Bandwidth);
    }

    fn traffic_section(&self, ui: &mut egui::Ui) {
        let session = self.session.as_ref();
        let latest = session
            .and_then(|session| session.samples.back())
            .map(|sample| sample.kib_per_second)
            .unwrap_or_default();
        let totals = session.map(|session| session.totals).unwrap_or_default();
        let connections = session.map_or(0, |session| {
            session.stats.connections.load(Ordering::Relaxed)
        });
        let limited = self.editor.form.limited;
        ui.horizontal(|ui| {
            section_heading(ui, "Traffic");
            ui.add_space(8.0);
            for direction in Direction::ALL {
                legend_swatch(ui, direction_color(direction), false);
                let text = format!(
                    "{} {}",
                    direction.label(),
                    format_rate(*latest.get(direction))
                );
                ui.colored_label(direction_color(direction), text);
                ui.add_space(6.0);
            }
            if limited {
                legend_swatch(ui, Color32::from_gray(150), true);
                ui.weak("Limit");
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.weak(format!(
                    "{:.2} MiB down, {:.2} MiB up",
                    totals.download as f64 / 1_048_576.0,
                    totals.upload as f64 / 1_048_576.0
                ));
                let plural = if connections == 1 { "" } else { "s" };
                ui.label(format!("{connections} connection{plural}"));
            });
        });
        let rates = self.editor.rates();
        let limits = limited.then_some(PerDirection {
            download: rates.download as f64,
            upload: rates.upload as f64,
        });
        let empty = Default::default();
        graph::show(
            ui,
            ("traffic_history", session.map(|session| session.started)),
            session.map_or(&empty, |session| &session.samples),
            limits,
            self.running(),
        );
    }

    fn log_panel(&self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        let Some(session) = &self.session else {
            ui.weak("Start the proxy to begin forwarding.");
            ui.add_space(4.0);
            return;
        };
        if let Some(status) = session.stats.status() {
            ui.label(tint(RichText::new(status.text), status.level));
        }
        let events = session.stats.events();
        egui::CollapsingHeader::new(format!("Session events ({})", events.len()))
            .id_salt("session_events")
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(140.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for event in events.iter().rev() {
                            let at = event.at.saturating_duration_since(session.started);
                            let text =
                                format!("{}  {}", format_clock(at.as_secs_f64()), event.text);
                            ui.label(tint(RichText::new(text).small(), event.level));
                        }
                    });
            });
        ui.add_space(4.0);
    }

    fn show_modal(&mut self, ctx: &egui::Context) {
        let modal = std::mem::replace(&mut self.modal, Modal::None);
        let (title, body, confirm) = match &modal {
            Modal::None => return,
            Modal::Stop => (
                "Stop this proxy?",
                DISCONNECT_WARNING.to_owned(),
                "Disconnect and stop",
            ),
            Modal::Close => (
                "Close Bandwidth Proxy?",
                DISCONNECT_WARNING.to_owned(),
                "Disconnect and close",
            ),
            Modal::Delete(name) => (
                "Delete saved profile?",
                format!("Delete \"{name}\"? This cannot be undone."),
                "Delete",
            ),
            Modal::Discard(choice) => (
                "Discard unsaved changes?",
                format!(
                    "Your edits to \"{}\" are not saved. Opening \"{}\" discards them.",
                    self.editor.form.name,
                    choice.name()
                ),
                "Discard changes",
            ),
        };
        match confirm_dialog(ctx, title, &body, confirm) {
            None => self.modal = modal,
            Some(false) => {}
            Some(true) => match modal {
                Modal::None => {}
                Modal::Stop => self.stop(),
                Modal::Close => {
                    self.stop();
                    self.allow_close = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                Modal::Delete(name) => self.delete_profile(&name),
                Modal::Discard(choice) => self.open_profile(&choice),
            },
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(session) = &mut self.session {
            session.reap();
            session.sample();
        }
        if ctx.input(|input| input.viewport().close_requested())
            && self.running()
            && !self.allow_close
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.modal = Modal::Close;
        }
        self.expire_notice(ctx);
        self.update_title(ctx);
        egui::SidePanel::left("profiles")
            .default_width(190.0)
            .width_range(150.0..=280.0)
            .show(ctx, |ui| self.profiles_panel(ui));
        egui::TopBottomPanel::bottom("log").show(ctx, |ui| self.log_panel(ui));
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(4.0);
            self.header(ui);
            ui.add_space(8.0);
            ui.columns(2, |columns| {
                card(&mut columns[0], |ui| self.connection_section(ui));
                card(&mut columns[1], |ui| self.bandwidth_section(ui));
            });
            ui.add_space(10.0);
            self.traffic_section(ui);
        });
        self.show_modal(ctx);
        if let Some(session) = &self.session {
            session.set_limits(self.editor.limits());
        }
        if self.running() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }
}

fn main() -> eframe::Result {
    eframe::run_native(
        "Bandwidth Proxy",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_icon(
                    eframe::icon_data::from_png_bytes(include_bytes!(
                        "../assets/bandwidth-proxy.png"
                    ))
                    .expect("The bundled application icon must be a valid PNG"),
                )
                .with_inner_size([1040.0, 760.0])
                .with_min_inner_size([820.0, 580.0]),
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}
