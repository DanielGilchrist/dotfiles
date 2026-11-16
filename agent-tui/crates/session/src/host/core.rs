use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use agent_core::size::TermSize;
use agent_host::{
    Admitted, ClientList, ClientRef, Colours, Command, ControlCall, ExitStatus, Forbidden, Frame,
    Greeting, HostFrame, MAX_CLIENTS, OwnerCall, PROTOCOL_VERSION, Role, Scrollback,
    Shared as SharedCall, Style, WatcherCall, Welcome,
};
use nix::sys::signal::Signal;

use super::Shared;
use super::child::{Program, Typed};
use super::message::{Delivery, Held, Joining, Message, Outbox, Outgoing, Pending, output_frames};
use crate::egress::{Egress, Filtered, Policy, Recipient, Tally};
use crate::screen::Screen;
use crate::seating::{Change, Demotion, Seating};

/// The longest a refusal tally waits before it is logged, so a program that
/// prints what is dropped in a loop costs the log a line a second.
const REFUSALS_LOGGED_EVERY: Duration = Duration::from_secs(1);

struct Attached {
    outbox: Outbox,
    connection: UnixStream,
    /// What the client said its terminal's colours are, which is what the
    /// program is told when this client owns the session.
    colours: Option<Colours>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Output {
    Open,
    /// The pty reported end of file: nothing more will be printed.
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Termination {
    NotAsked,
    Hangup { at: Instant },
    Killed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Life {
    Running(Termination),
    /// The program exited. What it printed last may still be on its way.
    Reaped {
        status: ExitStatus,
        output_until: Instant,
    },
    /// Every client was told, the socket is gone and the lock is given up.
    /// The host stays until `until` for the clients still connected.
    Ended {
        status: ExitStatus,
        until: Instant,
    },
}

/// Owns the screen, the seating and every client's queue. Each message is
/// handled to the end before the next, so a client's replay and the output
/// after it are one ordered stream.
pub(super) struct Core {
    inbox: Receiver<Message>,
    screen: Box<dyn Screen>,
    egress: Egress,
    /// Dropped sequences not logged yet, and when the last line was.
    refused: Tally,
    refusals_logged: Option<Instant>,
    seating: Seating<Attached>,
    program: Program,
    size: TermSize,
    /// Clients that arrived during a synchronized update, in order.
    waiting: Vec<Joining>,
    waiting_since: Option<Instant>,
    life: Life,
    output: Output,
    shared: Arc<Shared>,
}

impl Core {
    pub(super) fn new(
        inbox: Receiver<Message>,
        screen: Box<dyn Screen>,
        policy: Policy,
        program: Program,
        size: TermSize,
        shared: Arc<Shared>,
    ) -> Self {
        Self {
            inbox,
            screen,
            egress: Egress::new(policy),
            refused: Tally::default(),
            refusals_logged: None,
            seating: Seating::default(),
            program,
            size,
            waiting: Vec::new(),
            waiting_since: None,
            life: Life::Running(Termination::NotAsked),
            output: Output::Open,
            shared,
        }
    }

    /// Serves until the program has exited, every client was told and the
    /// drain time has passed.
    pub(super) fn run(mut self) -> ExitStatus {
        loop {
            let message = match self.wait() {
                Some(wait) => match self.inbox.recv_timeout(wait) {
                    Ok(message) => Some(message),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => return self.last_status(),
                },
                None => match self.inbox.recv() {
                    Ok(message) => Some(message),
                    Err(_) => return self.last_status(),
                },
            };
            let now = Instant::now();
            if let Some(message) = message {
                self.handle(message, now);
            }
            if let Some(status) = self.tick(now) {
                return status;
            }
        }
    }

    fn last_status(&self) -> ExitStatus {
        match self.life {
            Life::Reaped { status, .. } | Life::Ended { status, .. } => status,
            Life::Running(_) => ExitStatus::Unknown,
        }
    }

    fn deadline(&self) -> Option<Instant> {
        let timing = self.shared.timing;
        let hold = self.waiting_since.map(|since| since + timing.update_hold);
        let life = match self.life {
            Life::Running(Termination::Hangup { at }) => Some(at + timing.kill_grace),
            Life::Running(_) => None,
            Life::Reaped { output_until, .. } => Some(output_until),
            Life::Ended { until, .. } => Some(until),
        };
        [hold, life].into_iter().flatten().min()
    }

    /// How long to wait for a message. This is a timeout for the wait and not
    /// a decision, so it reads the clock for itself.
    fn wait(&self) -> Option<Duration> {
        self.deadline()
            .map(|at| at.saturating_duration_since(Instant::now()))
    }

    /// Acts on whatever deadline has passed. `Some` ends the host.
    fn tick(&mut self, now: Instant) -> Option<ExitStatus> {
        let timing = self.shared.timing;
        if self
            .waiting_since
            .is_some_and(|since| now >= since + timing.update_hold)
        {
            self.admit_waiting(now);
        }
        match self.life {
            Life::Running(Termination::Hangup { at }) if now >= at + timing.kill_grace => {
                self.signal(Signal::SIGKILL);
                self.life = Life::Running(Termination::Killed);
                None
            }
            Life::Reaped {
                status,
                output_until,
            } if now >= output_until => {
                self.finish(status, now);
                None
            }
            Life::Ended { status, until } if now >= until => Some(status),
            Life::Running(_) | Life::Reaped { .. } | Life::Ended { .. } => None,
        }
    }

    fn handle(&mut self, message: Message, now: Instant) {
        match message {
            Message::Output(bytes) => self.output(&bytes, now),
            Message::OutputClosed => {
                self.output = Output::Closed;
                if let Life::Reaped { status, .. } = self.life {
                    self.finish(status, now);
                }
            }
            Message::Exited(status) => {
                if matches!(self.life, Life::Running(_)) {
                    self.life = Life::Reaped {
                        status,
                        output_until: now + self.shared.timing.output_after_exit,
                    };
                    if self.output == Output::Closed {
                        self.finish(status, now);
                    }
                }
            }
            Message::Join(joining) => self.arrive(joining, now),
            Message::Command {
                client,
                command,
                held,
            } => self.command(client, command, held, now),
            Message::Gone(client) => {
                self.waiting.retain(|joining| joining.client != client);
                self.remove(client);
            }
        }
    }

    /// The program printed `bytes`: what is allowed goes to the screen and on
    /// to each client in the form its standing gets, so the screen is exactly
    /// what the owner is shown, and the queries in them are answered to the
    /// program from the screen.
    fn output(&mut self, bytes: &[u8], now: Instant) {
        let Filtered {
            owner,
            watchers,
            asked,
            refused,
        } = self.egress.feed(bytes);
        let colours = self
            .seating
            .owner_attached()
            .and_then(|owner| owner.colours);
        let replies = self.screen.feed(&owner, &asked, colours);
        if !replies.is_empty() {
            self.reply_to_program(replies);
        }
        self.note_refusals(refused, now);
        let owner = output_frames(&owner);
        let watchers = output_frames(&watchers);
        let failed: Vec<(ClientRef, Delivery)> = self
            .seating
            .attachments()
            .filter_map(|(client, standing, attached)| {
                let frames = match Recipient::of(standing)? {
                    Recipient::Owner => &owner,
                    Recipient::Watcher => &watchers,
                };
                match attached.outbox.deliver_all(frames) {
                    Delivery::Queued => None,
                    other => Some((client, other)),
                }
            })
            .collect();
        for (client, delivery) in failed {
            self.drop_failed(client, delivery);
        }
        if !self.waiting.is_empty() && !self.screen.synchronized() {
            self.admit_waiting(now);
        }
    }

    /// Replies are generated by the host, so a program that asks without
    /// reading what comes back can't make them pile up: they are counted
    /// against a budget of their own, and those that don't fit are dropped.
    fn reply_to_program(&mut self, replies: Vec<u8>) {
        let Some(held) = self.shared.replies.try_acquire(replies.len()) else {
            self.shared
                .log
                .warn("replies to the program's queries were dropped, it reads none of them");
            return;
        };
        self.program_input(replies, held);
    }

    fn note_refusals(&mut self, refused: Tally, now: Instant) {
        self.refused.merge(refused);
        let due = self
            .refusals_logged
            .is_none_or(|at| now.duration_since(at) >= REFUSALS_LOGGED_EVERY);
        if due {
            self.log_refusals(now);
        }
    }

    fn log_refusals(&mut self, now: Instant) {
        if self.refused.is_empty() {
            return;
        }
        let refused = std::mem::take(&mut self.refused);
        self.refusals_logged = Some(now);
        self.shared.log.warn(format_args!(
            "dropped {} sequences from the program: {refused}",
            refused.total()
        ));
    }

    /// A control client has no screen to keep from showing half a frame.
    fn arrive(&mut self, joining: Joining, now: Instant) {
        if joining.role != Role::Control && self.screen.synchronized() {
            self.waiting_since.get_or_insert(now);
            self.waiting.push(joining);
        } else {
            self.admit(joining, now);
        }
    }

    fn admit_waiting(&mut self, now: Instant) {
        self.waiting_since = None;
        for joining in std::mem::take(&mut self.waiting) {
            self.admit(joining, now);
        }
    }

    /// Seats a client and sends it `Welcome` and, unless it is a control
    /// client, the replay of the screen. Nothing is fed to the screen
    /// between building the replay and registering the client, so the live
    /// output it gets next continues exactly where the replay ends.
    fn admit(&mut self, joining: Joining, now: Instant) {
        let Joining {
            client,
            peer,
            role,
            outbox,
            connection,
            pending,
        } = joining;
        if self.seating.len() >= MAX_CLIENTS {
            self.shared.log.warn(format_args!(
                "too many clients, {} was closed",
                client.get()
            ));
            let _ = connection.shutdown(std::net::Shutdown::Both);
            return;
        }
        let attached = Attached {
            outbox,
            connection,
            colours: role.colours(),
        };
        let arrival = self.seating.join(client, peer.pid, role, attached);
        self.apply(arrival.change);
        let welcome = Welcome {
            version: PROTOCOL_VERSION,
            size: self.size,
            standing: arrival.standing,
            you: client,
        };
        let replay = Recipient::of(arrival.standing).map(|recipient| {
            let replay = self.egress.clean(&self.screen.replay(), recipient);
            self.note_refusals(replay.refused, now);
            output_frames(&replay.bytes)
        });
        let ended = match self.life {
            Life::Ended { status, .. } => Some(status),
            Life::Running(_) | Life::Reaped { .. } => None,
        };
        let Some(attached) = self.seating.attached(client) else {
            return;
        };
        let mut delivery = attached.outbox.send(&Greeting::Welcome(welcome));
        if let (Delivery::Queued, Some(frames)) = (delivery, &replay) {
            delivery = attached.outbox.deliver_replay(frames);
        }
        if let (Delivery::Queued, Some(status)) = (delivery, ended) {
            delivery = attached.outbox.send(&HostFrame::Exited(status));
        }
        if delivery != Delivery::Queued {
            self.drop_failed(client, delivery);
            return;
        }
        for Pending { command, held } in pending {
            self.command(client, command, held, now);
        }
    }

    fn command(&mut self, client: ClientRef, command: Command, held: Held, now: Instant) {
        let Some(standing) = self.seating.standing(client) else {
            self.queue_unseated(client, command, held);
            return;
        };
        match standing.admit(command) {
            Err(Forbidden { command }) => self.tell(client, &HostFrame::Forbidden { command }),
            Ok(Admitted::Owner(call)) => match call {
                OwnerCall::Input(payload) => self.program_input(payload.as_slice().to_vec(), held),
                OwnerCall::Resize(size) => {
                    if let Some(size) = self.seating.resize(client, size) {
                        self.apply_size(size);
                    }
                }
                OwnerCall::Detach(target) => self.detach(client, target),
                OwnerCall::Shared(call) => self.shared_call(client, call, held, now),
            },
            Ok(Admitted::Watcher(call)) => match call {
                WatcherCall::Claim(size) => self.claim(client, size),
                WatcherCall::Shared(call) => self.shared_call(client, call, held, now),
            },
            Ok(Admitted::Control(call)) => match call {
                ControlCall::Detach(target) => self.detach(client, target),
                ControlCall::Shared(call) => self.shared_call(client, call, held, now),
            },
        }
    }

    /// A command that arrives before its client was seated, because it is
    /// held for a synchronized update, waits and is handled after `Welcome`.
    /// One for a client that isn't here at all is ignored.
    fn queue_unseated(&mut self, client: ClientRef, command: Command, held: Held) {
        if let Some(joining) = self
            .waiting
            .iter_mut()
            .find(|joining| joining.client == client)
        {
            joining.pending.push(Pending { command, held });
        }
    }

    fn shared_call(&mut self, client: ClientRef, call: SharedCall, held: Held, now: Instant) {
        match call {
            SharedCall::Inject(payload) => self.program_input(payload.as_slice().to_vec(), held),
            SharedCall::Snapshot { scrollback, style } => {
                self.snapshot(client, scrollback, style, now);
            }
            SharedCall::Clients => match ClientList::new(self.seating.infos()) {
                Ok(list) => self.tell(client, &HostFrame::Clients(list)),
                Err(_) => self.tell(
                    client,
                    &HostFrame::Forbidden {
                        command: agent_host::CommandKind::Clients,
                    },
                ),
            },
            SharedCall::Kill => self.terminate(now),
            SharedCall::Bye => self.remove(client),
        }
    }

    fn program_input(&mut self, bytes: Vec<u8>, held: Held) {
        if self.program.type_in(bytes, held) == Typed::Lost {
            self.shared
                .log
                .warn("input for the program was dropped, its pty takes no more");
        }
    }

    /// The screen and its scrollback are copied on this thread, which holds
    /// up reading the program's output for as long as that takes, and at
    /// most [`super::SNAPSHOTS_AT_ONCE`] are in flight.
    fn snapshot(&mut self, client: ClientRef, scrollback: Scrollback, style: Style, now: Instant) {
        let Some(permit) = self.shared.snapshots.permit() else {
            self.tell(
                client,
                &HostFrame::Forbidden {
                    command: agent_host::CommandKind::Snapshot,
                },
            );
            return;
        };
        let dump = self
            .egress
            .clean(&self.screen.dump(scrollback, style), Recipient::Watcher);
        self.note_refusals(dump.refused, now);
        let bytes: Arc<[u8]> = dump.bytes.into();
        let Some(attached) = self.seating.attached(client) else {
            return;
        };
        let delivery = attached.outbox.deliver(Outgoing::Snapshot(bytes, permit));
        if delivery != Delivery::Queued {
            self.drop_failed(client, delivery);
        }
    }

    fn claim(&mut self, client: ClientRef, size: TermSize) {
        match self.seating.claim(client, size) {
            Ok(change) => {
                self.tell(client, &HostFrame::Promoted);
                self.apply(change);
            }
            Err(_) => self.tell(
                client,
                &HostFrame::Forbidden {
                    command: agent_host::CommandKind::Claim,
                },
            ),
        }
    }

    /// Closes another client, telling it who did.
    fn detach(&mut self, by: ClientRef, target: ClientRef) {
        if by == target {
            self.tell(
                by,
                &HostFrame::Forbidden {
                    command: agent_host::CommandKind::Detach,
                },
            );
            return;
        }
        let (Some(by), true) = (
            self.seating.info(by),
            self.seating.attached(target).is_some(),
        ) else {
            return;
        };
        self.tell(target, &HostFrame::Detached { by });
        self.remove(target);
    }

    fn terminate(&mut self, now: Instant) {
        if self.life != Life::Running(Termination::NotAsked) {
            return;
        }
        self.signal(Signal::SIGHUP);
        self.life = Life::Running(Termination::Hangup { at: now });
    }

    fn signal(&self, signal: Signal) {
        if let Err(e) = self.program.signal(signal) {
            self.shared
                .log
                .warn(format_args!("signalling the program with {signal}: {e}"));
        }
    }

    /// Tells every client, then has the accept thread take the socket away
    /// and give up the lock, so the session can be started again while the
    /// clients that are still connected finish.
    fn finish(&mut self, status: ExitStatus, now: Instant) {
        self.log_refusals(now);
        for (client, _, attached) in self.seating.attachments() {
            if attached.outbox.send(&HostFrame::Exited(status)) != Delivery::Queued {
                let _ = attached.connection.shutdown(std::net::Shutdown::Both);
                self.shared
                    .log
                    .warn(format_args!("client {} missed the exit", client.get()));
            }
        }
        self.life = Life::Ended {
            status,
            until: now + self.shared.timing.drain,
        };
        self.shared.ended.store(true, Ordering::Release);
    }

    /// Sends one frame to a client, closing it when it can't take it.
    fn tell(&mut self, client: ClientRef, frame: &impl Frame) {
        let Some(attached) = self.seating.attached(client) else {
            return;
        };
        let delivery = attached.outbox.send(frame);
        if delivery != Delivery::Queued {
            self.drop_failed(client, delivery);
        }
    }

    /// A client that fell behind is closed, and one whose writer already
    /// ended is just removed, which docs/host.md replaces with a resync for
    /// the first.
    fn drop_failed(&mut self, client: ClientRef, delivery: Delivery) {
        if delivery == Delivery::Full {
            self.shared.log.warn(format_args!(
                "client {} didn't keep up and was closed",
                client.get()
            ));
        }
        if let Some(attached) = self.seating.attached(client) {
            let _ = attached.connection.shutdown(std::net::Shutdown::Both);
        }
        self.remove(client);
    }

    fn remove(&mut self, client: ClientRef) {
        let Some((_attached, change)) = self.seating.leave(client) else {
            return;
        };
        self.apply(change);
    }

    fn apply(&mut self, change: Change) {
        match change {
            Change::Unchanged => {}
            Change::Takeover { demoted, size } => {
                if let Some(Demotion { client, by }) = demoted
                    && let Some(by) = self.seating.info(by)
                {
                    self.tell(client, &HostFrame::Demoted { by });
                }
                self.apply_size(size);
            }
            Change::Succession { promoted, size } => {
                self.tell(promoted, &HostFrame::Promoted);
                self.apply_size(size);
            }
        }
    }

    fn apply_size(&mut self, size: TermSize) {
        if size == self.size {
            return;
        }
        if let Err(e) = self.program.resize(size) {
            self.shared.log.warn(format_args!("resizing the pty: {e}"));
            return;
        }
        self.screen.resize(size);
        self.size = size;
    }
}
