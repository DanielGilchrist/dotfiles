//! Suggested agent names from a prompt: a linked issue's identifier and
//! title if there is one, otherwise the prompt's meaningful words.

use crate::names::{AgentName, MAX_SESSION_LEN};

/// Words that carry no meaning in a name: articles, pronouns, and the way
/// people talk to an agent ("can you figure out what's going on here").
const FILLER: &[&str] = &[
    "a",
    "about",
    "an",
    "and",
    "any",
    "are",
    "as",
    "at",
    "be",
    "been",
    "but",
    "by",
    "can",
    "could",
    "did",
    "do",
    "does",
    "figure",
    "for",
    "from",
    "going",
    "got",
    "have",
    "help",
    "here",
    "hey",
    "how",
    "i",
    "if",
    "im",
    "in",
    "into",
    "is",
    "it",
    "its",
    "just",
    "kind",
    "let",
    "lets",
    "like",
    "look",
    "me",
    "maybe",
    "my",
    "need",
    "of",
    "on",
    "or",
    "out",
    "please",
    "pls",
    "really",
    "s",
    "see",
    "should",
    "so",
    "some",
    "something",
    "sort",
    "that",
    "the",
    "there",
    "these",
    "this",
    "those",
    "to",
    "up",
    "us",
    "want",
    "was",
    "we",
    "what",
    "whats",
    "when",
    "where",
    "which",
    "why",
    "will",
    "with",
    "would",
    "wtf",
    "you",
    "your",
];

/// Where a suggested name came from, so the form can say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// A Linear or GitHub link, or a ticket ID like `ENG-123`.
    Link,
    /// The prompt's words.
    Words,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    pub name: AgentName,
    pub source: Source,
}

/// Joins `words` with `-`, stopping at the last whole word that fits.
fn fit<I: IntoIterator<Item = String>>(words: I) -> Option<AgentName> {
    let mut name = String::new();
    for word in words {
        let joined = if name.is_empty() {
            word.clone()
        } else {
            format!("{name}-{word}")
        };
        if joined.len() > MAX_SESSION_LEN {
            if name.is_empty() {
                name = word.chars().take(MAX_SESSION_LEN).collect();
            }
            break;
        }
        name = joined;
    }
    name.parse().ok()
}

fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
}

fn is_link(token: &str) -> bool {
    token.contains("://") || token.starts_with('<') || token.starts_with("www.")
}

/// `ENG-123` style ticket IDs.
fn is_ticket(token: &str) -> bool {
    let Some((team, number)) = token.split_once('-') else {
        return false;
    };
    team.len() >= 2
        && team
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        && team.starts_with(|c: char| c.is_ascii_uppercase())
        && !number.is_empty()
        && number.chars().all(|c| c.is_ascii_digit())
}

fn tokens(prompt: &str) -> impl Iterator<Item = &str> {
    prompt.split_whitespace().map(|t| {
        t.trim_matches(|c: char| matches!(c, '<' | '>' | '(' | ')' | ',' | '.' | '"' | '\''))
    })
}

/// `linear.app/<team>/issue/ENG-123/fix-the-thing` → `eng-123-fix-the-thing`.
fn linear(prompt: &str) -> Option<AgentName> {
    tokens(prompt).find_map(|token| {
        let rest = token.split("linear.app/").nth(1)?;
        let mut parts = rest.split('/').skip_while(|p| *p != "issue").skip(1);
        let id = parts.next().filter(|id| is_ticket(id))?;
        let title = parts.next().unwrap_or_default();
        fit(std::iter::once(id.to_ascii_lowercase()).chain(words(title)))
    })
}

/// `github.com/<owner>/<repo>/pull/482` → `pr-482`; issues → `issue-482`.
fn github(prompt: &str) -> Option<AgentName> {
    tokens(prompt).find_map(|token| {
        let rest = token.split("github.com/").nth(1)?;
        let parts: Vec<&str> = rest.split('/').collect();
        let (kind, number) = (parts.get(2)?, parts.get(3)?);
        let number = number
            .split(['#', '?'])
            .next()
            .filter(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))?;
        let kind = match *kind {
            "pull" => "pr",
            "issues" => "issue",
            _ => return None,
        };
        format!("{kind}-{number}").parse().ok()
    })
}

