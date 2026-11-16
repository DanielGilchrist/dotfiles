//! The user's config file: actions, the keys the TUI reserves, where
//! repos can be found and the colours.
//!
//! ```toml
//! [keys]
//! nav = "ctrl-]"
//! focus_left = "alt-h"
//! focus_right = "alt-l"
//! actions = "alt-x"
//! detach = "ctrl-\\"
//!
//! [repos]
//! dirs = ["~/repos", "~/.config"]
//!
//! [theme]
//! accent = "#89b4fa"
//!
//! [host]
//! kind = "native"
//! osc52 = "owner"
//! ```

use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::actions::{Actions, RawAction};
use crate::keyspec::{Binding, Control, Detachable, FunctionKey, InvalidKeySpec, KeySpec};
use crate::paths::{self, Typed};
use crate::theme::{RawTheme, Theme, ThemeError};

/// A key of the `[keys]` table. A new field is added to `ALL`, to
/// `Keys::binding`, `Keys::with`, `Display` and the parse in `RawKeys`, and
/// each match fails to compile until it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyField {
    Nav,
    /// Moves from [0] or an agent's terminal to the left panels.
    FocusLeft,
    /// Moves from a left panel to [0], entering the agent when [0] shows its
    /// terminal.
    FocusRight,
    /// Opens the actions menu for the agent the user is looking at.
    Actions,
}

impl KeyField {
    pub const ALL: [Self; 4] = [Self::Nav, Self::FocusLeft, Self::FocusRight, Self::Actions];

    /// The always-on chords: every field but nav, which always has a key.
    pub const CHORDS: [Self; 3] = [Self::FocusLeft, Self::FocusRight, Self::Actions];

    /// The line that sets this field to `spec` in a `[keys]` table.
    pub fn assignment(self, spec: KeySpec) -> String {
        format!(
            "{self} = {}",
            toml::Value::String(spec.written().to_string())
        )
    }
}

impl fmt::Display for KeyField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Nav => "nav",
            Self::FocusLeft => "focus_left",
            Self::FocusRight => "focus_right",
            Self::Actions => "actions",
        })
    }
}

/// The keys the TUI reserves for itself. Every default is a key any
/// terminal can send. Nav always has a key. The chords default to off,
/// because window managers bind the usual alt chords globally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Keys {
    nav: KeySpec,
    focus_left: Binding,
    focus_right: Binding,
    actions: Binding,
}

impl Default for Keys {
    fn default() -> Self {
        Self {
            nav: KeySpec::CTRL_CLOSE_BRACKET,
            focus_left: Binding::Off,
            focus_right: Binding::Off,
            actions: Binding::Off,
        }
    }
}

impl Keys {
    pub fn binding(&self, field: KeyField) -> Binding {
        match field {
            KeyField::Nav => Binding::On(self.nav),
            KeyField::FocusLeft => self.focus_left,
            KeyField::FocusRight => self.focus_right,
            KeyField::Actions => self.actions,
        }
    }

    pub fn nav(&self) -> KeySpec {
        self.nav
    }

    /// `field` back at its default.
    #[must_use]
    pub fn reset(self, field: KeyField) -> Self {
        let defaults = Self::default();
        match field {
            KeyField::Nav => self.with_nav(defaults.nav),
            KeyField::FocusLeft | KeyField::FocusRight | KeyField::Actions => {
                self.with(field, Binding::Off)
            }
        }
    }

    #[must_use]
    pub fn with_nav(self, nav: KeySpec) -> Self {
        Self { nav, ..self }
    }

    /// `binding` for a chord. Nav always has a key, so it is set with
    /// `with_nav` and this leaves it alone.
    #[must_use]
    pub fn with(self, field: KeyField, binding: Binding) -> Self {
        match field {
            KeyField::Nav => self,
            KeyField::FocusLeft => Self {
                focus_left: binding,
                ..self
            },
            KeyField::FocusRight => Self {
                focus_right: binding,
                ..self
            },
            KeyField::Actions => Self {
                actions: binding,
                ..self
            },
        }
    }

    pub fn bound(&self) -> Vec<(KeyField, KeySpec)> {
        KeyField::ALL
            .into_iter()
            .filter_map(|field| Some((field, self.binding(field).spec()?)))
            .collect()
    }

