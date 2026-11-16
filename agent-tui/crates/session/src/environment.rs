use std::ffi::OsString;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};

use agent_core::names::SessionName;
use nix::unistd::Uid;

/// A variable the spawner may have, which would mislead the program about
/// the terminal or the login it runs in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Denied {
    Exactly(&'static str),
    Prefix(&'static str),
}

impl Denied {
    fn matches(self, name: &str) -> bool {
        match self {
            Self::Exactly(exact) => name == exact,
            Self::Prefix(prefix) => name.starts_with(prefix),
        }
    }
}

/// `SSH_AUTH_SOCK` is not here: it is decided by [`AgentSocket`]. Other `SSH_`
/// variables (`SSH_ASKPASS`) are the user's configuration and stay. This list
/// is about terminals and logins. The variables Claude Code sets in its own
/// children are the harness's, and `agent exec` removes them.
const DENIED: [Denied; 21] = [
    Denied::Exactly("SSH_CONNECTION"),
    Denied::Exactly("SSH_CLIENT"),
    Denied::Exactly("SSH_TTY"),
    Denied::Exactly("SSH_ORIGINAL_COMMAND"),
    Denied::Exactly("COLUMNS"),
    Denied::Exactly("LINES"),
    Denied::Exactly("NVIM"),
    Denied::Prefix("NVIM_"),
    Denied::Prefix("LC_TERMINAL"),
    Denied::Exactly("TERM_SESSION_ID"),
    Denied::Exactly("WT_SESSION"),
    Denied::Prefix("TERM_PROGRAM"),
    Denied::Prefix("WEZTERM_"),
    Denied::Prefix("ZELLIJ"),
    Denied::Prefix("KITTY_"),
    Denied::Prefix("ITERM_"),
    Denied::Prefix("GHOSTTY_"),
    Denied::Prefix("ALACRITTY_"),
    Denied::Exactly("VTE_VERSION"),
    Denied::Prefix("TMUX"),
    Denied::Exactly("STY"),
];

const TERM: &str = "xterm-256color";
const COLORTERM: &str = "truecolor";
pub const SESSION: &str = "AGENT_SESSION";
const AUTH_SOCK: &str = "SSH_AUTH_SOCK";

/// The session whose program the caller runs in, from the `AGENT_SESSION` the
/// host sets for it, read through `var`.
pub fn own_session(var: &dyn Fn(&str) -> Option<String>) -> Option<SessionName> {
    var(SESSION)?.parse().ok()
}

/// What the program's `SSH_AUTH_SOCK` is, decided once from the spawner's
/// environment. A forwarded agent belongs to the ssh login that started the
/// host, which ends while the session goes on, so it is never passed on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AgentSocket {
    Use(PathBuf),
    Unset,
}

/// Whether what `path` leads to is a socket owned by `ours`. Links are
/// followed, and only the end of the chain is judged: wezterm's
/// `agent.<pid>` is a link in the user's own directory to launchd's socket,
/// and a link planted by someone else can only lead to our own agent, since
/// a socket we don't own is refused wherever the chain ends. A link that
/// dangles or leads to anything but a socket is refused.
fn is_our_socket(path: &Path, ours: Uid) -> bool {
    std::fs::canonicalize(path)
        .and_then(std::fs::symlink_metadata)
        .is_ok_and(|meta| meta.file_type().is_socket() && meta.uid() == ours.as_raw())
}

impl AgentSocket {
    /// Outside an ssh login, the spawner's own agent when it leads to our
    /// socket. Otherwise (inside a login, or no agent, or one that isn't
    /// ours) the first of `stable` that does, which is an agent that outlives
    /// the login. Else none.
    fn decide(inherited: &[(OsString, OsString)], stable: &[PathBuf], ours: Uid) -> Self {
        let value = |name: &str| {
            inherited
                .iter()
                .find(|(own, _)| own == name)
                .map(|(_, value)| value)
                .filter(|value| !value.is_empty())
        };
        let in_ssh = value("SSH_CONNECTION").is_some();
        let own = value(AUTH_SOCK)
            .map(PathBuf::from)
            .filter(|path| !in_ssh && is_our_socket(path, ours));
        own.or_else(|| {
            stable
                .iter()
                .find(|path| is_our_socket(path, ours))
                .cloned()
        })
        .map_or(Self::Unset, Self::Use)
    }
}

