//! The right-hand panel (changes, sandbox terminal) and the settings window.

use eframe::egui::{
    self, Align, CornerRadius, Frame, Key, Layout, Margin, Panel, RichText, Stroke, Vec2,
};

use super::state::*;
use super::theme;
use super::views::{heading_label, plain_button, primary_button, status_dot};
use super::GagentApp;
use crate::shadow::ChangeKind;
use crate::util::{format_local, human_bytes, truncate_end};

pub fn side_panel(app: &mut GagentApp, ui: &mut egui::Ui) {
    Panel::right("side_panel")
        .default_size(420.0)
        .size_range(340.0..=780.0)
        .resizable(true)
        .frame(
            Frame::new()
                .fill(theme::panel_bg())
                .stroke(Stroke::new(1.0, theme::border()))
                .inner_margin(Margin::same(14)),
        )
        .show(ui, |ui| {
            tab_bar(app, ui);
            ui.add_space(8.0);
            match app.tab {
                Tab::Changes => changes_tab(app, ui),
                Tab::Sandbox => sandbox_tab(app, ui),
            }
        });
}

fn tab_bar(app: &mut GagentApp, ui: &mut egui::Ui) {
    let pending = app.active().map(|a| a.changes.len()).unwrap_or(0);
    ui.horizontal(|ui| {
        for (tab, label) in [
            (
                Tab::Changes,
                if pending > 0 {
                    format!("Changes ({pending})")
                } else {
                    "Changes".to_string()
                },
            ),
            (Tab::Sandbox, "Sandbox".to_string()),
        ] {
            let selected = app.tab == tab;
            let text = RichText::new(label).strong().color(if selected {
                theme::accent()
            } else {
                theme::muted()
            });
            let response = ui.add(egui::Button::new(text).frame(false));
            if selected {
                let r = response.rect;
                ui.painter().line_segment(
                    [
                        egui::pos2(r.left() + 4.0, r.bottom() + 1.0),
                        egui::pos2(r.right() - 4.0, r.bottom() + 1.0),
                    ],
                    Stroke::new(2.0, theme::accent()),
                );
            }
            if response.clicked() {
                app.tab = tab;
                if tab == Tab::Changes {
                    app.refresh_changes();
                }
                if tab == Tab::Sandbox && app.docker.is_none() {
                    app.check_docker();
                }
            }
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui
                .add(egui::Button::new("×").frame(false))
                .on_hover_text("Close this panel")
                .clicked()
            {
                app.right_open = false;
            }
        });
    });
    ui.separator();
}

// ---------------------------------------------------------------------------
// Changes
// ---------------------------------------------------------------------------

