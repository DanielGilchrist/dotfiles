use std::time::{Duration, SystemTime};

use super::{Error, Own, Planned, Result, Services, find_session, snapshot, start_then};
use crate::activity::{Activity, Busy};
use crate::model::{Presence, Running, Startable};
use crate::names::SessionName;
use crate::ports::{HarnessId, Lookup, SessionStatus, SurvivorState};

/// How long to wait for something that settles on its own, as polls `every`.
#[derive(Debug, Clone, Copy)]
pub struct Patience {
    pub polls: u32,
    pub every: Duration,
}

impl Patience {
    /// The session host lists a session as gone or running a moment after
    /// it was killed or started, and a harness takes a moment to notice its
    /// terminal went and to register the process as one it can stop.
    pub const SETTLE: Self = Self {
        polls: 60,
        every: Duration::from_millis(50),
    };

    /// Polls `done` until it says yes, which is whether it did within the
    /// time.
    fn wait(self, mut done: impl FnMut() -> Result<bool>) -> Result<bool> {
        for poll in 0..=self.polls {
            if done()? {
                return Ok(true);
            }
            if poll < self.polls {
                std::thread::sleep(self.every);
            }
        }
        Ok(false)
    }
}

/// What a restart does with an agent whose session already exited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnExited {
    /// Start it again, like attaching to it.
    Start,
    /// Only stop the harness process it left running, and keep it exited.
    Leave,
}

/// Where a restart leaves the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
    Started,
    LeftExited,
}

impl End {
    /// What happened to the agent `name`, run by a harness called `label`.
    pub fn says(self, name: &SessionName, label: &str) -> String {
        match self {
            Self::Started => format!("restarted {name}"),
            Self::LeftExited => format!("stopped {label} for {name} · stays exited"),
        }
    }
}

/// What a restart would interrupt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interrupts {
    /// The session's own activity.
    Session(Busy),
    /// A process the harness runs in the background, mid-task.
    Survivor,
}

impl Interrupts {
    pub fn label(self) -> &'static str {
        match self {
            Self::Session(busy) => busy.label(),
            Self::Survivor => "working in the background",
        }
    }

    /// What an agent approved as idle did since.
    pub fn became(self) -> String {
        match self {
            Self::Session(busy) => busy.became(),
            Self::Survivor => "started working in the background".to_string(),
        }
    }
}

/// What restarting an agent would do, as seen when it was planned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestartPlan {
    at: Startable,
    harness: HarnessId,
    label: &'static str,
    was: Was,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Was {
    Live {
        /// For killing it first.
        running: Running,
        interrupts: Option<Interrupts>,
    },
    /// Exited or never started, with the process its harness left running.
    Down {
        survivor: Option<SurvivorState>,
        then: OnExited,
    },
}

/// What is known of restarting the caller's own session, which can't be
/// done from inside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnRestart {
    interrupts: Option<Interrupts>,
}

impl OwnRestart {
    pub fn interrupts(&self) -> Option<Interrupts> {
        self.interrupts
    }
}

impl RestartPlan {
    pub fn name(&self) -> SessionName {
        self.at.name()
    }

    pub fn interrupts(&self) -> Option<Interrupts> {
        match &self.was {
            Was::Live { interrupts, .. } => *interrupts,
            Was::Down {
                survivor: Some(SurvivorState::Busy),
                ..
            } => Some(Interrupts::Survivor),
            Was::Down { .. } => None,
        }
    }

    pub fn end(&self) -> End {
        match &self.was {
            Was::Live { .. }
            | Was::Down {
                then: OnExited::Start,
                ..
            } => End::Started,
            Was::Down {
                then: OnExited::Leave,
                ..
            } => End::LeftExited,
        }
    }

    pub fn label(&self) -> &'static str {
        self.label
    }

    /// Approval without interrupting, refused when it would.
    pub fn approve(self) -> Result<Restart, Interrupting> {
        match self.interrupts() {
            None => Ok(Restart(Box::new(Consented::Idle(self)))),
            Some(interrupts) => Err(Interrupting(Box::new(self), interrupts)),
        }
    }
}

