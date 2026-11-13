//! The user's config file: actions, the keys the TUI reserves, where
//! repos can be found and the colours.
//!
//! ```toml
//! [keys]
//! nav = "ctrl-]"
//! prev_agent = "alt-,"
//! next_agent = "none"
//!
//! [repos]
//! dirs = ["~/repos", "~/.config"]
//!
//! [theme]
//! accent = "#89b4fa"
//! ```

use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::actions::{Actions, RawAction};
use crate::keyspec::{Binding, Glyph, InvalidKeySpec, KeySpec};
use crate::paths::{self, Typed};
use crate::theme::{RawTheme, Theme, ThemeError};

/// Which neighbour in [1] an always-on chord moves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hop {
    Previous,
    Next,
}

impl Hop {
    pub const ALL: [Self; 2] = [Self::Previous, Self::Next];

    pub fn delta(self) -> isize {
        match self {
            Self::Previous => -1,
            Self::Next => 1,
        }
    }
}

/// A key of the `[keys]` table. A new field is added here, to `all`, to
/// `Keys::binding`, `Keys::with_hop` and `Keys::reset`, and each match fails
/// to compile until it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyField {
    Nav,
    Hop(Hop),
}

impl KeyField {
    pub fn all() -> impl Iterator<Item = Self> {
        std::iter::once(Self::Nav).chain(Hop::ALL.into_iter().map(Self::Hop))
    }
}

impl KeyField {
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
            Self::Hop(Hop::Previous) => "prev_agent",
            Self::Hop(Hop::Next) => "next_agent",
        })
    }
}

/// The keys the TUI reserves for itself. Every default is a key any
/// terminal can send. Nav always has a key, the agent chords may be off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Keys {
    nav: KeySpec,
    previous: Binding,
    next: Binding,
}

impl Default for Keys {
    fn default() -> Self {
        Self {
            nav: KeySpec::CTRL_CLOSE_BRACKET,
            previous: Binding::On(KeySpec::Alt(Glyph::COMMA)),
            next: Binding::On(KeySpec::Alt(Glyph::PERIOD)),
        }
    }
}

impl Keys {
    pub fn binding(&self, field: KeyField) -> Binding {
        match field {
            KeyField::Nav => Binding::On(self.nav),
            KeyField::Hop(hop) => self.hop(hop),
        }
    }

    pub fn nav(&self) -> KeySpec {
        self.nav
    }

    pub fn hop(&self, hop: Hop) -> Binding {
        match hop {
            Hop::Previous => self.previous,
            Hop::Next => self.next,
        }
    }

    /// `field` back at its default, or `Taken` when another key already has
    /// that default, so a fallback never makes two fields share a key. Nav
    /// can't meet that: a nav key that needs a fallback also has the escape
    /// hatch `ctrl-]`, which no chord may be.
    #[must_use]
    pub fn reset(self, field: KeyField) -> Self {
        let defaults = Self::default();
        match field {
            KeyField::Nav => Self {
                nav: defaults.nav,
                ..self
            },
            KeyField::Hop(hop) => {
                self.with_hop(hop, self.default_or_taken(field, defaults.hop(hop)))
            }
        }
    }

    fn default_or_taken(&self, field: KeyField, default: Binding) -> Binding {
        let Some(spec) = default.spec() else {
            return default;
        };
        match self
            .bound()
            .into_iter()
            .find(|(other, bound)| *other != field && *bound == spec)
        {
            Some((by, _)) => Binding::Taken { default: spec, by },
            None => default,
        }
    }

    #[must_use]
    pub fn with_nav(self, nav: KeySpec) -> Self {
        Self { nav, ..self }
    }

    #[must_use]
    pub fn with_hop(self, hop: Hop, binding: Binding) -> Self {
        match hop {
            Hop::Previous => Self {
                previous: binding,
                ..self
            },
            Hop::Next => Self {
                next: binding,
                ..self
            },
        }
    }

