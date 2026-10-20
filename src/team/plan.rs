//! The plan of a team project: tasks, who does them, in which order.
//!
//! The lead writes the plan as JSON; the program checks it (a plan that does not hold together
//! is sent back), orders it by its dependencies and keeps it on disk so that a project can be
//! continued after the window was closed.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::{AgentSpec, Role};
use crate::util::{truncate_end, write_atomic};
use crate::workspace::normalize_rel;

const MAX_TASKS: usize = 40;
const MAX_DESCRIPTION_CHARS: usize = 1_500;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    #[default]
    Todo,
    Doing,
    Done,
    Failed,
    /// A task it depends on failed, so it was not started.
    Blocked,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Status::Todo => "todo",
            Status::Doing => "doing",
            Status::Done => "done",
            Status::Failed => "failed",
            Status::Blocked => "blocked",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    /// Files the task creates or changes (relative paths).
    #[serde(default)]
    pub files: Vec<String>,
    /// Ids of the tasks that must be done first.
    #[serde(default)]
    pub deps: Vec<String>,
    /// Name of the agent that does it.
    #[serde(default)]
    pub owner: String,
    /// How to tell that it is done.
    #[serde(default)]
    pub accept: String,
    #[serde(default)]
    pub status: Status,
    /// The owner's short note for the next agent.
    #[serde(default)]
    pub summary: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    #[serde(default)]
    pub goal: String,
    /// Names, files and rules every agent keeps to (the contract between the parts).
    #[serde(default)]
    pub conventions: String,
    pub tasks: Vec<Task>,
}

/// The JSON object in a reply: inside a code fence, or the outermost braces.
fn json_in(reply: &str) -> Option<&str> {
    let start = reply.find('{')?;
    let end = reply.rfind('}')?;
    (end > start).then(|| &reply[start..=end])
}

impl Plan {
    /// Reads the plan out of the lead's reply.
    pub fn parse(reply: &str) -> Result<Plan, String> {
        let json = json_in(reply)
            .ok_or_else(|| "the reply holds no JSON object (write { ... } and nothing else)".to_string())?;
        serde_json::from_str::<Plan>(json)
            .map_err(|e| format!("the JSON does not have the expected shape: {e}"))
    }

    /// Checks the plan and fills in what can be filled in (owners that are missing or unknown
    /// are chosen by the program). `Err` lists what is wrong, so it can be sent back.
    pub fn validate(&mut self, agents: &[AgentSpec]) -> Result<(), String> {
        let mut problems: Vec<String> = Vec::new();
        if self.tasks.is_empty() {
            return Err("the plan has no tasks".into());
        }
        if self.tasks.len() > MAX_TASKS {
            return Err(format!("the plan has {} tasks; the most allowed is {MAX_TASKS}", self.tasks.len()));
        }
        let mut seen: HashSet<String> = HashSet::new();
        for t in &mut self.tasks {
            t.id = t.id.trim().to_string();
            t.title = t.title.trim().to_string();
            t.description = truncate_end(t.description.trim(), MAX_DESCRIPTION_CHARS);
            if t.id.is_empty() {
                problems.push("a task has no id".into());
            } else if !seen.insert(t.id.clone()) {
                problems.push(format!("the id '{}' is used twice", t.id));
            }
            if t.title.is_empty() {
                problems.push(format!("task '{}' has no title", t.id));
            }
            for f in &mut t.files {
                match normalize_rel(f) {
                    Ok(clean) => *f = clean,
                    Err(_) => problems.push(format!(
                        "task '{}': the file '{f}' must be a relative path inside the project",
                        t.id
                    )),
                }
            }
        }
        for t in &self.tasks {
            for d in &t.deps {
                if d == &t.id {
                    problems.push(format!("task '{}' depends on itself", t.id));
                } else if !seen.contains(d) {
                    problems.push(format!("task '{}' depends on '{d}', which does not exist", t.id));
                }
            }
        }
        if problems.is_empty() && self.order_checked().is_none() {
            problems.push("the tasks depend on each other in a circle".into());
        }
        if !problems.is_empty() {
            problems.truncate(8);
            return Err(problems.join("; "));
        }
        // Owners: the named agent, or one chosen by the program.
        let mut load: HashMap<String, usize> = HashMap::new();
        for i in 0..self.tasks.len() {
            let named = agents
                .iter()
                .find(|a| a.name.eq_ignore_ascii_case(self.tasks[i].owner.trim()))
                .map(|a| a.name.clone());
            let owner = match named {
                Some(n) => n,
                None => assign_owner(&self.tasks[i], agents, &load),
            };
            *load.entry(owner.clone()).or_default() += 1;
            self.tasks[i].owner = owner;
        }
        Ok(())
    }

