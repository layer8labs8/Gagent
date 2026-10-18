//! The sidebar, the welcome page and the chat view.

use eframe::egui::{
    self, Align, Color32, CornerRadius, Frame, Key, Layout, Margin, Modifiers, Panel, RichText,
    Sense, Stroke, Vec2,
};

use super::markdown;
use super::state::*;
use super::theme;
use super::GagentApp;
use crate::session::ChatItem;
use crate::util::{format_local, truncate_end};

// ---------------------------------------------------------------------------
// Small reusable widgets
// ---------------------------------------------------------------------------

/// A filled accent button.
pub fn primary_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(
        egui::Button::new(
            RichText::new(text)
                .color(Color32::from_rgb(0x03, 0x1B, 0x24))
                .strong(),
        )
        .fill(theme::ACCENT)
        .stroke(Stroke::NONE)
        .corner_radius(CornerRadius::same(theme::RADIUS))
        .min_size(Vec2::new(0.0, 30.0)),
    )
}

/// A button with a quiet background.
pub fn plain_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(egui::Button::new(RichText::new(text).color(theme::TEXT)).min_size(Vec2::new(0.0, 28.0)))
}

/// A small coloured status dot followed by text.
pub fn status_dot(ui: &mut egui::Ui, color: Color32, text: &str) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
        ui.painter().circle_filled(rect.center(), 4.0, color);
        ui.label(RichText::new(text).small().color(theme::MUTED));
    });
}

pub fn heading_label(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).small().strong().color(theme::FAINT));
}

fn card(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    Frame::new()
        .fill(theme::CARD_BG)
        .stroke(Stroke::new(1.0, theme::BORDER))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        });
}

// ---------------------------------------------------------------------------
// Sidebar
// ---------------------------------------------------------------------------

pub fn sidebar(app: &mut GagentApp, ui: &mut egui::Ui) {
    Panel::left("sidebar")
        .exact_size(270.0)
        .resizable(false)
        .frame(
            Frame::new()
                .fill(theme::SIDEBAR_BG)
                .stroke(Stroke::new(1.0, theme::BORDER))
                .inner_margin(Margin::same(12)),
        )
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                theme::logo(ui, 26.0);
                ui.add_space(2.0);
                ui.label(
                    RichText::new(crate::APP_NAME)
                        .size(19.0)
                        .strong()
                        .color(theme::TEXT_STRONG),
                );
                ui.label(
                    RichText::new(format!("v{}", crate::VERSION))
                        .small()
                        .color(theme::FAINT),
                );
            });
            ui.add_space(10.0);
            if primary_button(ui, "+  New session").clicked() {
                app.current = None;
            }
            ui.add_space(10.0);

            Panel::bottom("sidebar_footer")
                .resizable(false)
                .show_separator_line(false)
                .frame(Frame::new().inner_margin(Margin::symmetric(0, 4)))
                .show(ui, |ui| sidebar_footer(app, ui));

            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    sidebar_sessions(app, ui);
                });
        });
}

