use std::time::Duration;

use agent_core::activity::{BackgroundCommands, CommandLine, Running, TaskId};

/// What a command is called in a row: text that is safe to draw and has
/// something visible in it. It is only built from such text, so no row or
/// card can show an empty name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShownName(String);

impl ShownName {
    /// The command's first line that shows once cleaned, else its id as it
    /// shows once cleaned, else nothing.
    pub fn of(id: &TaskId, command: &CommandLine) -> Option<Self> {
        command_lines(command)
            .into_iter()
            .next()
            .or_else(|| printable_id(id))
            .map(Self)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// What `command` says as lines that are safe to draw, none of them blank.
/// Core's rule on what shows is applied to the cleaned text, since what is
/// drawn is the cleaned text.
pub fn command_lines(command: &CommandLine) -> Vec<String> {
    command
        .visible_lines()
        .map(|line| crate::printable(line, crate::Lines::Flatten))
        .map(|line| line.trim().to_string())
        .filter(|line| agent_core::visible::has_visible(line))
        .collect()
}

/// `id` as text that is safe to draw, when anything of it shows.
pub fn printable_id(id: &TaskId) -> Option<String> {
    let cleaned = crate::printable(id.as_str(), crate::Lines::Flatten);
    let cleaned = cleaned.trim();
    agent_core::visible::has_visible(cleaned).then(|| cleaned.to_string())
}

/// `text` cut to `width` display columns, keeping its start and ending in
/// `…` when something was cut.
pub fn truncate_end(text: &str, width: usize) -> String {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
    if text.width() <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = c.width().unwrap_or(0);
        if used + w + 1 > width {
            break;
        }
        used += w;
        out.push(c);
    }
    out.push('…');
    out
}

/// Why a restart that was agreed was skipped: what `unseen` would stop that
/// wasn't shown, naming any new commands.
pub fn skipped_because(unseen: &agent_core::ops::Stops) -> String {
    let because = unseen.became();
    match unseen
        .commands()
        .and_then(|commands| CommandsShown::of(commands).list())
    {
        Some(names) => format!("{because}: {names}"),
        None => because,
    }
}

/// A background command as a list of them shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShownCommand {
    pub name: ShownName,
    pub running_at_least: Duration,
}

impl ShownCommand {
    /// `None` when nothing about the command can be drawn as its name.
    pub fn of(running: Running<'_>) -> Option<Self> {
        Some(Self {
            name: ShownName::of(running.id, running.command)?,
            running_at_least: running.running_at_least,
        })
    }

    /// The name as a list shows it, cut to [`CommandsShown::NAME_COLUMNS`].
    pub fn label(&self) -> String {
        truncate_end(self.name.as_str(), CommandsShown::NAME_COLUMNS)
    }
}

/// What a list that names only some commands says about the others.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Remainder {
    /// This many more, after the ones named.
    More(usize),
    /// This many, none of which could be named.
    Unnamed(usize),
}

/// The first few of an agent's background commands as names, and the rest
/// counted. A command that prints nothing once cleaned has no name to show
/// but is still one of them, so a count made from `named` alone would
/// understate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandsShown {
    pub named: Vec<ShownCommand>,
    unshown: usize,
}

impl CommandsShown {
    /// How many commands are named, wherever a list names only some.
    pub const NAMED: usize = 3;
    /// The width a name is cut to.
    pub const NAME_COLUMNS: usize = 40;

    /// The first [`NAMED`](Self::NAMED) of `commands`, longest running
    /// first, that can be named.
    pub fn of(commands: &BackgroundCommands) -> Self {
        let named: Vec<_> = commands
            .iter()
            .filter_map(ShownCommand::of)
            .take(Self::NAMED)
            .collect();
        Self {
            unshown: commands.count().get() - named.len(),
            named,
        }
    }

