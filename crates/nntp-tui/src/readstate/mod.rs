//! Which articles have been read, and where that is remembered.
//!
//! A newsreader that cannot tell you what is new is doing its main job badly, and until
//! now this one forgot everything when it closed ([#7]). The state is two things:
//!
//! - [`ReadSet`] — the set of read article numbers for one group, stored as ranges in the
//!   `.newsrc` syntax. Pure data, no IO, exhaustively tested.
//! - [`ReadStore`] — where those sets live between runs: one `.newsrc`-format file per
//!   server, since article numbers are the server's and mean nothing on another one.
//!
//! [#7]: https://github.com/edusouza/rust-nntp/issues/7

mod set;
mod store;

pub use set::ReadSet;
pub use store::{MAX_STORE_BYTES, Problem, ReadStore};
