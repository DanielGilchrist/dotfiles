use super::*;
use crate::fixtures::elsewhere_on;
use crate::fixtures::sighted;
use crate::model::{Intent, Need, Windows, elsewhere_banner, elsewhere_note};
use agent_core::ports::Place;

const SHARED: &str = "obs-layout-4";

fn held_elsewhere_model() -> Model {
    let mut m = elsewhere_on(SHARED);
    set_state(&mut m, SHARED, done(), 1);
    m
}

fn nav() -> Msg {
    Msg::Key(KeyEvent::new(KeyCode::Char('\\'), KeyModifiers::CONTROL))
}

fn previewed(m: &mut Model) -> Vec<Effect> {
    m.schedule_preview();
    let due = later(m, crate::model::PREVIEW_DEBOUNCE);
    tick_at(m, due)
}

fn closed_windows_of(effects: &[Effect]) -> Vec<String> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Job(Job::CloseElsewhere { name, .. }) => Some(name.to_string()),
            _ => None,
        })
        .collect()
}

fn closes_of(work: &[Background]) -> Vec<String> {
    work.iter()
        .filter_map(|w| match w {
            Background::Job(Job::CloseElsewhere { name, .. }) => Some(name.to_string()),
            _ => None,
        })
        .collect()
}

fn looks_of(work: &[Background]) -> usize {
    work.iter()
        .filter(|w| matches!(w, Background::Look(_)))
        .count()
}

fn looks_asked(effects: &[Effect]) -> usize {
    effects
        .iter()
        .filter(|e| matches!(e, Effect::CheckElsewhere(_)))
        .count()
}

fn attaches(effects: &[Effect]) -> Vec<Intent> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Attach { intent, .. } => Some(*intent),
            _ => None,
        })
        .collect()
}

fn stays_held_elsewhere(m: &Model, effects: &[Effect], agent: &str) {
    assert_eq!(attaches(effects), []);
    assert_eq!(closed_windows_of(effects), Vec::<String>::new());
    assert_eq!(m.pane.held_elsewhere(), Some(&name(agent)));
    assert!(m.pane.connection().is_none());
    assert_eq!(
        m.cursor().and_then(Cursor::entry_name),
        Some(&name(agent)),
        "the banner is under the row it is about"
    );
}

#[test]
fn a_hovered_agent_is_checked_for_other_windows_before_anything_attaches() {
    let mut m = crate::fixtures::fleet(10);
    m.set_cursor(Cursor::Entry(name(SHARED)));
    let effects = previewed(&mut m);
    assert_eq!(checked_sessions(&m, &effects), [SHARED]);
    assert_eq!(attaches(&effects), []);
    assert!(matches!(m.pane, Pane::Checking { .. }));
}

#[test]
fn the_preview_attaches_once_the_check_finds_no_other_window() {
    let mut m = crate::fixtures::fleet(10);
    m.set_cursor(Cursor::Entry(name(SHARED)));
    previewed(&mut m);
    let effects = crate::fixtures::sighted(&mut m, &[]);
    assert_eq!(attaches(&effects), [Intent::Preview]);
}

#[test]
fn a_preview_of_an_agent_open_elsewhere_leaves_it_alone_instead_of_attaching() {
    let mut m = crate::fixtures::fleet(10);
    m.set_cursor(Cursor::Entry(name(SHARED)));
    previewed(&mut m);
    let effects = crate::fixtures::sighted(&mut m, &[(SHARED, crate::fixtures::in_nvim())]);
    stays_held_elsewhere(&m, &effects, SHARED);
    assert_eq!(m.moves_here().map(|(n, _)| n), Some(&name(SHARED)));
}

#[test]
fn the_pane_attaches_again_once_the_other_window_is_gone() {
    let mut m = held_elsewhere_model();
    assert!(recheck_elsewhere(&mut m).is_some());
    let still = crate::fixtures::sighted(&mut m, &[(SHARED, crate::fixtures::in_nvim())]);
    stays_held_elsewhere(&m, &still, SHARED);
    recheck_elsewhere(&mut m);
    let effects = crate::fixtures::sighted(&mut m, &[]);
    assert_eq!(attaches(&effects), [Intent::Preview]);
    assert_eq!(closed_windows_of(&effects), Vec::<String>::new());
}

