use std::fmt;
use std::num::NonZeroUsize;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// The name of the old meta-session. Never a valid agent or session name.
pub const RESERVED: &str = "agents";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NameError {
    #[error("name is empty")]
    Empty,
    #[error("'{0}' is reserved")]
    Reserved(String),
    #[error("'{name}' is {} over the session host's limit of {limit}", bytes(*.over))]
    TooLong {
        name: String,
        over: NonZeroUsize,
        limit: usize,
    },
    #[error(
        "'{0}' must start with a letter or digit and contain only letters, digits, '.', '-' or '_'"
    )]
    InvalidAgent(String),
    #[error("'{0}' must not start with '-' or contain whitespace, control characters or '/'")]
    InvalidSession(String),
    #[error("'{0}' is not a valid git branch name")]
    InvalidBranch(String),
    #[error("not a pull request link: github.com/<owner>/<repo>/pull/<number>")]
    InvalidPullRequest,
}

fn bytes(count: NonZeroUsize) -> String {
    match count.get() {
        1 => "1 byte".to_string(),
        n => format!("{n} bytes"),
    }
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
    Ok(())
}

/// The longest session name a session host accepts, in bytes. Each
/// `SessionHost` adapter declares its own, so names parse without one and
/// are admitted against it where a session is made for the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NameLimit(NonZeroUsize);

/// A session name within the host's limit, which `SessionHost::create` needs.
/// Only [`NameLimit::admit`] makes one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostName {
    name: SessionName,
    limit: NameLimit,
}

impl HostName {
    pub fn session(&self) -> &SessionName {
        &self.name
    }

    pub fn into_session(self) -> SessionName {
        self.name
    }

    /// Whether a host with `limit` takes it, which a host checks because
    /// the name may have been admitted under another host's limit.
    pub fn within(&self, limit: NameLimit) -> Result<(), NameError> {
        if self.limit.bytes() <= limit.bytes() {
            return Ok(());
        }
        limit.admit(self.name.clone()).map(drop)
    }
}

impl NameLimit {
    pub const fn new(bytes: NonZeroUsize) -> Self {
        Self(bytes)
    }

    pub fn bytes(self) -> usize {
        self.0.get()
    }

    pub fn admit(self, name: SessionName) -> Result<HostName, NameError> {
        match NonZeroUsize::new(name.as_str().len().saturating_sub(self.bytes())) {
            Some(over) => Err(NameError::TooLong {
                name: name.into(),
                over,
                limit: self.bytes(),
            }),
            None => Ok(HostName { name, limit: self }),
        }
    }

    pub fn agent(self, text: &str) -> Result<AgentName, NameError> {
        let name = text.parse::<AgentName>()?;
        self.admit(name.session())?;
        Ok(name)
    }
}

/// A session name as the session host reports it. Session names are global,
/// so this is also the identity of an agent across the whole app.
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
    /// The branch's words, lowercased and joined with `-`, cut at the last
    /// whole word that fits `limit`.
    pub fn from_branch(branch: &BranchName, limit: NameLimit) -> Result<Self, NameError> {
        crate::naming::fit(crate::naming::words(branch.as_str()), limit)
            .ok_or_else(|| NameError::InvalidAgent(branch.to_string()))
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

impl FromStr for BranchName {
    type Err = NameError;

    fn from_str(s: &str) -> Result<Self, NameError> {
        // git's own rules: only ASCII controls and the space are refused, so
        // names with other Unicode whitespace or C1 controls are real branches.
        let forbidden = |c: char| c < ' ' || c == '\x7f' || c == ' ' || "~^:?*[\\".contains(c);
        let bad_component = |c: &str| {
            c.is_empty()
                || c.starts_with('.')
                || c.rsplit_once('.')
                    .is_some_and(|(_, extension)| extension == "lock")
        };
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

/// The first control character in `text` other than a newline or tab, which
/// could end a bracketed paste early and run the rest as keystrokes.
#[must_use]
pub fn first_control(text: &str) -> Option<char> {
    text.chars()
        .find(|c| c.is_control() && !matches!(c, '\n' | '\t'))
}

/// What [`clean`] does with carriage returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CarriageReturn {
    Keep,
    Drop,
    /// `\r\n` and a lone `\r` both become `\n`.
    Newline,
}

/// `text` without its control characters other than newlines and tabs:
/// each is removed, or becomes `replacement` when there is one. Carriage
/// returns follow `cr`.
#[must_use]
pub fn clean(text: &str, cr: CarriageReturn, replacement: Option<char>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => match cr {
                CarriageReturn::Keep => out.push(c),
                CarriageReturn::Drop => {}
                CarriageReturn::Newline => {
                    chars.next_if_eq(&'\n');
                    out.push('\n');
                }
            },
            '\n' | '\t' => out.push(c),
            c if c.is_control() => out.extend(replacement),
            c => out.push(c),
        }
    }
    out
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
        if let Some(c) = first_control(s) {
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

/// A pull request on GitHub, by its link.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PullRequest {
    pub owner: String,
    pub repo: String,
    pub number: u64,
}

impl PullRequest {
    /// `owner/repo`, as `gh --repo` takes it.
    pub fn slug(&self) -> String {
        format!("{}/{}", self.owner, self.repo)
    }
}

impl fmt::Display for PullRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}#{}", self.slug(), self.number)
    }
}

