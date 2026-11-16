//! The process table and where a client's window lives, from its ancestry.
//! Shared by every session host that has clients to name.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use agent_core::ports::{Place, TerminalApp};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

pub fn program_is(argv: &[String], program: &str) -> bool {
    argv.first()
        .and_then(|exe| Path::new(exe).file_name())
        .is_some_and(|n| n == program)
}

/// What the scan keeps of a process: its parent, the program it was started
/// as, the kernel's name for it (cut short on Linux), and the argv of the
/// programs that matter to it.
pub struct Process {
    parent: Option<u32>,
    program: String,
    comm: String,
    argv: Vec<String>,
}

impl Process {
    pub fn argv(&self) -> &[String] {
        &self.argv
    }
}

/// The program an argv[0] names: its basename, cut at the `:` some daemons
/// retitle themselves with (`sshd: user@pts/0`).
pub fn program_name(argv0: &str) -> &str {
    let head = argv0.split(':').next().unwrap_or(argv0);
    head.rsplit('/')
        .next()
        .unwrap_or(head)
        .trim_start_matches('-')
}

/// How long [`Processes::scan_shared`] hands out the scan it took, so the
/// hosts a look asks share one pass over the process table.
const SHARED_FOR: Duration = Duration::from_millis(300);

static SHARED: Mutex<Option<(Instant, Arc<Processes>)>> = Mutex::new(None);

/// Every process, for walking up from a client.
pub struct Processes {
    all: HashMap<u32, Process>,
}

impl Processes {
    /// A scan taken within the last [`SHARED_FOR`], or a new one. A look
    /// that asks several hosts for their windows takes one scan.
    pub fn scan_shared() -> Arc<Self> {
        let mut shared = SHARED.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((taken, scan)) = shared.as_ref()
            && taken.elapsed() < SHARED_FOR
        {
            return Arc::clone(scan);
        }
        let scan = Arc::new(Self::scan());
        *shared = Some((Instant::now(), Arc::clone(&scan)));
        scan
    }

    /// Every process with its id.
    pub fn iter(&self) -> impl Iterator<Item = (u32, &Process)> {
        self.all.iter().map(|(pid, process)| (*pid, process))
    }

    pub fn scan() -> Self {
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
    pub fn lineage(&self, mut pid: u32) -> Vec<u32> {
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

    pub fn argv(&self, pid: u32) -> &[String] {
        self.all.get(&pid).map_or(&[], |p| &p.argv)
    }

    /// Whether any of `lineage` is an `agent` that `is_tui` says runs a TUI.
    pub fn under_tui(&self, lineage: &[u32], is_tui: fn(&[String]) -> bool) -> bool {
        lineage.iter().any(|pid| {
            let argv = self.argv(*pid);
            program_is(argv, "agent") && is_tui(&argv[1..])
        })
    }

    /// The nearest ancestor of `client` that names a place. With none, a
    /// client that has no shell above it either was left behind by a window
    /// that went away.
    pub fn place_of(&self, client: u32) -> Place {
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
pub fn place_named(program: &str) -> Option<Place> {
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

#[cfg(feature = "testing")]
pub mod testing {
    use super::{Process, Processes, program_name};

    impl Processes {
        /// A table from `(pid, parent, command line)` rows.
        pub fn from_lines(rows: &[(u32, u32, &str)]) -> Self {
            let all = rows
                .iter()
                .map(|(pid, parent, line)| {
                    let argv: Vec<String> = shell_words(line);
                    let program = argv.first().map_or("", |a| program_name(a)).to_string();
                    let process = Process {
                        parent: Some(*parent),
                        comm: program.clone(),
                        program,
                        argv,
                    };
                    (*pid, process)
                })
                .collect();
            Self { all }
        }

        /// Names `pid` as the kernel and its argv[0] would, for a program
        /// that retitled itself or whose name the kernel cut short.
        pub fn retitle(&mut self, pid: u32, program: Option<&str>, comm: &str) {
            if let Some(process) = self.all.get_mut(&pid) {
                comm.clone_into(&mut process.comm);
                if let Some(program) = program {
                    program.clone_into(&mut process.program);
                }
            }
        }
    }

    fn shell_words(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_owned).collect()
    }
}

#[cfg(test)]
mod tests {
    use agent_core::ports::{Place, TerminalApp};

    use super::*;

    fn table() -> Processes {
        Processes::from_lines(&[
            (1, 0, "launchd"),
            (
                10,
                1,
                "/Applications/WezTerm.app/Contents/MacOS/wezterm-gui",
            ),
            (11, 10, "-fish"),
            (12, 11, "nvim"),
            (13, 12, "agent attach fix"),
        ])
    }

    #[test]
    fn a_lineage_runs_from_the_process_up_to_init() {
        assert_eq!(table().lineage(13), [13, 12, 11, 10, 1, 0]);
    }

    #[test]
    fn the_nearest_named_ancestor_is_the_place() {
        assert_eq!(table().place_of(13), Place::Nvim);
        assert_eq!(
            table().place_of(12),
            Place::Terminal(TerminalApp::new("wezterm"))
        );
    }

    #[test]
    fn a_process_with_no_shell_above_it_was_left_behind() {
        let procs = Processes::from_lines(&[(1, 0, "launchd"), (11, 1, "agent attach fix")]);
        assert_eq!(procs.place_of(11), Place::Detached);
    }

    #[test]
    fn a_retitled_daemon_is_named_by_its_program_and_a_cut_name_by_its_comm() {
        assert_eq!(program_name("sshd: daniel@pts/0"), "sshd");
        assert_eq!(program_name("-fish"), "fish");
        let mut procs =
            Processes::from_lines(&[(1, 0, "launchd"), (10, 1, "x"), (11, 10, "agent a")]);
        procs.retitle(10, Some("unrelated"), "sshd-session");
        assert_eq!(procs.place_of(11), Place::Ssh);
    }

    #[test]
    fn only_an_agent_running_a_tui_is_a_tui_in_the_lineage() {
        let procs = Processes::from_lines(&[
            (1, 0, "launchd"),
            (10, 1, "/bin/agent tui"),
            (11, 10, "zellij attach fix"),
            (20, 1, "/bin/agent attach fix"),
            (21, 20, "zellij attach fix"),
        ]);
        let is_tui = |args: &[String]| args.first().is_some_and(|a| a == "tui");
        assert!(procs.under_tui(&procs.lineage(11), is_tui));
        assert!(!procs.under_tui(&procs.lineage(21), is_tui));
    }

    #[test]
    fn a_real_scan_names_processes_by_program_and_links_them_to_their_parents() {
        let procs = Processes::scan();
        let me = std::process::id();
        let (_, process) = procs
            .iter()
            .find(|(pid, _)| *pid == me)
            .expect("this process");
        assert_ne!(process.program, "");
        assert!(procs.lineage(me).len() > 1);
    }

    #[test]
    fn a_scan_is_shared_between_calls_that_follow_each_other() {
        assert!(Arc::ptr_eq(
            &Processes::scan_shared(),
            &Processes::scan_shared()
        ));
    }
}
