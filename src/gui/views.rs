//! The sidebar, the welcome page and the chat view.

use eframe::egui::{
    self, Align, Color32, CornerRadius, Frame, Key, Layout, Margin, Modifiers, Panel, Pos2,
    RichText, Sense, Shape, Stroke, Vec2,
};

use super::markdown;
use super::state::*;
use super::theme;
use super::GagentApp;
use crate::session::ChatItem;
use crate::util::{format_local, truncate_end};

/// Width of the text column in the chat.
const COLUMN: f32 = 780.0;

// ---------------------------------------------------------------------------
// Small reusable widgets
// ---------------------------------------------------------------------------

/// A filled accent button.
pub fn primary_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(text).color(theme::on_accent()).strong())
            .fill(theme::accent())
            .stroke(Stroke::NONE)
            .corner_radius(CornerRadius::same(theme::RADIUS))
            .min_size(Vec2::new(0.0, 32.0)),
    )
}

/// A button with a quiet background.
pub fn plain_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(egui::Button::new(RichText::new(text)).min_size(Vec2::new(0.0, 30.0)))
}

/// A small coloured status dot followed by text.
pub fn status_dot(ui: &mut egui::Ui, color: Color32, text: &str) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
        ui.painter().circle_filled(rect.center(), 3.5, color);
        ui.label(RichText::new(text).small().color(theme::muted()));
    });
}

/// A small section title.
pub fn heading_label(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).small().strong().color(theme::faint()));
}

fn card(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    Frame::new()
        .fill(theme::card_bg())
        .stroke(Stroke::new(1.0, theme::border()))
        .corner_radius(CornerRadius::same(14))
        .inner_margin(Margin::same(14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        });
}

/// A round button drawn by hand. `paint` draws the symbol on top of the circle.
fn round_button(
    ui: &mut egui::Ui,
    size: f32,
    fill: Color32,
    stroke: Stroke,
    enabled: bool,
    paint: impl FnOnce(&egui::Painter, Pos2),
) -> egui::Response {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), sense);
    let fill = if enabled && response.hovered() {
        fill.gamma_multiply(0.88)
    } else {
        fill
    };
    ui.painter().circle(rect.center(), size / 2.0, fill, stroke);
    paint(ui.painter(), rect.center());
    if enabled {
        response.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        response
    }
}

// ---------------------------------------------------------------------------
// Sidebar
// ---------------------------------------------------------------------------

