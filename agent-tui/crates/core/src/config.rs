//! The user's config file: actions and the key that leaves the agent.
//!
//! ```toml
//! [keys]
//! nav = "ctrl-\\"
//! ```

use std::fmt;

use serde::Deserialize;

use crate::actions::{Actions, RawAction};

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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    pub actions: Actions,
    pub keys: Keys,
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
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    action: Vec<RawAction>,
    #[serde(default)]
    keys: RawKeys,
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
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nav_defaults_to_ctrl_backslash() {
        let config: Config = "".parse().unwrap();
        assert_eq!(config.keys.nav().to_string(), "⌃\\");
    }

    #[test]
    fn nav_can_be_changed_but_must_be_sendable() {
        let config: Config = "[keys]\nnav = 'ctrl-g'".parse().unwrap();
        assert_eq!(config.keys.nav().to_string(), "⌃G");
        let err = |toml: &str| toml.parse::<Config>().unwrap_err().to_string();
        assert!(err("[keys]\nnav = 'ctrl-space'").contains("isn't a control key"));
        assert!(err("[keys]\nnav = 'alt-x'").contains("isn't a control key"));
        assert!(err("[keys]\nnav_sustained = 'ctrl-]'").contains("unknown field"));
    }
}