fn changes_tab(app: &mut GagentApp, ui: &mut egui::Ui) {
    let Some(a) = app.active() else {
        return;
    };
    if !matches!(a.engine, EngineState::Ready(_)) {
        ui.label(RichText::new("The workspace is not ready yet.").color(theme::faint()));
        return;
    }
    let changes = a.changes.clone();
    let selected = a.selected.clone();
    let diff_for = a.diff_for.clone();
    let diff_text = a.diff_text.clone();
    let backups = a.backups.clone();
    let loading = a.changes_loading;
    let error = a.changes_error.clone();
    let confirm = a.confirm_discard;
    let running = a.is_running();

    git_box(app, ui);

    let mut toggle: Option<String> = None;
    let mut view: Option<String> = None;
    let mut apply: Option<Vec<String>> = None;
    let mut discard_all = false;
    let mut set_confirm = false;
    let mut discard_selected = false;
    let mut undo: Option<std::path::PathBuf> = None;
    let mut refresh = false;
    let mut auto = app.settings.auto_apply;

    heading_label(ui, "Pending changes");
    ui.label(
        RichText::new(if auto {
            "The agent works on a private copy. When a task finishes its changes are applied to your project; every apply is saved, so you can undo it below."
        } else {
            "The agent works on a private copy. Nothing reaches your project until you apply it."
        })
        .small()
        .color(theme::faint()),
    );
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.checkbox(&mut auto, "Apply automatically when a task finishes");
    });
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        if loading {
            theme::spinner(ui, 12.0);
        }
        if plain_button(ui, "Refresh").clicked() {
            refresh = true;
        }
        let have = !changes.is_empty();
        let any_selected = !selected.is_empty();
        if ui
            .add_enabled(
                have && !running,
                egui::Button::new(
                    RichText::new("Apply all")
                        .strong()
                        .color(theme::on_accent()),
                )
                .fill(theme::accent()),
            )
            .clicked()
        {
            apply = Some(changes.iter().map(|c| c.rel.clone()).collect());
        }
        if ui
            .add_enabled(
                any_selected && !running,
                egui::Button::new("Apply selected"),
            )
            .clicked()
        {
            apply = Some(selected.iter().cloned().collect());
        }
    });
    ui.horizontal(|ui| {
        let have = !changes.is_empty();
        let label = if confirm {
            "Click again to discard"
        } else {
            "Discard all"
        };
        if ui
            .add_enabled(
                have && !running,
                egui::Button::new(RichText::new(label).color(theme::err())),
            )
            .clicked()
        {
            if confirm {
                discard_all = true;
            } else {
                set_confirm = true;
            }
        }
        if ui
            .add_enabled(
                !selected.is_empty() && !running,
                egui::Button::new(RichText::new("Discard selected").color(theme::err())),
            )
            .clicked()
        {
            discard_selected = true;
        }
    });
    ui.add_space(6.0);
    if let Some(e) = &error {
        ui.label(RichText::new(e).color(theme::err()));
    }

    let list_height = if diff_for.is_some() { 170.0 } else { 300.0 };
    if changes.is_empty() {
        ui.add_space(10.0);
        ui.label(
            RichText::new("No pending changes. The agent's copy matches your project.")
                .color(theme::faint()),
        );
    } else {
        egui::ScrollArea::vertical()
            .id_salt("changes_list")
            .max_height(list_height)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for c in &changes {
                    ui.horizontal(|ui| {
                        let mut checked = selected.contains(&c.rel);
                        if ui.checkbox(&mut checked, "").changed() {
                            toggle = Some(c.rel.clone());
                        }
                        let (letter, color) = match c.kind {
                            ChangeKind::Added => ("A", theme::ok()),
                            ChangeKind::Modified => ("M", theme::warn()),
                            ChangeKind::Deleted => ("D", theme::err()),
                        };
                        ui.label(RichText::new(letter).monospace().strong().color(color));
                        let is_open = diff_for.as_deref() == Some(c.rel.as_str());
                        let text = RichText::new(truncate_end(&c.rel, 48))
                            .monospace()
                            .size(13.0)
                            .color(if is_open {
                                theme::accent()
                            } else {
                                theme::text()
                            });
                        if ui
                            .add(egui::Button::new(text).frame(false))
                            .on_hover_text("Show the differences")
                            .clicked()
                        {
                            view = Some(c.rel.clone());
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(
                                RichText::new(human_bytes(c.size))
                                    .small()
                                    .color(theme::faint()),
                            );
                        });
                    });
                }
            });
    }

    if let Some(rel) = &diff_for {
        ui.add_space(6.0);
        ui.separator();
        ui.label(
            RichText::new(rel)
                .monospace()
                .strong()
                .color(theme::text_strong()),
        );
        egui::ScrollArea::both()
            .id_salt("diff_view")
            .auto_shrink([false, false])
            .max_height(ui.available_height() - 90.0)
            .show(ui, |ui| {
                for line in diff_text.lines() {
                    let color = if line.starts_with("+++") || line.starts_with("---") {
                        theme::muted()
                    } else if line.starts_with('+') {
                        theme::ok()
                    } else if line.starts_with('-') {
                        theme::err()
                    } else if line.starts_with("@@") {
                        theme::accent()
                    } else {
                        theme::muted()
                    };
                    ui.add(
                        egui::Label::new(RichText::new(line).monospace().size(12.5).color(color))
                            .extend(),
                    );
                }
            });
    }

    if !backups.is_empty() {
        ui.add_space(6.0);
        ui.separator();
        heading_label(ui, "Undo");
        for b in backups.iter().take(3) {
            let label = format!(
                "Undo the apply from {} ({} file(s))",
                format_local(b.created_at),
                b.files
            );
            if ui
                .add_enabled(!running, egui::Button::new(RichText::new(label).small()))
                .on_hover_text(
                    "Restores your project files from the backup taken before that apply",
                )
                .clicked()
            {
                undo = Some(b.dir.clone());
            }
        }
    }

    // Apply the collected actions.
    if auto != app.settings.auto_apply {
        app.settings.auto_apply = auto;
        app.save_settings();
        for s in app.open.values_mut() {
            s.engine_dirty = true;
        }
        app.refresh_engines_if_idle();
    }
    // Any other action cancels a pending "click again to discard".
    let other_action = toggle.is_some()
        || refresh
        || view.is_some()
        || apply.is_some()
        || discard_selected
        || undo.is_some();
    if let Some(a) = app.active_mut() {
        if let Some(rel) = toggle {
            if !a.selected.remove(&rel) {
                a.selected.insert(rel);
            }
        }
        if set_confirm {
            a.confirm_discard = true;
        } else if other_action {
            a.confirm_discard = false;
        }
    }
    if refresh {
        app.refresh_changes();
    }
    if let Some(rel) = view {
        app.show_diff(&rel);
    }
    if let Some(rels) = apply {
        app.apply_changes(rels, false);
    }
    if discard_all {
        if let Some(a) = app.active_mut() {
            a.confirm_discard = false;
        }
        app.discard_changes(None);
    }
    if discard_selected {
        let rels: Vec<String> = app
            .active()
            .map(|a| a.selected.iter().cloned().collect())
            .unwrap_or_default();
        if let Some(a) = app.active_mut() {
            a.selected.clear();
        }
        app.discard_changes(Some(rels));
    }
    if let Some(dir) = undo {
        app.revert_backup(dir);
    }
}