pub fn sidebar(app: &mut GagentApp, ui: &mut egui::Ui) {
    Panel::left("sidebar")
        .exact_size(264.0)
        .resizable(false)
        .frame(
            Frame::new()
                .fill(theme::sidebar_bg())
                .inner_margin(Margin::symmetric(12, 14)),
        )
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.add_space(4.0);
                theme::logo(ui, 26.0);
                ui.label(
                    RichText::new(crate::APP_NAME)
                        .size(18.0)
                        .strong()
                        .color(theme::text_strong()),
                );
            });
            ui.add_space(14.0);
            let new_session = ui.add(
                egui::Button::new(
                    RichText::new("+  New session")
                        .color(theme::accent())
                        .strong(),
                )
                .fill(Color32::TRANSPARENT)
                .stroke(Stroke::new(1.0, theme::border()))
                .corner_radius(CornerRadius::same(10))
                .min_size(Vec2::new(ui.available_width(), 34.0)),
            );
            if new_session.clicked() {
                app.current = None;
                app.project_draft = None;
            }
            ui.add_space(8.0);

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
            RichText::new("No sessions yet. Open a project to begin.")
                .small()
                .color(theme::faint()),
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
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(
                RichText::new(truncate_end(&GagentApp::project_name(&project), 28))
                    .small()
                    .strong()
                    .color(theme::faint()),
            )
            .on_hover_text(&project);
        });
        ui.add_space(2.0);
        for s in sessions.iter().filter(|s| s.project == project) {
            let selected = app.current.as_deref() == Some(s.id.as_str());
            let running = app.open.get(&s.id).map(|a| a.is_running()).unwrap_or(false);
            let width = ui.available_width();
            let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 32.0), Sense::click());
            // `response.hovered()` turns false while the pointer is over the delete
            // button (it is on top of the row), which made the button flicker away.
            let hovered = ui.rect_contains_pointer(rect);
            if selected || hovered {
                ui.painter().rect_filled(
                    rect,
                    CornerRadius::same(8),
                    if selected {
                        theme::row_selected()
                    } else {
                        theme::row_hover()
                    },
                );
            }
            ui.painter().text(
                egui::pos2(rect.left() + 10.0, rect.center().y),
                egui::Align2::LEFT_CENTER,
                if s.team.is_some() {
                    format!("[team] {}", truncate_end(&s.title, 22))
                } else {
                    truncate_end(&s.title, 28)
                },
                egui::FontId::proportional(14.0),
                if selected {
                    theme::text_strong()
                } else {
                    theme::text()
                },
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
                    egui::pos2(rect.right() - 18.0, rect.center().y),
                    Vec2::new(30.0, 22.0),
                );
                let confirming = app.confirm_delete.as_deref() == Some(s.id.as_str());
                let del = ui.interact(del_rect, egui::Id::unique(("del", &s.id)), Sense::click());
                ui.painter().text(
                    del_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    if confirming { "Delete?" } else { "×" },
                    egui::FontId::proportional(if confirming { 11.5 } else { 17.0 }),
                    if del.hovered() || confirming {
                        theme::err()
                    } else {
                        theme::faint()
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
            let clicked = response.clicked();
            response.on_hover_text(format!("{}\n{}", s.title, format_local(s.updated_at)));
            if clicked && !running_delete_click(ui, rect) {
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
        .map(|p| p.x > rect.right() - 34.0)
        .unwrap_or(false)
}

fn sidebar_footer(app: &mut GagentApp, ui: &mut egui::Ui) {
    ui.add_space(4.0);
    // Nothing is shown while everything is fine; a problem gets one line.
    let problem = if !crate::config::Provider::ALL.into_iter().any(|p| app.settings.provider_ready(p)) {
        Some("No AI key yet: open Settings".to_string())
    } else {
        match &app.docker {
            Some(crate::sandbox::DockerStatus::NotInstalled) => {
                Some("Git, Docker or WSL not found: commands cannot run".to_string())
            }
            Some(s) if !s.is_ready() => Some("The sandbox is not running".to_string()),
            _ => None,
        }
    };
    if let Some(text) = problem {
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.add(
                egui::Label::new(RichText::new(text).small().color(theme::warn())).wrap(),
            );
        });
        ui.add_space(4.0);
    }
    let settings = ui.add(
        egui::Button::new(RichText::new("Settings"))
            .fill(Color32::TRANSPARENT)
            .stroke(Stroke::NONE)
            .min_size(Vec2::new(ui.available_width(), 30.0)),
    );
    if settings.clicked() {
        app.draft = Some(app.settings.clone());
    }
}

// ---------------------------------------------------------------------------
// Welcome page
// ---------------------------------------------------------------------------

pub fn welcome(app: &mut GagentApp, ui: &mut egui::Ui) {
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space((ui.available_height() * 0.14).clamp(24.0, 120.0));
                theme::logo(ui, 60.0);
                ui.add_space(12.0);
                ui.label(
                    RichText::new("What should we work on?")
                        .size(30.0)
                        .color(theme::text_strong()),
                );
                ui.add_space(6.0);
                ui.label(
                    RichText::new("Pick a project folder. Gagent works on a private copy of it and puts the\nresult into your folder when a task is done. Every apply can be undone.")
                        .color(theme::muted()),
                );
                ui.add_space(26.0);
                ui.allocate_ui_with_layout(
                    Vec2::new(ui.available_width().min(620.0), 10.0),
                    Layout::top_down(Align::Min),
                    |ui| welcome_body(app, ui),
                );
            });
        });
}