/// A restart that would interrupt something. Only this can be forced.
#[derive(Debug)]
pub struct Interrupting(Box<RestartPlan>, Interrupts);

impl Interrupting {
    pub fn plan(&self) -> &RestartPlan {
        &self.0
    }

    pub fn interrupts(&self) -> Interrupts {
        self.1
    }

    /// Go ahead despite what the user saw it would interrupt.
    pub fn force(self) -> Restart {
        Restart(Box::new(Consented::Interrupting(*self.0)))
    }
}

/// A restart the user has agreed to. Only [`RestartPlan::approve`] and
/// [`Interrupting::force`] make one.
#[derive(Debug)]
pub struct Restart(Box<Consented>);

#[derive(Debug)]
enum Consented {
    /// Refused at restart time if the agent has become busy since.
    Idle(RestartPlan),
    Interrupting(RestartPlan),
}

impl Restart {
    pub fn plan(&self) -> &RestartPlan {
        match &*self.0 {
            Consented::Idle(plan) | Consented::Interrupting(plan) => plan,
        }
    }

    pub fn name(&self) -> SessionName {
        self.plan().name()
    }
}

#[derive(Debug)]
pub struct Restarted {
    pub end: End,
    pub label: &'static str,
}

/// What the session itself is doing, if the agent is in it.
fn session_interrupts(activity: Option<&Activity>) -> Option<Interrupts> {
    activity
        .and_then(|activity| activity.busy(SystemTime::now()))
        .map(Interrupts::Session)
}

fn survivor_interrupts(state: Option<SurvivorState>) -> Option<Interrupts> {
    (state == Some(SurvivorState::Busy)).then_some(Interrupts::Survivor)
}

fn plan_for(
    svc: Services<'_>,
    at: Startable,
    then: OnExited,
) -> Result<Planned<RestartPlan, OwnRestart>> {
    let name = at.name();
    let record = svc
        .store
        .record(&name)?
        .ok_or_else(|| Error::NoRecord(name.clone()))?;
    let harness = svc.harness(record.harness())?;
    let activity = svc.store.activities()?.remove(&name);
    let lookup = harness.survivor(
        at.cwd(),
        activity.as_ref().and_then(|a| a.transcript.as_ref()),
    )?;
    if matches!(lookup, Lookup::Elsewhere) {
        return Err(Error::OpenElsewhere {
            name,
            label: harness.label(),
        });
    }
    let live = is_live(svc, &name)?;
    let was = if live {
        let interrupts = session_interrupts(activity.as_ref())
            .or_else(|| survivor_interrupts(lookup.stoppable_state()));
        if svc.own == Some(&name) {
            return Ok(Planned::Own(Own::new(name, OwnRestart { interrupts })));
        }
        Was::Live {
            running: Running::new(name),
            interrupts,
        }
    } else {
        Was::Down {
            survivor: lookup.state(),
            then,
        }
    };
    Ok(Planned::Elsewhere(RestartPlan {
        at,
        harness: record.harness().clone(),
        label: harness.label(),
        was,
    }))
}

/// What restarting `at` would do. An agent that already exited is started
/// again. Sessions `agent` didn't start have no record and are refused.
pub fn plan_restart(svc: Services<'_>, at: Startable) -> Result<Planned<RestartPlan, OwnRestart>> {
    plan_for(svc, at, OnExited::Start)
}

/// Stops the harness process a killed session left behind, then starts the
/// agent again. The order is fixed here: what would be interrupted is checked
/// before anything is touched, the session goes first so no client sees its
/// command end, and the process is gone before the launch is marked, so a late
/// report from it can't overwrite the mark.
pub fn restart(svc: Services<'_>, go: Restart) -> Result<Restarted> {
    restart_within(svc, go, Patience::SETTLE)
}

