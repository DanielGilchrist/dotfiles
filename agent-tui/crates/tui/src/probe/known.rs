//! Keys other programs commonly use, for `agent keys` to warn about.
//!
//! Every claim here is from memory of those programs' defaults and has not
//! been checked against their documentation. A claim that is wrong costs
//! the user a warning they can ignore, so the table holds only what changes
//! a choice and the output says where its claims come from.

use std::sync::LazyLock;

use agent_core::keyspec::KeySpec;

pub const ATTRIBUTION: &str = "from memory, unchecked";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Program {
    Readline,
    Terminal,
    Zellij,
    Agents,
}

impl Program {
    pub fn label(self) -> &'static str {
        match self {
            Self::Readline => "readline and shells",
            Self::Terminal => "the terminal driver",
            Self::Zellij => "zellij's default keys",
            Self::Agents => "keys agents commonly use",
        }
    }
}

/// How likely taking the key is to cost the user something.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Weight {
    Common,
    Rare,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Claim {
    key: &'static str,
    pub program: Program,
    pub weight: Weight,
    pub uses_it_for: &'static str,
}

const fn claim(
    key: &'static str,
    program: Program,
    weight: Weight,
    uses_it_for: &'static str,
) -> Claim {
    Claim {
        key,
        program,
        weight,
        uses_it_for,
    }
}

use Program::{Agents, Readline, Terminal, Zellij};
use Weight::{Common, Rare};

const CLAIMS: &[Claim] = &[
    claim("ctrl-a", Readline, Common, "start of line"),
    claim("ctrl-b", Readline, Common, "back a character"),
    claim("ctrl-e", Readline, Common, "end of line"),
    claim("ctrl-f", Readline, Common, "forward a character"),
    claim("ctrl-g", Readline, Rare, "abort"),
    claim("ctrl-h", Readline, Common, "backspace"),
    claim("ctrl-k", Readline, Common, "kill to end of line"),
    claim("ctrl-l", Readline, Common, "clear screen"),
    claim("ctrl-n", Readline, Common, "next history entry"),
    claim("ctrl-o", Readline, Rare, "run line and fetch the next"),
    claim("ctrl-p", Readline, Common, "previous history entry"),
    claim("ctrl-r", Readline, Common, "reverse history search"),
    claim("ctrl-t", Readline, Rare, "transpose characters"),
    claim("ctrl-u", Readline, Common, "kill to start of line"),
    claim("ctrl-v", Readline, Rare, "insert the next key literally"),
    claim("ctrl-w", Readline, Common, "kill a word"),
    claim("ctrl-x", Readline, Rare, "prefix of two-key commands"),
    claim("ctrl-y", Readline, Rare, "paste what was killed"),
    claim("ctrl-]", Readline, Rare, "search for a character"),
    claim("ctrl-_", Readline, Rare, "undo"),
    claim("ctrl-space", Readline, Rare, "set the mark"),
    claim("alt-b", Readline, Common, "back a word"),
    claim("alt-d", Readline, Common, "kill the next word"),
    claim("alt-f", Readline, Common, "forward a word"),
    claim(
        "alt-.",
        Readline,
        Common,
        "paste the last argument (the default next_agent takes it on purpose)",
    ),
    claim("ctrl-c", Terminal, Common, "interrupt"),
    claim("ctrl-d", Terminal, Common, "end of input"),
    claim("ctrl-z", Terminal, Common, "suspend"),
    claim("ctrl-q", Terminal, Rare, "resume output"),
    claim("ctrl-s", Terminal, Rare, "stop output"),
    claim(
        "ctrl-\\",
        Terminal,
        Rare,
        "quit signal, in a terminal that isn't in raw mode",
    ),
    claim("ctrl-q", Zellij, Common, "quit"),
    claim("ctrl-g", Zellij, Common, "lock"),
    claim("ctrl-p", Zellij, Common, "pane mode"),
    claim("ctrl-t", Zellij, Common, "tab mode"),
    claim("ctrl-n", Zellij, Common, "resize mode"),
    claim("ctrl-s", Zellij, Common, "scroll mode"),
    claim("ctrl-o", Zellij, Common, "session mode"),
    claim("ctrl-h", Zellij, Common, "move mode"),
    claim("ctrl-b", Zellij, Common, "tmux mode"),
    claim("alt-h", Zellij, Common, "focus left"),
    claim("alt-j", Zellij, Common, "focus down"),
    claim("alt-k", Zellij, Common, "focus up"),
    claim("alt-l", Zellij, Common, "focus right"),
    claim("alt-n", Zellij, Common, "new pane"),
    claim("alt-f", Zellij, Common, "toggle floating panes"),
    claim("alt-i", Zellij, Common, "move tab left"),
    claim("alt-o", Zellij, Common, "move tab right"),
    claim("alt-[", Zellij, Common, "previous layout"),
    claim("alt-]", Zellij, Common, "next layout"),
    claim("alt-+", Zellij, Common, "resize up"),
    claim("alt--", Zellij, Common, "resize down"),
    claim("alt-=", Zellij, Common, "resize up"),
    claim("ctrl-b", Agents, Common, "run in the background"),
    claim("ctrl-g", Agents, Common, "edit the prompt in $EDITOR"),
    claim("ctrl-o", Agents, Common, "toggle the transcript"),
    claim("ctrl-s", Agents, Common, "stash the prompt"),
    claim("ctrl-t", Agents, Common, "toggle the task list"),
    claim("ctrl-v", Agents, Common, "paste an image"),
    claim("alt-m", Agents, Common, "cycle the mode"),
    claim("alt-p", Agents, Common, "switch model"),
    claim("alt-t", Agents, Common, "toggle thinking"),
];