fn welcome_body(app: &mut GagentApp, ui: &mut egui::Ui) {
    card(ui, |ui| {
        ui.horizontal(|ui| {
            let buttons = 170.0;
            let edit = egui::TextEdit::singleline(&mut app.new_project_path)
                .hint_text("Path to a project folder")
                .margin(Margin::symmetric(8, 7))
                .desired_width(ui.available_width() - buttons);
            let response = ui.add(edit);
            if plain_button(ui, "Browse").clicked() {
                if let Some(dir) = rfd::FileDialog::new()
                    .set_title("Choose your project folder")
                    .pick_folder()
                {
                    app.new_project_path = dir.to_string_lossy().into_owned();
                }
            }
            let go = primary_button(ui, "Open").clicked();
            let enter = response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            if go || enter {
                let path = app.new_project_path.clone();
                app.start_session(&path);
            }
        });
    });

    // Setup problems, only when there are any.
    ui.add_space(10.0);
    if !app.settings.is_configured() {
        let mut open = false;
        notice_line(
            ui,
            theme::warn(),
            &format!(
                "{} is not set up yet. Add your API key to start.",
                app.settings.provider_label()
            ),
            Some("Set up"),
            &mut open,
        );
        if open {
            app.draft = Some(app.settings.clone());
        }
    }
    let sandbox_problem = match &app.docker {
        Some(d) if !d.is_ready() => Some(d.describe()),
        _ => None,
    };
    if let Some(problem) = sandbox_problem {
        let mut check = false;
        notice_line(
            ui,
            theme::warn(),
            &format!("{problem} Without it the agent can edit files but cannot run code."),
            Some("Check again"),
            &mut check,
        );
        if check {
            app.check_docker();
        }
    }

    // Recent projects.
    let mut recent: Vec<String> = Vec::new();
    for s in &app.sessions {
        if !recent.contains(&s.project) && std::path::Path::new(&s.project).is_dir() {
            recent.push(s.project.clone());
        }
    }
    if !recent.is_empty() {
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            heading_label(ui, "Recent");
        });
        let mut chosen = None;
        for p in recent.iter().take(6) {
            let width = ui.available_width();
            let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 38.0), Sense::click());
            if response.hovered() {
                ui.painter()
                    .rect_filled(rect, CornerRadius::same(10), theme::row_hover());
            }
            ui.painter().text(
                egui::pos2(rect.left() + 12.0, rect.center().y),
                egui::Align2::LEFT_CENTER,
                GagentApp::project_name(p),
                egui::FontId::proportional(14.5),
                theme::text_strong(),
            );
            ui.painter().text(
                egui::pos2(rect.right() - 12.0, rect.center().y),
                egui::Align2::RIGHT_CENTER,
                truncate_end(p, 46),
                egui::FontId::proportional(12.0),
                theme::faint(),
            );
            if response
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
    ui.add_space(18.0);
    ui.label(RichText::new("AI can make mistakes. Check the Changes before you apply them, and keep a backup. You use Gagent at your own risk; see SECURITY.md.").small().color(theme::faint()));
}

// ---------------------------------------------------------------------------
// Chat
// ---------------------------------------------------------------------------

