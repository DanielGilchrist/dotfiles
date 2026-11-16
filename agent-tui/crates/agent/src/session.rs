//! The hidden `agent session` commands: the native session host and its raw
//! client (docs/host.md). `Native` starts hosts and attaches clients through
//! them when `[host] kind` is `native`.

use std::ffi::OsString;
use std::io::{IsTerminal, Write};
use std::num::NonZeroU16;
use std::os::fd::AsFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use agent_core::config::{Detach, Osc52};
use agent_core::names::SessionName;
use agent_core::size::TermSize;
use agent_host::{ExitStatus, Purpose, Take};
use agent_session::{
    Console, Creation, DetachKey, Ended, HostConfig, HostFiles, HostRecord, Launch, RuntimeDir,
    ScrollbackCap, Seat, Timing, detach_from_terminal, serve, spawn_host,
};
use agent_store::FsStore;
use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand, ValueEnum};

/// How long `--spawn` waits for the host's socket.
const READY_WITHIN: Duration = Duration::from_secs(10);

/// The exit status of a host that found another host running its session.
pub const EXIT_ALREADY_RUNNING: i32 = 3;

#[derive(Subcommand)]
pub enum SessionCmd {
    /// Run the host of a session in this process (--spawn: in one of its own)
    Host(HostArgs),
    /// Attach this terminal to a session
    Attach(AttachArgs),
}

/// A terminal size as `COLSxROWS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SizeArg(TermSize);

impl FromStr for SizeArg {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        let (cols, rows) = text
            .split_once('x')
            .ok_or_else(|| format!("{text:?} isn't COLSxROWS"))?;
        let dimension = |digits: &str| {
            digits
                .parse::<u16>()
                .ok()
                .and_then(NonZeroU16::new)
                .ok_or_else(|| format!("{digits:?} isn't a size from 1 to 65535"))
        };
        Ok(Self(TermSize {
            rows: dimension(rows)?,
            cols: dimension(cols)?,
        }))
    }
}

#[derive(Args)]
pub struct HostArgs {
    name: SessionName,
    /// Start the host in a process of its own and return once its socket is up
    #[arg(long)]
    spawn: bool,
    /// The program's working directory (defaults to this one)
    #[arg(long)]
    cwd: Option<PathBuf>,
    /// The pty's size until a window states its own
    #[arg(long, value_name = "COLSxROWS", default_value = "80x24")]
    size: SizeArg,
    /// Whether the owner's terminal is sent the program's clipboard writes,
    /// which a starter takes from `[host] osc52` in the config
    #[arg(long, value_name = "off|owner", default_value = "off")]
    osc52: Osc52,
    /// The program and its arguments, recorded for the next start. Without
    /// one the recorded command runs
    #[arg(last = true)]
    command: Vec<String>,
}

/// What a client attaches as.
#[derive(Clone, Copy, ValueEnum)]
pub enum Role {
    /// A window: takes the session over from another window
    Window,
    /// A window that owns the session only when no window does
    Polite,
    /// A preview, which owns the session only while no window does
    Preview,
    /// A window's read-only view: never sizes the session or types
    Watch,
    /// A preview's read-only view
    Peek,
}

impl From<Role> for Seat {
    fn from(role: Role) -> Self {
        match role {
            Role::Window => Self::Window(Take::Over),
            Role::Polite => Self::Window(Take::Politely),
            Role::Preview => Self::Preview,
            Role::Watch => Self::Watch(Purpose::Watch),
            Role::Peek => Self::Watch(Purpose::Preview),
        }
    }
}

#[derive(Args)]
pub struct AttachArgs {
    name: SessionName,
    /// What to attach as
    #[arg(long = "as", value_enum, default_value = "window")]
    role: Role,
    /// Shorthand for --as watch
    #[arg(long, conflicts_with = "role")]
    watch: bool,
    /// The key that leaves the session, or none to pass every key through, as
    /// a program that embeds this client does. A terminal's only
    #[arg(long, value_name = "KEY|none", default_value_t)]
    detach_key: Detach,
    /// The terminal's size when stdin or stdout isn't a terminal, and when
    /// the terminal reports none
    #[arg(long, value_name = "COLSxROWS", default_value = "80x24")]
    size: SizeArg,
    /// Start the session from its record first when no host runs it
    #[arg(long)]
    start: bool,
    /// What `--start` passes the host it starts as `--osc52`
    #[arg(long, value_name = "off|owner", default_value = "off")]
    osc52: Osc52,
}