#[test]
fn an_answer_that_finds_windows_only_beside_other_agents_leaves_the_held_one_alone_to_attach() {
    let mut m = held_elsewhere_model();
    recheck_elsewhere(&mut m);
    let effects =
        crate::fixtures::sighted(&mut m, &[("auth-refresh-2", crate::fixtures::in_nvim())]);
    assert_eq!(attaches(&effects), [Intent::Preview]);
    assert_eq!(
        *m.company(&name("auth-refresh-2")),
        crate::fixtures::in_nvim()
    );
}

#[test]
fn an_answer_listing_the_held_agent_beside_another_keeps_it_held() {
    let mut m = held_elsewhere_model();
    recheck_elsewhere(&mut m);
    let ticket = m.current_look().expect("asked");
    let found = HashMap::from([
        (name(SHARED), crate::fixtures::in_nvim()),
        (name("auth-refresh-2"), crate::fixtures::in_nvim()),
    ]);
    let effects = update(
        &mut m,
        Msg::Elsewhere {
            ticket,
            found: Ok(found),
        },
    );
    stays_held_elsewhere(&m, &effects, SHARED);
}

#[test]
fn an_answer_to_an_older_look_is_dropped() {
    let mut m = held_elsewhere_model();
    recheck_elsewhere(&mut m);
    let old = m.current_look().expect("asked");
    recheck_elsewhere(&mut m);
    let effects = update(
        &mut m,
        Msg::Elsewhere {
            ticket: old,
            found: Ok(HashMap::new()),
        },
    );
    stays_held_elsewhere(&m, &effects, SHARED);
}

#[test]
fn esc_goes_nowhere_on_an_agent_held_elsewhere() {
    let mut m = held_elsewhere_model();
    let effects = update(&mut m, key(KeyCode::Esc));
    stays_held_elsewhere(&m, &effects, SHARED);
}

#[test]
fn the_switcher_never_moves_an_agent_held_elsewhere() {
    let mut m = held_elsewhere_model();
    update(&mut m, key(KeyCode::Char(' ')));
    type_text(&mut m, SHARED);
    let effects = update(&mut m, key(KeyCode::Enter));
    stays_held_elsewhere(&m, &effects, SHARED);
    assert!(
        m.status
            .as_ref()
            .is_some_and(|s| s.text.contains("in nvim"))
    );
}

#[test]
fn the_switcher_on_an_agent_found_elsewhere_selects_it_and_leaves_it_alone() {
    let mut m = crate::fixtures::fleet(10);
    m.set_cursor(Cursor::Entry(name("auth-refresh-2")));
    m.pane = Pane::Empty;
    update(&mut m, key(KeyCode::Char(' ')));
    type_text(&mut m, SHARED);
    let asked = update(&mut m, key(KeyCode::Enter));
    assert_eq!(checked_sessions(&m, &asked), [SHARED]);
    let effects = crate::fixtures::sighted(&mut m, &[(SHARED, crate::fixtures::in_nvim())]);
    stays_held_elsewhere(&m, &effects, SHARED);
}

#[test]
fn the_a_key_on_an_agent_found_elsewhere_selects_it_and_leaves_it_alone() {
    let mut m = crate::fixtures::fleet(10);
    let urgent = m.needs_you()[0].clone();
    let other = m
        .snapshot()
        .expect("loaded")
        .entries()
        .map(|(_, e)| e.name().clone())
        .find(|n| *n != urgent && m.attachable(n))
        .expect("another live agent");
    m.set_cursor(Cursor::Entry(other));
    m.pane = Pane::Empty;
    let asked = update(&mut m, key(KeyCode::Char('a')));
    assert_eq!(checked_sessions(&m, &asked), [urgent.as_str()]);
    let effects =
        crate::fixtures::sighted(&mut m, &[(urgent.as_str(), crate::fixtures::in_nvim())]);
    stays_held_elsewhere(&m, &effects, urgent.as_str());
}

#[test]
fn the_a_key_never_moves_an_agent_held_elsewhere() {
    let mut m = held_elsewhere_model();
    let urgent = m.needs_you()[0].clone();
    m.set_cursor(Cursor::Entry(urgent.clone()));
    m.pane = Pane::Elsewhere {
        name: urgent.clone(),
    };
    m.record_company(&urgent, crate::fixtures::in_nvim());
    let effects = update(&mut m, key(KeyCode::Char('a')));
    stays_held_elsewhere(&m, &effects, urgent.as_str());
}