pub fn chat(app: &mut GagentApp, ui: &mut egui::Ui) {
    chat_header(app, ui);
    let is_team = app.active().is_some_and(|a| a.team.is_some());
    if let Some(view) = app.active().and_then(|a| a.team.as_ref()).map(|t| t.view()) {
        Panel::top("team_board")
            .resizable(false)
            .show_separator_line(true)
            .frame(
                Frame::new()
                    .fill(theme::bg())
                    .inner_margin(Margin::symmetric(16, 8)),
            )
            .show(ui, |ui| super::project::board(ui, &view));
    }
    Panel::bottom("composer")
        .resizable(false)
        .show_separator_line(false)
        .frame(
            Frame::new()
                .fill(theme::bg())
                .inner_margin(Margin::symmetric(16, 8)),
        )
        .show(ui, |ui| {
            // One message box for everything; a team's controls sit above it.
            if is_team {
                super::project::bar(app, ui);
            }
            composer(app, ui)
        });

    let Some(id) = app.current.clone() else {
        return;
    };
    // Engine state notices come first; messages fill the rest.
    let mut retry = false;
    if let Some(a) = app.open.get(&id) {
        match &a.engine {
            EngineState::Opening | EngineState::None => {
                let waiting = app.trust_ask.as_deref() == Some(id.as_str());
                ui.add_space(30.0);
                ui.vertical_centered(|ui| {
                    if !waiting {
                        theme::spinner(ui, 22.0);
                    }
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(if waiting {
                            "Waiting for your answer: do you trust this folder?"
                        } else {
                            "Preparing a private copy of your project..."
                        })
                        .color(theme::muted()),
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
                                .fill(theme::err_bg())
                                .corner_radius(CornerRadius::same(12))
                                .inner_margin(Margin::same(14))
                                .show(ui, |ui| {
                                    ui.label(
                                        RichText::new("The workspace could not be opened")
                                            .strong()
                                            .color(theme::err()),
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
    let pending = a.changes.len();
    let spent = crate::usage::summary(&a.session.meta.spent);
    Panel::top("chat_header")
        .resizable(false)
        .show_separator_line(true)
        .frame(
            Frame::new()
                .fill(theme::bg())
                .inner_margin(Margin::symmetric(18, 9)),
        )
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(truncate_end(&title, 60))
                        .strong()
                        .size(15.0)
                        .color(theme::text_strong()),
                );
                ui.label(
                    RichText::new(GagentApp::project_name(&project))
                        .small()
                        .color(theme::faint()),
                )
                .on_hover_text(&project);
                if let Some(spent) = &spent {
                    ui.label(
                        RichText::new(truncate_end(spent, 70))
                            .small()
                            .color(theme::faint()),
                    )
                    .on_hover_text(format!("Used in this session: {spent}"));
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let label = if pending > 0 {
                        RichText::new(format!("Changes ({pending})"))
                            .color(theme::accent())
                            .strong()
                    } else {
                        RichText::new("Changes")
                    };
                    let button = egui::Button::new(label)
                        .fill(if app.right_open {
                            theme::row_selected()
                        } else {
                            Color32::TRANSPARENT
                        })
                        .stroke(Stroke::NONE);
                    if ui.add(button).clicked() {
                        app.right_open = !app.right_open;
                        if app.right_open {
                            app.tab = Tab::Changes;
                            app.refresh_changes();
                        }
                    }
                    if ui
                        .add(
                            egui::Button::new("Open folder")
                                .fill(Color32::TRANSPARENT)
                                .stroke(Stroke::NONE),
                        )
                        .clicked()
                    {
                        let _ = opener::open(&project);
                    }
                });
            });
        });
}

fn column_width(ui: &egui::Ui) -> f32 {
    ui.available_width().min(COLUMN)
}

fn messages(app: &mut GagentApp, ui: &mut egui::Ui, id: &str) {
    let mut toggle: Vec<u32> = Vec::new();
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
            r.last_event.elapsed().as_secs(),
        )
    });
    let sandbox_problem = match &app.docker {
        Some(d) if !d.is_ready() => Some(d.describe()),
        _ => None,
    };
    let not_configured = !app.settings.is_configured();
    let provider = app.settings.provider_label();
    let team_view = app.open.get(id).and_then(|a| a.team.as_ref()).map(|t| t.view());
    let waiting_model = team_view
        .as_ref()
        .and_then(|v| v.current_model.clone())
        .unwrap_or_else(|| app.settings.active_model().to_string());
    let mut open_settings = false;
    let mut recheck = false;

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .stick_to_bottom(true)
        .show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.allocate_ui_with_layout(
                    Vec2::new(column_width(ui), 10.0),
                    Layout::top_down(Align::Min),
                    |ui| {
                        ui.add_space(14.0);
                        if items.is_empty() && team_view.is_none() {
                            empty_state(ui);
                        }
                        if not_configured {
                            notice_line(
                                ui,
                                theme::warn(),
                                &format!("Set up {provider} in Settings to start."),
                                Some("Open Settings"),
                                &mut open_settings,
                            );
                        }
                        if let Some(problem) = &sandbox_problem {
                            notice_line(
                                ui,
                                theme::warn(),
                                &format!("{problem} The agent can still read and edit files, but it cannot run code."),
                                Some("Check again"),
                                &mut recheck,
                            );
                        }
                        let mut previous_tool = false;
                        for item in &items {
                            let is_tool = matches!(item, ChatItem::Tool { .. });
                            ui.add_space(if is_tool && previous_tool { 1.0 } else { 14.0 });
                            previous_tool = is_tool;
                            match item {
                                ChatItem::User(text) => user_message(ui, text),
                                ChatItem::Assistant(text) => assistant_message(ui, text),
                                ChatItem::Tool {
                                    id,
                                    summary,
                                    ok,
                                    result,
                                    output,
                                    millis,
                                    ..
                                } => {
                                    if tool_row(
                                        ui,
                                        *id,
                                        summary,
                                        *ok,
                                        result,
                                        output,
                                        *millis,
                                        expanded.contains(id),
                                    ) {
                                        toggle.push(*id);
                                    }
                                }
                                ChatItem::Notice(text) => {
                                    ui.vertical_centered(|ui| {
                                        ui.label(
                                            RichText::new(text).small().color(theme::faint()),
                                        );
                                    });
                                }
                                ChatItem::Error(text) => error_card(ui, text),
                            }
                        }
                        if let Some((status, secs, tokens, idle)) = &run_status {
                            ui.add_space(14.0);
                            ui.horizontal(|ui| {
                                theme::spinner(ui, 14.0);
                                ui.label(
                                    RichText::new(truncate_end(status, 100))
                                        .color(theme::muted()),
                                );
                                let mut meta = format!("{secs}s");
                                if *tokens > 0 {
                                    meta.push_str(&format!(" · {tokens} tokens"));
                                }
                                ui.label(RichText::new(meta).small().color(theme::faint()));
                            });
                            if *idle >= 40 {
                                let usual = crate::modelstats::typical_secs(&waiting_model)
                                    .map(|t| format!(" It took about {t:.0} s last time."))
                                    .unwrap_or_default();
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(format!(
                                            "No answer from {} for {idle} s. Free endpoints queue requests, so some models answer slowly.{usual} You can press stop and choose a faster model (the model menu shows reply times).",
                                            crate::models::short_name(&waiting_model)
                                        ))
                                        .small()
                                        .color(theme::faint()),
                                    )
                                    .wrap(),
                                );
                            }
                        }
                        ui.add_space(20.0);
                    },
                );
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
    ui.add_space(40.0);
    ui.vertical_centered(|ui| {
        theme::logo(ui, 44.0);
        ui.add_space(10.0);
        ui.label(
            RichText::new("What should we build or fix?")
                .size(22.0)
                .color(theme::text_strong()),
        );
        ui.add_space(4.0);
        ui.label(
            RichText::new("Describe a task. The agent reads your code, edits files, runs them\nand fixes problems until it works.")
                .color(theme::muted()),
        );
        ui.add_space(10.0);
        ui.label(RichText::new("AI can make mistakes. Check the Changes before you apply them, and keep a backup. You use Gagent at your own risk; see SECURITY.md.").small().color(theme::faint()));
    });
}