/// Git of the project: the checkpoint, the diff of the agent's files and a commit of them.
fn git_box(app: &mut GagentApp, ui: &mut egui::Ui) {
    let Some(a) = app.active() else {
        return;
    };
    let Some(g) = a.git.clone() else {
        return;
    };
    let mine: Vec<String> = a.applied.iter().filter(|f| g.changed.contains(*f)).cloned().collect();
    let checkpoint = a.checkpoint.clone();
    let diff_text = a.git_diff.clone();
    let draft = a.commit_draft.clone();
    let title = a.session.meta.title.clone();
    let busy = a.is_running();

    let (mut show_diff, mut close_diff, mut start_commit) = (false, false, false);
    let (mut cancel_commit, mut do_commit) = (false, false);
    let mut new_draft = draft.clone();
    Frame::new()
        .fill(theme::card_bg())
        .stroke(Stroke::new(1.0, theme::border()))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            heading_label(ui, "Git");
            ui.label(
                RichText::new(format!(
                    "Branch {}  |  {} file(s) with changes that are not committed",
                    g.branch,
                    g.changed.len()
                ))
                .small()
                .color(theme::muted()),
            );
            if let Some(c) = &checkpoint {
                ui.add(
                    egui::Label::new(
                        RichText::new(format!(
                            "Checkpoint {c} was saved before the agent's changes reached your project. `git diff {c}` shows what changed since."
                        ))
                        .small()
                        .color(theme::faint()),
                    )
                    .wrap(),
                );
            }
            if mine.is_empty() {
                ui.label(
                    RichText::new("No change of the agent is waiting for a commit.")
                        .small()
                        .color(theme::faint()),
                );
            } else {
                ui.label(format!("{} file(s) the agent changed are not committed.", mine.len()));
                ui.horizontal(|ui| {
                    if plain_button(ui, "Show git diff").clicked() {
                        show_diff = true;
                    }
                    if ui
                        .add_enabled(
                            !busy && draft.is_none(),
                            egui::Button::new(RichText::new("Commit...").strong()),
                        )
                        .on_hover_text("Commits these files only; your other changes stay as they are")
                        .clicked()
                    {
                        start_commit = true;
                    }
                });
                if let Some(text) = new_draft.as_mut() {
                    ui.add(
                        egui::TextEdit::singleline(text)
                            .hint_text("Commit message")
                            .desired_width(f32::INFINITY),
                    );
                    ui.horizontal(|ui| {
                        if primary_button(ui, "Commit").clicked() {
                            do_commit = true;
                        }
                        if plain_button(ui, "Cancel").clicked() {
                            cancel_commit = true;
                        }
                    });
                }
            }
            if let Some(text) = &diff_text {
                ui.separator();
                egui::ScrollArea::both()
                    .id_salt("git_diff_view")
                    .max_height(220.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for line in text.lines() {
                            let color = if line.starts_with("+++") || line.starts_with("---") {
                                theme::muted()
                            } else if line.starts_with('+') {
                                theme::ok()
                            } else if line.starts_with('-') {
                                theme::err()
                            } else if line.starts_with("@@") {
                                theme::accent()
                            } else {
                                theme::muted()
                            };
                            ui.add(
                                egui::Label::new(
                                    RichText::new(line).monospace().size(12.5).color(color),
                                )
                                .extend(),
                            );
                        }
                    });
                if plain_button(ui, "Close diff").clicked() {
                    close_diff = true;
                }
            }
        });
    ui.add_space(8.0);

    if show_diff {
        app.git_show_diff();
    }
    if start_commit {
        if let Some(a) = app.active_mut() {
            a.commit_draft = Some(format!("Gagent: {}", truncate_end(&title, 60)));
        }
    } else if cancel_commit {
        if let Some(a) = app.active_mut() {
            a.commit_draft = None;
        }
    } else if do_commit {
        app.git_commit(new_draft.clone().unwrap_or_default());
    } else if new_draft != draft {
        if let Some(a) = app.active_mut() {
            a.commit_draft = new_draft;
        }
    }
    if close_diff {
        if let Some(a) = app.active_mut() {
            a.git_diff = None;
        }
    }
}

// ---------------------------------------------------------------------------
// Sandbox
// ---------------------------------------------------------------------------