#[test]
fn the_nav_key_twice_toward_an_agent_open_elsewhere_lands_in_nav_and_leaves_it_to_its_other_window()
{
    let mut m = crate::fixtures::fleet(10);
    crate::fixtures::connect(&mut m, "auth-refresh-2", Mode::Nav);
    m.touch_mru(&name(SHARED));
    m.touch_mru(&name("auth-refresh-2"));
    m.chord = Chord::Armed;
    let effects = update(&mut m, nav());
    assert_eq!(checked_sessions(&m, &effects), [SHARED]);
    let effects = crate::fixtures::sighted(&mut m, &[(SHARED, crate::fixtures::in_nvim())]);
    stays_held_elsewhere(&m, &effects, SHARED);
}

#[test]
fn i_from_another_panel_never_moves_an_agent_held_elsewhere() {
    let mut m = held_elsewhere_model();
    update(&mut m, key(KeyCode::Char('2')));
    assert!(matches!(m.panel, Panel::Agent { .. }));
    let effects = update(&mut m, key(KeyCode::Char('i')));
    stays_held_elsewhere(&m, &effects, SHARED);
}

#[test]
fn enter_zero_and_i_on_the_banner_row_attach_first_and_close_nothing_until_connected() {
    for code in [KeyCode::Enter, KeyCode::Char('0'), KeyCode::Char('i')] {
        let mut m = held_elsewhere_model();
        let effects = update(&mut m, key(code));
        assert_eq!(attaches(&effects), [Intent::MoveHere], "{code:?}");
        assert_eq!(closed_windows_of(&effects), Vec::<String>::new());
        assert!(matches!(m.pane, Pane::Connecting { .. }));
    }
}

#[test]
fn the_other_window_closes_only_after_the_connection_exists() {
    let mut m = held_elsewhere_model();
    update(&mut m, key(KeyCode::Enter));
    assert!(m.pane.connection().is_none(), "a failed attach ends here");
    crate::fixtures::connect(&mut m, SHARED, Mode::Nav);
    let effects = connected(&mut m, Intent::MoveHere);
    assert_eq!(closes_of(&effects), [SHARED]);
    assert!(matches!(m.focus(), Focus::Agent(_)));
}

#[test]
fn a_closed_other_window_leaves_the_session_alone_and_a_failed_close_leaves_what_was_known() {
    let mut m = elsewhere_on(SHARED);
    update(&mut m, key(KeyCode::Enter));
    crate::fixtures::connect(&mut m, SHARED, Mode::Nav);
    connected(&mut m, Intent::MoveHere);
    let company = |m: &Model| m.company(&name(SHARED)).clone();
    let closed = |m: &mut Model, closed| {
        update(
            m,
            Msg::ClosedElsewhere {
                name: name(SHARED),
                closed,
            },
        )
    };
    m.record_company(&name(SHARED), crate::fixtures::in_nvim());
    let effects = closed(&mut m, Err("nope".into()));
    assert_eq!(company(&m), crate::fixtures::in_nvim());
    assert_eq!(looks_asked(&effects), 1);
    let in_flight = m.current_look();
    let effects = closed(&mut m, Ok(1));
    assert_eq!(company(&m), Company::Alone);
    assert_eq!(looks_asked(&effects), 1, "and it looks again");
    assert_ne!(m.current_look(), in_flight);
    assert!(
        m.status
            .as_ref()
            .is_some_and(|s| s.text.contains("closed 1"))
    );
}

#[test]
fn a_look_from_before_a_close_cannot_bring_the_closed_window_back() {
    let mut m = elsewhere_on(SHARED);
    update(&mut m, key(KeyCode::Enter));
    crate::fixtures::connect(&mut m, SHARED, Mode::Nav);
    connected(&mut m, Intent::MoveHere);
    recheck_elsewhere(&mut m);
    let before = m.current_look().expect("asked");
    update(
        &mut m,
        Msg::ClosedElsewhere {
            name: name(SHARED),
            closed: Ok(1),
        },
    );
    update(
        &mut m,
        Msg::Elsewhere {
            ticket: before,
            found: Ok(HashMap::from([(name(SHARED), crate::fixtures::in_nvim())])),
        },
    );
    assert_eq!(*m.company(&name(SHARED)), Company::Alone);
}

