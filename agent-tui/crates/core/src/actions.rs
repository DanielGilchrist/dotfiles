//! User-defined actions on the selected agent, declared in the config file:
//!
//! ```toml
//! [[action]]
//! key = "s"
//! label = "dev server"
//! terminal_event = { name = "agent-spawn-dev", value = "{path}" }
//!
//! [[action]]
//! key = "t"
//! label = "tests"
//! run = ["wezterm", "cli", "spawn", "--cwd", "{path}", "--", "bin/test"]
//! ```
//!
//! An action only sees the agent through the placeholders in its arguments.

use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::config::ConfigError;
use crate::names::SessionName;

/// What a placeholder stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    /// `{name}`: the agent's name.
    Name,
    /// `{path}`: the agent's checkout.
    Path,
    /// `{repo}`: the main checkout of the agent's repo.
    Repo,
}

impl Field {
    const ALL: [Self; 3] = [Self::Name, Self::Path, Self::Repo];

    fn placeholder(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Path => "path",
            Self::Repo => "repo",
        }
    }
}

impl fmt::Display for Field {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{{{}}}", self.placeholder())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Piece {
    Text(String),
    Field(Field),
}

/// Text with `{name}`, `{path}` and `{repo}` placeholders; `{{` and `}}`
/// are literal braces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template(Vec<Piece>);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TemplateError {
    #[error("unknown placeholder {{{0}}} (known: {{name}}, {{path}}, {{repo}})")]
    Unknown(String),
    #[error("unclosed '{{' (write '{{{{' for a literal brace)")]
    Unclosed,
    #[error("unmatched '}}' (write '}}}}' for a literal brace)")]
    Unmatched,
}

impl std::str::FromStr for Template {
    type Err = TemplateError;

    fn from_str(s: &str) -> Result<Self, TemplateError> {
        let mut pieces = Vec::new();
        let mut text = String::new();
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '{' if chars.peek() == Some(&'{') => {
                    chars.next();
                    text.push('{');
                }
                '}' if chars.peek() == Some(&'}') => {
                    chars.next();
                    text.push('}');
                }
                '}' => return Err(TemplateError::Unmatched),
                '{' => {
                    let mut name = String::new();
                    loop {
                        match chars.next() {
                            Some('}') => break,
                            Some(c) => name.push(c),
                            None => return Err(TemplateError::Unclosed),
                        }
                    }
                    let field = Field::ALL
                        .into_iter()
                        .find(|f| f.placeholder() == name)
                        .ok_or(TemplateError::Unknown(name))?;
                    if !text.is_empty() {
                        pieces.push(Piece::Text(std::mem::take(&mut text)));
                    }
                    pieces.push(Piece::Field(field));
                }
                c => text.push(c),
            }
        }
        if !text.is_empty() {
            pieces.push(Piece::Text(text));
        }
        Ok(Self(pieces))
    }
}

/// The selected agent, as actions see it. `path` and `repo` are `None`
/// when the agent has no checkout (or it's gone) or no known repo.
#[derive(Debug, Clone, Copy)]
pub struct Context<'a> {
    pub name: &'a SessionName,
    pub path: Option<&'a Path>,
    pub repo: Option<&'a Path>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("this agent has no {0}")]
pub struct Missing(pub Field);

impl Template {
    /// Paths go in byte for byte, so this is an `OsString`, not a `String`.
    pub fn render(&self, ctx: Context<'_>) -> Result<OsString, Missing> {
        let mut out = OsString::new();
        for piece in &self.0 {
            match piece {
                Piece::Text(text) => out.push(text),
                Piece::Field(Field::Name) => out.push(ctx.name.as_str()),
                Piece::Field(field @ Field::Path) => out.push(ctx.path.ok_or(Missing(*field))?),
                Piece::Field(field @ Field::Repo) => out.push(ctx.repo.ok_or(Missing(*field))?),
            }
        }
        Ok(out)
    }
}

/// Shows the template as written, placeholders and all.
impl fmt::Display for Template {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for piece in &self.0 {
            match piece {
                Piece::Text(text) => write!(f, "{}", text.replace('{', "{{").replace('}', "}}"))?,
                Piece::Field(field) => write!(f, "{field}")?,
            }
        }
        Ok(())
    }
}

/// A terminal user variable name (`OSC 1337 SetUserVar`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventName(String);

impl EventName {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// Start the command detached from the TUI.
    Run(Command),
    /// Suspend the TUI and run the command in its terminal.
    Interactive(Command),
    /// Ask the terminal the TUI runs in to set a user variable.
    TerminalEvent { name: EventName, value: Template },
}

/// A program, its arguments and the directory to run it in (the TUI's own
/// if `cwd` is unset).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    program: Template,
    args: Vec<Template>,
    cwd: Option<Template>,
}

impl Command {
    pub fn program(&self) -> &Template {
        &self.program
    }

