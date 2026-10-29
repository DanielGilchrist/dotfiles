use std::ffi::OsStr;
use std::io::{BufRead, IsTerminal, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use agent_core::config::Config;
use agent_core::model::Presence;
use agent_core::names::{AgentName, BranchName, Prompt, SessionName};
use agent_core::ops::{self, Backend, Consent, Delivery, Launch, Removed, Services};
use agent_core::paths::{self, Layout, RepoRoot};
use agent_core::ports::{Harness, HarnessId};
use agent_git::Git;
use agent_harness::Registry;
use agent_store::FsStore;
use agent_zellij::Zellij;
use anyhow::{Context, Result, bail};
use clap::{ArgGroup, CommandFactory, Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "agent",
    version,
    about = "Coding agents in git worktrees, persisted in zellij sessions"
)]
struct Cli {
    /// Harness for new agents (e.g. claude)
    #[arg(long, global = true)]
    harness: Option<HarnessId>,

    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Open the TUI (the default)
    Tui,
    /// Attach to the session for the repo's main checkout, starting it if needed
    Repo {
        /// The repo (defaults to the one containing the cwd)
        #[arg(long)]
        repo: Option<PathBuf>,
        /// Print `headless_session:`/`headless_cwd:`/`headless_cmd:` instead of attaching
        #[arg(long)]
        headless: bool,
    },
    /// Print shell completions
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// Create or attach the agent <name>: its worktree, branch and session
    #[command(group = ArgGroup::new("initial_prompt").args(["prompt", "seed", "no_prompt"]))]
    Attach {
        name: AgentName,
        /// First prompt for a new agent, or typed into a running one
        #[arg(short = 'e', long)]
        prompt: Option<Prompt>,
        /// Read the prompt from a file
        #[arg(long)]
        seed: Option<PathBuf>,
        /// Start without a prompt and without opening $EDITOR
        #[arg(long)]
        no_prompt: bool,
        /// Repo for a new worktree (defaults to the one containing the cwd)
        #[arg(long)]
        repo: Option<PathBuf>,
        /// Print `headless_session:`/`headless_cwd:`/`headless_cmd:` instead of attaching
        #[arg(long)]
        headless: bool,
        /// Accepted from older callers. There's no pane to focus any more
        #[arg(long, hide = true)]
        no_focus: bool,
    },
    /// Attach the worktree for <branch>, creating it from a local or origin branch
    #[command(visible_alias = "co")]
    Checkout {
        branch: BranchName,
        /// Agent name (defaults to the branch with '/' replaced by '-')
        #[arg(long)]
        name: Option<AgentName>,
        #[arg(long)]
        repo: Option<PathBuf>,
        #[arg(long)]
        headless: bool,
        #[arg(long, hide = true)]
        no_focus: bool,
    },
    /// Remove an agent's session, worktree and branch (inferred from the cwd)
    Rm {
        name: Option<SessionName>,
        /// Discard unmerged commits
        #[arg(short, long)]
        force: bool,
        /// Every agent worktree (asks first)
        #[arg(short, long, conflicts_with = "name")]
        all: bool,
    },
    /// List agents by repo
    Ls,
    /// List agent worktrees whose branch can be removed without losing work
    Merged {
        #[arg(short, long)]
        verbose: bool,
    },
    /// Print an agent's screen with ANSI styling
    Dump { name: SessionName },
    /// Type a prompt into a running agent
    #[command(group = ArgGroup::new("input").args(["text", "file"]).required(true))]
    Prompt {
        name: SessionName,
        text: Option<Prompt>,
        #[arg(long)]
        file: Option<PathBuf>,
    },
    /// Pin an agent to the top of the TUI and a 1-9 slot (inferred from the cwd)
    Pin { name: Option<SessionName> },
    /// Unpin an agent
    Unpin { name: Option<SessionName> },
    /// Run the harness for a session's pane (used in session layouts)
    #[command(hide = true)]
    Exec { name: SessionName },

    /// Finish deleting removed worktrees
    #[command(hide = true)]
    Sweep { worktrees: Vec<PathBuf> },
}

/// Where a new agent's first prompt comes from.
enum Seed {
    Text(Prompt),
    File(PathBuf),
    /// Ask in $EDITOR when a new worktree is created interactively.
    Editor,
    None,
}

