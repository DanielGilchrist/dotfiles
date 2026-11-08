use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

use agent_core::model::Running;
use agent_core::names::SessionName;
use agent_core::ports::{
    ClientControl, ClientId, HostError, PaneCommand, Place, SessionHost, SessionInfo,
    SessionStatus, TerminalApp, Window,
};
use agent_core::process::{self, CommandError, LaunchSpec, parallel_map};
use nix::errno::Errno;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

const NESTING_ENV: &[&str] = &["ZELLIJ", "ZELLIJ_SESSION_NAME", "ZELLIJ_PANE_ID"];
/// A pane query for one session; a hung session shouldn't stall a refresh.
const PANE_QUERY_TIMEOUT: Duration = Duration::from_secs(2);
/// Bytes per `zellij action write`, well under the argv limit.
const WRITE_CHUNK: usize = 1024;

/// [`SessionHost`] backed by the `zellij` CLI. Layouts for new sessions are
/// kept in `layouts`, since zellij reads them after the create command starts.
pub struct Zellij {
    layouts: PathBuf,
    is_tui: fn(&[String]) -> bool,
}

impl Zellij {
    /// `is_tui` says whether an `agent` process's argv (after the program)
    /// runs a TUI, whose preview clients aren't someone else's window.
    pub fn new(layouts: PathBuf, is_tui: fn(&[String]) -> bool) -> Self {
        Self { layouts, is_tui }
    }

    fn layout(&self, name: &SessionName) -> PathBuf {
        self.layouts.join(format!("{name}.kdl"))
    }
}

fn zellij() -> Command {
    process::quiet("zellij")
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Listed {
    Live,
    Exited,
}

fn session_cmd(name: &SessionName) -> Command {
    let mut cmd = zellij();
    cmd.arg("--session").arg(name.as_str());
    cmd
}

fn parse_sessions(listing: &str) -> Vec<(SessionName, Listed)> {
    listing
        .lines()
        .filter_map(|line| {
            let name = line.split_whitespace().next()?.parse().ok()?;
            let listed = if line.contains("(EXITED") {
                Listed::Exited
            } else {
                Listed::Live
            };
            Some((name, listed))
        })
        .collect()
}

fn list() -> Result<Vec<(SessionName, Listed)>, HostError> {
    let mut cmd = zellij();
    cmd.args(["list-sessions", "--no-formatting"]);
    let out = process::output(&mut cmd)?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return if stderr.contains("No active zellij sessions")
            || stdout.contains("No active zellij sessions")
        {
            Ok(Vec::new())
        } else {
            Err(process::failed(&cmd, &out).into())
        };
    }
    Ok(parse_sessions(&stdout))
}

fn panes(name: &SessionName) -> Result<Vec<serde_json::Value>, HostError> {
    let mut cmd = session_cmd(name);
    cmd.args(["action", "list-panes", "--json"]);
    let out = process::output_within(&mut cmd, PANE_QUERY_TIMEOUT)?;
    if !out.status.success() {
        return Err(process::failed(&cmd, &out).into());
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str(&stdout).map_err(|e| {
        CommandError::Parse {
            command: process::describe(&cmd),
            detail: e.to_string(),
        }
        .into()
    })
}

fn terminal_panes(panes: &[serde_json::Value]) -> impl Iterator<Item = &serde_json::Value> {
    panes.iter().filter(|p| p["is_plugin"] != true)
}

/// Where zellij keeps resurrection layouts, from `zellij setup --check`.
fn cache_dir() -> Option<&'static Path> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        let out = process::output(zellij().args(["setup", "--check"])).ok()?;
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .find_map(|line| line.strip_prefix("[CACHE DIR]:"))
            .map(|dir| PathBuf::from(dir.trim()))
    })
    .as_deref()
}

/// Where an exited session last ran, from its newest resurrection layout.
fn exited_cwd(name: &SessionName) -> Option<PathBuf> {
    let newest = std::fs::read_dir(cache_dir()?)
        .ok()?
        .flatten()
        .map(|dir| {
            dir.path()
                .join("session_info")
                .join(name.as_str())
                .join("session-layout.kdl")
        })
        .filter_map(|path| Some((std::fs::metadata(&path).ok()?.modified().ok()?, path)))
        .max_by_key(|(modified, _): &(SystemTime, PathBuf)| *modified)?;
    layout_cwd(&std::fs::read_to_string(newest.1).ok()?)
}

/// The first `cwd` in a serialized layout: the layout's own, or else its
/// first pane's.
fn layout_cwd(kdl: &str) -> Option<PathBuf> {
    kdl.lines().find_map(|line| {
        let (_, rest) = line.split_once(" cwd")?;
        let value = rest.strip_prefix(' ').or_else(|| rest.strip_prefix('='))?;
        kdl_unquote(value).map(PathBuf::from)
    })
}

