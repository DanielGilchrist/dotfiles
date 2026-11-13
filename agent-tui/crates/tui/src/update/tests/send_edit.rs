use super::*;

const WHO: &str = "auth-refresh-2";

fn ctrl_e(m: &mut Model) -> Vec<Effect> {
    update(m, key_with(KeyCode::Char('e'), KeyModifiers::CONTROL))
}

fn edits(effects: &[Effect]) -> Vec<(String, String)> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::EditMessage { to, text } => Some((to.to_string(), text.clone())),
            _ => None,
        })
        .collect()
}

fn box_with(text: &str) -> Model {
    let mut m = crate::fixtures::fleet(10);
    set_state(&mut m, WHO, working(), 1);
    m.set_cursor(Cursor::Entry(name(WHO)));
    update(&mut m, key(KeyCode::Char('s')));
    type_text(&mut m, text);
    m
}

fn edited(m: &mut Model, result: Result<Option<String>, String>) -> Option<String> {
    message_edited(m, &name(WHO), result)
}

#[test]
fn ctrl_e_in_the_box_asks_for_the_editor_with_the_text_so_far_and_keeps_the_box_open() {
    let mut m = box_with("first\nsecond");
    update(&mut m, key_with(KeyCode::Enter, KeyModifiers::ALT));
    type_text(&mut m, "third");
    let effects = ctrl_e(&mut m);
    assert_eq!(
        edits(&effects),
        [(WHO.to_string(), "first\nsecond\nthird".to_string())]
    );
    assert!(matches!(m.overlay, Overlay::Talk(_)), "the box stays open");
    assert_eq!(replies(&effects), Vec::<String>::new(), "nothing is sent");
}

#[test]
fn what_the_editor_returns_replaces_the_text_with_the_caret_at_its_end_and_is_not_sent() {
    let mut m = box_with("draft");
    assert_eq!(edited(&mut m, Ok(Some("a longer\nmessage".into()))), None);
    assert_eq!(composing(&m, WHO), Some((None, "a longer\nmessage".into())));
    type_text(&mut m, "!");
    assert_eq!(
        composing(&m, WHO),
        Some((None, "a longer\nmessage!".into())),
        "caret at the end"
    );
    assert!(matches!(m.overlay, Overlay::Talk(_)));
    let sent = update(&mut m, key(KeyCode::Enter));
    assert_eq!(replies(&sent), ["a longer\nmessage!"], "only ⏎ sends");
}

#[test]
fn an_unchanged_result_leaves_the_text_as_it_was() {
    let mut m = box_with("same");
    assert_eq!(edited(&mut m, Ok(Some("same".into()))), None);
    assert_eq!(composing(&m, WHO), Some((None, "same".into())));
}

#[test]
fn an_empty_result_leaves_the_box_empty() {
    let mut m = box_with("draft");
    assert_eq!(edited(&mut m, Ok(Some(String::new()))), None);
    assert_eq!(composing(&m, WHO), Some((None, String::new())));
    assert!(
        update(&mut m, key(KeyCode::Enter)).is_empty(),
        "nothing to send"
    );
}

#[test]
fn a_cancelled_editor_keeps_the_text() {
    let mut m = box_with("draft");
    assert_eq!(edited(&mut m, Ok(None)), None);
    assert_eq!(composing(&m, WHO), Some((None, "draft".into())));
}

#[test]
fn a_failed_editor_says_so_and_keeps_the_text() {
    let mut m = box_with("draft");
    edited(
        &mut m,
        Err("running nvim: No such file or directory (os error 2)".into()),
    );
    assert_eq!(composing(&m, WHO), Some((None, "draft".into())));
    let status = m.status.as_ref().expect("said");
    assert_eq!(status.kind, StatusKind::Error);
    assert!(status.text.contains("running nvim"));
}

#[test]
fn a_message_refused_for_a_menu_keeps_its_text_for_the_editor_and_for_the_next_s() {
    let mut m = crate::fixtures::fleet(10);
    crate::fixtures::ask(&mut m, WHO);
    m.set_cursor(Cursor::Entry(name(WHO)));
    update(&mut m, key(KeyCode::Char('s')));
    type_text(&mut m, "rebase");
    assert!(update(&mut m, key(KeyCode::Enter)).is_empty(), "refused");
    let effects = ctrl_e(&mut m);
    assert_eq!(edits(&effects), [(WHO.to_string(), "rebase".to_string())]);
    assert_eq!(edited(&mut m, Ok(Some("rebase onto main".into()))), None);
    assert_eq!(composing(&m, WHO), Some((None, "rebase onto main".into())));
    assert!(
        update(&mut m, key(KeyCode::Enter)).is_empty(),
        "still refused, still there"
    );
    assert_eq!(composing(&m, WHO), Some((None, "rebase onto main".into())));
}

#[test]
fn the_editor_holds_only_the_typed_message_and_the_quote_and_review_stay_outside_it() {
    let (mut m, who) = reading_the_story();
    let line = crate::fixtures::line_info("a.rs", "+let x = 3;", 4);
    add_comment(&mut m, &who, &[line], "Use the limit.", None);
    update(&mut m, key(KeyCode::Char('g')));
    update(&mut m, key(KeyCode::Char(']')));
    update(&mut m, key(KeyCode::Char('s')));
    type_text(&mut m, "why?");
    let quote = talk_of(&m).and_then(|(quote, _)| quote.map(str::to_string));
    assert!(quote.is_some(), "a reply");
    let effects = ctrl_e(&mut m);
    let [Effect::EditMessage { text, .. }] = effects.as_slice() else {
        panic!("one edit")
    };
    assert_eq!(text, "why?", "no quote and no review comments in it");
    assert_eq!(
        message_edited(&mut m, &who, Ok(Some("why not?".into()))),
        None
    );
    assert_eq!(
        talk_of(&m).map(|(quote, text)| (quote.map(str::to_string), text.to_string())),
        Some((quote, "why not?".to_string()))
    );
    assert_eq!(m.draft(&who).len(), 1, "the drafted review stays");
}

#[test]
fn a_box_that_is_with_the_worker_cannot_be_edited() {
    let mut m = box_with("rebase");
    let sent = update(&mut m, key(KeyCode::Enter));
    assert_eq!(replies(&sent), ["rebase"]);
    update(&mut m, key(KeyCode::Char('s')));
    assert_eq!(edits(&ctrl_e(&mut m)), Vec::new());
}

#[test]
fn an_edited_message_whose_agent_went_while_the_editor_ran_is_said_and_given_back() {
    let mut m = box_with("draft");
    m.agents.remove(&name(WHO));
    let lost = edited(&mut m, Ok(Some("what I wrote".into())));
    assert_eq!(lost.as_deref(), Some("what I wrote"));
    let status = m.status.as_ref().expect("said");
    assert_eq!(status.kind, StatusKind::Error);
    assert!(status.text.contains("the edited message was dropped") && status.text.contains(WHO));
}

#[test]
fn an_empty_edit_of_a_box_that_went_is_nothing_to_keep() {
    let mut m = box_with("draft");
    m.agents.remove(&name(WHO));
    assert_eq!(edited(&mut m, Ok(Some("  \n".into()))), None);
}
