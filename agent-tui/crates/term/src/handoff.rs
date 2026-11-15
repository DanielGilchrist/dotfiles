//! Handing the terminal to another program: `$EDITOR`, an interactive
//! action, a session host's client.
//!
//! A program suspending itself (nvim's Ctrl-Z) stops its whole process
//! group. Sharing ours, it stopped us too, and with no job-control shell
//! above us (`exec agent`, a session host's pane) nothing resumed either.
//! When the terminal closed, the kernel didn't resume them, since the
//! group was orphaned from the start, so the program outlived it. Each
//! program gets a group of its own and the terminal, and is resumed when
//! it stops.

use std::fmt;
use std::io;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Command, ExitStatus};

use nix::errno::Errno;
use nix::sys::signal::{SigSet, SigmaskHow, Signal, killpg, pthread_sigmask};
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::{Pid, getpgrp, tcgetpgrp, tcsetpgrp};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    Exited(i32),
    /// The raw signal number, since `Signal` can't name every signal
    /// (Linux's real-time ones).
    Killed(i32),
}

impl Ended {
    pub fn success(self) -> bool {
        self == Self::Exited(0)
    }

    fn of(status: ExitStatus) -> io::Result<Self> {
        match (status.code(), status.signal()) {
            (Some(code), _) => Ok(Self::Exited(code)),
            (None, Some(signal)) => Ok(Self::Killed(signal)),
            (None, None) => Err(io::Error::other(format!("unknown exit status {status}"))),
        }
    }
}

impl fmt::Display for Ended {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exited(code) => write!(f, "code {code}"),
            Self::Killed(signal) => match Signal::try_from(*signal) {
                Ok(named) => write!(f, "{named}"),
                Err(_) => write!(f, "signal {signal}"),
            },
        }
    }
}

/// Runs `command` on this process's terminal until it exits. When the
/// terminal's foreground is ours, the program runs in a process group of
/// its own that holds the foreground until it exits, and is resumed each
/// time it stops. Otherwise it shares our group as any child would.
pub fn hand_over(command: &mut Command) -> io::Result<Ended> {
    let Some(foreground) = Foreground::ours() else {
        return Ended::of(command.status()?);
    };
    let group = Group::spawn(command)?;
    let lent = match foreground.lend(&group) {
        Ok(lent) => lent,
        Err(e) => return group.kill().and(Err(e.into())),
    };
    let ended = group.wait(&lent);
    let back = lent.take_back();
    match ended {
        Ok(ended) => back.map(|()| ended),
        Err(e) => {
            let killed = group.kill();
            back.and(killed).and(Err(e))
        }
    }
}

/// A child leading the process group named by its own pid, waited on
/// through that pid alone.
struct Group {
    id: Pid,
}

impl Group {
    fn spawn(command: &mut Command) -> io::Result<Self> {
        let mut child = command.process_group(0).spawn()?;
        match i32::try_from(child.id()) {
            Ok(id) => Ok(Self {
                id: Pid::from_raw(id),
            }),
            Err(e) => {
                let killed = child.kill();
                let waited = child.wait();
                killed.and(waited).and(Err(io::Error::other(e)))
            }
        }
    }

