//! The user's config file: actions, the key that leaves the agent, where
//! repos can be found and the colours.
//!
//! ```toml
//! [keys]
//! nav = "ctrl-\\"
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
use crate::paths::{self, Typed};
use crate::theme::{RawTheme, Theme, ThemeError};

/// A control chord a terminal can send on its own: Ctrl with a letter or
/// one of `\ ] ^ _`. Everything else would reach the TUI as plain text or
/// not at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlKey(char);

impl ControlKey {
    pub const BACKSLASH: Self = Self('\\');

    pub fn char(self) -> char {
        self.0
    }

    /// How legacy terminals report it: `Ctrl+\ ] ^ _` arrive as the bytes
    /// 0x1C-0x1F, which decoders read as `Ctrl+4`-`Ctrl+7`.
    pub fn legacy_digit(self) -> Option<char> {
        match self.0 {
            '\\' => Some('4'),
            ']' => Some('5'),
            '^' => Some('6'),
            '_' => Some('7'),
            _ => None,
        }
    }
}

impl fmt::Display for ControlKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "⌃{}", self.0.to_ascii_uppercase())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0:?} isn't a control key (use ctrl- and a letter or one of \\ ] ^ _)")]
pub struct InvalidControlKey(String);

impl std::str::FromStr for ControlKey {
    type Err = InvalidControlKey;

    fn from_str(s: &str) -> Result<Self, InvalidControlKey> {
        let invalid = || InvalidControlKey(s.to_string());
        let rest = s.strip_prefix("ctrl-").ok_or_else(invalid)?;
        let mut chars = rest.chars();
        match (chars.next(), chars.next()) {
            (Some(c @ ('a'..='z' | '\\' | ']' | '^' | '_')), None) => Ok(Self(c)),
            _ => Err(invalid()),
        }
    }
}

/// The key that leaves the agent for NAV.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Keys {
    nav: ControlKey,
}

impl Default for Keys {
    fn default() -> Self {
        Self {
            nav: ControlKey::BACKSLASH,
        }
    }
}

impl Keys {
    pub fn nav(&self) -> ControlKey {
        self.nav
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
        field: &'static str,
        source: InvalidControlKey,
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
        let mut dirs = Vec::new();
        for raw in &self.dirs {
            let typed = raw
                .starts_with('~')
                .then(|| paths::typed_path(raw, Path::new("/")))
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
                field: "nav",
                source,
            })?,
            None => Keys::default().nav,
        };
        Ok(Keys { nav })
    }
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
    fn nav_defaults_to_ctrl_backslash() {
        let config: Config = "".parse().expect("parses");
        assert_eq!(config.keys.nav().to_string(), "⌃\\");
    }

    #[test]
    fn nav_can_be_changed_but_must_be_sendable() {
        let config: Config = "[keys]\nnav = 'ctrl-g'".parse().expect("parses");
        assert_eq!(config.keys.nav().to_string(), "⌃G");
        let err = |toml: &str| toml.parse::<Config>().expect_err("fails").to_string();
        assert!(err("[keys]\nnav = 'ctrl-space'").contains("isn't a control key"));
        assert!(err("[keys]\nnav = 'alt-x'").contains("isn't a control key"));
        assert!(err("[keys]\nnav_sustained = 'ctrl-]'").contains("unknown field"));
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
