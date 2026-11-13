//! Shell completion: the static script clap generates, plus glue that asks
//! `agent complete` for the words clap can't know (agent names, branches,
//! repos, harness ids).

use agent_core::complete::{self, Completion};
use agent_core::config::Sources;
use agent_core::ops::Services;
use clap::{Command, ValueEnum};
use clap_complete::Shell;

/// What `agent complete` lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CompleteKind {
    Agents,
    Branches,
    Repos,
    Harnesses,
}

impl CompleteKind {
    /// The subcommands whose first positional argument is this kind.
    fn first_positional_of(self) -> &'static [&'static str] {
        match self {
            Self::Agents => &["attach", "rm", "restart", "pin", "unpin", "dump", "prompt"],
            Self::Branches => &["checkout", "co"],
            Self::Repos | Self::Harnesses => &[],
        }
    }

    /// The long flag whose value is this kind.
    fn value_of(self) -> Option<&'static str> {
        match self {
            Self::Repos => Some("repo"),
            Self::Harnesses => Some("harness"),
            Self::Agents | Self::Branches => None,
        }
    }

    fn word(self) -> String {
        self.to_possible_value()
            .expect("no variant is skipped")
            .get_name()
            .to_string()
    }

    pub fn list(self, svc: Services<'_>, sources: &Sources, dir: &std::path::Path) -> Completion {
        match self {
            Self::Agents => complete::agents(svc),
            Self::Branches => complete::branches(svc, dir),
            Self::Repos => complete::repos(svc, sources),
            Self::Harnesses => complete::harnesses(svc),
        }
    }
}

/// The shells that complete dynamically. Elvish and PowerShell keep the
/// static script.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Glue {
    Fish,
    Bash,
    Zsh,
}

impl Glue {
    pub fn of(shell: Shell) -> Option<Self> {
        match shell {
            Shell::Fish => Some(Self::Fish),
            Shell::Bash => Some(Self::Bash),
            Shell::Zsh => Some(Self::Zsh),
            _ => None,
        }
    }

    pub fn render(self, grammar: &Grammar) -> String {
        match self {
            Self::Fish => fish(grammar),
            Self::Bash => bash(grammar),
            Self::Zsh => zsh(grammar),
        }
    }
}

/// What the glue has to know about the command line's grammar, read from
/// clap's own definition so a new flag can't be missed.
#[derive(Debug, PartialEq, Eq)]
pub struct Grammar {
    /// Every flag that takes a value, as `(long, short)`.
    value_flags: Vec<(String, Option<char>)>,
    /// The subcommands that take `--repo`.
    repo_subcommands: Vec<String>,
}

impl Grammar {
    pub fn of(cli: &Command) -> Self {
        let mut value_flags: Vec<(String, Option<char>)> = Vec::new();
        let mut repo_subcommands = Vec::new();
        let commands = std::iter::once(cli).chain(cli.get_subcommands());
        for command in commands {
            for arg in command
                .get_arguments()
                .filter(|a| a.get_action().takes_values())
            {
                let Some(long) = arg.get_long() else { continue };
                if !value_flags.iter().any(|(known, _)| known == long) {
                    value_flags.push((long.to_string(), arg.get_short()));
                }
                if long == "repo" && command.get_name() != cli.get_name() {
                    repo_subcommands.push(command.get_name().to_string());
                    repo_subcommands.extend(command.get_all_aliases().map(String::from));
                }
            }
        }
        Self {
            value_flags,
            repo_subcommands,
        }
    }

    fn fish_patterns(&self) -> String {
        self.flag_spellings()
            .map(|spelling| format!("'{spelling}'"))
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn flag_spellings(&self) -> impl Iterator<Item = String> + '_ {
        self.value_flags.iter().flat_map(|(long, short)| {
            let short = short.map(|s| format!("-{s}"));
            std::iter::once(format!("--{long}")).chain(short)
        })
    }

    fn case_flags(&self) -> String {
        self.value_flags
            .iter()
            .flat_map(|(long, short)| {
                let short = short.map(|s| format!("-{s}"));
                std::iter::once(format!("--{long}")).chain(short)
            })
            .collect::<Vec<_>>()
            .join("|")
    }
}

fn kinds() -> impl Iterator<Item = CompleteKind> {
    CompleteKind::value_variants().iter().copied()
}

