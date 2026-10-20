//! Team projects in the window: the New project screen, the plan board and the controls.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;

use eframe::egui::{self, Align, CornerRadius, Frame, Layout, Margin, RichText, Stroke, Vec2};

use super::state::*;
use super::theme;
use super::views::{plain_button, primary_button};
use super::{apply_summary, worker, GagentApp};
use crate::agent::AgentEvent;
use crate::config::{Provider, Settings};
use crate::session::{ChatItem, Session};
use crate::team::{AgentSpec, Plan, Role, Status, TeamConfig, TeamEvent, DEFAULT_AGENT_STEPS};
use crate::util::truncate_end;
use crate::{modelstats, models};

// ---------------------------------------------------------------------------
// The New project screen
// ---------------------------------------------------------------------------

pub struct AgentDraft {
    pub provider: Provider,
    pub model: String,
    pub role: Role,
    /// Token budget in thousands.
    pub tokens_k: u32,
}

pub struct ProjectDraft {
    pub name: String,
    pub folder: String,
    pub goal: String,
    pub agents: Vec<AgentDraft>,
    pub error: Option<String>,
}

/// Models worth offering for a provider.
fn suggestions(settings: &Settings, live: &[String], provider: Provider) -> Vec<String> {
    if provider == settings.provider && !live.is_empty() {
        return models::picker_list(live, "", false);
    }
    let mut s = settings.clone();
    s.provider = provider;
    s.preset_models()
}

impl ProjectDraft {
    pub fn new(app: &GagentApp) -> Self {
        let s = &app.settings;
        let presets = s.preset_models();
        let first = presets
            .first()
            .cloned()
            .unwrap_or_else(|| s.active_model().to_string());
        let second = presets.get(1).cloned().unwrap_or_else(|| first.clone());
        let agent = |model: &str, role: Role, tokens_k: u32| AgentDraft {
            provider: s.provider,
            model: model.to_string(),
            role,
            tokens_k,
        };
        Self {
            name: String::new(),
            folder: app.new_project_path.clone(),
            goal: String::new(),
            agents: vec![
                agent(&first, Role::Lead, 150),
                agent(&second, Role::Coder, 300),
                agent(&first, Role::Tester, 200),
            ],
            error: None,
        }
    }

    fn specs(&self) -> Vec<AgentSpec> {
        self.agents
            .iter()
            .enumerate()
            .map(|(i, a)| AgentSpec {
                name: format!("{}{}", a.role.label(), i + 1),
                provider: a.provider,
                model: a.model.trim().to_string(),
                role: a.role,
                token_budget: u64::from(a.tokens_k) * 1000,
                max_steps: DEFAULT_AGENT_STEPS,
            })
            .collect()
    }
}

/// True for the names of picture files.
fn is_picture(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    ["png", "jpg", "jpeg", "gif", "webp"]
        .iter()
        .any(|ext| lower.ends_with(&format!(".{ext}")))
}

/// A team of one model in three roles, for when the models cannot be chosen automatically.
fn one_model_team(settings: &Settings) -> Vec<AgentSpec> {
    let model = settings.active_model().to_string();
    [(Role::Lead, 150u64), (Role::Coder, 300), (Role::Tester, 200)]
        .into_iter()
        .enumerate()
        .map(|(i, (role, k))| AgentSpec {
            name: format!("{}{}", role.label(), i + 1),
            provider: settings.provider,
            model: model.clone(),
            role,
            token_budget: k * 1000,
            max_steps: DEFAULT_AGENT_STEPS,
        })
        .collect()
}

impl GagentApp {
    pub(crate) fn open_project_draft(&mut self) {
        self.current = None;
        self.project_draft = Some(ProjectDraft::new(self));
    }

