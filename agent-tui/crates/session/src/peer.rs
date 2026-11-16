use std::io;
use std::os::unix::net::UnixStream;

use agent_core::activity::Pid;
use nix::unistd::Uid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Peer {
    pub uid: Uid,
    pub pid: Pid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a connection from user {uid}, not {ours}")]
pub struct Foreign {
    pub uid: Uid,
    pub ours: Uid,
}

/// Admits only a peer that runs as the same user. A connection is checked
/// before any byte of it is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerPolicy {
    ours: Uid,
}

impl PeerPolicy {
    pub fn current_user() -> Self {
        Self::of(Uid::current())
    }

    pub fn of(ours: Uid) -> Self {
        Self { ours }
    }

    pub fn admit(self, peer: Peer) -> Result<Peer, Foreign> {
        if peer.uid == self.ours {
            Ok(peer)
        } else {
            Err(Foreign {
                uid: peer.uid,
                ours: self.ours,
            })
        }
    }
}

impl Peer {
    #[cfg(target_os = "linux")]
    pub fn of(stream: &UnixStream) -> io::Result<Self> {
        use nix::sys::socket::{getsockopt, sockopt::PeerCredentials};
        let credentials = getsockopt(stream, PeerCredentials)?;
        let pid = u32::try_from(credentials.pid())
            .ok()
            .and_then(Pid::new)
            .ok_or_else(|| io::Error::other("the peer has no process id"))?;
        Ok(Self {
            uid: Uid::from_raw(credentials.uid()),
            pid,
        })
    }

    #[cfg(target_os = "macos")]
    pub fn of(stream: &UnixStream) -> io::Result<Self> {
        use nix::sys::socket::{getsockopt, sockopt::LocalPeerPid};
        use nix::unistd::getpeereid;
        let (uid, _) = getpeereid(stream)?;
        let pid = u32::try_from(getsockopt(stream, LocalPeerPid)?)
            .ok()
            .and_then(Pid::new)
            .ok_or_else(|| io::Error::other("the peer has no process id"))?;
        Ok(Self { uid, pid })
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    pub fn of(_stream: &UnixStream) -> io::Result<Self> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(uid: u32) -> Peer {
        Peer {
            uid: Uid::from_raw(uid),
            pid: Pid::new(7).expect("a pid"),
        }
    }

    #[test]
    fn a_peer_of_another_user_is_refused() {
        let policy = PeerPolicy::of(Uid::from_raw(501));
        assert!(policy.admit(peer(501)).is_ok());
        let refused = policy.admit(peer(502)).expect_err("refused");
        assert_eq!(refused.uid, Uid::from_raw(502));
        assert_eq!(refused.ours, Uid::from_raw(501));
    }

    #[test]
    fn root_is_not_admitted_as_the_same_user() {
        assert!(PeerPolicy::of(Uid::from_raw(501)).admit(peer(0)).is_err());
    }

    #[test]
    fn the_kernel_reports_this_process_for_a_connection_from_it() {
        let (ours, _theirs) = UnixStream::pair().expect("pair");
        let found = Peer::of(&ours).expect("credentials");
        assert_eq!(found.uid, Uid::current());
        assert_eq!(Some(found.pid), Pid::current());
    }
}