/// What to check out for an agent: a branch, or the branch behind a pull
/// request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Checkout {
    Branch(BranchName),
    PullRequest(PullRequest),
}

impl FromStr for Checkout {
    type Err = NameError;

    /// A GitHub link is a pull request (anything else under github.com is
    /// an error, not a branch). The rest is a branch name.
    fn from_str(s: &str) -> Result<Self, NameError> {
        let s = s.trim().trim_matches(|c: char| matches!(c, '<' | '>'));
        let Some(rest) = s.split("github.com/").nth(1) else {
            return s.parse().map(Self::Branch);
        };
        let invalid = || NameError::InvalidPullRequest;
        let mut parts = rest.split('/');
        let owner = parts.next().filter(|p| !p.is_empty()).ok_or_else(invalid)?;
        let repo = parts.next().filter(|p| !p.is_empty()).ok_or_else(invalid)?;
        if parts.next() != Some("pull") {
            return Err(invalid());
        }
        let number = parts
            .next()
            .and_then(|n| n.split(['#', '?']).next())
            .and_then(|n| n.parse().ok())
            .ok_or_else(invalid)?;
        Ok(Self::PullRequest(PullRequest {
            owner: owner.to_string(),
            repo: repo.to_string(),
            number,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::NAME_LIMIT;

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
            NAME_LIMIT.agent("123456789012345678901"),
            Err(NameError::TooLong {
                over,
                limit: 20,
                ..
            }) if over.get() == 1
        ));
        assert!("123456789012345678901".parse::<AgentName>().is_ok());
    }

    #[test]
    fn a_name_at_the_limit_fits_and_one_byte_over_says_so_in_the_singular() {
        assert!(NAME_LIMIT.agent("12345678901234567890").is_ok());
        let over = NAME_LIMIT.agent("123456789012345678901").expect_err("over");
        assert_eq!(
            over.to_string(),
            "'123456789012345678901' is 1 byte over the session host's limit of 20"
        );
        let more = NAME_LIMIT
            .agent("12345678901234567890123")
            .expect_err("over");
        assert!(more.to_string().contains("is 3 bytes over"));
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
        assert!(NAME_LIMIT.admit(name.parse().expect("parses")).is_err());
        assert!(
            NAME_LIMIT
                .admit("é".repeat(10).parse().expect("parses"))
                .is_ok()
        );
    }

    #[test]
    fn branch_names_follow_check_ref_format() {
        for ok in [
            "main",
            "feature/x",
            "fix-1.2",
            "user/a_b",
            "a\u{a0}b",
            "a\u{3000}b",
            "a\u{85}b",
            "a\u{2028}b",
        ] {
            assert!(ok.parse::<BranchName>().is_ok(), "{ok}");
        }
        for bad in [
            "", "-x", "a..b", "a b", "a/", "/a", "a.lock", "a/.b", "@", "a@{1}", "a~1", "a.",
            "a//b", "a\tb", "a\x7fb", "a\nb",
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
    fn agent_name_from_branch_flattens_slashes_and_fits() {
        let branch: BranchName = "user/fix-ci".parse().expect("parses");
        assert_eq!(
            AgentName::from_branch(&branch, NAME_LIMIT)
                .expect("succeeds")
                .as_str(),
            "user-fix-ci"
        );
        let long: BranchName = "axel-chargebee-webhook-handler-classes"
            .parse()
            .expect("parses");
        assert_eq!(
            AgentName::from_branch(&long, NAME_LIMIT)
                .expect("succeeds")
                .as_str(),
            "axel-chargebee"
        );
        let cased: BranchName = "Feature/Add_Thing".parse().expect("parses");
        assert_eq!(
            AgentName::from_branch(&cased, NAME_LIMIT)
                .expect("succeeds")
                .as_str(),
            "feature-add-thing"
        );
    }

    #[test]
    fn a_checkout_is_a_pull_request_link_or_a_branch() {
        let pr: Checkout = "https://github.com/acme/api/pull/482/files"
            .parse()
            .expect("parses");
        assert_eq!(
            pr,
            Checkout::PullRequest(PullRequest {
                owner: "acme".into(),
                repo: "api".into(),
                number: 482,
            })
        );
        let branch: Checkout = "user/fix-ci".parse().expect("parses");
        assert_eq!(
            branch,
            Checkout::Branch("user/fix-ci".parse().expect("parses"))
        );
        assert!(
            "https://github.com/acme/api/issues/5"
                .parse::<Checkout>()
                .is_err()
        );
        assert!("https://github.com/acme/api".parse::<Checkout>().is_err());
        assert!("".parse::<Checkout>().is_err());
    }

    #[test]
    fn cleaning_text_handles_carriage_returns_and_other_controls_apart() {
        use CarriageReturn::{Drop, Keep, Newline};
        assert_eq!(
            clean("a\r\nb\rc\x1bd\te", Newline, Some('?')),
            "a\nb\nc?d\te"
        );
        assert_eq!(clean("a\r\nb\x1b", Keep, None), "a\r\nb");
        assert_eq!(clean("a\r\nb\x07", Drop, None), "a\nb");
        assert_eq!(first_control("a\r"), Some('\r'));
        assert_eq!(first_control("a\n\tb"), None);
    }
}
