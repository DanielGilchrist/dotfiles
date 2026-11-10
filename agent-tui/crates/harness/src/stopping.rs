//! Stopping a background session for real. `claude stop` can fail or leave
//! the process running, and `--resume` or `attach` would then join the old
//! version, so the process is checked gone by its pid and start time.

use std::io;
use std::time::Duration;

use agent_core::activity::Pid;
use agent_core::ports::HarnessError;

use crate::background::{JobId, ProcStart, Survivor};

/// What stopping needs from the machine, so the logic runs without signalling
/// anything.
pub(crate) trait Processes: Send + Sync {
    /// When `pid` started, if it is in the process table.
    fn started(&self, pid: Pid) -> Option<ProcStart>;
    fn stop_job(&self, job: &JobId) -> io::Result<()>;
    fn terminate(&self, pid: Pid) -> io::Result<()>;
}

/// How long to wait for a process to go, as polls of `every`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Patience {
    pub(crate) polls: u32,
    pub(crate) every: Duration,
}

impl Patience {
    pub(crate) const STOP: Self = Self {
        polls: 30,
        every: Duration::from_millis(100),
    };
    pub(crate) const TERM: Self = Self {
        polls: 50,
        every: Duration::from_millis(100),
    };
}

/// Whether the process `survivor` names is no longer running. A pid that
/// another process has taken, by its start time, is gone.
fn gone(procs: &dyn Processes, survivor: &Survivor) -> bool {
    procs.started(survivor.pid).as_ref() != Some(&survivor.started)
}

fn gone_within(procs: &dyn Processes, survivor: &Survivor, patience: Patience) -> bool {
    for poll in 0..=patience.polls {
        if gone(procs, survivor) {
            return true;
        }
        if poll < patience.polls {
            std::thread::sleep(patience.every);
        }
    }
    false
}