fn sidebar_sessions(app: &mut GagentApp, ui: &mut egui::Ui) {
    if app.sessions.is_empty() {
        ui.add_space(6.0);
        ui.label(
            RichText::new("No sessions yet.\nOpen a project to begin.")
                .small()
                .color(theme::FAINT),
        );
        return;
    }
    // Group by project, newest project first (the list is already sorted by time).
    let mut order: Vec<String> = Vec::new();
    for s in &app.sessions {
        if !order.contains(&s.project) {
            order.push(s.project.clone());
        }
    }
    let sessions = app.sessions.clone();
    let mut select: Option<String> = None;
    let mut delete: Option<String> = None;
    for project in order {
        ui.add_space(6.0);
        ui.label(
            RichText::new(GagentApp::project_name(&project))
                .small()
                .strong()
                .color(theme::FAINT),
        )
        .on_hover_text(&project);
        for s in sessions.iter().filter(|s| s.project == project) {
            let selected = app.current.as_deref() == Some(s.id.as_str());
            let running = app.open.get(&s.id).map(|a| a.is_running()).unwrap_or(false);
            let width = ui.available_width();
            let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 40.0), Sense::click());
            let hovered = response.hovered();
            if selected || hovered {
                ui.painter().rect_filled(
                    rect,
                    CornerRadius::same(8),
                    if selected {
                        theme::ACCENT_BG
                    } else {
                        theme::CARD_BG
                    },
                );
            }
            let text_rect = rect.shrink2(Vec2::new(8.0, 4.0));
            ui.painter().text(
                text_rect.left_top(),
                egui::Align2::LEFT_TOP,
                truncate_end(&s.title, 30),
                egui::FontId::proportional(14.0),
                if selected {
                    theme::TEXT_STRONG
                } else {
                    theme::TEXT
                },
            );
            ui.painter().text(
                text_rect.left_bottom(),
                egui::Align2::LEFT_BOTTOM,
                format_local(s.updated_at),
                egui::FontId::proportional(11.5),
                theme::FAINT,
            );
            if running {
                theme::paint_spinner(
                    ui.painter(),
                    ui.ctx(),
                    egui::pos2(rect.right() - 16.0, rect.center().y),
                    14.0,
                );
            } else if hovered || app.confirm_delete.as_deref() == Some(s.id.as_str()) {
                let del_rect = egui::Rect::from_center_size(
                    egui::pos2(rect.right() - 16.0, rect.center().y),
                    Vec2::splat(22.0),
                );
                let confirming = app.confirm_delete.as_deref() == Some(s.id.as_str());
                let del = ui.interact(del_rect, egui::Id::unique(("del", &s.id)), Sense::click());
                ui.painter().text(
                    del_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    if confirming { "Sure?" } else { "×" },
                    egui::FontId::proportional(if confirming { 11.0 } else { 17.0 }),
                    if del.hovered() || confirming {
                        theme::ERR
                    } else {
                        theme::FAINT
                    },
                );
                if del.clicked() {
                    if confirming {
                        delete = Some(s.id.clone());
                    } else {
                        app.confirm_delete = Some(s.id.clone());
                    }
                }
                del.on_hover_text(
                    "Delete this session and its private copy (your project is not touched)",
                );
            }
            if response.clicked() && !running_delete_click(ui, rect) {
                select = Some(s.id.clone());
            }
        }
    }
    if let Some(id) = delete {
        app.confirm_delete = None;
        app.delete_session(&id);
    } else if let Some(id) = select {
        app.confirm_delete = None;
        app.select_session(&id);
    }
}

/// True when the pointer is over the delete button area of a row.
fn running_delete_click(ui: &egui::Ui, rect: egui::Rect) -> bool {
    ui.input(|i| i.pointer.interact_pos())
        .map(|p| p.x > rect.right() - 30.0)
        .unwrap_or(false)
}

fn sidebar_footer(app: &mut GagentApp, ui: &mut egui::Ui) {
    ui.separator();
    ui.add_space(2.0);
    // Gemini
    let key_ok = !app.settings.effective_api_key().is_empty();
    status_dot(
        ui,
        if key_ok { theme::OK } else { theme::WARN },
        &if key_ok {
            format!("Gemini: {}", app.settings.model)
        } else {
            "Gemini: API key missing".to_string()
        },
    );
    // Sandbox
    match &app.docker {
        None => status_dot(ui, theme::FAINT, "Sandbox: checking..."),
        Some(s) if s.is_ready() => status_dot(ui, theme::OK, "Sandbox: Docker ready"),
        Some(crate::sandbox::DockerStatus::NotInstalled) => {
            status_dot(ui, theme::ERR, "Sandbox: Docker not installed")
        }
        Some(_) => status_dot(ui, theme::ERR, "Sandbox: Docker not running"),
    }
    // Drive
    if app.drive.client.is_some() {
        let name = app
            .drive
            .account
            .clone()
            .unwrap_or_else(|| "connected".into());
        status_dot(
            ui,
            theme::OK,
            &format!("Drive: {}", truncate_end(&name, 26)),
        );
    } else {
        status_dot(ui, theme::FAINT, "Drive: not connected");
    }
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if plain_button(ui, "Settings").clicked() {
            app.draft = Some(app.settings.clone());
        }
    });
}

// ---------------------------------------------------------------------------
// Welcome page
// ---------------------------------------------------------------------------