/// [`restart`], waiting `patience` for each thing that settles on its own.
pub fn restart_within(svc: Services<'_>, go: Restart, patience: Patience) -> Result<Restarted> {
    let (plan, idle) = match *go.0 {
        Consented::Idle(plan) => (plan, true),
        Consented::Interrupting(plan) => (plan, false),
    };
    let name = plan.at.name();
    let harness = svc.harness(&plan.harness)?;
    let activity = svc.store.activities()?.remove(&name);
    let transcript = activity.as_ref().and_then(|a| a.transcript.as_ref());
    let looked = || match harness.survivor(plan.at.cwd(), transcript)? {
        Lookup::Elsewhere => Err(Error::OpenElsewhere {
            name: name.clone(),
            label: plan.label,
        }),
        other => Ok(other),
    };
    let before = looked()?;
    if idle {
        let in_session = match plan.was {
            Was::Live { .. } => session_interrupts(activity.as_ref()),
            Was::Down { .. } => None,
        };
        let process = match plan.was {
            Was::Live { .. } => before.stoppable_state(),
            Was::Down { .. } => before.state(),
        };
        if let Some(interrupts) = in_session.or_else(|| survivor_interrupts(process)) {
            return Err(Error::Busy { name, interrupts });
        }
    }
    if let Was::Live { running, .. } = &plan.was
        && is_live(svc, &name)?
    {
        svc.host.kill(running)?;
        if !patience.wait(|| Ok(!is_live(svc, &name)?))? {
            return Err(Error::Lingers(name));
        }
    }
    match clear(looked, patience)? {
        None => {}
        Some(Left::Stopped) => {
            return Err(Error::Survives {
                name,
                label: plan.label,
            });
        }
        Some(Left::NeverStoppable) => {
            return Err(Error::StillInTerminal {
                name,
                label: plan.label,
            });
        }
    }
    let end = plan.end();
    match end {
        End::Started => {
            start_then(svc, &plan.at, harness, None, || {
                svc.host.start_detached(&name)?;
                if patience.wait(|| is_live(svc, &name))? {
                    Ok(())
                } else {
                    Err(Error::NotStarted(name.clone()))
                }
            })?;
        }
        End::LeftExited => {}
    }
    Ok(Restarted {
        end,
        label: plan.label,
    })
}

fn is_live(svc: Services<'_>, name: &SessionName) -> Result<bool> {
    Ok(matches!(
        find_session(svc, name)?.map(|s| s.status),
        Some(SessionStatus::Live { .. })
    ))
}

/// What was left of a process after waiting for it to be gone.
enum Left {
    /// It was stopped and is still there.
    Stopped,
    /// It was alive the whole time but never became something to stop.
    NeverStoppable,
}

/// Stops what `lookup` finds, once, and waits for it to be gone. A process
/// that isn't stoppable yet is waited for. It is looked for again after the
/// stop because the harness's stop can report success and leave a process,
/// and a second worker of the same conversation can be behind the first.
/// What is left, if anything, is the answer.
fn clear(mut lookup: impl FnMut() -> Result<Lookup>, patience: Patience) -> Result<Option<Left>> {
    let mut left = Left::NeverStoppable;
    let gone = patience.wait(|| match lookup()? {
        Lookup::Gone => Ok(true),
        Lookup::Survivor(survivor) if matches!(left, Left::NeverStoppable) => {
            left = Left::Stopped;
            survivor.stop()?;
            Ok(false)
        }
        Lookup::Survivor(_) | Lookup::NotYet(_) | Lookup::Elsewhere => Ok(false),
    })?;
    Ok((!gone).then_some(left))
}

/// Whether a fleet restart may interrupt the agents it found busy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interrupt {
    Skip,
    Allow,
}

/// Every agent `agent` runs, as [`plan_restarts`] found them.
#[derive(Debug)]
pub struct Fleet {
    idle: Vec<Restart>,
    busy: Vec<Interrupting>,
    own: Option<Own<OwnRestart>>,
    unplanned: Vec<(SessionName, Error)>,
}

