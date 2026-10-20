//! Teams of AI agents that build a project together.
//!
//! A *lead* agent turns the goal into a plan (tasks, who does what, what "done" means). The
//! program then hands the tasks to the agents one by one, each on its own model, and every
//! agent leaves a short note for the next. Design: `docs/TEAM_MODE.md`.

mod plan;
mod run;

pub use plan::{assign_owner, Plan, Status, Task};
pub use run::{run_team, TeamEvent, TeamOutcome};

use serde::{Deserialize, Serialize};

use crate::config::Provider;

/// What an agent is for. The lead plans; the others take the tasks that fit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Lead,
    #[default]
    Coder,
    Tester,
    Reviewer,
    Docs,
}

impl Role {
    pub const ALL: [Role; 5] = [Role::Lead, Role::Coder, Role::Tester, Role::Reviewer, Role::Docs];

    pub fn label(self) -> &'static str {
        match self {
            Role::Lead => "lead",
            Role::Coder => "coder",
            Role::Tester => "tester",
            Role::Reviewer => "reviewer",
            Role::Docs => "docs",
        }
    }

    pub fn from_name(name: &str) -> Option<Role> {
        let name = name.trim().to_ascii_lowercase();
        Role::ALL.into_iter().find(|r| r.label() == name)
    }

    /// One sentence for the agent's instructions.
    pub fn brief(self) -> &'static str {
        match self {
            Role::Lead => "You plan the work and keep the pieces consistent.",
            Role::Coder => "You write the code of your tasks and run it to check that it works.",
            Role::Tester => "You test what the others built: run it, try edge cases, fix small bugs you find and report big ones.",
            Role::Reviewer => "You review what the others built: read the code, check that the parts fit together, and fix or report problems.",
            Role::Docs => "You write the documentation: README, comments where they help, usage examples.",
        }
    }
}

/// One member of the team.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentSpec {
    /// Short unique name used in the plan (`coder2`).
    pub name: String,
    pub provider: Provider,
    pub model: String,
    pub role: Role,
    /// Tokens this agent may use in total; 0 means no limit.
    pub token_budget: u64,
    /// Model calls this agent may make for one task.
    pub max_steps: u32,
}

/// Default token budget of an agent.
pub const DEFAULT_AGENT_TOKENS: u64 = 300_000;
/// Default steps of an agent for one task.
pub const DEFAULT_AGENT_STEPS: u32 = 25;

impl AgentSpec {
    /// Reads `provider:model[:role[:tokens]]`, for example
    /// `nvidia:nvidia/nemotron-3.5-super-vl-120b-a12b:lead:400000`. `index` (from 0) makes the name.
    pub fn parse(spec: &str, index: usize) -> Result<AgentSpec, String> {
        let mut parts: Vec<&str> = spec.trim().split(':').collect();
        if parts.len() < 2 {
            return Err(format!("'{spec}': write provider:model[:role[:tokens]]"));
        }
        let provider = Provider::from_name(parts.remove(0))
            .ok_or_else(|| format!("'{spec}': the provider must be gemini, nvidia or openai"))?;
        let mut token_budget = DEFAULT_AGENT_TOKENS;
        if let Some(last) = parts.last() {
            if parts.len() > 1 && !last.is_empty() && last.chars().all(|c| c.is_ascii_digit()) {
                token_budget = last
                    .parse::<u64>()
                    .map_err(|_| format!("'{spec}': the token budget is too large"))?;
                parts.pop();
            }
        }
        let mut role = Role::Coder;
        if let Some(last) = parts.last() {
            if parts.len() > 1 {
                if let Some(r) = Role::from_name(last) {
                    role = r;
                    parts.pop();
                }
            }
        }
        let model = parts.join(":").trim().to_string();
        if model.is_empty() {
            return Err(format!("'{spec}': the model is missing"));
        }
        Ok(AgentSpec {
            name: format!("{}{}", role.label(), index + 1),
            provider,
            model,
            role,
            token_budget,
            max_steps: DEFAULT_AGENT_STEPS,
        })
    }
}

/// What a team project keeps in its session: the goal and the agents.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TeamConfig {
    pub goal: String,
    pub agents: Vec<AgentSpec>,
}

impl TeamConfig {
    pub fn team(&self) -> Result<Team, String> {
        Team::new(self.agents.clone())
    }
}

/// The agents of a project and which one leads.
#[derive(Clone, Debug)]
pub struct Team {
    pub agents: Vec<AgentSpec>,
    pub lead: usize,
}

impl Team {
    /// The lead is the first agent with the role `lead`, or else the first agent.
    pub fn new(agents: Vec<AgentSpec>) -> Result<Team, String> {
        if agents.is_empty() {
            return Err("a team needs at least one agent".into());
        }
        if agents.len() > 12 {
            return Err("a team can have at most 12 agents".into());
        }
        for (i, a) in agents.iter().enumerate() {
            if agents[..i].iter().any(|b| b.name.eq_ignore_ascii_case(&a.name)) {
                return Err(format!("two agents are called '{}'", a.name));
            }
        }
        let lead = agents.iter().position(|a| a.role == Role::Lead).unwrap_or(0);
        Ok(Team { agents, lead })
    }

    pub fn lead(&self) -> &AgentSpec {
        &self.agents[self.lead]
    }

    pub fn find(&self, name: &str) -> Option<&AgentSpec> {
        self.agents
            .iter()
            .find(|a| a.name.eq_ignore_ascii_case(name.trim()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_agent_is_read_from_a_short_text() {
        let a = AgentSpec::parse("nvidia:nvidia/nemotron-3.5-super-vl-120b-a12b:lead:400000", 0).unwrap();
        assert_eq!(a.provider, Provider::Nvidia);
        assert_eq!(a.model, "nvidia/nemotron-3.5-super-vl-120b-a12b");
        assert_eq!(a.role, Role::Lead);
        assert_eq!(a.token_budget, 400_000);
        assert_eq!(a.name, "lead1");

        let a = AgentSpec::parse("nvidia:openai/gpt-oss-20b", 2).unwrap();
        assert_eq!((a.role, a.token_budget, a.name.as_str()), (Role::Coder, DEFAULT_AGENT_TOKENS, "coder3"));

        // A colon inside the model name stays in the model.
        let a = AgentSpec::parse("openai:llama3:8b:tester", 1).unwrap();
        assert_eq!((a.model.as_str(), a.role), ("llama3:8b", Role::Tester));
        let a = AgentSpec::parse("openai:llama3:8b", 0).unwrap();
        assert_eq!(a.model, "llama3:8b");

        assert!(AgentSpec::parse("nvidia", 0).is_err());
        assert!(AgentSpec::parse("nowhere:model", 0).is_err());
        assert!(AgentSpec::parse("nvidia::coder", 0).is_err());
    }

    #[test]
    fn the_lead_is_the_marked_agent_or_the_first() {
        let mk = |s: &str, i| AgentSpec::parse(s, i).unwrap();
        let team = Team::new(vec![mk("nvidia:a", 0), mk("nvidia:b:lead", 1)]).unwrap();
        assert_eq!(team.lead().model, "b");
        let team = Team::new(vec![mk("nvidia:a", 0), mk("nvidia:b", 1)]).unwrap();
        assert_eq!(team.lead().model, "a");
        assert_eq!(team.find("CODER2").unwrap().model, "b");
        assert!(Team::new(vec![]).is_err());
        let mut twins = vec![mk("nvidia:a", 0), mk("nvidia:b", 0)];
        twins[1].name = twins[0].name.clone();
        assert!(Team::new(twins).unwrap_err().contains("two agents"));
    }
}
