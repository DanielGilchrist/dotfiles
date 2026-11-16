//! The native session host as a [`SessionHost`]: records and locks in the
//! state directory say what exists and what runs, a host's socket answers
//! the rest (docs/host.md).

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use agent_core::config::{Detach, HostKind, Osc52};
use agent_core::model::Running;
use agent_core::names::{HostName, NameLimit, SessionName};
use agent_core::ports::{
    Attachment, ClientControl, ClientId, Extent, HostError, Liveness, Observation, PaneCommand,
    Rendering, SessionHost, SessionInfo, SessionStatus, Window, WindowHandle,
};
use agent_core::process::{LaunchSpec, parallel_map};
use agent_host::{ClientInfo, ClientRef, Purpose, Scrollback, Style};
use agent_places::Processes;
use agent_session::{
    Control, ControlError, Creation, HostFiles, HostRecord, RuntimeDir, forget, probe,
};

/// The longest session name the native host takes before the socket path
/// budget of the runtime directory is looked at.
const NAME_CAP: NonZeroUsize = NonZeroUsize::new(48).expect("nonzero");

/// How long `kill` waits for the host to let go of its lock. The host
/// signals the program's group, and gives it 2 s before `SIGKILL`.
const KILL_WITHIN: Duration = Duration::from_secs(6);

const KILL_POLL: Duration = Duration::from_millis(25);

/// How long a look for windows waits on any host. The hosts are asked at
/// once, so it bounds the whole look.
const LOOK_WITHIN: Duration = Duration::from_secs(2);

pub struct Native {
    state: PathBuf,
    files: HostFiles,
    runtime: RuntimeDir,
    exe: PathBuf,
    osc52: Osc52,
    detach: Detach,
    is_tui: fn(&[String]) -> bool,
}

impl Native {
    /// `state` is the directory `FsStore` keeps, `<XDG_STATE_HOME>/agent`.
    /// The host processes this starts find it again by that variable, so
    /// they read the same records.
    pub fn new(
        state: PathBuf,
        runtime: RuntimeDir,
        exe: PathBuf,
        osc52: Osc52,
        detach: Detach,
        is_tui: fn(&[String]) -> bool,
    ) -> Self {
        Self {
            files: HostFiles::under(&state),
            state,
            runtime,
            exe,
            osc52,
            detach,
            is_tui,
        }
    }

    fn socket(&self, name: &SessionName) -> Result<agent_session::SocketPath, HostError> {
        let runtime = self
            .runtime
            .open(Creation::Existing)
            .map_err(failed(format!("opening the runtime directory for {name}")))?;
        runtime
            .socket(name)
            .map_err(failed(format!("finding the socket of {name}")))
    }

    fn control(&self, name: &SessionName) -> Result<Control, HostError> {
        Control::connect(&self.socket(name)?).map_err(failed(format!("reaching {name}")))
    }