    fn render(&self, ctx: Context<'_>) -> Result<CommandLine, Missing> {
        Ok(CommandLine {
            program: self.program.render(ctx)?,
            args: self
                .args
                .iter()
                .map(|a| a.render(ctx))
                .collect::<Result<_, _>>()?,
            cwd: self
                .cwd
                .as_ref()
                .map(|c| c.render(ctx).map(PathBuf::from))
                .transpose()?,
        })
    }
}

/// A command with its placeholders filled in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandLine {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub cwd: Option<PathBuf>,
}

/// The key that runs an action in the actions overlay: one printable
/// character other than the overlay's own `Esc`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Key(char);

impl Key {
    pub fn get(self) -> char {
        self.0
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// What an action is called in the overlay and status messages: one
/// non-blank line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Label(String);

impl Label {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Label {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    pub key: Key,
    pub label: Label,
    pub kind: Kind,
}

/// An action with its placeholders filled in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invocation {
    Run(CommandLine),
    Interactive(CommandLine),
    TerminalEvent { name: EventName, value: OsString },
}

impl Action {
    pub fn invoke(&self, ctx: Context<'_>) -> Result<Invocation, Missing> {
        Ok(match &self.kind {
            Kind::Run(command) => Invocation::Run(command.render(ctx)?),
            Kind::Interactive(command) => Invocation::Interactive(command.render(ctx)?),
            Kind::TerminalEvent { name, value } => Invocation::TerminalEvent {
                name: name.clone(),
                value: value.render(ctx)?,
            },
        })
    }
}

/// The actions from the config file, with distinct keys.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Actions(Vec<Action>);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawAction {
    key: String,
    label: String,
    run: Option<Vec<String>>,
    interactive: Option<Vec<String>>,
    terminal_event: Option<RawEvent>,
    cwd: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEvent {
    name: String,
    value: String,
}

fn template(s: &str) -> Result<Template, String> {
    s.parse().map_err(|e: TemplateError| format!("{s:?}: {e}"))
}

fn command(field: &str, argv: &[String], cwd: Option<&str>) -> Result<Command, String> {
    let Some((program, args)) = argv
        .split_first()
        .filter(|(program, _)| !program.trim().is_empty())
    else {
        return Err(format!("{field} needs a program first"));
    };
    Ok(Command {
        program: template(program)?,
        args: args.iter().map(|a| template(a)).collect::<Result<_, _>>()?,
        cwd: cwd.map(template).transpose()?,
    })
}

impl RawAction {
    fn parse(self) -> Result<Action, String> {
        let mut chars = self.key.chars();
        let key = match (chars.next(), chars.next()) {
            (Some(c), None) if !c.is_whitespace() && !c.is_control() => Key(c),
            _ => {
                return Err(format!(
                    "key {:?} must be one printable character",
                    self.key
                ));
            }
        };
        if self.label.trim().is_empty() || self.label.chars().any(char::is_control) {
            return Err(format!("label {:?} must be one non-blank line", self.label));
        }
        let kind = match (self.run, self.interactive, self.terminal_event, self.cwd) {
            (Some(argv), None, None, cwd) => Kind::Run(command("run", &argv, cwd.as_deref())?),
            (None, Some(argv), None, cwd) => {
                Kind::Interactive(command("interactive", &argv, cwd.as_deref())?)
            }
            (None, None, Some(_), Some(_)) => {
                return Err("cwd only applies to run and interactive".into());
            }
            (None, None, Some(event), None) => {
                let valid = !event.name.is_empty()
                    && event
                        .name
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
                if !valid {
                    return Err(format!(
                        "terminal_event name {:?} must be letters, digits, '-', '_' or '.'",
                        event.name
                    ));
                }
                Kind::TerminalEvent {
                    name: EventName(event.name),
                    value: template(&event.value)?,
                }
            }
            (None, None, None, _) => {
                return Err("needs one of run, interactive or terminal_event".into());
            }
            _ => return Err("can only have one of run, interactive or terminal_event".into()),
        };
        Ok(Action {
            key,
            label: Label(self.label),
            kind,
        })
    }
}

impl Actions {
    pub(crate) fn parse(raw: Vec<RawAction>) -> Result<Self, ConfigError> {
        let mut actions: Vec<Action> = Vec::new();
        for (i, raw) in raw.into_iter().enumerate() {
            let label = raw.label.clone();
            let invalid = |reason: String| ConfigError::Action {
                index: i + 1,
                label: label.clone(),
                reason,
            };
            let action = raw.parse().map_err(invalid)?;
            if let Some(other) = actions.iter().find(|a| a.key == action.key) {
                return Err(invalid(format!(
                    "key '{}' is also used by {}",
                    action.key, other.label
                )));
            }
            actions.push(action);
        }
        Ok(Self(actions))
    }

    pub fn all(&self) -> &[Action] {
        &self.0
    }

    pub fn get(&self, key: char) -> Option<&Action> {
        self.0.iter().find(|a| a.key.0 == key)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(name: &SessionName) -> Context<'_> {
        Context {
            name,
            path: Some(Path::new("/w/api/fix")),
            repo: Some(Path::new("/r/api")),
        }
    }

    fn fix() -> SessionName {
        "fix".parse().unwrap()
    }

    fn parse(toml: &str) -> Result<Actions, String> {
        toml.parse::<crate::config::Config>()
            .map(|c| c.actions)
            .map_err(|e| e.to_string())
    }

    #[test]
    fn placeholders_fill_each_argument() {
        let actions = parse(
            r#"
            [[action]]
            key = "t"
            label = "tests"
            run = ["bin/test", "--cwd={path}", "{name} in {repo}", "{{literal}}"]
            "#,
        )
        .unwrap();
        let invocation = actions.get('t').unwrap().invoke(ctx(&fix())).unwrap();
        assert_eq!(
            invocation,
            Invocation::Run(CommandLine {
                program: "bin/test".into(),
                args: vec![
                    "--cwd=/w/api/fix".into(),
                    "fix in /r/api".into(),
                    "{literal}".into()
                ],
                cwd: None,
            })
        );
    }

    #[test]
    fn terminal_events_carry_a_rendered_value() {
        let actions = parse(
            r#"
            [[action]]
            key = "s"
            label = "dev server"
            terminal_event = { name = "agent-spawn-dev", value = "{path}" }
            "#,
        )
        .unwrap();
        let Invocation::TerminalEvent { name, value } =
            actions.get('s').unwrap().invoke(ctx(&fix())).unwrap()
        else {
            panic!("expected a terminal event");
        };
        assert_eq!(
            (name.as_str(), value.to_str()),
            ("agent-spawn-dev", Some("/w/api/fix"))
        );
    }

    #[test]
    fn a_missing_checkout_is_refused() {
        let actions = parse("[[action]]\nkey = 't'\nlabel = 't'\nrun = ['x', '{path}']").unwrap();
        let name = fix();
        let ctx = Context {
            path: None,
            ..ctx(&name)
        };
        assert_eq!(
            actions.get('t').unwrap().invoke(ctx),
            Err(Missing(Field::Path))
        );
    }

    #[test]
    fn mistakes_are_reported_with_the_action() {
        let err = |toml: &str| parse(toml).unwrap_err();
        assert!(
            err("[[action]]\nkey = 'tt'\nlabel = 'a'\nrun = ['x']")
                .contains("one printable character")
        );
        assert!(
            err("[[action]]\nkey = 't'\nlabel = 'a'\nrun = ['x', '{cwd}']")
                .contains("unknown placeholder {cwd}")
        );
        assert!(err("[[action]]\nkey = 't'\nlabel = 'a'\nrun = ['{path']").contains("unclosed"));
        assert!(
            err("[[action]]\nkey = 't'\nlabel = 'a'")
                .contains("needs one of run, interactive or terminal_event")
        );
        assert!(err("[[action]]\nkey = 't'\nlabel = 'a'\nrun = []").contains("needs a program"));
        assert!(err("[[action]]\nkey = 't'\nlabel = 'a'\nrun = ['']").contains("needs a program"));
        assert!(
            err("[[action]]\nkey = 't'\nlabel = \"a\\nb\"\nrun = ['x']")
                .contains("one non-blank line")
        );
        assert!(err("[[action]]\nkey = 't'\nlabel = 'a'\nrun = ['x']\nterminal_event = { name = 'n', value = 'v' }")
            .contains("only have one of"));
        assert!(err("[[action]]\nkey = 't'\nlabel = 'a'\nrun = ['x']\n[[action]]\nkey = 't'\nlabel = 'b'\nrun = ['y']")
            .contains("also used by a"));
        assert!(
            err("[[action]]\nkey = 't'\nlabel = 'a'\ncommand = ['x']").contains("unknown field")
        );
    }

    #[test]
    fn interactive_commands_can_run_in_the_checkout() {
        let actions = parse(
            "[[action]]\nkey = 'g'\nlabel = 'lazygit'\ninteractive = ['lazygit']\ncwd = '{path}'",
        )
        .unwrap();
        let invocation = actions.get('g').unwrap().invoke(ctx(&fix())).unwrap();
        assert_eq!(
            invocation,
            Invocation::Interactive(CommandLine {
                program: "lazygit".into(),
                args: Vec::new(),
                cwd: Some("/w/api/fix".into())
            })
        );
        assert!(parse("[[action]]\nkey = 's'\nlabel = 's'\ncwd = '{path}'\nterminal_event = { name = 'n', value = 'v' }")
            .unwrap_err()
            .contains("cwd only applies"));
    }

    #[test]
    fn no_config_means_no_actions() {
        assert!(parse("").unwrap().is_empty());
    }
}