/// One line of advice with an optional button, e.g. "Set up Gemini ... [Open Settings]".
fn notice_line(
    ui: &mut egui::Ui,
    color: Color32,
    text: &str,
    button: Option<&str>,
    clicked: &mut bool,
) {
    ui.add_space(4.0);
    Frame::new()
        .fill(theme::card_bg())
        .stroke(Stroke::new(1.0, theme::border()))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("●").small().color(color));
                ui.label(RichText::new(text).color(theme::text()));
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
            .fill(theme::user_bg())
            .corner_radius(CornerRadius::same(16))
            .inner_margin(Margin::symmetric(16, 11))
            .show(ui, |ui| {
                ui.set_max_width((ui.available_width() * 0.85).max(240.0));
                // The bubble sits at the right, the text starts at the left of it.
                ui.with_layout(Layout::top_down(Align::Min), |ui| {
                    ui.add(
                        egui::Label::new(RichText::new(text).color(theme::text_strong())).wrap(),
                    );
                });
            });
    });
}

fn assistant_message(ui: &mut egui::Ui, text: &str) {
    ui.horizontal_top(|ui| {
        theme::logo(ui, 22.0);
        ui.add_space(4.0);
        ui.vertical(|ui| {
            ui.set_width(ui.available_width());
            markdown::render(ui, text);
        });
    });
}