    fn recorded(&self) -> Result<Vec<(SessionName, Liveness)>, HostError> {
        let dir = self.files.dir();
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(failed(format!("listing {}", dir.display()))(e)),
        };
        let mut found = Vec::new();
        for entry in entries {
            let path = entry
                .map_err(failed(format!("listing {}", dir.display())))?
                .path();
            let Some(name) = record_name(&path) else {
                continue;
            };
            let liveness = match probe(&path).map_err(failed(format!("probing {name}")))? {
                agent_session::Liveness::Live => Liveness::Live,
                agent_session::Liveness::Dead => Liveness::Exited,
                agent_session::Liveness::Unrecorded => continue,
            };
            found.push((name, liveness));
        }
        found.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(found)
    }

    fn live(&self) -> Result<Vec<SessionName>, HostError> {
        Ok(self
            .recorded()?
            .into_iter()
            .filter_map(|(name, liveness)| (liveness == Liveness::Live).then_some(name))
            .collect())
    }

    fn clients_of(&self, name: &SessionName) -> Result<Vec<ClientInfo>, HostError> {
        let mut control = Control::connect_within(&self.socket(name)?, LOOK_WITHIN)
            .map_err(failed(format!("reaching {name}")))?;
        let list = control
            .clients()
            .map_err(failed(format!("listing the clients of {name}")))?;
        Ok(list.as_slice().to_vec())
    }

    fn attach_command(&self, name: &SessionName, flags: &[&str]) -> LaunchSpec {
        LaunchSpec::new(&self.exe)
            .args(["session", "attach", name.as_str()])
            .args(flags.iter().copied())
    }

    fn wait_until_dead(&self, name: &SessionName) -> Result<(), HostError> {
        let record = self.files.record(name);
        let deadline = Instant::now() + KILL_WITHIN;
        loop {
            match probe(&record).map_err(failed(format!("probing {name}")))? {
                agent_session::Liveness::Live => {}
                agent_session::Liveness::Dead | agent_session::Liveness::Unrecorded => {
                    return Ok(());
                }
            }
            if Instant::now() >= deadline {
                return Err(HostError::Io {
                    context: format!("stopping {name}"),
                    source: io::Error::other(format!("still running after {KILL_WITHIN:?}")),
                });
            }
            std::thread::sleep(KILL_POLL);
        }
    }

    fn stop(&self, name: &SessionName) -> Result<(), HostError> {
        let record = self.files.record(name);
        if probe(&record).map_err(failed(format!("probing {name}")))?
            != agent_session::Liveness::Live
        {
            return Ok(());
        }
        match self.control(name) {
            Ok(mut control) => control.kill().map_err(failed(format!("stopping {name}")))?,
            Err(error) => {
                if probe(&record).is_ok_and(|now| now != agent_session::Liveness::Live) {
                    return Ok(());
                }
                return Err(error);
            }
        }
        self.wait_until_dead(name)
    }

    fn host_command(&self, args: &[OsString]) -> Command {
        let mut command = Command::new(&self.exe);
        command
            .args(["session", "host"])
            .args(args)
            .env("AGENT_RUNTIME_DIR", self.runtime.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        if let Some(base) = self.state.parent() {
            command.env("XDG_STATE_HOME", base);
        }
        command
    }

    fn spawn(&self, name: &SessionName, command: Option<&PaneCommand>) -> Result<(), HostError> {
        let runtime = self
            .runtime
            .open(Creation::Create)
            .map_err(failed(format!("opening the runtime directory for {name}")))?;
        runtime
            .socket(name)
            .map_err(failed(format!("finding the socket of {name}")))?;
        self.files
            .create_dir()
            .map_err(failed("creating the host directory"))?;
        let mut args: Vec<OsString> = vec![name.as_str().into(), "--spawn".into()];
        args.extend(["--osc52".into(), self.osc52.word().into()]);
        if let Some(pane) = command {
            args.extend(["--cwd".into(), pane.cwd.clone().into(), "--".into()]);
            args.push(pane.program.clone());
            args.extend(pane.args.iter().map(OsString::from));
        }
        let output = self
            .host_command(&args)
            .output()
            .map_err(failed(format!("starting the host of {name}")))?;
        if output.status.success() {
            return Ok(());
        }
        let said = String::from_utf8_lossy(&output.stderr);
        Err(HostError::Io {
            context: format!("starting the host of {name}"),
            source: io::Error::other(said.trim().to_owned()),
        })
    }

    fn windows_of(
        &self,
        name: &SessionName,
        processes: &Processes,
        own: Option<ClientId>,
    ) -> Result<Vec<Window>, HostError> {
        Ok(self
            .clients_of(name)?
            .into_iter()
            .filter(|client| client.purpose == Some(Purpose::Window))
            .filter_map(|client| {
                let pid = client.pid.get();
                let lineage = processes.lineage(pid);
                let ours = own.is_some_and(|own| lineage.contains(&own.get()));
                (!ours && !processes.under_tui(&lineage, self.is_tui)).then(|| Window {
                    client: ClientId::new(pid),
                    session: name.clone(),
                    place: processes.place_of(pid),
                    handle: WindowHandle::new(HostKind::Native, client.client.get()),
                })
            })
            .collect())
    }
}

fn failed<E: std::fmt::Display>(context: impl Into<String>) -> impl FnOnce(E) -> HostError {
    let context = context.into();
    move |error| HostError::Io {
        context,
        source: io::Error::other(error.to_string()),
    }
}

fn record_name(path: &Path) -> Option<SessionName> {
    if path.extension()? != "json" {
        return None;
    }
    path.file_stem()?.to_str()?.parse().ok()
}

impl SessionHost for Native {
    /// The cap of 48, or what the runtime directory's socket path leaves.
    fn name_limit(&self) -> NameLimit {
        NameLimit::new(NAME_CAP.min(self.runtime.name_budget()))
    }

    fn sessions(&self) -> Result<Vec<SessionInfo>, HostError> {
        self.recorded()?
            .into_iter()
            .map(|(name, liveness)| {
                let record = self.files.record(&name);
                let cwd = HostRecord::read(&record)
                    .ok()
                    .flatten()
                    .map(|record| record.cwd);
                let status = match liveness {
                    Liveness::Live => SessionStatus::Live { cwd },
                    Liveness::Exited => SessionStatus::Exited { cwd },
                };
                Ok(SessionInfo { name, status })
            })
            .collect()
    }

    fn liveness(&self, _: Duration) -> Result<Vec<(SessionName, Liveness)>, HostError> {
        self.recorded()
    }