    /// Creates the project from the screen and starts planning once its workspace is ready.
    pub(crate) fn create_project(&mut self, draft: &ProjectDraft) -> Result<(), String> {
        let folder = draft.folder.trim();
        if folder.is_empty() || !std::path::Path::new(folder).is_dir() {
            return Err("Choose an existing project folder.".into());
        }
        let goal = draft.goal.trim();
        if goal.is_empty() {
            return Err("Describe what the team should build.".into());
        }
        let agents = draft.specs();
        if agents.iter().any(|a| a.model.is_empty()) {
            return Err("Every agent needs a model.".into());
        }
        crate::team::Team::new(agents.clone())?;
        for a in &agents {
            if !self.settings.provider_ready(a.provider) {
                return Err(format!(
                    "{} has no key yet. Add it in Settings.",
                    a.provider.label()
                ));
            }
        }
        let path = std::path::PathBuf::from(folder);
        let canonical = dunce::canonicalize(&path).unwrap_or(path);
        let project = canonical.to_string_lossy().into_owned();
        let title = if draft.name.trim().is_empty() {
            truncate_end(goal.lines().next().unwrap_or("Team project"), 40)
        } else {
            draft.name.trim().to_string()
        };
        let config = TeamConfig {
            goal: goal.to_string(),
            agents,
        };
        let mut session = Session::new_team(&project, &title, config);
        let _ = self.store.save(&mut session);
        self.settings.last_project = project.clone();
        self.new_project_path = project;
        self.save_settings();
        let id = session.meta.id.clone();
        let mut active = ActiveSession::new(session);
        if let Some(t) = active.team.as_mut() {
            t.start_when_ready = true;
        }
        self.open.insert(id.clone(), active);
        self.sessions = self.store.list();
        self.current = Some(id.clone());
        self.open_engine(&id);
        Ok(())
    }

    /// A big job written in the message box goes to a team: the lead plans, the agents are
    /// chosen by themselves (the best that are available), and they start after the user's yes.
    /// Returns true when the message was taken as a team project, false when one agent answers it.
    pub(crate) fn try_start_team(&mut self, id: &str) -> bool {
        use crate::config::TeamMode;
        let settings = self.settings.clone();
        let Some(a) = self.open.get(id) else {
            return false;
        };
        if a.run.is_some() || !matches!(a.engine, EngineState::Ready(_)) || !settings.is_configured() {
            return false;
        }
        let typed = a.input.trim().to_string();
        if typed.is_empty() {
            return false;
        }
        let (goal, forced) = crate::team::split_command(&typed);
        let goal = goal.to_string();
        let has_picture = a.attachments.iter().any(|f| is_picture(f));
        let wanted = !has_picture
            && !goal.is_empty()
            && match forced {
                Some(team) => team,
                None => match settings.team_mode {
                    TeamMode::Always => true,
                    TeamMode::Never => false,
                    TeamMode::Auto => crate::team::looks_big(&goal),
                },
            };
        if !wanted {
            // `/solo` and `/team` are not part of what the agent reads.
            if forced.is_some() {
                if let Some(a) = self.open.get_mut(id) {
                    a.input = goal;
                }
            }
            return false;
        }
        let agents = if settings.auto_pick {
            crate::router::suggest_team(&settings)
        } else {
            Vec::new()
        };
        let agents = if agents.is_empty() {
            one_model_team(&settings)
        } else {
            agents
        };
        let attachments = a.attachments.clone();
        let text = if attachments.is_empty() {
            goal.clone()
        } else {
            format!(
                "[Attached files in the workspace: {}]\n\n{goal}",
                attachments.join(", ")
            )
        };
        let config = TeamConfig {
            goal: text,
            agents,
        };
        if let Err(e) = config.team() {
            self.show_banner(e, true);
            return false;
        }
        let Some(a) = self.open.get_mut(id) else {
            return false;
        };
        a.session.set_title_from(&goal);
        a.session.items.push(ChatItem::User(goal));
        a.session.meta.team = Some(config.clone());
        a.team = Some(TeamUi::new(config));
        a.input.clear();
        a.attachments.clear();
        self.start_team(id, false);
        true
    }