pub fn welcome(app: &mut GagentApp, ui: &mut egui::Ui) {
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        ui.vertical_centered(|ui| {
            ui.add_space((ui.available_height() * 0.10).clamp(24.0, 90.0));
            theme::logo(ui, 72.0);
            ui.add_space(8.0);
            ui.label(RichText::new("Gagent").size(34.0).strong().color(theme::TEXT_STRONG));
            ui.label(
                RichText::new("A Gemini coding agent that works on a private copy of your project\nand tests its changes in a Docker sandbox.")
                    .color(theme::MUTED),
            );
            ui.add_space(22.0);
            ui.allocate_ui_with_layout(
                Vec2::new(ui.available_width().min(620.0), 10.0),
                Layout::top_down(Align::Min),
                |ui| welcome_cards(app, ui),
            );
        });
    });
}

fn welcome_cards(app: &mut GagentApp, ui: &mut egui::Ui) {
    card(ui, |ui| {
        heading_label(ui, "OPEN A PROJECT");
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let browse_width = 90.0;
            let edit = egui::TextEdit::singleline(&mut app.new_project_path)
                .hint_text("Path to your project folder")
                .desired_width(ui.available_width() - browse_width - 8.0);
            let response = ui.add(edit);
            if plain_button(ui, "Browse...").clicked() {
                if let Some(dir) = rfd::FileDialog::new()
                    .set_title("Choose your project folder")
                    .pick_folder()
                {
                    app.new_project_path = dir.to_string_lossy().into_owned();
                }
            }
            if response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                let path = app.new_project_path.clone();
                app.start_session(&path);
            }
        });
        ui.add_space(8.0);
        if primary_button(ui, "Start a session").clicked() {
            let path = app.new_project_path.clone();
            app.start_session(&path);
        }
        ui.add_space(6.0);
        ui.label(
            RichText::new("The agent edits a private copy. Your folder changes only when you apply the result, and every apply can be undone.")
                .small()
                .color(theme::FAINT),
        );

        // Recent projects.
        let mut recent: Vec<String> = Vec::new();
        for s in &app.sessions {
            if !recent.contains(&s.project) && std::path::Path::new(&s.project).is_dir() {
                recent.push(s.project.clone());
            }
        }
        if !recent.is_empty() {
            ui.add_space(12.0);
            heading_label(ui, "RECENT PROJECTS");
            let mut chosen = None;
            for p in recent.iter().take(5) {
                let label = format!("{}   {}", GagentApp::project_name(p), p);
                if ui
                    .add(
                        egui::Button::new(
                            RichText::new(truncate_end(&label, 80)).color(theme::TEXT),
                        )
                        .frame(false),
                    )
                    .on_hover_text("Start a new session in this project")
                    .clicked()
                {
                    chosen = Some(p.clone());
                }
            }
            if let Some(p) = chosen {
                app.start_session(&p);
            }
        }
    });

    ui.add_space(12.0);
    card(ui, |ui| {
        heading_label(ui, "SETUP");
        ui.add_space(4.0);
        let key_ok = !app.settings.effective_api_key().is_empty();
        ui.horizontal(|ui| {
            status_dot(
                ui,
                if key_ok { theme::OK } else { theme::WARN },
                if key_ok {
                    "Gemini API key is set"
                } else {
                    "Gemini API key is missing"
                },
            );
            if !key_ok && plain_button(ui, "Add key").clicked() {
                app.draft = Some(app.settings.clone());
            }
        });
        let docker_text = app
            .docker
            .as_ref()
            .map(|d| d.describe())
            .unwrap_or_else(|| "Checking Docker...".into());
        let docker_ok = app.docker.as_ref().map(|d| d.is_ready()).unwrap_or(false);
        ui.horizontal_wrapped(|ui| {
            status_dot(
                ui,
                if docker_ok { theme::OK } else { theme::WARN },
                &docker_text,
            );
            if plain_button(ui, "Check again").clicked() {
                app.check_docker();
            }
        });
        if let Some(crate::sandbox::DockerStatus::Ready {
            image_present: false,
            ..
        }) = &app.docker
        {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(
                        "The sandbox image is built automatically on first use (a few minutes).",
                    )
                    .small()
                    .color(theme::FAINT),
                );
                if !app.building_image && plain_button(ui, "Build now").clicked() {
                    app.build_image();
                }
            });
        }
    });
}

