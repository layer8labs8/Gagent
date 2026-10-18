//! The right-hand panel (activity, changes, sandbox, drive) and the settings window.

use eframe::egui::{
    self, Align, Color32, CornerRadius, Frame, Key, Layout, Margin, Panel, RichText, Stroke, Vec2,
};

use super::state::*;
use super::theme;
use super::views::{heading_label, plain_button, primary_button, status_dot};
use super::GagentApp;
use crate::drive::RemoteFile;
use crate::shadow::ChangeKind;
use crate::util::{format_clock, format_local, human_bytes, truncate_end};

pub fn side_panel(app: &mut GagentApp, ui: &mut egui::Ui) {
    Panel::right("side_panel")
        .default_size(430.0)
        .size_range(340.0..=780.0)
        .resizable(true)
        .frame(
            Frame::new()
                .fill(theme::PANEL_BG)
                .stroke(Stroke::new(1.0, theme::BORDER))
                .inner_margin(Margin::same(12)),
        )
        .show(ui, |ui| {
            tab_bar(app, ui);
            ui.add_space(8.0);
            match app.tab {
                Tab::Activity => activity_tab(app, ui),
                Tab::Changes => changes_tab(app, ui),
                Tab::Sandbox => sandbox_tab(app, ui),
                Tab::Drive => drive_tab(app, ui),
            }
        });
}

fn tab_bar(app: &mut GagentApp, ui: &mut egui::Ui) {
    let pending = app.active().map(|a| a.changes.len()).unwrap_or(0);
    ui.horizontal(|ui| {
        for (tab, label) in [
            (Tab::Activity, "Activity".to_string()),
            (
                Tab::Changes,
                if pending > 0 {
                    format!("Changes ({pending})")
                } else {
                    "Changes".to_string()
                },
            ),
            (Tab::Sandbox, "Sandbox".to_string()),
            (Tab::Drive, "Drive".to_string()),
        ] {
            let selected = app.tab == tab;
            let text = RichText::new(label).strong().color(if selected {
                theme::ACCENT
            } else {
                theme::MUTED
            });
            let response = ui.add(egui::Button::new(text).frame(false));
            if selected {
                let r = response.rect;
                ui.painter().line_segment(
                    [
                        egui::pos2(r.left() + 4.0, r.bottom() + 1.0),
                        egui::pos2(r.right() - 4.0, r.bottom() + 1.0),
                    ],
                    Stroke::new(2.0, theme::ACCENT),
                );
            }
            if response.clicked() {
                app.tab = tab;
                if tab == Tab::Changes {
                    app.refresh_changes();
                }
                if tab == Tab::Drive && app.drive.client.is_some() && app.drive.entries.is_empty() {
                    app.drive_refresh();
                }
                if tab == Tab::Sandbox && app.docker.is_none() {
                    app.check_docker();
                }
            }
        }
    });
    ui.separator();
}

// ---------------------------------------------------------------------------
// Activity
// ---------------------------------------------------------------------------

fn activity_tab(app: &mut GagentApp, ui: &mut egui::Ui) {
    let Some(a) = app.active() else {
        return;
    };
    heading_label(ui, "LIVE ACTIVITY");
    ui.label(
        RichText::new("Every file operation and sandbox command, as it happens.")
            .small()
            .color(theme::FAINT),
    );
    if let Some(run) = &a.run {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            theme::spinner(ui, 13.0);
            ui.label(
                RichText::new(truncate_end(&run.status, 60))
                    .small()
                    .color(theme::MUTED),
            );
        });
    }
    if let Some(u) = a.last_usage.filter(|u| u.total_tokens > 0) {
        ui.label(
            RichText::new(format!(
                "Last task: {} tokens ({} in, {} out)",
                u.total_tokens, u.prompt_tokens, u.output_tokens
            ))
            .small()
            .color(theme::FAINT),
        );
    }
    ui.add_space(6.0);
    if a.log.is_empty() {
        ui.label(
            RichText::new("Nothing yet. Activity appears here while the agent works.")
                .color(theme::FAINT),
        );
        return;
    }
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .stick_to_bottom(true)
        .show(ui, |ui| {
            for (i, entry) in a.log.iter().enumerate() {
                match entry.kind {
                    LogKind::Output => {
                        Frame::new()
                            .fill(theme::CODE_BG)
                            .corner_radius(CornerRadius::same(6))
                            .inner_margin(Margin::same(6))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                egui::ScrollArea::both()
                                    .id_salt(("log_out", i))
                                    .max_height(180.0)
                                    .show(ui, |ui| {
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(&entry.text)
                                                    .monospace()
                                                    .size(12.0)
                                                    .color(theme::MUTED),
                                            )
                                            .extend(),
                                        );
                                    });
                            });
                    }
                    kind => {
                        let color = match kind {
                            LogKind::Good => theme::OK,
                            LogKind::Bad => theme::ERR,
                            LogKind::Tool => theme::TEXT,
                            _ => theme::MUTED,
                        };
                        ui.horizontal_top(|ui| {
                            ui.label(
                                RichText::new(format_clock(entry.time))
                                    .monospace()
                                    .size(11.5)
                                    .color(theme::FAINT),
                            );
                            ui.add(
                                egui::Label::new(
                                    RichText::new(&entry.text).size(13.0).color(color),
                                )
                                .wrap(),
                            );
                        });
                    }
                }
            }
        });
}