fn sandbox_tab(app: &mut GagentApp, ui: &mut egui::Ui) {
    heading_label(ui, "Sandbox");
    let wsl = app.docker.as_ref().is_some_and(|s| s.is_wsl());
    let host = app.docker.as_ref().is_some_and(|s| s.is_host());
    ui.label(
        RichText::new(if host {
            "Code runs directly on your computer, in the agent's private copy, at low priority. No virtual machine and nothing to install, so it is fast and light. It is not isolated: commands that reach outside the private copy, sudo, shutdown or killing programs are refused."
        } else if wsl {
            "Code runs in your WSL Ubuntu, in the agent's private copy. It is light on memory and disk, but not isolated from your Windows files."
        } else {
            "Code runs in an isolated Ubuntu container: no root, no host files except the agent's private copy."
        })
        .small()
        .color(theme::faint()),
    );
    ui.add_space(6.0);

    let status = app.docker.clone();
    match &status {
        None => status_dot(ui, theme::faint(), "Checking the sandbox..."),
        Some(s) if s.is_ready() => status_dot(ui, theme::ok(), &s.describe()),
        Some(s) => {
            ui.label(RichText::new(s.describe()).color(theme::warn()));
        }
    }
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        if plain_button(ui, "Check again").clicked() {
            app.check_docker();
        }
        let build_label = if app.building_image {
            "Building image..."
        } else {
            "Build / rebuild image"
        };
        if !wsl
            && !host
            && ui
                .add_enabled(!app.building_image, egui::Button::new(build_label))
                .clicked()
        {
            app.build_image();
        }
        if app.building_image {
            theme::spinner(ui, 13.0);
        }
        if !host
            && plain_button(
                ui,
                if wsl {
                    "Stop running commands"
                } else {
                    "Reset container"
                },
            )
            .on_hover_text("Stops and removes the container; it starts again on the next command")
            .clicked()
        {
            app.reset_sandbox();
        }
    });
    ui.add_space(4.0);
    if !wsl && !host {
        let mut network = app.settings.sandbox.network;
        if ui
            .checkbox(
                &mut network,
                "Allow network access (pip/npm installs, downloads)",
            )
            .changed()
        {
            let mut s = app.settings.clone();
            s.sandbox.network = network;
            app.commit_settings(s);
        }
        ui.label(
            RichText::new(format!(
                "Limits: {} MB memory, {} CPU(s), {} s per command. Change them in Settings.",
                app.settings.sandbox.memory_mb,
                app.settings.sandbox.cpus,
                app.settings.sandbox.command_timeout_secs
            ))
            .small()
            .color(theme::faint()),
        );
    } else {
        ui.label(
            RichText::new(format!(
                "{} s per command. Network, memory and CPU limits do not apply here.",
                app.settings.sandbox.command_timeout_secs
            ))
            .small()
            .color(theme::faint()),
        );
    }

    if !app.sandbox_log.is_empty() {
        ui.add_space(6.0);
        egui::CollapsingHeader::new(
            RichText::new("Image build log")
                .small()
                .color(theme::muted()),
        )
        .default_open(app.building_image)
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("image_log")
                .max_height(140.0)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    for line in &app.sandbox_log {
                        ui.add(
                            egui::Label::new(
                                RichText::new(line)
                                    .monospace()
                                    .size(11.5)
                                    .color(theme::muted()),
                            )
                            .extend(),
                        );
                    }
                });
        });
    }

    ui.add_space(8.0);
    ui.separator();
    heading_label(ui, "Run a command");
    ui.label(
        RichText::new(
            "Runs in the same sandbox the agent uses, in the agent's copy of your project.",
        )
        .small()
        .color(theme::faint()),
    );
    terminal(app, ui);
}

fn terminal(app: &mut GagentApp, ui: &mut egui::Ui) {
    let ready = app
        .active()
        .map(|a| matches!(a.engine, EngineState::Ready(_)))
        .unwrap_or(false);
    let mut run: Option<String> = None;
    let mut clear = false;
    if let Some(a) = app.active_mut() {
        ui.horizontal(|ui| {
            let width = ui.available_width() - 70.0;
            let edit = egui::TextEdit::singleline(&mut a.terminal.input)
                .hint_text("e.g. python3 main.py")
                .font(egui::TextStyle::Monospace)
                .desired_width(width);
            let response = ui.add_enabled(ready && !a.terminal.busy, edit);
            let enter = response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            let click = ui
                .add_enabled(ready && !a.terminal.busy, egui::Button::new("Run"))
                .clicked();
            if (enter || click) && !a.terminal.input.trim().is_empty() {
                run = Some(a.terminal.input.clone());
            }
        });
        if a.terminal.busy {
            ui.horizontal(|ui| {
                theme::spinner(ui, 13.0);
                ui.label(RichText::new("Running...").small().color(theme::muted()));
            });
        }
        if !a.terminal.entries.is_empty() && plain_button(ui, "Clear output").clicked() {
            clear = true;
        }
        ui.add_space(4.0);
        let scroll = std::mem::take(&mut a.terminal.scroll);
        egui::ScrollArea::vertical()
            .id_salt("terminal_out")
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for entry in &a.terminal.entries {
                    ui.add(
                        egui::Label::new(
                            RichText::new(format!("$ {}", entry.command))
                                .monospace()
                                .size(12.5)
                                .color(theme::accent()),
                        )
                        .extend(),
                    );
                    if !entry.output.is_empty() {
                        ui.add(
                            egui::Label::new(
                                RichText::new(truncate_end(&entry.output, 20_000))
                                    .monospace()
                                    .size(12.0)
                                    .color(theme::text()),
                            )
                            .extend(),
                        );
                    }
                    ui.add(egui::Label::new(
                        RichText::new(&entry.status).small().color(if entry.ok {
                            theme::ok()
                        } else {
                            theme::err()
                        }),
                    ));
                    ui.add_space(6.0);
                }
                if scroll {
                    ui.scroll_to_cursor(Some(Align::BOTTOM));
                }
            });
        if clear {
            a.terminal.entries.clear();
        }
    }
    if let Some(cmd) = run {
        app.run_in_sandbox(cmd);
    }
}

// ---------------------------------------------------------------------------
// Settings window
// ---------------------------------------------------------------------------

