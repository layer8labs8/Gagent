//! Asking the user before the agent does something that needs their say.
//!
//! [`crate::policy`] sorts every command into allowed, refused or "ask". This module
//! holds the questions: the agent's thread waits here while the app shows a dialog (or
//! the command line tool asks on the terminal), and goes on with the answer.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

use crate::config::PermissionMode;
use crate::policy::{self, Verdict};

/// A question for the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AskRequest {
    pub id: u64,
    pub command: String,
    /// Why it is asked, in a short phrase ("installs Python packages with pip").
    pub reason: String,
    /// The kind of thing, used to remember "always allow".
    pub key: String,
}

/// The user's answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// This time only.
    Allow,
    /// This time and every time for the same kind of thing, until the app is closed.
    Always,
    Deny,
}

type Responder = Box<dyn Fn(&AskRequest) -> Decision + Send + Sync>;
type Wake = Box<dyn Fn() + Send + Sync>;

#[derive(Default)]
struct Inner {
    next_id: u64,
    pending: Vec<AskRequest>,
    answers: HashMap<u64, Decision>,
    always: HashSet<String>,
}

/// The place where the agent's questions meet the user's answers. One per app.
pub struct Approvals {
    mode: Mutex<PermissionMode>,
    inner: Mutex<Inner>,
    changed: Condvar,
    /// Answers questions on the spot (the command line tool). Without it, questions wait
    /// in `pending` for [`Approvals::answer`] (the app).
    responder: Mutex<Option<Responder>>,
    /// Called when a question is waiting, so that the app can wake up and show it.
    wake: Mutex<Option<Wake>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl Approvals {
    pub fn new(mode: PermissionMode) -> Self {
        Self {
            mode: Mutex::new(mode),
            inner: Mutex::new(Inner::default()),
            changed: Condvar::new(),
            responder: Mutex::new(None),
            wake: Mutex::new(None),
        }
    }

    pub fn mode(&self) -> PermissionMode {
        *lock(&self.mode)
    }

    pub fn set_mode(&self, mode: PermissionMode) {
        *lock(&self.mode) = mode;
    }

    /// Answers every question with `f` right away, on the agent's own thread.
    pub fn set_responder(&self, f: impl Fn(&AskRequest) -> Decision + Send + Sync + 'static) {
        *lock(&self.responder) = Some(Box::new(f));
    }

    /// `f` is called whenever a question starts waiting.
    pub fn set_wake(&self, f: impl Fn() + Send + Sync + 'static) {
        *lock(&self.wake) = Some(Box::new(f));
    }

    /// The oldest question that waits for an answer.
    pub fn pending(&self) -> Option<AskRequest> {
        lock(&self.inner).pending.first().cloned()
    }

    /// Gives the answer to a waiting question.
    pub fn answer(&self, id: u64, decision: Decision) {
        let mut inner = lock(&self.inner);
        if let Some(pos) = inner.pending.iter().position(|r| r.id == id) {
            let request = inner.pending.remove(pos);
            if decision == Decision::Always {
                inner.always.insert(request.key);
            }
            inner.answers.insert(id, decision);
            self.changed.notify_all();
        }
    }

    /// Forgets every "always allow".
    pub fn forget_always(&self) {
        lock(&self.inner).always.clear();
    }

    /// Decides whether `command` may run. `Ok` when it may; `Err(text)` with the reason
    /// for the model when it may not. `on_host` is true when the command runs directly on
    /// the user's computer (the policy looks at it); Docker and WSL are not checked.
    pub fn authorize(
        &self,
        command: &str,
        workspace: &Path,
        on_host: bool,
        cancel: &AtomicBool,
    ) -> Result<(), String> {
        let verdict = if on_host {
            policy::assess(command, workspace)
        } else {
            Verdict::Allow
        };
        let mode = self.mode();
        let (reason, key) = match verdict {
            Verdict::Refuse(why) => return Err(format!("blocked: {why}")),
            Verdict::Ask { reason, key } => {
                if mode == PermissionMode::All {
                    return Ok(());
                }
                (reason, key)
            }
            Verdict::Allow => {
                if mode != PermissionMode::Ask {
                    return Ok(());
                }
                let word = command.split_whitespace().next().unwrap_or("command");
                ("runs a command".to_string(), format!("cmd:{word}"))
            }
        };
        if lock(&self.inner).always.contains(&key) {
            return Ok(());
        }
        match self.ask(command, &reason, &key, cancel) {
            Some(Decision::Allow | Decision::Always) => Ok(()),
            Some(Decision::Deny) => Err(format!(
                "the user did not allow this ({reason}). Do not look for a way around it. Carry on without it, or tell the user what you need and why."
            )),
            None => Err("cancelled".to_string()),
        }
    }

    /// Asks before the agent reads the internet (a search, a page). Asked in the modes Ask and
    /// Auto; "always" is remembered for the same `key` (a host, or the search).
    pub fn authorize_web(
        &self,
        what: &str,
        reason: &str,
        key: &str,
        cancel: &AtomicBool,
    ) -> Result<(), String> {
        if self.mode() == PermissionMode::All || lock(&self.inner).always.contains(key) {
            return Ok(());
        }
        match self.ask(what, reason, key, cancel) {
            Some(Decision::Allow | Decision::Always) => Ok(()),
            Some(Decision::Deny) => Err(format!(
                "the user did not allow this ({reason}). Do not look for a way around it. Carry on without it, or tell the user what you need and why."
            )),
            None => Err("cancelled".to_string()),
        }
    }

    /// Asks and waits. `None` when the task was cancelled while waiting.
    fn ask(&self, command: &str, reason: &str, key: &str, cancel: &AtomicBool) -> Option<Decision> {
        let request = {
            let mut inner = lock(&self.inner);
            inner.next_id += 1;
            AskRequest {
                id: inner.next_id,
                command: command.to_string(),
                reason: reason.to_string(),
                key: key.to_string(),
            }
        };
        if let Some(responder) = lock(&self.responder).as_ref() {
            let decision = responder(&request);
            if decision == Decision::Always {
                lock(&self.inner).always.insert(request.key.clone());
            }
            return Some(decision);
        }
        let id = request.id;
        lock(&self.inner).pending.push(request);
        if let Some(wake) = lock(&self.wake).as_ref() {
            wake();
        }
        let mut inner = lock(&self.inner);
        loop {
            if let Some(decision) = inner.answers.remove(&id) {
                return Some(decision);
            }
            if cancel.load(Ordering::Relaxed) {
                inner.pending.retain(|r| r.id != id);
                return None;
            }
            inner = match self.changed.wait_timeout(inner, Duration::from_millis(150)) {
                Ok((guard, _)) => guard,
                Err(p) => p.into_inner().0,
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Arc;
    use std::time::Instant;

    fn ws() -> PathBuf {
        PathBuf::from("/home/me/work")
    }

    fn no_cancel() -> AtomicBool {
        AtomicBool::new(false)
    }

    #[test]
    fn auto_mode_only_asks_about_installs_and_risky_commands() {
        let a = Approvals::new(PermissionMode::Auto);
        let asked = Arc::new(AtomicUsize::new(0));
        let n = asked.clone();
        a.set_responder(move |_| {
            n.fetch_add(1, Ordering::SeqCst);
            Decision::Allow
        });
        assert!(a.authorize("python main.py", &ws(), true, &no_cancel()).is_ok());
        assert_eq!(asked.load(Ordering::SeqCst), 0);
        assert!(a.authorize("pip install rich", &ws(), true, &no_cancel()).is_ok());
        assert_eq!(asked.load(Ordering::SeqCst), 1);
        // Docker and WSL commands are not looked at.
        assert!(a.authorize("pip install rich", &ws(), false, &no_cancel()).is_ok());
        assert_eq!(asked.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn dangerous_commands_are_refused_in_every_mode() {
        for mode in PermissionMode::ALL {
            let a = Approvals::new(mode);
            a.set_responder(|_| Decision::Allow);
            let err = a
                .authorize("shutdown /s /t 0", &ws(), true, &no_cancel())
                .unwrap_err();
            assert!(err.starts_with("blocked:"), "{mode:?}: {err}");
            let err = a.authorize("sudo ls", &ws(), true, &no_cancel()).unwrap_err();
            assert!(err.contains("sudo"), "{err}");
        }
    }

    #[test]
    fn a_no_is_explained_to_the_model() {
        let a = Approvals::new(PermissionMode::Auto);
        a.set_responder(|_| Decision::Deny);
        let err = a
            .authorize("npm install left-pad", &ws(), true, &no_cancel())
            .unwrap_err();
        assert!(err.contains("did not allow") && err.contains("npm"), "{err}");
        assert!(err.contains("Do not look for a way around"));
    }

    #[test]
    fn always_allow_is_remembered_for_the_same_kind_only() {
        let a = Approvals::new(PermissionMode::Auto);
        let asked = Arc::new(AtomicUsize::new(0));
        let n = asked.clone();
        a.set_responder(move |_| {
            n.fetch_add(1, Ordering::SeqCst);
            Decision::Always
        });
        assert!(a.authorize("pip install a", &ws(), true, &no_cancel()).is_ok());
        assert!(a.authorize("pip install b && python x.py", &ws(), true, &no_cancel()).is_ok());
        assert_eq!(asked.load(Ordering::SeqCst), 1, "the second pip install is not asked");
        assert!(a.authorize("npm install c", &ws(), true, &no_cancel()).is_ok());
        assert_eq!(asked.load(Ordering::SeqCst), 2, "npm is another kind");
        a.forget_always();
        assert!(a.authorize("pip install d", &ws(), true, &no_cancel()).is_ok());
        assert_eq!(asked.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn ask_mode_asks_for_every_command_and_all_mode_for_none() {
        let asked = Arc::new(AtomicUsize::new(0));
        let n = asked.clone();
        let a = Approvals::new(PermissionMode::Ask);
        a.set_responder(move |r| {
            n.fetch_add(1, Ordering::SeqCst);
            assert!(r.reason.contains("runs a command") || !r.reason.is_empty());
            Decision::Allow
        });
        assert!(a.authorize("ls", &ws(), false, &no_cancel()).is_ok());
        assert!(a.authorize("python a.py", &ws(), true, &no_cancel()).is_ok());
        assert_eq!(asked.load(Ordering::SeqCst), 2);

        a.set_mode(PermissionMode::All);
        assert!(a.authorize("pip install x", &ws(), true, &no_cancel()).is_ok());
        assert!(a.authorize("cat /etc/passwd", &ws(), true, &no_cancel()).is_ok());
        assert_eq!(asked.load(Ordering::SeqCst), 2, "allow-all asks nothing");
    }

    #[test]
    fn the_app_answers_through_the_queue() {
        let a = Arc::new(Approvals::new(PermissionMode::Auto));
        let woken = Arc::new(AtomicUsize::new(0));
        let w = woken.clone();
        a.set_wake(move || {
            w.fetch_add(1, Ordering::SeqCst);
        });
        let agent = {
            let a = a.clone();
            std::thread::spawn(move || a.authorize("pip install rich", &ws(), true, &no_cancel()))
        };
        let started = Instant::now();
        let request = loop {
            if let Some(r) = a.pending() {
                break r;
            }
            assert!(started.elapsed() < Duration::from_secs(5), "no question appeared");
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(request.command, "pip install rich");
        assert_eq!(request.key, "install:pip");
        assert_eq!(woken.load(Ordering::SeqCst), 1);
        a.answer(request.id, Decision::Always);
        assert!(agent.join().unwrap().is_ok());
        assert!(a.pending().is_none());
        // Remembered: the next one does not wait.
        assert!(a.authorize("pip install more", &ws(), true, &no_cancel()).is_ok());
    }

    #[test]
    fn stopping_the_task_ends_the_wait() {
        let a = Arc::new(Approvals::new(PermissionMode::Auto));
        let cancel = Arc::new(AtomicBool::new(false));
        let agent = {
            let (a, cancel) = (a.clone(), cancel.clone());
            std::thread::spawn(move || a.authorize("pip install rich", &ws(), true, &cancel))
        };
        while a.pending().is_none() {
            std::thread::sleep(Duration::from_millis(10));
        }
        let started = Instant::now();
        cancel.store(true, Ordering::Relaxed);
        assert_eq!(agent.join().unwrap(), Err("cancelled".to_string()));
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(a.pending().is_none(), "the question is withdrawn");
    }
}