impl Seed {
    fn from_args(prompt: Option<Prompt>, seed: Option<PathBuf>, no_prompt: bool) -> Self {
        match (prompt, seed, no_prompt) {
            (Some(prompt), _, _) => Self::Text(prompt),
            (None, Some(file), _) => Self::File(file),
            (None, None, true) => Self::None,
            (None, None, false) => Self::Editor,
        }
    }
}

fn backend() -> Result<Backend> {
    let layout = Layout::from_env()?;
    let store = FsStore::from_env()?;
    let host = Zellij::new(store.dir().join("layouts"));
    let exe = std::env::current_exe().context("locating the agent binary")?;
    Ok(Backend {
        vcs: Box::new(Git),
        host: Box::new(host),
        harnesses: Box::new(Registry::new(&paths::home()?)),
        store: Box::new(store),
        layout,
        exe: exe.canonicalize().unwrap_or(exe),
    })
}

/// `$XDG_CONFIG_HOME/agent/config.toml` (default `~/.config`), or the
/// defaults if there's no file.
fn config() -> Result<Config> {
    let dir = match std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|d| d.is_absolute())
    {
        Some(dir) => dir,
        None => paths::home()?.join(".config"),
    };
    let path = dir.join("agent").join("config.toml");
    match std::fs::read_to_string(&path) {
        Ok(text) => text
            .parse()
            .with_context(|| format!("in {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

fn cwd() -> Result<PathBuf> {
    std::env::current_dir().context("reading the current directory")
}

fn repo_arg(svc: Services<'_>, repo: Option<PathBuf>) -> Result<Option<RepoRoot>> {
    let dir = match repo {
        Some(repo) => repo,
        None => cwd()?,
    };
    Ok(ops::repo_for(svc, &dir)?)
}

/// The session for the agent worktree or repo checkout containing the cwd.
fn infer_name(svc: Services<'_>, name: Option<SessionName>) -> Result<SessionName> {
    if let Some(name) = name {
        return Ok(name);
    }
    let cwd = cwd()?;
    if let Some(wt) = svc.layout.enclosing(&cwd) {
        return Ok(wt.name().session());
    }
    let (snapshot, _) = ops::snapshot(svc)?;
    snapshot
        .containing(&cwd)
        .map(|(_, e)| e.name().clone())
        .context("not inside an agent's checkout (pass a name)")
}

fn fish_quote(s: &OsStr) -> String {
    format!(
        "'{}'",
        s.to_string_lossy()
            .replace('\\', "\\\\")
            .replace('\'', "\\'")
    )
}

fn read_prompt(file: &Path) -> Result<Prompt> {
    let text =
        std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
    text.parse()
        .with_context(|| format!("{} is empty", file.display()))
}

fn edit_prompt(name: &AgentName) -> Result<Option<Prompt>> {
    let file = tempfile::Builder::new()
        .prefix(&format!("agent-{name}-"))
        .suffix(".md")
        .tempfile()?;
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "nvim".into());
    let argv =
        shell_words::split(&editor).with_context(|| format!("parsing $EDITOR ({editor})"))?;
    let (program, args) = argv.split_first().context("$EDITOR is empty")?;
    let status = Command::new(program)
        .args(args)
        .arg(file.path())
        .status()
        .with_context(|| format!("running {program}"))?;
    if !status.success() {
        return Ok(None);
    }
    Ok(std::fs::read_to_string(file.path())?.parse().ok())
}

/// Where a launched session ends up.
#[derive(Clone, Copy)]
enum Frontend {
    /// Print how to start it (`--headless`) for the caller to run.
    Headless,
    /// No terminal to attach (an LLM's shell, a script): start it detached.
    Detached,
    /// Attach this terminal.
    Foreground,
}

impl Frontend {
    fn from_flag(headless: bool) -> Self {
        match (headless, std::io::stdin().is_terminal()) {
            (true, _) => Self::Headless,
            (false, false) => Self::Detached,
            (false, true) => Self::Foreground,
        }
    }
}

fn finish(svc: Services<'_>, launch: &Launch, frontend: Frontend) -> Result<()> {
    if launch.delivery == Delivery::Injected {
        println!("agent: sent the prompt to {}", launch.session);
    }
    match frontend {
        Frontend::Headless => headless(launch),
        Frontend::Detached => {
            svc.host.start_detached(&launch.session)?;
            println!(
                "agent: {} is running. Attach with `zellij attach {}`",
                launch.session, launch.session
            );
            Ok(())
        }
        Frontend::Foreground => {
            let status = launch
                .spec
                .command()
                .status()
                .with_context(|| format!("running {}", launch.spec.program().display()))?;
            if !status.success() {
                bail!("{} exited with {status}", launch.spec.program().display());
            }
            Ok(())
        }
    }
}

fn headless(launch: &Launch) -> Result<()> {
    let cwd = match launch.spec.working_dir() {
        Some(dir) => dir.to_path_buf(),
        None => cwd()?,
    };
    let argv: Vec<String> = launch.spec.argv().map(fish_quote).collect();
    println!("headless_session:{}", launch.session);
    println!("headless_cwd:{}", cwd.display());
    println!("headless_cmd:{}", argv.join(" "));
    Ok(())
}

fn attach(
    svc: Services<'_>,
    name: &AgentName,
    seed: Seed,
    repo: Option<&RepoRoot>,
    frontend: Frontend,
    harness: &dyn Harness,
) -> Result<()> {
    let plan = ops::plan(svc, name, repo)?;
    let task = match (seed, frontend, plan.new_worktree()) {
        (Seed::Text(prompt), ..) => Some(prompt),
        (Seed::File(file), ..) => Some(read_prompt(&file)?),
        (Seed::Editor, Frontend::Foreground, Some(_)) => {
            let Some(prompt) = edit_prompt(name)? else {
                println!("agent: cancelled (empty prompt)");
                return Ok(());
            };
            Some(prompt)
        }
        (Seed::Editor | Seed::None, ..) => None,
    };
    let launch = ops::execute(svc, plan, task.as_ref(), harness)?;
    finish(svc, &launch, frontend)
}

fn own_session() -> Option<SessionName> {
    std::env::var("ZELLIJ_SESSION_NAME")
        .ok()
        .and_then(|s| s.parse().ok())
}

fn consent(force: bool) -> Consent {
    if force {
        Consent::Discard
    } else {
        Consent::Clean
    }
}

fn remove(svc: Services<'_>, name: &SessionName, consent: Consent) -> Result<String> {
    let own = own_session();
    let plan = ops::plan_removal(svc, ops::removable(svc, name)?, own.as_ref())?;
    let approved = match (plan.approve(), consent) {
        (Ok(approved), _) => approved,
        (Err(hazardous), Consent::Discard) => hazardous.force(),
        (Err(hazardous), Consent::Clean) => {
            let why: Vec<String> = hazardous
                .plan()
                .hazards()
                .iter()
                .map(ToString::to_string)
                .collect();
            bail!(
                "refusing to remove {name}: {} (re-run with --force to discard)",
                why.join(", ")
            );
        }
    };
    if own.as_ref() == Some(name) {
        return remove_own(&approved);
    }
    Ok(match ops::remove(svc, approved, None)? {
        Removed::Session(name) => format!("killed {name}. It has no worktree to remove"),
        Removed::Agent {
            worktree, warnings, ..
        } => {
            ops::spawn_sweep(svc, std::slice::from_ref(&worktree))?;
            for warning in &warnings {
                eprintln!("agent rm: warning: {warning}");
            }
            format!("removed {}", worktree.name())
        }
    })
}

/// Removing the session this runs in would kill the removal halfway, so
/// hand it to a detached `agent rm` outside the session's process group.
fn remove_own(approved: &ops::Approved) -> Result<String> {
    let name = approved.name();
    let force = match approved.consent() {
        Consent::Clean => None,
        Consent::Discard => Some("--force"),
    };
    Command::new(std::env::current_exe().context("finding the agent binary")?)
        .arg("rm")
        .arg(name.as_str())
        .args(force)
        .env_remove("ZELLIJ_SESSION_NAME")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn()
        .context("starting the removal")?;
    Ok(format!(
        "removing {name} in the background. This session is about to end"
    ))
}

fn rm_all(svc: Services<'_>, consent: Consent) -> Result<()> {
    let worktrees = svc.layout.scan().context("scanning worktrees")?;
    if worktrees.is_empty() {
        println!("agent rm: nothing to remove");
        return Ok(());
    }
    println!("agent rm --all will remove:");
    for wt in &worktrees {
        println!("  {}", wt.path().display());
    }
    print!("type 'yes' to proceed: ");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    if answer.trim() != "yes" {
        bail!("aborted");
    }
    let mut failed = 0;
    for wt in &worktrees {
        match remove(svc, &wt.name().session(), consent) {
            Ok(message) => println!("agent rm: {message}"),
            Err(e) => {
                failed += 1;
                eprintln!("agent rm: {}: {e:#}", wt.name());
            }
        }
    }
    if failed > 0 {
        bail!("{failed} agent(s) not removed");
    }
    Ok(())
}

fn ls(svc: Services<'_>) -> Result<()> {
    let (snapshot, warnings) = ops::snapshot(svc)?;
    for warning in warnings {
        eprintln!("agent: {warning}");
    }
    let rows: Vec<[String; 5]> = snapshot
        .entries()
        .map(|(repo, e)| {
            let state = match e.presence() {
                Some(Presence::Live) => "live",
                Some(Presence::Exited) => "exited",
                None => "idle",
            };
            let pin = match e.pin() {
                Some(pin) => pin
                    .slot()
                    .map_or_else(|| "*".to_string(), |s| s.get().to_string()),
                None => String::new(),
            };
            let path = e
                .path()
                .map_or_else(|| "-".into(), |p| p.display().to_string());
            [
                repo.name().into_owned(),
                e.name().to_string(),
                state.into(),
                pin,
                path,
            ]
        })
        .collect();
    if rows.is_empty() {
        println!("no agents");
        return Ok(());
    }
    let header = ["REPO", "NAME", "STATE", "PIN", "PATH"].map(String::from);
    let widths: Vec<usize> = (0..5)
        .map(|i| {
            rows.iter()
                .chain([&header])
                .map(|r| r[i].chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    for row in std::iter::once(&header).chain(&rows) {
        let cells: Vec<String> = row
            .iter()
            .zip(&widths)
            .map(|(c, w)| format!("{c:<w$}"))
            .collect();
        println!("{}", cells.join("  ").trim_end());
    }
    Ok(())
}

fn exec(svc: Services<'_>, name: &SessionName) -> Result<()> {
    let spec = ops::exec_spec(svc, name, &cwd()?)?;
    let error = spec.command().exec();
    Err(error).with_context(|| format!("running {}", spec.program().display()))
}

/// `agent event <session>`, run by an agent's harness hooks with the hook's
/// JSON payload on stdin. Handled before argument parsing and always exits
/// 0: a failure would surface as a hook error in the agent, and exit
/// status 2 would block what it was about to do.
fn hook_event(name: Option<&std::ffi::OsString>) -> ! {
    let name = name
        .and_then(|n| n.to_str())
        .and_then(|n| n.parse::<SessionName>().ok());
    if let (Some(name), Ok(backend)) = (name, backend()) {
        event(backend.services(), &name);
    }
    std::process::exit(0)
}

/// Records a hook's payload as the agent's activity.
fn event(svc: Services<'_>, name: &SessionName) {
    let mut payload = String::new();
    if std::io::stdin().read_to_string(&mut payload).is_err() {
        return;
    }
    let harness = match svc.store.record(name) {
        Ok(Some(record)) => svc.harnesses.get(&record.harness),
        _ => None,
    }
    .unwrap_or_else(|| svc.harnesses.default_harness());
    if let Some(event) = harness.event(&payload) {
        let _ = svc.store.report(name, event);
    }
}

fn sweep(svc: Services<'_>, paths: &[PathBuf]) -> Result<()> {
    std::thread::sleep(std::time::Duration::from_secs(5));
    let worktrees: Vec<_> = paths
        .iter()
        .filter_map(|p| svc.layout.worktree_at(p))
        .collect();
    Ok(ops::sweep(svc.layout, &worktrees)?)
}

fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    if args.next().is_some_and(|first| first == "event") {
        hook_event(args.next().as_ref());
    }
    let cli = Cli::parse();
    let backend = backend()?;
    let svc = backend.services();
    let harness: &dyn Harness = match &cli.harness {
        Some(id) => svc.harnesses.get(id).with_context(|| {
            let known: Vec<String> = svc
                .harnesses
                .all()
                .iter()
                .map(|h| h.id().to_string())
                .collect();
            format!("unknown harness {id} (known: {})", known.join(", "))
        })?,
        None => svc.harnesses.default_harness(),
    };
    match cli.command.unwrap_or(Cmd::Tui) {
        Cmd::Tui => agent_tui::run(agent_tui::Options {
            config: config()?,
            own_session: own_session(),
            start_dir: cwd()?,
            harness: harness.id().clone(),
            emulator: agent_term::vt100,
            backend: Arc::new(backend),
        }),
        Cmd::Repo { repo, headless } => {
            let repo = repo_arg(svc, repo)?.ok_or(ops::Error::NoRepo)?;
            finish(
                svc,
                &ops::open_repo(svc, &repo, harness)?,
                Frontend::from_flag(headless),
            )
        }
        Cmd::Completions { shell } => {
            clap_complete::generate(shell, &mut Cli::command(), "agent", &mut std::io::stdout());
            Ok(())
        }
        Cmd::Attach {
            name,
            prompt,
            seed,
            no_prompt,
            repo,
            headless,
            no_focus: _,
        } => {
            let repo = repo_arg(svc, repo)?;
            attach(
                svc,
                &name,
                Seed::from_args(prompt, seed, no_prompt),
                repo.as_ref(),
                Frontend::from_flag(headless),
                harness,
            )
        }
        Cmd::Checkout {
            branch,
            name,
            repo,
            headless,
            no_focus: _,
        } => {
            let repo = repo_arg(svc, repo)?.ok_or(ops::Error::NoRepo)?;
            let name = ops::checkout(svc, &repo, &branch, name)?;
            attach(
                svc,
                &name,
                Seed::None,
                Some(&repo),
                Frontend::from_flag(headless),
                harness,
            )
        }
        Cmd::Rm {
            name: _,
            force,
            all: true,
        } => rm_all(svc, consent(force)),
        Cmd::Rm {
            name,
            force,
            all: false,
        } => {
            let name = infer_name(svc, name)?;
            println!("agent rm: {}", remove(svc, &name, consent(force))?);
            Ok(())
        }
        Cmd::Ls => ls(svc),
        Cmd::Merged { verbose } => {
            for (wt, branch) in ops::merged(svc)? {
                match (verbose, branch) {
                    (true, Some(branch)) => println!("{}  ({branch})", wt.name()),
                    (true, None) => println!("{}  (detached)", wt.name()),
                    (false, _) => println!("{}", wt.name()),
                }
            }
            Ok(())
        }
        Cmd::Dump { name } => {
            print!("{}", svc.host.screen(&name)?);
            Ok(())
        }
        Cmd::Prompt { name, text, file } => {
            let prompt = match (text, file) {
                (Some(prompt), _) => prompt,
                (None, Some(file)) => read_prompt(&file)?,
                (None, None) => unreachable!("clap requires text or --file"),
            };
            Ok(ops::inject(svc, &name, &prompt)?)
        }
        Cmd::Pin { name } => Ok(svc.store.pin(&infer_name(svc, name)?)?),
        Cmd::Unpin { name } => Ok(svc.store.unpin(&infer_name(svc, name)?)?),
        Cmd::Exec { name } => exec(svc, &name),

        Cmd::Sweep { worktrees } => sweep(svc, &worktrees),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cli_is_well_formed() {
        Cli::command().debug_assert();
    }

    #[test]
    fn attach_takes_one_kind_of_first_prompt() {
        let parse =
            |args: &[&str]| Cli::try_parse_from(["agent", "attach", "x"].iter().chain(args));
        assert!(parse(&["--no-prompt"]).is_ok());
        assert!(parse(&["--seed", "f"]).is_ok());
        assert!(parse(&["--prompt", "hi"]).is_ok());
        assert!(parse(&["--prompt", "hi", "--seed", "f"]).is_err());
    }
}