    fn attach(&self, name: &SessionName, how: Attachment) -> LaunchSpec {
        let start = ["--start", "--osc52", self.osc52.word()];
        match how {
            Attachment::Window => {
                let detach = self.detach.to_string();
                self.attach_command(name, &start)
                    .args(["--detach-key", detach.as_str()])
            }
            Attachment::Preview => {
                self.attach_command(name, &start)
                    .args(["--as", "preview", "--detach-key", "none"])
            }
        }
    }

    fn observe(&self, name: &SessionName) -> Result<Observation, HostError> {
        let size = self.control(name)?.size();
        let spec = self
            .attach_command(name, &["--as", "peek", "--detach-key", "none"])
            .args(["--size".to_owned(), format!("{}x{}", size.cols, size.rows)]);
        Ok(Observation { spec, size })
    }

    fn create(
        &self,
        name: &HostName,
        pane: &PaneCommand,
        how: Attachment,
    ) -> Result<LaunchSpec, HostError> {
        name.within(self.name_limit())?;
        let name = name.session();
        self.spawn(name, Some(pane))?;
        Ok(self.attach(name, how).cwd(&pane.cwd))
    }

    fn start_detached(&self, name: &SessionName) -> Result<(), HostError> {
        let record = self.files.record(name);
        match probe(&record).map_err(failed(format!("probing {name}")))? {
            agent_session::Liveness::Live => Ok(()),
            agent_session::Liveness::Dead => self.spawn(name, None),
            agent_session::Liveness::Unrecorded => Err(HostError::Io {
                context: format!("starting {name} detached"),
                source: io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("no record at {}", record.display()),
                ),
            }),
        }
    }

    fn kill(&self, session: &Running) -> Result<(), HostError> {
        self.stop(session.name())
    }

    fn delete(&self, name: &SessionName) -> Result<(), HostError> {
        self.stop(name)?;
        forget(&self.files.record(name), &self.files.log(name))
            .map_err(failed(format!("forgetting {name}")))
    }

    fn write(&self, name: &SessionName, bytes: &[u8]) -> Result<(), HostError> {
        self.control(name)?
            .inject(bytes)
            .map_err(failed(format!("typing into {name}")))
    }

    fn screen(
        &self,
        name: &SessionName,
        extent: Extent,
        rendering: Rendering,
    ) -> Result<String, HostError> {
        let text = self
            .control(name)?
            .snapshot(
                match extent {
                    Extent::Visible => Scrollback::Screen,
                    Extent::Scrollback => Scrollback::Full,
                },
                match rendering {
                    Rendering::Styled => Style::Styled,
                    Rendering::Plain => Style::Plain,
                },
            )
            .map_err(failed(format!("reading the screen of {name}")))?;
        Ok(String::from_utf8_lossy(&text).into_owned())
    }

    fn clients(&self) -> Option<&dyn ClientControl> {
        Some(self)
    }

    fn own_session(&self, var: &dyn Fn(&str) -> Option<String>) -> Option<SessionName> {
        agent_session::own_session(var)
    }

    fn nesting_env(&self) -> Vec<&'static str> {
        vec![agent_session::SESSION_VAR]
    }
}

impl ClientControl for Native {
    fn windows(&self, own: Option<ClientId>) -> Result<Vec<Window>, HostError> {
        let live = self.live()?;
        if live.is_empty() {
            return Ok(Vec::new());
        }
        let processes = Processes::scan_shared();
        let mut windows: Vec<Window> = parallel_map(&live, |name| {
            self.windows_of(name, &processes, own).unwrap_or_default()
        })
        .into_iter()
        .flatten()
        .collect();
        windows.sort_by_key(|window| window.client.get());
        Ok(windows)
    }

    fn detach(&self, windows: &[Window]) -> Result<(), HostError> {
        let mut by_session: BTreeMap<&SessionName, Vec<(ClientId, ClientRef)>> = BTreeMap::new();
        for window in windows {
            if window.handle.host() != HostKind::Native {
                return Err(HostError::Detach {
                    client: window.client,
                    reason: "it isn't a window of the native host".to_owned(),
                });
            }
            by_session
                .entry(&window.session)
                .or_default()
                .push((window.client, ClientRef::new(window.handle.key())));
        }
        for (session, clients) in by_session {
            let Some(&(first, _)) = clients.first() else {
                continue;
            };
            let control = match self.control(session) {
                Ok(control) => control,
                Err(error) => {
                    let gone = probe(&self.files.record(session))
                        .is_ok_and(|now| now != agent_session::Liveness::Live);
                    if gone {
                        continue;
                    }
                    return Err(error);
                }
            };
            let refs: Vec<ClientRef> = clients.iter().map(|(_, client)| *client).collect();
            control
                .detach(&refs)
                .map_err(|error: ControlError| HostError::Detach {
                    client: first,
                    reason: error.to_string(),
                })?;
        }
        Ok(())
    }
}
