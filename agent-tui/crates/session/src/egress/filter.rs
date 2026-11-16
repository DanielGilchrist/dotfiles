use super::parser::{Parser, Sink};
use super::table::{Class, Policy, Query, Recipient, Tally, Verdict};

/// A query the program printed, and where in the owner's stream it sat: the
/// bytes of that stream before it. The screen is fed that stream, so a reply
/// reads the screen as it was at exactly that point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Asked {
    pub at: usize,
    pub query: Query,
}

/// What one chunk of the program's output comes to: the bytes each kind of
/// client is sent, the queries the host answers, and what was dropped. The
/// owner's bytes are everything that is forwarded to anyone, so they are what
/// the screen is fed.
#[derive(Debug, PartialEq, Eq)]
pub struct Filtered {
    pub owner: Vec<u8>,
    pub watchers: Vec<u8>,
    pub asked: Vec<Asked>,
    pub refused: Tally,
}

struct Collecting {
    policy: Policy,
    filtered: Filtered,
}

impl Collecting {
    fn new(policy: Policy) -> Self {
        Self {
            policy,
            filtered: Filtered {
                owner: Vec::new(),
                watchers: Vec::new(),
                asked: Vec::new(),
                refused: Tally::default(),
            },
        }
    }
}

impl Sink for Collecting {
    fn handle(&mut self, bytes: &[u8], class: Class) {
        let filtered = &mut self.filtered;
        match class.verdict(self.policy) {
            Verdict::Forward(audience) => {
                if audience.includes(Recipient::Owner) {
                    filtered.owner.extend_from_slice(bytes);
                }
                if audience.includes(Recipient::Watcher) {
                    filtered.watchers.extend_from_slice(bytes);
                }
            }
            Verdict::Answer(query) => filtered.asked.push(Asked {
                at: filtered.owner.len(),
                query,
            }),
            Verdict::Drop(refusal) => filtered.refused.add(refusal),
        }
    }
}

/// The filter between a program's output and its clients, which is the
/// allowlist in [`super::table`] applied to what [`Parser`] reads.
pub struct Egress {
    policy: Policy,
    parser: Parser,
}

impl Egress {
    pub fn new(policy: Policy) -> Self {
        Self {
            policy,
            parser: Parser::new(),
        }
    }

    /// Reads the next chunk of the program's output. What a chunk leaves half
    /// read is kept for the next, so the bytes a client is sent are the same
    /// however the output was split.
    pub fn feed(&mut self, bytes: &[u8]) -> Filtered {
        let mut collecting = Collecting::new(self.policy);
        self.parser.feed(bytes, &mut collecting);
        collecting.filtered
    }

    /// What a replay or a dump is once it has gone through the same allowlist
    /// as live output, for one recipient, so it never holds what live output
    /// couldn't. It is read by a parser of its own, since it is a stream of
    /// its own.
    pub fn clean(&self, bytes: &[u8], recipient: Recipient) -> Cleaned {
        let mut collecting = Collecting::new(self.policy);
        Parser::new().feed(bytes, &mut collecting);
        let Filtered {
            owner,
            watchers,
            refused,
            ..
        } = collecting.filtered;
        let bytes = match recipient {
            Recipient::Owner => owner,
            Recipient::Watcher => watchers,
        };
        Cleaned { bytes, refused }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Cleaned {
    pub bytes: Vec<u8>,
    pub refused: Tally,
}