/// Agents that outlive a login, where they usually are: the user service
/// socket systemd provides, and launchd's on macOS.
fn stable_agents(inherited: &[(OsString, OsString)]) -> Vec<PathBuf> {
    let mut found = Vec::new();
    if let Some((_, runtime)) = inherited.iter().find(|(name, _)| name == "XDG_RUNTIME_DIR") {
        found.push(PathBuf::from(runtime).join("ssh-agent.socket"));
    }
    #[cfg(target_os = "macos")]
    if let Ok(out) = std::process::Command::new("launchctl")
        .args(["getenv", AUTH_SOCK])
        .output()
        && out.status.success()
    {
        let path = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        if !path.is_empty() {
            found.push(PathBuf::from(path));
        }
    }
    found
}

/// The environment of a session's program: the host's own with what names a
/// terminal or login the program isn't in removed, then `TERM`, `COLORTERM`,
/// `AGENT_SESSION` and the decided agent socket set. It is built here and
/// not inherited from whoever spawned the host, which may be a TUI in one
/// terminal or an ssh login.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChildEnvironment {
    session: SessionName,
    agent: AgentSocket,
    inherited: Vec<(OsString, OsString)>,
}

impl ChildEnvironment {
    pub(crate) fn from_spawner(session: SessionName, inherited: Vec<(OsString, OsString)>) -> Self {
        let agent = AgentSocket::decide(&inherited, &stable_agents(&inherited), Uid::current());
        Self {
            session,
            agent,
            inherited,
        }
    }

