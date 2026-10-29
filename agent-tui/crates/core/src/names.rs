use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// The name of the old meta-session; never a valid agent or session name.
pub const RESERVED: &str = "agents";
/// Zellij's unix socket path budget on macOS caps session names at 20 bytes.
pub const MAX_SESSION_LEN: usize = 20;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NameError {
    #[error("name is empty")]
    Empty,
    #[error("'{0}' is reserved")]
    Reserved(String),
    #[error("'{name}' is {len} bytes, over the {MAX_SESSION_LEN} byte limit for session names")]
    TooLong { name: String, len: usize },
    #[error(
        "'{0}' must start with a letter or digit and contain only letters, digits, '.', '-' or '_'"
    )]
    InvalidAgent(String),
    #[error("'{0}' must not start with '-' or contain whitespace, control characters or '/'")]
    InvalidSession(String),
    #[error("'{0}' is not a valid git branch name")]
    InvalidBranch(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum InvalidPrompt {
    #[error("prompt is empty")]
    Empty,
    /// Escape sequences could end a bracketed paste and inject keystrokes.
    #[error("prompt contains a control character ({0:?})")]
    Control(char),
}

macro_rules! string_newtype {
    ($name:ident) => {
        string_newtype!($name, NameError);
    };
    ($name:ident, $error:ty) => {
        impl $name {
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl AsRef<std::ffi::OsStr> for $name {
            fn as_ref(&self) -> &std::ffi::OsStr {
                self.0.as_ref()
            }
        }

        impl TryFrom<String> for $name {
            type Error = $error;

            fn try_from(s: String) -> Result<Self, $error> {
                s.parse()
            }
        }

        impl From<$name> for String {
            fn from(n: $name) -> String {
                n.0
            }
        }
    };
}

fn check_session_rules(s: &str) -> Result<(), NameError> {
    if s.is_empty() {
        return Err(NameError::Empty);
    }
    if s == RESERVED {
        return Err(NameError::Reserved(s.into()));
    }
    if s.len() > MAX_SESSION_LEN {
        return Err(NameError::TooLong {
            name: s.into(),
            len: s.len(),
        });
    }
    Ok(())
}

/// A zellij session name as zellij reports it. Session names are global, so
/// this is also the identity of an agent across the whole app.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SessionName(String);
string_newtype!(SessionName);

impl FromStr for SessionName {
    type Err = NameError;

    fn from_str(s: &str) -> Result<Self, NameError> {
        check_session_rules(s)?;
        let unsafe_char = |c: char| c.is_whitespace() || c.is_control() || c == '/';
        if s.starts_with('-') || s.chars().any(unsafe_char) {
            return Err(NameError::InvalidSession(s.into()));
        }
        Ok(Self(s.into()))
    }
}

/// The name of an agent worktree: a single safe path component that is also a
/// valid session name, so it can never escape the worktrees root.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct AgentName(String);
string_newtype!(AgentName);

impl FromStr for AgentName {
    type Err = NameError;

    fn from_str(s: &str) -> Result<Self, NameError> {
        check_session_rules(s)?;
        let mut chars = s.chars();
        let valid_first = chars.next().is_some_and(|c| c.is_ascii_alphanumeric());
        let valid_rest = chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
        if !(valid_first && valid_rest) {
            return Err(NameError::InvalidAgent(s.into()));
        }
        Ok(Self(s.into()))
    }
}

impl AgentName {
    pub fn from_branch(branch: &BranchName) -> Result<Self, NameError> {
        branch.as_str().replace('/', "-").parse()
    }

    pub fn session(&self) -> SessionName {
        SessionName(self.0.clone())
    }
}

impl From<AgentName> for SessionName {
    fn from(name: AgentName) -> Self {
        SessionName(name.0)
    }
}

/// A branch name that satisfies `git check-ref-format --branch`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct BranchName(String);
string_newtype!(BranchName);

#[allow(clippy::case_sensitive_file_extension_comparisons)]
impl FromStr for BranchName {
    type Err = NameError;

    fn from_str(s: &str) -> Result<Self, NameError> {
        let forbidden = |c: char| c.is_control() || c.is_whitespace() || "~^:?*[\\".contains(c);
        let bad_component = |c: &str| c.is_empty() || c.starts_with('.') || c.ends_with(".lock");
        let invalid = s.is_empty()
            || s == "@"
            || s.starts_with('-')
            || s.ends_with('.')
            || s.contains("..")
            || s.contains("@{")
            || s.chars().any(forbidden)
            || s.split('/').any(bad_component);
        if invalid {
            return Err(NameError::InvalidBranch(s.into()));
        }
        Ok(Self(s.into()))
    }
}

/// Text to send an agent: not blank, and no control characters other than
/// newlines and tabs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Prompt(String);
string_newtype!(Prompt, InvalidPrompt);

impl FromStr for Prompt {
    type Err = InvalidPrompt;

    fn from_str(s: &str) -> Result<Self, InvalidPrompt> {
        if s.trim().is_empty() {
            return Err(InvalidPrompt::Empty);
        }
        if let Some(c) = s
            .chars()
            .find(|c| c.is_control() && !matches!(c, '\n' | '\t'))
        {
            return Err(InvalidPrompt::Control(c));
        }
        Ok(Self(s.into()))
    }
}

impl Prompt {
    /// `self`, then a blank line, then `other`.
    #[must_use]
    pub fn then(mut self, other: &Prompt) -> Self {
        self.0.push_str("\n\n");
        self.0.push_str(&other.0);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_names_are_single_safe_components() {
        for ok in ["fix-ci", "a", "payroll.v2", "x_1", "12345678901234567890"] {
            assert!(ok.parse::<AgentName>().is_ok(), "{ok}");
        }
        for bad in [
            "", ".", "..", "../x", "/tmp/x", "-rf", ".config", "a b", "a/b", "agents",
        ] {
            assert!(bad.parse::<AgentName>().is_err(), "{bad}");
        }
        assert!(matches!(
            "123456789012345678901".parse::<AgentName>(),
            Err(NameError::TooLong { len: 21, .. })
        ));
    }

    #[test]
    fn session_names_accept_repo_basenames_but_not_flags() {
        for ok in [".config", "tanda_cli", "heucristic"] {
            assert!(ok.parse::<SessionName>().is_ok(), "{ok}");
        }
        for bad in ["", "-x", "a b", "a/b", "agents", "tab\there"] {
            assert!(bad.parse::<SessionName>().is_err(), "{bad}");
        }
    }

    #[test]
    fn length_is_counted_in_bytes() {
        let name = "é".repeat(11);
        assert!(name.parse::<SessionName>().is_err());
    }

    #[test]
    fn branch_names_follow_check_ref_format() {
        for ok in ["main", "feature/x", "fix-1.2", "user/a_b"] {
            assert!(ok.parse::<BranchName>().is_ok(), "{ok}");
        }
        for bad in [
            "", "-x", "a..b", "a b", "a/", "/a", "a.lock", "a/.b", "@", "a@{1}", "a~1", "a.",
            "a//b",
        ] {
            assert!(bad.parse::<BranchName>().is_err(), "{bad}");
        }
    }

    #[test]
    fn prompts_must_have_content() {
        assert!("fix the build".parse::<Prompt>().is_ok());
        assert_eq!("  \n ".parse::<Prompt>(), Err(InvalidPrompt::Empty));
        assert_eq!(
            "a\x1b[201~b".parse::<Prompt>(),
            Err(InvalidPrompt::Control('\x1b'))
        );
        assert!("line one\n\tline two".parse::<Prompt>().is_ok());
    }

    #[test]
    fn agent_name_from_branch_flattens_slashes() {
        let branch: BranchName = "user/fix-ci".parse().expect("parses");
        assert_eq!(
            AgentName::from_branch(&branch).expect("succeeds").as_str(),
            "user-fix-ci"
        );
    }
}