#[test]
fn a_close_while_the_pane_checks_hands_the_check_to_the_newer_look() {
    let mut m = crate::fixtures::fleet(10);
    m.set_cursor(Cursor::Entry(name(SHARED)));
    previewed(&mut m);
    assert!(matches!(m.pane, Pane::Checking { .. }));
    let first = m.current_look().expect("asked");
    let effects = update(
        &mut m,
        Msg::ClosedElsewhere {
            name: name(SHARED),
            closed: Ok(1),
        },
    );
    assert_eq!(looks_asked(&effects), 1);
    assert_ne!(m.current_look(), Some(first));
    let stale = update(
        &mut m,
        Msg::Elsewhere {
            ticket: first,
            found: Ok(HashMap::new()),
        },
    );
    assert_eq!(attaches(&stale), []);
    let effects = crate::fixtures::sighted(&mut m, &[]);
    assert_eq!(attaches(&effects), [Intent::Preview]);
}

#[test]
fn a_move_here_on_a_session_that_exited_resurrects_it_instead_of_moving() {
    let mut m = crate::fixtures::fleet(10);
    let exited = m
        .snapshot()
        .expect("loaded")
        .entries()
        .find(|(_, e)| {
            matches!(
                m.reach(e),
                Reach::Other {
                    enter: Enter::Resurrect | Enter::Restart(_),
                    ..
                }
            )
        })
        .map(|(_, e)| e.name().clone())
        .expect("an exited agent");
    m.set_cursor(Cursor::Entry(exited.clone()));
    m.pane = Pane::Elsewhere {
        name: exited.clone(),
    };
    let effects = update(&mut m, key(KeyCode::Enter));
    assert!(
        attaches(&effects)
            .iter()
            .all(|intent| *intent != Intent::MoveHere)
    );
    assert!(m.pane.held_elsewhere().is_none());
}

#[test]
fn moving_here_notes_the_session_alone_once_connected_and_drops_looks_still_on_their_way() {
    let mut m = elsewhere_on(SHARED);
    update(&mut m, key(KeyCode::Enter));
    assert!(
        m.company(&name(SHARED)).is_elsewhere(),
        "still so while attaching"
    );
    let before = m.start_look();
    crate::fixtures::connect(&mut m, SHARED, Mode::Nav);
    connected(&mut m, Intent::MoveHere);
    assert_eq!(*m.company(&name(SHARED)), Company::Alone);
    assert_ne!(m.current_look(), Some(before));
    update(
        &mut m,
        Msg::Elsewhere {
            ticket: before,
            found: Ok(HashMap::from([(name(SHARED), crate::fixtures::in_nvim())])),
        },
    );
    assert_eq!(*m.company(&name(SHARED)), Company::Alone, "no flash back");
}

#[test]
fn an_agent_that_leaves_the_snapshot_while_paused_empties_the_pane() {
    let mut m = held_elsewhere_model();
    update(
        &mut m,
        Msg::Snapshot(Ok((snapshot(Vec::new(), &[]), Vec::new()))),
    );
    assert!(m.pane.held_elsewhere().is_none());
    assert!(matches!(m.pane, Pane::Empty));
}

#[test]
fn a_failed_look_on_a_paused_pane_whose_company_is_alone_reattaches() {
    let mut m = held_elsewhere_model();
    m.record_company(&name(SHARED), Company::Alone);
    recheck_elsewhere(&mut m);
    let ticket = m.current_look().expect("asked");
    let effects = update(
        &mut m,
        Msg::Elsewhere {
            ticket,
            found: Err("ps failed".into()),
        },
    );
    assert_eq!(attaches(&effects), [Intent::Preview]);
}

#[test]
fn a_plain_visit_connects_without_closing_anything() {
    let mut m = crate::fixtures::fleet(10);
    crate::fixtures::connect(&mut m, SHARED, Mode::Nav);
    let effects = connected(&mut m, Intent::Activate);
    assert_eq!(closes_of(&effects), Vec::<String>::new());
    assert_eq!(looks_of(&effects), 1, "and watches for newcomers");
}

