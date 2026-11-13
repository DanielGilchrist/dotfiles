//! The one-time pointer to `agent keys`, for a user who never chose keys.

use agent_core::config::KeysTable;
use agent_core::ports::{Hint, Store};

use crate::keymap::Spelling;
use crate::model::Model;
use crate::update::Effect;
use crate::worker::Job;

/// Whether the keys hint is still to be shown, known at startup from the
/// config and from the state directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeysHint {
    Due,
    NotDue,
}

impl KeysHint {
    /// Reads the one-time marker, with the rest of the startup state. A
    /// store that can't be read is noted in the error log and counts as
    /// shown, rather than showing the hint on every start.
    pub fn at_startup(table: KeysTable, store: &dyn Store) -> Self {
        match table {
            KeysTable::Present => Self::NotDue,
            KeysTable::Absent => match store.hint_shown(Hint::Keys) {
                Ok(false) => Self::Due,
                Ok(true) => Self::NotDue,
                Err(e) => {
                    store.note(&format!("reading the keys hint: {e}"));
                    Self::NotDue
                }
            },
        }
    }
}

/// Says where to change the keys, and asks for it to be recorded as shown.
pub fn announce(m: &mut Model, hint: KeysHint) -> Vec<Effect> {
    match hint {
        KeysHint::NotDue => Vec::new(),
        KeysHint::Due => {
            m.info(text(&Spelling::of(&m.keys)));
            vec![Effect::Job(Job::MarkHint(Hint::Keys))]
        }
    }
}

fn text(spelling: &Spelling) -> String {
    let switch = spelling
        .chords()
        .map(|chords| format!(" · {chords} switch agent"))
        .unwrap_or_default();
    format!(
        "keys: {} leaves an agent{switch} · `agent keys` to change them",
        spelling.nav()
    )
}

#[cfg(test)]
mod tests {
    use agent_core::config::{Hop, Keys};
    use agent_core::keyspec::Binding;
    use agent_core::testing::MemStore;

    use super::*;
    use crate::fixtures;
    use crate::keyspec::{Keyboard, Sendable};

    fn status(m: &Model) -> Option<&str> {
        m.status.as_ref().map(|status| status.text.as_str())
    }

    fn marked(effects: &[Effect]) -> bool {
        matches!(effects, [Effect::Job(Job::MarkHint(Hint::Keys))])
    }

    #[test]
    fn a_start_with_no_keys_table_and_no_marker_is_due_and_says_so_as_info_asking_to_be_recorded() {
        let store = MemStore::default();
        let hint = KeysHint::at_startup(KeysTable::Absent, &store);
        assert_eq!(hint, KeysHint::Due);
        let mut m = fixtures::model("/repo");
        let effects = announce(&mut m, hint);
        assert_eq!(status(&m), Some(text(&Spelling::of(&m.keys)).as_str()));
        assert_eq!(
            m.status.as_ref().map(|status| status.kind),
            Some(crate::model::StatusKind::Info)
        );
        assert!(marked(&effects));
        assert!(
            store.hints_shown.lock().expect("lock").is_empty(),
            "recording is the job's, not the UI thread's"
        );
    }

    #[test]
    fn the_text_names_the_nav_key_the_chords_and_the_command() {
        let shown = text(&Spelling::default());
        assert!(shown.contains("⌃]"), "{shown}");
        assert!(shown.contains("⌥, ⌥."), "{shown}");
        assert!(shown.contains("`agent keys`"), "{shown}");
    }

    #[test]
    fn once_recorded_the_hint_is_not_due_again() {
        let store = MemStore::default();
        store.mark_hint_shown(Hint::Keys).expect("succeeds");
        assert_eq!(
            KeysHint::at_startup(KeysTable::Absent, &store),
            KeysHint::NotDue
        );
        let mut m = fixtures::model("/repo");
        assert!(announce(&mut m, KeysHint::NotDue).is_empty());
        assert_eq!(status(&m), None);
    }

    #[test]
    fn a_config_with_a_keys_table_is_never_due() {
        let store = MemStore::default();
        assert_eq!(
            KeysHint::at_startup(KeysTable::Present, &store),
            KeysHint::NotDue
        );
    }

    #[test]
    fn chords_that_are_off_are_left_out_of_the_text() {
        let keys = Keys::default()
            .with_hop(Hop::Previous, Binding::Off)
            .with_hop(Hop::Next, Binding::Off);
        let sendable = Sendable::resolve(keys, Keyboard::Legacy);
        assert!(!text(&Spelling::of(&sendable)).contains("switch agent"));
    }
}
