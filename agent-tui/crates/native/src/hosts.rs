//! The session hosts a migration runs side by side. Each session belongs to
//! the host that runs it, found by asking, so a session started before the
//! switch keeps working. New sessions go to the configured host.
//!
//! With no native state the router is a pass-through: zellij is asked what it
//! was asked before, with the same timeouts and errors.

use std::collections::HashMap;
use std::time::Duration;

use agent_core::config::HostKind;
use agent_core::model::Running;
use agent_core::names::{HostName, NameLimit, SessionName};
use agent_core::ports::{
    Attachment, ClientControl, ClientId, Extent, HostError, HostListing, Liveness, Observation,
    PaneCommand, Rendering, SessionHost, SessionInfo, SessionStatus, Window,
};
use agent_core::process::LaunchSpec;

/// How long a question about one session's owner may take.
const OWNER_WITHIN: Duration = Duration::from_secs(2);

const BOTH: [HostKind; 2] = [HostKind::Zellij, HostKind::Native];

/// What each host says about one name, where it was asked and says anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Says {
    zellij: Option<Liveness>,
    native: Option<Liveness>,
}

impl Says {
    fn of(self, host: HostKind) -> Option<Liveness> {
        match host {
            HostKind::Zellij => self.zellij,
            HostKind::Native => self.native,
        }
    }

    /// A live session decides it, zellij's first. Without one, the only host
    /// that has the name does, and two hosts that each have only a dead
    /// record leave it to the default.
    fn owner(self, default: HostKind) -> HostKind {
        match (self.zellij, self.native) {
            (Some(Liveness::Live), _) | (Some(Liveness::Exited), None) => HostKind::Zellij,
            (_, Some(Liveness::Live)) | (None, Some(Liveness::Exited)) => HostKind::Native,
            (Some(Liveness::Exited), Some(Liveness::Exited)) | (None, None) => default,
        }
    }

    fn doubled(self) -> bool {
        self.zellij == Some(Liveness::Live) && self.native == Some(Liveness::Live)
    }

    fn running_in(self) -> Vec<HostKind> {
        BOTH.into_iter()
            .filter(|host| self.of(*host) == Some(Liveness::Live))
            .collect()
    }
}

fn liveness_of_status(status: &SessionStatus) -> Liveness {
    match status {
        SessionStatus::Live { .. } => Liveness::Live,
        SessionStatus::Exited { .. } => Liveness::Exited,
    }
}

/// What each host lists, joined by name.
fn said_by(
    zellij: impl IntoIterator<Item = (SessionName, Liveness)>,
    native: impl IntoIterator<Item = (SessionName, Liveness)>,
) -> HashMap<SessionName, Says> {
    let mut all: HashMap<SessionName, Says> = HashMap::new();
    let blank = Says {
        zellij: None,
        native: None,
    };
    for (name, liveness) in zellij {
        all.entry(name).or_insert(blank).zellij = Some(liveness);
    }
    for (name, liveness) in native {
        all.entry(name).or_insert(blank).native = Some(liveness);
    }
    all
}

pub struct Hosts {
    zellij: Box<dyn SessionHost>,
    native: Box<dyn SessionHost>,
    kind: HostKind,
}

impl Hosts {
    pub fn new(zellij: Box<dyn SessionHost>, native: Box<dyn SessionHost>, kind: HostKind) -> Self {
        Self {
            zellij,
            native,
            kind,
        }
    }

    fn host(&self, which: HostKind) -> &dyn SessionHost {
        match which {
            HostKind::Zellij => &*self.zellij,
            HostKind::Native => &*self.native,
        }
    }

    fn other(which: HostKind) -> HostKind {
        match which {
            HostKind::Zellij => HostKind::Native,
            HostKind::Native => HostKind::Zellij,
        }
    }

    /// `None` is a host that doesn't have the name. A host that can't say
    /// is an error, never `None`.
    fn liveness_of(
        host: &dyn SessionHost,
        name: &SessionName,
    ) -> Result<Option<Liveness>, HostError> {
        Ok(host
            .liveness(OWNER_WITHIN)?
            .into_iter()
            .find_map(|(listed, liveness)| (&listed == name).then_some(liveness)))
    }

