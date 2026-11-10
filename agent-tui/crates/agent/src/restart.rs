use std::io::Write;

use agent_core::model::{Startable, Unstartable};
use agent_core::names::SessionName;
use agent_core::ops::{self, Error, Fleet, Interrupt, Interrupting, Planned, Restart, Services};
use anyhow::{Context, Result, bail};

/// Whether a person can answer a question.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Terminal {
    Attached,
    Absent,
}

/// What restarting one agent does when it is busy.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OnBusy {
    /// Ask first, or refuse where nobody can answer.
    Ask,
    /// Interrupt it.
    Force,
}

/// What restarting every agent does with the busy ones.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Policy {
    Ask,
    Force,
    /// Leave them out.
    IdleOnly,
}

/// What `agent restart` was asked to restart.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    One(Option<SessionName>, OnBusy),
    All(Policy),
}

impl Target {
    /// What the arguments ask for. `--all` takes no name and `--idle` needs
    /// `--all`, which the parser has already seen to.
    pub fn from_args(name: Option<SessionName>, force: bool, all: bool, idle: bool) -> Self {
        match (all, force, idle) {
            (true, true, _) => Self::All(Policy::Force),
            (true, false, true) => Self::All(Policy::IdleOnly),
            (true, false, false) => Self::All(Policy::Ask),
            (false, true, _) => Self::One(name, OnBusy::Force),
            (false, false, _) => Self::One(name, OnBusy::Ask),
        }
    }
}

/// What the command reads and writes, and how the caller's own session is
/// handed over, so none of it needs a terminal or a process.
pub struct Console<'a> {
    pub terminal: Terminal,
    pub out: &'a mut dyn Write,
    pub err: &'a mut dyn Write,
    pub ask: &'a mut dyn FnMut(&str) -> bool,
    /// Restarts the caller's own session from a process that outlives it.
    pub hand_over: &'a mut dyn FnMut(&SessionName) -> Result<()>,
}

fn startable(svc: Services<'_>, name: &SessionName) -> Result<Startable> {
    let (snapshot, _) = ops::snapshot(svc)?;
    let (_, entry) = snapshot
        .entry(name)
        .with_context(|| format!("no agent or session named {name}"))?;
    entry.startable().map_err(|why| match why {
        Unstartable::NotOurs => {
            anyhow::anyhow!("{name} wasn't started by agent, so it can't be restarted")
        }
        Unstartable::Unhosted(why) => anyhow::anyhow!("{name} can't be restarted: {why}"),
    })
}

fn handed_over(out: &mut dyn Write, name: &SessionName) -> Result<()> {
    writeln!(
        out,
        "agent restart: restarting {name} in the background. This session is about to end"
    )?;
    Ok(())
}

pub fn one(
    svc: Services<'_>,
    name: &SessionName,
    on_busy: OnBusy,
    console: &mut Console<'_>,
) -> Result<()> {
    let plan = match ops::plan_restart(svc, startable(svc, name)?)? {
        Planned::Own(own) => {
            if let (OnBusy::Ask, Terminal::Attached, Some(interrupts)) =
                (on_busy, console.terminal, own.plan().interrupts())
                && !(console.ask)(&format!(
                    "{name} is {}. Restart it? [y/N] ",
                    interrupts.label()
                ))
            {
                bail!("not restarting {name}");
            }
            (console.hand_over)(name)?;
            return handed_over(console.out, name);
        }
        Planned::Elsewhere(plan) => plan,
    };
    let go = match plan.approve() {
        Ok(go) => go,
        Err(interrupting) => {
            interrupt(name, &interrupting, on_busy, console).map(|()| interrupting.force())?
        }
    };
    let label = go.plan().label();
    let restarted = ops::restart(svc, go)?;
    writeln!(
        console.out,
        "agent restart: {}",
        restarted.end.says(name, label)
    )?;
    Ok(())
}