    /// Indexes of the tasks in an order where every task comes after its dependencies.
    pub fn order(&self) -> Vec<usize> {
        self.order_checked().unwrap_or_else(|| (0..self.tasks.len()).collect())
    }

    /// `None` when the dependencies form a circle.
    fn order_checked(&self) -> Option<Vec<usize>> {
        let index: HashMap<&str, usize> = self
            .tasks
            .iter()
            .enumerate()
            .map(|(i, t)| (t.id.as_str(), i))
            .collect();
        let mut done = vec![false; self.tasks.len()];
        let mut order = Vec::with_capacity(self.tasks.len());
        while order.len() < self.tasks.len() {
            let before = order.len();
            for (i, t) in self.tasks.iter().enumerate() {
                if done[i] {
                    continue;
                }
                let ready = t
                    .deps
                    .iter()
                    .all(|d| index.get(d.as_str()).is_none_or(|&j| done[j]));
                if ready {
                    done[i] = true;
                    order.push(i);
                }
            }
            if order.len() == before {
                return None;
            }
        }
        Some(order)
    }

    pub fn count(&self, status: Status) -> usize {
        self.tasks.iter().filter(|t| t.status == status).count()
    }

    /// The plan file of a project, inside the private copy (hidden from the diff).
    fn file(work: &Path) -> std::path::PathBuf {
        work.join(".gagent").join("project").join("plan.json")
    }

    pub fn save(&self, work: &Path) -> Result<()> {
        let path = Self::file(work);
        std::fs::create_dir_all(path.parent().unwrap_or(work)).context("cannot create the project folder")?;
        write_atomic(&path, &serde_json::to_vec_pretty(self)?)
    }

    pub fn load(work: &Path) -> Option<Plan> {
        let text = std::fs::read_to_string(Self::file(work)).ok()?;
        serde_json::from_str(&text).ok()
    }
}