    /// What the hosts say about `name`, asking the fewest. Native's records
    /// are a directory listing, so they are read first, and zellij, which
    /// runs a process to answer, is asked only when native has a record or
    /// native is where new sessions go. A name native has no record of and
    /// that new sessions don't go to native for is zellij's, asked nothing.
    fn says(&self, name: &SessionName) -> Result<Says, HostError> {
        let native = Self::liveness_of(&*self.native, name)?;
        if native.is_none() && self.kind == HostKind::Zellij {
            return Ok(Says {
                zellij: None,
                native,
            });
        }
        Ok(Says {
            zellij: Self::liveness_of(&*self.zellij, name)?,
            native,
        })
    }

    fn says_both(&self, name: &SessionName) -> Result<Says, HostError> {
        Ok(Says {
            zellij: Self::liveness_of(&*self.zellij, name)?,
            native: Self::liveness_of(&*self.native, name)?,
        })
    }

    fn owner(&self, name: &SessionName) -> Result<HostKind, HostError> {
        Ok(self.says(name)?.owner(self.kind))
    }
}

impl SessionHost for Hosts {
    /// The limit of the host new sessions go to. Sessions of the other host
    /// that are over it still list, as unhostable ones that can be attached
    /// and removed but not started again.
    fn name_limit(&self) -> NameLimit {
        self.host(self.kind).name_limit()
    }

    fn sessions(&self) -> Result<Vec<SessionInfo>, HostError> {
        Ok(self.listing()?.sessions)
    }

    /// Each host is listed once, and what each says of a shared name is
    /// read from that listing. A native host that can't be read leaves
    /// zellij's sessions listed, with a skipped host to say so.
    fn listing(&self) -> Result<HostListing, HostError> {
        let (native, skipped) = match self.native.sessions() {
            Ok(native) => (native, Vec::new()),
            Err(error) => (Vec::new(), vec![error]),
        };
        if native.is_empty() {
            let mut listing = self.zellij.listing()?;
            listing.skipped.extend(skipped);
            return Ok(listing);
        }
        let mut zellij: HashMap<SessionName, SessionInfo> = self
            .zellij
            .sessions()?
            .into_iter()
            .map(|info| (info.name.clone(), info))
            .collect();
        let mut sessions = Vec::new();
        let mut doubled = Vec::new();
        for info in native {
            let Some(other) = zellij.remove(&info.name) else {
                sessions.push(info);
                continue;
            };
            let says = Says {
                zellij: Some(liveness_of_status(&other.status)),
                native: Some(liveness_of_status(&info.status)),
            };
            if says.doubled() {
                doubled.push(info.name.clone());
            }
            sessions.push(match says.owner(self.kind) {
                HostKind::Zellij => other,
                HostKind::Native => info,
            });
        }
        sessions.extend(zellij.into_values());
        sessions.sort_by(|a, b| a.name.cmp(&b.name));
        doubled.sort();
        Ok(HostListing {
            sessions,
            doubled,
            skipped,
        })
    }

    fn liveness(&self, within: Duration) -> Result<Vec<(SessionName, Liveness)>, HostError> {
        let native = self.native.liveness(within).unwrap_or_default();
        if native.is_empty() {
            return self.zellij.liveness(within);
        }
        let mut merged: Vec<(SessionName, Liveness)> =
            said_by(self.zellij.liveness(within)?, native)
                .into_iter()
                .filter_map(|(name, says)| {
                    let liveness = says.of(says.owner(self.kind))?;
                    Some((name, liveness))
                })
                .collect();
        merged.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(merged)
    }

    /// Attaching is the one call that goes on when the hosts can't say who
    /// owns the name: the host's own attach reports what is wrong.
    fn attach(&self, name: &SessionName, how: Attachment) -> LaunchSpec {
        let owner = self.owner(name).unwrap_or(self.kind);
        self.host(owner).attach(name, how)
    }

    fn observe(&self, name: &SessionName) -> Result<Observation, HostError> {
        self.host(self.owner(name)?).observe(name)
    }

    fn create(
        &self,
        name: &HostName,
        pane: &PaneCommand,
        how: Attachment,
    ) -> Result<LaunchSpec, HostError> {
        let leftover = Self::other(self.kind);
        let session = name.session();
        match Self::liveness_of(self.host(leftover), session)? {
            Some(Liveness::Live) => {
                return Err(HostError::RunningElsewhere {
                    name: session.clone(),
                    under: leftover,
                });
            }
            Some(Liveness::Exited) => self.host(leftover).delete(session)?,
            None => {}
        }
        self.host(self.kind).create(name, pane, how)
    }