fn fish(grammar: &Grammar) -> String {
    let helper = format!(
        r#"
function __fish_agent_words
    set -l positionals 0
    set -l value 0
    set -l literal 0
    set -l tokens (commandline -opc)
    set -e tokens[1]
    for token in $tokens
        if test $value -eq 1
            set value 0
        else if test $literal -eq 1
            set positionals (math $positionals + 1)
        else
            switch $token
                case '--'
                    set literal 1
                case {flags}
                    set value 1
                case '-*'
                case '*'
                    set positionals (math $positionals + 1)
            end
        end
    end
    echo $positionals
end

function __fish_agent_wants_word
    test (__fish_agent_words) -eq 1
end

function __fish_agent_branches
    set -l tokens (commandline -opc)
    set -l repo
    for index in (seq (count $tokens))
        switch $tokens[$index]
            case --repo
                set repo $tokens[(math $index + 1)]
            case '--repo=*'
                set repo (string replace -- '--repo=' '' $tokens[$index])
        end
    end
    if test -n "$repo"
        agent complete branches --notes --repo $repo 2>/dev/null
    else
        agent complete branches --notes 2>/dev/null
    end
end

"#,
        flags = grammar.fish_patterns()
    );
    let entries = kinds().flat_map(|kind| {
        let word = kind.word();
        let source = match kind {
            CompleteKind::Branches => "__fish_agent_branches".to_string(),
            _ => format!("agent complete {word} --notes 2>/dev/null"),
        };
        let positional = kind.first_positional_of();
        let words = (!positional.is_empty()).then(|| {
            format!(
                "complete -c agent -n \"__fish_agent_using_subcommand {}; and __fish_agent_wants_word\" -f -k -a \"({source})\"\n",
                positional.join(" ")
            )
        });
        let value = kind.value_of().map(|flag| {
            let (condition, takes) = match flag {
                "repo" => (
                    format!(
                        "-n \"__fish_agent_using_subcommand {}\" ",
                        grammar.repo_subcommands.join(" ")
                    ),
                    "-r",
                ),
                _ => (String::new(), "-x"),
            };
            format!(
                "complete -c agent {condition}-l {flag} {takes} -a \"(agent complete {word} --notes 2>/dev/null)\"\n"
            )
        });
        words.into_iter().chain(value)
    });
    std::iter::once(helper).chain(entries).collect()
}

const ARMS: &str = "            ";

fn flag_arms() -> String {
    kinds()
        .filter_map(|kind| {
            let flag = kind.value_of()?;
            Some(format!("{ARMS}--{flag}) kind={} ;;\n", kind.word()))
        })
        .collect()
}

fn subcommand_arms() -> String {
    kinds()
        .filter(|kind| !kind.first_positional_of().is_empty())
        .map(|kind| {
            format!(
                "{ARMS}{}) (( positionals == 0 )) && kind={} ;;\n",
                kind.first_positional_of().join("|"),
                kind.word()
            )
        })
        .collect::<Vec<_>>()
        .concat()
}

const SCAN: &str = r"        if [[ -n $pending ]]; then
            pending=
        elif [[ -n $literal ]]; then
            if [[ -z $subcommand ]]; then subcommand=$word; else positionals=$((positionals + 1)); fi
        else
            case $word in
                --) literal=1 ;;
                {value_flags}) pending=$word ;;
                -*) ;;
                *)
                    if [[ -z $subcommand ]]; then
                        subcommand=$word
                    else
                        positionals=$((positionals + 1))
                    fi
                    ;;
            esac
        fi
";

fn scan(grammar: &Grammar) -> String {
    SCAN.replace("{value_flags}", &grammar.case_flags())
}