#[test]
fn a_window_that_opens_while_typing_is_left_alone_until_the_keys_leave() {
    let mut m = crate::fixtures::fleet(10);
    m.set_cursor(Cursor::Entry(name(SHARED)));
    crate::fixtures::connect(&mut m, SHARED, Mode::Nav);
    update(&mut m, key(KeyCode::Enter));
    assert!(matches!(m.focus(), Focus::Agent(_)));
    assert!(recheck_elsewhere(&mut m).is_some());
    crate::fixtures::sighted(&mut m, &[(SHARED, crate::fixtures::in_nvim())]);
    assert!(matches!(m.focus(), Focus::Agent(_)), "still typing");
    assert!(m.squished(&name(SHARED)).is_some());
    update(&mut m, nav());
    assert_eq!(
        m.pane.held_elsewhere(),
        Some(&name(SHARED)),
        "its client is dropped"
    );
}

#[test]
fn a_window_that_opens_beside_a_preview_leaves_it_alone() {
    let mut m = crate::fixtures::fleet(10);
    m.set_cursor(Cursor::Entry(name(SHARED)));
    crate::fixtures::connect(&mut m, SHARED, Mode::Nav);
    recheck_elsewhere(&mut m);
    crate::fixtures::sighted(&mut m, &[(SHARED, crate::fixtures::in_nvim())]);
    assert_eq!(m.pane.held_elsewhere(), Some(&name(SHARED)));
}

#[test]
fn peeking_an_agent_open_elsewhere_holds_a_client_only_while_the_float_is_open() {
    let mut m = held_elsewhere_model();
    let effects = update(&mut m, key(KeyCode::Char('v')));
    assert_eq!(attaches(&effects), [Intent::Preview]);
    assert_eq!(m.peek, Some(name(SHARED)));
    crate::fixtures::connect(&mut m, SHARED, Mode::Nav);
    assert!(m.pane.connection().is_some(), "the float has its client");
    update(&mut m, key(KeyCode::Char('v')));
    assert_eq!(
        m.pane.held_elsewhere(),
        Some(&name(SHARED)),
        "detached with the float"
    );
}

#[test]
fn enter_in_a_peek_before_any_answer_never_enters_agent_on_an_agent_open_elsewhere() {
    for code in [KeyCode::Enter, KeyCode::Char('i')] {
        let mut m = held_elsewhere_model();
        let peeked = update(&mut m, key(KeyCode::Char('v')));
        assert_eq!(attaches(&peeked), [Intent::Preview]);
        crate::fixtures::connect(&mut m, SHARED, Mode::Nav);
        let effects = update(&mut m, key(code));
        stays_held_elsewhere(&m, &effects, SHARED);
        assert!(!m.peeking(), "{code:?}");
    }
}

#[test]
fn s_sends_to_an_agent_left_alone_without_a_client() {
    let mut m = held_elsewhere_model();
    update(&mut m, key(KeyCode::Char('s')));
    assert!(matches!(m.overlay, Overlay::Talk { .. }));
    type_text(&mut m, "hi");
    let effects = update(&mut m, key(KeyCode::Enter));
    assert_eq!(replies(&effects), ["hi"]);
    assert!(m.pane.connection().is_none());
}

#[test]
fn the_footer_says_move_here_on_the_banner_row_and_attach_otherwise() {
    let enter = |m: &Model| {
        crate::keymap::footer(&Situation::of(m), "⌃\\")
            .into_iter()
            .find(|h| h.keys == "⏎")
            .map(|h| h.label)
    };
    let mut m = held_elsewhere_model();
    assert_eq!(enter(&m), Some("move here"));
    m.pane = Pane::Empty;
    assert_eq!(enter(&m), Some("attach"));
}

#[test]
fn nothing_is_asked_while_checking_or_connecting() {
    let mut m = crate::fixtures::fleet(10);
    m.set_cursor(Cursor::Entry(name(SHARED)));
    previewed(&mut m);
    assert!(recheck_elsewhere(&mut m).is_none());
}