/// A key and every claim on it, never none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collision {
    first: &'static Claim,
    rest: Vec<&'static Claim>,
}

impl Collision {
    pub fn claims(&self) -> impl Iterator<Item = &'static Claim> {
        std::iter::once(self.first).chain(self.rest.iter().copied())
    }

    /// Whether any claim is one a user is likely to hit.
    pub fn is_common(&self) -> bool {
        self.claims().any(|claim| claim.weight == Weight::Common)
    }
}

static PARSED: LazyLock<Vec<(KeySpec, &'static Claim)>> = LazyLock::new(|| {
    CLAIMS
        .iter()
        .filter_map(|claim| Some((claim.key.parse().ok()?, claim)))
        .collect()
});

pub fn claims_on(spec: KeySpec) -> Option<Collision> {
    let mut claims = PARSED
        .iter()
        .filter(|(key, _)| *key == spec)
        .map(|(_, claim)| *claim);
    let first = claims.next()?;
    Some(Collision {
        first,
        rest: claims.collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(text: &str) -> KeySpec {
        text.parse().expect("parses")
    }

    #[test]
    fn every_claimed_key_is_one_the_config_can_name() {
        assert_eq!(PARSED.len(), CLAIMS.len());
    }

    #[test]
    fn a_key_several_programs_use_names_them_all() {
        let collision = claims_on(spec("ctrl-g")).expect("claimed");
        let programs: Vec<_> = collision.claims().map(|claim| claim.program).collect();
        assert_eq!(programs, [Readline, Zellij, Agents]);
    }

    #[test]
    fn a_key_nobody_is_known_to_use_has_no_collision() {
        assert_eq!(claims_on(spec("f12")), None);
        assert_eq!(claims_on(spec("alt-,")), None);
    }

    #[test]
    fn a_key_only_rarely_used_is_not_common_and_one_with_a_common_use_is() {
        assert!(!claims_on(spec("ctrl-]")).expect("claimed").is_common());
        assert!(claims_on(spec("alt-.")).expect("claimed").is_common());
        assert!(claims_on(spec("ctrl-g")).expect("claimed").is_common());
    }
}