/// Chooses an agent for a task by what the task is about: the agent of the fitting role who
/// has the fewest tasks so far, and among those the one that answers fastest.
pub fn assign_owner(task: &Task, agents: &[AgentSpec], load: &HashMap<String, usize>) -> String {
    let text = format!("{} {}", task.title, task.description).to_ascii_lowercase();
    let wanted = if ["test", "verify", "check that", "qa "].iter().any(|k| text.contains(k)) {
        Role::Tester
    } else if ["review", "audit"].iter().any(|k| text.contains(k)) {
        Role::Reviewer
    } else if ["readme", "documentation", "docs"].iter().any(|k| text.contains(k)) {
        Role::Docs
    } else {
        Role::Coder
    };
    let fitting: Vec<&AgentSpec> = agents.iter().filter(|a| a.role == wanted).collect();
    let pool: Vec<&AgentSpec> = if !fitting.is_empty() {
        fitting
    } else {
        // Nobody has that role: coders first, then anyone.
        let coders: Vec<&AgentSpec> = agents.iter().filter(|a| a.role == Role::Coder).collect();
        if coders.is_empty() { agents.iter().collect() } else { coders }
    };
    pool.into_iter()
        .min_by(|a, b| {
            let cost = |s: &AgentSpec| {
                let tasks = load.get(&s.name).copied().unwrap_or(0) as f32;
                let secs = crate::modelstats::typical_secs(&s.model).unwrap_or(10.0);
                tasks * 100.0 + secs
            };
            cost(a).total_cmp(&cost(b))
        })
        .map(|a| a.name.clone())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agents() -> Vec<AgentSpec> {
        vec![
            AgentSpec::parse("nvidia:lead-model:lead", 0).unwrap(),
            AgentSpec::parse("nvidia:coder-a", 1).unwrap(),
            AgentSpec::parse("nvidia:coder-b", 2).unwrap(),
            AgentSpec::parse("nvidia:tester-model:tester", 3).unwrap(),
        ]
    }

    const PLAN: &str = r#"Here is the plan:
```json
{"goal":"notes app","conventions":"ids: #list, #add","tasks":[
 {"id":"t1","title":"HTML","files":["index.html"],"owner":"coder2"},
 {"id":"t2","title":"CSS","files":["style.css"],"deps":["t1"],"owner":"coder3"},
 {"id":"t3","title":"Test the page","deps":["t1","t2"]}
]}
```"#;

    #[test]
    fn a_plan_is_found_in_the_reply() {
        let plan = Plan::parse(PLAN).unwrap();
        assert_eq!(plan.tasks.len(), 3);
        assert_eq!(plan.conventions, "ids: #list, #add");
        assert!(Plan::parse("no json here").is_err());
        assert!(Plan::parse("{\"tasks\": 5}").unwrap_err().contains("shape"));
    }

    #[test]
    fn a_good_plan_is_ordered_and_gets_owners() {
        let mut plan = Plan::parse(PLAN).unwrap();
        plan.validate(&agents()).unwrap();
        assert_eq!(plan.tasks[0].owner, "coder2");
        assert_eq!(plan.tasks[1].owner, "coder3");
        // Nobody was named for the test task: the tester takes it.
        assert_eq!(plan.tasks[2].owner, "tester4");
        assert_eq!(plan.order(), vec![0, 1, 2]);
    }

    #[test]
    fn dependencies_decide_the_order() {
        let mut plan = Plan::parse(
            r#"{"tasks":[{"id":"b","title":"B","deps":["a"]},{"id":"a","title":"A"},{"id":"c","title":"C","deps":["b"]}]}"#,
        )
        .unwrap();
        plan.validate(&agents()).unwrap();
        let ids: Vec<&str> = plan.order().into_iter().map(|i| plan.tasks[i].id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b", "c"]);
    }

    #[test]
    fn a_plan_that_does_not_hold_together_is_explained() {
        let bad = |json: &str| Plan::parse(json).unwrap().validate(&agents()).unwrap_err();
        assert!(bad(r#"{"tasks":[]}"#).contains("no tasks"));
        assert!(bad(r#"{"tasks":[{"id":"a","title":"x"},{"id":"a","title":"y"}]}"#).contains("used twice"));
        assert!(bad(r#"{"tasks":[{"id":"a","title":"x","deps":["zz"]}]}"#).contains("does not exist"));
        assert!(bad(r#"{"tasks":[{"id":"a","title":"x","deps":["a"]}]}"#).contains("itself"));
        assert!(bad(r#"{"tasks":[{"id":"a","title":"x","deps":["b"]},{"id":"b","title":"y","deps":["a"]}]}"#)
            .contains("circle"));
        assert!(bad(r#"{"tasks":[{"id":"a","title":"x","files":["../secret"]}]}"#).contains("relative path"));
        assert!(bad(r#"{"tasks":[{"id":"a","title":""}]}"#).contains("no title"));
    }

    #[test]
    fn unknown_owners_are_replaced_and_work_is_shared() {
        let mut plan = Plan::parse(
            r#"{"tasks":[
              {"id":"a","title":"Write A","owner":"nobody"},
              {"id":"b","title":"Write B"},
              {"id":"c","title":"Write C"},
              {"id":"d","title":"Review everything"}
            ]}"#,
        )
        .unwrap();
        plan.validate(&agents()).unwrap();
        let owners: Vec<&str> = plan.tasks.iter().map(|t| t.owner.as_str()).collect();
        // The two coders share the three coding tasks; with nobody to review, a coder takes it.
        assert_eq!(owners[0], "coder2");
        assert_eq!(owners[1], "coder3");
        assert_eq!(owners[2], "coder2");
        assert!(owners[3].starts_with("coder"), "{owners:?}");
    }

    #[test]
    fn the_plan_is_kept_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Plan::load(dir.path()).is_none());
        let mut plan = Plan::parse(PLAN).unwrap();
        plan.validate(&agents()).unwrap();
        plan.tasks[0].status = Status::Done;
        plan.tasks[0].summary = "made index.html".into();
        plan.save(dir.path()).unwrap();
        let back = Plan::load(dir.path()).unwrap();
        assert_eq!(back, plan);
        assert_eq!(back.count(Status::Done), 1);
        assert_eq!(back.count(Status::Todo), 2);
    }
}
