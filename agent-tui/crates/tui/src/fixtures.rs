//! Models for tests, built the way discovery builds them.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, SystemTime};

use agent_core::activity::{
    Activity, AgentId, Answer, Ask, Launch, Origin, State, Subagent, Tool, ToolId,
};
use agent_core::discover::{Observation, assemble};
use agent_core::model::{Head, Worktree};
use agent_core::names::SessionName;
use agent_core::paths::{Layout, RepoRoot};
use agent_core::ports::{Change, Lines, RepoWorktrees, SessionInfo, SessionStatus, WorktreeFacts};

use crate::model::Model;
use crate::update::update;
use crate::worker::Msg;

pub fn name(s: &str) -> SessionName {
    s.parse().expect("parses")
}

pub fn wt(path: &str) -> Worktree {
    Worktree {
        path: path.into(),
        head: Head::Detached,
    }
}

pub fn reported(state: State, secs_ago: u64) -> Activity {
    let since = SystemTime::now() - Duration::from_secs(secs_ago);
    Activity {
        state,
        since,
        prompt: Some("go".into()),
        heard: since,
        events: 1,
        asks: Vec::new(),
        subagents: BTreeMap::new(),
    }
}

pub fn asking(question: &str, secs_ago: u64) -> Activity {
    let mut activity = reported(State::Working { tools: Vec::new() }, secs_ago);
    activity.asks.push(Ask {
        asker: Origin::Main,
        question: question.into(),
        answered_by: Answer::AnyTool,
        since: activity.since,
    });
    activity
}

const REPOS: [&str; 5] = ["heucristic", "agent-tui", "tanda_cli", "lanjam", "payroll"];
const AGENTS: [&str; 8] = [
    "triage-flow",
    "rate-limit",
    "auth-refresh",
    "payroll-fix",
    "obs-layout",
    "bell-fix",
    "docs-sweep",
    "leave-request",
];
/// Entries per repo: its main checkout and four agents.
const PER_REPO: usize = 5;

/// What the `k`th entry of a [`fleet`] is doing, cycling through every
/// state a card shows. `None` leaves it without a report: idle when live.
fn doing(k: usize) -> Option<Activity> {
    let path = |p: &str| Some(p.to_string());
    match k % 9 {
        0 => Some(asking("Bash: pnpm test --filter triage", 240)),
        1 => Some(reported(
            State::Failed {
                reason: "rate limit".into(),
            },
            120,
        )),
        2 => Some(reported(
            State::Done {
                summary: Some(
                    "Added token refresh with retry and a **401** interceptor.\n\n14 tests pass. \
                     I didn't open a PR because the branch is behind main by 2."
                        .into(),
                ),
            },
            720,
        )),
        3 => Some(reported(
            State::Working {
                tools: vec![Tool {
                    id: ToolId::new("t1"),
                    name: "Edit".into(),
                    target: path("src/payroll/rules/overtime/calc.ts"),
                    since: SystemTime::now() - Duration::from_secs(38),
                }],
            },
            38,
        )),
        4 => {
            let mut activity = reported(State::Delegating, 180);
            for (i, description) in ["Adversarial review: modelling", "Reverting memo edit", ""]
                .into_iter()
                .enumerate()
            {
                activity.subagents.insert(
                    AgentId::new(format!("s{i}")),
                    Subagent {
                        kind: "general-purpose".into(),
                        description: Some(description.into()),
                        launch: Launch::Background,
                        tools: Vec::new(),
                        since: SystemTime::now() - Duration::from_secs(30 + 10 * i as u64),
                    },
                );
            }
            Some(activity)
        }
        5 => Some(reported(State::Working { tools: Vec::new() }, 10)),
        _ => None,
    }
}

/// Whether the `k`th entry of a [`fleet`] has a session.
fn session(k: usize, cwd: &str, name: &SessionName) -> Option<SessionInfo> {
    let status = match k % 9 {
        7 => SessionStatus::Exited {
            cwd: Some(cwd.into()),
        },
        8 => return None,
        _ => SessionStatus::Live {
            cwd: Some(cwd.into()),
        },
    };
    Some(SessionInfo {
        name: name.clone(),
        status,
    })
}

fn facts(k: usize) -> WorktreeFacts {
    let files = [
        (
            "src/triage/flow.ts",
            Lines::Counted {
                added: 9,
                removed: 3,
            },
        ),
        (
            "src/triage/flow.test.ts",
            Lines::Counted {
                added: 75,
                removed: 0,
            },
        ),
        (
            "src/shared/types.ts",
            Lines::Counted {
                added: 2,
                removed: 0,
            },
        ),
        ("assets/logo.png", Lines::Binary),
        ("notes/plan.md", Lines::Untracked),
        (
            "crates/tui/src/view/sidebar.rs",
            Lines::Counted {
                added: 412,
                removed: 120,
            },
        ),
    ];
    WorktreeFacts {
        ahead: k % 3,
        behind: k % 2,
        changed: files
            .iter()
            .take(k % 7)
            .map(|(path, lines)| Change {
                path: (*path).into(),
                lines: *lines,
            })
            .collect(),
        dirty: k % 4,
    }
}

/// `n` entries over repos of [`PER_REPO`] (each repo's main checkout, then
/// its agents), in states cycling through every kind a card shows, with
/// git facts that overlap within a repo.
pub fn fleet(n: usize) -> Model {
    let mut sessions = Vec::new();
    let mut repos = Vec::new();
    let mut activities = HashMap::new();
    let mut facts_of = HashMap::new();
    for (r, start) in (0..n).step_by(PER_REPO).enumerate() {
        let repo = format!("{}{}", REPOS[r % REPOS.len()], r / REPOS.len());
        let main = format!("/src/{repo}");
        let agents: Vec<String> = (start + 1..n.min(start + PER_REPO))
            .map(|k| format!("{}-{k}", AGENTS[k % AGENTS.len()]))
            .collect();
        let entries = std::iter::once((repo.clone(), main.clone())).chain(
            agents
                .iter()
                .map(|agent| (agent.clone(), format!("/w/{repo}/{agent}"))),
        );
        for (offset, (entry, cwd)) in entries.enumerate() {
            let k = start + offset;
            let entry = name(&entry);
            sessions.extend(session(k, &cwd, &entry));
            if let Some(activity) = doing(k) {
                activities.insert(entry.clone(), activity);
            }
            facts_of.insert(entry, Ok(facts(k)));
        }
        repos.push((
            RepoRoot::new(main.clone().into()).expect("absolute"),
            RepoWorktrees {
                main: wt(&main),
                linked: agents
                    .iter()
                    .map(|agent| wt(&format!("/w/{repo}/{agent}")))
                    .collect(),
            },
        ));
    }
    let observation = Observation {
        sessions,
        repos,
        warnings: Vec::new(),
    };
    let snapshot = assemble(
        &observation,
        &Layout::new("/w".into()).expect("absolute"),
        &[],
    )
    .0;
    let mut m = Model::new(
        "/elsewhere".into(),
        None,
        "claude".parse().expect("parses"),
        "claude".into(),
    );
    update(&mut m, Msg::Snapshot(Ok((snapshot, Vec::new()))));
    m.activities = Some(activities);
    m.facts = facts_of;
    m.revalidate();
    m
}