/// The KDL v1 string literal at the start of `s`.
fn kdl_unquote(s: &str) -> Option<String> {
    let mut chars = s.strip_prefix('"')?.chars();
    let mut out = String::new();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                c @ ('"' | '\\' | '/') => out.push(c),
                _ => return None,
            },
            c => out.push(c),
        }
    }
    None
}

fn pane_cwd(name: &SessionName) -> Result<Option<PathBuf>, HostError> {
    Ok(terminal_panes(&panes(name)?)
        .find_map(|p| p["pane_cwd"].as_str())
        .map(PathBuf::from))
}

/// The pane the harness runs in: the session's first terminal pane, since
/// the user may have split others off it.
fn harness_pane(panes: &[serde_json::Value]) -> Result<String, HostError> {
    terminal_panes(panes)
        .filter_map(|p| p["id"].as_u64())
        .min()
        .map(|id| format!("terminal_{id}"))
        .ok_or_else(|| {
            CommandError::Parse {
                command: "zellij action list-panes".into(),
                detail: "no terminal panes".into(),
            }
            .into()
        })
}

/// A KDL v1 string literal.
fn kdl_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{{{:x}}}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn layout_kdl(pane: &PaneCommand) -> String {
    let mut node = format!(
        "pane command={}",
        kdl_string(&pane.program.to_string_lossy())
    );
    let _ = write!(node, " cwd={}", kdl_string(&pane.cwd.to_string_lossy()));
    if !pane.args.is_empty() {
        let args: Vec<String> = pane.args.iter().map(|a| kdl_string(a)).collect();
        let _ = write!(node, " {{\n            args {}\n        }}", args.join(" "));
    }
    format!("layout {{\n    tab {{\n        {node}\n    }}\n}}\n")
}

/// Flags before the subcommand that take a value.
const VALUED_GLOBALS: &[&str] = &[
    "-c",
    "--config",
    "--config-dir",
    "--data-dir",
    "-l",
    "--layout",
    "-n",
    "--new-session-with-layout",
    "-s",
    "--session",
    "--max-panes",
];

/// zellij's argv after the program: the session its flags name, and what
/// follows the first positional word, which is the subcommand.
struct Invocation<'a> {
    session: Option<&'a str>,
    subcommand: Option<&'a str>,
    rest: &'a [&'a str],
}

impl<'a> Invocation<'a> {
    fn parse(args: &'a [&'a str]) -> Self {
        let mut session = None;
        let mut i = 0;
        while i < args.len() {
            let arg = args[i];
            if let Some(named) = arg.strip_prefix("--session=") {
                session = Some(named);
            } else if VALUED_GLOBALS.contains(&arg) {
                i += 1;
                if matches!(arg, "-s" | "--session") {
                    session = args.get(i).copied();
                }
            } else if !arg.starts_with('-') {
                return Self {
                    session,
                    subcommand: Some(arg),
                    rest: &args[i + 1..],
                };
            }
            i += 1;
        }
        Self {
            session,
            subcommand: None,
            rest: &[],
        }
    }
}

/// The session `zellij attach` names: the first word that isn't a flag or
/// a flag's value.
fn attach_target<'a>(rest: &[&'a str]) -> Option<&'a str> {
    let mut words = rest.iter();
    while let Some(&word) = words.next() {
        match word {
            "--index" | "-t" | "--token" => {
                words.next();
            }
            flag if flag.starts_with('-') => {}
            name => return Some(name),
        }
    }
    None
}