pub fn settings_window(app: &mut GagentApp, ctx: &egui::Context) {
    let Some(mut draft) = app.draft.take() else {
        return;
    };
    let mut open = true;
    let mut save = false;
    let mut cancel = false;
    let mut test_key = false;
    let mut check = false;

    egui::Window::new("Settings")
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_width(580.0)
        .default_height(620.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            let mut area = egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .max_height(ui.available_height() - 48.0);
            if let Some(y) = app.settings_scroll {
                area = area.vertical_scroll_offset(y);
            }
            area.show(ui, |ui| {
                settings_form(app, ui, &mut draft, &mut test_key, &mut check);
            });
            ui.separator();
            ui.horizontal(|ui| {
                if primary_button(ui, "Save").clicked() {
                    save = true;
                }
                if plain_button(ui, "Cancel").clicked() {
                    cancel = true;
                }
            });
        });

    if check {
        app.start_models_check(draft.clone());
    }
    if test_key {
        app.models_loading = true;
        app.key_status = None;
        super::worker::load_models(app.notify.clone(), draft.clone());
    }
    if save {
        app.commit_settings(draft);
        app.show_banner("Settings saved.", false);
    } else if cancel || !open {
        // Discard the draft. The model list on screen may belong to another provider.
        if draft.provider != app.settings.provider {
            app.models = app.settings.preset_models();
            app.key_status = None;
            if app.settings.is_configured() {
                app.load_models();
            }
        }
    } else {
        app.draft = Some(draft);
    }
}

fn section(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(8.0);
    heading_label(ui, title);
    ui.add_space(2.0);
    Frame::new()
        .fill(theme::card_bg())
        .stroke(Stroke::new(1.0, theme::border()))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        });
}

fn field(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(
            Vec2::new(150.0, 22.0),
            Layout::left_to_right(Align::Center),
            |ui| {
                ui.label(RichText::new(label).color(theme::muted()));
            },
        );
        add(ui);
    });
}

/// The dot and the words that say how a model is doing.
fn state_look(state: crate::modelstats::State) -> (egui::Color32, String) {
    use crate::modelstats::State;
    match state {
        State::Unknown => (theme::faint(), "not tested yet".to_string()),
        State::Works(s) => (theme::ok(), format!("works, answers in {s:.1}s")),
        State::Slow(s) => (theme::warn(), format!("works, but slow (~{s:.0}s)")),
        State::Quota(left) => (
            theme::warn(),
            if left >= 3600 {
                format!("out of quota for ~{}h", left / 3600)
            } else {
                format!("out of quota for ~{}min", (left / 60).max(1))
            },
        ),
        State::Blocked => (
            theme::err(),
            "blocked in your account: allow it on the service's site".to_string(),
        ),
        State::Missing => (theme::err(), "this service does not have it".to_string()),
    }
}

/// One service: its key (and address). The keys of all services sit together, so that nobody has
/// to switch between them.
fn service_card(
    app: &mut GagentApp,
    ui: &mut egui::Ui,
    d: &mut crate::config::Settings,
    provider: crate::config::Provider,
) {
    use crate::config::Provider;
    let ready = d.provider_ready(provider);
    Frame::new()
        .fill(theme::bg())
        .stroke(Stroke::new(1.0, theme::border()))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                status_dot(
                    ui,
                    if ready { theme::ok() } else { theme::faint() },
                    "",
                );
                ui.label(
                    RichText::new(provider.label())
                        .strong()
                        .color(theme::text_strong()),
                );
                ui.label(
                    RichText::new(if ready { "key saved" } else { "no key" })
                        .small()
                        .color(theme::faint()),
                );
            });
            ui.label(
                RichText::new(provider.key_help())
                    .small()
                    .color(theme::faint()),
            );
            let (key, base_url, model): (&mut String, Option<&mut String>, Option<&mut String>) =
                match provider {
                    Provider::Gemini => (&mut d.api_key, None, None),
                    Provider::Nvidia => (&mut d.nvidia.api_key, Some(&mut d.nvidia.base_url), None),
                    Provider::Groq => (&mut d.groq.api_key, Some(&mut d.groq.base_url), None),
                    Provider::Openai => (
                        &mut d.openai.api_key,
                        Some(&mut d.openai.base_url),
                        Some(&mut d.openai.model),
                    ),
                };
            field(ui, "API key", |ui| {
                ui.add(
                    egui::TextEdit::singleline(key)
                        .password(!app.show_key)
                        .hint_text(match provider {
                            Provider::Gemini => "AIza...",
                            Provider::Nvidia => "nvapi-...",
                            Provider::Groq => "gsk_...",
                            Provider::Openai => "(optional for a server on this computer)",
                        })
                        .desired_width(260.0),
                );
                ui.checkbox(&mut app.show_key, "Show");
            });
            if let Some(url) = base_url {
                field(ui, "Server address", |ui| {
                    ui.add(
                        egui::TextEdit::singleline(url)
                            .hint_text(match provider {
                                Provider::Nvidia => crate::openai::NVIDIA_BASE_URL,
                                Provider::Groq => crate::openai::GROQ_BASE_URL,
                                _ => "http://localhost:11434/v1",
                            })
                            .desired_width(300.0),
                    );
                });
            }
            if let Some(model) = model {
                field(ui, "Model name", |ui| {
                    ui.add(
                        egui::TextEdit::singleline(model)
                            .hint_text("e.g. deepseek-chat")
                            .desired_width(300.0),
                    );
                });
                ui.label(
                    RichText::new("Any service that speaks the OpenAI API works here: OpenRouter, DeepSeek, Mistral, Together, a server on your own computer (Ollama, LM Studio)...")
                        .small()
                        .color(theme::faint()),
                );
            }
        });
    ui.add_space(4.0);
}

