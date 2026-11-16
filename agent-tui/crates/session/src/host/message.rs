use std::collections::VecDeque;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use agent_host::{ClientRef, Command, ExitStatus, Frame, HostFrame, Payload, Role};

use crate::peer::Peer;

/// Everything the core hears, in the order it is to be handled.
pub(super) enum Message {
    Output(Vec<u8>),
    OutputClosed,
    Exited(ExitStatus),
    Join(Joining),
    Command {
        client: ClientRef,
        command: Command,
        /// The input bytes of the command, which are counted against the
        /// program's input budget until they are written or dropped.
        held: Held,
    },
    Gone(ClientRef),
}

/// A client that sent a valid `Hello` and waits to be seated.
pub(super) struct Joining {
    pub client: ClientRef,
    pub peer: Peer,
    pub role: Role,
    pub outbox: Outbox,
    /// Closing it ends both of the client's threads.
    pub connection: UnixStream,
    /// Commands it sent while it waited, which are handled once it is seated.
    pub pending: Vec<Pending>,
}

pub(super) struct Pending {
    pub command: Command,
    pub held: Held,
}

/// A count of bytes that may be in flight, so a sender that gets ahead of
/// its receiver waits and nothing is dropped from the middle of a stream.
pub(super) struct Budget {
    cap: usize,
    state: Mutex<BudgetState>,
    freed: Condvar,
}

struct BudgetState {
    used: usize,
    closed: bool,
}

impl Budget {
    pub(super) fn new(cap: usize) -> Arc<Self> {
        Arc::new(Self {
            cap,
            state: Mutex::new(BudgetState {
                used: 0,
                closed: false,
            }),
            freed: Condvar::new(),
        })
    }

    /// Waits until `bytes` fit, or the budget is closed, for at most `within`.
    /// A payload larger than the cap waits for the budget to be empty and then
    /// goes alone. `None` is a wait that ran out: nothing is being consumed,
    /// and a thread that waited on it would otherwise outlive its client.
    pub(super) fn acquire(self: &Arc<Self>, bytes: usize, within: Duration) -> Option<Held> {
        let deadline = Instant::now() + within;
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        while !state.closed && state.used > 0 && state.used + bytes > self.cap {
            let left = deadline.checked_duration_since(Instant::now())?;
            state = self
                .freed
                .wait_timeout(state, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        if state.closed {
            return Some(Held::nothing());
        }
        state.used += bytes;
        Some(Held {
            budget: Some(Arc::clone(self)),
            bytes,
        })
    }

    /// Takes `bytes` if they fit now, and never waits. A budget that was
    /// closed takes everything, since nothing is counted any more.
    pub(super) fn try_acquire(self: &Arc<Self>, bytes: usize) -> Option<Held> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.closed {
            return Some(Held::nothing());
        }
        if state.used > 0 && state.used + bytes > self.cap {
            return None;
        }
        state.used += bytes;
        Some(Held {
            budget: Some(Arc::clone(self)),
            bytes,
        })
    }

    /// Nothing will be released again, so nobody waits.
    pub(super) fn close(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.closed = true;
        self.freed.notify_all();
    }
}

/// Bytes counted against a [`Budget`] until it is dropped.
pub(super) struct Held {
    budget: Option<Arc<Budget>>,
    bytes: usize,
}

impl Held {
    pub(super) fn nothing() -> Self {
        Self {
            budget: None,
            bytes: 0,
        }
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        let Some(budget) = &self.budget else { return };
        let mut state = budget.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.used = state.used.saturating_sub(self.bytes);
        budget.freed.notify_all();
    }
}

/// How many snapshots are being sent at once. Each is a copy of the screen
/// and its scrollback, up to the scrollback cap.
pub(super) struct Snapshots {
    pending: AtomicUsize,
    limit: usize,
}

impl Snapshots {
    pub(super) fn new(limit: usize) -> Self {
        Self {
            pending: AtomicUsize::new(0),
            limit,
        }
    }

