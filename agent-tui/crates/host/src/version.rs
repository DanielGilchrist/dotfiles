#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(u16);

impl Version {
    pub const fn new(raw: u16) -> Self {
        Self(raw)
    }

    pub const fn get(self) -> u16 {
        self.0
    }
}

/// The versions a host or client speaks, never empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VersionRange {
    min: Version,
    max: Version,
}

impl VersionRange {
    pub fn new(min: Version, max: Version) -> Option<Self> {
        (min <= max).then_some(Self { min, max })
    }

    pub fn min(self) -> Version {
        self.min
    }

    pub fn max(self) -> Version {
        self.max
    }

    pub fn admits(self, version: Version) -> Admission {
        if version < self.min {
            Admission::TooOld
        } else if version > self.max {
            Admission::TooNew
        } else {
            Admission::Speaks
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    Speaks,
    TooOld,
    TooNew,
}

pub const PROTOCOL_VERSION: Version = Version::new(1);

/// What this build accepts from a peer.
pub const SUPPORTED: VersionRange = VersionRange {
    min: PROTOCOL_VERSION,
    max: PROTOCOL_VERSION,
};