/// The session a zellij process shows in a terminal: `attach` (or `a`) of
/// it, or no subcommand with `-s <name>`, which creates it. Background
/// creation and `--session <name> action …` show nothing.
fn attached_session<'a>(args: &'a [&'a str]) -> Option<&'a str> {
    let invocation = Invocation::parse(args);
    match invocation.subcommand {
        Some("attach" | "a") => {
            let background = invocation
                .rest
                .iter()
                .any(|a| matches!(*a, "-b" | "--create-background"));
            if background {
                None
            } else {
                attach_target(invocation.rest)
            }
        }
        Some(_) => None,
        None => invocation.session,
    }
}

fn program_is(argv: &[String], program: &str) -> bool {
    argv.first()
        .and_then(|exe| Path::new(exe).file_name())
        .is_some_and(|n| n == program)
}

/// What the scan keeps of a process: its parent, the program it was started
/// as, the kernel's name for it (cut short on Linux), and the argv of the
/// programs that matter to it.
struct Process {
    parent: Option<u32>,
    program: String,
    comm: String,
    argv: Vec<String>,
}

/// The program an argv[0] names: its basename, cut at the `:` some daemons
/// retitle themselves with (`sshd: user@pts/0`).
fn program_name(argv0: &str) -> &str {
    let head = argv0.split(':').next().unwrap_or(argv0);
    head.rsplit('/')
        .next()
        .unwrap_or(head)
        .trim_start_matches('-')
}

/// Every process, for walking up from a client.
struct Processes {
    all: HashMap<u32, Process>,
}

impl Processes {
    fn scan() -> Self {
        let mut system = System::new();
        system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing().with_cmd(UpdateKind::Always),
        );
        let all = system
            .processes()
            .iter()
            .map(|(pid, p)| {
                let argv: Vec<String> = p
                    .cmd()
                    .iter()
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect();
                let keeps = program_is(&argv, "zellij") || program_is(&argv, "agent");
                let comm = program_name(&p.name().to_string_lossy()).to_string();
                let process = Process {
                    parent: p.parent().map(sysinfo::Pid::as_u32),
                    program: argv
                        .first()
                        .map_or_else(|| comm.clone(), |a| program_name(a).to_string()),
                    comm,
                    argv: if keeps { argv } else { Vec::new() },
                };
                (pid.as_u32(), process)
            })
            .collect();
        Self { all }
    }

    /// `pid` and its ancestors, nearest first.
    fn lineage(&self, mut pid: u32) -> Vec<u32> {
        let mut lineage = vec![pid];
        for _ in 0..64 {
            match self.all.get(&pid).and_then(|p| p.parent) {
                Some(parent) if parent != pid => {
                    lineage.push(parent);
                    pid = parent;
                }
                _ => break,
            }
        }
        lineage
    }

    fn argv(&self, pid: u32) -> &[String] {
        self.all.get(&pid).map_or(&[], |p| &p.argv)
    }

    /// Every window showing a session: zellij clients that are neither
    /// `own`'s nor under a process `is_tui` recognises, whose previews are
    /// not a window of someone else's.
    fn windows(&self, own: Option<ClientId>, is_tui: fn(&[String]) -> bool) -> Vec<Window> {
        let runs_tui = |pid: u32| {
            let argv = self.argv(pid);
            program_is(argv, "agent") && is_tui(&argv[1..])
        };
        let mut windows: Vec<Window> = self
            .all
            .iter()
            .filter_map(|(pid, process)| {
                if !program_is(&process.argv, "zellij") {
                    return None;
                }
                let rest: Vec<&str> = process.argv.iter().skip(1).map(String::as_str).collect();
                let session = attached_session(&rest)?.parse().ok()?;
                let lineage = self.lineage(*pid);
                let ours = own.is_some_and(|own| lineage.contains(&own.get()));
                (!ours && !lineage.iter().any(|p| runs_tui(*p))).then(|| Window {
                    client: ClientId::new(*pid),
                    session,
                    place: self.place_of(*pid),
                })
            })
            .collect();
        windows.sort_by_key(|w| w.client.get());
        windows
    }

    /// The nearest ancestor of `client` that names a place. With none, a
    /// client that has no shell above it either was left behind by a window
    /// that went away.
    fn place_of(&self, client: u32) -> Place {
        let above: Vec<&Process> = self
            .lineage(client)
            .into_iter()
            .skip(1)
            .filter_map(|pid| self.all.get(&pid))
            .collect();
        above
            .iter()
            .find_map(|p| place_named(&p.program).or_else(|| place_named(&p.comm)))
            .unwrap_or_else(|| {
                if above.iter().any(|p| SHELLS.contains(&p.program.as_str())) {
                    Place::Unknown
                } else {
                    Place::Detached
                }
            })
    }
}

const SHELLS: &[&str] = &[
    "fish", "bash", "zsh", "sh", "dash", "ksh", "tcsh", "csh", "nu", "pwsh", "elvish", "xonsh",
    "login",
];

/// How a program name has to match a terminal's.
enum Matching {
    Exactly,
    Prefix,
}

/// Terminal programs by program name, and what to call them.
const TERMINALS: &[(&str, Matching, &str)] = &[
    ("wezterm-gui", Matching::Exactly, "wezterm"),
    ("wezterm-mux-server", Matching::Exactly, "wezterm"),
    ("ghostty", Matching::Exactly, "ghostty"),
    ("iTerm2", Matching::Exactly, "iTerm"),
    ("iTermServer", Matching::Prefix, "iTerm"),
    ("Terminal", Matching::Exactly, "Terminal"),
    ("alacritty", Matching::Exactly, "alacritty"),
    ("kitty", Matching::Exactly, "kitty"),
    ("Hyper", Matching::Exactly, "Hyper"),
    ("konsole", Matching::Exactly, "konsole"),
    ("xterm", Matching::Exactly, "xterm"),
    ("foot", Matching::Exactly, "foot"),
];

