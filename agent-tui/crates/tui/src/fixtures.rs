//! Models for tests, built the way discovery builds them.

use std::collections::HashMap;
use std::time::{Duration, SystemTime};

use agent_core::activity::{
    Activity, AgentId, Answer, Ask, Join, Launch, Origin, Session, State, Subagent, Tool, ToolId,
};
use agent_core::discover::{Observation, assemble};
use agent_core::model::{Head, Worktree};
use agent_core::names::SessionName;
use agent_core::paths::{Layout, RepoRoot};
use agent_core::ports::{
    Added, Change, Checkout, Compared, Lines, Place, RepoWorktrees, SessionInfo, SessionStatus,
    TrackedState, WorktreeFacts,
};

use crate::model::{Company, Connection, Cursor, Mode, Model, Pane};
use crate::update::{Effect, update};
use crate::worker::Msg;

/// The company of an agent whose other window is an nvim tab.
pub fn in_nvim() -> Company {
    Company::of(vec![Place::Nvim])
}

/// Answers the newest look as finding `windows` (sessions by company), and
/// every other agent alone.
pub fn sighted(m: &mut Model, windows: &[(&str, Company)]) -> Vec<Effect> {
    let ticket = m.current_look().expect("a look was asked for");
    let found = windows
        .iter()
        .map(|(agent, company)| (name(agent), company.clone()))
        .collect();
    update(
        m,
        Msg::Elsewhere {
            ticket,
            found: Ok(found),
        },
    )
}

/// A pty running `cat` as the pane's client for `agent`.
pub fn connect(m: &mut Model, agent: &str, mode: Mode) {
    m.pane = Pane::Connected(Connection {
        name: name(agent),
        term: cat(),
        mode,
        opened: std::time::Instant::now(),
        id: agent_core::ports::ClientId::new(1),
    });
}

pub fn cat() -> agent_term::Terminal<Box<dyn agent_term::Emulator>> {
    let size = agent_term::TermSize::of(ratatui::layout::Rect::new(0, 0, 80, 24)).expect("size");
    agent_term::Terminal::spawn(
        &agent_core::process::LaunchSpec::new("cat"),
        size,
        Box::new(agent_term::Vt100::new(size)) as Box<dyn agent_term::Emulator>,
    )
    .expect("a pty")
}

/// The peek float watching `agent`'s session, which the other window shows
/// at 200 by 50.
pub fn watch(m: &mut Model, agent: &str) {
    let size = |n| std::num::NonZeroU16::new(n).expect("nonzero");
    m.peek = Some(crate::model::Peek::Watch(crate::model::Watch {
        name: name(agent),
        view: crate::model::WatchView::Watched(crate::model::Watcher {
            term: cat(),
            opened: std::time::Instant::now(),
            seen: (size(200), size(50)),
            measure: crate::model::Measure::Idle,
        }),
    }));
}

/// A fleet with the cursor on `agent`, whose other window has the pane paused.
pub fn elsewhere_on(agent: &str) -> Model {
    let mut m = fleet(10);
    m.set_cursor(Cursor::Entry(name(agent)));
    m.pane = Pane::Elsewhere { name: name(agent) };
    m.record_company(&name(agent), in_nvim());
    m
}

pub fn nz(n: usize) -> std::num::NonZeroUsize {
    std::num::NonZeroUsize::new(n).expect("a line number")
}

/// An added line `new` of `path`, as the diff view lists it.
pub fn line_info(path: &str, text: &str, new: usize) -> crate::review::LineInfo {
    crate::review::LineInfo {
        row: 0,
        height: 1,
        path: path.into(),
        kind: crate::review::LineKind::Added,
        old: None,
        new: Some(new),
        text: text.into(),
        edit: nz(new),
    }
}

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
    let mut activity = Activity::fresh(since);
    activity.state = state;
    activity.prompt = Some("go".into());
    activity.prompts = 1;
    activity.prompt_event = 1;
    activity.events = 1;
    activity
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