/// A bare `ENG-123` in the prompt, followed by the prompt's words.
fn ticket(prompt: &str) -> Option<AgentName> {
    let id = tokens(prompt).find(|t| is_ticket(t))?;
    fit(std::iter::once(id.to_ascii_lowercase()).chain(prose_words(&prompt.replace(id, ""))))
}

fn prose_words(prompt: &str) -> impl Iterator<Item = String> + '_ {
    prompt
        .split_whitespace()
        .filter(|t| !is_link(t))
        .flat_map(words)
        .filter(|w| !FILLER.contains(&w.as_str()))
}

pub fn suggest(prompt: &str) -> Option<Suggestion> {
    if let Some(name) = linear(prompt)
        .or_else(|| github(prompt))
        .or_else(|| ticket(prompt))
    {
        return Some(Suggestion {
            name,
            source: Source::Link,
        });
    }
    fit(prose_words(prompt)).map(|name| Suggestion {
        name,
        source: Source::Words,
    })
}

/// The question to ask a harness's model for a name for `task`.
pub fn question(task: &crate::names::Prompt) -> crate::names::Prompt {
    format!(
        "Reply with only a short kebab-case name (at most {MAX_SESSION_LEN} characters: lowercase letters, digits \
         and hyphens) for a git branch for the task below. No other text.\n\nTask:\n{task}"
    )
    .parse()
    .expect("a non-empty task makes a valid question")
}

/// Turns free text from a model into a name, or `None` if it isn't one.
pub fn from_model(reply: &str) -> Option<AgentName> {
    let line = reply.lines().map(str::trim).find(|l| !l.is_empty())?;
    let line = line.trim_matches(|c: char| matches!(c, '`' | '"' | '\'' | '*' | '.'));
    fit(words(line))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(prompt: &str) -> Option<(String, Source)> {
        suggest(prompt).map(|s| (s.name.to_string(), s.source))
    }

    #[test]
    fn linear_links_give_the_issue_and_its_title() {
        let prompt = "Can you figure out wtf is going on here https://linear.app/tanda/issue/ENG-1234/payroll-export-breaks-on-dst";
        assert_eq!(
            name(prompt),
            Some(("eng-1234-payroll".into(), Source::Link))
        );
        assert_eq!(
            name("<https://linear.app/t/issue/PAY-9>")
                .map(|n| n.0)
                .as_deref(),
            Some("pay-9")
        );
    }

    #[test]
    fn github_links_give_the_pr_or_issue_number() {
        assert_eq!(
            name("review https://github.com/acme/api/pull/482/files")
                .map(|n| n.0)
                .as_deref(),
            Some("pr-482")
        );
        assert_eq!(
            name("https://github.com/acme/api/issues/77#c3")
                .map(|n| n.0)
                .as_deref(),
            Some("issue-77")
        );
    }

    #[test]
    fn bare_ticket_ids_lead_the_name() {
        assert_eq!(
            name("ENG-42: timezone bug in export")
                .map(|n| n.0)
                .as_deref(),
            Some("eng-42-timezone-bug")
        );
    }

    #[test]
    fn conversational_filler_is_dropped() {
        assert_eq!(
            name("Can you figure out wtf is going on here <linear-link>"),
            None
        );
        assert_eq!(
            name("Can you figure out why the export breaks on timezones")
                .map(|n| n.0)
                .as_deref(),
            Some("export-breaks")
        );
        assert_eq!(
            name("Fix the flaky login test on CI")
                .map(|n| n.0)
                .as_deref(),
            Some("fix-flaky-login-test")
        );
    }

    #[test]
    fn model_replies_are_cleaned_into_names() {
        assert_eq!(
            from_model("`payroll-dst-export`\n")
                .map(|n| n.to_string())
                .as_deref(),
            Some("payroll-dst-export")
        );
        assert_eq!(
            from_model("Sure! Fix Login Flake")
                .map(|n| n.to_string())
                .as_deref(),
            Some("sure-fix-login-flake")
        );
        assert!(from_model("  \n").is_none());
    }
}