fn bash(grammar: &Grammar) -> String {
    format!(
        r#"
_agent_dynamic() {{
    local cur="${{COMP_WORDS[COMP_CWORD]}}" subcommand= positionals=0 pending= literal= word kind= index
    for ((index = 1; index < COMP_CWORD; index++)); do
        word="${{COMP_WORDS[index]}}"
{scan}    done
    [[ $cur == -* && -z $literal && -z $pending ]] && {{ _agent "$@"; return; }}
    if [[ -n $pending ]]; then
        case $pending in
{flag_arms}        esac
    else
        case $subcommand in
{subcommand_arms}        esac
    fi
    [[ -n $kind ]] || {{ _agent "$@"; return; }}
    local plain="${{cur//\\/}}" noglob=1 candidate
    plain="${{plain#[\'\"]}}"
    [[ $- == *f* ]] && noglob=0
    set -f
    local IFS=$'\n'
    COMPREPLY=()
    for candidate in $(agent complete "$kind" 2>/dev/null); do
        [[ $candidate == "$plain"* ]] && COMPREPLY+=("$(printf '%q' "$candidate")")
    done
    (( noglob )) && set +f
    (( ${{#COMPREPLY[@]}} )) || _agent "$@"
}}

if [[ "${{BASH_VERSINFO[0]}}" -eq 4 && "${{BASH_VERSINFO[1]}}" -ge 4 || "${{BASH_VERSINFO[0]}}" -gt 4 ]]; then
    complete -F _agent_dynamic -o nosort -o bashdefault -o default agent
else
    complete -F _agent_dynamic -o bashdefault -o default agent
fi
"#,
        scan = scan(grammar),
        flag_arms = flag_arms(),
        subcommand_arms = subcommand_arms(),
    )
}

fn zsh(grammar: &Grammar) -> String {
    format!(
        r#"
_agent_dynamic() {{
    local subcommand= positionals=0 pending= literal= word kind=
    local -a candidates
    for word in "${{(@)words[2,CURRENT-1]}}"; do
{scan}    done
    if [[ $words[CURRENT] == -* && -z $literal && -z $pending ]]; then
        _agent "$@"
        return
    fi
    if [[ -n $pending ]]; then
        case $pending in
{flag_arms}        esac
    else
        case $subcommand in
{subcommand_arms}        esac
    fi
    if [[ -z $kind ]]; then
        _agent "$@"
        return
    fi
    candidates=("${{(@f)$(agent complete $kind --notes 2>/dev/null)}}")
    candidates=("${{(@)candidates:#}}")
    if (( $#candidates == 0 )); then
        _agent "$@"
        return
    fi
    candidates=("${{(@)candidates//:/\\:}}")
    candidates=("${{(@)candidates/$'\t'/:}}")
    _describe -V -t "$kind" "$kind" candidates || _agent "$@"
}}

if [ "$funcstack[1]" != "_agent" ]; then
    compdef _agent_dynamic agent
fi
"#,
        scan = scan(grammar),
        flag_arms = flag_arms(),
        subcommand_arms = subcommand_arms(),
    )
}

/// `cli` without its hidden subcommands: `clap_complete` lists them anyway.
fn without_hidden(cli: &Command) -> Command {
    let mut pruned = Command::new("agent")
        .args(cli.get_arguments().cloned())
        .subcommands(cli.get_subcommands().filter(|c| !c.is_hide_set()).cloned());
    if let Some(about) = cli.get_about() {
        pruned = pruned.about(about.clone());
    }
    pruned.version(env!("CARGO_PKG_VERSION"))
}

/// The script `agent completions <shell>` prints.
pub fn script(shell: Shell, cli: &Command) -> String {
    let mut cli = without_hidden(cli);
    let mut buffer = Vec::new();
    clap_complete::generate(shell, &mut cli, "agent", &mut buffer);
    let mut script = String::from_utf8_lossy(&buffer).into_owned();
    if let Some(glue) = Glue::of(shell) {
        script.push_str(&glue.render(&Grammar::of(&cli)));
    }
    script
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn every_subcommand_the_glue_completes_has_a_positional_to_complete() {
        let cli = crate::Cli::command();
        for kind in kinds() {
            for name in kind.first_positional_of() {
                let sub = cli
                    .find_subcommand(name)
                    .unwrap_or_else(|| panic!("{name}"));
                assert!(
                    sub.get_positionals().next().is_some(),
                    "{name} takes no positional"
                );
            }
        }
    }

    #[test]
    fn the_flags_the_glue_completes_exist_and_take_values() {
        let grammar = Grammar::of(&crate::Cli::command());
        for flag in kinds().filter_map(CompleteKind::value_of) {
            assert!(
                grammar.value_flags.iter().any(|(long, _)| long == flag),
                "{flag}"
            );
        }
        assert!(grammar.repo_subcommands.contains(&"co".to_string()));
        assert!(grammar.value_flags.contains(&("prompt".into(), Some('e'))));
    }
}