    /// A second key that leaves AGENT, for a nav key the terminal or the
    /// agent may swallow. Only a Ctrl-C0 nav key is sure to arrive, and
    /// whoever chose one chose it over this one.
    pub fn escape_hatch(&self) -> Option<KeySpec> {
        match self.nav {
            KeySpec::Ctrl(_) => None,
            KeySpec::Alt(_) | KeySpec::Function(_) | KeySpec::Kitty(_) => {
                Some(KeySpec::CTRL_CLOSE_BRACKET)
            }
        }
    }
}

/// Where the pickers look for repos: each directory is a repo itself or
/// holds repos, as `[repos] dirs` lists them, absolute.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sources(Vec<PathBuf>);

impl Sources {
    pub fn new(dirs: Vec<PathBuf>) -> Self {
        Self(dirs)
    }

    pub fn dirs(&self) -> &[PathBuf] {
        &self.0
    }
}

/// Whether a session host passes a program's OSC 52 clipboard writes to the
/// terminal of the client that owns the session. Reads are never passed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub enum Osc52 {
    #[default]
    Off,
    Owner,
}

impl Osc52 {
    /// The word the config and `agent session host --osc52` spell it with.
    pub const fn word(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Owner => "owner",
        }
    }
}

impl TryFrom<String> for Osc52 {
    type Error = String;

    fn try_from(text: String) -> Result<Self, String> {
        text.parse()
    }
}

impl std::str::FromStr for Osc52 {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        [Self::Off, Self::Owner]
            .into_iter()
            .find(|osc52| osc52.word() == text)
            .ok_or_else(|| format!("{text:?} isn't off or owner"))
    }
}

/// Which session host new sessions are made under. Sessions that already
/// exist stay with the host that runs them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Deserialize)]
#[serde(try_from = "String")]
pub enum HostKind {
    Zellij,
    #[default]
    Native,
}

impl HostKind {
    pub const fn word(self) -> &'static str {
        match self {
            Self::Zellij => "zellij",
            Self::Native => "native",
        }
    }
}

impl TryFrom<String> for HostKind {
    type Error = String;

    fn try_from(text: String) -> Result<Self, String> {
        text.parse()
    }
}

impl std::str::FromStr for HostKind {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        [Self::Zellij, Self::Native]
            .into_iter()
            .find(|kind| kind.word() == text)
            .ok_or_else(|| format!("{text:?} isn't zellij or native"))
    }
}

/// The key `agent attach` leaves a session with, `[keys] detach`. A person's
/// terminal has it and the TUI's pane never does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detach {
    Off,
    On(Detachable),
}