    /// Plans (or continues) the project of session `id`.
    pub(crate) fn start_team(&mut self, id: &str, resume: bool) {
        let notify = self.notify.clone();
        let Some(a) = self.open.get_mut(id) else {
            return;
        };
        if a.run.is_some() {
            return;
        }
        let EngineState::Ready(engine) = &a.engine else {
            return;
        };
        let engine = engine.clone();
        let Some(t) = a.team.as_mut() else {
            return;
        };
        let team = match t.config.team() {
            Ok(team) => team,
            Err(e) => {
                a.session.items.push(ChatItem::Error(e));
                return;
            }
        };
        let gate = Arc::new(Gate::default());
        t.gate = gate.clone();
        t.phase = if resume && t.plan.is_some() {
            TeamPhase::Running
        } else {
            TeamPhase::Planning
        };
        let goal = t.config.goal.clone();
        if a.session.items.is_empty() {
            a.session.items.push(ChatItem::User(goal.clone()));
        }
        let cancel = Arc::new(AtomicBool::new(false));
        a.run = Some(RunState {
            cancel: cancel.clone(),
            status: if resume { "Continuing..." } else { "Planning..." }.into(),
            started: Instant::now(),
            usage: Default::default(),
            last_event: Instant::now(),
        });
        a.log_info("Team project started");
        a.scroll_to_bottom = true;
        self.save_session(id);
        worker::run_team(notify, id.to_string(), engine, team, goal, resume, gate, cancel);
    }

    /// The user's answer to "start this plan?".
    pub(crate) fn team_decision(&mut self, approve: bool) {
        if let Some(t) = self.active_mut().and_then(|a| a.team.as_mut()) {
            t.gate.set(approve);
            if approve {
                t.phase = TeamPhase::Running;
            }
        }
    }

    pub(crate) fn handle_team_event(&mut self, session: &str, event: TeamEvent) {
        let Some(a) = self.open.get_mut(session) else {
            return;
        };
        if let Some(run) = a.run.as_mut() {
            run.last_event = Instant::now();
        }
        let Some(t) = a.team.as_mut() else {
            return;
        };
        let mut forward: Option<AgentEvent> = None;
        match event {
            TeamEvent::Planning { lead, model } => {
                t.phase = TeamPhase::Planning;
                a.session.items.push(ChatItem::Notice(format!(
                    "{lead} ({}) is writing the plan.",
                    models::short_name(&model)
                )));
            }
            TeamEvent::PlanReady(plan) => {
                t.plan = Some(plan);
                if self.settings.team_auto_start {
                    t.gate.set(true);
                    t.phase = TeamPhase::Running;
                    a.session
                        .items
                        .push(ChatItem::Notice("The plan is ready. Starting.".into()));
                } else {
                    t.phase = TeamPhase::Review;
                    if let Some(run) = a.run.as_mut() {
                        run.status = "Waiting for your answer to the plan".into();
                    }
                    a.session.items.push(ChatItem::Notice(
                        "The plan is ready. Read it above, then press Start.".into(),
                    ));
                }
            }
            TeamEvent::Resumed(plan) => {
                t.plan = Some(plan);
                t.phase = TeamPhase::Running;
                a.session
                    .items
                    .push(ChatItem::Notice("Continuing the saved plan.".into()));
            }
            TeamEvent::TaskStart { id, owner, title } => {
                t.phase = TeamPhase::Running;
                t.tool_base += 1000;
                if let Some(task) = t
                    .plan
                    .as_mut()
                    .and_then(|p| p.tasks.iter_mut().find(|x| x.id == id))
                {
                    task.status = Status::Doing;
                    task.owner = owner.clone();
                }
                if let Some(run) = a.run.as_mut() {
                    run.status = format!("{owner}: {title}");
                }
                a.session
                    .items
                    .push(ChatItem::Notice(format!("{owner} starts {id}: {title}")));
            }
            TeamEvent::Agent { owner, event, .. } => {
                let base = t.tool_base;
                forward = Some(match event {
                    AgentEvent::Text(text) => AgentEvent::Text(format!("[{owner}] {text}")),
                    AgentEvent::ToolStart { id, name, summary } => AgentEvent::ToolStart {
                        id: base + id,
                        name,
                        summary: format!("[{owner}] {summary}"),
                    },
                    AgentEvent::ToolEnd {
                        id,
                        ok,
                        summary,
                        output,
                        millis,
                    } => AgentEvent::ToolEnd {
                        id: base + id,
                        ok,
                        summary,
                        output,
                        millis,
                    },
                    AgentEvent::Status(s) => AgentEvent::Status(format!("{owner}: {s}")),
                    other => other,
                });
            }
            TeamEvent::TaskEnd {
                id,
                status,
                summary,
                tokens,
            } => {
                t.tokens += tokens;
                if let Some(task) = t
                    .plan
                    .as_mut()
                    .and_then(|p| p.tasks.iter_mut().find(|x| x.id == id))
                {
                    task.status = status;
                    task.summary = summary.clone();
                }
                a.session.items.push(ChatItem::Notice(format!(
                    "{id} {}: {}",
                    status.label(),
                    truncate_end(&summary, 300)
                )));
            }
            TeamEvent::Note(text) => a.session.items.push(ChatItem::Notice(text)),
        }
        a.scroll_to_bottom = true;
        if let Some(e) = forward {
            self.handle_agent_event(session, e);
        }
    }