#[test]
fn a_failed_look_keeps_the_pane_and_says_so_once() {
    let mut m = held_elsewhere_model();
    let fail = |m: &mut Model| {
        recheck_elsewhere(m);
        let ticket = m.current_look().expect("asked");
        update(
            m,
            Msg::Elsewhere {
                ticket,
                found: Err("ps failed".into()),
            },
        )
    };
    let effects = fail(&mut m);
    stays_held_elsewhere(&m, &effects, SHARED);
    assert!(
        m.status
            .as_ref()
            .is_some_and(|s| s.kind == StatusKind::Error && s.text.contains("ps failed"))
    );
    m.status = None;
    fail(&mut m);
    assert!(m.status.is_none(), "not again");
    recheck_elsewhere(&mut m);
    crate::fixtures::sighted(&mut m, &[(SHARED, crate::fixtures::in_nvim())]);
    assert_eq!(m.check_health, crate::model::CheckHealth::Working);
}

#[test]
fn a_first_look_that_fails_attaches_anyway_and_says_so() {
    let mut m = crate::fixtures::fleet(10);
    m.set_cursor(Cursor::Entry(name(SHARED)));
    previewed(&mut m);
    let ticket = m.current_look().expect("asked");
    let effects = update(
        &mut m,
        Msg::Elsewhere {
            ticket,
            found: Err("ps failed".into()),
        },
    );
    assert_eq!(attaches(&effects), [Intent::Preview]);
    assert!(
        m.status
            .as_ref()
            .is_some_and(|s| s.kind == StatusKind::Error)
    );
}

#[test]
fn a_look_that_takes_too_long_attaches_without_it_and_says_nothing() {
    let mut m = crate::fixtures::fleet(10);
    m.set_cursor(Cursor::Entry(name(SHARED)));
    previewed(&mut m);
    let early = later(&m, CHECK_TIMEOUT.saturating_sub(Duration::from_millis(1)));
    assert_eq!(attaches(&tick_at(&mut m, early)), []);
    let due = later(&m, Duration::from_millis(2));
    let effects = tick_at(&mut m, due);
    assert_eq!(attaches(&effects), [Intent::Preview]);
    assert!(m.status.is_none());
    assert_eq!(m.check_health, crate::model::CheckHealth::Working);
}

fn one(place: Place) -> Company {
    Company::of(vec![place])
}

fn windows_of(company: &Company) -> &Windows {
    company.windows().expect("windows")
}

#[test]
fn an_idle_pane_still_looks_so_every_card_can_be_marked() {
    let mut m = crate::fixtures::fleet(10);
    assert!(recheck_elsewhere(&mut m).is_some());
}

#[test]
fn one_look_marks_every_agent_open_elsewhere_and_clears_those_that_left() {
    let mut m = crate::fixtures::fleet(10);
    recheck_elsewhere(&mut m);
    sighted(
        &mut m,
        &[
            ("rate-limit-1", one(Place::Ssh)),
            (SHARED, one(Place::Tmux)),
        ],
    );
    assert_eq!(*m.company(&name("rate-limit-1")), one(Place::Ssh));
    assert_eq!(*m.company(&name(SHARED)), one(Place::Tmux));
    recheck_elsewhere(&mut m);
    sighted(&mut m, &[(SHARED, one(Place::Tmux))]);
    assert_eq!(*m.company(&name("rate-limit-1")), Company::Alone);
    assert_eq!(*m.company(&name(SHARED)), one(Place::Tmux));
}

#[test]
fn an_agent_nothing_was_seen_beside_is_not_tracked_just_to_be_alone() {
    let mut m = crate::fixtures::fleet(10);
    m.record_company(&name("never-seen"), Company::Alone);
    assert!(m.tracked(&name("never-seen")).is_none());
}

#[test]
fn the_pane_and_the_cache_agree_about_the_connected_agent() {
    let mut m = crate::fixtures::fleet(10);
    crate::fixtures::connect(&mut m, SHARED, Mode::Agent);
    recheck_elsewhere(&mut m);
    sighted(&mut m, &[(SHARED, one(Place::Nvim))]);
    assert!(m.squished(&name(SHARED)).is_some());
    recheck_elsewhere(&mut m);
    sighted(&mut m, &[]);
    assert!(m.squished(&name(SHARED)).is_none());
}

#[test]
fn the_session_the_pane_is_on_is_noted_even_when_the_snapshot_doesnt_list_it() {
    let mut m = crate::fixtures::fleet(10);
    m.pane = Pane::Elsewhere {
        name: name("unlisted-1"),
    };
    recheck_elsewhere(&mut m);
    sighted(&mut m, &[("unlisted-1", one(Place::Ssh))]);
    assert_eq!(*m.company(&name("unlisted-1")), one(Place::Ssh));
}

