//! The native session host as a [`agent_core::ports::SessionHost`], and the
//! router that runs it beside zellij during the migration.

mod hosts;
mod native;

pub use hosts::Hosts;
pub use native::Native;