/// The keys of every service, and the button that finds out which models work.
fn ai_services_section(
    app: &mut GagentApp,
    ui: &mut egui::Ui,
    d: &mut crate::config::Settings,
    check: &mut bool,
) {
    section(ui, "AI services", |ui| {
        ui.label(
            RichText::new("Paste a key for every service you have. Gagent then picks the best model that works, for every message and for every member of a team.")
                .small()
                .color(theme::faint()),
        );
        ui.add_space(4.0);
        for provider in crate::config::Provider::ALL {
            service_card(app, ui, d, provider);
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!app.checking_models, egui::Button::new("Check keys and models"))
                .on_hover_text("Asks the best models a tiny question to see which ones answer now, which are blocked and which are out of quota.")
                .clicked()
            {
                *check = true;
            }
            if app.checking_models {
                theme::spinner(ui, 13.0);
            }
        });
        if let Some((ok, text)) = &app.key_status {
            ui.add(
                egui::Label::new(
                    RichText::new(truncate_end(text, 240))
                        .small()
                        .color(if *ok { theme::ok() } else { theme::err() }),
                )
                .wrap(),
            );
        }
    });
}

/// Every model Gagent may use, with what is known about it, and a tick to leave it out.
fn models_section(ui: &mut egui::Ui, d: &mut crate::config::Settings) {
    section(ui, "Models", |ui| {
        ui.label(
            RichText::new("Gagent picks the best of these for every message. Untick a model to leave it out. Green: works. Orange: slow or out of quota. Red: blocked or missing. Grey: not tested yet.")
                .small()
                .color(theme::faint()),
        );
        let mut any = false;
        for provider in crate::config::Provider::ALL {
            if !d.provider_ready(provider) {
                continue;
            }
            any = true;
            let host = crate::router::host_for(d, provider);
            ui.add_space(6.0);
            ui.label(
                RichText::new(provider.label())
                    .small()
                    .strong()
                    .color(theme::muted()),
            );
            let mut rows: Vec<(String, crate::modelstats::State)> =
                crate::router::candidate_models(d, provider)
                    .into_iter()
                    .map(|m| {
                        let state = crate::modelstats::state(&host, &m);
                        (m, state)
                    })
                    // A model the service does not have is not worth showing.
                    .filter(|(_, state)| *state != crate::modelstats::State::Missing)
                    .collect();
            // The ones that work come first, the fastest at the top.
            let order = |s: crate::modelstats::State| -> f32 {
                use crate::modelstats::State;
                match s {
                    State::Works(t) => t,
                    State::Unknown => 100.0,
                    State::Slow(t) => 200.0 + t,
                    State::Quota(left) => 1000.0 + left as f32 / 3600.0,
                    State::Blocked => 5000.0,
                    State::Missing => 6000.0,
                }
            };
            rows.sort_by(|a, b| order(a.1).total_cmp(&order(b.1)).then(a.0.cmp(&b.0)));
            for (model, state) in rows {
                let (color, text) = state_look(state);
                ui.horizontal(|ui| {
                    let mut on = d.model_enabled(provider, &model);
                    if ui.checkbox(&mut on, "").changed() {
                        d.set_model_enabled(provider, &model, on);
                    }
                    ui.label(
                        RichText::new(truncate_end(&crate::models::label(&model), 44))
                            .color(theme::text()),
                    );
                    status_dot(ui, color, &text);
                });
            }
        }
        if !any {
            ui.label(
                RichText::new("Add a key above to see the models.")
                    .color(theme::faint()),
            );
        }
    });
}