    pub(super) fn permit(self: &Arc<Self>) -> Option<Permit> {
        let taken = self.pending.fetch_add(1, Ordering::AcqRel);
        let permit = Permit(Arc::clone(self));
        (taken < self.limit).then_some(permit)
    }
}

/// One snapshot in flight, given back when dropped.
pub(super) struct Permit(Arc<Snapshots>);

impl Drop for Permit {
    fn drop(&mut self) {
        self.0.pending.fetch_sub(1, Ordering::AcqRel);
    }
}

/// What a client's writer thread sends.
pub(super) enum Outgoing {
    Frame(Arc<[u8]>),
    /// A frame of the replay a client is sent when it joins. It is as large
    /// as the screen, which is capped by the size cap, so it isn't held
    /// against the queue's cap, which a large screen's replay would exceed.
    Replay(Arc<[u8]>),
    /// Bytes for the writer to cut into `Snapshot` frames and end.
    Snapshot(Arc<[u8]>, Permit),
}

impl Outgoing {
    /// What it holds in memory that is the queue's to count.
    fn queued_bytes(&self) -> usize {
        match self {
            Self::Frame(bytes) | Self::Snapshot(bytes, _) => bytes.len(),
            Self::Replay(_) => 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Delivery {
    Queued,
    /// The client doesn't read as fast as the program prints.
    Full,
    /// The client's writer has ended, so it is already gone.
    Gone,
}

/// The most bytes queued for one client before it is closed. Live output is
/// cut at the pty's read size.
const OUTBOX_BYTES: usize = 4 * 1024 * 1024;

struct Queue {
    state: Mutex<QueueState>,
    ready: Condvar,
}

struct QueueState {
    items: VecDeque<Outgoing>,
    bytes: usize,
    producer_gone: bool,
    writer_gone: bool,
}

pub(super) fn queue() -> (Outbox, Drain) {
    let shared = Arc::new(Queue {
        state: Mutex::new(QueueState {
            items: VecDeque::new(),
            bytes: 0,
            producer_gone: false,
            writer_gone: false,
        }),
        ready: Condvar::new(),
    });
    (Outbox(Arc::clone(&shared)), Drain(shared))
}

/// A queue to one client's writer thread, bounded in bytes. Delivery never
/// waits. Dropping it lets the writer send what is queued and close.
pub(super) struct Outbox(Arc<Queue>);

impl Outbox {
    pub(super) fn deliver(&self, outgoing: Outgoing) -> Delivery {
        let mut state = self.0.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.writer_gone {
            return Delivery::Gone;
        }
        let size = outgoing.queued_bytes();
        if !state.items.is_empty() && state.bytes + size > OUTBOX_BYTES {
            return Delivery::Full;
        }
        state.bytes += size;
        state.items.push_back(outgoing);
        self.0.ready.notify_one();
        Delivery::Queued
    }

    pub(super) fn send(&self, frame: &impl Frame) -> Delivery {
        self.deliver(Outgoing::Frame(frame.encode().into()))
    }

    /// Queues a client's replay, whatever its size.
    pub(super) fn deliver_replay(&self, frames: &[Arc<[u8]>]) -> Delivery {
        for frame in frames {
            let delivery = self.deliver(Outgoing::Replay(Arc::clone(frame)));
            if delivery != Delivery::Queued {
                return delivery;
            }
        }
        Delivery::Queued
    }

    pub(super) fn deliver_all(&self, frames: &[Arc<[u8]>]) -> Delivery {
        for frame in frames {
            let delivery = self.deliver(Outgoing::Frame(Arc::clone(frame)));
            if delivery != Delivery::Queued {
                return delivery;
            }
        }
        Delivery::Queued
    }
}

impl Drop for Outbox {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.producer_gone = true;
        self.0.ready.notify_all();
    }
}

/// The writer's end of a client's queue.
pub(super) struct Drain(Arc<Queue>);

impl Drain {
    /// The next thing to write, waiting for one. `None` once the queue was
    /// dropped and everything in it has been taken.
    pub(super) fn next(&self) -> Option<Outgoing> {
        let mut state = self.0.state.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if let Some(outgoing) = state.items.pop_front() {
                state.bytes = state.bytes.saturating_sub(outgoing.queued_bytes());
                return Some(outgoing);
            }
            if state.producer_gone {
                return None;
            }
            state = self
                .0
                .ready
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
}

impl Drop for Drain {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.writer_gone = true;
        state.items.clear();
    }
}

/// `bytes` as `Output` frames, one or several, encoded once for every
/// client that gets them.
pub(super) fn output_frames(bytes: &[u8]) -> Vec<Arc<[u8]>> {
    Payload::split(bytes)
        .map(|chunk| HostFrame::Output(chunk).encode().into())
        .collect()
}

#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::Duration;

    use agent_host::{Decoder, MAX_FRAME, Progress};

    use super::*;

    #[test]
    fn a_replay_larger_than_a_frame_is_several_frames_that_each_fit_and_carry_all_of_it() {
        let replay: Vec<u8> = (0..3 * MAX_FRAME as usize)
            .map(|n| u8::try_from(n % 251).unwrap_or(0))
            .collect();
        let frames = output_frames(&replay);
        assert!(frames.len() >= 3);
        let mut carried = Vec::new();
        for frame in &frames {
            assert!(frame.len() <= MAX_FRAME as usize + 4);
            let mut input = &frame[..];
            let mut decoder = Decoder::<HostFrame>::new();
            loop {
                match decoder.read_from(&mut input) {
                    Ok(Progress::Frame(HostFrame::Output(bytes), _)) => {
                        carried.extend_from_slice(bytes.as_slice());
                        break;
                    }
                    Ok(Progress::More(next)) => decoder = next,
                    other => panic!("an output frame, not {other:?}"),
                }
            }
        }
        assert_eq!(carried, replay);
    }

    fn frame(len: usize) -> Outgoing {
        Outgoing::Frame(vec![0u8; len].into())
    }

    #[test]
    fn an_outbox_is_bounded_in_bytes_not_in_frames() {
        let (outbox, drain) = queue();
        for _ in 0..4 {
            assert_eq!(outbox.deliver(frame(1024 * 1024)), Delivery::Queued);
        }
        assert_eq!(outbox.deliver(frame(1)), Delivery::Full);
        assert!(drain.next().is_some());
        assert_eq!(outbox.deliver(frame(1)), Delivery::Queued);
    }

    #[test]
    fn many_tiny_frames_fill_an_outbox_by_their_size() {
        let (outbox, _drain) = queue();
        let mut queued = 0usize;
        while outbox.deliver(frame(4096)) == Delivery::Queued {
            queued += 1;
        }
        assert_eq!(queued, OUTBOX_BYTES / 4096);
    }

    #[test]
    fn a_writer_that_ended_is_gone_and_not_slow() {
        let (outbox, drain) = queue();
        drop(drain);
        assert_eq!(outbox.deliver(frame(1)), Delivery::Gone);
    }

    #[test]
    fn a_dropped_outbox_lets_the_writer_finish_what_was_queued() {
        let (outbox, drain) = queue();
        outbox.deliver(frame(1));
        outbox.deliver(frame(2));
        drop(outbox);
        assert!(drain.next().is_some());
        assert!(drain.next().is_some());
        assert!(drain.next().is_none());
    }

    #[test]
    fn a_sender_waits_for_the_budget_and_nothing_is_dropped() {
        let budget = Budget::new(10);
        let first = budget.acquire(8, Duration::from_secs(5)).expect("fits");
        let waiting = {
            let budget = Arc::clone(&budget);
            thread::spawn(move || {
                budget
                    .acquire(8, Duration::from_secs(5))
                    .map(|held| held.bytes)
            })
        };
        thread::sleep(Duration::from_millis(100));
        assert!(!waiting.is_finished(), "the sender should wait");
        drop(first);
        assert_eq!(waiting.join().expect("joins"), Some(8));
    }

    #[test]
    fn a_wait_that_nothing_ends_gives_up_so_its_thread_can_go() {
        let budget = Budget::new(10);
        let _first = budget.acquire(8, Duration::from_secs(1));
        let started = Instant::now();
        assert!(budget.acquire(8, Duration::from_millis(150)).is_none());
        assert!(started.elapsed() >= Duration::from_millis(140));
    }

    #[test]
    fn a_replay_larger_than_the_queue_is_taken_and_leaves_room_for_output() {
        let (outbox, drain) = queue();
        let big: Vec<Arc<[u8]>> = (0..8).map(|_| Arc::from(vec![0u8; 1024 * 1024])).collect();
        assert_eq!(outbox.deliver_replay(&big), Delivery::Queued);
        assert_eq!(outbox.deliver(frame(16 * 1024)), Delivery::Queued);
        assert!(drain.next().is_some());
    }

    #[test]
    fn a_queued_snapshot_counts_against_the_queue() {
        let (outbox, _drain) = queue();
        let snapshots = Arc::new(Snapshots::new(8));
        let snapshot = |bytes: usize| {
            Outgoing::Snapshot(
                vec![0u8; bytes].into(),
                snapshots.permit().expect("a permit"),
            )
        };
        assert_eq!(outbox.deliver(snapshot(3 * 1024 * 1024)), Delivery::Queued);
        assert_eq!(outbox.deliver(snapshot(3 * 1024 * 1024)), Delivery::Full);
    }

    #[test]
    fn a_payload_larger_than_the_cap_goes_through_alone() {
        let budget = Budget::new(10);
        assert_eq!(
            budget
                .acquire(100, Duration::from_secs(1))
                .map(|held| held.bytes),
            Some(100)
        );
    }

    #[test]
    fn a_closed_budget_lets_every_sender_through() {
        let budget = Budget::new(1);
        let _held = budget.acquire(1, Duration::from_secs(5));
        let waiting = {
            let budget = Arc::clone(&budget);
            thread::spawn(move || {
                budget
                    .acquire(1, Duration::from_secs(5))
                    .map(|held| held.bytes)
            })
        };
        thread::sleep(Duration::from_millis(50));
        budget.close();
        assert_eq!(waiting.join().expect("joins"), Some(0));
    }

    #[test]
    fn snapshots_in_flight_are_limited() {
        let snapshots = Arc::new(Snapshots::new(2));
        let one = snapshots.permit();
        let two = snapshots.permit();
        assert!(one.is_some() && two.is_some());
        assert!(snapshots.permit().is_none());
        drop(one);
        assert!(snapshots.permit().is_some());
    }
}