/// The place a program of this name is, if it is one. Shells, `agent` and
/// `zellij` name none, so the walk goes on past them.
fn place_named(program: &str) -> Option<Place> {
    match program {
        "nvim" | "vim" => Some(Place::Nvim),
        "mosh-server" => Some(Place::Ssh),
        "tmux" => Some(Place::Tmux),
        _ if program.starts_with("sshd") => Some(Place::Ssh),
        _ => TERMINALS
            .iter()
            .find(|(name, matching, _)| match matching {
                Matching::Exactly => program == *name,
                Matching::Prefix => program.starts_with(name),
            })
            .map(|(_, _, app)| Place::Terminal(TerminalApp::new(*app))),
    }
}

impl SessionHost for Zellij {
    fn sessions(&self) -> Result<Vec<SessionInfo>, HostError> {
        let listed = list()?;
        Ok(parallel_map(&listed, |(name, listed)| {
            let status = match listed {
                Listed::Live => SessionStatus::Live {
                    cwd: pane_cwd(name).ok().flatten(),
                },
                Listed::Exited => SessionStatus::Exited {
                    cwd: exited_cwd(name),
                },
            };
            SessionInfo {
                name: name.clone(),
                status,
            }
        }))
    }

    fn attach(&self, name: &SessionName) -> LaunchSpec {
        LaunchSpec::new("zellij").args(["attach", name.as_str()])
    }

    fn create(&self, name: &SessionName, pane: &PaneCommand) -> Result<LaunchSpec, HostError> {
        if list()?
            .iter()
            .any(|(n, listed)| n == name && *listed == Listed::Exited)
        {
            process::run(zellij().args(["delete-session", name.as_str()]))?;
        }
        let io = |context: String| move |source| HostError::Io { context, source };
        std::fs::create_dir_all(&self.layouts)
            .map_err(io(format!("creating {}", self.layouts.display())))?;
        let path = self.layout(name);
        let mut tmp =
            tempfile::NamedTempFile::new_in(&self.layouts).map_err(io("creating layout".into()))?;
        tmp.write_all(layout_kdl(pane).as_bytes())
            .map_err(io("writing layout".into()))?;
        tmp.persist(&path).map_err(|e| HostError::Io {
            context: format!("saving {}", path.display()),
            source: e.error,
        })?;
        Ok(LaunchSpec::new("zellij")
            .args(["-s", name.as_str(), "-n"])
            .arg(&path)
            .cwd(&pane.cwd))
    }

    fn start_detached(&self, name: &SessionName) -> Result<(), HostError> {
        let status = list()?
            .into_iter()
            .find_map(|(n, listed)| (n == *name).then_some(listed));
        if status == Some(Listed::Live) {
            return Ok(());
        }
        let layout = self.layout(name);
        if !layout.exists() {
            let missing = std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("no layout at {}", layout.display()),
            );
            return Err(HostError::Io {
                context: format!("starting {name} detached"),
                source: missing,
            });
        }
        if status == Some(Listed::Exited) {
            process::run(zellij().args(["delete-session", name.as_str()]))?;
        }
        let mut cmd = zellij();
        cmd.args([
            "attach",
            "--create-background",
            name.as_str(),
            "options",
            "--default-layout",
        ])
        .arg(&layout);
        for var in NESTING_ENV {
            cmd.env_remove(var);
        }
        process::run(&mut cmd)?;
        Ok(())
    }

    fn kill(&self, session: &Running) -> Result<(), HostError> {
        process::run(zellij().args(["kill-session", session.name().as_str()]))?;
        Ok(())
    }

    fn delete(&self, name: &SessionName) -> Result<(), HostError> {
        if list()?.iter().any(|(n, _)| n == name) {
            process::run(zellij().args(["delete-session", "--force", name.as_str()]))?;
        }
        let layout = self.layout(name);
        match std::fs::remove_file(&layout) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(HostError::Io {
                context: format!("removing {}", layout.display()),
                source: e,
            }),
            _ => Ok(()),
        }
    }

    /// Writes to the harness's pane, whichever pane has focus.
    fn write(&self, name: &SessionName, bytes: &[u8]) -> Result<(), HostError> {
        let pane = harness_pane(&panes(name)?)?;
        for chunk in bytes.chunks(WRITE_CHUNK) {
            let mut cmd = session_cmd(name);
            cmd.args(["action", "write", "--pane-id", &pane])
                .args(chunk.iter().map(u8::to_string));
            process::run(&mut cmd)?;
        }
        Ok(())
    }

    fn screen(&self, name: &SessionName) -> Result<String, HostError> {
        let pane = harness_pane(&panes(name)?)?;
        let mut cmd = session_cmd(name);
        cmd.args(["action", "dump-screen", "--ansi", "--pane-id", &pane]);
        Ok(process::run(&mut cmd)?)
    }

    fn clients(&self) -> Option<&dyn ClientControl> {
        Some(self)
    }

    fn nesting_env(&self) -> &'static [&'static str] {
        NESTING_ENV
    }
}