// ---------------------------------------------------------------------------
// Chat
// ---------------------------------------------------------------------------

pub fn chat(app: &mut GagentApp, ui: &mut egui::Ui) {
    chat_header(app, ui);
    Panel::bottom("composer")
        .resizable(false)
        .show_separator_line(false)
        .frame(
            Frame::new()
                .fill(theme::BG)
                .inner_margin(Margin::symmetric(16, 10)),
        )
        .show(ui, |ui| composer(app, ui));

    let Some(id) = app.current.clone() else {
        return;
    };
    // Engine state notices come first; messages fill the rest.
    let mut retry = false;
    if let Some(a) = app.open.get(&id) {
        match &a.engine {
            EngineState::Opening | EngineState::None => {
                ui.add_space(30.0);
                ui.vertical_centered(|ui| {
                    theme::spinner(ui, 22.0);
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new("Preparing a private copy of your project...")
                            .color(theme::MUTED),
                    );
                });
                return;
            }
            EngineState::Failed(msg) => {
                ui.add_space(20.0);
                ui.vertical_centered(|ui| {
                    ui.allocate_ui_with_layout(
                        Vec2::new(ui.available_width().min(640.0), 10.0),
                        Layout::top_down(Align::Min),
                        |ui| {
                            Frame::new()
                                .fill(theme::ERR_BG)
                                .stroke(Stroke::new(1.0, theme::ERR))
                                .corner_radius(CornerRadius::same(10))
                                .inner_margin(Margin::same(14))
                                .show(ui, |ui| {
                                    ui.label(
                                        RichText::new("The workspace could not be opened")
                                            .strong()
                                            .color(theme::ERR),
                                    );
                                    ui.label(msg);
                                    ui.add_space(6.0);
                                    if plain_button(ui, "Try again").clicked() {
                                        retry = true;
                                    }
                                });
                        },
                    );
                });
            }
            EngineState::Ready(_) => {}
        }
    }
    if retry {
        app.open_engine(&id);
        return;
    }
    messages(app, ui, &id);
}

fn chat_header(app: &mut GagentApp, ui: &mut egui::Ui) {
    let Some(a) = app.active() else {
        return;
    };
    let title = a.session.meta.title.clone();
    let project = a.session.meta.project.clone();
    let running = a.is_running();
    Panel::top("chat_header")
        .resizable(false)
        .show_separator_line(true)
        .frame(
            Frame::new()
                .fill(theme::BG)
                .inner_margin(Margin::symmetric(16, 8)),
        )
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new(truncate_end(&title, 70))
                            .strong()
                            .size(15.5)
                            .color(theme::TEXT_STRONG),
                    );
                    ui.label(RichText::new(&project).small().color(theme::FAINT));
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .add(egui::Button::new(
                            RichText::new(if app.right_open {
                                "Hide panel"
                            } else {
                                "Show panel"
                            })
                            .small(),
                        ))
                        .clicked()
                    {
                        app.right_open = !app.right_open;
                    }
                    if ui
                        .add(egui::Button::new(RichText::new("Open folder").small()))
                        .clicked()
                    {
                        let _ = opener::open(&project);
                    }
                    if running {
                        theme::spinner(ui, 14.0);
                    }
                });
            });
        });
}

fn column_width(ui: &egui::Ui) -> f32 {
    ui.available_width().min(860.0)
}

