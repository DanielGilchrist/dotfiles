use std::num::NonZeroU16;

/// A terminal size with no zero dimension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TermSize {
    pub rows: NonZeroU16,
    pub cols: NonZeroU16,
}