    fn start_detached(&self, name: &SessionName) -> Result<(), HostError> {
        self.host(self.owner(name)?).start_detached(name)
    }

    /// Every host that runs the session stops it, so a name run by two
    /// is gone from both. A name neither runs goes to its owner.
    fn kill(&self, session: &Running) -> Result<(), HostError> {
        let says = self.says(session.name())?;
        let mut running = says.running_in();
        if running.is_empty() {
            running.push(says.owner(self.kind));
        }
        for host in running {
            self.host(host).kill(session)?;
        }
        Ok(())
    }

    fn delete(&self, name: &SessionName) -> Result<(), HostError> {
        let says = self.says_both(name)?;
        for host in BOTH {
            if says.of(host).is_some() {
                self.host(host).delete(name)?;
            }
        }
        Ok(())
    }

    fn write(&self, name: &SessionName, bytes: &[u8]) -> Result<(), HostError> {
        self.host(self.owner(name)?).write(name, bytes)
    }

    fn screen(
        &self,
        name: &SessionName,
        extent: Extent,
        rendering: Rendering,
    ) -> Result<String, HostError> {
        self.host(self.owner(name)?).screen(name, extent, rendering)
    }

    fn clients(&self) -> Option<&dyn ClientControl> {
        (self.zellij.clients().is_some() || self.native.clients().is_some()).then_some(self)
    }

    fn own_session(&self, var: &dyn Fn(&str) -> Option<String>) -> Option<SessionName> {
        self.zellij
            .own_session(var)
            .or_else(|| self.native.own_session(var))
    }

    fn nesting_env(&self) -> Vec<&'static str> {
        let mut vars = self.zellij.nesting_env();
        vars.extend(self.native.nesting_env());
        vars
    }
}

impl ClientControl for Hosts {
    fn windows(&self, own: Option<ClientId>) -> Result<Vec<Window>, HostError> {
        let mut windows = Vec::new();
        for host in [&self.zellij, &self.native] {
            if let Some(clients) = host.clients() {
                windows.extend(clients.windows(own)?);
            }
        }
        windows.sort_by_key(|window| window.client.get());
        Ok(windows)
    }