    pub(crate) fn build(self) -> Vec<(OsString, OsString)> {
        let mut set: Vec<(&str, OsString)> = vec![
            ("TERM", TERM.into()),
            ("COLORTERM", COLORTERM.into()),
            (SESSION, self.session.as_str().into()),
        ];
        if let AgentSocket::Use(path) = &self.agent {
            set.push((AUTH_SOCK, path.clone().into_os_string()));
        }
        let own: Vec<&str> = set.iter().map(|(name, _)| *name).collect();
        let kept = self.inherited.into_iter().filter(|(name, _)| {
            name.to_str().is_none_or(|name| {
                name != AUTH_SOCK
                    && !DENIED.iter().any(|denied| denied.matches(name))
                    && !own.contains(&name)
            })
        });
        kept.chain(
            set.into_iter()
                .map(|(name, value)| (OsString::from(name), value)),
        )
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixListener;

    use super::*;

    fn pairs(inherited: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        inherited
            .iter()
            .map(|(name, value)| (OsString::from(name), OsString::from(value)))
            .collect()
    }

    fn built(inherited: &[(&str, &str)]) -> Vec<(String, String)> {
        let environment = ChildEnvironment {
            session: "fix-bug".parse().expect("a name"),
            agent: AgentSocket::Unset,
            inherited: pairs(inherited),
        };
        environment
            .build()
            .into_iter()
            .map(|(name, value)| {
                (
                    name.into_string().expect("utf-8"),
                    value.into_string().expect("utf-8"),
                )
            })
            .collect()
    }

    fn has(environment: &[(String, String)], name: &str) -> bool {
        environment.iter().any(|(own, _)| own == name)
    }

    #[test]
    fn variables_naming_the_spawners_terminal_or_login_are_removed() {
        let names = [
            "SSH_CONNECTION",
            "SSH_CLIENT",
            "SSH_TTY",
            "SSH_ORIGINAL_COMMAND",
            "SSH_AUTH_SOCK",
            "COLUMNS",
            "LINES",
            "NVIM",
            "NVIM_LOG_FILE",
            "LC_TERMINAL",
            "LC_TERMINAL_VERSION",
            "TERM_SESSION_ID",
            "WT_SESSION",
            "WEZTERM_PANE",
            "WEZTERM_EXECUTABLE",
            "TERM_PROGRAM",
            "TERM_PROGRAM_VERSION",
            "ZELLIJ",
            "ZELLIJ_SESSION_NAME",
            "ZELLIJ_PANE_ID",
            "KITTY_WINDOW_ID",
            "KITTY_PID",
            "ITERM_SESSION_ID",
            "ITERM_PROFILE",
            "GHOSTTY_RESOURCES_DIR",
            "ALACRITTY_SOCKET",
            "VTE_VERSION",
            "TMUX",
            "TMUX_PANE",
            "STY",
        ];
        let inherited: Vec<(&str, &str)> = names.iter().map(|name| (*name, "x")).collect();
        let environment = built(&inherited);
        for name in names {
            assert!(!has(&environment, name), "{name} was kept");
        }
    }

    #[test]
    fn variables_that_only_look_similar_or_are_the_users_configuration_are_kept() {
        let kept = [
            "PATH",
            "HOME",
            "STYLE",
            "SSHD",
            "SSH_ASKPASS",
            "SSH_ASKPASS_REQUIRE",
            "TERMINFO",
            "TERM_COLOUR",
            "NVIMRC",
            "LC_ALL",
            "LC_CTYPE",
            "COLUMNS_X",
        ];
        let inherited: Vec<(&str, &str)> = kept.iter().map(|name| (*name, "x")).collect();
        let environment = built(&inherited);
        for name in kept {
            assert!(has(&environment, name), "{name} was dropped");
        }
    }

    #[test]
    fn the_terminal_is_declared_whatever_the_spawner_had() {
        let environment = built(&[
            ("TERM", "dumb"),
            ("COLORTERM", "no"),
            ("AGENT_SESSION", "x"),
        ]);
        let value = |name: &str| {
            environment
                .iter()
                .filter(|(own, _)| own == name)
                .map(|(_, value)| value.as_str())
                .collect::<Vec<_>>()
        };
        assert_eq!(value("TERM"), ["xterm-256color"]);
        assert_eq!(value("COLORTERM"), ["truecolor"]);
        assert_eq!(value("AGENT_SESSION"), ["fix-bug"]);
    }

    #[test]
    fn a_name_that_is_not_utf8_is_kept() {
        use std::os::unix::ffi::OsStringExt;
        let odd = OsString::from_vec(vec![b'X', 0xff]);
        let environment = ChildEnvironment {
            session: "a".parse().expect("a name"),
            agent: AgentSocket::Unset,
            inherited: vec![(odd.clone(), OsString::from("v"))],
        };
        assert!(environment.build().iter().any(|(name, _)| *name == odd));
    }

    struct Sockets {
        dir: tempfile::TempDir,
        _listeners: Vec<UnixListener>,
    }

    impl Sockets {
        fn new(names: &[&str]) -> Self {
            let dir = tempfile::Builder::new()
                .prefix("a")
                .tempdir_in("/tmp")
                .expect("tempdir");
            let listeners = names
                .iter()
                .map(|name| UnixListener::bind(dir.path().join(name)).expect("binds"))
                .collect();
            Self {
                dir,
                _listeners: listeners,
            }
        }

        fn path(&self, name: &str) -> PathBuf {
            self.dir.path().join(name)
        }
    }

    fn decided(inherited: &[(&str, OsString)], stable: &[PathBuf]) -> AgentSocket {
        let inherited: Vec<_> = inherited
            .iter()
            .map(|(name, value)| (OsString::from(name), value.clone()))
            .collect();
        AgentSocket::decide(&inherited, stable, Uid::current())
    }

    #[test]
    fn outside_an_ssh_login_the_spawners_own_agent_is_kept_when_it_is_our_socket() {
        let sockets = Sockets::new(&["agent"]);
        let mine = sockets.path("agent");
        assert_eq!(
            decided(&[("SSH_AUTH_SOCK", mine.clone().into_os_string())], &[]),
            AgentSocket::Use(mine)
        );
    }

    #[test]
    fn an_agent_that_is_not_there_or_not_a_socket_is_unset() {
        let sockets = Sockets::new(&[]);
        let file = sockets.path("plain");
        std::fs::write(&file, "x").expect("writes");
        for path in [sockets.path("missing"), file] {
            assert_eq!(
                decided(&[("SSH_AUTH_SOCK", path.into_os_string())], &[]),
                AgentSocket::Unset
            );
        }
    }

    #[test]
    fn a_link_to_our_socket_is_kept_as_the_link() {
        let sockets = Sockets::new(&["agent"]);
        let link = sockets.path("agent.123");
        std::os::unix::fs::symlink(sockets.path("agent"), &link).expect("symlink");
        assert_eq!(
            decided(&[("SSH_AUTH_SOCK", link.clone().into_os_string())], &[]),
            AgentSocket::Use(link)
        );
    }

    #[test]
    fn a_chain_of_links_is_followed_to_its_end() {
        let sockets = Sockets::new(&["agent"]);
        let (first, second) = (sockets.path("one"), sockets.path("two"));
        std::os::unix::fs::symlink(sockets.path("agent"), &first).expect("symlink");
        std::os::unix::fs::symlink(&first, &second).expect("symlink");
        assert_eq!(
            decided(&[("SSH_AUTH_SOCK", second.clone().into_os_string())], &[]),
            AgentSocket::Use(second)
        );
    }

    #[test]
    fn a_link_that_dangles_or_leads_to_a_file_is_refused() {
        let sockets = Sockets::new(&[]);
        let file = sockets.path("plain");
        std::fs::write(&file, "x").expect("writes");
        let dangling = sockets.path("dangling");
        std::os::unix::fs::symlink(sockets.path("gone"), &dangling).expect("symlink");
        let to_file = sockets.path("to_file");
        std::os::unix::fs::symlink(&file, &to_file).expect("symlink");
        for link in [dangling, to_file] {
            assert_eq!(
                decided(&[("SSH_AUTH_SOCK", link.into_os_string())], &[]),
                AgentSocket::Unset
            );
        }
    }

    #[test]
    fn a_socket_that_belongs_to_someone_else_is_refused_at_the_end_of_a_link_too() {
        let sockets = Sockets::new(&["agent"]);
        let link = sockets.path("link");
        std::os::unix::fs::symlink(sockets.path("agent"), &link).expect("symlink");
        let stranger = Uid::from_raw(Uid::current().as_raw().wrapping_add(1));
        for path in [sockets.path("agent"), link] {
            let inherited = vec![(OsString::from("SSH_AUTH_SOCK"), path.into_os_string())];
            assert_eq!(
                AgentSocket::decide(&inherited, &[], stranger),
                AgentSocket::Unset
            );
        }
    }

    #[test]
    fn with_no_agent_or_one_that_is_refused_a_stable_one_is_used_outside_a_login_too() {
        let sockets = Sockets::new(&["stable"]);
        let stable = [sockets.path("stable")];
        assert_eq!(
            decided(&[("PATH", OsString::from("/bin"))], &stable),
            AgentSocket::Use(sockets.path("stable"))
        );
        assert_eq!(
            decided(
                &[("SSH_AUTH_SOCK", sockets.path("missing").into_os_string())],
                &stable
            ),
            AgentSocket::Use(sockets.path("stable"))
        );
    }

    #[test]
    fn over_ssh_the_forwarded_agent_is_dropped_for_a_stable_one_that_exists() {
        let sockets = Sockets::new(&["forwarded", "stable"]);
        let inherited = [
            ("SSH_CONNECTION", OsString::from("1.2.3.4 1 5.6.7.8 22")),
            ("SSH_AUTH_SOCK", sockets.path("forwarded").into_os_string()),
        ];
        let stable = [sockets.path("absent"), sockets.path("stable")];
        assert_eq!(
            decided(&inherited, &stable),
            AgentSocket::Use(sockets.path("stable"))
        );
        assert_eq!(decided(&inherited, &[]), AgentSocket::Unset);
    }

    #[test]
    fn the_decided_agent_is_what_the_program_gets() {
        let sockets = Sockets::new(&["agent"]);
        let mine = sockets.path("agent");
        let environment = ChildEnvironment::from_spawner(
            "s".parse().expect("a name"),
            pairs(&[
                ("SSH_AUTH_SOCK", mine.to_str().expect("utf-8")),
                ("PATH", "/bin"),
            ]),
        );
        let built = environment.build();
        let found: Vec<_> = built
            .iter()
            .filter(|(name, _)| name == "SSH_AUTH_SOCK")
            .collect();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].1, mine.into_os_string());
    }
}
