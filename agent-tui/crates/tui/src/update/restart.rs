use agent_core::names::SessionName;
use agent_core::ops::{Fleet, Interrupt, Restart, RestartPlan};

use super::{Effect, Job, drop_connection_to, waiting_on};
use crate::cause::Cause;
use crate::model::{
    Confirm, Finish, FleetAsk, Model, Overlay, Planning, Reach, RestartAsk, Ticket, Unrestartable,
};
use agent_core::model::Unstartable;

/// `r`: works out whether restarting the selected agent would interrupt it.
pub(super) fn request_restart(m: &mut Model) -> Vec<Effect> {
    let Some(entry) = m.selected_entry().cloned() else {
        return Vec::new();
    };
    let name = entry.name().clone();
    match m.reach(&entry) {
        Reach::Own => m.error(format!("can't restart {name}: this TUI runs in it")),
        Reach::Restarting => m.info(format!("{name} is already restarting")),
        Reach::Other {
            restart: Ok(at), ..
        } => {
            let ticket = m.ticket();
            m.overlay = Overlay::Planning {
                ticket,
                planning: Planning::Restart(name),
            };
            return vec![Effect::Job(Job::PlanRestart { at: *at, ticket })];
        }
        Reach::Other {
            restart: Err(Unrestartable::NoSession),
            ..
        } => m.info(format!("{name} has no session to restart")),
        Reach::Other {
            restart: Err(Unrestartable::Nowhere(Unstartable::NotOurs)),
            ..
        } => m.error(format!("can't restart {name}: agent didn't start it")),
        Reach::Other {
            restart: Err(Unrestartable::Nowhere(Unstartable::Unhosted(why))),
            ..
        } => m.error(format!("can't restart {name}: {why}")),
    }
    Vec::new()
}

/// `R`: works out what restarting every agent would interrupt.
pub(super) fn request_restarts(m: &mut Model) -> Vec<Effect> {
    let ticket = m.ticket();
    m.overlay = Overlay::Planning {
        ticket,
        planning: Planning::Restarts,
    };
    vec![Effect::Job(Job::PlanRestarts { ticket })]
}

pub(super) fn on_restart_planned(
    m: &mut Model,
    ticket: Ticket,
    result: Result<RestartPlan, Cause>,
) -> Vec<Effect> {
    if !waiting_on(m, ticket, |p| matches!(p, Planning::Restart(_))) {
        return Vec::new();
    }
    m.overlay = Overlay::None;
    let plan = match result {
        Ok(plan) => plan,
        Err(e) => {
            m.error(e);
            return Vec::new();
        }
    };
    let windows = m.company(&plan.name()).windows().cloned();
    match (plan.approve(), windows) {
        (Ok(go), None) => begin(m, vec![go]),
        (go, windows) => {
            m.overlay = Overlay::Confirm(Confirm::Restart(Box::new(RestartAsk { go, windows })));
            Vec::new()
        }
    }
}

pub(super) fn on_fleet_planned(
    m: &mut Model,
    ticket: Ticket,
    result: Result<Fleet, Cause>,
) -> Vec<Effect> {
    if !waiting_on(m, ticket, |p| matches!(p, Planning::Restarts)) {
        return Vec::new();
    }
    m.overlay = Overlay::None;
    let fleet = match result {
        Ok(fleet) => fleet,
        Err(e) => {
            m.error(e);
            return Vec::new();
        }
    };
    if fleet.is_empty() {
        match fleet.unplanned().first() {
            Some((name, why)) => m.error(format!("can't restart {name}: {why}")),
            None => m.info("nothing to restart"),
        }
        return Vec::new();
    }
    let elsewhere = fleet
        .idle()
        .iter()
        .map(Restart::name)
        .chain(fleet.busy().iter().map(|b| b.plan().name()))
        .filter(|name| m.company(name).is_elsewhere())
        .count();
    m.overlay = Overlay::Confirm(Confirm::Restarts(Box::new(FleetAsk { fleet, elsewhere })));
    Vec::new()
}

/// What a key does in either restart confirmation, or the confirmation back
/// when the key isn't one it offers.
pub(super) fn on_restart_key(
    m: &mut Model,
    confirm: Confirm,
    key: crossterm::event::KeyCode,
) -> Vec<Effect> {
    use crossterm::event::KeyCode::{Char, Enter};
    match (confirm, key) {
        (Confirm::Restart(ask), Char('y') | Enter) => begin(m, vec![ask.go()]),
        (Confirm::Restarts(ask), Char('y') | Enter) => {
            let approvals = ask.fleet.approve(Interrupt::Allow);
            begin(m, approvals.restarts)
        }
        (Confirm::Restarts(ask), Char('i')) if !ask.fleet.busy().is_empty() => {
            let approvals = ask.fleet.approve(Interrupt::Skip);
            begin(m, approvals.restarts)
        }
        (confirm, _) => {
            m.overlay = Overlay::Confirm(confirm);
            Vec::new()
        }
    }
}

/// Hands approved restarts to the restart thread. Cards read restarting from
/// now until each answers. An agent already restarting isn't handed over
/// twice.
fn begin(m: &mut Model, mut restarts: Vec<Restart>) -> Vec<Effect> {
    restarts.retain(|go| !m.restarting(&go.name()));
    if restarts.is_empty() {
        return Vec::new();
    }
    let names: Vec<SessionName> = restarts.iter().map(Restart::name).collect();
    for name in &names {
        drop_connection_to(m, name);
    }
    m.restarts.get_or_insert_with(Default::default).add(names);
    m.status = None;
    vec![Effect::Restart(restarts)]
}

pub(super) fn on_restarted(m: &mut Model, name: &SessionName, finish: Finish) -> Vec<Effect> {
    let Some(batch) = &mut m.restarts else {
        return Vec::new();
    };
    batch.answer(name, finish);
    if batch.is_done() {
        m.status = Some(batch.summary(m.clock.mono));
        m.restarts = None;
    }
    if m.selected_entry().is_some_and(|e| e.name() == name) {
        m.schedule_preview();
    }
    Vec::new()
}