#[test]
fn an_answer_to_an_older_look_changes_no_card() {
    let mut m = crate::fixtures::fleet(10);
    recheck_elsewhere(&mut m);
    let old = m.current_look().expect("asked");
    recheck_elsewhere(&mut m);
    update(
        &mut m,
        Msg::Elsewhere {
            ticket: old,
            found: Ok(HashMap::from([(name(SHARED), one(Place::Nvim))])),
        },
    );
    assert_eq!(*m.company(&name(SHARED)), Company::Alone);
}

#[test]
fn a_failed_look_leaves_the_cards_as_they_were() {
    let mut m = crate::fixtures::fleet(10);
    recheck_elsewhere(&mut m);
    sighted(&mut m, &[(SHARED, one(Place::Nvim))]);
    recheck_elsewhere(&mut m);
    let ticket = m.current_look().expect("asked");
    update(
        &mut m,
        Msg::Elsewhere {
            ticket,
            found: Err("ps failed".into()),
        },
    );
    assert_eq!(*m.company(&name(SHARED)), one(Place::Nvim));
}

#[test]
fn the_banner_names_the_place_the_other_window_is_in() {
    let banner = |place: Place| elsewhere_banner(windows_of(&one(place)));
    let terminal = |app: &str| Place::Terminal(agent_core::ports::TerminalApp::new(app));
    assert_eq!(banner(Place::Nvim), "⇄ in nvim · ⏎ move here");
    assert_eq!(banner(Place::Ssh), "⇄ over SSH · ⏎ move here");
    assert_eq!(banner(Place::Tmux), "⇄ in tmux · ⏎ move here");
    assert_eq!(banner(terminal("wezterm")), "⇄ in wezterm · ⏎ move here");
    assert_eq!(banner(Place::Detached), "⇄ detached window · ⏎ move here");
    assert_eq!(banner(Place::Unknown), "⇄ elsewhere · ⏎ move here");
}

#[test]
fn windows_in_one_place_say_that_place_and_otherwise_how_many_windows() {
    let note = |places: Vec<Place>| elsewhere_note(windows_of(&Company::of(places)));
    assert_eq!(note(vec![Place::Nvim, Place::Nvim]), "⇄ in nvim");
    assert_eq!(
        note(vec![Place::Detached, Place::Detached]),
        "⇄ detached window"
    );
    assert_eq!(
        note(vec![Place::Detached, Place::Unknown]),
        "⇄ in 2 windows"
    );
    assert_eq!(
        note(vec![Place::Nvim, Place::Ssh, Place::Tmux]),
        "⇄ in 3 windows"
    );
    assert_eq!(
        note(vec![Place::Nvim, Place::Nvim, Place::Ssh]),
        "⇄ in 3 windows"
    );
}

#[test]
fn the_status_line_says_the_same_place_as_the_banner() {
    let mut m = held_elsewhere_model();
    m.record_company(&name(SHARED), one(Place::Ssh));
    stay_elsewhere(&mut m, &name(SHARED));
    let said = m
        .status
        .as_ref()
        .map(|s| s.text.clone())
        .unwrap_or_default();
    assert!(said.contains("⇄ over SSH · ⏎ move here"), "{said}");
}

fn asked_looks(effects: &[Effect]) -> Vec<Need> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::CheckElsewhere(look) => Some(look.need),
            _ => None,
        })
        .collect()
}

#[test]
fn a_check_before_attaching_is_a_decision_and_the_refresh_is_a_refresh() {
    let mut m = crate::fixtures::fleet(10);
    m.set_cursor(Cursor::Entry(name(SHARED)));
    assert_eq!(asked_looks(&previewed(&mut m)), [Need::Decision]);
    let mut idle = crate::fixtures::fleet(10);
    let routine = recheck_elsewhere(&mut idle).expect("asked");
    assert_eq!(routine.need, Need::Refresh);
}

#[test]
fn the_look_after_a_close_is_a_decision() {
    let mut m = elsewhere_on(SHARED);
    let effects = update(
        &mut m,
        Msg::ClosedElsewhere {
            name: name(SHARED),
            closed: Ok(1),
        },
    );
    assert_eq!(asked_looks(&effects), [Need::Decision]);
}