// ---------------------------------------------------------------------------
// Changes
// ---------------------------------------------------------------------------

fn changes_tab(app: &mut GagentApp, ui: &mut egui::Ui) {
    let Some(a) = app.active() else {
        return;
    };
    if !matches!(a.engine, EngineState::Ready(_)) {
        ui.label(RichText::new("The workspace is not ready yet.").color(theme::FAINT));
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

    let mut toggle: Option<String> = None;
    let mut view: Option<String> = None;
    let mut apply: Option<Vec<String>> = None;
    let mut discard_all = false;
    let mut set_confirm = false;
    let mut discard_selected = false;
    let mut undo: Option<std::path::PathBuf> = None;
    let mut refresh = false;
    let mut auto = app.settings.auto_apply;

    heading_label(ui, "PENDING CHANGES");
    ui.label(
        RichText::new(
            "The agent works on a private copy. Nothing reaches your project until you apply it.",
        )
        .small()
        .color(theme::FAINT),
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
                        .color(Color32::from_rgb(0x03, 0x1B, 0x24)),
                )
                .fill(theme::ACCENT),
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
                egui::Button::new(RichText::new(label).color(theme::ERR)),
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
                egui::Button::new(RichText::new("Discard selected").color(theme::ERR)),
            )
            .clicked()
        {
            discard_selected = true;
        }
    });
    ui.add_space(6.0);
    if let Some(e) = &error {
        ui.label(RichText::new(e).color(theme::ERR));
    }

    let list_height = if diff_for.is_some() { 170.0 } else { 300.0 };
    if changes.is_empty() {
        ui.add_space(10.0);
        ui.label(
            RichText::new("No pending changes. The agent's copy matches your project.")
                .color(theme::FAINT),
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
                            ChangeKind::Added => ("A", theme::OK),
                            ChangeKind::Modified => ("M", theme::WARN),
                            ChangeKind::Deleted => ("D", theme::ERR),
                        };
                        ui.label(RichText::new(letter).monospace().strong().color(color));
                        let is_open = diff_for.as_deref() == Some(c.rel.as_str());
                        let text = RichText::new(truncate_end(&c.rel, 48))
                            .monospace()
                            .size(13.0)
                            .color(if is_open { theme::ACCENT } else { theme::TEXT });
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
                                    .color(theme::FAINT),
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
                .color(theme::TEXT_STRONG),
        );
        egui::ScrollArea::both()
            .id_salt("diff_view")
            .auto_shrink([false, false])
            .max_height(ui.available_height() - 90.0)
            .show(ui, |ui| {
                for line in diff_text.lines() {
                    let color = if line.starts_with("+++") || line.starts_with("---") {
                        theme::MUTED
                    } else if line.starts_with('+') {
                        theme::OK
                    } else if line.starts_with('-') {
                        theme::ERR
                    } else if line.starts_with("@@") {
                        theme::ACCENT
                    } else {
                        theme::MUTED
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
        heading_label(ui, "UNDO");
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

// ---------------------------------------------------------------------------
// Sandbox
// ---------------------------------------------------------------------------

fn sandbox_tab(app: &mut GagentApp, ui: &mut egui::Ui) {
    heading_label(ui, "SANDBOX");
    ui.label(
        RichText::new("Code runs in an isolated Ubuntu container: no root, no host files except the agent's private copy.")
            .small()
            .color(theme::FAINT),
    );
    ui.add_space(6.0);

    let status = app.docker.clone();
    match &status {
        None => status_dot(ui, theme::FAINT, "Checking Docker..."),
        Some(s) if s.is_ready() => status_dot(ui, theme::OK, &s.describe()),
        Some(s) => {
            ui.label(RichText::new(s.describe()).color(theme::WARN));
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
        if ui
            .add_enabled(!app.building_image, egui::Button::new(build_label))
            .clicked()
        {
            app.build_image();
        }
        if app.building_image {
            theme::spinner(ui, 13.0);
        }
        if plain_button(ui, "Reset container")
            .on_hover_text("Stops and removes the container; it starts again on the next command")
            .clicked()
        {
            app.reset_sandbox();
        }
    });
    ui.add_space(4.0);
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
        .color(theme::FAINT),
    );

    if !app.sandbox_log.is_empty() {
        ui.add_space(6.0);
        egui::CollapsingHeader::new(RichText::new("Image build log").small().color(theme::MUTED))
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
                                        .color(theme::MUTED),
                                )
                                .extend(),
                            );
                        }
                    });
            });
    }

    ui.add_space(8.0);
    ui.separator();
    heading_label(ui, "RUN A COMMAND");
    ui.label(
        RichText::new(
            "Runs in the same sandbox the agent uses, in the agent's copy of your project.",
        )
        .small()
        .color(theme::FAINT),
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
                ui.label(RichText::new("Running...").small().color(theme::MUTED));
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
                                .color(theme::ACCENT),
                        )
                        .extend(),
                    );
                    if !entry.output.is_empty() {
                        ui.add(
                            egui::Label::new(
                                RichText::new(truncate_end(&entry.output, 20_000))
                                    .monospace()
                                    .size(12.0)
                                    .color(theme::TEXT),
                            )
                            .extend(),
                        );
                    }
                    ui.add(egui::Label::new(
                        RichText::new(&entry.status).small().color(if entry.ok {
                            theme::OK
                        } else {
                            theme::ERR
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
// Drive
// ---------------------------------------------------------------------------

fn drive_tab(app: &mut GagentApp, ui: &mut egui::Ui) {
    heading_label(ui, "GOOGLE DRIVE");
    ui.label(
        RichText::new("Keep project files and chat sessions in your own Drive. Gagent only sees the folder it creates.")
            .small()
            .color(theme::FAINT),
    );
    ui.add_space(8.0);

    if app.drive.client.is_none() {
        drive_connect_view(app, ui);
        return;
    }

    let account = app
        .drive
        .account
        .clone()
        .unwrap_or_else(|| "connected".into());
    ui.horizontal(|ui| {
        status_dot(ui, theme::OK, &format!("Connected: {account}"));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if plain_button(ui, "Disconnect").clicked() {
                app.drive_disconnect();
            }
        });
    });
    ui.label(
        RichText::new(format!("Drive folder: {}", app.settings.drive.folder()))
            .small()
            .color(theme::FAINT),
    );
    ui.add_space(6.0);

    let busy = app.drive.job.is_some();
    let has_project = app
        .active()
        .map(|a| matches!(a.engine, EngineState::Ready(_)))
        .unwrap_or(false);
    let mut job: Option<DriveJobKind> = None;
    ui.horizontal_wrapped(|ui| {
        if ui
            .add_enabled(!busy && has_project, egui::Button::new("Upload project"))
            .on_hover_text("Copies your project folder to Drive (only changed files)")
            .clicked()
        {
            job = Some(DriveJobKind::UploadProject);
        }
        if ui
            .add_enabled(!busy && has_project, egui::Button::new("Download project"))
            .on_hover_text("Brings the Drive copy into the agent's workspace so you can review it")
            .clicked()
        {
            job = Some(DriveJobKind::DownloadProject);
        }
        if ui
            .add_enabled(!busy, egui::Button::new("Back up sessions"))
            .on_hover_text("Saves your chat sessions to Drive")
            .clicked()
        {
            job = Some(DriveJobKind::BackupSessions);
        }
        if ui
            .add_enabled(!busy, egui::Button::new("Restore sessions"))
            .on_hover_text("Brings back sessions that are not on this computer")
            .clicked()
        {
            job = Some(DriveJobKind::RestoreSessions);
        }
    });
    if let Some(label) = app.drive.job.clone() {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            theme::spinner(ui, 13.0);
            ui.label(
                RichText::new(format!(
                    "{label}: {}",
                    truncate_end(&app.drive.job_status, 60)
                ))
                .small()
                .color(theme::MUTED),
            );
            if plain_button(ui, "Cancel").clicked() {
                app.drive
                    .job_cancel
                    .store(true, std::sync::atomic::Ordering::Relaxed);
            }
        });
    }
    if let Some((ok, text)) = &app.drive.last_result {
        ui.add_space(4.0);
        ui.label(
            RichText::new(truncate_end(text, 300))
                .small()
                .color(if *ok { theme::OK } else { theme::ERR }),
        );
    }

    ui.add_space(8.0);
    ui.separator();
    heading_label(ui, "BROWSE");
    let cwd = app.drive.cwd.clone();
    let mut go: Option<String> = None;
    ui.horizontal(|ui| {
        if ui
            .add_enabled(!cwd.is_empty(), egui::Button::new("Up"))
            .clicked()
        {
            go = Some(
                cwd.rsplit_once('/')
                    .map(|(p, _)| p.to_string())
                    .unwrap_or_default(),
            );
        }
        if plain_button(ui, "Refresh").clicked() {
            go = Some(cwd.clone());
        }
        ui.label(
            RichText::new(format!(
                "{} / {}",
                app.settings.drive.folder(),
                cwd.replace('/', " / ")
            ))
            .small()
            .color(theme::MUTED),
        );
        if app.drive.loading {
            theme::spinner(ui, 12.0);
        }
    });
    if let Some(e) = &app.drive.error {
        ui.label(
            RichText::new(truncate_end(e, 300))
                .small()
                .color(theme::ERR),
        );
    }
    let entries: Vec<RemoteFile> = app.drive.entries.clone();
    if entries.is_empty() && !app.drive.loading && app.drive.error.is_none() {
        ui.label(
            RichText::new("This folder is empty.")
                .small()
                .color(theme::FAINT),
        );
    }
    egui::ScrollArea::vertical()
        .id_salt("drive_list")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for e in &entries {
                ui.horizontal(|ui| {
                    if e.is_dir {
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new(format!("{}/", e.name)).color(theme::ACCENT),
                                )
                                .frame(false),
                            )
                            .clicked()
                        {
                            go = Some(e.path.clone());
                        }
                    } else {
                        ui.label(RichText::new(&e.name).color(theme::TEXT));
                        ui.label(
                            RichText::new(human_bytes(e.size))
                                .small()
                                .color(theme::FAINT),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui
                                .add_enabled(
                                    !busy && has_project,
                                    egui::Button::new(RichText::new("Download").small()),
                                )
                                .on_hover_text("Saves it into the workspace under drive/")
                                .clicked()
                            {
                                job = Some(DriveJobKind::DownloadFile(e.path.clone()));
                            }
                        });
                    }
                });
            }
        });
    if let Some(dir) = go {
        app.drive_list(&dir);
    }
    if let Some(kind) = job {
        app.drive_job(kind);
    }
}