fn messages(app: &mut GagentApp, ui: &mut egui::Ui, id: &str) {
    let docker_problem = match &app.docker {
        Some(d) if !d.is_ready() => Some(d.describe()),
        _ => None,
    };
    let key_missing = app.settings.effective_api_key().is_empty();
    let mut toggle: Vec<u32> = Vec::new();
    let mut open_settings = false;
    let mut recheck = false;
    let scroll = app
        .open
        .get(id)
        .map(|a| a.scroll_to_bottom)
        .unwrap_or(false);

    let Some(a) = app.open.get(id) else {
        return;
    };
    let items = a.session.items.clone();
    let expanded = a.expanded.clone();
    let run_status = a.run.as_ref().map(|r| {
        (
            r.status.clone(),
            r.started.elapsed().as_secs(),
            r.usage.total_tokens,
        )
    });

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .stick_to_bottom(true)
        .show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.allocate_ui_with_layout(Vec2::new(column_width(ui), 10.0), Layout::top_down(Align::Min), |ui| {
                    ui.add_space(12.0);
                    if items.is_empty() {
                        empty_state(ui);
                    }
                    if key_missing {
                        notice_card(ui, theme::WARN, "Add your Gemini API key to start.", Some("Open Settings"), &mut open_settings);
                    }
                    if let Some(problem) = &docker_problem {
                        let mut dummy = false;
                        notice_card(ui, theme::WARN, &format!("{problem} The agent can still read and edit files, but it cannot run code."), Some("Check again"), &mut dummy);
                        recheck |= dummy;
                    }
                    let mut previous_tool = false;
                    for item in &items {
                        let is_tool = matches!(item, ChatItem::Tool { .. });
                        if !(is_tool && previous_tool) {
                            ui.add_space(10.0);
                        } else {
                            ui.add_space(3.0);
                        }
                        previous_tool = is_tool;
                        match item {
                            ChatItem::User(text) => user_message(ui, text),
                            ChatItem::Assistant(text) => assistant_message(ui, text),
                            ChatItem::Tool { id, summary, ok, result, output, millis, .. } => {
                                if tool_row(ui, *id, summary, *ok, result, output, *millis, expanded.contains(id)) {
                                    toggle.push(*id);
                                }
                            }
                            ChatItem::Notice(text) => {
                                ui.vertical_centered(|ui| {
                                    ui.label(RichText::new(text).small().color(theme::MUTED));
                                });
                            }
                            ChatItem::Error(text) => error_card(ui, text),
                        }
                    }
                    if let Some((status, secs, tokens)) = &run_status {
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            theme::spinner(ui, 14.0);
                            ui.label(RichText::new(truncate_end(status, 110)).color(theme::MUTED));
                            ui.label(RichText::new(format!("{secs}s")).small().color(theme::FAINT));
                            if *tokens > 0 {
                                ui.label(RichText::new(format!("{} tokens", tokens)).small().color(theme::FAINT));
                            }
                        });
                    }
                    ui.add_space(16.0);
                });
            });
            if scroll {
                ui.scroll_to_cursor(Some(Align::BOTTOM));
            }
        });

    if let Some(a) = app.open.get_mut(id) {
        a.scroll_to_bottom = false;
        for t in toggle {
            if !a.expanded.remove(&t) {
                a.expanded.insert(t);
            }
        }
    }
    if open_settings {
        app.draft = Some(app.settings.clone());
    }
    if recheck {
        app.check_docker();
    }
}

fn empty_state(ui: &mut egui::Ui) {
    ui.add_space(30.0);
    ui.vertical_centered(|ui| {
        theme::logo(ui, 44.0);
        ui.add_space(6.0);
        ui.label(RichText::new("What should we build or fix?").size(20.0).strong().color(theme::TEXT_STRONG));
        ui.add_space(4.0);
        ui.label(
            RichText::new("Describe a task. The agent reads your code, edits files, runs them in the sandbox,\nreads the logs, and fixes problems until it works.")
                .color(theme::MUTED),
        );
    });
}

fn notice_card(
    ui: &mut egui::Ui,
    color: Color32,
    text: &str,
    button: Option<&str>,
    clicked: &mut bool,
) {
    ui.add_space(6.0);
    Frame::new()
        .fill(theme::CARD_BG)
        .stroke(Stroke::new(1.0, color))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(text).color(theme::TEXT));
                if let Some(b) = button {
                    if plain_button(ui, b).clicked() {
                        *clicked = true;
                    }
                }
            });
        });
}

fn user_message(ui: &mut egui::Ui, text: &str) {
    ui.with_layout(Layout::top_down(Align::Max), |ui| {
        Frame::new()
            .fill(theme::USER_BG)
            .stroke(Stroke::new(1.0, theme::BORDER))
            .corner_radius(CornerRadius::same(12))
            .inner_margin(Margin::symmetric(14, 10))
            .show(ui, |ui| {
                ui.set_max_width((ui.available_width() * 0.82).max(200.0));
                ui.add(egui::Label::new(RichText::new(text).color(theme::TEXT_STRONG)).wrap());
            });
    });
}

