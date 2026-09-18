//! IO-free implementation of the NNTP wire grammar.
//!
//! This crate contains no sockets, no clock and no global state: every entry point is a
//! function over bytes or strings. That makes the grammar testable against input a real
//! server would never send, which is exactly the input that breaks news readers. The
//! layering is described in [ADR-0002].
//!
//! # What it covers
//!
//! - [`response`] — status lines and response-code classification (RFC 3977 §3.2).
//! - [`command`] — typed commands and their encoding, with CRLF-injection refused.
//! - [`block`] — multi-line data blocks and dot-stuffing (RFC 3977 §3.1.1).
//! - [`capabilities`] — the `CAPABILITIES` response (RFC 3977 §5.2).
//! - [`group`], [`list`] — `GROUP` and the `LIST` family.
//! - [`overview`] — `OVER`/`XOVER` records and `LIST OVERVIEW.FMT`.
//! - [`headers`], [`article`] — RFC 5322 headers, folding, bodies and their encodings.
//! - [`mime`] — RFC 2047 encoded words, charsets, `quoted-printable`, base64.
//! - [`date`] — `Date` headers, including the obsolete and malformed forms.
//!
//! # Example
//!
//! Parsing a response to `GROUP`, then an overview line from the group it selected:
//!
//! ```
//! use nntp_proto::group::GroupSummary;
//! use nntp_proto::overview::{OverviewFmt, OverviewRecord};
//! use nntp_proto::response::StatusLine;
//!
//! let status = StatusLine::parse(b"211 1234 3000234 3002322 misc.test\r\n")?;
//! let summary = GroupSummary::parse(&status, None)?;
//! assert_eq!(summary.range(), Some((3_000_234, 3_002_322)));
//!
//! let line = b"3000234\t=?UTF-8?Q?caf=C3=A9?=\ta@example.net\t\t<a@b>\t\t42\t2";
//! let record = OverviewRecord::parse(line, &OverviewFmt::standard())?;
//! assert_eq!(record.subject, "café");
//! assert_eq!(record.bytes, Some(42));
//! # Ok::<(), nntp_proto::ProtoError>(())
//! ```
//!
//! # Robustness
//!
//! Malformed input is expected, not exceptional. Parsers either return a [`ProtoError`] or
//! degrade to a partial value, and the choice is made per construct: a status line that
//! cannot be read is fatal to the response, whereas an article with an unparseable `Date`
//! is still worth showing. Where a block is parsed as a whole, the lines that failed are
//! returned alongside the ones that succeeded (see [`list::ListResult`]) so a hundred
//! thousand good group entries are not lost to one bad line.
//!
//! [ADR-0002]: https://github.com/edusouza/rust-nntp/blob/main/docs/adr/0002-layered-workspace.md
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

pub mod article;
pub mod block;
pub mod body;
pub mod capabilities;
pub mod command;
pub mod date;
pub mod error;
pub mod group;
pub mod hdr;
pub mod headers;
pub mod list;
pub mod message_id;
pub mod mime;
pub mod overview;
pub mod post;
pub mod response;
pub mod spec;
pub mod thread;

pub use article::{Article, ContentType, TransferEncoding};
pub use block::DataBlock;
pub use body::{BodyPart, Disposition, DispositionKind, strip_clearsign, unflow};
pub use capabilities::Capabilities;
pub use command::{Command, ListKeyword, Wildmat};
pub use error::{ProtoError, Result};
pub use group::{GroupName, GroupSummary, PostingStatus};
pub use hdr::HeaderEntry;
pub use headers::{HeaderName, HeaderValue, Headers};
pub use list::{ActiveEntry, ActiveTimesEntry, ListResult, NewsgroupEntry};
pub use message_id::MessageId;
pub use overview::{OverviewFmt, OverviewRecord};
pub use post::{Draft, FollowUp};
pub use response::{ResponseCode, ResponseKind, StatusLine};
pub use spec::{ArticleSpec, Range, RangeOrId};
pub use thread::{ThreadNode, thread};