    /// A window is closed by the host its handle names.
    fn detach(&self, windows: &[Window]) -> Result<(), HostError> {
        for host in BOTH {
            let theirs: Vec<Window> = windows
                .iter()
                .filter(|window| window.handle.host() == host)
                .cloned()
                .collect();
            if let Some(control) = self.host(host).clients().filter(|_| !theirs.is_empty()) {
                control.detach(&theirs)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use agent_core::names::HostName;
    use agent_core::ops;
    use agent_core::ports::{Place, SessionStatus, WindowHandle};
    use agent_core::testing::FakeHost;

    use super::*;

    fn name(text: &str) -> SessionName {
        text.parse().expect("a session name")
    }

    fn live() -> SessionStatus {
        SessionStatus::Live { cwd: None }
    }

    fn exited() -> SessionStatus {
        SessionStatus::Exited { cwd: None }
    }

    fn limit(bytes: usize) -> NameLimit {
        NameLimit::new(std::num::NonZeroUsize::new(bytes).expect("nonzero"))
    }

    fn hostname(limit: NameLimit, text: &str) -> HostName {
        limit.admit(name(text)).expect("within the limit")
    }

    fn pane() -> PaneCommand {
        PaneCommand {
            program: "agent".into(),
            args: vec!["exec".into(), "x".into()],
            cwd: "/w".into(),
        }
    }

    /// A `FakeHost` that says which host it is, in the programs it launches.
    struct Tagged {
        tag: &'static str,
        limit: NameLimit,
        fake: Arc<FakeHost>,
        windows: Mutex<Vec<Window>>,
        detached: Mutex<Vec<Window>>,
        has_clients: bool,
        asked: Mutex<Vec<&'static str>>,
    }

    impl Tagged {
        fn new(tag: &'static str, limit: NameLimit, sessions: &[(&str, SessionStatus)]) -> Self {
            let fake = FakeHost::default();
            *fake.sessions.lock().expect("lock") = sessions
                .iter()
                .map(|(text, status)| SessionInfo {
                    name: name(text),
                    status: status.clone(),
                })
                .collect();
            Self {
                tag,
                limit,
                fake: Arc::new(fake),
                windows: Mutex::new(Vec::new()),
                detached: Mutex::new(Vec::new()),
                has_clients: false,
                asked: Mutex::new(Vec::new()),
            }
        }

        fn showing(mut self, windows: Vec<Window>) -> Self {
            self.has_clients = true;
            *self.windows.lock().expect("lock") = windows;
            self
        }
    }

    struct Shared(Arc<Tagged>);

    impl SessionHost for Shared {
        fn name_limit(&self) -> NameLimit {
            self.0.limit
        }
        fn sessions(&self) -> Result<Vec<SessionInfo>, HostError> {
            self.0.asked.lock().expect("lock").push("sessions");
            self.0.fake.sessions()
        }
        fn liveness(&self, within: Duration) -> Result<Vec<(SessionName, Liveness)>, HostError> {
            self.0.asked.lock().expect("lock").push("liveness");
            self.0.fake.liveness(within)
        }
        fn attach(&self, name: &SessionName, _: Attachment) -> LaunchSpec {
            LaunchSpec::new(self.0.tag).arg(name.as_str())
        }
        fn observe(&self, name: &SessionName) -> Result<Observation, HostError> {
            let mut observation = self.0.fake.observe(name)?;
            observation.spec = LaunchSpec::new(self.0.tag).arg(name.as_str());
            Ok(observation)
        }
        fn create(
            &self,
            name: &HostName,
            pane: &PaneCommand,
            how: Attachment,
        ) -> Result<LaunchSpec, HostError> {
            name.within(self.0.limit)?;
            self.0.fake.create(name, pane, how)?;
            Ok(LaunchSpec::new(self.0.tag).arg(name.session().as_str()))
        }
        fn start_detached(&self, name: &SessionName) -> Result<(), HostError> {
            self.0.fake.start_detached(name)
        }
        fn kill(&self, session: &Running) -> Result<(), HostError> {
            self.0.fake.kill(session)
        }
        fn delete(&self, name: &SessionName) -> Result<(), HostError> {
            self.0.fake.delete(name)
        }
        fn write(&self, name: &SessionName, bytes: &[u8]) -> Result<(), HostError> {
            self.0.fake.write(name, bytes)
        }
        fn screen(
            &self,
            name: &SessionName,
            extent: Extent,
            rendering: Rendering,
        ) -> Result<String, HostError> {
            Ok(self.0.tag.to_owned() + &self.0.fake.screen(name, extent, rendering)?)
        }
        fn clients(&self) -> Option<&dyn ClientControl> {
            self.0.has_clients.then_some(self)
        }
        fn own_session(&self, var: &dyn Fn(&str) -> Option<String>) -> Option<SessionName> {
            let variable = if self.0.tag == "zellij" {
                "ZELLIJ_SESSION_NAME"
            } else {
                "AGENT_SESSION"
            };
            var(variable)?.parse().ok()
        }
        fn nesting_env(&self) -> Vec<&'static str> {
            Vec::new()
        }
    }

    impl ClientControl for Shared {
        fn windows(&self, _: Option<ClientId>) -> Result<Vec<Window>, HostError> {
            Ok(self.0.windows.lock().expect("lock").clone())
        }
        fn detach(&self, windows: &[Window]) -> Result<(), HostError> {
            self.0
                .detached
                .lock()
                .expect("lock")
                .extend(windows.iter().cloned());
            Ok(())
        }
    }

    struct Pair {
        zellij: Arc<Tagged>,
        native: Arc<Tagged>,
        hosts: Hosts,
    }

    fn pair(kind: HostKind, zellij: Tagged, native: Tagged) -> Pair {
        let (zellij, native) = (Arc::new(zellij), Arc::new(native));
        let hosts = Hosts::new(
            Box::new(Shared(zellij.clone())),
            Box::new(Shared(native.clone())),
            kind,
        );
        Pair {
            zellij,
            native,
            hosts,
        }
    }

    fn sessions(
        kind: HostKind,
        zellij: &[(&str, SessionStatus)],
        native: &[(&str, SessionStatus)],
    ) -> Pair {
        pair(
            kind,
            Tagged::new("zellij", limit(20), zellij),
            Tagged::new("native", limit(48), native),
        )
    }

    fn program(spec: &LaunchSpec) -> String {
        spec.program().to_string_lossy().into_owned()
    }

    #[test]
    fn a_live_zellij_session_is_zellijs_even_with_a_native_record_of_the_name() {
        let p = sessions(HostKind::Native, &[("fix", live())], &[("fix", exited())]);
        assert_eq!(
            program(&p.hosts.attach(&name("fix"), Attachment::Window)),
            "zellij"
        );
    }

    #[test]
    fn a_live_native_host_beats_an_exited_zellij_session() {
        let p = sessions(HostKind::Zellij, &[("fix", exited())], &[("fix", live())]);
        assert_eq!(
            program(&p.hosts.attach(&name("fix"), Attachment::Window)),
            "native"
        );
    }

    #[test]
    fn a_name_only_one_host_knows_belongs_to_it_whatever_the_default() {
        let p = sessions(HostKind::Native, &[("old", exited())], &[("new", exited())]);
        assert_eq!(
            program(&p.hosts.attach(&name("old"), Attachment::Window)),
            "zellij"
        );
        assert_eq!(
            program(&p.hosts.attach(&name("new"), Attachment::Window)),
            "native"
        );
        let p = sessions(HostKind::Zellij, &[("old", exited())], &[("new", exited())]);
        assert_eq!(
            program(&p.hosts.attach(&name("new"), Attachment::Window)),
            "native"
        );
    }

    #[test]
    fn a_name_both_hosts_have_only_dead_goes_to_the_default_and_an_unknown_one_too() {
        for (kind, tag) in [(HostKind::Zellij, "zellij"), (HostKind::Native, "native")] {
            let p = sessions(kind, &[("both", exited())], &[("both", exited())]);
            assert_eq!(
                program(&p.hosts.attach(&name("both"), Attachment::Window)),
                tag
            );
            assert_eq!(
                program(&p.hosts.attach(&name("nobody"), Attachment::Window)),
                tag
            );
        }
    }

    #[test]
    fn a_name_live_in_both_is_listed_once_as_zellijs_and_reported_doubled() {
        let p = sessions(
            HostKind::Native,
            &[("fix", live()), ("z", live())],
            &[("fix", live()), ("n", exited())],
        );
        let listing = p.hosts.listing().expect("lists");
        assert_eq!(listing.doubled, [name("fix")]);
        let names: Vec<_> = listing.sessions.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["fix", "n", "z"]);
        assert_eq!(
            program(&p.hosts.attach(&name("fix"), Attachment::Window)),
            "zellij"
        );
    }

    #[test]
    fn a_stale_exited_twin_is_not_a_double_and_the_live_one_is_listed() {
        let p = sessions(HostKind::Native, &[("fix", exited())], &[("fix", live())]);
        let listing = p.hosts.listing().expect("lists");
        assert_eq!(listing.doubled, Vec::<SessionName>::new());
        assert_eq!(listing.sessions.len(), 1);
        assert_eq!(listing.sessions[0].status, live());
    }

    #[test]
    fn liveness_merges_both_hosts_with_the_owner_deciding_a_shared_name() {
        let p = sessions(
            HostKind::Native,
            &[("a", live()), ("b", exited()), ("c", exited())],
            &[("b", live()), ("c", exited()), ("d", live())],
        );
        let merged = p.hosts.liveness(Duration::from_secs(1)).expect("merges");
        assert_eq!(
            merged,
            [
                (name("a"), Liveness::Live),
                (name("b"), Liveness::Live),
                (name("c"), Liveness::Exited),
                (name("d"), Liveness::Live),
            ]
        );
    }

    #[test]
    fn liveness_fails_when_zellij_fails_and_leaves_zellijs_when_native_does() {
        let p = sessions(HostKind::Zellij, &[("a", live())], &[("n", live())]);
        *p.native.fake.liveness_fails.lock().expect("lock") = true;
        assert_eq!(
            p.hosts.liveness(Duration::from_secs(1)).expect("zellij's"),
            [(name("a"), Liveness::Live)]
        );
        *p.zellij.fake.liveness_fails.lock().expect("lock") = true;
        *p.native.fake.liveness_fails.lock().expect("lock") = false;
        assert!(p.hosts.liveness(Duration::from_secs(1)).is_err());
    }

    #[test]
    fn the_name_limit_is_the_default_hosts_so_long_native_names_need_native_as_default() {
        let zellij = sessions(HostKind::Zellij, &[], &[]);
        assert_eq!(zellij.hosts.name_limit().bytes(), 20);
        let native = sessions(HostKind::Native, &[], &[]);
        assert_eq!(native.hosts.name_limit().bytes(), 48);
    }

    #[test]
    fn a_native_session_over_zellijs_limit_still_lists_and_routes_to_native() {
        let long = "a-native-name-over-twenty-bytes";
        let p = sessions(HostKind::Zellij, &[], &[(long, live())]);
        let listed = p.hosts.sessions().expect("lists");
        assert_eq!(listed[0].name.as_str(), long);
        assert_eq!(
            program(&p.hosts.attach(&name(long), Attachment::Window)),
            "native"
        );
        let refused = p
            .hosts
            .create(&hostname(limit(48), long), &pane(), Attachment::Window)
            .expect_err("it runs natively already");
        assert!(
            matches!(
                &refused,
                HostError::RunningElsewhere {
                    under: HostKind::Native,
                    ..
                }
            ),
            "{refused:?}"
        );
        let fresh = sessions(HostKind::Zellij, &[], &[]);
        let too_long = fresh
            .hosts
            .create(&hostname(limit(48), long), &pane(), Attachment::Window)
            .expect_err("zellij can't create it");
        assert!(
            matches!(
                &too_long,
                HostError::Name(agent_core::names::NameError::TooLong { limit: 20, .. })
            ),
            "{too_long:?}"
        );
    }

    #[test]
    fn kill_write_screen_and_observe_go_to_the_owner() {
        let p = sessions(HostKind::Zellij, &[("z", live())], &[("n", live())]);
        let kill_target = |text: &str| {
            let running = Running::for_testing(name(text));
            p.hosts.kill(&running).expect("kills");
        };
        kill_target("n");
        kill_target("z");
        assert_eq!(p.native.fake.killed(), [name("n")]);
        assert_eq!(p.zellij.fake.killed(), [name("z")]);
        p.hosts.write(&name("n"), b"hi").expect("writes");
        assert_eq!(
            p.native.fake.written.lock().expect("lock").as_slice(),
            [(name("n"), b"hi".to_vec())]
        );
        assert!(p.zellij.fake.written.lock().expect("lock").is_empty());
        assert_eq!(
            p.hosts
                .screen(&name("n"), Extent::Visible, Rendering::Styled)
                .expect("reads"),
            "native"
        );
        assert_eq!(
            p.hosts
                .screen(&name("z"), Extent::Visible, Rendering::Styled)
                .expect("reads"),
            "zellij"
        );
        let watching = p.hosts.observe(&name("n")).expect("observes");
        assert_eq!(program(&watching.spec), "native");
    }

    #[test]
    fn delete_removes_the_session_from_every_host_that_has_it() {
        let p = sessions(
            HostKind::Native,
            &[("both", exited()), ("z", exited())],
            &[("both", exited())],
        );
        p.hosts.delete(&name("both")).expect("deletes");
        assert_eq!(p.zellij.fake.deleted.lock().expect("lock").len(), 1);
        assert_eq!(p.native.fake.deleted.lock().expect("lock").len(), 1);
        p.hosts.delete(&name("z")).expect("deletes");
        assert_eq!(p.native.fake.deleted.lock().expect("lock").len(), 1);
    }

    #[test]
    fn new_sessions_go_to_the_configured_host() {
        for (kind, created_by) in [(HostKind::Zellij, "zellij"), (HostKind::Native, "native")] {
            let p = sessions(kind, &[], &[]);
            let spec = p
                .hosts
                .create(
                    &hostname(p.hosts.name_limit(), "fresh"),
                    &pane(),
                    Attachment::Window,
                )
                .expect("creates");
            assert_eq!(program(&spec), created_by);
        }
    }

    #[test]
    fn creating_over_the_other_hosts_dead_session_deletes_it_and_over_a_live_one_refuses() {
        let p = sessions(
            HostKind::Native,
            &[("old", exited()), ("busy", live())],
            &[],
        );
        let limit = p.hosts.name_limit();
        p.hosts
            .create(&hostname(limit, "old"), &pane(), Attachment::Window)
            .expect("creates");
        assert_eq!(
            p.zellij.fake.deleted.lock().expect("lock").as_slice(),
            [name("old")]
        );
        assert!(
            p.hosts
                .create(&hostname(limit, "busy"), &pane(), Attachment::Window)
                .is_err()
        );
        assert_eq!(p.native.fake.created.lock().expect("lock").len(), 1);
    }

    #[test]
    fn a_restart_over_the_router_stops_the_zellij_session_and_starts_it_natively() {
        let p = sessions(HostKind::Native, &[("fix", live())], &[]);
        let fix = name("fix");
        let w = agent_core::testing::World::new();
        w.existing_worktree("fix");
        w.add_session("fix", live());
        let at = w.agent("fix", agent_core::testing::Doing::Idle);
        let go = match ops::plan_restart(w.svc(), at)
            .expect("plans")
            .elsewhere()
            .expect("not own")
        {
            ops::RestartApproval::Free(go) => go,
            ops::RestartApproval::Needs(_) => panic!("idle with nothing to stop"),
        };
        let svc = ops::Services {
            host: &p.hosts,
            ..w.svc()
        };
        ops::restart(svc, go).expect("restarts");
        assert_eq!(p.zellij.fake.killed(), std::slice::from_ref(&fix));
        assert_eq!(p.native.fake.detached(), std::slice::from_ref(&fix));
        assert_eq!(
            p.hosts.liveness(Duration::from_secs(1)).expect("lists"),
            [(fix, Liveness::Live)]
        );
    }

    #[test]
    fn a_restart_of_a_name_live_in_both_hosts_stops_both() {
        let p = sessions(HostKind::Native, &[("fix", live())], &[("fix", live())]);
        let fix = name("fix");
        p.hosts
            .kill(&Running::for_testing(fix.clone()))
            .expect("kills");
        assert_eq!(p.zellij.fake.killed(), std::slice::from_ref(&fix));
        assert_eq!(p.native.fake.killed(), std::slice::from_ref(&fix));
    }

    #[test]
    fn a_host_that_cannot_say_who_owns_a_name_fails_the_call_instead_of_guessing() {
        let p = sessions(HostKind::Native, &[("fix", live())], &[]);
        *p.zellij.fake.liveness_fails.lock().expect("lock") = true;
        assert!(
            p.hosts
                .create(
                    &hostname(p.hosts.name_limit(), "fix"),
                    &pane(),
                    Attachment::Window
                )
                .is_err(),
            "the live twin must not be skipped"
        );
        assert!(p.native.fake.created.lock().expect("lock").is_empty());
        assert!(p.hosts.kill(&Running::for_testing(name("fix"))).is_err());
        assert_eq!(p.zellij.fake.killed(), Vec::<SessionName>::new());
        assert_eq!(p.native.fake.killed(), Vec::<SessionName>::new());
        p.hosts
            .delete(&name("fix"))
            .expect_err("can't tell where it is");
    }

    #[test]
    fn creating_over_a_live_twin_names_the_host_it_runs_under() {
        let p = sessions(HostKind::Native, &[("busy", live())], &[]);
        let refused = p
            .hosts
            .create(
                &hostname(p.hosts.name_limit(), "busy"),
                &pane(),
                Attachment::Window,
            )
            .expect_err("refused");
        assert!(matches!(
            refused,
            HostError::RunningElsewhere {
                under: HostKind::Zellij,
                ..
            }
        ));
        assert_eq!(
            refused.to_string(),
            "busy is already running under zellij, stop it there first"
        );
    }

    #[test]
    fn with_no_native_state_only_zellij_is_asked_and_only_what_it_was_asked_before() {
        let p = sessions(HostKind::Zellij, &[("z", live())], &[]);
        p.hosts.listing().expect("lists");
        p.hosts.attach(&name("z"), Attachment::Window);
        p.hosts.write(&name("z"), b"x").expect("writes");
        p.hosts
            .screen(&name("z"), Extent::Visible, Rendering::Styled)
            .expect("reads");
        p.hosts
            .kill(&Running::for_testing(name("z")))
            .expect("kills");
        assert_eq!(
            p.zellij.asked.lock().expect("lock").as_slice(),
            ["sessions"]
        );
        assert_eq!(p.zellij.fake.killed(), [name("z")]);
    }

    #[test]
    fn a_native_host_that_cannot_be_read_leaves_zellijs_sessions_and_a_skipped_host() {
        let p = sessions(HostKind::Zellij, &[("z", live())], &[("n", live())]);
        *p.native.fake.list_fails.lock().expect("lock") = true;
        let listing = p.hosts.listing().expect("zellij's sessions still list");
        let names: Vec<_> = listing.sessions.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["z"]);
        assert_eq!(listing.skipped.len(), 1);
    }

