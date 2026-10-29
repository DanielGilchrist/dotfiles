use std::collections::HashMap;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

use agent_core::model::Running;
use agent_core::names::SessionName;
use agent_core::ports::{
    ClientControl, ClientId, HostError, PaneCommand, SessionHost, SessionInfo, SessionStatus,
};
use agent_core::process::{self, CommandError, LaunchSpec, parallel_map};
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
}

impl Zellij {
    pub fn new(layouts: PathBuf) -> Self {
        Self { layouts }
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

/// `zellij attach <name>` or `zellij -s <name> -n <layout>`, as argv after the program.
fn is_client_of(args: &[OsString], name: &str) -> bool {
    let args: Vec<&str> = args.iter().filter_map(|a| a.to_str()).collect();
    match args.as_slice() {
        ["attach", rest @ ..] => rest.last() == Some(&name),
        ["-s" | "--session", session, rest @ ..] => {
            *session == name
                && rest
                    .iter()
                    .any(|a| matches!(*a, "-n" | "--new-session-with-layout"))
        }
        _ => false,
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
    fn other_clients(
        &self,
        name: &SessionName,
        own: Option<ClientId>,
    ) -> Result<Vec<ClientId>, HostError> {
        let mut system = System::new();
        system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing().with_cmd(UpdateKind::Always),
        );
        let parents: HashMap<u32, u32> = system
            .processes()
            .iter()
            .filter_map(|(pid, p)| Some((pid.as_u32(), p.parent()?.as_u32())))
            .collect();
        let descends_from_own = |mut pid: u32| {
            let Some(own) = own else { return false };
            for _ in 0..64 {
                if pid == own.get() {
                    return true;
                }
                match parents.get(&pid) {
                    Some(&parent) if parent != pid => pid = parent,
                    _ => return false,
                }
            }
            false
        };
        let mut clients: Vec<ClientId> = system
            .processes()
            .iter()
            .filter(|(_, p)| {
                let cmd = p.cmd();
                cmd.first()
                    .and_then(|exe| Path::new(exe).file_name())
                    .is_some_and(|n| n == "zellij")
                    && is_client_of(&cmd[1..], name.as_str())
            })
            .map(|(pid, _)| pid.as_u32())
            .filter(|pid| !descends_from_own(*pid))
            .map(ClientId::new)
            .collect();
        clients.sort_by_key(|c| c.get());
        Ok(clients)
    }

    /// SIGHUP is what a closing terminal sends, so the session just loses a viewer.
    fn detach(&self, clients: &[ClientId]) -> Result<(), HostError> {
        for &client in clients {
            let pid = i32::try_from(client.get()).map_err(|e| HostError::Detach {
                client,
                reason: e.to_string(),
            })?;
            kill(Pid::from_raw(pid), Signal::SIGHUP).map_err(|e| HostError::Detach {
                client,
                reason: e.to_string(),
            })?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

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

    #[test]
    fn client_detection_matches_attach_and_create_but_not_actions() {
        assert!(is_client_of(&os(&["attach", "fix"]), "fix"));
        assert!(is_client_of(&os(&["attach", "--create", "fix"]), "fix"));
        assert!(is_client_of(&os(&["-s", "fix", "-n", "/l.kdl"]), "fix"));
        assert!(!is_client_of(
            &os(&["--session", "fix", "action", "list-panes"]),
            "fix"
        ));
        assert!(!is_client_of(&os(&["attach", "fixer"]), "fix"));
    }
}