/// Whether `interrupting` may go ahead, asking where somebody can answer.
fn interrupt(
    name: &SessionName,
    interrupting: &Interrupting,
    on_busy: OnBusy,
    console: &mut Console<'_>,
) -> Result<()> {
    let busy = interrupting.interrupts().label();
    match (on_busy, console.terminal) {
        (OnBusy::Force, _) => Ok(()),
        (OnBusy::Ask, Terminal::Attached) => {
            if (console.ask)(&format!("{name} is {busy}. Restart it? [y/N] ")) {
                Ok(())
            } else {
                bail!("not restarting {name}")
            }
        }
        (OnBusy::Ask, Terminal::Absent) => {
            bail!("not restarting {name}: it is {busy} (re-run with --force to interrupt it)")
        }
    }
}

fn busy_list(fleet: &Fleet) -> Vec<String> {
    fleet
        .busy()
        .iter()
        .map(|b| format!("  {}: {}", b.plan().name(), b.interrupts().label()))
        .collect()
}

/// What `agent restart --all` would do, with the caller's own session, which
/// goes last, counted in.
fn said(fleet: &Fleet, own: bool) -> String {
    let summary = fleet.summary_counting(usize::from(own));
    if own {
        format!("{summary} · this session last")
    } else {
        summary
    }
}

pub fn all(svc: Services<'_>, policy: Policy, console: &mut Console<'_>) -> Result<()> {
    let fleet = ops::plan_restarts(svc)?;
    for (name, why) in fleet.unplanned() {
        writeln!(console.err, "agent restart: {name}: {why}")?;
    }
    if fleet.is_empty() && fleet.own().is_none() {
        writeln!(console.out, "agent restart: nothing to restart")?;
        return Ok(());
    }
    let own = fleet.own().map(|own| (own.name(), own.plan().interrupts()));
    let interrupt = match (policy, console.terminal, fleet.busy().is_empty()) {
        (Policy::Force, ..) | (Policy::Ask, Terminal::Absent, true) => Interrupt::Allow,
        (Policy::IdleOnly, ..) => Interrupt::Skip,
        (Policy::Ask, Terminal::Attached, _) => {
            writeln!(
                console.out,
                "agent restart: {}",
                said(&fleet, own.is_some())
            )?;
            for line in busy_list(&fleet) {
                writeln!(console.out, "{line}")?;
            }
            if !(console.ask)(&format!("{}? [y/N] ", said(&fleet, own.is_some()))) {
                bail!("aborted");
            }
            Interrupt::Allow
        }
        (Policy::Ask, Terminal::Absent, false) => bail!(
            "not restarting, these are busy:\n{}\nre-run with --force to interrupt them or --idle to leave them out",
            busy_list(&fleet).join("\n")
        ),
    };
    let approvals = fleet.approve(interrupt);
    for skipped in &approvals.skipped {
        writeln!(
            console.out,
            "agent restart: skipped {}: {}",
            skipped.plan().name(),
            skipped.interrupts().label()
        )?;
    }
    let failed = restart_each(svc, approvals.restarts, console)?;
    if let Some((name, interrupts)) = own {
        if let (Policy::IdleOnly, Some(interrupts)) = (policy, interrupts) {
            writeln!(
                console.out,
                "agent restart: skipped {name}: {}",
                interrupts.label()
            )?;
        } else {
            (console.hand_over)(&name)?;
            handed_over(console.out, &name)?;
        }
    }
    if failed > 0 {
        bail!("{failed} agent(s) not restarted");
    }
    Ok(())
}