/// What a fleet approval decided.
#[derive(Debug)]
pub struct Approvals {
    pub restarts: Vec<Restart>,
    pub skipped: Vec<Interrupting>,
}

impl Fleet {
    pub fn idle(&self) -> &[Restart] {
        &self.idle
    }

    pub fn busy(&self) -> &[Interrupting] {
        &self.busy
    }

    /// The caller's own session, which a fleet restart leaves alone.
    pub fn own(&self) -> Option<&Own<OwnRestart>> {
        self.own.as_ref()
    }

    pub fn unplanned(&self) -> &[(SessionName, Error)] {
        &self.unplanned
    }

    pub fn is_empty(&self) -> bool {
        self.idle.is_empty() && self.busy.is_empty()
    }

    fn plans(&self) -> impl Iterator<Item = &RestartPlan> {
        self.idle
            .iter()
            .map(Restart::plan)
            .chain(self.busy.iter().map(Interrupting::plan))
    }

    pub fn starts(&self) -> usize {
        self.total() - self.stops()
    }

    pub fn stops(&self) -> usize {
        self.plans()
            .filter(|plan| plan.end() == End::LeftExited)
            .count()
    }

    pub fn total(&self) -> usize {
        self.idle.len() + self.busy.len()
    }

    /// What it would do, as `restart 5 · stop 2 exited · 3 busy`, each part
    /// left out when it is zero.
    pub fn summary(&self) -> String {
        self.summary_counting(0)
    }

    /// [`summary`](Self::summary) with `more` extra restarts, as when the
    /// caller's own session is restarted too.
    pub fn summary_counting(&self, more: usize) -> String {
        let counted = [
            (self.starts() + more, "restart {}"),
            (self.stops(), "stop {} exited"),
            (self.busy.len(), "{} busy"),
        ];
        counted
            .iter()
            .filter(|(n, _)| *n > 0)
            .map(|(n, form)| form.replace("{}", &n.to_string()))
            .collect::<Vec<_>>()
            .join(" · ")
    }

    /// [`summary`](Self::summary) as a title.
    pub fn title(&self) -> String {
        let summary = self.summary();
        let mut chars = summary.chars();
        chars
            .next()
            .map(|first| first.to_uppercase().chain(chars).collect())
            .unwrap_or_default()
    }

    pub fn approve(self, interrupt: Interrupt) -> Approvals {
        let mut restarts = self.idle;
        let mut skipped = Vec::new();
        for busy in self.busy {
            match interrupt {
                Interrupt::Allow => restarts.push(busy.force()),
                Interrupt::Skip => skipped.push(busy),
            }
        }
        Approvals { restarts, skipped }
    }
}

/// Every agent with a session, or an exited one whose harness left a process
/// running. Agents `agent` didn't start have no record and aren't listed.
pub fn plan_restarts(svc: Services<'_>) -> Result<Fleet> {
    let (found, _) = snapshot(svc)?;
    let mut fleet = Fleet {
        idle: Vec::new(),
        busy: Vec::new(),
        own: None,
        unplanned: Vec::new(),
    };
    for (_, entry) in found.entries() {
        let (Ok(at), Some(presence)) = (entry.startable(), entry.presence()) else {
            continue;
        };
        let then = match presence {
            Presence::Live => OnExited::Start,
            Presence::Exited => OnExited::Leave,
        };
        match plan_for(svc, at, then) {
            Ok(Planned::Own(own)) => fleet.own = Some(own),
            Ok(Planned::Elsewhere(RestartPlan {
                was:
                    Was::Down {
                        survivor: None,
                        then: OnExited::Leave,
                    },
                ..
            }))
            | Err(Error::NoRecord(_)) => {}
            Ok(Planned::Elsewhere(plan)) => match plan.approve() {
                Ok(restart) => fleet.idle.push(restart),
                Err(interrupting) => fleet.busy.push(interrupting),
            },
            Err(e) => fleet.unplanned.push((entry.name().clone(), e)),
        }
    }
    Ok(fleet)
}