fn error_card(ui: &mut egui::Ui, text: &str) {
    Frame::new()
        .fill(theme::err_bg())
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::symmetric(14, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(text).color(theme::err()));
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
    // The hover background is painted first (reserved slot), the content on top of it.
    let background = ui.painter().add(Shape::Noop);
    let inner = Frame::new()
        .inner_margin(Margin::symmetric(8, 3))
        .show(ui, |ui| {
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
                            3.5,
                            if good { theme::faint() } else { theme::err() },
                        );
                    }
                }
                ui.add(
                    egui::Label::new(
                        RichText::new(truncate_end(summary, 90))
                            .size(13.5)
                            .color(theme::muted()),
                    )
                    .selectable(false),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    theme::chevron(ui, expanded, theme::faint());
                    if ok.is_some() {
                        if millis >= 100 {
                            ui.label(
                                RichText::new(format!("{:.1}s", millis as f64 / 1000.0))
                                    .small()
                                    .color(theme::faint()),
                            );
                        }
                        ui.add(
                            egui::Label::new(
                                RichText::new(truncate_end(result, 40)).small().color(
                                    if ok == Some(true) {
                                        theme::faint()
                                    } else {
                                        theme::err()
                                    },
                                ),
                            )
                            .selectable(false),
                        );
                    }
                });
            });
            if expanded && !output.is_empty() {
                ui.add_space(4.0);
                Frame::new()
                    .fill(theme::code_bg())
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(Margin::same(10))
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
                                            .color(theme::text()),
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
    if response.hovered() && ok.is_some() {
        ui.painter().set(
            background,
            Shape::rect_filled(
                inner.response.rect,
                CornerRadius::same(8),
                theme::row_hover(),
            ),
        );
    }
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
    // After the first answer, a small thank-you line (hidden for good with one click).
    let show_star = !app.settings.star_hint_hidden
        && app.open.get(&id).is_some_and(|a| {
            a.run.is_none()
                && a.session
                    .items
                    .iter()
                    .any(|i| matches!(i, ChatItem::Assistant(_)))
        });
    let mut hide_star = false;
    let mut new_permission: Option<crate::config::PermissionMode> = None;
    let mut toggle_mic = false;
    let mut stop_voice = false;

    ui.vertical_centered(|ui| {
        ui.allocate_ui_with_layout(
            Vec2::new(column_width(ui), 10.0),
            Layout::top_down(Align::Min),
            |ui| {
                if has_attachments {
                    ui.horizontal_wrapped(|ui| {
                        if let Some(a) = app.open.get(&id) {
                            for f in &a.attachments {
                                if ui
                                    .add(egui::Button::new(
                                        RichText::new(format!("{f}  ×")).small(),
                                    ))
                                    .on_hover_text("Remove from this message")
                                    .clicked()
                                {
                                    remove_attachment = Some(f.clone());
                                }
                            }
                        }
                    });
                }
                Frame::new()
                    .fill(theme::input_bg())
                    .stroke(Stroke::new(1.0, theme::border()))
                    .corner_radius(CornerRadius::same(18))
                    .inner_margin(Margin::symmetric(16, 12))
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
                                .hint_text(if ready {
                                    "Describe a task, or ask a question about your project..."
                                } else {
                                    "Preparing the workspace..."
                                });
                            let response = ui.add_enabled(ready, edit);
                            if a.focus_input && ready {
                                response.request_focus();
                                a.focus_input = false;
                            }
                        }
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            let attach_button = round_button(
                                ui,
                                30.0,
                                Color32::TRANSPARENT,
                                Stroke::new(1.0, theme::border()),
                                ready,
                                |p, c| {
                                    let s = Stroke::new(1.6, theme::muted());
                                    p.line_segment(
                                        [c + Vec2::new(-5.0, 0.0), c + Vec2::new(5.0, 0.0)],
                                        s,
                                    );
                                    p.line_segment(
                                        [c + Vec2::new(0.0, -5.0), c + Vec2::new(0.0, 5.0)],
                                        s,
                                    );
                                },
                            )
                            .on_hover_text(
                                "Add files to the workspace (you can also drop files on the window)",
                            );
                            if attach_button.clicked() {
                                attach = true;
                            }
                            let listening = app.recording.as_ref().map(|(_, r)| r.seconds());
                            let voice_busy = app.voice_status.clone();
                            let ink = if listening.is_some() {
                                theme::on_accent()
                            } else {
                                theme::muted()
                            };
                            let mic_button = round_button(
                                ui,
                                30.0,
                                if listening.is_some() {
                                    theme::err()
                                } else {
                                    Color32::TRANSPARENT
                                },
                                Stroke::new(1.0, theme::border()),
                                ready && (voice_busy.is_none() || listening.is_some()),
                                move |p, c| {
                                    let s = Stroke::new(1.5, ink);
                                    p.rect_filled(
                                        egui::Rect::from_center_size(
                                            c + Vec2::new(0.0, -3.0),
                                            Vec2::new(6.0, 10.0),
                                        ),
                                        CornerRadius::same(3),
                                        ink,
                                    );
                                    p.line_segment([c + Vec2::new(-5.0, -1.0), c + Vec2::new(-5.0, 1.5)], s);
                                    p.line_segment([c + Vec2::new(-5.0, 1.5), c + Vec2::new(-2.5, 4.5)], s);
                                    p.line_segment([c + Vec2::new(-2.5, 4.5), c + Vec2::new(2.5, 4.5)], s);
                                    p.line_segment([c + Vec2::new(2.5, 4.5), c + Vec2::new(5.0, 1.5)], s);
                                    p.line_segment([c + Vec2::new(5.0, 1.5), c + Vec2::new(5.0, -1.0)], s);
                                    p.line_segment([c + Vec2::new(0.0, 4.5), c + Vec2::new(0.0, 8.0)], s);
                                },
                            )
                            .on_hover_text(if listening.is_some() {
                                "Stop: what you said is written into the message box"
                            } else {
                                "Speak: click, talk, click again. The sound is sent to the AI service to be written down, and not kept."
                            });
                            if mic_button.clicked() {
                                toggle_mic = true;
                            }
                            if let Some(secs) = listening {
                                ui.label(
                                    RichText::new(format!("Listening... {secs}s"))
                                        .small()
                                        .color(theme::err()),
                                );
                            } else if let Some(status) = &voice_busy {
                                ui.label(RichText::new(status).small().color(theme::muted()));
                            } else if app.playback.is_some()
                                && ui.small_button("Stop voice").clicked()
                            {
                                stop_voice = true;
                            }
                            let permission = app.settings.permission;
                            egui::ComboBox::from_id_salt("permission_combo")
                                .selected_text(
                                    RichText::new(format!("Permission: {}", permission.short_label()))
                                        .small()
                                        .color(theme::muted()),
                                )
                                .width(150.0)
                                .show_ui(ui, |ui| {
                                    for mode in crate::config::PermissionMode::ALL {
                                        if ui
                                            .selectable_label(mode == permission, mode.label())
                                            .clicked()
                                        {
                                            new_permission = Some(mode);
                                        }
                                    }
                                })
                                .response
                                .on_hover_text(
                                    "When the agent asks before it runs a command (installing packages, stopping programs, files outside the project). Dangerous commands are always refused.",
                                );
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if running {
                                    let stop_button = round_button(
                                        ui,
                                        32.0,
                                        theme::text_strong(),
                                        Stroke::NONE,
                                        true,
                                        |p, c| {
                                            p.rect_filled(
                                                egui::Rect::from_center_size(c, Vec2::splat(11.0)),
                                                CornerRadius::same(2),
                                                theme::bg(),
                                            );
                                        },
                                    )
                                    .on_hover_text("Stop");
                                    if stop_button.clicked() {
                                        stop = true;
                                    }
                                } else {
                                    let can_send = ready
                                        && app
                                            .open
                                            .get(&id)
                                            .map(|a| !a.input.trim().is_empty())
                                            .unwrap_or(false);
                                    let fill = if can_send {
                                        theme::accent()
                                    } else {
                                        theme::card_hover()
                                    };
                                    let ink = if can_send {
                                        theme::on_accent()
                                    } else {
                                        theme::faint()
                                    };
                                    let send_button = round_button(
                                        ui,
                                        32.0,
                                        fill,
                                        Stroke::NONE,
                                        can_send,
                                        move |p, c| {
                                            let s = Stroke::new(2.0, ink);
                                            p.line_segment(
                                                [c + Vec2::new(0.0, 6.0), c + Vec2::new(0.0, -6.0)],
                                                s,
                                            );
                                            p.line_segment(
                                                [c + Vec2::new(-5.5, -1.0), c + Vec2::new(0.0, -6.5)],
                                                s,
                                            );
                                            p.line_segment(
                                                [c + Vec2::new(5.5, -1.0), c + Vec2::new(0.0, -6.5)],
                                                s,
                                            );
                                        },
                                    )
                                    .on_hover_text("Send (Enter)");
                                    if send_button.clicked() {
                                        send = true;
                                    }
                                }
                            });
                        });
                    });
                ui.add_space(2.0);
                ui.vertical_centered(|ui| {
                    ui.label(
                        RichText::new("Enter to send, Shift+Enter for a new line")
                            .size(11.0)
                            .color(theme::faint()),
                    );
                    if show_star {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new("Did Gagent help? A star on GitHub helps others find it.")
                                    .size(11.0)
                                    .color(theme::faint()),
                            );
                            ui.hyperlink_to(
                                RichText::new("★ Star").size(11.0),
                                "https://github.com/layer8labs8/Gagent",
                            );
                            if ui.small_button("Hide").clicked() {
                                hide_star = true;
                            }
                        });
                    }
                });
            },
        );
    });

    if let Some(f) = remove_attachment {
        if let Some(a) = app.open.get_mut(&id) {
            a.attachments.retain(|x| x != &f);
        }
    }
    if let Some(mode) = new_permission {
        if mode != app.settings.permission {
            let mut s = app.settings.clone();
            s.permission = mode;
            app.commit_settings(s);
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
    if hide_star {
        app.settings.star_hint_hidden = true;
        app.save_settings();
    }
    if toggle_mic {
        app.toggle_recording();
    }
    if stop_voice {
        app.stop_voice();
    }
    if stop {
        app.stop_run();
    }
    if send {
        app.send_message();
    }
}

// ---------------------------------------------------------------------------
// Questions of the agent
// ---------------------------------------------------------------------------

/// The card that asks "may the agent run this?". The agent waits until it is answered.
pub fn approval_card(app: &mut GagentApp, ctx: &egui::Context) {
    use crate::permission::Decision;
    let Some(request) = app.approvals.pending() else {
        return;
    };
    let mut decision: Option<Decision> = None;
    egui::Area::new(egui::Id::unique("approval_card"))
        .anchor(egui::Align2::CENTER_BOTTOM, [0.0, -150.0])
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            Frame::new()
                .fill(theme::card_bg())
                .stroke(Stroke::new(1.5, theme::accent()))
                .corner_radius(CornerRadius::same(14))
                .inner_margin(Margin::symmetric(16, 12))
                .show(ui, |ui| {
                    ui.set_width(560.0f32.min(ui.ctx().content_rect().width() - 60.0));
                    ui.label(
                        RichText::new("The agent asks for your permission")
                            .strong()
                            .color(theme::text_strong()),
                    );
                    ui.label(
                        RichText::new(format!("It {}.", request.reason)).color(theme::muted()),
                    );
                    ui.add_space(6.0);
                    Frame::new()
                        .fill(theme::input_bg())
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(Margin::symmetric(10, 8))
                        .show(ui, |ui| {
                            egui::ScrollArea::vertical().max_height(110.0).show(ui, |ui| {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(truncate_end(&request.command, 1500))
                                            .monospace()
                                            .size(12.5)
                                            .color(theme::text()),
                                    )
                                    .wrap(),
                                );
                            });
                        });
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui
                            .add(
                                egui::Button::new(RichText::new("Allow").color(theme::on_accent()))
                                    .fill(theme::accent()),
                            )
                            .clicked()
                        {
                            decision = Some(Decision::Allow);
                        }
                        if ui
                            .button("Always allow this kind")
                            .on_hover_text("Do not ask again for the same kind of command until Gagent is closed")
                            .clicked()
                        {
                            decision = Some(Decision::Always);
                        }
                        if ui.button("Deny").clicked() {
                            decision = Some(Decision::Deny);
                        }
                    });
                });
        });
    if let Some(d) = decision {
        app.approvals.answer(request.id, d);
    }
}

