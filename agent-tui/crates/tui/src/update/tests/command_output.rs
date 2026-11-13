use super::*;
use crate::fixtures::{fleet, listed, run_commands};
use crate::output::{OUTPUT_REFRESH, OUTPUT_STALLED};
use crate::worker::{OutputRead, OutputRequest};
use agent_core::activity::TaskId;
use agent_core::ports::{CommandOutput, Cut};

const AGENT: &str = "payroll-fix-3";
const FOUR_MINUTES: Duration = Duration::from_secs(240);

fn id(raw: &str) -> TaskId {
    raw.parse().expect("shows")
}

fn on_command(commands: &[&str]) -> Model {
    let mut m = fleet(10);
    let listings: Vec<_> = commands.iter().map(|c| listed(c, "cargo test")).collect();
    run_commands(&mut m, AGENT, &listings, FOUR_MINUTES);
    m.activities
        .as_mut()
        .and_then(|a| a.get_mut(&name(AGENT)))
        .expect("reports")
        .transcript = Some(TranscriptRef::new("/p/s.jsonl"));
    m.set_cursor(Cursor::Entry(name(AGENT)));
    update(&mut m, key(KeyCode::Char('2')));
    m.point_at_row(|_, last| last);
    m
}

fn reads(effects: Vec<Effect>) -> Vec<(String, Ticket, Option<String>)> {
    effects
        .into_iter()
        .filter_map(|effect| match effect {
            Effect::ReadCommandOutput(OutputRequest { id, ticket, at, .. }) => Some((
                id.as_str().to_string(),
                ticket,
                at.map(|at| at.as_str().to_string()),
            )),
            _ => None,
        })
        .collect()
}

fn tail(text: &str) -> CommandOutput {
    CommandOutput::of(text.into(), Cut::Whole)
}

fn answer(
    m: &mut Model,
    who: &str,
    task: &str,
    ticket: Ticket,
    result: Result<CommandOutput, Cause>,
) {
    update(
        m,
        Msg::CommandOutput(OutputRead {
            name: name(who),
            id: id(task),
            ticket,
            result,
        }),
    );
}

fn heard(m: &Model, task: &str) -> Option<String> {
    match &m.command_output(&name(AGENT), &id(task))?.last()?.output {
        CommandOutput::Tail(tail) => Some(tail.text().to_string()),
        other => Some(format!("{other:?}")),
    }
}

fn failure() -> Result<CommandOutput, Cause> {
    Err(Cause::of(&std::io::Error::other("denied")))
}

#[test]
fn the_output_is_asked_for_when_the_command_row_is_shown_with_the_transcript_the_hooks_named() {
    let mut m = on_command(&["c1"]);
    let asked = reads(tick(&mut m));
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].0, "c1");
    assert_eq!(asked[0].2.as_deref(), Some("/p/s.jsonl"));
}

#[test]
fn the_output_is_not_asked_for_while_another_row_is_shown() {
    let mut m = on_command(&["c1"]);
    m.point_at_row(|_, _| 0);
    assert_eq!(reads(tick(&mut m)), Vec::new());
}

#[test]
fn a_request_still_with_the_worker_is_not_repeated_before_it_has_stalled() {
    let mut m = on_command(&["c1"]);
    assert_eq!(reads(tick(&mut m)).len(), 1);
    let before = later(&m, OUTPUT_STALLED.saturating_sub(Duration::from_millis(1)));
    assert_eq!(reads(tick_at(&mut m, before)), Vec::new());
}

#[test]
fn a_stalled_request_is_made_again_and_the_newest_answer_to_either_wins() {
    let mut m = on_command(&["c1"]);
    let first = reads(tick(&mut m))[0].1;
    let stalled = later(&m, OUTPUT_STALLED);
    let second = reads(tick_at(&mut m, stalled));
    assert_eq!(second.len(), 1);
    answer(&mut m, AGENT, "c1", first, Ok(tail("slow\n")));
    assert_eq!(heard(&m, "c1").as_deref(), Some("slow\n"));
    answer(&mut m, AGENT, "c1", second[0].1, Ok(tail("fresh\n")));
    assert_eq!(
        heard(&m, "c1").as_deref(),
        Some("slow\n"),
        "the answer to the other request came after one counted"
    );
}

#[test]
fn reads_that_always_take_longer_than_the_stall_still_end_in_an_answer() {
    let mut m = on_command(&["c1"]);
    let t1 = reads(tick(&mut m))[0].1;
    m.clock = later(&m, OUTPUT_STALLED);
    let moment = m.clock;
    let t2 = reads(tick_at(&mut m, moment))[0].1;
    m.clock = later(&m, Duration::from_secs(5));
    answer(&mut m, AGENT, "c1", t1, Ok(tail("first\n")));
    assert_eq!(heard(&m, "c1").as_deref(), Some("first\n"));
    assert_ne!(t1, t2);
}