fn assistant_message(ui: &mut egui::Ui, text: &str) {
    ui.horizontal_top(|ui| {
        theme::logo(ui, 20.0);
        ui.vertical(|ui| {
            ui.set_width(ui.available_width());
            markdown::render(ui, text);
        });
    });
}

fn error_card(ui: &mut egui::Ui, text: &str) {
    Frame::new()
        .fill(theme::ERR_BG)
        .stroke(Stroke::new(1.0, theme::ERR))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::symmetric(14, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(text).color(Color32::from_rgb(0xFE, 0xCA, 0xCA)));
        });
}

/// One tool call. Returns true when the user clicked it to expand or collapse.
#[allow(clippy::too_many_arguments)]
fn tool_row(
    ui: &mut egui::Ui,
    id: u32,
    summary: &str,
    ok: Option<bool>,
    result: &str,
    output: &str,
    millis: u64,
    expanded: bool,
) -> bool {
    let frame = Frame::new()
        .fill(theme::CARD_BG)
        .stroke(Stroke::new(1.0, theme::BORDER))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(10, 6));
    let inner = frame.show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            match ok {
                None => {
                    theme::spinner(ui, 12.0);
                }
                Some(good) => {
                    let (rect, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
                    ui.painter().circle_filled(
                        rect.center(),
                        4.5,
                        if good { theme::OK } else { theme::ERR },
                    );
                }
            }
            ui.add(
                egui::Label::new(
                    RichText::new(truncate_end(summary, 90))
                        .monospace()
                        .size(13.0)
                        .color(theme::TEXT),
                )
                .selectable(false),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                theme::chevron(ui, expanded, theme::FAINT);
                if ok.is_some() {
                    if millis >= 100 {
                        ui.label(
                            RichText::new(format!("{:.1}s", millis as f64 / 1000.0))
                                .small()
                                .color(theme::FAINT),
                        );
                    }
                    ui.add(
                        egui::Label::new(RichText::new(truncate_end(result, 40)).small().color(
                            if ok == Some(true) {
                                theme::MUTED
                            } else {
                                theme::ERR
                            },
                        ))
                        .selectable(false),
                    );
                }
            });
        });
        if expanded && !output.is_empty() {
            ui.add_space(4.0);
            Frame::new()
                .fill(theme::CODE_BG)
                .corner_radius(CornerRadius::same(6))
                .inner_margin(Margin::same(8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    egui::ScrollArea::both()
                        .id_salt(("tool_out", id))
                        .max_height(260.0)
                        .show(ui, |ui| {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(truncate_end(output, 12_000))
                                        .monospace()
                                        .size(12.5)
                                        .color(theme::TEXT),
                                )
                                .extend(),
                            );
                        });
                });
        }
    });
    let response = ui.interact(
        inner.response.rect,
        egui::Id::unique(("tool_click", id)),
        Sense::click(),
    );
    response.clicked() && ok.is_some()
}

// ---------------------------------------------------------------------------
// Composer
// ---------------------------------------------------------------------------