/// "Do you trust this folder?", asked once for every folder. The agent does not open in it before.
pub fn trust_card(app: &mut GagentApp, ctx: &egui::Context) {
    let Some(id) = app.trust_ask.clone() else {
        return;
    };
    let Some(project) = app.open.get(&id).map(|a| a.session.meta.project.clone()) else {
        app.trust_ask = None;
        return;
    };
    let mut answer: Option<bool> = None;
    ctx.layer_painter(egui::LayerId::new(
        egui::Order::Middle,
        egui::Id::unique("trust_scrim"),
    ))
    .rect_filled(
        ctx.content_rect(),
        CornerRadius::ZERO,
        egui::Color32::from_black_alpha(120),
    );
    egui::Area::new(egui::Id::unique("trust_card"))
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            Frame::new()
                .fill(theme::card_bg())
                .stroke(Stroke::new(1.5, theme::accent()))
                .corner_radius(CornerRadius::same(14))
                .inner_margin(Margin::symmetric(18, 14))
                .show(ui, |ui| {
                    ui.set_width(520.0f32.min(ui.ctx().content_rect().width() - 60.0));
                    ui.label(
                        RichText::new("Do you trust this folder?")
                            .size(18.0)
                            .strong()
                            .color(theme::text_strong()),
                    );
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(truncate_end(&project, 90))
                            .monospace()
                            .size(12.5)
                            .color(theme::text()),
                    );
                    ui.add_space(8.0);
                    ui.add(
                        egui::Label::new(
                            RichText::new("Gagent lets an AI read the files here and run commands on your computer. Files can hide instructions that try to steer the AI. Continue only with a folder you wrote yourself or got from a source you trust.")
                                .color(theme::muted()),
                        )
                        .wrap(),
                    );
                    ui.add_space(4.0);
                    ui.add(
                        egui::Label::new(
                            RichText::new("For a folder you do not trust, choose Docker in Settings (Sandbox) first. See SECURITY.md.")
                                .small()
                                .color(theme::faint()),
                        )
                        .wrap(),
                    );
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("Yes, I trust this folder")
                                        .color(theme::on_accent()),
                                )
                                .fill(theme::accent()),
                            )
                            .clicked()
                        {
                            answer = Some(true);
                        }
                        if ui.button("No, do not open it").clicked() {
                            answer = Some(false);
                        }
                    });
                });
        });
    if let Some(yes) = answer {
        app.trust_ask = None;
        if yes {
            app.settings.trust_folder(&project);
            app.save_settings();
            app.open_engine(&id);
        } else if app.open.get(&id).is_some_and(|a| a.session.items.is_empty()) {
            app.delete_session(&id);
        } else if app.current.as_deref() == Some(id.as_str()) {
            app.current = None;
        }
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
        .anchor(egui::Align2::CENTER_BOTTOM, [0.0, -110.0])
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            Frame::new()
                .fill(if error {
                    theme::err_bg()
                } else {
                    theme::card_bg()
                })
                .stroke(Stroke::new(
                    1.0,
                    if error { theme::err() } else { theme::border() },
                ))
                .corner_radius(CornerRadius::same(12))
                .inner_margin(Margin::symmetric(14, 9))
                .show(ui, |ui| {
                    ui.set_max_width(640.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(truncate_end(&text, 400)).color(if error {
                            theme::err()
                        } else {
                            theme::text_strong()
                        }));
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