/// Restarts them one after another, saying how each went, and counts the
/// failures.
fn restart_each(
    svc: Services<'_>,
    restarts: Vec<Restart>,
    console: &mut Console<'_>,
) -> Result<usize> {
    let mut failed = 0;
    for go in restarts {
        let (name, label) = (go.name(), go.plan().label());
        match ops::restart(svc, go) {
            Ok(restarted) => {
                writeln!(
                    console.out,
                    "agent restart: {}",
                    restarted.end.says(&name, label)
                )?;
            }
            Err(Error::Busy { name, interrupts }) => {
                writeln!(
                    console.out,
                    "agent restart: skipped {name}: {}",
                    interrupts.became()
                )?;
            }
            Err(e) => {
                failed += 1;
                writeln!(console.err, "agent restart: {name}: {e}")?;
            }
        }
    }
    Ok(failed)
}

#[cfg(test)]
mod tests {
    use agent_core::testing::{Doing, Lurking, Stopping, World};

    use super::*;

    fn session(s: &str) -> SessionName {
        s.parse().expect("parses")
    }

    struct Run {
        result: Result<()>,
        out: String,
        err: String,
        asked: Vec<String>,
        handed: Vec<SessionName>,
    }

    fn run(
        terminal: Terminal,
        answer: bool,
        act: impl FnOnce(&mut Console<'_>) -> Result<()>,
    ) -> Run {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let mut asked = Vec::new();
        let mut handed = Vec::new();
        let result = {
            let mut ask = |q: &str| {
                asked.push(q.to_string());
                answer
            };
            let mut hand_over = |name: &SessionName| {
                handed.push(name.clone());
                Ok(())
            };
            let mut console = Console {
                terminal,
                out: &mut out,
                err: &mut err,
                ask: &mut ask,
                hand_over: &mut hand_over,
            };
            act(&mut console)
        };
        Run {
            result,
            out: String::from_utf8(out).expect("utf8"),
            err: String::from_utf8(err).expect("utf8"),
            asked,
            handed,
        }
    }

    fn one_of(w: &World, name: &str, on_busy: OnBusy, terminal: Terminal, answer: bool) -> Run {
        run(terminal, answer, |c| {
            one(w.svc(), &session(name), on_busy, c)
        })
    }

    #[test]
    fn an_idle_agent_restarts_without_asking_and_exits_ok() {
        let w = World::new();
        w.agent("fix", Doing::Idle);
        let r = one_of(&w, "fix", OnBusy::Ask, Terminal::Absent, false);
        assert!(r.result.is_ok(), "{:?}", r.result);
        assert_eq!(r.out, "agent restart: restarted fix\n");
        assert_eq!(r.asked.len(), 0);
        assert_eq!(w.host.detached().len(), 1);
    }

    #[test]
    fn a_busy_agent_without_a_terminal_is_refused_unless_forced() {
        let w = World::new();
        w.agent("fix", Doing::Working);
        let refused = one_of(&w, "fix", OnBusy::Ask, Terminal::Absent, true);
        assert_eq!(
            refused.result.expect_err("refused").to_string(),
            "not restarting fix: it is working (re-run with --force to interrupt it)"
        );
        assert_eq!(w.host.killed().len(), 0);
        let forced = one_of(&w, "fix", OnBusy::Force, Terminal::Absent, false);
        assert!(forced.result.is_ok());
        assert_eq!(w.host.killed().len(), 1);
    }

    #[test]
    fn a_busy_agent_at_a_terminal_restarts_only_on_a_yes() {
        let w = World::new();
        w.agent("fix", Doing::Working);
        let no = one_of(&w, "fix", OnBusy::Ask, Terminal::Attached, false);
        assert_eq!(no.asked, ["fix is working. Restart it? [y/N] "]);
        assert!(no.result.is_err());
        assert_eq!(w.host.killed().len(), 0);
        let yes = one_of(&w, "fix", OnBusy::Ask, Terminal::Attached, true);
        assert!(yes.result.is_ok());
    }

    #[test]
    fn a_restart_that_fails_exits_non_zero_and_says_why() {
        let w = World::new();
        let at = w.agent("fix", Doing::Idle);
        w.harnesses.0.lurk(
            at.cwd(),
            Lurking::Survivor(agent_core::ports::SurvivorState::Idle),
        );
        *w.harnesses.0.stop.lock().expect("lock") = Stopping::Fails;
        let r = one_of(&w, "fix", OnBusy::Ask, Terminal::Absent, false);
        assert!(r.result.is_err());
        assert_eq!(r.out, "");
    }

    #[test]
    fn the_callers_own_session_is_handed_over_not_restarted_here() {
        let mut w = World::new();
        w.own = Some(session("me"));
        w.agent("me", Doing::Working);
        let r = one_of(&w, "me", OnBusy::Ask, Terminal::Absent, false);
        assert!(r.result.is_ok());
        assert_eq!(r.handed, [session("me")]);
        assert_eq!(w.host.killed().len(), 0);
    }

    #[test]
    fn an_unrecorded_session_fails_the_command() {
        let w = World::new();
        w.agent("fix", Doing::Idle);
        w.store.records.lock().expect("lock").clear();
        let r = one_of(&w, "fix", OnBusy::Ask, Terminal::Absent, false);
        assert!(r.result.is_err());
    }

    fn all_of(w: &World, policy: Policy, terminal: Terminal, answer: bool) -> Run {
        run(terminal, answer, |c| all(w.svc(), policy, c))
    }

    #[test]
    fn all_without_a_terminal_refuses_busy_agents_unless_forced_or_idle() {
        let w = World::new();
        w.agent("idle", Doing::Idle);
        w.agent("busy", Doing::Working);
        let refused = all_of(&w, Policy::Ask, Terminal::Absent, false);
        let error = refused.result.expect_err("refused").to_string();
        assert!(error.contains("busy: working"), "{error}");
        assert_eq!(w.host.killed().len(), 0);

        let idle = all_of(&w, Policy::IdleOnly, Terminal::Absent, false);
        assert!(idle.result.is_ok());
        assert!(idle.out.contains("skipped busy: working"), "{}", idle.out);
        assert_eq!(w.host.killed().len(), 1);

        let forced = all_of(&w, Policy::Force, Terminal::Absent, false);
        assert!(forced.result.is_ok());
        assert!(forced.out.contains("restarted busy"), "{}", forced.out);
    }

    #[test]
    fn all_at_a_terminal_asks_even_when_nothing_is_busy_and_never_says_restart_zero() {
        let w = World::new();
        w.agent("idle", Doing::Idle);
        let declined = all_of(&w, Policy::Ask, Terminal::Attached, false);
        assert!(declined.result.is_err());
        assert_eq!(declined.asked, ["restart 1? [y/N] "]);
        assert_eq!(w.host.killed().len(), 0);
        let accepted = all_of(&w, Policy::Ask, Terminal::Attached, true);
        assert!(accepted.result.is_ok());
        assert_eq!(w.host.killed().len(), 1);

        let only_stops = World::new();
        only_stops.ghost(
            "ghost",
            Lurking::Survivor(agent_core::ports::SurvivorState::Idle),
        );
        let asked = all_of(&only_stops, Policy::Ask, Terminal::Attached, false);
        assert_eq!(asked.asked, ["stop 1 exited? [y/N] "]);
    }

    #[test]
    fn all_restarts_its_own_session_last_and_exits_one_when_an_agent_fails() {
        let mut w = World::new();
        w.own = Some(session("me"));
        w.agent("me", Doing::Idle);
        w.ghost(
            "ghost",
            Lurking::Survivor(agent_core::ports::SurvivorState::Idle),
        );
        *w.harnesses.0.stop.lock().expect("lock") = Stopping::Fails;
        let r = all_of(&w, Policy::Ask, Terminal::Absent, false);
        assert_eq!(
            r.result.expect_err("one failed").to_string(),
            "1 agent(s) not restarted"
        );
        assert!(r.err.contains("ghost"), "{}", r.err);
        assert_eq!(r.handed, [session("me")], "own is still handed over");
    }

    #[test]
    fn all_with_only_the_own_session_says_so_and_never_prints_an_empty_summary() {
        let mut w = World::new();
        w.own = Some(session("me"));
        w.agent("me", Doing::Idle);
        let r = all_of(&w, Policy::Ask, Terminal::Attached, true);
        assert!(r.result.is_ok());
        assert_eq!(r.asked, ["restart 1 · this session last? [y/N] "]);
        assert!(
            r.out
                .starts_with("agent restart: restart 1 · this session last"),
            "{}",
            r.out
        );
        assert_eq!(r.handed, [session("me")]);

        w.agent("other", Doing::Idle);
        let r = all_of(&w, Policy::Ask, Terminal::Attached, false);
        assert_eq!(r.asked, ["restart 2 · this session last? [y/N] "]);
        assert_eq!(r.handed.len(), 0, "declined");
    }

    #[test]
    fn the_own_session_is_asked_about_at_a_terminal_when_busy_and_never_without_one() {
        let mut w = World::new();
        w.own = Some(session("me"));
        w.agent("me", Doing::Working);
        let declined = one_of(&w, "me", OnBusy::Ask, Terminal::Attached, false);
        assert_eq!(declined.asked, ["me is working. Restart it? [y/N] "]);
        assert!(declined.result.is_err());
        assert_eq!(declined.handed.len(), 0);
        let yes = one_of(&w, "me", OnBusy::Ask, Terminal::Attached, true);
        assert_eq!(yes.handed, [session("me")]);
        let forced = one_of(&w, "me", OnBusy::Force, Terminal::Attached, false);
        assert_eq!((forced.asked.len(), forced.handed.len()), (0, 1));
        let unattended = one_of(&w, "me", OnBusy::Ask, Terminal::Absent, false);
        assert_eq!((unattended.asked.len(), unattended.handed.len()), (0, 1));
    }

    #[test]
    fn all_with_idle_leaves_the_own_session_alone_when_it_is_busy() {
        let mut w = World::new();
        w.own = Some(session("me"));
        w.agent("me", Doing::Working);
        w.agent("other", Doing::Idle);
        let r = all_of(&w, Policy::IdleOnly, Terminal::Absent, false);
        assert!(r.result.is_ok());
        assert_eq!(r.handed.len(), 0);
        assert!(r.out.contains("skipped me: working"), "{}", r.out);

        let idle_own = World::new();
        let mut idle_own = idle_own;
        idle_own.own = Some(session("me"));
        idle_own.agent("me", Doing::Idle);
        let r = all_of(&idle_own, Policy::IdleOnly, Terminal::Absent, false);
        assert_eq!(r.handed, [session("me")]);
    }

    #[test]
    fn the_arguments_become_one_target() {
        let name = Some(session("fix"));
        assert_eq!(
            Target::from_args(name.clone(), false, false, false),
            Target::One(name.clone(), OnBusy::Ask)
        );
        assert_eq!(
            Target::from_args(name.clone(), true, false, false),
            Target::One(name, OnBusy::Force)
        );
        assert_eq!(
            Target::from_args(None, false, true, false),
            Target::All(Policy::Ask)
        );
        assert_eq!(
            Target::from_args(None, true, true, false),
            Target::All(Policy::Force)
        );
        assert_eq!(
            Target::from_args(None, false, true, true),
            Target::All(Policy::IdleOnly)
        );
    }

    #[test]
    fn restart_flags_are_parsed_so_idle_needs_all_and_excludes_force() {
        use clap::Parser;
        let parse = |args: &[&str]| {
            crate::Cli::try_parse_from(["agent", "restart"].iter().chain(args)).is_ok()
        };
        assert!(parse(&["fix"]));
        assert!(parse(&["fix", "--force"]));
        assert!(parse(&["--all", "--idle"]));
        assert!(parse(&["--all", "--force"]));
        assert!(!parse(&["--idle"]));
        assert!(!parse(&["--all", "--idle", "--force"]));
        assert!(!parse(&["fix", "--all"]));
    }
}