fn composer(app: &mut GagentApp, ui: &mut egui::Ui) {
    let Some(id) = app.current.clone() else {
        return;
    };
    let (running, ready, has_attachments) = match app.open.get(&id) {
        Some(a) => (
            a.is_running(),
            matches!(a.engine, EngineState::Ready(_)),
            !a.attachments.is_empty(),
        ),
        None => return,
    };
    let mut send = false;
    let mut stop = false;
    let mut attach = false;
    let mut remove_attachment: Option<String> = None;
    let mut new_model: Option<String> = None;

    ui.vertical_centered(|ui| {
        ui.allocate_ui_with_layout(Vec2::new(column_width(ui), 10.0), Layout::top_down(Align::Min), |ui| {
            if has_attachments {
                ui.horizontal_wrapped(|ui| {
                    if let Some(a) = app.open.get(&id) {
                        for f in &a.attachments {
                            if ui.add(egui::Button::new(RichText::new(format!("{f}  ×")).small())).on_hover_text("Remove from this message").clicked() {
                                remove_attachment = Some(f.clone());
                            }
                        }
                    }
                });
            }
            Frame::new()
                .fill(theme::INPUT_BG)
                .stroke(Stroke::new(1.0, theme::BORDER))
                .corner_radius(CornerRadius::same(14))
                .inner_margin(Margin::symmetric(12, 10))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    let text_id = egui::Id::unique(("composer_text", &id));
                    let focused = ui.ctx().memory(|m| m.has_focus(text_id));
                    if focused && ready && !running {
                        // Enter sends; Shift+Enter inserts a new line.
                        if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) {
                            send = true;
                        }
                    }
                    if let Some(a) = app.open.get_mut(&id) {
                        let edit = egui::TextEdit::multiline(&mut a.input)
                            .id(text_id)
                            .desired_rows(2)
                            .desired_width(f32::INFINITY)
                            .frame(egui::Frame::NONE)
                            .hint_text(if ready { "Describe a task, or ask a question about your project..." } else { "Preparing the workspace..." });
                        let response = ui.add_enabled(ready, edit);
                        if a.focus_input && ready {
                            response.request_focus();
                            a.focus_input = false;
                        }
                    }
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        if ui.add_enabled(ready, egui::Button::new(RichText::new("+ Attach").small())).on_hover_text("Add files to the workspace (you can also drop files on the window)").clicked() {
                            attach = true;
                        }
                        let current = app.settings.model.clone();
                        egui::ComboBox::from_id_salt("model_combo")
                            .selected_text(RichText::new(&current).small())
                            .width(190.0)
                            .show_ui(ui, |ui| {
                                let mut models = app.models.clone();
                                if !models.contains(&current) {
                                    models.insert(0, current.clone());
                                }
                                for m in models {
                                    if ui.selectable_label(m == current, &m).clicked() {
                                        new_model = Some(m);
                                    }
                                }
                            });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if running {
                                if ui.add(egui::Button::new(RichText::new("Stop").color(theme::ERR).strong())).clicked() {
                                    stop = true;
                                }
                            } else {
                                let can_send = ready && app.open.get(&id).map(|a| !a.input.trim().is_empty()).unwrap_or(false);
                                let (fill, ink) = if can_send {
                                    (theme::ACCENT, Color32::from_rgb(0x03, 0x1B, 0x24))
                                } else {
                                    (theme::CARD_BG_HOVER, theme::FAINT)
                                };
                                let button = egui::Button::new(RichText::new("Send").color(ink).strong())
                                    .fill(fill)
                                    .stroke(Stroke::NONE);
                                if ui.add_enabled(can_send, button).clicked() {
                                    send = true;
                                }
                            }
                            ui.label(RichText::new("Shift+Enter for a new line").small().color(theme::FAINT));
                        });
                    });
                });
        });
    });

    if let Some(f) = remove_attachment {
        if let Some(a) = app.open.get_mut(&id) {
            a.attachments.retain(|x| x != &f);
        }
    }
    if let Some(m) = new_model {
        if m != app.settings.model {
            app.settings.model = m;
            app.save_settings();
            for a in app.open.values_mut() {
                a.engine_dirty = true;
            }
            app.refresh_engines_if_idle();
        }
    }
    if attach {
        if let Some(files) = rfd::FileDialog::new()
            .set_title("Add files to the workspace")
            .pick_files()
        {
            app.attach_files(files);
        }
    }
    if stop {
        app.stop_run();
    }
    if send {
        app.send_message();
    }
}

// ---------------------------------------------------------------------------
// Banner
// ---------------------------------------------------------------------------

pub fn banner(app: &mut GagentApp, ctx: &egui::Context) {
    let Some(b) = &app.banner else {
        return;
    };
    let (text, error) = (b.text.clone(), b.error);
    let mut close = false;
    egui::Area::new(egui::Id::unique("banner"))
        .anchor(egui::Align2::CENTER_BOTTOM, [0.0, -86.0])
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            Frame::new()
                .fill(if error {
                    theme::ERR_BG
                } else {
                    theme::CARD_BG_HOVER
                })
                .stroke(Stroke::new(
                    1.0,
                    if error { theme::ERR } else { theme::ACCENT },
                ))
                .corner_radius(CornerRadius::same(10))
                .inner_margin(Margin::symmetric(14, 9))
                .show(ui, |ui| {
                    ui.set_max_width(640.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(truncate_end(&text, 400)).color(theme::TEXT_STRONG));
                        if ui.add(egui::Button::new("×").frame(false)).clicked() {
                            close = true;
                        }
                    });
                });
        });
    if close {
        app.banner = None;
    }
}