fn drive_connect_view(app: &mut GagentApp, ui: &mut egui::Ui) {
    if let Some(c) = &app.drive.connecting {
        match &c.code {
            None => {
                ui.horizontal(|ui| {
                    theme::spinner(ui, 14.0);
                    ui.label(RichText::new("Contacting Google...").color(theme::MUTED));
                });
            }
            Some(code) => {
                ui.label(RichText::new("1. Open this page in your browser:").color(theme::TEXT));
                let url = code.verification_url.clone();
                if ui
                    .add(
                        egui::Button::new(RichText::new(&url).color(theme::ACCENT).underline())
                            .frame(false),
                    )
                    .clicked()
                {
                    let _ = opener::open(&url);
                }
                ui.add_space(4.0);
                ui.label(
                    RichText::new("2. Enter this code and approve access:").color(theme::TEXT),
                );
                ui.add_space(2.0);
                Frame::new()
                    .fill(theme::CODE_BG)
                    .stroke(Stroke::new(1.0, theme::ACCENT))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(Margin::symmetric(16, 10))
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new(&code.user_code)
                                .monospace()
                                .size(26.0)
                                .strong()
                                .color(theme::TEXT_STRONG),
                        );
                    });
                ui.horizontal(|ui| {
                    if plain_button(ui, "Copy code").clicked() {
                        ui.ctx().copy_text(code.user_code.clone());
                    }
                    if plain_button(ui, "Open page").clicked() {
                        let _ = opener::open(&url);
                    }
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    theme::spinner(ui, 13.0);
                    ui.label(
                        RichText::new("Waiting for approval...")
                            .small()
                            .color(theme::MUTED),
                    );
                });
            }
        }
        ui.add_space(6.0);
        if plain_button(ui, "Cancel").clicked() {
            c.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        return;
    }

    if !app.settings.drive.has_client() {
        ui.label(
            RichText::new("Drive needs your own Google OAuth client (free). Steps:")
                .color(theme::TEXT),
        );
        for step in [
            "1. In Google Cloud Console, create a project and enable the Google Drive API.",
            "2. Create OAuth credentials of type \"TVs and Limited Input devices\".",
            "3. Paste the client ID and secret in Settings, then connect here.",
        ] {
            ui.label(RichText::new(step).small().color(theme::MUTED));
        }
        ui.add_space(8.0);
        if primary_button(ui, "Open Settings").clicked() {
            app.draft = Some(app.settings.clone());
        }
        return;
    }
    ui.label(RichText::new("Not connected.").color(theme::MUTED));
    ui.add_space(6.0);
    if primary_button(ui, "Connect Google Drive").clicked() {
        app.drive_connect();
    }
    ui.add_space(4.0);
    ui.label(
        RichText::new(
            "You will get a short code to enter on google.com/device. No local web server is used.",
        )
        .small()
        .color(theme::FAINT),
    );
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

    egui::Window::new("Settings")
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_width(580.0)
        .default_height(620.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .max_height(ui.available_height() - 48.0)
                .show(ui, |ui| {
                    settings_form(app, ui, &mut draft, &mut test_key);
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

    if test_key {
        app.models_loading = true;
        app.key_status = None;
        super::worker::load_models(app.notify.clone(), draft.clone());
    }
    if save {
        app.commit_settings(draft);
        app.show_banner("Settings saved.", false);
    } else if cancel || !open {
        // Discard the draft.
    } else {
        app.draft = Some(draft);
    }
}

fn section(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(8.0);
    heading_label(ui, title);
    ui.add_space(2.0);
    Frame::new()
        .fill(theme::CARD_BG)
        .stroke(Stroke::new(1.0, theme::BORDER))
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
                ui.label(RichText::new(label).color(theme::MUTED));
            },
        );
        add(ui);
    });
}