    #[test]
    fn inside_a_native_session_the_own_session_has_no_restart_to_approve() {
        let p = sessions(HostKind::Native, &[], &[("fix", live())]);
        let mut w = agent_core::testing::World::new();
        w.existing_worktree("fix");
        w.add_session("fix", live());
        let at = w.agent("fix", agent_core::testing::Doing::Idle);
        w.own = p
            .hosts
            .own_session(&|var| (var == "AGENT_SESSION").then(|| "fix".to_owned()));
        assert!(
            ops::plan_restart(w.svc(), at)
                .expect("plans")
                .elsewhere()
                .is_err(),
            "the TUI's own session can't be restarted from inside it"
        );
    }

    #[test]
    fn the_own_session_is_found_through_either_hosts_variable() {
        let p = sessions(HostKind::Zellij, &[], &[]);
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |var: &str| {
                pairs
                    .iter()
                    .find(|(name, _)| *name == var)
                    .map(|(_, value)| (*value).to_owned())
            }
        };
        assert_eq!(
            p.hosts
                .own_session(&env(&[("ZELLIJ_SESSION_NAME", "in-zellij")])),
            Some(name("in-zellij"))
        );
        assert_eq!(
            p.hosts.own_session(&env(&[("AGENT_SESSION", "in-native")])),
            Some(name("in-native"))
        );
        assert_eq!(p.hosts.own_session(&env(&[])), None);
    }

    fn window(host: HostKind, client: u32, session: &str, place: Place) -> Window {
        Window {
            client: ClientId::new(client),
            session: name(session),
            place,
            handle: WindowHandle::new(host, client + 1000),
        }
    }

    #[test]
    fn windows_of_both_hosts_merge_and_a_client_is_detached_by_the_host_that_lists_it() {
        let p = pair(
            HostKind::Native,
            Tagged::new("zellij", limit(20), &[]).showing(vec![window(
                HostKind::Zellij,
                30,
                "z",
                Place::Nvim,
            )]),
            Tagged::new("native", limit(48), &[]).showing(vec![window(
                HostKind::Native,
                20,
                "n",
                Place::Ssh,
            )]),
        );
        let windows = p
            .hosts
            .clients()
            .expect("has clients")
            .windows(None)
            .expect("lists");
        let clients: Vec<u32> = windows.iter().map(|w| w.client.get()).collect();
        assert_eq!(clients, [20, 30]);
        p.hosts
            .clients()
            .expect("has clients")
            .detach(&windows)
            .expect("detaches");
        let detached = |host: &Tagged| -> Vec<u32> {
            host.detached
                .lock()
                .expect("lock")
                .iter()
                .map(|w| w.handle.key())
                .collect()
        };
        assert_eq!(detached(&p.native), [1020]);
        assert_eq!(detached(&p.zellij), [1030]);
    }

    #[test]
    fn a_window_is_detached_by_the_host_that_listed_it_even_when_that_host_no_longer_lists_it() {
        let p = pair(
            HostKind::Native,
            Tagged::new("zellij", limit(20), &[]).showing(Vec::new()),
            Tagged::new("native", limit(48), &[]).showing(Vec::new()),
        );
        let gone = window(HostKind::Native, 20, "n", Place::Ssh);
        p.hosts
            .clients()
            .expect("has clients")
            .detach(std::slice::from_ref(&gone))
            .expect("detaches");
        assert_eq!(p.native.detached.lock().expect("lock").len(), 1);
        assert!(p.zellij.detached.lock().expect("lock").is_empty());
    }

    #[test]
    fn with_no_client_control_in_either_host_there_is_none() {
        let p = sessions(HostKind::Zellij, &[], &[]);
        assert!(p.hosts.clients().is_none());
    }
}
