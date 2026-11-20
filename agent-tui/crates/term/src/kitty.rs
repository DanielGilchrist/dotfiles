const STACK_LIMIT: usize = 16;
const DISAMBIGUATE: u32 = 1;

/// The kitty keyboard flags a program has asked for, read from its output:
/// `CSI > flags u` pushes, `CSI < n u` pops and `CSI = flags ; mode u`
/// changes the top. The terminal emulator doesn't track them, and a key is
/// encoded differently once the program asked.
#[derive(Debug, Default)]
pub struct KittyFlags {
    stack: Vec<u32>,
    parse: Parse,
}

#[derive(Debug, Default)]
enum Parse {
    #[default]
    Ground,
    Escape,
    Csi {
        prefix: Option<u8>,
        params: Vec<u32>,
        current: Option<u32>,
    },
}

impl KittyFlags {
    pub fn feed(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.byte(byte);
        }
    }

    pub fn current(&self) -> u32 {
        self.stack.last().copied().unwrap_or(0)
    }

    pub fn disambiguates(flags: u32) -> bool {
        flags & DISAMBIGUATE != 0
    }

    fn byte(&mut self, byte: u8) {
        self.parse = match (std::mem::take(&mut self.parse), byte) {
            (_, 0x1b) => Parse::Escape,
            (Parse::Escape, b'[') => Parse::Csi {
                prefix: None,
                params: Vec::new(),
                current: None,
            },
            (
                Parse::Csi {
                    prefix: None,
                    params,
                    current: None,
                },
                prefix @ (b'>' | b'<' | b'=' | b'?'),
            ) if params.is_empty() => Parse::Csi {
                prefix: Some(prefix),
                params,
                current: None,
            },
            (
                Parse::Csi {
                    prefix,
                    params,
                    current,
                },
                digit @ b'0'..=b'9',
            ) => Parse::Csi {
                prefix,
                params,
                current: Some(
                    current
                        .unwrap_or(0)
                        .saturating_mul(10)
                        .saturating_add(u32::from(digit - b'0')),
                ),
            },
            (
                Parse::Csi {
                    prefix,
                    mut params,
                    current,
                },
                b';',
            ) => {
                params.push(current.unwrap_or(0));
                Parse::Csi {
                    prefix,
                    params,
                    current: None,
                }
            }
            (
                Parse::Csi {
                    prefix,
                    mut params,
                    current,
                },
                b'u',
            ) => {
                if let Some(last) = current {
                    params.push(last);
                }
                self.apply(prefix, &params);
                Parse::Ground
            }
            _ => Parse::Ground,
        };
    }

    fn apply(&mut self, prefix: Option<u8>, params: &[u32]) {
        let first = params.first().copied();
        match prefix {
            Some(b'>') => {
                if self.stack.len() == STACK_LIMIT {
                    self.stack.remove(0);
                }
                self.stack.push(first.unwrap_or(0));
            }
            Some(b'<') => {
                let count = usize::try_from(first.unwrap_or(1).max(1)).unwrap_or(usize::MAX);
                self.stack.truncate(self.stack.len().saturating_sub(count));
            }
            Some(b'=') => {
                let flags = first.unwrap_or(0);
                let mode = params.get(1).copied().unwrap_or(1);
                if self.stack.is_empty() {
                    self.stack.push(0);
                }
                if let Some(top) = self.stack.last_mut() {
                    match mode {
                        2 => *top |= flags,
                        3 => *top &= !flags,
                        _ => *top = flags,
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flags(output: &[u8]) -> u32 {
        let mut kitty = KittyFlags::default();
        kitty.feed(output);
        kitty.current()
    }

    #[test]
    fn a_push_sets_the_flags_and_a_pop_restores_the_previous() {
        assert_eq!(flags(b"\x1b[>1u"), 1);
        assert_eq!(flags(b"\x1b[>1u\x1b[>5u"), 5);
        assert_eq!(flags(b"\x1b[>1u\x1b[>5u\x1b[<u"), 1);
        assert_eq!(flags(b"\x1b[>1u\x1b[<u"), 0);
    }

    #[test]
    fn a_change_sets_adds_or_removes_flags_on_the_top() {
        assert_eq!(flags(b"\x1b[>1u\x1b[=4;2u"), 5);
        assert_eq!(flags(b"\x1b[>5u\x1b[=4;3u"), 1);
        assert_eq!(flags(b"\x1b[>1u\x1b[=8u"), 8);
    }

    #[test]
    fn a_sequence_split_across_feeds_still_counts_and_a_query_does_not() {
        let mut kitty = KittyFlags::default();
        kitty.feed(b"text\x1b[>");
        kitty.feed(b"1");
        kitty.feed(b"u more");
        assert_eq!(kitty.current(), 1);
        kitty.feed(b"\x1b[?u");
        assert_eq!(kitty.current(), 1);
    }
}