impl AttachArgs {
    fn seat(&self) -> Seat {
        if self.watch {
            Seat::from(Role::Watch)
        } else {
            Seat::from(self.role)
        }
    }
}

pub fn run(command: SessionCmd) -> Result<()> {
    match command {
        SessionCmd::Host(args) => host(args),
        SessionCmd::Attach(args) => attach(&args),
    }
}

fn files() -> Result<HostFiles> {
    Ok(HostFiles::under(FsStore::from_env()?.dir()))
}

fn runs(name: &SessionName) -> Result<bool> {
    let record = files()?.record(name);
    let liveness = agent_session::probe(&record)
        .with_context(|| format!("looking at {}", record.display()))?;
    Ok(liveness == agent_session::Liveness::Live)
}

fn host(args: HostArgs) -> Result<()> {
    let files = files()?;
    let runtime = RuntimeDir::from_env()?.open(Creation::Create)?;
    runtime.socket(&args.name)?;
    let cwd = match args.cwd {
        Some(cwd) => cwd,
        None => std::env::current_dir().context("reading the current directory")?,
    };
    if args.spawn {
        files.create_dir()?;
        let mut host_args: Vec<OsString> = ["session", "host"].map(OsString::from).to_vec();
        host_args.push(args.name.as_str().into());
        host_args.extend(["--cwd".into(), cwd.into()]);
        let SizeArg(size) = args.size;
        host_args.extend([
            "--size".into(),
            format!("{}x{}", size.cols, size.rows).into(),
        ]);
        host_args.extend(["--osc52".into(), args.osc52.word().into()]);
        if !args.command.is_empty() {
            host_args.push("--".into());
            host_args.extend(args.command.iter().map(OsString::from));
        }
        let exe = std::env::current_exe().context("locating the agent binary")?;
        spawn_host(&exe, &host_args, &args.name, &files, READY_WITHIN)?;
        return Ok(());
    }
    let launch = match args.command.split_first() {
        Some((program, rest)) => Launch::Command(HostRecord {
            program: program.clone(),
            args: rest.to_vec(),
            cwd,
        }),
        None => Launch::Recorded,
    };
    let SizeArg(size) = args.size;
    let name = args.name;
    let ready = std::io::stdout()
        .as_fd()
        .try_clone_to_owned()
        .context("keeping the pipe the host reports readiness on")?;
    let config = HostConfig {
        name: name.clone(),
        files: files.clone(),
        runtime,
        launch,
        size,
        scrollback: ScrollbackCap::DEFAULT,
        osc52: args.osc52,
        inherited: std::env::vars_os().collect(),
        timing: Timing::default(),
        max_connections: None,
        ready: Some(std::fs::File::from(ready)),
    };
    detach_from_terminal()?;
    match serve(config) {
        Ok(_) => Ok(()),
        Err(error) if error.already_running() => std::process::exit(EXIT_ALREADY_RUNNING),
        Err(error) => {
            if let Ok(mut log) = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(files.log(&name))
            {
                let _ = writeln!(log, "{error}");
            }
            std::process::exit(1)
        }
    }
}

fn attach(args: &AttachArgs) -> Result<()> {
    if args.start && !runs(&args.name)? {
        host(HostArgs {
            name: args.name.clone(),
            spawn: true,
            cwd: None,
            size: SizeArg(args.size.0),
            osc52: args.osc52,
            command: Vec::new(),
        })?;
    }
    let runtime = RuntimeDir::from_env()?.open(Creation::Existing)?;
    let socket = runtime.socket(&args.name)?;
    let console = if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
        Console::Tty {
            fallback: args.size.0,
        }
    } else {
        Console::Piped(args.size.0)
    };
    let ended = agent_session::run(
        &socket,
        args.seat(),
        detach_for(args.detach_key, console),
        console,
    )?;
    finish(&args.name, ended)
}