fn settings_form(
    app: &mut GagentApp,
    ui: &mut egui::Ui,
    d: &mut crate::config::Settings,
    test_key: &mut bool,
) {
    section(ui, "GEMINI", |ui| {
        field(ui, "API key", |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut d.api_key)
                    .password(!app.show_key)
                    .hint_text("AIza...")
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
                            .color(if *ok { theme::OK } else { theme::ERR }),
                    )
                    .wrap(),
                );
            });
        }
        ui.horizontal(|ui| {
            ui.add_space(158.0);
            ui.label(RichText::new("Get a free key at aistudio.google.com/apikey. Leave empty to use the GEMINI_API_KEY environment variable.").small().color(theme::FAINT));
        });
        field(ui, "Model", |ui| {
            egui::ComboBox::from_id_salt("settings_model")
                .selected_text(&d.model)
                .width(260.0)
                .show_ui(ui, |ui| {
                    for m in &app.models {
                        ui.selectable_value(&mut d.model, m.clone(), m);
                    }
                });
        });
        field(ui, "Model ID", |ui| {
            ui.add(egui::TextEdit::singleline(&mut d.model).desired_width(260.0));
        });
        field(ui, "Max steps per task", |ui| {
            ui.add(
                egui::DragValue::new(&mut d.max_steps)
                    .range(5..=100)
                    .speed(1),
            );
        });
    });

    section(ui, "PROJECT FILES", |ui| {
        ui.checkbox(
            &mut d.auto_apply,
            "Apply the agent's changes to my project automatically when a task finishes",
        );
        ui.checkbox(
            &mut d.share_secret_files,
            "Let the agent see secret files (.env, private keys)",
        );
        ui.label(
            RichText::new(
                "Everything the agent reads is sent to Gemini. Secret files are hidden by default.",
            )
            .small()
            .color(theme::FAINT),
        );
    });

    section(ui, "SANDBOX (DOCKER)", |ui| {
        field(ui, "Image", |ui| {
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

    section(ui, "GOOGLE DRIVE", |ui| {
        field(ui, "OAuth client ID", |ui| {
            ui.add(egui::TextEdit::singleline(&mut d.drive.client_id).desired_width(300.0));
        });
        field(ui, "OAuth client secret", |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut d.drive.client_secret)
                    .password(true)
                    .desired_width(300.0),
            );
        });
        field(ui, "Drive folder name", |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut d.drive.folder_name)
                    .hint_text("Gagent")
                    .desired_width(300.0),
            );
        });
        ui.label(
            RichText::new("Create the client in Google Cloud Console as type \"TVs and Limited Input devices\" and enable the Google Drive API. Then connect from the Drive tab.")
                .small()
                .color(theme::FAINT),
        );
    });

    section(ui, "DATA", |ui| {
        ui.label(
            RichText::new(app.dirs.root().display().to_string())
                .monospace()
                .small()
                .color(theme::MUTED),
        );
        ui.label(
            RichText::new(
                "Settings, sessions, private project copies and backups are stored here.",
            )
            .small()
            .color(theme::FAINT),
        );
        if plain_button(ui, "Open data folder").clicked() {
            let _ = opener::open(app.dirs.root());
        }
    });
}