/// Stops `survivor` and returns once it has exited. `claude stop` goes
/// first. If it fails or the process stays, the verified pid is sent SIGTERM
/// and `claude stop` runs again to clear the stale job.
pub(crate) fn stop(
    procs: &dyn Processes,
    survivor: &Survivor,
    stop: Patience,
    term: Patience,
) -> Result<(), HarnessError> {
    let job = survivor.job.as_str();
    let stopped = procs.stop_job(&survivor.job);
    if stopped.is_ok() && gone_within(procs, survivor, stop) {
        return Ok(());
    }
    if !gone(procs, survivor) {
        procs
            .terminate(survivor.pid)
            .map_err(|source| HarnessError {
                context: format!("signalling claude (pid {})", survivor.pid.get()),
                source,
            })?;
        gone_within(procs, survivor, term);
    }
    let _ = procs.stop_job(&survivor.job);
    if gone(procs, survivor) {
        return Ok(());
    }
    Err(HarnessError {
        context: format!("stopping background session {job}"),
        source: io::Error::other(match stopped {
            Ok(()) => format!("claude (pid {}) is still running", survivor.pid.get()),
            Err(e) => format!(
                "claude (pid {}) is still running, and `claude stop` failed: {e}",
                survivor.pid.get()
            ),
        }),
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    use agent_core::ports::SurvivorState;

    use super::*;
    use crate::background::JobField;

    struct Fake {
        alive: AtomicBool,
        dies_when: Dies,
        stop_job_fails: bool,
        started: ProcStart,
        calls: Mutex<Vec<&'static str>>,
        replaced_by_another: AtomicBool,
    }

    #[derive(Clone, Copy)]
    enum Dies {
        OnStop,
        OnTerm,
        Never,
    }

    impl Fake {
        fn new(dies_when: Dies, stop_job_fails: bool) -> (Self, Survivor) {
            let started = ProcStart::from("Mon Jan  1 00:00:00 2026".to_string());
            let fake = Self {
                alive: AtomicBool::new(true),
                dies_when,
                stop_job_fails,
                started: started.clone(),
                calls: Mutex::default(),
                replaced_by_another: AtomicBool::new(false),
            };
            (fake, survivor(started))
        }

        fn calls(&self) -> Vec<&'static str> {
            self.calls.lock().expect("lock").clone()
        }
    }

    fn survivor(started: ProcStart) -> Survivor {
        #[derive(serde::Deserialize)]
        struct Job {
            #[serde(rename = "jobId")]
            job: JobField,
        }
        let JobField::Valid(job) = serde_json::from_str::<Job>(r#"{"jobId":"470bb73a"}"#)
            .expect("parses")
            .job
        else {
            panic!("a valid job id")
        };
        Survivor {
            job,
            pid: Pid::new(4242).expect("pid"),
            started,
            state: SurvivorState::Idle,
        }
    }

    impl Processes for Fake {
        fn started(&self, _: Pid) -> Option<ProcStart> {
            if self.replaced_by_another.load(Ordering::SeqCst) {
                return Some(ProcStart::from("Tue Feb  2 00:00:00 2027".to_string()));
            }
            self.alive
                .load(Ordering::SeqCst)
                .then(|| self.started.clone())
        }

        fn stop_job(&self, _: &JobId) -> io::Result<()> {
            self.calls.lock().expect("lock").push("claude stop");
            if matches!(self.dies_when, Dies::OnStop) {
                self.alive.store(false, Ordering::SeqCst);
            }
            if self.stop_job_fails {
                return Err(io::Error::other("exit 1"));
            }
            Ok(())
        }

        fn terminate(&self, _: Pid) -> io::Result<()> {
            self.calls.lock().expect("lock").push("SIGTERM");
            if matches!(self.dies_when, Dies::OnTerm) {
                self.alive.store(false, Ordering::SeqCst);
            }
            Ok(())
        }
    }

    const NOW: Patience = Patience {
        polls: 2,
        every: Duration::ZERO,
    };

    #[test]
    fn a_process_that_exits_on_claude_stop_is_never_signalled() {
        let (fake, survivor) = Fake::new(Dies::OnStop, false);
        stop(&fake, &survivor, NOW, NOW).expect("stopped");
        assert_eq!(fake.calls(), ["claude stop"]);
    }

    #[test]
    fn a_process_that_outlives_claude_stop_gets_sigterm_then_a_second_stop_to_clear_the_job() {
        let (fake, survivor) = Fake::new(Dies::OnTerm, false);
        stop(&fake, &survivor, NOW, NOW).expect("stopped");
        assert_eq!(fake.calls(), ["claude stop", "SIGTERM", "claude stop"]);
    }

    #[test]
    fn a_claude_stop_that_fails_is_followed_by_sigterm_and_a_second_stop() {
        let (fake, survivor) = Fake::new(Dies::OnTerm, true);
        stop(&fake, &survivor, NOW, NOW).expect("stopped");
        assert_eq!(fake.calls(), ["claude stop", "SIGTERM", "claude stop"]);
    }

    #[test]
    fn a_claude_stop_that_fails_but_the_process_is_gone_needs_no_signal() {
        let (fake, survivor) = Fake::new(Dies::OnStop, true);
        stop(&fake, &survivor, NOW, NOW).expect("the process is gone");
        assert_eq!(fake.calls(), ["claude stop", "claude stop"]);
    }

    #[test]
    fn a_process_that_survives_everything_is_an_error() {
        let (fake, survivor) = Fake::new(Dies::Never, false);
        let error = stop(&fake, &survivor, NOW, NOW).expect_err("still running");
        assert!(error.to_string().contains("still running"), "{error}");
        assert_eq!(fake.calls(), ["claude stop", "SIGTERM", "claude stop"]);
    }

    #[test]
    fn a_pid_another_process_has_taken_is_gone_and_is_never_signalled() {
        let (fake, survivor) = Fake::new(Dies::Never, true);
        fake.replaced_by_another.store(true, Ordering::SeqCst);
        stop(&fake, &survivor, NOW, NOW).expect("the process we meant is gone");
        assert!(!fake.calls().contains(&"SIGTERM"), "{:?}", fake.calls());
    }
}
