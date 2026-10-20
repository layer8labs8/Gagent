//! Chat sessions saved on disk.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::util::{now_secs, truncate_end, unique_id, write_atomic};

/// Longest tool output kept in a saved session.
const MAX_SAVED_OUTPUT: usize = 20_000;

/// One entry of the visible conversation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum ChatItem {
    User(String),
    Assistant(String),
    Tool {
        id: u32,
        name: String,
        /// What the agent is doing, e.g. "Read src/main.py".
        summary: String,
        /// `None` while the tool is still running.
        ok: Option<bool>,
        /// Short outcome, e.g. "exit 1 in 2.3s".
        result: String,
        /// Full text returned to the model (shown when expanded).
        output: String,
        millis: u64,
    },
    Notice(String),
    Error(String),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SessionMeta {
    pub id: String,
    pub title: String,
    pub project: String,
    pub created_at: i64,
    pub updated_at: i64,
    /// Set for a team project: the goal and the agents.
    #[serde(default)]
    pub team: Option<crate::team::TeamConfig>,
}

/// A conversation: what the user sees plus the raw Gemini history.
#[derive(Clone, Debug)]
pub struct Session {
    pub meta: SessionMeta,
    pub items: Vec<ChatItem>,
    pub history: Vec<Value>,
}

#[derive(Serialize, Deserialize)]
struct ItemsFile {
    meta: SessionMeta,
    items: Vec<ChatItem>,
}

impl Session {
    pub fn new(project: &str) -> Self {
        let now = now_secs();
        Session {
            meta: SessionMeta {
                id: unique_id(),
                title: "New session".to_string(),
                project: project.to_string(),
                created_at: now,
                updated_at: now,
                team: None,
            },
            items: Vec::new(),
            history: Vec::new(),
        }
    }

    /// A team project in `project`.
    pub fn new_team(project: &str, title: &str, team: crate::team::TeamConfig) -> Self {
        let mut session = Session::new(project);
        session.meta.title = truncate_end(title.trim(), 48);
        session.meta.team = Some(team);
        session
    }

    /// Names the session after its first user message.
    pub fn set_title_from(&mut self, text: &str) {
        if self.meta.title == "New session" {
            let line = text
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("")
                .trim();
            if !line.is_empty() {
                self.meta.title = truncate_end(line, 48);
            }
        }
    }
}

/// Reads and writes sessions in a directory.
#[derive(Clone, Debug)]
pub struct SessionStore {
    dir: PathBuf,
}

impl SessionStore {
    pub fn new(dir: &Path) -> Self {
        Self {
            dir: dir.to_path_buf(),
        }
    }

    fn items_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    fn history_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.history.json"))
    }

    /// All saved sessions, newest first. Unreadable files are skipped.
    pub fn list(&self) -> Vec<SessionMeta> {
        let mut out = Vec::new();
        let Ok(rd) = fs::read_dir(&self.dir) else {
            return out;
        };
        for entry in rd.flatten() {
            let path = entry.path();
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if !name.ends_with(".json") || name.ends_with(".history.json") {
                continue;
            }
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            if let Ok(file) = serde_json::from_str::<ItemsFile>(&text) {
                out.push(file.meta);
            }
        }
        out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then(b.id.cmp(&a.id)));
        out
    }

    pub fn load(&self, id: &str) -> Result<Session> {
        let text = fs::read_to_string(self.items_path(id))
            .with_context(|| format!("session {id} not found"))?;
        let file: ItemsFile = serde_json::from_str(&text).context("session file is corrupt")?;
        let history = fs::read_to_string(self.history_path(id))
            .ok()
            .and_then(|t| serde_json::from_str::<Vec<Value>>(&t).ok())
            .unwrap_or_default();
        Ok(Session {
            meta: file.meta,
            items: file.items,
            history,
        })
    }

    pub fn save(&self, session: &mut Session) -> Result<()> {
        fs::create_dir_all(&self.dir)?;
        session.meta.updated_at = now_secs();
        let mut items = session.items.clone();
        for item in &mut items {
            if let ChatItem::Tool { output, .. } = item {
                if output.chars().count() > MAX_SAVED_OUTPUT {
                    *output = truncate_end(output, MAX_SAVED_OUTPUT);
                }
            }
        }
        let file = ItemsFile {
            meta: session.meta.clone(),
            items,
        };
        write_atomic(
            &self.items_path(&session.meta.id),
            &serde_json::to_vec(&file)?,
        )?;
        write_atomic(
            &self.history_path(&session.meta.id),
            &serde_json::to_vec(&crate::vision::without_images(&session.history))?,
        )?;
        Ok(())
    }

    pub fn delete(&self, id: &str) {
        let _ = fs::remove_file(self.items_path(id));
        let _ = fs::remove_file(self.history_path(id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn save_load_list_delete() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path());
        let mut s = Session::new("/home/me/project");
        s.set_title_from("\n  Fix the login bug in the API server please\nmore");
        s.items.push(ChatItem::User("hello".into()));
        s.items.push(ChatItem::Tool {
            id: 1,
            name: "read_file".into(),
            summary: "Read a.py".into(),
            ok: Some(true),
            result: "3 lines".into(),
            output: "x".repeat(30_000),
            millis: 5,
        });
        s.history
            .push(json!({"role":"user","parts":[{"text":"hello"}]}));
        store.save(&mut s).unwrap();

        let listed = store.list();
        assert_eq!(listed.len(), 1);
        assert_eq!(
            listed[0].title,
            "Fix the login bug in the API server please"
        );

        let loaded = store.load(&s.meta.id).unwrap();
        assert_eq!(loaded.items.len(), 2);
        assert_eq!(loaded.history.len(), 1);
        match &loaded.items[1] {
            ChatItem::Tool { output, .. } => {
                assert!(output.chars().count() <= MAX_SAVED_OUTPUT + 1)
            }
            _ => panic!("expected a tool item"),
        }

        store.delete(&s.meta.id);
        assert!(store.list().is_empty());
        assert!(store.load(&s.meta.id).is_err());
    }

    #[test]
    fn title_is_set_once() {
        let mut s = Session::new("p");
        s.set_title_from("first");
        s.set_title_from("second");
        assert_eq!(s.meta.title, "first");
    }

    #[test]
    fn corrupt_files_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("bad.json"), "{nope").unwrap();
        assert!(SessionStore::new(dir.path()).list().is_empty());
    }
}