#[test]
fn the_first_answer_is_said_to_be_taking_long_only_once_the_wait_has_lasted() {
    let mut m = on_command(&["c1"]);
    reads(tick(&mut m));
    let stalled_at = |m: &Model, at: Moment| {
        m.command_output(&name(AGENT), &id("c1"))
            .expect("asked")
            .stalled(at.mono)
    };
    assert!(!stalled_at(&m, m.clock));
    let stalled = later(&m, OUTPUT_STALLED);
    assert!(stalled_at(&m, stalled));
    reads(tick_at(&mut m, stalled));
    let after = later(&m, OUTPUT_STALLED);
    assert!(
        stalled_at(&m, after),
        "asking again doesn't restart the wait"
    );
}

#[test]
fn the_refresh_is_measured_from_the_answer_and_not_from_the_ask() {
    let mut m = on_command(&["c1"]);
    let ticket = reads(tick(&mut m))[0].1;
    m.clock = later(&m, Duration::from_secs(10));
    answer(&mut m, AGENT, "c1", ticket, Ok(tail("slow\n")));
    let soon = later(&m, OUTPUT_REFRESH.saturating_sub(Duration::from_millis(1)));
    assert_eq!(reads(tick_at(&mut m, soon)), Vec::new());
    let due = later(&m, OUTPUT_REFRESH);
    assert_eq!(reads(tick_at(&mut m, due)).len(), 1);
}

#[test]
fn an_answered_request_is_repeated_with_a_new_ticket() {
    let mut m = on_command(&["c1"]);
    let ticket = reads(tick(&mut m))[0].1;
    answer(&mut m, AGENT, "c1", ticket, Ok(tail("one\n")));
    let due = later(&m, OUTPUT_REFRESH);
    let again = reads(tick_at(&mut m, due));
    assert_eq!(again.len(), 1);
    assert_ne!(again[0].1, ticket);
}

#[test]
fn an_answer_is_kept_for_the_command_it_was_read_for() {
    let mut m = on_command(&["c1", "c2"]);
    let (shown, ticket, _) = reads(tick(&mut m)).remove(0);
    answer(&mut m, AGENT, &shown, ticket, Ok(tail("one\n")));
    assert_eq!(heard(&m, &shown).as_deref(), Some("one\n"));
}

#[test]
fn an_answer_with_the_ticket_of_another_request_is_dropped() {
    let mut m = on_command(&["c1"]);
    let older = m.ticket();
    let ticket = reads(tick(&mut m))[0].1;
    answer(&mut m, AGENT, "c1", older, Ok(tail("stale\n")));
    assert_eq!(heard(&m, "c1"), None);
    answer(&mut m, AGENT, "c1", ticket, Ok(tail("fresh\n")));
    assert_eq!(heard(&m, "c1").as_deref(), Some("fresh\n"));
}

#[test]
fn an_answer_for_a_command_nothing_asked_about_is_dropped() {
    let mut m = on_command(&["c1", "c2"]);
    let (shown, ticket, _) = reads(tick(&mut m)).remove(0);
    let other = if shown == "c1" { "c2" } else { "c1" };
    answer(&mut m, AGENT, other, ticket, Ok(tail("not asked\n")));
    assert!(m.command_output(&name(AGENT), &id(other)).is_none());
}

#[test]
fn an_answer_for_an_agent_that_is_not_tracked_is_dropped() {
    let mut m = on_command(&["c1"]);
    let ticket = reads(tick(&mut m))[0].1;
    answer(&mut m, "docs-sweep-6", "c1", ticket, Ok(tail("x\n")));
    assert!(m.command_output(&name("docs-sweep-6"), &id("c1")).is_none());
}

#[test]
fn a_request_from_before_the_command_was_dropped_and_listed_again_is_not_answered_for() {
    let mut m = on_command(&["c1"]);
    let first = reads(tick(&mut m))[0].1;
    run_commands(&mut m, AGENT, &[], FOUR_MINUTES);
    m.hold_activity_row();
    m.point_at_row(|_, _| 0);
    update(&mut m, Msg::Focus(TerminalFocus::Focused));
    run_commands(&mut m, AGENT, &[listed("c1", "cargo test")], FOUR_MINUTES);
    m.point_at_row(|_, last| last);
    let second = reads(tick(&mut m))[0].1;
    answer(&mut m, AGENT, "c1", first, Ok(tail("old\n")));
    assert_eq!(heard(&m, "c1"), None);
    answer(&mut m, AGENT, "c1", second, Ok(tail("new\n")));
    assert_eq!(heard(&m, "c1").as_deref(), Some("new\n"));
}