    /// Kills everything in the group, not only its leader, and reaps the
    /// leader. A group already gone or reaped is not an error.
    fn kill(self) -> io::Result<()> {
        match killpg(self.id, Signal::SIGKILL) {
            Ok(()) | Err(Errno::ESRCH) => {}
            Err(e) => return Err(e.into()),
        }
        loop {
            match waitpid(self.id, None) {
                Ok(_) | Err(Errno::ECHILD) => return Ok(()),
                Err(Errno::EINTR) => {}
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// Only while the foreground is lent, since resuming a program in
    /// the background would stop it again at once.
    fn wait(&self, _: &Lent) -> io::Result<Ended> {
        loop {
            match waitpid(self.id, Some(WaitPidFlag::WUNTRACED)) {
                Ok(WaitStatus::Exited(_, code)) => return Ok(Ended::Exited(code)),
                Ok(WaitStatus::Signaled(_, signal, _)) => {
                    return Ok(Ended::Killed(signal as i32));
                }
                Ok(WaitStatus::Stopped(..)) => self.resume()?,
                Ok(WaitStatus::Continued(_) | WaitStatus::StillAlive) | Err(Errno::EINTR) => {}
                #[cfg(any(target_os = "linux", target_os = "android"))]
                Ok(WaitStatus::PtraceEvent(..) | WaitStatus::PtraceSyscall(_)) => {}
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// Lends the foreground again first: a shell inside the program can
    /// have moved it. A group that has gone since it stopped is reaped by
    /// the next wait.
    fn resume(&self) -> io::Result<()> {
        set_foreground(self.id)?;
        match killpg(self.id, Signal::SIGCONT) {
            Ok(()) | Err(Errno::ESRCH) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

/// Proof that the terminal's foreground process group is ours.
struct Foreground {
    ours: Pid,
}

impl Foreground {
    fn ours() -> Option<Self> {
        let ours = getpgrp();
        (tcgetpgrp(io::stdin()).ok()? == ours).then_some(Self { ours })
    }

    fn lend(self, group: &Group) -> nix::Result<Lent> {
        set_foreground(group.id)?;
        Ok(Lent { ours: self.ours })
    }
}

/// The terminal's foreground, lent to another group until taken back.
#[must_use]
struct Lent {
    ours: Pid,
}

impl Lent {
    fn take_back(self) -> io::Result<()> {
        Ok(set_foreground(self.ours)?)
    }
}

/// Setting the foreground from the background raises `SIGTTOU`, which
/// would stop us unless blocked.
fn set_foreground(group: Pid) -> nix::Result<()> {
    let mut ttou = SigSet::empty();
    ttou.add(Signal::SIGTTOU);
    let mut previous = SigSet::empty();
    pthread_sigmask(SigmaskHow::SIG_BLOCK, Some(&ttou), Some(&mut previous))?;
    let set = tcsetpgrp(io::stdin(), group);
    pthread_sigmask(SigmaskHow::SIG_SETMASK, Some(&previous), None)?;
    set
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    use nix::sys::signal::kill;
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};

    const PATIENCE: Duration = Duration::from_secs(10);
    const PROBE: &str = "AGENT_HANDOFF_PROBE";
    const SIGNALLED: i32 = 100;
    const FOREGROUND_LOST: i32 = 99;

    /// Runs in a child test process as the session leader of a pty, the
    /// way `exec agent` runs, handing the terminal to the script in
    /// `PROBE`. It exits with the script's code, `SIGNALLED` plus its
    /// signal, or `FOREGROUND_LOST` if the terminal wasn't given back.
    #[test]
    fn probe() {
        let Some(script) = std::env::var_os(PROBE) else {
            return;
        };
        let ended = hand_over(Command::new("sh").arg("-c").arg(script)).unwrap();
        if tcgetpgrp(io::stdin()) != Ok(getpgrp()) {
            std::process::exit(FOREGROUND_LOST);
        }
        std::process::exit(match ended {
            Ended::Exited(code) => code,
            Ended::Killed(signal) => SIGNALLED + signal,
        });
    }

    /// The probe's exit code, or `None` if it didn't exit within
    /// `PATIENCE`, killed so it can't outlive the test. Its output is
    /// drained, since closing a terminal waits for that.
    fn probe_exit(script: &str) -> Option<u32> {
        let pty = native_pty_system()
            .openpty(PtySize::default())
            .expect("a pty");
        let mut command = CommandBuilder::new(std::env::current_exe().unwrap());
        command.args(["--exact", "handoff::tests::probe", "--nocapture"]);
        command.env(PROBE, script);
        let mut probe = pty.slave.spawn_command(command).expect("the probe");
        drop(pty.slave);
        let mut output = pty.master.try_clone_reader().expect("the pty's reader");
        std::thread::spawn(move || std::io::copy(&mut output, &mut std::io::sink()));
        let until = Instant::now() + PATIENCE;
        while Instant::now() < until {
            if let Some(status) = probe.try_wait().expect("waiting on the probe") {
                return Some(status.exit_code());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let leader = Pid::from_raw(i32::try_from(probe.process_id().unwrap()).unwrap());
        let _ = killpg(leader, Signal::SIGKILL);
        let _ = kill(leader, Signal::SIGKILL);
        let _ = probe.wait();
        None
    }

    #[test]
    fn a_program_that_stops_its_whole_group_is_resumed_without_stopping_us() {
        assert_eq!(probe_exit("kill -STOP 0; exit 3"), Some(3));
    }

    #[test]
    fn a_program_holds_the_terminal_while_it_runs() {
        assert_eq!(
            probe_exit(
                "for _ in 1 2 3 4 5; do \
                 [ $(ps -o tpgid= -p $$) -eq $(ps -o pgid= -p $$) ] && exit 3; sleep 0.1; \
                 done; exit 6"
            ),
            Some(3)
        );
    }

    #[test]
    fn a_program_killed_by_a_signal_reports_the_signal() {
        assert_eq!(
            probe_exit("kill -TERM $$"),
            Some(u32::try_from(SIGNALLED + Signal::SIGTERM as i32).unwrap())
        );
    }

    #[test]
    fn an_exit_status_becomes_how_the_program_ended() {
        assert_eq!(
            Ended::of(ExitStatus::from_raw(3 << 8)).unwrap(),
            Ended::Exited(3)
        );
        assert_eq!(
            Ended::of(ExitStatus::from_raw(Signal::SIGTERM as i32)).unwrap(),
            Ended::Killed(Signal::SIGTERM as i32)
        );
        assert_eq!(Ended::Killed(Signal::SIGTERM as i32).to_string(), "SIGTERM");
        assert_eq!(Ended::Killed(99).to_string(), "signal 99");
    }
}