/// Only a terminal has a key to leave with. Piped input is data, so a
/// detach byte in it reaches the program.
fn detach_for(detach: Detach, console: Console) -> Option<DetachKey> {
    match (detach, console) {
        (Detach::On(key), Console::Tty { .. }) => Some(DetachKey::new(key)),
        (Detach::Off, _) | (Detach::On(_), Console::Piped(_)) => None,
    }
}

fn finish(name: &SessionName, ended: Ended) -> Result<()> {
    match ended {
        Ended::Detached => Ok(()),
        Ended::DetachedBy(_) => {
            eprintln!("agent: detached from {name} by another client");
            Ok(())
        }
        Ended::Exited(status) => {
            eprintln!("agent: {name} exited");
            std::process::exit(exit_code(status))
        }
        Ended::Terminated(signal) => std::process::exit(128 + signal),
        Ended::HostGone => bail!("the host of {name} went away"),
    }
}

fn exit_code(status: ExitStatus) -> i32 {
    match status {
        ExitStatus::Code(code) => i32::from(code),
        ExitStatus::Signal(signal) => 128 + i32::from(signal.get()),
        ExitStatus::Unknown => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_size_is_columns_then_rows() {
        let SizeArg(size) = "120x40".parse().expect("parses");
        assert_eq!((size.cols.get(), size.rows.get()), (120, 40));
        for bad in ["80", "0x24", "80x0", "axb", "80x24x1", "70000x24"] {
            assert!(bad.parse::<SizeArg>().is_err(), "{bad}");
        }
    }

    #[derive(clap::Parser)]
    struct Wrapper {
        #[command(flatten)]
        args: AttachArgs,
    }

    fn seat(flags: &[&str]) -> Seat {
        use clap::Parser;
        let argv = ["x", "name"].iter().chain(flags);
        Wrapper::parse_from(argv).args.seat()
    }

    #[test]
    fn one_flag_picks_the_seat_and_no_other_flag_can_contradict_it() {
        use clap::Parser;
        assert_eq!(seat(&[]), Seat::Window(Take::Over));
        assert_eq!(seat(&["--as", "window"]), Seat::Window(Take::Over));
        assert_eq!(seat(&["--as", "polite"]), Seat::Window(Take::Politely));
        assert_eq!(seat(&["--as", "preview"]), Seat::Preview);
        assert_eq!(seat(&["--as", "watch"]), Seat::Watch(Purpose::Watch));
        assert_eq!(seat(&["--as", "peek"]), Seat::Watch(Purpose::Preview));
        assert_eq!(seat(&["--watch"]), Seat::Watch(Purpose::Watch));
        for old in ["--take", "--polite", "--purpose"] {
            assert!(
                Wrapper::try_parse_from(["x", "name", old]).is_err(),
                "{old}"
            );
        }
        assert!(Wrapper::try_parse_from(["x", "name", "--watch", "--as", "polite"]).is_err());
    }

    #[test]
    fn only_a_terminal_with_a_detach_key_has_one() {
        let SizeArg(size) = "80x24".parse().expect("parses");
        let tty = Console::Tty { fallback: size };
        let piped = Console::Piped(size);
        let key = Detach::default();
        assert!(detach_for(key, piped).is_none());
        assert!(detach_for(Detach::Off, piped).is_none());
        assert!(detach_for(Detach::Off, tty).is_none());
        assert!(detach_for(key, tty).is_some());
    }

    #[test]
    fn detach_key_none_is_the_one_way_to_pass_every_key_through() {
        use clap::Parser;
        assert!(Wrapper::try_parse_from(["x", "name", "--no-detach-key"]).is_err());
        let args = Wrapper::parse_from(["x", "name", "--detach-key", "none"]).args;
        assert_eq!(args.detach_key, Detach::Off);
    }
}