#[test]
fn a_failed_read_is_told_once_until_a_read_succeeds() {
    let mut m = on_command(&["c1"]);
    let ask = |m: &mut Model, secs: u64| {
        let at = later(m, Duration::from_secs(secs));
        reads(tick_at(m, at))[0].1
    };
    let ticket = ask(&mut m, 0);
    answer(&mut m, AGENT, "c1", ticket, failure());
    let told = m.status.as_ref().map(|s| s.text.clone());
    assert!(told.is_some_and(|text| text.contains("denied")));
    m.status = None;
    let ticket = ask(&mut m, 10);
    answer(&mut m, AGENT, "c1", ticket, failure());
    assert!(m.status.is_none(), "not told again");
    let ticket = ask(&mut m, 20);
    answer(&mut m, AGENT, "c1", ticket, Ok(tail("back\n")));
    let ticket = ask(&mut m, 30);
    answer(&mut m, AGENT, "c1", ticket, failure());
    assert!(m.status.is_some(), "told again after a success");
}

#[test]
fn a_failed_read_keeps_the_last_output_it_found_beside_the_failure() {
    let mut m = on_command(&["c1"]);
    let ticket = reads(tick(&mut m))[0].1;
    answer(&mut m, AGENT, "c1", ticket, Ok(tail("good\n")));
    let due = later(&m, OUTPUT_REFRESH);
    let ticket = reads(tick_at(&mut m, due))[0].1;
    answer(&mut m, AGENT, "c1", ticket, failure());
    let watch = m.command_output(&name(AGENT), &id("c1")).expect("kept");
    assert!(watch.failure().is_some());
    assert_eq!(heard(&m, "c1").as_deref(), Some("good\n"));
}

fn ended_with_a_last_read(listed_when_read: bool) -> Model {
    let mut m = on_command(&["c1"]);
    let ticket = reads(tick(&mut m))[0].1;
    answer(&mut m, AGENT, "c1", ticket, Ok(tail("last words\n")));
    if !listed_when_read {
        run_commands(&mut m, AGENT, &[], FOUR_MINUTES);
        m.hold_activity_row();
        let again = later(&m, OUTPUT_REFRESH);
        let ticket = reads(tick_at(&mut m, again))[0].1;
        answer(&mut m, AGENT, "c1", ticket, Ok(tail("last words\n")));
    }
    m
}

#[test]
fn a_command_that_ended_after_its_last_read_is_read_once_more_and_then_never() {
    let mut m = ended_with_a_last_read(true);
    run_commands(&mut m, AGENT, &[], FOUR_MINUTES);
    m.hold_activity_row();
    assert!(matches!(
        m.hover(),
        Some(crate::document::Hover::Command(..))
    ));
    let soon = later(&m, Duration::from_millis(10));
    let once = reads(tick_at(&mut m, soon));
    assert_eq!(once.len(), 1, "what it printed since the last read");
    answer(&mut m, AGENT, "c1", once[0].1, Ok(tail("final\n")));
    let much_later = later(&m, OUTPUT_REFRESH * 10);
    assert_eq!(reads(tick_at(&mut m, much_later)), Vec::new());
    assert_eq!(heard(&m, "c1").as_deref(), Some("final\n"));
}

#[test]
fn a_command_whose_last_read_was_after_it_ended_is_not_read_again() {
    let mut m = ended_with_a_last_read(false);
    assert!(matches!(
        m.hover(),
        Some(crate::document::Hover::Command(..))
    ));
    let much_later = later(&m, OUTPUT_REFRESH * 10);
    assert_eq!(reads(tick_at(&mut m, much_later)), Vec::new());
    assert_eq!(heard(&m, "c1").as_deref(), Some("last words\n"));
}

#[test]
fn what_was_read_of_a_command_is_dropped_once_it_is_unlisted_and_the_cursor_has_left() {
    let mut m = on_command(&["c1"]);
    let ticket = reads(tick(&mut m))[0].1;
    answer(&mut m, AGENT, "c1", ticket, Ok(tail("x\n")));
    m.point_at_row(|_, _| 0);
    run_commands(&mut m, AGENT, &[], FOUR_MINUTES);
    m.hold_activity_row();
    update(&mut m, Msg::Focus(TerminalFocus::Focused));
    assert!(m.command_output(&name(AGENT), &id("c1")).is_none());
}
