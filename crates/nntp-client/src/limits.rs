//! Size limits applied to everything read from a server.
//!
//! A news server can send an unbounded amount of data in response to a single command.
//! Without limits, a hostile or merely broken peer can exhaust memory by never sending a
//! block terminator, or by sending one line that never ends. Both have to be bounded, and
//! the bounds have to be generous enough for real traffic: binary groups carry articles of
//! tens of megabytes, and `Path` headers on a widely propagated article run to kilobytes.

/// Limits on response sizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// The largest single response line, in octets, excluding its CRLF.
    ///
    /// RFC 3977 §3.1 bounds *command* lines at 512 octets but says nothing about response
    /// lines, and header lines in the wild exceed 1 KiB.
    pub max_line_len: usize,

    /// The largest multi-line block, in octets of line content.
    pub max_block_bytes: usize,

    /// The largest number of lines in a multi-line block.
    ///
    /// `LIST ACTIVE` against a full feed returns well over 100 000 lines, so this is
    /// generous by necessity.
    pub max_block_lines: usize,
}

impl Limits {
    /// The default limits: 64 KiB per line, 32 MiB and 4 000 000 lines per block.
    pub const DEFAULT: Self = Self {
        max_line_len: 64 * 1024,
        max_block_bytes: 32 * 1024 * 1024,
        max_block_lines: 4_000_000,
    };

    /// Tight limits for paths where a large response is a bug rather than a possibility,
    /// such as a capability probe: 8 KiB per line, 1 MiB and 10 000 lines per block.
    pub const SMALL: Self = Self {
        max_line_len: 8 * 1024,
        max_block_bytes: 1024 * 1024,
        max_block_lines: 10_000,
    };

    /// Limits with every bound set high enough not to interfere, for tests.
    pub const UNLIMITED: Self = Self {
        max_line_len: usize::MAX,
        max_block_bytes: usize::MAX,
        max_block_lines: usize::MAX,
    };
}

impl Default for Limits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_limits_are_generous_but_finite() {
        let limits = Limits::default();
        assert_eq!(limits, Limits::DEFAULT);
        // A 40 MiB binary article must be refused by the default block limit, and a
        // 2 KiB Path header must not be refused by the line limit.
        assert!(limits.max_block_bytes < 40 * 1024 * 1024);
        assert!(limits.max_line_len > 2048);
        // LIST ACTIVE on a full feed is around 120 000 lines.
        assert!(limits.max_block_lines > 200_000);
    }

    #[test]
    fn small_limits_are_tighter_than_the_defaults() {
        const {
            assert!(Limits::SMALL.max_line_len < Limits::DEFAULT.max_line_len);
            assert!(Limits::SMALL.max_block_bytes < Limits::DEFAULT.max_block_bytes);
            assert!(Limits::SMALL.max_block_lines < Limits::DEFAULT.max_block_lines);
        }
    }
}
