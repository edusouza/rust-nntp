//! Which articles have been read, and where that is remembered.
//!
//! A newsreader that cannot tell you what is new is doing its main job badly, and until
//! now this one forgot everything when it closed ([#7]). The state is two things:
//!
//! - [`ReadSet`] — the set of read article numbers for one group, stored as ranges in the
//!   `.newsrc` syntax. Pure data, no IO, exhaustively tested.
//! - the store — where those sets live between runs. Deliberately a separate concern, and
//!   a separate commit, because the representation is the part other programs can read.
//!
//! [#7]: https://github.com/edusouza/rust-nntp/issues/7

mod set;

pub use set::ReadSet;
