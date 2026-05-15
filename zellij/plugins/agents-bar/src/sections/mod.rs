//! Bar sections. Each section is a fn that takes the shared `Bar` state and
//! returns a `Cell` to render, or `None` to skip.
//!
//! To add a section: write a fn in a new module here, then append it to
//! `LEFT` or `RIGHT` below. Sections are rendered in declaration order.

use crate::{Bar, Cell};

pub mod agents;
pub mod attention;
pub mod focused;
pub mod mode;
pub mod page;
pub mod tab_dots;

pub type Section = fn(&Bar) -> Option<Cell>;

pub const LEFT: &[Section] = &[agents::render, focused::render, mode::render];
pub const RIGHT: &[Section] = &[attention::render, tab_dots::render, page::render];