impl Default for Detach {
    fn default() -> Self {
        Self::On(Detachable::Ctrl(Control::Backslash))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DetachError {
    #[error(transparent)]
    Key(#[from] InvalidKeySpec),
    #[error("f{} can't be the detach key, use a ctrl or alt key or none", .0.number())]
    Function(FunctionKey),
}

impl std::str::FromStr for Detach {
    type Err = DetachError;

    fn from_str(text: &str) -> Result<Self, DetachError> {
        if text == "none" {
            return Ok(Self::Off);
        }
        let spec: KeySpec = text.parse()?;
        Detachable::try_from(spec)
            .map(Self::On)
            .map_err(DetachError::Function)
    }
}

impl fmt::Display for Detach {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Off => f.write_str("none"),
            Self::On(key) => write!(f, "{}", KeySpec::from(*key).written()),
        }
    }
}

/// The `[host]` table: which host makes new sessions, and what the native
/// session host lets a program's output do. The host never reads the config
/// itself, whoever starts it passes these on its command line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HostSettings {
    pub kind: HostKind,
    pub osc52: Osc52,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawHost {
    #[serde(default)]
    kind: HostKind,
    #[serde(default)]
    osc52: Osc52,
}

/// Whether the config file has a `[keys]` table, so the first-run hint can
/// tell a user who never chose keys from one who did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum KeysTable {
    #[default]
    Absent,
    Present,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    pub actions: Actions,
    pub keys: Keys,
    pub keys_table: KeysTable,
    pub sources: Sources,
    pub theme: Theme,
    pub host: HostSettings,
    pub detach: Detach,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error(transparent)]
    Toml(#[from] toml::de::Error),
    #[error("action {index} ({label}): {reason}")]
    Action {
        index: usize,
        label: String,
        reason: String,
    },
    #[error("keys.{field}: {source}")]
    Key {
        field: KeyField,
        source: InvalidKeySpec,
    },
    #[error("keys.{0} was removed; prev_agent and next_agent became focus_left and focus_right")]
    RemovedKey(&'static str),
    #[error("keys.detach: {0}")]
    Detach(#[from] DetachError),
    #[error("keys.{second} is the same key as keys.{first}")]
    DuplicateKey { first: KeyField, second: KeyField },
    #[error("keys.{field} is {hatch}, which also leaves AGENT while keys.nav is {nav}")]
    EscapeHatchTaken {
        field: KeyField,
        hatch: KeySpec,
        nav: KeySpec,
    },
    #[error("repos.dirs: {path:?} {reason}")]
    Source { path: String, reason: SourceError },
    #[error(transparent)]
    Theme(#[from] ThemeError),
}

/// Why a `[repos] dirs` entry can't be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SourceError {
    #[error("must be absolute, or start with ~/")]
    Relative,
    #[error("~user paths aren't supported")]
    UserHome,
    #[error("needs HOME set to stand for the home directory")]
    NoHome,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    action: Vec<RawAction>,
    keys: Option<RawKeys>,
    #[serde(default)]
    repos: RawRepos,
    #[serde(default)]
    theme: RawTheme,
    #[serde(default)]
    host: RawHost,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawRepos {
    #[serde(default)]
    dirs: Vec<String>,
}

impl RawRepos {
    /// Each dir absolute, with a leading `~` as the home directory.
    fn parse(self) -> Result<Sources, ConfigError> {
        let invalid = |path: &str, reason: SourceError| ConfigError::Source {
            path: path.to_string(),
            reason,
        };
        let home = paths::home().ok();
        let mut dirs = Vec::new();
        for raw in &self.dirs {
            let typed = raw
                .starts_with('~')
                .then(|| paths::typed_path(raw, Path::new("/"), home.as_deref()))
                .flatten();
            match typed {
                Some(Typed::Path(path)) => dirs.push(path),
                Some(Typed::Unsupported) => return Err(invalid(raw, SourceError::UserHome)),
                Some(Typed::NoHome) => return Err(invalid(raw, SourceError::NoHome)),
                None if Path::new(raw).is_absolute() => dirs.push(PathBuf::from(raw)),
                None => return Err(invalid(raw, SourceError::Relative)),
            }
        }
        Ok(Sources(dirs))
    }
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawKeys {
    nav: Option<String>,
    focus_left: Option<String>,
    focus_right: Option<String>,
    actions: Option<String>,
    detach: Option<String>,
    prev_agent: Option<toml::Value>,
    next_agent: Option<toml::Value>,
}

impl RawKeys {
    fn parse(self) -> Result<Keys, ConfigError> {
        for (key, removed) in [
            ("prev_agent", &self.prev_agent),
            ("next_agent", &self.next_agent),
        ] {
            if removed.is_some() {
                return Err(ConfigError::RemovedKey(key));
            }
        }
        let defaults = Keys::default();
        let nav = match self.nav {
            Some(raw) => raw.parse().map_err(|source| ConfigError::Key {
                field: KeyField::Nav,
                source,
            })?,
            None => defaults.nav(),
        };
        let chosen = |field: KeyField, raw: Option<String>| match raw {
            Some(raw) => raw
                .parse::<Binding>()
                .map(Some)
                .map_err(|source| ConfigError::Key { field, source }),
            None => Ok(None),
        };
        let chosen_chords = [
            (
                KeyField::FocusLeft,
                chosen(KeyField::FocusLeft, self.focus_left)?,
            ),
            (
                KeyField::FocusRight,
                chosen(KeyField::FocusRight, self.focus_right)?,
            ),
            (KeyField::Actions, chosen(KeyField::Actions, self.actions)?),
        ];
        let mut keys = Keys::default().with_nav(nav);
        for (field, binding) in chosen_chords {
            keys = keys.with(field, binding.unwrap_or(Binding::Off));
        }
        let explicit = keys.bound();
        ensure_distinct(&explicit)
            .map_err(|(first, second)| ConfigError::DuplicateKey { first, second })?;
        if let Some(hatch) = keys.escape_hatch()
            && let Some((field, _)) = explicit.iter().find(|(_, spec)| *spec == hatch)
        {
            return Err(ConfigError::EscapeHatchTaken {
                field: *field,
                hatch,
                nav: keys.nav(),
            });
        }
        Ok(keys)
    }
}

/// The first two fields given one key. Two specs are the same key exactly
/// when they are equal, because a spec matches one exact set of modifiers
/// and one key.
fn ensure_distinct<Field: Copy>(bindings: &[(Field, KeySpec)]) -> Result<(), (Field, Field)> {
    for (index, (first, spec)) in bindings.iter().enumerate() {
        if let Some((second, _)) = bindings[index + 1..]
            .iter()
            .find(|(_, other)| other == spec)
        {
            return Err((*first, *second));
        }
    }
    Ok(())
}

impl std::str::FromStr for Config {
    type Err = ConfigError;

    fn from_str(s: &str) -> Result<Self, ConfigError> {
        let raw: RawConfig = toml::from_str(s)?;
        let keys_table = if raw.keys.is_some() {
            KeysTable::Present
        } else {
            KeysTable::Absent
        };
        let detach = match raw.keys.as_ref().and_then(|keys| keys.detach.as_deref()) {
            Some(text) => text.parse()?,
            None => Detach::default(),
        };
        Ok(Self {
            actions: Actions::parse(raw.action)?,
            detach,
            keys: raw.keys.unwrap_or_default().parse()?,
            keys_table,
            sources: raw.repos.parse()?,
            theme: raw.theme.parse()?,
            host: HostSettings {
                kind: raw.host.kind,
                osc52: raw.host.osc52,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_detach_key_defaults_to_ctrl_backslash_and_can_be_off_or_chosen() {
        let config: Config = "".parse().expect("parses");
        assert_eq!(config.detach.to_string(), "ctrl-\\");
        let config: Config = "[keys]\ndetach = 'none'".parse().expect("parses");
        assert_eq!(config.detach, Detach::Off);
        let config: Config = "[keys]\ndetach = 'alt-q'".parse().expect("parses");
        assert_eq!(config.detach.to_string(), "alt-q");
        for bad in ["f5", "q", ""] {
            let text = format!("[keys]\ndetach = '{bad}'");
            assert!(text.parse::<Config>().is_err(), "{bad}");
        }
    }

    use super::*;

    #[test]
    fn new_sessions_go_to_zellij_unless_the_config_says_native() {
        let kind = |text: &str| text.parse::<Config>().expect("parses").host.kind;
        assert_eq!(kind(""), HostKind::Native);
        assert_eq!(kind("[host]\nkind = 'zellij'"), HostKind::Zellij);
        assert_eq!(kind("[host]\nkind = 'native'"), HostKind::Native);
        assert_eq!(kind("[host]\nosc52 = 'owner'"), HostKind::Native);
        for bad in [
            "[host]\nkind = 'tmux'",
            "[host]\nkind = 1",
            "[host]\nkind = ''",
        ] {
            assert!(bad.parse::<Config>().is_err(), "{bad}");
        }
    }

    #[test]
    fn the_hosts_clipboard_writes_are_off_unless_the_config_says_owner() {
        let config: Config = "".parse().expect("parses");
        assert_eq!(config.host.osc52, Osc52::Off);
        let config: Config = "[host]\nosc52 = 'owner'".parse().expect("parses");
        assert_eq!(config.host.osc52, Osc52::Owner);
        let config: Config = "[host]\nosc52 = 'off'".parse().expect("parses");
        assert_eq!(config.host.osc52, Osc52::Off);
        for bad in [
            "[host]\nosc52 = 'everyone'",
            "[host]\nosc52 = true",
            "[host]\nkind = 'x'",
        ] {
            assert!(bad.parse::<Config>().is_err(), "{bad}");
        }
    }

    #[test]
    fn every_chord_is_off_unless_the_config_sets_it() {
        let config: Config = "".parse().expect("parses");
        for field in KeyField::CHORDS {
            assert_eq!(config.keys.binding(field), Binding::Off, "{field}");
        }
        let config: Config =
            "[keys]\nfocus_left = 'alt-h'\nfocus_right = 'alt-l'\nactions = 'alt-x'"
                .parse()
                .expect("parses");
        let spelt: Vec<_> = KeyField::CHORDS
            .into_iter()
            .map(|field| config.keys.binding(field).to_string())
            .collect();
        assert_eq!(spelt, ["⌥h", "⌥l", "⌥x"]);
    }

    #[test]
    fn none_or_off_turns_only_that_chord_off() {
        for field in KeyField::CHORDS {
            for word in ["none", "off", "None"] {
                let config: Config = format!("[keys]\n{field} = '{word}'")
                    .parse()
                    .expect("parses");
                assert_eq!(config.keys.binding(field), Binding::Off, "{field} {word}");
            }
        }
        let config: Config = "[keys]\nfocus_left = 'f5'".parse().expect("parses");
        assert_eq!(config.keys.binding(KeyField::FocusLeft).to_string(), "F5");
        assert_eq!(config.keys.binding(KeyField::FocusRight), Binding::Off);
    }

    #[test]
    fn a_chord_may_not_share_a_key_with_nav_or_another_chord() {
        for (toml, first, second) in [
            (
                "[keys]\nactions = 'ctrl-]'",
                KeyField::Nav,
                KeyField::Actions,
            ),
            (
                "[keys]\nfocus_left = 'alt-h'\nfocus_right = 'alt-h'",
                KeyField::FocusLeft,
                KeyField::FocusRight,
            ),
            (
                "[keys]\nactions = 'alt-x'\nfocus_right = 'alt-x'",
                KeyField::FocusRight,
                KeyField::Actions,
            ),
        ] {
            match toml.parse::<Config>() {
                Err(ConfigError::DuplicateKey {
                    first: a,
                    second: b,
                }) => assert_eq!((a, b), (first, second), "{toml}"),
                other => panic!("{toml}: {other:?}"),
            }
        }
    }

    #[test]
    fn the_actions_chord_may_not_be_the_escape_hatch_of_a_non_ctrl_nav() {
        let error = "[keys]\nnav = 'alt-x'\nactions = 'ctrl-]'"
            .parse::<Config>()
            .expect_err("rejected");
        assert!(
            matches!(
                error,
                ConfigError::EscapeHatchTaken {
                    field: KeyField::Actions,
                    ..
                }
            ),
            "{error:?}"
        );
    }

    #[test]
    fn nav_defaults_to_ctrl_close_bracket() {
        let config: Config = "".parse().expect("parses");
        assert_eq!(config.keys.nav().to_string(), "⌃]");
    }

    #[test]
    fn every_default_key_is_one_any_terminal_can_send() {
        let defaults = Keys::default();
        assert!(!defaults.nav().needs_kitty());
        assert!(matches!(Detach::default(), Detach::On(Detachable::Ctrl(_))));
    }

    #[test]
    fn the_removed_agent_hop_keys_are_refused_naming_the_key_and_replacements() {
        for key in ["prev_agent", "next_agent"] {
            let error = format!("[keys]\n{key} = 'alt-;'")
                .parse::<Config>()
                .expect_err("rejected")
                .to_string();
            assert!(error.contains(key), "{error}");
            assert!(error.contains("focus_left and focus_right"), "{error}");
        }
    }

    #[test]
    fn a_mistake_in_a_chord_names_the_config_key_and_the_spec() {
        let err = |toml: &str| toml.parse::<Config>().expect_err("fails").to_string();
        assert!(
            err("[keys]\nfocus_left = 'banana'")
                .starts_with("keys.focus_left: \"banana\" isn't a key")
        );
        assert!(
            err("[keys]\nfocus_right = 'x'")
                .starts_with("keys.focus_right: \"x\" is a key on its own")
        );
        assert!(err("[keys]\nactions = ''").starts_with("keys.actions: is empty"));
        assert!(err("[keys]\nactions = 'alt-xx'").starts_with("keys.actions: "));
    }

    #[test]
    fn a_config_knows_whether_it_has_a_keys_table() {
        let config: Config = "".parse().expect("parses");
        assert_eq!(config.keys_table, KeysTable::Absent);
        let config: Config = "[theme]\naccent = 'red'".parse().expect("parses");
        assert_eq!(config.keys_table, KeysTable::Absent);
        let config: Config = "[keys]".parse().expect("parses");
        assert_eq!(config.keys_table, KeysTable::Present);
        assert_eq!(config.keys, Keys::default());
    }

    #[test]
    fn an_assignment_quotes_the_spec_so_the_config_reads_back_a_key_with_a_quote_or_backslash() {
        for written in ["ctrl-\\", "alt-\"", "alt-'"] {
            let spec: KeySpec = written.parse().expect("parses");
            let line = KeyField::FocusRight.assignment(spec);
            let config: Config = format!("[keys]\n{line}").parse().expect("parses");
            assert_eq!(
                config.keys.binding(KeyField::FocusRight),
                Binding::On(spec),
                "{line}"
            );
        }
    }

    #[test]
    fn two_keys_the_user_set_to_one_key_are_refused_naming_both() {
        let err = |toml: &str| toml.parse::<Config>().expect_err("fails").to_string();
        assert_eq!(
            err("[keys]\nnav = \"alt-'\"\nfocus_right = \"alt-'\""),
            "keys.focus_right is the same key as keys.nav"
        );
        assert_eq!(
            err("[keys]\nnav = 'alt-;'\nfocus_left = 'alt-;'"),
            "keys.focus_left is the same key as keys.nav"
        );
        assert_eq!(
            err("[keys]\nfocus_left = 'f5'\nfocus_right = 'f5'"),
            "keys.focus_right is the same key as keys.focus_left"
        );
    }

    #[test]
    fn a_chord_the_config_leaves_out_is_off_whatever_the_other_keys_are() {
        let config: Config = "[keys]\nnav = 'alt-h'".parse().expect("parses");
        for field in KeyField::CHORDS {
            assert_eq!(config.keys.binding(field), Binding::Off, "{field}");
        }
    }

    #[test]
    fn a_chord_on_none_does_not_clash_with_the_other_keys() {
        let config: Config =
            "[keys]\nfocus_left = 'none'\nfocus_right = 'none'\nactions = 'none'\nnav = 'alt-;'"
                .parse()
                .expect("parses");
        assert_eq!(config.keys.nav().to_string(), "⌥;");
    }

    #[test]
    fn a_nav_key_that_may_need_a_fallback_never_shares_ctrl_close_bracket_with_a_chord() {
        for nav in ["ctrl-shift-t", "super-j", "alt-x", "f5", "ctrl-h"] {
            let err = format!("[keys]\nnav = '{nav}'\nfocus_left = 'ctrl-]'")
                .parse::<Config>()
                .expect_err("fails")
                .to_string();
            assert!(err.contains("which also leaves AGENT"), "{nav}: {err}");
        }
    }

    #[test]
    fn a_chord_cannot_be_the_second_way_out_of_agent_while_nav_is_not_a_control_key() {
        let err = |toml: &str| toml.parse::<Config>().expect_err("fails").to_string();
        assert_eq!(
            err("[keys]\nnav = 'alt-x'\nfocus_right = 'ctrl-]'"),
            "keys.focus_right is ⌃], which also leaves AGENT while keys.nav is ⌥x"
        );
        assert_eq!(
            err("[keys]\nnav = 'f5'\nactions = 'ctrl-]'"),
            "keys.actions is ⌃], which also leaves AGENT while keys.nav is F5"
        );
        let config: Config = "[keys]\nnav = 'ctrl-g'\nfocus_left = 'ctrl-]'"
            .parse()
            .expect("a control nav key has no second way out");
        assert_eq!(config.keys.binding(KeyField::FocusLeft).to_string(), "⌃]");
    }

    #[test]
    fn reset_puts_a_chord_back_off_and_nav_back_on_ctrl_close_bracket() {
        let chosen = Keys::default()
            .with_nav("alt-'".parse().expect("parses"))
            .with(
                KeyField::FocusLeft,
                Binding::On("f5".parse().expect("parses")),
            );
        let reset = chosen.reset(KeyField::FocusLeft);
        assert_eq!(reset.binding(KeyField::FocusLeft), Binding::Off);
        assert_eq!(
            reset.reset(KeyField::Nav).nav(),
            KeySpec::CTRL_CLOSE_BRACKET
        );
    }

    #[test]
    fn a_config_naming_ctrl_backslash_still_parses() {
        let config: Config = "[keys]\nnav = 'ctrl-\\'".parse().expect("parses");
        assert_eq!(config.keys.nav().to_string(), "⌃\\");
    }

    #[test]
    fn nav_can_be_changed() {
        let config: Config = "[keys]\nnav = 'ctrl-g'".parse().expect("parses");
        assert_eq!(config.keys.nav().to_string(), "⌃G");
    }

    #[test]
    fn a_mistake_in_nav_names_the_config_key_and_the_spec() {
        let err = |toml: &str| toml.parse::<Config>().expect_err("fails").to_string();
        assert!(err("[keys]\nnav = 'esc'").starts_with("keys.nav: \"esc\""));
        assert!(err("[keys]\nnav = 'banana'").starts_with("keys.nav: \"banana\" isn't a key"));
    }

    #[test]
    fn a_key_that_isnt_in_the_table_is_refused() {
        let err = "[keys]\nnav_sustained = 'ctrl-]'"
            .parse::<Config>()
            .expect_err("fails");
        assert!(err.to_string().contains("unknown field"));
    }

    #[test]
    fn the_first_two_fields_with_one_key_are_reported() {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        enum Field {
            Nav,
            Prev,
            Next,
        }
        let spec: KeySpec = "alt-;".parse().expect("parses");
        let other: KeySpec = "alt-'".parse().expect("parses");
        assert_eq!(
            ensure_distinct(&[
                (Field::Nav, other),
                (Field::Prev, spec),
                (Field::Next, spec)
            ]),
            Err((Field::Prev, Field::Next))
        );
        assert_eq!(
            ensure_distinct(&[(Field::Nav, spec), (Field::Prev, other)]),
            Ok(())
        );
    }

    #[test]
    fn with_replaces_only_its_field_and_bound_lists_every_field_that_has_a_key() {
        let spec: KeySpec = "f5".parse().expect("parses");
        let on = |text: &str| Binding::On(text.parse().expect("parses"));
        let keys = Keys::default()
            .with_nav(spec)
            .with(KeyField::FocusLeft, on("f6"))
            .with(KeyField::FocusRight, on("f7"))
            .with(KeyField::Actions, on("f8"));
        assert_eq!(keys.nav(), spec);
        let fields: Vec<_> = keys.bound().into_iter().map(|(field, _)| field).collect();
        assert_eq!(fields, KeyField::ALL);
        let off = keys.with(KeyField::FocusRight, Binding::Off);
        let fields: Vec<_> = off.bound().into_iter().map(|(field, _)| field).collect();
        assert_eq!(
            fields,
            [KeyField::Nav, KeyField::FocusLeft, KeyField::Actions]
        );
    }

    #[test]
    fn theme_errors_name_the_field() {
        let err = "[theme]\naccent = 'mauve'"
            .parse::<Config>()
            .expect_err("fails");
        assert!(
            err.to_string()
                .starts_with("theme.accent: \"mauve\" isn't a colour")
        );
        let err = "[theme]\nborder = 'cyan'"
            .parse::<Config>()
            .expect_err("fails");
        assert!(err.to_string().contains("unknown field"));
    }

    #[test]
    fn repo_dirs_are_absolute_with_home_expanded() {
        let config: Config = "[repos]\ndirs = ['~/repos', '/srv/code']"
            .parse()
            .expect("parses");
        let home = paths::home().expect("HOME");
        assert_eq!(
            config.sources.dirs(),
            [home.join("repos"), PathBuf::from("/srv/code")]
        );
        let err = |toml: &str| toml.parse::<Config>().expect_err("fails");
        assert!(matches!(
            err("[repos]\ndirs = ['repos']"),
            ConfigError::Source {
                reason: SourceError::Relative,
                ..
            }
        ));
        assert!(matches!(
            err("[repos]\ndirs = ['./repos']"),
            ConfigError::Source {
                reason: SourceError::Relative,
                ..
            }
        ));
        assert!(matches!(
            err("[repos]\ndirs = ['~bob/repos']"),
            ConfigError::Source {
                reason: SourceError::UserHome,
                ..
            }
        ));
        assert!(
            err("[repos]\nroots = []")
                .to_string()
                .contains("unknown field")
        );
    }
}
