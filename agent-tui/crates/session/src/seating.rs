use agent_core::activity::Pid;
use agent_core::size::TermSize;
use agent_host::{ClientInfo, ClientRef, Purpose, Role, Standing, Take};

/// Where a client sits. What only some places have is in them: an owner and
/// a client that waits to own have the size they would give the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Place {
    Owner {
        size: TermSize,
    },
    /// Takes ownership when the owner leaves.
    Waiting {
        size: TermSize,
    },
    /// Never owns until it claims, which also gives its size.
    Watching,
    Controlling,
}

impl Place {
    fn standing(self) -> Standing {
        match self {
            Self::Owner { .. } => Standing::Owner,
            Self::Waiting { .. } | Self::Watching => Standing::Watcher,
            Self::Controlling => Standing::Control,
        }
    }
}

#[derive(Debug)]
struct Seat<A> {
    client: ClientRef,
    pid: Pid,
    purpose: Option<Purpose>,
    place: Place,
    attached: A,
}

impl<A> Seat<A> {
    fn info(&self) -> ClientInfo {
        ClientInfo {
            client: self.client,
            pid: self.pid,
            purpose: self.purpose,
            standing: self.place.standing(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Demotion {
    pub client: ClientRef,
    pub by: ClientRef,
}

/// What a change of seating asks the host to do. The owner changes in one
/// of two ways, so a change never says both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Unchanged,
    /// A newcomer or a claimant became the owner, displacing `demoted`.
    Takeover {
        demoted: Option<Demotion>,
        size: TermSize,
    },
    /// The owner left and a waiting client took its place with nobody asking.
    Succession {
        promoted: ClientRef,
        size: TermSize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Arrival {
    pub standing: Standing,
    pub change: Change,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ClaimRefused {
    #[error("a preview never displaces a window")]
    PreviewAgainstWindow,
    #[error("only a watcher can claim")]
    NotWatching,
}

/// Who owns a session and who watches (docs/host.md, D1), with what the host
/// keeps for each client (`A`), so a client has one entry and not one in two
/// maps. At most one client owns, and only the owner sizes the session. Every
/// change of owner is a [`Change`] that says who to tell and what size to
/// apply, so the host can't change ownership without hearing about it.
#[derive(Debug)]
pub struct Seating<A> {
    seats: Vec<Seat<A>>,
}

impl<A> Default for Seating<A> {
    fn default() -> Self {
        Self { seats: Vec::new() }
    }
}

impl<A> Seating<A> {
    fn owner(&self) -> Option<&Seat<A>> {
        self.seats
            .iter()
            .find(|seat| matches!(seat.place, Place::Owner { .. }))
    }

    fn seat_mut(&mut self, client: ClientRef) -> Option<&mut Seat<A>> {
        self.seats.iter_mut().find(|seat| seat.client == client)
    }

    /// What the host keeps for the client that owns the session, if one does.
    pub fn owner_attached(&self) -> Option<&A> {
        self.owner().map(|seat| &seat.attached)
    }

    pub fn standing(&self, client: ClientRef) -> Option<Standing> {
        self.seats
            .iter()
            .find(|seat| seat.client == client)
            .map(|seat| seat.place.standing())
    }

    pub fn info(&self, client: ClientRef) -> Option<ClientInfo> {
        self.seats
            .iter()
            .find(|seat| seat.client == client)
            .map(Seat::info)
    }

    pub fn infos(&self) -> Vec<ClientInfo> {
        self.seats.iter().map(Seat::info).collect()
    }

    pub fn attached(&self, client: ClientRef) -> Option<&A> {
        self.seats
            .iter()
            .find(|seat| seat.client == client)
            .map(|seat| &seat.attached)
    }

    /// Every client with its standing, in the order they joined.
    pub fn attachments(&self) -> impl Iterator<Item = (ClientRef, Standing, &A)> {
        self.seats
            .iter()
            .map(|seat| (seat.client, seat.place.standing(), &seat.attached))
    }

    pub fn len(&self) -> usize {
        self.seats.len()
    }

    fn displace_owner(&mut self, by: ClientRef) -> Option<Demotion> {
        let owner = self
            .seats
            .iter_mut()
            .find(|seat| matches!(seat.place, Place::Owner { .. }))?;
        let Place::Owner { size } = owner.place else {
            return None;
        };
        owner.place = match owner.purpose {
            Some(Purpose::Preview) => Place::Waiting { size },
            Some(Purpose::Window | Purpose::Watch) | None => Place::Watching,
        };
        Some(Demotion {
            client: owner.client,
            by,
        })
    }

    pub fn join(&mut self, client: ClientRef, pid: Pid, role: Role, attached: A) -> Arrival {
        let owner = self.owner().and_then(|seat| seat.purpose);
        let place = match role {
            Role::Window {
                take: Take::Over,
                size,
                ..
            } => Place::Owner { size },
            Role::Window {
                take: Take::Politely,
                size,
                ..
            } => match owner {
                None | Some(Purpose::Preview) => Place::Owner { size },
                Some(Purpose::Window | Purpose::Watch) => Place::Waiting { size },
            },
            Role::Preview { size, .. } => {
                if self.owner().is_none() {
                    Place::Owner { size }
                } else {
                    Place::Waiting { size }
                }
            }
            Role::Watch { .. } => Place::Watching,
            Role::Control => Place::Controlling,
        };
        let change = match place {
            Place::Owner { size } => Change::Takeover {
                demoted: self.displace_owner(client),
                size,
            },
            Place::Waiting { .. } | Place::Watching | Place::Controlling => Change::Unchanged,
        };
        self.seats.push(Seat {
            client,
            pid,
            purpose: role.purpose(),
            place,
            attached,
        });
        Arrival {
            standing: place.standing(),
            change,
        }
    }

    /// A watcher takes ownership and sizes the session to `size`.
    pub fn claim(&mut self, client: ClientRef, size: TermSize) -> Result<Change, ClaimRefused> {
        let claimant = self
            .seats
            .iter()
            .find(|seat| seat.client == client)
            .ok_or(ClaimRefused::NotWatching)?;
        if !matches!(claimant.place, Place::Waiting { .. } | Place::Watching) {
            return Err(ClaimRefused::NotWatching);
        }
        if claimant.purpose == Some(Purpose::Preview)
            && self.owner().and_then(|seat| seat.purpose) == Some(Purpose::Window)
        {
            return Err(ClaimRefused::PreviewAgainstWindow);
        }
        let demoted = self.displace_owner(client);
        if let Some(seat) = self.seat_mut(client) {
            seat.place = Place::Owner { size };
        }
        Ok(Change::Takeover { demoted, size })
    }

    /// The owner states its terminal's size. The session takes it.
    pub fn resize(&mut self, client: ClientRef, size: TermSize) -> Option<TermSize> {
        let seat = self.seat_mut(client)?;
        match &mut seat.place {
            Place::Owner { size: owned } => {
                *owned = size;
                Some(size)
            }
            Place::Waiting { .. } | Place::Watching | Place::Controlling => None,
        }
    }

    /// Removes a client. When it owned, the next watcher that waits for
    /// ownership takes it: a window before a preview, oldest first.
    pub fn leave(&mut self, client: ClientRef) -> Option<(A, Change)> {
        let at = self.seats.iter().position(|seat| seat.client == client)?;
        let left = self.seats.remove(at);
        if !matches!(left.place, Place::Owner { .. }) {
            return Some((left.attached, Change::Unchanged));
        }
        let waiting = |purpose| {
            self.seats.iter().position(|seat| {
                matches!(seat.place, Place::Waiting { .. }) && seat.purpose == Some(purpose)
            })
        };
        let next = waiting(Purpose::Window).or_else(|| waiting(Purpose::Preview));
        let change = match next.map(|next| &mut self.seats[next]) {
            Some(seat) => match seat.place {
                Place::Waiting { size } => {
                    seat.place = Place::Owner { size };
                    Change::Succession {
                        promoted: seat.client,
                        size,
                    }
                }
                Place::Owner { .. } | Place::Watching | Place::Controlling => Change::Unchanged,
            },
            None => Change::Unchanged,
        };
        Some((left.attached, change))
    }
}

#[cfg(test)]
mod tests {
    use agent_host::Take;

    use super::*;
    use crate::testing::size;

    fn client(n: u32) -> ClientRef {
        ClientRef::new(n)
    }

    fn pid() -> Pid {
        Pid::new(100).expect("a pid")
    }

    fn window(take: Take, rows: u16) -> Role {
        Role::Window {
            take,
            size: size(rows, 80),
            colours: None,
        }
    }

    fn preview(rows: u16) -> Role {
        Role::Preview {
            size: size(rows, 80),
            colours: None,
        }
    }

    fn watch(purpose: Purpose) -> Role {
        Role::Watch { purpose }
    }

    fn join(seating: &mut Seating<()>, n: u32, role: Role) -> Arrival {
        seating.join(client(n), pid(), role, ())
    }

    fn leave(seating: &mut Seating<()>, n: u32) -> Change {
        seating
            .leave(client(n))
            .map_or(Change::Unchanged, |((), change)| change)
    }

    fn standings(seating: &Seating<()>) -> Vec<(u32, Standing)> {
        seating
            .infos()
            .iter()
            .map(|info| (info.client.get(), info.standing))
            .collect()
    }

    fn demotion(client_: u32, by: u32) -> Demotion {
        Demotion {
            client: client(client_),
            by: client(by),
        }
    }

    #[test]
    fn the_first_window_owns_and_sizes_the_session() {
        let mut seating = Seating::default();
        let arrival = join(&mut seating, 1, window(Take::Politely, 30));
        assert_eq!(arrival.standing, Standing::Owner);
        assert_eq!(
            arrival.change,
            Change::Takeover {
                demoted: None,
                size: size(30, 80)
            }
        );
    }

    #[test]
    fn a_window_takes_ownership_from_a_preview_without_asking_either_way() {
        for take in [Take::Over, Take::Politely] {
            let mut seating = Seating::default();
            join(&mut seating, 1, preview(20));
            let arrival = join(&mut seating, 2, window(take, 30));
            assert_eq!(arrival.standing, Standing::Owner);
            assert_eq!(
                arrival.change,
                Change::Takeover {
                    demoted: Some(demotion(1, 2)),
                    size: size(30, 80)
                }
            );
            assert_eq!(
                standings(&seating),
                [(1, Standing::Watcher), (2, Standing::Owner)]
            );
        }
    }

    #[test]
    fn a_newcomer_window_taking_over_demotes_the_old_owner_to_a_watcher() {
        let mut seating = Seating::default();
        join(&mut seating, 1, window(Take::Politely, 20));
        let arrival = join(&mut seating, 2, window(Take::Over, 30));
        assert_eq!(arrival.standing, Standing::Owner);
        assert_eq!(
            arrival.change,
            Change::Takeover {
                demoted: Some(demotion(1, 2)),
                size: size(30, 80)
            }
        );
    }

    #[test]
    fn a_polite_window_watches_while_another_window_owns() {
        let mut seating = Seating::default();
        join(&mut seating, 1, window(Take::Over, 20));
        let arrival = join(&mut seating, 2, window(Take::Politely, 30));
        assert_eq!(arrival.standing, Standing::Watcher);
        assert_eq!(arrival.change, Change::Unchanged);
    }

    #[test]
    fn a_preview_never_displaces_a_window_and_owns_only_an_empty_session() {
        let mut seating = Seating::default();
        assert_eq!(join(&mut seating, 1, preview(20)).standing, Standing::Owner);
        let mut seating = Seating::default();
        join(&mut seating, 1, window(Take::Over, 20));
        let arrival = join(&mut seating, 2, preview(30));
        assert_eq!(arrival.standing, Standing::Watcher);
        assert_eq!(arrival.change, Change::Unchanged);
    }

    #[test]
    fn a_preview_watching_beside_a_preview_owner_does_not_displace_it() {
        let mut seating = Seating::default();
        join(&mut seating, 1, preview(20));
        assert_eq!(
            join(&mut seating, 2, preview(30)).standing,
            Standing::Watcher
        );
    }

    #[test]
    fn a_watch_and_a_control_client_never_own_and_never_size() {
        let mut seating = Seating::default();
        for role in [
            watch(Purpose::Window),
            watch(Purpose::Preview),
            Role::Control,
        ] {
            let arrival = join(&mut seating, 7, role);
            assert_ne!(arrival.standing, Standing::Owner);
            assert_eq!(arrival.change, Change::Unchanged);
            leave(&mut seating, 7);
        }
    }

    #[test]
    fn a_control_client_is_behind_no_purpose() {
        let mut seating = Seating::default();
        join(&mut seating, 1, Role::Control);
        join(&mut seating, 2, window(Take::Over, 20));
        let purposes: Vec<_> = seating.infos().iter().map(|info| info.purpose).collect();
        assert_eq!(purposes, [None, Some(Purpose::Window)]);
    }

    #[test]
    fn a_demoted_window_claims_back_and_demotes_the_one_that_took_over() {
        let mut seating = Seating::default();
        join(&mut seating, 1, window(Take::Over, 20));
        join(&mut seating, 2, window(Take::Over, 30));
        let change = seating.claim(client(1), size(25, 90)).expect("claims");
        assert_eq!(
            change,
            Change::Takeover {
                demoted: Some(demotion(2, 1)),
                size: size(25, 90)
            }
        );
        assert_eq!(
            standings(&seating),
            [(1, Standing::Owner), (2, Standing::Watcher)]
        );
    }

    #[test]
    fn a_preview_cannot_claim_from_a_window_but_can_from_a_preview() {
        let mut seating = Seating::default();
        join(&mut seating, 1, window(Take::Over, 20));
        join(&mut seating, 2, preview(30));
        assert_eq!(
            seating.claim(client(2), size(30, 80)),
            Err(ClaimRefused::PreviewAgainstWindow)
        );
        let mut seating = Seating::default();
        join(&mut seating, 1, preview(20));
        join(&mut seating, 2, preview(30));
        assert!(seating.claim(client(2), size(30, 80)).is_ok());
    }

    #[test]
    fn an_owner_or_an_unknown_client_cannot_claim() {
        let mut seating = Seating::default();
        join(&mut seating, 1, window(Take::Over, 20));
        assert_eq!(
            seating.claim(client(1), size(20, 80)),
            Err(ClaimRefused::NotWatching)
        );
        assert_eq!(
            seating.claim(client(9), size(20, 80)),
            Err(ClaimRefused::NotWatching)
        );
    }

    #[test]
    fn a_watcher_that_never_had_a_size_gets_the_one_it_claims_with() {
        let mut seating = Seating::default();
        join(&mut seating, 1, window(Take::Over, 20));
        join(&mut seating, 2, watch(Purpose::Window));
        let change = seating.claim(client(2), size(40, 100)).expect("claims");
        assert!(matches!(change, Change::Takeover { size: s, .. } if s == size(40, 100)));
        assert_eq!(
            seating.resize(client(2), size(41, 100)),
            Some(size(41, 100))
        );
    }

    #[test]
    fn only_the_owner_resizes() {
        let mut seating = Seating::default();
        join(&mut seating, 1, window(Take::Over, 20));
        join(&mut seating, 2, window(Take::Politely, 30));
        assert_eq!(seating.resize(client(2), size(50, 50)), None);
        assert_eq!(seating.resize(client(1), size(21, 81)), Some(size(21, 81)));
    }

    #[test]
    fn when_the_owner_leaves_a_waiting_preview_takes_over_with_its_own_size() {
        let mut seating = Seating::default();
        join(&mut seating, 1, window(Take::Over, 20));
        join(&mut seating, 2, preview(33));
        assert_eq!(
            leave(&mut seating, 1),
            Change::Succession {
                promoted: client(2),
                size: size(33, 80)
            }
        );
        assert_eq!(standings(&seating), [(2, Standing::Owner)]);
    }

    #[test]
    fn a_waiting_window_is_promoted_before_a_waiting_preview() {
        let mut seating = Seating::default();
        join(&mut seating, 1, window(Take::Over, 20));
        join(&mut seating, 2, preview(33));
        join(&mut seating, 3, window(Take::Politely, 44));
        assert!(matches!(
            leave(&mut seating, 1),
            Change::Succession { promoted, .. } if promoted == client(3)
        ));
    }

    #[test]
    fn a_window_that_was_taken_over_from_waits_for_a_claim_and_is_not_promoted() {
        let mut seating = Seating::default();
        join(&mut seating, 1, window(Take::Over, 20));
        join(&mut seating, 2, window(Take::Over, 30));
        assert_eq!(leave(&mut seating, 2), Change::Unchanged);
        assert_eq!(standings(&seating), [(1, Standing::Watcher)]);
    }

    #[test]
    fn a_preview_displaced_by_a_window_is_promoted_again_when_the_window_leaves() {
        let mut seating = Seating::default();
        join(&mut seating, 1, preview(20));
        join(&mut seating, 2, window(Take::Over, 30));
        assert_eq!(
            leave(&mut seating, 2),
            Change::Succession {
                promoted: client(1),
                size: size(20, 80)
            }
        );
    }

    #[test]
    fn watchers_and_control_clients_are_never_promoted() {
        let mut seating = Seating::default();
        join(&mut seating, 1, window(Take::Over, 20));
        join(&mut seating, 2, watch(Purpose::Window));
        join(&mut seating, 3, Role::Control);
        assert_eq!(leave(&mut seating, 1), Change::Unchanged);
        assert!(
            seating
                .infos()
                .iter()
                .all(|info| info.standing != Standing::Owner)
        );
    }

    #[test]
    fn a_watcher_leaving_changes_nothing_and_an_unknown_client_is_ignored() {
        let mut seating = Seating::default();
        join(&mut seating, 1, window(Take::Over, 20));
        join(&mut seating, 2, preview(30));
        assert_eq!(leave(&mut seating, 2), Change::Unchanged);
        assert!(seating.leave(client(9)).is_none());
        assert_eq!(seating.len(), 1);
    }

    #[test]
    fn what_the_host_keeps_for_a_client_leaves_with_it() {
        let mut seating: Seating<&str> = Seating::default();
        seating.join(client(1), pid(), window(Take::Over, 20), "first");
        seating.join(client(2), pid(), watch(Purpose::Window), "second");
        assert_eq!(seating.attached(client(2)), Some(&"second"));
        let (left, _) = seating.leave(client(2)).expect("leaves");
        assert_eq!(left, "second");
        assert_eq!(seating.attached(client(2)), None);
    }

    #[test]
    fn at_most_one_client_owns_through_any_sequence() {
        let mut seating = Seating::default();
        let roles = [
            window(Take::Over, 20),
            preview(21),
            window(Take::Politely, 22),
            watch(Purpose::Window),
            window(Take::Over, 23),
            preview(24),
        ];
        let owners = |seating: &Seating<()>| {
            seating
                .infos()
                .iter()
                .filter(|info| info.standing == Standing::Owner)
                .count()
        };
        for (n, role) in roles.into_iter().enumerate() {
            join(&mut seating, u32::try_from(n).expect("small"), role);
            assert!(owners(&seating) <= 1, "{:?}", standings(&seating));
        }
        for n in [0, 4, 1, 2] {
            leave(&mut seating, n);
            assert!(owners(&seating) <= 1, "{:?}", standings(&seating));
        }
    }
}
