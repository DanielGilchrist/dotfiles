use super::*;
use crate::fixtures::{asking, connect};
use crate::model::Mode;

const WHO: &str = "auth-refresh-2";
const CTRL: KeyModifiers = KeyModifiers::CONTROL;

fn ready_box() -> Model {
    let mut m = crate::fixtures::fleet(10);
    set_state(&mut m, WHO, working(), 1);
    m.set_cursor(Cursor::Entry(name(WHO)));
    update(&mut m, key(KeyCode::Char('s')));
    m
}

fn ctrl_v(m: &mut Model) -> Vec<Effect> {
    update(m, key_with(KeyCode::Char('v'), CTRL))
}

fn image_pastes(effects: &[Effect]) -> Vec<String> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Job(Job::PasteImage { to }) => Some(to.to_string()),
            _ => None,
        })
        .collect()
}

fn jobs_in_order(effects: &[Effect]) -> Vec<&'static str> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Job(Job::PasteImage { .. }) => Some("image"),
            Effect::Job(Job::Reply { .. }) => Some("message"),
            _ => None,
        })
        .collect()
}

fn pty_bytes(effects: &[Effect]) -> Vec<Vec<u8>> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Pty { bytes, .. } => Some(bytes.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn ctrl_v_in_the_box_asks_the_worker_to_paste_an_image_into_that_agent() {
    let mut m = ready_box();
    let effects = ctrl_v(&mut m);
    assert_eq!(image_pastes(&effects), [WHO]);
    assert_eq!(effects.len(), 1, "nothing else is asked");
}

#[test]
fn ctrl_v_keeps_the_box_open_with_its_text() {
    let mut m = ready_box();
    type_text(&mut m, "look at this");
    ctrl_v(&mut m);
    assert!(matches!(&m.overlay, Overlay::Talk(to) if to.as_str() == WHO));
    assert_eq!(composing(&m, WHO), Some((None, "look at this".into())));
}

#[test]
fn ctrl_v_is_refused_with_a_reason_while_the_agent_is_asking() {
    let mut m = ready_box();
    m.activities
        .get_or_insert_default()
        .insert(name(WHO), asking("rm?", 1));
    type_text(&mut m, "keep");
    let effects = ctrl_v(&mut m);
    assert_eq!(image_pastes(&effects), Vec::<String>::new());
    assert!(status(&m).contains("image not pasted"), "{}", status(&m));
    assert!(status(&m).contains("asking"), "{}", status(&m));
    assert_eq!(composing(&m, WHO), Some((None, "keep".into())));
}

#[test]
fn ctrl_v_then_enter_delivers_the_image_before_the_text() {
    let mut m = ready_box();
    let mut effects = ctrl_v(&mut m);
    type_text(&mut m, "what is this");
    effects.extend(update(&mut m, key(KeyCode::Enter)));
    assert_eq!(effects.len(), 2, "both go to the worker's one queue");
    assert_eq!(jobs_in_order(&effects), ["image", "message"]);
    assert_eq!(replies(&effects), ["what is this"]);
}

#[test]
fn a_control_character_in_pasted_text_never_reaches_the_message() {
    let mut m = ready_box();
    update(&mut m, Msg::Paste("a\x16b".into()));
    type_text(&mut m, "c");
    assert_eq!(composing(&m, WHO), Some((None, "abc".into())));
    let sent = update(&mut m, key(KeyCode::Enter));
    assert_eq!(replies(&sent), ["abc"]);
}

#[test]
fn the_worker_answer_says_the_image_paste_was_sent() {
    let mut m = ready_box();
    update(
        &mut m,
        Msg::ImagePasted {
            to: name(WHO),
            result: Ok(()),
        },
    );
    assert_eq!(status(&m), format!("image paste sent to {WHO}"));
}

#[test]
fn a_worker_refusal_of_the_image_paste_is_said_not_dropped() {
    let mut m = ready_box();
    update(
        &mut m,
        Msg::ImagePasted {
            to: name(WHO),
            result: Err(ImageError::Held(Hold::Asking)),
        },
    );
    assert!(status(&m).contains("image not pasted"), "{}", status(&m));
}

#[test]
fn a_nav_paste_opens_the_box_prefilled_with_cleaned_text_and_sends_nothing() {
    let mut m = crate::fixtures::fleet(10);
    set_state(&mut m, WHO, done(), 1);
    m.set_cursor(Cursor::Entry(name(WHO)));
    let effects = update(&mut m, Msg::Paste("one\r\ntwo\x1b[31m\rthree".into()));
    assert!(matches!(&m.overlay, Overlay::Talk(to) if to.as_str() == WHO));
    assert_eq!(composing(&m, WHO), Some((None, "one\ntwo\nthree".into())));
    assert!(effects.is_empty(), "nothing is sent");
    let sent = update(&mut m, key(KeyCode::Enter));
    assert_eq!(replies(&sent), ["one\ntwo\nthree"], "only ⏎ sends");
}

#[test]
fn a_nav_paste_puts_the_caret_at_the_end_of_the_pasted_text() {
    let mut m = crate::fixtures::fleet(10);
    set_state(&mut m, WHO, done(), 1);
    m.set_cursor(Cursor::Entry(name(WHO)));
    update(&mut m, Msg::Paste("abc".into()));
    type_text(&mut m, "d");
    assert_eq!(composing(&m, WHO), Some((None, "abcd".into())));
}

#[test]
fn a_nav_paste_with_no_agent_selected_says_so() {
    let mut m = model(&[]);
    update(&mut m, key(KeyCode::Char('[')));
    update(&mut m, key(KeyCode::Char('g')));
    assert!(matches!(m.cursor(), Some(Cursor::Repo(_))));
    update(&mut m, Msg::Paste("text".into()));
    assert!(matches!(m.overlay, Overlay::None));
    assert!(status(&m).contains("select an agent"), "{}", status(&m));
}

#[test]
fn a_nav_paste_on_this_tuis_own_session_says_why() {
    let mut m = crate::fixtures::fleet(10);
    set_state(&mut m, WHO, done(), 1);
    m.own_session = Some(name(WHO));
    m.set_cursor(Cursor::Entry(name(WHO)));
    update(&mut m, Msg::Paste("text".into()));
    assert!(matches!(m.overlay, Overlay::None));
    assert!(status(&m).contains("where this TUI runs"), "{}", status(&m));
}

#[test]
fn a_nav_paste_to_a_held_agent_opens_the_box_as_s_does_and_keeps_the_text() {
    let mut m = crate::fixtures::fleet(10);
    m.activities
        .get_or_insert_default()
        .insert(name(WHO), asking("rm?", 1));
    m.set_cursor(Cursor::Entry(name(WHO)));
    let effects = update(&mut m, Msg::Paste("text".into()));
    assert!(matches!(&m.overlay, Overlay::Talk(to) if to.as_str() == WHO));
    assert_eq!(composing(&m, WHO), Some((None, "text".into())));
    assert!(effects.is_empty());
    let sent = update(&mut m, key(KeyCode::Enter));
    assert_eq!(
        replies(&sent),
        Vec::<String>::new(),
        "⏎ is refused as for s"
    );
    assert!(status(&m).contains("asking"), "{}", status(&m));
    assert_eq!(composing(&m, WHO), Some((None, "text".into())), "kept");
}

#[test]
fn an_empty_nav_paste_opens_nothing() {
    let mut m = crate::fixtures::fleet(10);
    set_state(&mut m, WHO, done(), 1);
    m.set_cursor(Cursor::Entry(name(WHO)));
    update(&mut m, Msg::Paste(String::new()));
    update(&mut m, Msg::Paste("\x1b[31m\x07".into()));
    assert!(matches!(m.overlay, Overlay::None));
    assert_eq!(composing(&m, WHO), None);
}

#[test]
fn a_nav_paste_while_a_message_is_with_the_worker_is_refused_not_lost() {
    let mut m = crate::fixtures::fleet(10);
    set_state(&mut m, WHO, done(), 1);
    let effects = talk_to(&mut m, WHO, "first");
    assert_eq!(replies(&effects), ["first"]);
    update(&mut m, key(KeyCode::Esc));
    update(&mut m, Msg::Paste("second".into()));
    assert!(matches!(m.overlay, Overlay::None), "no box opens");
    assert!(status(&m).contains("still sending to"), "{}", status(&m));
    delivered(&mut m, WHO);
    assert_eq!(composing(&m, WHO), None);
}

#[test]
fn a_nav_paste_goes_to_the_agent_the_peek_shows_not_the_selected_one() {
    let mut m = crate::fixtures::fleet(10);
    set_state(&mut m, WHO, done(), 1);
    connect(&mut m, WHO, Mode::Nav);
    m.peek = Some(Peek::Pane);
    m.set_cursor(Cursor::Entry(name("obs-layout-4")));
    update(&mut m, Msg::Paste("hi".into()));
    assert!(matches!(&m.overlay, Overlay::Talk(to) if to.as_str() == WHO));
    assert_eq!(composing(&m, WHO), Some((None, "hi".into())));
}

#[test]
fn a_nav_paste_with_another_overlay_open_is_dropped_with_a_reason_and_opens_no_box() {
    for (label, opener) in [
        ("help", '?'),
        ("switcher", ' '),
        ("picker", 'p'),
        ("confirm", 'D'),
    ] {
        let mut m = crate::fixtures::fleet(10);
        set_state(&mut m, WHO, done(), 1);
        m.set_cursor(Cursor::Entry(name(WHO)));
        update(&mut m, key(KeyCode::Char(opener)));
        assert!(!matches!(m.overlay, Overlay::None), "{label} is open");
        update(&mut m, Msg::Paste("text".into()));
        assert!(
            !matches!(m.overlay, Overlay::Talk(_)),
            "{label}: the box opened"
        );
        assert_eq!(composing(&m, WHO), None, "{label}");
        assert!(
            status(&m).contains("paste not taken"),
            "{label}: {}",
            status(&m)
        );
    }
}

#[test]
fn a_nav_paste_on_an_exited_agent_says_why() {
    let mut m = model(&[]);
    m.set_cursor(Cursor::Entry(name("api")));
    update(&mut m, Msg::Paste("text".into()));
    assert!(matches!(m.overlay, Overlay::None));
    assert!(status(&m).contains("isn't running"), "{}", status(&m));
}

#[test]
fn a_paste_into_the_open_box_inserts_at_the_caret() {
    let mut m = ready_box();
    type_text(&mut m, "ab");
    update(&mut m, key(KeyCode::Left));
    update(&mut m, Msg::Paste("XY".into()));
    assert_eq!(composing(&m, WHO), Some((None, "aXYb".into())));
}

#[test]
fn a_paste_in_agent_mode_is_forwarded_to_the_agent_as_before() {
    let mut m = crate::fixtures::fleet(10);
    connect(&mut m, WHO, Mode::Agent);
    let effects = update(&mut m, Msg::Paste("hello".into()));
    assert_eq!(pty_bytes(&effects), [b"hello".to_vec()]);
    assert!(matches!(m.overlay, Overlay::None), "no box opens");
}

#[test]
fn a_raw_control_key_event_in_the_text_is_refused_at_send_and_nothing_is_typed() {
    let mut m = ready_box();
    type_text(&mut m, "a\x16b");
    let effects = update(&mut m, key(KeyCode::Enter));
    assert_eq!(replies(&effects), Vec::<String>::new());
    assert_eq!(image_pastes(&effects), Vec::<String>::new());
    assert!(status(&m).contains("control character"), "{}", status(&m));
    assert!(matches!(m.overlay, Overlay::Talk(_)), "the box stays");
}

#[test]
fn escape_sequences_in_a_paste_are_removed_whole() {
    let mut m = ready_box();
    update(
        &mut m,
        Msg::Paste("a\x1b[31mb\x1b]0;title\x07c\x1bPq\x1b\\d\x1b".into()),
    );
    assert_eq!(composing(&m, WHO), Some((None, "abcd".into())));
}

#[test]
fn a_second_ctrl_v_before_the_first_is_answered_is_ignored() {
    let mut m = ready_box();
    assert_eq!(image_pastes(&ctrl_v(&mut m)), [WHO]);
    let second = ctrl_v(&mut m);
    assert_eq!(image_pastes(&second), Vec::<String>::new());
    assert_eq!(status(&m), "image paste already pending");
    update(
        &mut m,
        Msg::ImagePasted {
            to: name(WHO),
            result: Ok(()),
        },
    );
    assert_eq!(
        image_pastes(&ctrl_v(&mut m)),
        [WHO],
        "free again once answered"
    );
}

#[test]
fn a_failed_image_paste_is_repeated_by_the_status_of_the_message_sent_after_it() {
    let mut m = ready_box();
    ctrl_v(&mut m);
    type_text(&mut m, "hi");
    let effects = update(&mut m, key(KeyCode::Enter));
    update(
        &mut m,
        Msg::ImagePasted {
            to: name(WHO),
            result: Err(ImageError::Held(Hold::Asking)),
        },
    );
    assert_eq!(m.status.as_ref().map(|s| s.kind), Some(StatusKind::Error));
    worker_says(&mut m, &effects, Ok(()));
    assert!(
        status(&m).starts_with(&format!("sent to {WHO}")),
        "{}",
        status(&m)
    );
    assert!(status(&m).contains("image not pasted"), "{}", status(&m));
}

#[test]
fn a_harness_without_an_image_key_says_so() {
    let mut m = ready_box();
    update(
        &mut m,
        Msg::ImagePasted {
            to: name(WHO),
            result: Err(ImageError::NoKey),
        },
    );
    assert_eq!(status(&m), "image not pasted: this harness can't");
}
