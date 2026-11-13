//! The user's config file: actions, the key that leaves the agent, where
//! repos can be found and the colours.
//!
//! ```toml
//! [keys]
//! nav = "ctrl-]"
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
use crate::keyspec::{InvalidKeySpec, KeySpec};
use crate::paths::{self, Typed};
use crate::theme::{RawTheme, Theme, ThemeError};

/// A key of the `[keys]` table. A new field is added here, to `ALL`, to
/// `Keys::spec` and to `Keys::with`, and each match fails to compile until it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyField {
    Nav,
}

impl KeyField {
    pub const ALL: [Self; 1] = [Self::Nav];
}

impl fmt::Display for KeyField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Nav => "nav",
        })
    }
}

/// The keys the TUI reserves for itself. Every default is a key any
/// terminal can send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Keys {
    nav: KeySpec,
}

impl Default for Keys {
    fn default() -> Self {
        Self {
            nav: KeySpec::CTRL_CLOSE_BRACKET,
        }
    }
}

impl Keys {
    pub fn spec(&self, field: KeyField) -> KeySpec {
        match field {
            KeyField::Nav => self.nav,
        }
    }

    #[must_use]
    pub fn with(self, field: KeyField, spec: KeySpec) -> Self {
        match field {
            KeyField::Nav => Self { nav: spec },
        }
    }

    pub fn nav(&self) -> KeySpec {
        self.spec(KeyField::Nav)
    }

    pub fn bindings(&self) -> Vec<(KeyField, KeySpec)> {
        KeyField::ALL
            .into_iter()
            .map(|field| (field, self.spec(field)))
            .collect()
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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    pub actions: Actions,
    pub keys: Keys,
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
    #[serde(default)]
    keys: RawKeys,
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
}

impl RawKeys {
    fn parse(self) -> Result<Keys, ConfigError> {
        let nav = match self.nav {
            Some(raw) => raw.parse().map_err(|source| ConfigError::Key {
                field: KeyField::Nav,
                source,
            })?,
            None => Keys::default().nav,
        };
        let keys = Keys { nav };
        ensure_distinct(&keys.bindings())
            .map_err(|(first, second)| ConfigError::DuplicateKey { first, second })?;
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
        Ok(Self {
            actions: Actions::parse(raw.action)?,
            keys: raw.keys.parse()?,
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
        for field in KeyField::ALL {
            assert!(!defaults.spec(field).needs_kitty(), "{field}");
        }
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
    fn with_replaces_only_its_field_and_bindings_lists_every_field() {
        let spec: KeySpec = "f5".parse().expect("parses");
        let keys = Keys::default().with(KeyField::Nav, spec);
        assert_eq!(keys.spec(KeyField::Nav), spec);
        let fields: Vec<_> = keys
            .bindings()
            .into_iter()
            .map(|(field, _)| field)
            .collect();
        assert_eq!(fields, KeyField::ALL);
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