    pub(crate) fn handle_team_done(
        &mut self,
        session: &str,
        outcome: Result<TeamSummary, String>,
    ) {
        if let Some(a) = self.open.get_mut(session) {
            a.run = None;
            if let Some(t) = a.team.as_mut() {
                t.phase = TeamPhase::Idle;
            }
            match outcome {
                Ok(done) => {
                    let total = done.plan.tasks.len();
                    let finished = done.plan.count(Status::Done);
                    if let Some(t) = a.team.as_mut() {
                        if total > 0 {
                            t.plan = Some(done.plan.clone());
                        }
                    }
                    let text = if done.cancelled {
                        "Stopped. Press Continue to go on.".to_string()
                    } else if done.declined {
                        "The plan was not started.".to_string()
                    } else if finished == total {
                        format!("All {total} tasks are done ({} tokens).", done.tokens)
                    } else {
                        format!(
                            "{finished} of {total} tasks are done; {} failed, {} blocked. Press Continue to try the rest again.",
                            done.plan.count(Status::Failed),
                            done.plan.count(Status::Blocked)
                        )
                    };
                    a.log_info(&text);
                    a.session.items.push(ChatItem::Notice(text));
                    if let Some(report) = done.auto_applied {
                        a.applied.extend(report.applied.iter().cloned());
                        if report.checkpoint.is_some() {
                            a.checkpoint = report.checkpoint.clone();
                        }
                        a.session
                            .items
                            .push(ChatItem::Notice(apply_summary(&report)));
                    }
                }
                Err(e) => {
                    a.log_error(&e);
                    a.session.items.push(ChatItem::Error(e));
                }
            }
            a.scroll_to_bottom = true;
        }
        self.save_session(session);
        self.refresh_changes_for(session);
        self.refresh_git(session);
    }
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

fn status_color(status: Status) -> egui::Color32 {
    match status {
        Status::Todo => theme::faint(),
        Status::Doing => theme::accent(),
        Status::Done => theme::ok(),
        Status::Failed => theme::err(),
        Status::Blocked => theme::warn(),
    }
}

fn plan_rows(ui: &mut egui::Ui, plan: &Plan) {
    for t in &plan.tasks {
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(Vec2::splat(10.0), egui::Sense::hover());
            ui.painter()
                .circle_filled(rect.center(), 4.0, status_color(t.status));
            ui.label(RichText::new(&t.id).small().color(theme::faint()));
            let title = ui.add(
                egui::Label::new(RichText::new(truncate_end(&t.title, 70)).color(theme::text()))
                    .truncate(),
            );
            let mut tip = format!("{}: {}", t.status.label(), t.description);
            if !t.accept.is_empty() {
                tip.push_str(&format!("\nDone when: {}", t.accept));
            }
            if !t.deps.is_empty() {
                tip.push_str(&format!("\nAfter: {}", t.deps.join(", ")));
            }
            if !t.summary.is_empty() {
                tip.push_str(&format!("\n\n{}", t.summary));
            }
            title.on_hover_text(tip);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new(&t.owner).small().color(theme::muted()));
            });
        });
    }
}