    pub fn bound(&self) -> Vec<(KeyField, KeySpec)> {
        KeyField::all()
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
    prev_agent: Option<String>,
    next_agent: Option<String>,
}

impl RawKeys {
    fn parse(self) -> Result<Keys, ConfigError> {
        let defaults = Keys::default();
        let nav = match self.nav {
            Some(raw) => raw.parse().map_err(|source| ConfigError::Key {
                field: KeyField::Nav,
                source,
            })?,
            None => defaults.nav(),
        };
        let chosen = |hop: Hop, raw: Option<String>| match raw {
            Some(raw) => raw.parse().map(Some).map_err(|source| ConfigError::Key {
                field: KeyField::Hop(hop),
                source,
            }),
            None => Ok(None),
        };
        let chosen_hops = [
            (Hop::Previous, chosen(Hop::Previous, self.prev_agent)?),
            (Hop::Next, chosen(Hop::Next, self.next_agent)?),
        ];
        let mut keys = Keys::default().with_nav(nav);
        for (hop, binding) in chosen_hops {
            keys = keys.with_hop(hop, binding.unwrap_or(Binding::Off));
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
        for (hop, binding) in chosen_hops {
            if binding.is_none() {
                let field = KeyField::Hop(hop);
                keys = keys.with_hop(hop, keys.default_or_taken(field, defaults.hop(hop)));
            }
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
        Ok(Self {
            actions: Actions::parse(raw.action)?,
            keys: raw.keys.unwrap_or_default().parse()?,
            keys_table,
            sources: raw.repos.parse()?,
            theme: raw.theme.parse()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nav_defaults_to_ctrl_close_bracket() {
        let config: Config = "".parse().expect("parses");
        assert_eq!(config.keys.nav().to_string(), "⌃]");
    }

    #[test]
    fn every_default_key_is_one_any_terminal_can_send() {
        let defaults = Keys::default();
        for field in KeyField::all() {
            let spec = defaults.binding(field).spec().expect("on by default");
            assert!(!spec.needs_kitty(), "{field}");
        }
    }

    #[test]
    fn the_agent_chords_are_on_by_default_as_alt_comma_and_alt_period() {
        let config: Config = "".parse().expect("parses");
        assert_eq!(config.keys.hop(Hop::Previous).to_string(), "⌥,");
        assert_eq!(config.keys.hop(Hop::Next).to_string(), "⌥.");
        let config: Config = "[keys]\nnav = 'ctrl-g'".parse().expect("parses");
        assert_eq!(config.keys.hop(Hop::Next).to_string(), "⌥.");
    }

    #[test]
    fn a_chord_can_be_changed_and_none_or_off_turns_only_that_one_off() {
        let config: Config = "[keys]\nprev_agent = 'f5'".parse().expect("parses");
        assert_eq!(config.keys.hop(Hop::Previous).to_string(), "F5");
        assert_eq!(config.keys.hop(Hop::Next).to_string(), "⌥.");
        for word in ["none", "off", "None"] {
            let config: Config = format!("[keys]\nprev_agent = '{word}'")
                .parse()
                .expect("parses");
            assert_eq!(config.keys.hop(Hop::Previous), Binding::Off, "{word}");
            assert_eq!(config.keys.hop(Hop::Next).to_string(), "⌥.", "{word}");
            let config: Config = format!("[keys]\nnext_agent = '{word}'")
                .parse()
                .expect("parses");
            assert_eq!(config.keys.hop(Hop::Next), Binding::Off, "{word}");
            assert_eq!(config.keys.hop(Hop::Previous).to_string(), "⌥,", "{word}");
        }
    }

    #[test]
    fn a_mistake_in_a_chord_names_the_config_key_and_the_spec() {
        let err = |toml: &str| toml.parse::<Config>().expect_err("fails").to_string();
        assert!(
            err("[keys]\nprev_agent = 'banana'")
                .starts_with("keys.prev_agent: \"banana\" isn't a key")
        );
        assert!(
            err("[keys]\nnext_agent = 'x'")
                .starts_with("keys.next_agent: \"x\" is a key on its own")
        );
        assert!(err("[keys]\nnext_agent = ''").starts_with("keys.next_agent: is empty"));
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
        for text in ["ctrl-\\", "alt-\"", "alt-'"] {
            let spec: KeySpec = text.parse().expect("parses");
            let line = KeyField::Hop(Hop::Next).assignment(spec);
            let config: Config = format!("[keys]\n{line}\n").parse().expect("parses");
            assert_eq!(config.keys.hop(Hop::Next), Binding::On(spec), "{line}");
        }
    }

    #[test]
    fn two_keys_the_user_set_to_one_key_are_refused_naming_both() {
        let err = |toml: &str| toml.parse::<Config>().expect_err("fails").to_string();
        assert_eq!(
            err("[keys]\nnav = 'alt-.'\nnext_agent = 'alt-.'"),
            "keys.next_agent is the same key as keys.nav"
        );
        assert_eq!(
            err("[keys]\nnav = 'alt-,'\nprev_agent = 'alt-,'"),
            "keys.prev_agent is the same key as keys.nav"
        );
        assert_eq!(
            err("[keys]\nprev_agent = 'f5'\nnext_agent = 'f5'"),
            "keys.next_agent is the same key as keys.prev_agent"
        );
    }

    #[test]
    fn a_default_chord_that_another_key_the_user_set_has_is_left_off_not_an_error() {
        for (toml, hop, by) in [
            ("[keys]\nnav = 'alt-,'", Hop::Previous, KeyField::Nav),
            ("[keys]\nnav = 'alt-.'", Hop::Next, KeyField::Nav),
            (
                "[keys]\nprev_agent = 'alt-.'",
                Hop::Next,
                KeyField::Hop(Hop::Previous),
            ),
        ] {
            let config: Config = toml.parse().expect("an existing config still parses");
            let default = Keys::default().hop(hop).spec().expect("on by default");
            assert_eq!(
                config.keys.hop(hop),
                Binding::Taken { default, by },
                "{toml}"
            );
            assert_eq!(config.keys.hop(hop).spec(), None);
        }
        let config: Config = "[keys]\nnav = 'alt-,'".parse().expect("parses");
        assert_eq!(config.keys.hop(Hop::Next).to_string(), "⌥.");
    }

    #[test]
    fn a_chord_on_none_does_not_clash_with_the_other_keys() {
        let config: Config = "[keys]\nprev_agent = 'none'\nnext_agent = 'none'\nnav = 'alt-,'"
            .parse()
            .expect("parses");
        assert_eq!(config.keys.nav().to_string(), "⌥,");
    }

    #[test]
    fn a_nav_key_that_may_need_a_fallback_never_shares_ctrl_close_bracket_with_a_chord() {
        for nav in ["ctrl-shift-t", "super-j", "alt-x", "f5", "ctrl-h"] {
            let err = format!("[keys]\nnav = '{nav}'\nprev_agent = 'ctrl-]'")
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
            err("[keys]\nnav = 'alt-x'\nnext_agent = 'ctrl-]'"),
            "keys.next_agent is ⌃], which also leaves AGENT while keys.nav is ⌥x"
        );
        assert_eq!(
            err("[keys]\nnav = 'f5'\nprev_agent = 'ctrl-]'"),
            "keys.prev_agent is ⌃], which also leaves AGENT while keys.nav is F5"
        );
        let config: Config = "[keys]\nnav = 'ctrl-g'\nnext_agent = 'ctrl-]'"
            .parse()
            .expect("a control nav key has no second way out");
        assert_eq!(config.keys.hop(Hop::Next).to_string(), "⌃]");
    }

    #[test]
    fn reset_restores_the_default_or_leaves_a_chord_off_when_another_key_took_it() {
        let spec: KeySpec = "alt-.".parse().expect("parses");
        let taken = Keys::default()
            .with_nav(spec)
            .with_hop(Hop::Previous, Binding::On("f5".parse().expect("parses")))
            .with_hop(Hop::Next, Binding::On("f6".parse().expect("parses")));
        assert_eq!(
            taken.reset(KeyField::Hop(Hop::Next)).hop(Hop::Next),
            Binding::Taken {
                default: spec,
                by: KeyField::Nav
            },
            "alt-. is nav's"
        );
        let free = taken.reset(KeyField::Hop(Hop::Previous));
        assert_eq!(free.hop(Hop::Previous), Keys::default().hop(Hop::Previous));
        assert_eq!(free.reset(KeyField::Nav).nav(), KeySpec::CTRL_CLOSE_BRACKET);
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
        let spec: KeySpec = "alt-,".parse().expect("parses");
        let other: KeySpec = "alt-.".parse().expect("parses");
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
        let keys = Keys::default().with_nav(spec);
        assert_eq!(keys.nav(), spec);
        assert_eq!(keys.hop(Hop::Next), Keys::default().hop(Hop::Next));
        let fields: Vec<_> = keys.bound().into_iter().map(|(field, _)| field).collect();
        assert_eq!(fields, KeyField::all().collect::<Vec<_>>());
        let off = keys.with_hop(Hop::Next, Binding::Off);
        let fields: Vec<_> = off.bound().into_iter().map(|(field, _)| field).collect();
        assert_eq!(fields, [KeyField::Nav, KeyField::Hop(Hop::Previous)]);
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