    /// The names, then `and N more` for the others the list names, as
    /// `cargo test, npm run dev, and 2 more`. Nothing when none is named.
    pub fn list(&self) -> Option<String> {
        if self.named.is_empty() {
            return None;
        }
        let mut names: Vec<String> = self.named.iter().map(ShownCommand::label).collect();
        if let Some(Remainder::More(count)) = self.remainder() {
            names.push(format!("and {count} more"));
        }
        Some(names.join(", "))
    }

    pub fn remainder(&self) -> Option<Remainder> {
        match (self.unshown, self.named.is_empty()) {
            (0, _) => None,
            (count, true) => Some(Remainder::Unnamed(count)),
            (count, false) => Some(Remainder::More(count)),
        }
    }
}

#[cfg(test)]
mod tests {
    use agent_core::activity::testing::with_commands;
    use agent_core::activity::{Activity, BackgroundCommands, ListedCommand};
    use std::time::{Duration, SystemTime};

    use super::*;

    fn listed(id: &str, line: &str) -> ListedCommand {
        ListedCommand {
            id: id.parse().expect("visible"),
            command: line.parse().expect("visible"),
            description: None,
        }
    }

    fn shown(commands: &[ListedCommand]) -> CommandsShown {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let activity = with_commands(
            Activity::fresh(now),
            commands,
            Duration::from_secs(240),
            now,
        );
        CommandsShown::of(activity.commands().expect("listed"))
    }

    #[test]
    fn commands_within_the_limit_are_all_named_with_nothing_left_over() {
        let shown = shown(&[listed("b1", "one"), listed("b2", "two")]);
        assert_eq!(shown.named.len(), 2);
        assert_eq!(shown.remainder(), None);
    }

    #[test]
    fn commands_past_the_limit_are_counted_as_more() {
        let commands: Vec<_> = (1..=5)
            .map(|n| listed(&format!("b{n}"), &format!("cmd{n}")))
            .collect();
        let shown = shown(&commands);
        assert_eq!(shown.named.len(), CommandsShown::NAMED);
        assert_eq!(shown.remainder(), Some(Remainder::More(2)));
    }

    #[test]
    fn a_command_with_nothing_to_show_is_counted_beside_the_named_ones() {
        let shown = shown(&[listed("b1", "cargo test"), listed("\u{1b}[m", "\u{1b}[0m")]);
        assert_eq!(shown.named.len(), 1);
        assert_eq!(shown.remainder(), Some(Remainder::More(1)));
    }

    #[test]
    fn commands_that_all_print_nothing_are_counted_as_unnamed() {
        let shown = shown(&[
            listed("\u{1b}[m", "\u{1b}[0m"),
            listed("\u{200b}\u{1b}[m", "\u{1b}[1m"),
        ]);
        assert_eq!(shown.named.len(), 0);
        assert_eq!(shown.remainder(), Some(Remainder::Unnamed(2)));
    }

    #[test]
    fn a_long_name_is_cut_to_the_shared_width() {
        let long = format!("cargo {}", "x".repeat(100));
        let shown = shown(&[listed("b1", &long)]);
        let label = shown.named[0].label();
        assert_eq!(label.chars().count(), CommandsShown::NAME_COLUMNS);
        assert!(label.ends_with('…'));
    }

    fn commands_of(commands: &[ListedCommand]) -> BackgroundCommands {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        with_commands(
            Activity::fresh(now),
            commands,
            Duration::from_secs(240),
            now,
        )
        .commands()
        .expect("listed")
        .clone()
    }

    #[test]
    fn a_skip_names_the_new_commands() {
        use agent_core::activity::Busy;
        use agent_core::ops::{Interrupts, Stops};
        let new = commands_of(&[listed("b2", "npm run dev")]);
        assert_eq!(
            skipped_because(&Stops::Commands(new.clone())),
            "started running 1 background command: npm run dev"
        );
        assert_eq!(
            skipped_because(&Stops::Both(Interrupts::Session(Busy::Working), new)),
            "started working and started running 1 background command: npm run dev"
        );
        assert_eq!(
            skipped_because(&Stops::Activity(Interrupts::Session(Busy::Working))),
            "started working"
        );
    }
}