/// The card at the top of a team project: goal, team and plan.
pub fn board(ui: &mut egui::Ui, view: &TeamView) {
    Frame::new()
        .fill(theme::card_bg())
        .stroke(Stroke::new(1.0, theme::border()))
        .corner_radius(CornerRadius::same(14))
        .inner_margin(Margin::same(14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("TEAM PROJECT").small().strong().color(theme::faint()));
            ui.label(RichText::new(truncate_end(&view.goal, 400)).color(theme::text_strong()));
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                for a in &view.agents {
                    ui.label(
                        RichText::new(format!(
                            "{} | {} | {}{}",
                            a.name,
                            a.role.label(),
                            models::short_name(&a.model),
                            modelstats::hint(&a.model)
                        ))
                        .small()
                        .color(theme::muted()),
                    );
                }
            });
            ui.add_space(8.0);
            match &view.plan {
                Some(plan) => {
                    let done = plan.count(Status::Done);
                    ui.label(
                        RichText::new(format!(
                            "Plan: {done} of {} tasks done  |  {} tokens",
                            plan.tasks.len(),
                            view.tokens
                        ))
                        .small()
                        .color(theme::faint()),
                    );
                    if !plan.conventions.trim().is_empty() {
                        egui::CollapsingHeader::new(
                            RichText::new("Contract between the parts")
                                .small()
                                .color(theme::muted()),
                        )
                        .id_salt("team_contract")
                        .show(ui, |ui| {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(&plan.conventions)
                                        .small()
                                        .color(theme::muted()),
                                )
                                .wrap(),
                            );
                        });
                    }
                    ui.add_space(4.0);
                    egui::ScrollArea::vertical()
                        .id_salt("team_plan_rows")
                        .max_height(150.0)
                        .auto_shrink([false, true])
                        .show(ui, |ui| plan_rows(ui, plan));
                }
                None if view.phase == TeamPhase::Planning => {
                    ui.horizontal(|ui| {
                        theme::spinner(ui, 14.0);
                        ui.label(RichText::new("The lead is writing the plan...").color(theme::muted()));
                    });
                }
                None => {
                    ui.label(RichText::new("No plan yet.").color(theme::muted()));
                }
            }
        });
}

/// The bar at the bottom of a team project: what can be done now.
pub fn bar(app: &mut GagentApp, ui: &mut egui::Ui) {
    let Some(id) = app.current.clone() else {
        return;
    };
    let Some(a) = app.open.get(&id) else {
        return;
    };
    let Some(t) = a.team.as_ref() else {
        return;
    };
    let ready = matches!(a.engine, EngineState::Ready(_));
    let phase = t.phase;
    let status = a.run.as_ref().map(|r| (r.status.clone(), r.started.elapsed().as_secs()));
    let unfinished = t
        .plan
        .as_ref()
        .map(|p| p.tasks.iter().any(|x| x.status != Status::Done))
        .unwrap_or(false);
    let has_plan = t.plan.is_some();

    let (mut start, mut cancel_plan, mut stop, mut go_on) = (false, false, false, false);
    ui.add_space(4.0);
    ui.horizontal(|ui| match phase {
        TeamPhase::Review => {
            if primary_button(ui, "Start").clicked() {
                start = true;
            }
            if plain_button(ui, "Cancel").clicked() {
                cancel_plan = true;
            }
            ui.label(
                RichText::new("Start lets the agents work. This uses tokens.")
                    .small()
                    .color(theme::faint()),
            );
        }
        TeamPhase::Planning | TeamPhase::Running => {
            theme::spinner(ui, 14.0);
            if let Some((text, secs)) = &status {
                ui.label(RichText::new(truncate_end(text, 80)).color(theme::muted()));
                ui.label(RichText::new(format!("{secs}s")).small().color(theme::faint()));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if plain_button(ui, "Stop").clicked() {
                    stop = true;
                }
            });
        }
        TeamPhase::Idle => {
            if has_plan && unfinished {
                if ui
                    .add_enabled(ready, egui::Button::new(RichText::new("Continue").strong()))
                    .clicked()
                {
                    go_on = true;
                }
                ui.label(
                    RichText::new("Tasks that are not done are started again.")
                        .small()
                        .color(theme::faint()),
                );
            } else if has_plan {
                // All tasks are done: the board says so, and the message box is free again.
            } else if ready && primary_button(ui, "Create plan").clicked() {
                go_on = true;
            }
        }
    });
    ui.add_space(4.0);

    if start {
        app.team_decision(true);
    }
    if cancel_plan {
        app.team_decision(false);
    }
    if stop {
        app.stop_run();
    }
    if go_on {
        app.start_team(&id, has_plan);
    }
}