/// `agent` with a question open.
pub fn ask(m: &mut Model, agent: &str) {
    m.activities
        .get_or_insert_default()
        .insert(name(agent), asking("rm?", 1));
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
                    Subagent::running(
                        "general-purpose",
                        Some(description.into()),
                        Launch::Background,
                        SystemTime::now() - Duration::from_secs(30 + 10 * i as u64),
                    ),
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

fn fleet_facts(k: usize) -> WorktreeFacts {
    let files = [
        (
            "src/triage/flow.ts",
            Some(Lines::Counted {
                added: 9,
                removed: 3,
            }),
        ),
        (
            "src/triage/flow.test.ts",
            Some(Lines::Counted {
                added: 75,
                removed: 0,
            }),
        ),
        (
            "src/shared/types.ts",
            Some(Lines::Counted {
                added: 2,
                removed: 0,
            }),
        ),
        ("assets/logo.png", Some(Lines::Binary)),
        ("notes/plan.md", None),
        (
            "crates/tui/src/view/sidebar.rs",
            Some(Lines::Counted {
                added: 412,
                removed: 120,
            }),
        ),
    ];
    let changed: Vec<Change> = files
        .iter()
        .take(k % 7)
        .map(|(path, lines)| match lines {
            Some(lines) => Change::tracked(*path, *lines, TrackedState::Committed),
            None => Change::untracked(*path, Added::Unread),
        })
        .collect();
    WorktreeFacts {
        ahead: k % 3,
        behind: k % 2,
        pending: changed.clone(),
        changed,
        compared: Compared::Branch {
            branch: "feature".parse().expect("parses"),
            base: "origin/main".parse().expect("parses"),
        },
    }
}

/// A worktree with nothing changed, on a detached checkout with no base.
pub fn model(start_dir: &str) -> Model {
    Model::new(
        start_dir.into(),
        None,
        "claude".parse().expect("parses"),
        "claude".into(),
        agent_core::testing::NAME_LIMIT,
        None,
    )
}

pub fn unchanged() -> WorktreeFacts {
    WorktreeFacts::unchanged(Compared::NoBase {
        checkout: Checkout::Detached("abc1234".into()),
    })
}

/// A worktree with `pending` files uncommitted and `changed` since the fork.
pub fn facts(pending: Vec<Change>, changed: Vec<Change>) -> WorktreeFacts {
    WorktreeFacts {
        pending,
        changed,
        ..unchanged()
    }
}

/// A file committed with two lines added and one removed.
pub fn modified(path: &str) -> Change {
    Change::tracked(
        path,
        Lines::Counted {
            added: 2,
            removed: 1,
        },
        TrackedState::Committed,
    )
}

/// `n` uncommitted files with no counts.
pub fn pending_files(n: usize) -> Vec<Change> {
    (0..n)
        .map(|i| Change::untracked(format!("pending{i}"), Added::Unread))
        .collect()
}

/// `n` entries over repos of [`PER_REPO`] (each repo's main checkout, then
/// its agents), in states cycling through every kind a card shows, with
/// git facts that overlap within a repo.
pub fn fleet(n: usize) -> Model {
    let mut sessions = Vec::new();
    let mut repos = Vec::new();
    let mut activities = HashMap::new();
    let mut facts_of = HashMap::new();
    let mut pins = Vec::new();
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
            if k % 10 == 0 {
                pins.push(entry.clone());
            }
            facts_of.insert(entry, Ok(fleet_facts(k)));
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
        &pins,
        agent_core::testing::NAME_LIMIT,
    )
    .0;
    let mut m = model("/elsewhere");
    m.view = crate::model::View::Focus;
    update(&mut m, Msg::Snapshot(Ok((snapshot, Vec::new()))));
    m.activities = Some(activities);
    for (name, facts) in facts_of {
        m.set_facts(&name, facts.map(crate::model::Inspected::of));
    }
    m.revalidate();
    m.changes_view = crate::tree::ChangesView::Flat;
    m
}

/// A session launched `ago` ago, with nothing reported since.
pub fn launched(ago: Duration) -> Session {
    Session::Launched {
        join: Join::Untraced,
        at: SystemTime::now() - ago,
    }
}

pub fn booting() -> Session {
    Session::Booting {
        join: Join::Untraced,
    }
}

/// A restart plan for `agent`, which `world` gets doing `doing`.
pub fn restart_plan(
    world: &agent_core::testing::World,
    agent: &str,
    doing: agent_core::testing::Doing,
) -> agent_core::ops::RestartPlan {
    let at = world.agent(agent, doing);
    agent_core::ops::plan_restart(world.svc(), at)
        .expect("plans")
        .elsewhere()
        .expect("not own")
}

pub fn restart_fleet(world: &agent_core::testing::World) -> agent_core::ops::Fleet {
    agent_core::ops::plan_restarts(world.svc()).expect("plans")
}