fn settings_form(
    app: &mut GagentApp,
    ui: &mut egui::Ui,
    d: &mut crate::config::Settings,
    test_key: &mut bool,
    check: &mut bool,
) {
    section(ui, "Appearance", |ui| {
        field(ui, "Theme", |ui| {
            egui::ComboBox::from_id_salt("settings_appearance")
                .selected_text(d.appearance.label())
                .width(300.0)
                .show_ui(ui, |ui| {
                    for a in crate::config::Appearance::ALL {
                        ui.selectable_value(&mut d.appearance, a, a.label());
                    }
                });
        });
    });

    ai_services_section(app, ui, d, check);
    models_section(ui, d);

    section(ui, "How the model is chosen", |ui| {
        ui.checkbox(
            &mut d.auto_pick,
            "Choose the best working model for every message by itself (recommended)",
        );
        if !d.auto_pick {
        let provider = d.provider;
        field(ui, "Service", |ui| {
            let mut chosen = provider;
            egui::ComboBox::from_id_salt("settings_provider")
                .selected_text(provider.label())
                .width(260.0)
                .show_ui(ui, |ui| {
                    for p in crate::config::Provider::ALL {
                        ui.selectable_value(&mut chosen, p, p.label());
                    }
                });
            if chosen != provider {
                d.provider = chosen;
                app.models = d.preset_models();
                app.key_status = None;
            }
        });
        let provider = d.provider;
        let (key, model, base_url): (&mut String, &mut String, Option<&mut String>) = match provider
        {
            crate::config::Provider::Gemini => (&mut d.api_key, &mut d.model, None),
            crate::config::Provider::Nvidia => (
                &mut d.nvidia.api_key,
                &mut d.nvidia.model,
                Some(&mut d.nvidia.base_url),
            ),
            crate::config::Provider::Groq => (
                &mut d.groq.api_key,
                &mut d.groq.model,
                Some(&mut d.groq.base_url),
            ),
            crate::config::Provider::Openai => (
                &mut d.openai.api_key,
                &mut d.openai.model,
                Some(&mut d.openai.base_url),
            ),
        };
        if let Some(url) = base_url {
            field(ui, "Server address", |ui| {
                ui.add(
                    egui::TextEdit::singleline(url)
                        .hint_text(if provider == crate::config::Provider::Nvidia {
                            crate::openai::NVIDIA_BASE_URL
                        } else {
                            "http://localhost:11434/v1"
                        })
                        .desired_width(300.0),
                );
            });
        }
        if provider == crate::config::Provider::Openai {
            ui.label(
                RichText::new("Any service that speaks the OpenAI API works here: OpenRouter, DeepSeek, Mistral, Together, a server on your own computer (Ollama, LM Studio)... Enter its address, your key and a model name.")
                    .small()
                    .color(theme::faint()),
            );
        }
        field(ui, "API key", |ui| {
            ui.add(
                egui::TextEdit::singleline(key)
                    .password(!app.show_key)
                    .hint_text(match provider {
                        crate::config::Provider::Gemini => "AIza...",
                        crate::config::Provider::Nvidia => "nvapi-...",
                        crate::config::Provider::Groq => "gsk_...",
                        crate::config::Provider::Openai => "(optional for local servers)",
                    })
                    .desired_width(260.0),
            );
            ui.checkbox(&mut app.show_key, "Show");
        });
        ui.horizontal(|ui| {
            ui.add_space(158.0);
            if ui
                .add_enabled(
                    !app.models_loading,
                    egui::Button::new("Test key and load models"),
                )
                .clicked()
            {
                *test_key = true;
            }
            if app.models_loading {
                theme::spinner(ui, 13.0);
            }
        });
        if let Some((ok, text)) = &app.key_status {
            ui.horizontal(|ui| {
                ui.add_space(158.0);
                ui.add(
                    egui::Label::new(
                        RichText::new(truncate_end(text, 200))
                            .small()
                            .color(if *ok { theme::ok() } else { theme::err() }),
                    )
                    .wrap(),
                );
            });
        }
        ui.horizontal(|ui| {
            ui.add_space(158.0);
            let help = match provider {
                crate::config::Provider::Gemini => "Free key: aistudio.google.com/apikey. Leave empty to use the GEMINI_API_KEY environment variable.",
                crate::config::Provider::Nvidia => "Create a key at build.nvidia.com (it starts with nvapi-). Leave empty to use the NVIDIA_API_KEY environment variable.",
                crate::config::Provider::Groq => "Create a key at console.groq.com/keys (it starts with gsk_). Leave empty to use the GROQ_API_KEY environment variable.",
                crate::config::Provider::Openai => "OpenAI, OpenRouter, Ollama (http://localhost:11434/v1), LM Studio, vLLM and others. The address must end in /v1.",
            };
            ui.add(egui::Label::new(RichText::new(help).small().color(theme::faint())).wrap());
        });
        if !app.models.is_empty() {
            field(ui, "Model", |ui| {
                egui::ComboBox::from_id_salt("settings_model")
                    .selected_text(model.as_str())
                    .width(300.0)
                    .show_ui(ui, |ui| {
                        for m in crate::models::picker_list(&app.models, model.as_str(), false) {
                            ui.selectable_value(
                                model,
                                m.clone(),
                                format!("{}{}", crate::models::label(&m), crate::modelstats::hint(&m)),
                            );
                        }
                    });
            });
        }
        field(ui, "Model ID", |ui| {
            ui.add(egui::TextEdit::singleline(model).desired_width(300.0));
        });
        if provider != crate::config::Provider::Gemini {
            field(ui, "Max answer tokens", |ui| {
                ui.add(
                    egui::DragValue::new(&mut d.max_output_tokens)
                        .range(256..=131072)
                        .speed(256),
                );
            });
        }
        if provider != crate::config::Provider::Gemini {
            field(ui, "Tool calling", |ui| {
                egui::ComboBox::from_id_salt("settings_tool_mode")
                    .selected_text(d.tool_mode.label())
                    .width(300.0)
                    .show_ui(ui, |ui| {
                        for m in crate::config::ToolMode::ALL {
                            ui.selectable_value(&mut d.tool_mode, m, m.label());
                        }
                    });
            });
        }
        }
        field(ui, "Token budget per task", |ui| {
            ui.add(
                egui::DragValue::new(&mut d.max_task_tokens)
                    .range(0..=100_000_000)
                    .speed(50_000)
                    .custom_formatter(|n, _| {
                        if n == 0.0 {
                            "no limit".to_string()
                        } else {
                            format!("{:.0} k", n / 1000.0)
                        }
                    }),
            )
            .on_hover_text(
                "The task stops when all its requests together reach this many tokens. 0 means no limit.",
            );
        });
        ui.checkbox(
            &mut d.auto_failover,
            "When a model runs out of quota or fails, go on with another one",
        );
        field(ui, "Big jobs", |ui| {
            egui::ComboBox::from_id_salt("settings_team_mode")
                .selected_text(d.team_mode.label())
                .width(300.0)
                .show_ui(ui, |ui| {
                    for m in crate::config::TeamMode::ALL {
                        ui.selectable_value(&mut d.team_mode, m, m.label());
                    }
                });
        });
        ui.checkbox(
            &mut d.team_auto_start,
            "Start the plan of a team without asking me (a team uses more tokens)",
        );
        field(ui, "Requests per minute", |ui| {
            ui.add(
                egui::DragValue::new(&mut d.requests_per_minute)
                    .range(0..=600)
                    .speed(1),
            )
            .on_hover_text(
                "0 is automatic: 35 on NVIDIA's free endpoint (it allows about 40 for the whole account, whatever the model), no limit elsewhere.",
            );
        });
        field(ui, "Max steps per task", |ui| {
            ui.add(
                egui::DragValue::new(&mut d.max_steps)
                    .range(5..=200)
                    .speed(1),
            );
        });
    });

    section(ui, "Project files", |ui| {
        field(ui, "Ask before commands", |ui| {
            egui::ComboBox::from_id_salt("settings_permission")
                .selected_text(d.permission.label())
                .width(300.0)
                .show_ui(ui, |ui| {
                    for m in crate::config::PermissionMode::ALL {
                        ui.selectable_value(&mut d.permission, m, m.label());
                    }
                });
        });
        ui.label(
            RichText::new("AI models can be wrong and the commands run on your own computer. Look at the Changes before you apply them. See SECURITY.md.")
                .small()
                .color(theme::faint()),
        );
        field(ui, "Check files after writing", |ui| {
            egui::ComboBox::from_id_salt("settings_auto_check")
                .selected_text(d.auto_check.label())
                .width(300.0)
                .show_ui(ui, |ui| {
                    for c in crate::config::CheckLevel::ALL {
                        ui.selectable_value(&mut d.auto_check, c, c.label());
                    }
                });
        });
        ui.checkbox(
            &mut d.auto_apply,
            "Apply the agent's changes to my project automatically when a task finishes",
        );
        ui.checkbox(
            &mut d.share_secret_files,
            "Let the agent see secret files (.env, private keys)",
        );
        ui.checkbox(
            &mut d.web_tools,
            "Let the agent search the web and read pages (it asks you first; the page address or search words go to the internet)",
        );
        ui.checkbox(
            &mut d.git_checkpoint,
            "Save a git checkpoint before the agent's changes reach a git project (your branch is not touched)",
        );
        ui.checkbox(
            &mut d.speak_answers,
            "Read the answers aloud (a Gemini voice; the text goes to Google)",
        );
        field(ui, "Language you speak", |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut d.voice_language)
                    .hint_text("empty = find out (az, tr, en, ru...)")
                    .desired_width(260.0),
            );
        });
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("Trusted folders: {}", d.trusted_folders.len()))
                    .color(theme::muted()),
            );
            if !d.trusted_folders.is_empty() && ui.small_button("Forget all").clicked() {
                d.trusted_folders.clear();
            }
        });
        ui.label(
            RichText::new(
                "Everything the agent reads is sent to the selected AI service. Secret files are hidden by default.",
            )
            .small()
            .color(theme::faint()),
        );
    });

    section(ui, "Sandbox", |ui| {
        field(ui, "Run commands in", |ui| {
            egui::ComboBox::from_id_salt("settings_backend")
                .selected_text(d.sandbox.backend.label())
                .width(300.0)
                .show_ui(ui, |ui| {
                    for b in crate::config::SandboxBackend::ALL {
                        ui.selectable_value(&mut d.sandbox.backend, b, b.label());
                    }
                });
        });
        field(ui, "WSL distribution", |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut d.sandbox.wsl_distro)
                    .hint_text("empty = first installed (e.g. Ubuntu)")
                    .desired_width(300.0),
            );
        });
        ui.label(
            RichText::new("Automatic runs commands with bash on this computer when there is one (Git Bash on Windows): the fastest and lightest way, not isolated, dangerous commands are refused. Otherwise Docker, then WSL.")
                .small()
                .color(theme::faint()),
        );
        ui.add_space(4.0);
        field(ui, "Docker image", |ui| {
            ui.add(egui::TextEdit::singleline(&mut d.sandbox.image).desired_width(260.0));
        });
        ui.checkbox(
            &mut d.sandbox.network,
            "Allow network access inside the sandbox",
        );
        field(ui, "Memory (MB)", |ui| {
            ui.add(
                egui::DragValue::new(&mut d.sandbox.memory_mb)
                    .range(256..=65536)
                    .speed(64),
            );
        });
        field(ui, "CPU cores", |ui| {
            ui.add(
                egui::DragValue::new(&mut d.sandbox.cpus)
                    .range(0.25..=64.0)
                    .speed(0.25),
            );
        });
        field(ui, "Command timeout (s)", |ui| {
            ui.add(
                egui::DragValue::new(&mut d.sandbox.command_timeout_secs)
                    .range(5..=3600)
                    .speed(5),
            );
        });
    });

    section(ui, "Data", |ui| {
        ui.label(
            RichText::new(app.dirs.root().display().to_string())
                .monospace()
                .small()
                .color(theme::muted()),
        );
        ui.label(
            RichText::new(
                "Settings, sessions, private project copies and backups are stored here.",
            )
            .small()
            .color(theme::faint()),
        );
        if plain_button(ui, "Open data folder").clicked() {
            let _ = opener::open(app.dirs.root());
        }
    });
}