/// The New project screen.
pub fn draft_screen(app: &mut GagentApp, ui: &mut egui::Ui) {
    let Some(mut draft) = app.project_draft.take() else {
        return;
    };
    let ready: Vec<Provider> = Provider::ALL
        .into_iter()
        .filter(|p| app.settings.provider_ready(*p))
        .collect();
    let choices: Vec<(Provider, Vec<String>)> = Provider::ALL
        .into_iter()
        .map(|p| (p, suggestions(&app.settings, &app.models, p)))
        .collect();
    let (mut create, mut cancel, mut add, mut auto_team) = (false, false, false, false);
    let mut remove: Option<usize> = None;

            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.allocate_ui_with_layout(
                            Vec2::new(ui.available_width().min(760.0), 10.0),
                            Layout::top_down(Align::Min),
                            |ui| {
                                ui.add_space(24.0);
                                ui.label(
                                    RichText::new("New project")
                                        .size(22.0)
                                        .strong()
                                        .color(theme::text_strong()),
                                );
                                ui.label(
                                    RichText::new("Several AI agents build it together. The lead writes a plan, you approve it, then each agent does its tasks.")
                                        .color(theme::muted()),
                                );
                                ui.add_space(14.0);

                                ui.label(RichText::new("Name (optional)").small().color(theme::faint()));
                                ui.add(
                                    egui::TextEdit::singleline(&mut draft.name)
                                        .desired_width(f32::INFINITY),
                                );
                                ui.add_space(6.0);
                                ui.label(RichText::new("Folder").small().color(theme::faint()));
                                ui.horizontal(|ui| {
                                    ui.add(
                                        egui::TextEdit::singleline(&mut draft.folder)
                                            .desired_width(ui.available_width() - 90.0),
                                    );
                                    if plain_button(ui, "Browse").clicked() {
                                        if let Some(p) = rfd::FileDialog::new()
                                            .set_title("Project folder")
                                            .pick_folder()
                                        {
                                            draft.folder = p.to_string_lossy().into_owned();
                                        }
                                    }
                                });
                                ui.add_space(6.0);
                                ui.label(RichText::new("What should the team build?").small().color(theme::faint()));
                                ui.add(
                                    egui::TextEdit::multiline(&mut draft.goal)
                                        .desired_rows(4)
                                        .desired_width(f32::INFINITY)
                                        .hint_text("A notes app with search and tags, in HTML, CSS and JavaScript..."),
                                );

                                ui.add_space(14.0);
                                ui.label(RichText::new("Team").small().strong().color(theme::faint()));
                                ui.label(
                                    RichText::new("The agent with the role lead plans the work. Reply times come from earlier requests; a model that is slow or not available is marked in the list.")
                                        .small()
                                        .color(theme::faint()),
                                );
                                ui.add_space(4.0);
                                for i in 0..draft.agents.len() {
                                    ui.horizontal(|ui| {
                                        let a = &mut draft.agents[i];
                                        egui::ComboBox::from_id_salt(("team_provider", i))
                                            .selected_text(a.provider.label())
                                            .width(120.0)
                                            .show_ui(ui, |ui| {
                                                for p in &ready {
                                                    ui.selectable_value(&mut a.provider, *p, p.label());
                                                }
                                            });
                                        ui.add(
                                            egui::TextEdit::singleline(&mut a.model)
                                                .desired_width(230.0)
                                                .hint_text("model id"),
                                        );
                                        egui::ComboBox::from_id_salt(("team_model", i))
                                            .selected_text("list")
                                            .width(50.0)
                                            .show_ui(ui, |ui| {
                                                if let Some((_, list)) = choices.iter().find(|(p, _)| *p == a.provider) {
                                                    for m in list {
                                                        let label = format!(
                                                            "{}{}",
                                                            models::label(m),
                                                            modelstats::hint(m)
                                                        );
                                                        if ui.selectable_label(a.model == *m, label).clicked() {
                                                            a.model = m.clone();
                                                        }
                                                    }
                                                }
                                            });
                                        egui::ComboBox::from_id_salt(("team_role", i))
                                            .selected_text(a.role.label())
                                            .width(80.0)
                                            .show_ui(ui, |ui| {
                                                for r in Role::ALL {
                                                    ui.selectable_value(&mut a.role, r, r.label());
                                                }
                                            });
                                        ui.add(
                                            egui::DragValue::new(&mut a.tokens_k)
                                                .range(10..=5000)
                                                .suffix("k tokens"),
                                        )
                                        .on_hover_text("How many tokens this agent may use in total");
                                        if i > 0 && ui.small_button("x").on_hover_text("Remove this agent").clicked() {
                                            remove = Some(i);
                                        }
                                    });
                                }
                                ui.add_space(6.0);
                                ui.horizontal(|ui| {
                                    if draft.agents.len() < 12 && plain_button(ui, "+ Add agent").clicked() {
                                        add = true;
                                    }
                                    if plain_button(ui, "Auto team")
                                        .on_hover_text("Make the team from the services that have a key: a strong lead, two coders from different services (so their limits are separate) and a tester that can see images")
                                        .clicked()
                                    {
                                        auto_team = true;
                                    }
                                });
                                if let Some(e) = &draft.error {
                                    ui.add_space(8.0);
                                    ui.label(RichText::new(e).color(theme::err()));
                                }
                                ui.add_space(14.0);
                                ui.horizontal(|ui| {
                                    if primary_button(ui, "Create plan").clicked() {
                                        create = true;
                                    }
                                    if plain_button(ui, "Cancel").clicked() {
                                        cancel = true;
                                    }
                                    ui.label(
                                        RichText::new("The lead writes the plan first. Nothing else runs until you press Start.")
                                            .small()
                                            .color(theme::faint()),
                                    );
                                });
                                ui.add_space(30.0);
                            },
                        );
                    });
                });

    if let Some(i) = remove {
        if draft.agents.len() > 1 {
            draft.agents.remove(i);
        }
    }
    if auto_team {
        let suggested = crate::router::suggest_team(&app.settings);
        if suggested.is_empty() {
            draft.error = Some("No AI service is ready. Add a key in Settings first.".into());
        } else {
            draft.error = None;
            draft.agents = suggested
                .into_iter()
                .map(|s| AgentDraft {
                    provider: s.provider,
                    model: s.model,
                    role: s.role,
                    tokens_k: (s.token_budget / 1000) as u32,
                })
                .collect();
        }
    }
    if add {
        let last = draft.agents.last().map(|a| (a.provider, a.model.clone()));
        let (provider, model) = last.unwrap_or((app.settings.provider, app.settings.active_model().to_string()));
        draft.agents.push(AgentDraft {
            provider,
            model,
            role: Role::Coder,
            tokens_k: 300,
        });
    }
    if cancel {
        return;
    }
    if create {
        match app.create_project(&draft) {
            Ok(()) => return,
            Err(e) => draft.error = Some(e),
        }
    }
    app.project_draft = Some(draft);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::AgentEvent;
    use crate::config::AppDirs;
    use crate::session::Session;
    use crate::team::{Plan, TeamConfig, TeamEvent};
    use std::sync::atomic::Ordering;

    fn config() -> TeamConfig {
        TeamConfig {
            goal: "a notes page".into(),
            agents: vec![
                AgentSpec::parse("gemini:lead-model:lead", 0).unwrap(),
                AgentSpec::parse("gemini:coder-model", 1).unwrap(),
            ],
        }
    }

    fn app() -> (GagentApp, String, tempfile::TempDir) {
        let home = tempfile::tempdir().unwrap();
        let mut app = GagentApp::new(AppDirs::at(home.path()), egui::Context::default());
        let session = Session::new_team("C:/work", "Notes", config());
        let id = session.meta.id.clone();
        app.open.insert(id.clone(), ActiveSession::new(session));
        app.current = Some(id.clone());
        (app, id, home)
    }

    fn plan() -> Plan {
        Plan::parse(
            r#"{"conventions":"ids: #list","tasks":[
              {"id":"t1","title":"Page","owner":"coder2"},
              {"id":"t2","title":"Styles","owner":"coder2","deps":["t1"]}]}"#,
        )
        .unwrap()
    }

    fn tool_ids(app: &GagentApp, id: &str) -> Vec<u32> {
        app.open[id]
            .session
            .items
            .iter()
            .filter_map(|i| match i {
                ChatItem::Tool { id, .. } => Some(*id),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn events_become_a_board_and_a_feed() {
        let (mut app, id, _home) = app();
        app.handle_team_event(
            &id,
            TeamEvent::Planning {
                lead: "lead1".into(),
                model: "vendor/lead-model".into(),
            },
        );
        assert_eq!(app.open[&id].team.as_ref().unwrap().phase, TeamPhase::Planning);
        app.handle_team_event(&id, TeamEvent::PlanReady(plan()));
        let t = app.open[&id].team.as_ref().unwrap();
        assert_eq!(t.phase, TeamPhase::Review);
        assert_eq!(t.plan.as_ref().unwrap().tasks.len(), 2);

        // Every task has its own range of tool ids, so two tasks never clash.
        for task in ["t1", "t2"] {
            app.handle_team_event(
                &id,
                TeamEvent::TaskStart {
                    id: task.into(),
                    owner: "coder2".into(),
                    title: "x".into(),
                },
            );
            app.handle_team_event(
                &id,
                TeamEvent::Agent {
                    task: task.into(),
                    owner: "coder2".into(),
                    event: AgentEvent::ToolStart {
                        id: 1,
                        name: "write_file".into(),
                        summary: "Write a.html".into(),
                    },
                },
            );
            app.handle_team_event(
                &id,
                TeamEvent::Agent {
                    task: task.into(),
                    owner: "coder2".into(),
                    event: AgentEvent::ToolEnd {
                        id: 1,
                        ok: true,
                        summary: "1 lines".into(),
                        output: String::new(),
                        millis: 5,
                    },
                },
            );
            app.handle_team_event(
                &id,
                TeamEvent::TaskEnd {
                    id: task.into(),
                    status: Status::Done,
                    summary: "made it".into(),
                    tokens: 100,
                },
            );
        }
        assert_eq!(tool_ids(&app, &id), vec![1001, 2001]);
        let items = &app.open[&id].session.items;
        assert!(items.iter().any(|i| matches!(i, ChatItem::Tool { summary, ok: Some(true), .. } if summary == "[coder2] Write a.html")));
        let t = app.open[&id].team.as_ref().unwrap();
        let plan = t.plan.as_ref().unwrap();
        assert_eq!(plan.count(Status::Done), 2);
        assert_eq!(plan.tasks[0].summary, "made it");
        assert_eq!(t.tokens, 200);

        app.handle_team_done(
            &id,
            Ok(TeamSummary {
                plan: plan.clone(),
                tokens: 200,
                cancelled: false,
                declined: false,
                auto_applied: None,
            }),
        );
        let a = &app.open[&id];
        assert!(a.run.is_none());
        assert_eq!(a.team.as_ref().unwrap().phase, TeamPhase::Idle);
        assert!(matches!(a.session.items.last(), Some(ChatItem::Notice(t)) if t.contains("All 2 tasks are done")));
    }

    #[test]
    fn the_plan_waits_for_the_users_answer() {
        let gate = Arc::new(Gate::default());
        let no_cancel = AtomicBool::new(false);
        let waiter = {
            let gate = gate.clone();
            std::thread::spawn(move || gate.wait(&AtomicBool::new(false)))
        };
        std::thread::sleep(std::time::Duration::from_millis(100));
        gate.set(true);
        assert!(waiter.join().unwrap());
        // Stopping the project ends the wait with a no.
        let gate = Arc::new(Gate::default());
        let cancel = Arc::new(AtomicBool::new(false));
        let waiter = {
            let (gate, cancel) = (gate.clone(), cancel.clone());
            std::thread::spawn(move || gate.wait(&cancel))
        };
        std::thread::sleep(std::time::Duration::from_millis(100));
        cancel.store(true, Ordering::Relaxed);
        assert!(!waiter.join().unwrap());
        let _ = no_cancel;
    }

    #[test]
    fn the_new_project_screen_starts_with_a_working_team() {
        let (app, _id, _home) = app();
        let draft = ProjectDraft::new(&app);
        assert_eq!(draft.agents.len(), 3);
        let specs = draft.specs();
        assert_eq!(specs[0].role, Role::Lead);
        assert_eq!(specs[0].name, "lead1");
        assert!(crate::team::Team::new(specs).is_ok());
    }
}
