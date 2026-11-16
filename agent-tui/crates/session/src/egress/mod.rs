//! What the host does with a program's output before a client's terminal
//! sees it (docs/host.md, "Queries, escape policy and clipboard"). One
//! [`Parser`] reads the byte stream. Each item it reads is one of three
//! things: forwarded to the clients it is meant for, a query the host
//! answers itself, or dropped, and the table says which for every class.
//! What the screen and the ledger are fed is what was forwarded, never the
//! program's own bytes.

mod classify;
mod filter;
mod parser;
mod table;

pub use filter::{Asked, Egress, Filtered};
pub use table::{Mode, ModeQuery, Palette, Policy, Query, Recipient, Tally};

#[cfg(test)]
mod tests;