impl ClientControl for Zellij {
    fn windows(&self, own: Option<ClientId>) -> Result<Vec<Window>, HostError> {
        // `own` is the one client the caller is sure of whatever its binary
        // is called, which the TUI predicate can't promise.
        Ok(Processes::scan().windows(own, self.is_tui))
    }

    /// SIGHUP is what a closing terminal sends, so the session just loses a
    /// viewer. A stopped client acts on no signal until continued, so it is
    /// continued first.
    fn detach(&self, clients: &[ClientId]) -> Result<(), HostError> {
        for &client in clients {
            let pid = i32::try_from(client.get()).map_err(|e| HostError::Detach {
                client,
                reason: e.to_string(),
            })?;
            let signal = |signal| match kill(Pid::from_raw(pid), signal) {
                Ok(()) | Err(Errno::ESRCH) => Ok(()),
                Err(e) => Err(HostError::Detach {
                    client,
                    reason: e.to_string(),
                }),
            };
            signal(Signal::SIGCONT)?;
            signal(Signal::SIGHUP)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_listing_parses_names_and_presence_and_skips_invalid() {
        let listing = "knockoff [Created 2days ago] \nold [Created 3months ago] (EXITED - attach to resurrect)\nagents [Created 1h ago]\n";
        let sessions = parse_sessions(listing);
        assert_eq!(sessions.len(), 2);
        assert!(sessions[0].1 == Listed::Live);
        assert!(sessions[1].1 == Listed::Exited);
    }

    #[test]
    fn exited_sessions_ran_where_their_layout_or_first_pane_did() {
        let fish = "layout {\n    cwd \"/Users/d/.config\"\n    tab {\n        pane command=\"claude\" {\n";
        assert_eq!(layout_cwd(fish), Some("/Users/d/.config".into()));
        let ours = "layout {\n    tab name=\"Tab #1\" {\n        pane command=\"claude\" cwd=\"/w/api/fix\" {\n";
        assert_eq!(layout_cwd(ours), Some("/w/api/fix".into()));
        assert_eq!(layout_cwd("layout {\n    tab {\n        pane\n"), None);
    }

    #[test]
    fn writes_go_to_the_first_terminal_pane_not_the_focused_one() {
        let panes: Vec<serde_json::Value> = serde_json::from_str(
            r#"[{"id":0,"is_plugin":true},{"id":4,"is_plugin":false,"is_focused":true},{"id":2,"is_plugin":false}]"#,
        )
        .expect("succeeds");
        assert_eq!(harness_pane(&panes).expect("succeeds"), "terminal_2");
    }

    #[test]
    fn kdl_strings_read_back_what_was_written() {
        let path = "/a \"b\"\\c\td";
        assert_eq!(kdl_unquote(&kdl_string(path)).as_deref(), Some(path));
    }

    #[test]
    fn kdl_strings_escape_quotes_and_control_characters() {
        assert_eq!(kdl_string("a\"b\\c\nd\x1be"), "\"a\\\"b\\\\c\\nd\\u{1b}e\"");
    }

    #[test]
    fn layout_puts_args_in_a_child_block() {
        let pane = PaneCommand {
            program: "/bin/agent".into(),
            args: vec!["exec".into(), "fix".into()],
            cwd: "/w/api/fix".into(),
        };
        let kdl = layout_kdl(&pane);
        assert!(kdl.contains("pane command=\"/bin/agent\" cwd=\"/w/api/fix\" {"));
        assert!(kdl.contains("args \"exec\" \"fix\""));
    }

    fn argv(line: &str) -> Vec<String> {
        line.split_whitespace().map(String::from).collect()
    }

    fn client(args: &str, name: &str) -> bool {
        attached_session(&args.split_whitespace().collect::<Vec<_>>()) == Some(name)
    }

    #[test]
    fn typed_attaches_with_the_alias_or_flags_are_clients_of_the_session_they_name() {
        assert!(client("attach fix", "fix"));
        assert!(client("a fix", "fix"));
        assert!(client("attach --create fix", "fix"));
        assert!(client("attach -c fix", "fix"));
        assert!(client("attach fix options --default-shell fish", "fix"));
        assert!(client("attach --index 1 fix", "fix"));
        assert!(client("--config /c.kdl attach fix", "fix"));
        assert!(!client("attach fixer", "fix"));
        assert!(!client("a fixer", "fix"));
    }

    #[test]
    fn a_name_that_is_only_somewhere_in_the_arguments_is_not_the_one_attached() {
        assert!(!client("attach other options --default-cwd fix", "fix"));
        assert!(!client("attach other fix", "fix"));
        assert!(!client("attach --index 3", "3"));
    }

    #[test]
    fn a_session_made_with_s_is_a_client_but_subcommands_and_background_creation_are_not() {
        assert!(client("-s fix -n /l.kdl", "fix"));
        assert!(client("-n /l.kdl -s fix", "fix"));
        assert!(client(
            "--session fix --new-session-with-layout /l.kdl",
            "fix"
        ));
        assert!(client("--session=fix", "fix"));
        assert!(client("-s fix", "fix"));
        assert!(!client("--session fix action list-panes", "fix"));
        assert!(!client(
            "--session fix action new-tab --layout x.kdl",
            "fix"
        ));
        assert!(!client("--session=fix run -- ls", "fix"));
        assert!(!client("-s fix kill-session", "fix"));
        assert!(!client("-s fix pipe", "fix"));
        assert!(!client("-s fixer -n /l.kdl", "fix"));
        assert!(!client(
            "attach --create-background fix options --default-layout /l.kdl",
            "fix"
        ));
    }

    fn agent_runs_a_tui(args: &[String]) -> bool {
        args.iter()
            .all(|a| a == "tui" || a == "--harness" || a == "claude")
    }

    fn table(rows: &[(u32, u32, &str)]) -> Processes {
        Processes {
            all: rows
                .iter()
                .map(|(pid, parent, line)| {
                    (*pid, {
                        let argv = argv(line);
                        let program = argv.first().map_or("", |a| program_name(a)).to_string();
                        Process {
                            parent: Some(*parent),
                            comm: program.clone(),
                            program,
                            argv,
                        }
                    })
                })
                .collect(),
        }
    }

    fn windows_of_fix(procs: &Processes, own: Option<ClientId>) -> Vec<Window> {
        let fix: SessionName = "fix".parse().expect("a name");
        procs
            .windows(own, agent_runs_a_tui)
            .into_iter()
            .filter(|w| w.session == fix)
            .collect()
    }

    fn windows(procs: &Processes, own: Option<ClientId>) -> Vec<u32> {
        windows_of_fix(procs, own)
            .into_iter()
            .map(|w| w.client.get())
            .collect()
    }

    #[test]
    fn a_window_is_a_client_but_the_tui_and_other_tuis_previews_are_not() {
        let procs = table(&[
            (10, 1, "/bin/agent"),
            (11, 10, "zellij attach fix"),
            (20, 1, "/bin/agent --harness claude tui"),
            (21, 20, "zellij attach fix"),
            (30, 1, "nvim"),
            (31, 30, "zellij attach fix"),
            (40, 1, "fish"),
            (41, 40, "/bin/agent attach fix"),
            (42, 41, "zellij -s fix -n /l.kdl"),
            (50, 1, "sshd"),
            (51, 50, "zellij a fix"),
        ]);
        assert_eq!(windows(&procs, None), [31, 42, 51]);
    }

    #[test]
    fn the_client_the_tui_spawned_is_excluded_by_pid_even_when_no_tui_is_recognised() {
        let procs = table(&[(10, 1, "fish"), (11, 10, "zellij attach fix")]);
        assert_eq!(windows(&procs, Some(ClientId::new(11))), Vec::<u32>::new());
        assert_eq!(windows(&procs, Some(ClientId::new(10))), Vec::<u32>::new());
        assert_eq!(windows(&procs, None), [11]);
    }

    #[test]
    fn a_window_run_by_a_tui_action_is_still_under_that_tui() {
        let procs = table(&[
            (10, 1, "/bin/agent"),
            (11, 10, "/bin/agent attach fix"),
            (12, 11, "zellij attach fix"),
        ]);
        assert_eq!(windows(&procs, None), Vec::<u32>::new());
    }

    fn places_of(procs: &Processes) -> Vec<Place> {
        windows_of_fix(procs, None)
            .into_iter()
            .map(|w| w.place)
            .collect()
    }

    fn terminal(app: &str) -> Place {
        Place::Terminal(TerminalApp::new(app))
    }

    #[test]
    fn a_client_under_nvim_is_in_nvim() {
        let procs = table(&[
            (1, 0, "launchd"),
            (10, 1, "wezterm-gui"),
            (20, 10, "nvim"),
            (21, 20, "fish"),
            (22, 21, "zellij attach fix"),
        ]);
        assert_eq!(places_of(&procs), [Place::Nvim]);
    }

    #[test]
    fn a_client_under_sshd_is_over_ssh_whatever_the_sshd_is_called() {
        for sshd in ["sshd", "sshd-session", "/usr/sbin/sshd"] {
            let procs = table(&[
                (1, 0, "launchd"),
                (10, 1, sshd),
                (11, 10, "fish"),
                (12, 11, "zellij a fix"),
            ]);
            assert_eq!(places_of(&procs), [Place::Ssh], "{sshd}");
        }
    }

    #[test]
    fn a_client_under_the_tmux_server_is_in_tmux_even_when_the_server_is_ppid_1() {
        let procs = table(&[
            (1, 0, "launchd"),
            (10, 1, "tmux"),
            (11, 10, "fish"),
            (12, 11, "zellij attach fix"),
        ]);
        assert_eq!(places_of(&procs), [Place::Tmux]);
    }

    #[test]
    fn a_client_whose_chain_is_cut_at_a_tmux_is_in_tmux() {
        let procs = table(&[(10, 1, "tmux: server"), (11, 10, "zellij attach fix")]);
        assert_eq!(places_of(&procs), [Place::Tmux]);
    }

    #[test]
    fn a_client_in_a_wezterm_tab_or_its_mux_is_in_wezterm() {
        for gui in ["wezterm-gui", "wezterm-mux-server"] {
            let procs = table(&[
                (1, 0, "launchd"),
                (10, 1, gui),
                (11, 10, "fish"),
                (12, 11, "/bin/agent attach fix"),
                (13, 12, "zellij -s fix -n /l.kdl"),
            ]);
            assert_eq!(places_of(&procs), [terminal("wezterm")], "{gui}");
        }
    }

    #[test]
    fn known_terminal_apps_are_named_short() {
        for (process, app) in [
            ("ghostty", "ghostty"),
            ("iTerm2", "iTerm"),
            ("Terminal", "Terminal"),
        ] {
            let procs = table(&[
                (1, 0, "launchd"),
                (10, 1, process),
                (11, 10, "login"),
                (12, 11, "fish"),
                (13, 12, "zellij attach fix"),
            ]);
            assert_eq!(places_of(&procs), [terminal(app)], "{process}");
        }
    }

    #[test]
    fn agent_attach_and_shells_are_skipped_on_the_way_up() {
        let procs = table(&[
            (1, 0, "launchd"),
            (10, 1, "nvim"),
            (11, 10, "fish"),
            (12, 11, "/bin/agent attach fix"),
            (13, 12, "bash"),
            (14, 13, "zellij attach fix"),
        ]);
        assert_eq!(places_of(&procs), [Place::Nvim]);
    }

    #[test]
    fn the_nearest_place_wins() {
        let procs = table(&[
            (1, 0, "launchd"),
            (10, 1, "wezterm-gui"),
            (11, 10, "nvim"),
            (12, 11, "zellij attach fix"),
        ]);
        assert_eq!(places_of(&procs), [Place::Nvim]);
    }

    #[test]
    fn a_client_with_a_shell_but_no_known_place_above_it_is_unknown() {
        let procs = table(&[
            (1, 0, "launchd"),
            (10, 1, "fish"),
            (11, 10, "zellij attach fix"),
        ]);
        assert_eq!(places_of(&procs), [Place::Unknown]);
    }

    #[test]
    fn a_tuis_clients_are_never_a_place() {
        let procs = table(&[
            (10, 1, "/bin/agent"),
            (11, 10, "zellij attach fix"),
            (20, 1, "sshd"),
            (21, 20, "/bin/agent attach fix"),
            (22, 21, "zellij attach fix"),
        ]);
        assert_eq!(places_of(&procs), [Place::Ssh]);
    }

    #[test]
    fn several_windows_are_listed_one_each_with_their_session() {
        let procs = table(&[
            (10, 1, "sshd"),
            (11, 10, "zellij attach fix"),
            (20, 1, "nvim"),
            (21, 20, "zellij attach fix"),
            (30, 1, "nvim"),
            (31, 30, "zellij attach fix"),
            (40, 1, "tmux"),
            (41, 40, "zellij attach other"),
        ]);
        assert_eq!(places_of(&procs), [Place::Ssh, Place::Nvim, Place::Nvim]);
        let other = procs
            .windows(None, agent_runs_a_tui)
            .into_iter()
            .filter(|w| w.session.as_str() == "other")
            .map(|w| w.place)
            .collect::<Vec<_>>();
        assert_eq!(other, [Place::Tmux]);
    }

    #[test]
    fn a_program_cut_short_by_the_kernel_is_still_named_by_its_argv() {
        let mut procs = table(&[
            (1, 0, "launchd"),
            (
                10,
                1,
                "/Applications/WezTerm.app/Contents/MacOS/wezterm-mux-server",
            ),
            (11, 10, "fish"),
            (12, 11, "zellij attach fix"),
        ]);
        procs.all.get_mut(&10).expect("a process").comm = "wezterm-mux-ser".into();
        assert_eq!(places_of(&procs), [terminal("wezterm")]);
    }

    #[test]
    fn a_retitled_daemon_is_named_by_its_program_and_the_comm_covers_the_rest() {
        let mut procs = table(&[
            (1, 0, "launchd"),
            (10, 1, "sshd: daniel@pts/0"),
            (11, 10, "zellij attach fix"),
        ]);
        assert_eq!(places_of(&procs), [Place::Ssh]);
        procs.all.get_mut(&10).expect("a process").program = "unrelated".into();
        procs.all.get_mut(&10).expect("a process").comm = "sshd-session".into();
        assert_eq!(places_of(&procs), [Place::Ssh]);
    }

    #[test]
    fn iterm_shells_sit_under_its_server() {
        let procs = table(&[
            (1, 0, "launchd"),
            (10, 1, "iTermServer-main"),
            (11, 10, "login"),
            (12, 11, "fish"),
            (13, 12, "zellij attach fix"),
        ]);
        assert_eq!(places_of(&procs), [terminal("iTerm")]);
    }

    #[test]
    fn the_client_the_tui_spawned_is_no_window() {
        let procs = table(&[(10, 1, "nvim"), (11, 10, "zellij attach fix")]);
        assert_eq!(procs.windows(Some(ClientId::new(11)), agent_runs_a_tui), []);
    }

    #[test]
    fn a_real_scan_names_processes_by_program_and_links_them_to_their_parents() {
        let procs = Processes::scan();
        let me = procs.all.get(&std::process::id()).expect("this process");
        assert_ne!(me.program, "");
        assert!(procs.lineage(std::process::id()).len() > 1);
    }

    #[test]
    fn a_client_left_with_only_init_above_it_is_detached() {
        let procs = table(&[(1, 0, "launchd"), (11, 1, "zellij attach fix")]);
        assert_eq!(places_of(&procs), [Place::Detached]);
    }

    #[test]
    fn a_client_whose_agent_attach_wrapper_was_orphaned_is_detached() {
        let procs = table(&[
            (1, 0, "launchd"),
            (10, 1, "/bin/agent attach fix"),
            (11, 10, "zellij attach fix"),
        ]);
        assert_eq!(places_of(&procs), [Place::Detached]);
    }

    #[test]
    fn a_login_shell_with_a_dash_counts_as_a_shell_above_the_client() {
        let procs = table(&[
            (1, 0, "launchd"),
            (10, 1, "-fish"),
            (11, 10, "zellij attach fix"),
        ]);
        assert_eq!(places_of(&procs), [Place::Unknown]);
    }

    #[test]
    fn a_known_place_beats_detached() {
        let procs = table(&[
            (1, 0, "launchd"),
            (10, 1, "tmux"),
            (11, 10, "zellij attach fix"),
        ]);
        assert_eq!(places_of(&procs), [Place::Tmux]);
    }

    #[test]
    fn detaching_a_stopped_client_still_closes_it() {
        let mut child = std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .expect("sleep starts");
        let client = ClientId::new(child.id());
        kill(
            Pid::from_raw(i32::try_from(client.get()).expect("a pid")),
            Signal::SIGSTOP,
        )
        .expect("stops");
        let zellij = Zellij::new(PathBuf::new(), |_| false);
        zellij.detach(&[client]).expect("signals");
        let status = child.wait().expect("exits");
        assert_eq!(
            std::os::unix::process::ExitStatusExt::signal(&status),
            Some(1)
        );
    }

    #[test]
    fn a_client_that_already_left_does_not_stop_the_others_closing() {
        let mut gone = std::process::Command::new("true")
            .spawn()
            .expect("true starts");
        let left = ClientId::new(gone.id());
        gone.wait().expect("exits");
        let mut child = std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .expect("sleep starts");
        let staying = ClientId::new(child.id());
        let zellij = Zellij::new(PathBuf::new(), |_| false);
        zellij.detach(&[left, staying]).expect("signals");
        let status = child.wait().expect("exits");
        assert_eq!(
            std::os::unix::process::ExitStatusExt::signal(&status),
            Some(1)
        );
    }
}
