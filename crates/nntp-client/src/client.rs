//! The conversation: one method per NNTP command, with the session state they depend on.

use std::io::{Read, Write};

use chrono::{DateTime, Utc};
use nntp_proto::block::DataBlock;
use nntp_proto::response::codes;
use nntp_proto::{
    ActiveEntry, Article, ArticleSpec, Capabilities, Command, Draft, GroupName, GroupSummary,
    ListKeyword, ListResult, MessageId, NewsgroupEntry, OverviewFmt, OverviewRecord, Range,
    RangeOrId, ResponseCode, StatusLine, Wildmat, date,
};

use crate::connection::Connection;
use crate::error::{ClientError, Result};
use crate::limits::Limits;

/// The server's opening banner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Greeting {
    /// The status code: `200`, `201`, or an error.
    pub code: ResponseCode,
    /// The banner text, often naming the server software.
    pub text: String,
    /// Whether the greeting said posting is allowed (`200` rather than `201`).
    ///
    /// Only a hint: a server may still refuse an individual group, and one that greets
    /// with `201` may allow posting after authentication.
    pub posting_allowed: bool,
}

/// How to build a client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ClientOptions {
    /// Response size limits.
    pub limits: Limits,
    /// Whether the transport is encrypted.
    ///
    /// Defaults to `false`, which is the safe assumption: credentials are refused on an
    /// unencrypted link unless the caller explicitly allows it. The connector sets this
    /// to `true` after a successful TLS handshake.
    pub encrypted: bool,
}

/// Which overview command this server understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OverviewStyle {
    /// Not yet determined.
    Unknown,
    /// RFC 3977 `OVER`.
    Over,
    /// RFC 2980 `XOVER`.
    XOver,
    /// Neither is available.
    Unavailable,
}

/// An article's identity as reported by a status line: `223 number message-id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArticleId {
    /// The article number, or `None` when the server reported `0` because the article was
    /// named by message-id and no group is selected.
    pub number: Option<u64>,
    /// The message-id, if the server reported a well-formed one.
    pub message_id: Option<MessageId>,
}

impl ArticleId {
    fn parse(line: &StatusLine) -> Self {
        let mut args = line.args();
        let number = args.next().and_then(|raw| raw.parse::<u64>().ok());
        Self {
            number: number.filter(|n| *n != 0),
            message_id: args.next().and_then(|raw| MessageId::parse(raw).ok()),
        }
    }
}

/// A blocking NNTP client.
///
/// Holds the session state that commands depend on — capabilities, whether the session is
/// authenticated, which group is selected, which overview command works — so callers do
/// not have to track it themselves. Nothing here is `Send`-bound or thread-aware: run one
/// client per thread (see ADR-0003).
#[derive(Debug)]
pub struct Client<S> {
    connection: Connection<S>,
    greeting: Greeting,
    capabilities: Capabilities,
    encrypted: bool,
    authenticated: bool,
    group: Option<GroupSummary>,
    overview_fmt: Option<OverviewFmt>,
    overview_style: OverviewStyle,
}

impl<S: Read + Write> Client<S> {
    /// Wraps a stream and reads the server's greeting.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::Server`] if the greeting is a refusal (`400`, `502`), and
    /// the errors of [`Connection::read_status`] if it cannot be read.
    pub fn new(stream: S) -> Result<Self> {
        Self::with_options(stream, ClientOptions::default())
    }

    /// Wraps a stream with explicit options and reads the greeting.
    ///
    /// # Errors
    ///
    /// As [`Self::new`].
    pub fn with_options(stream: S, options: ClientOptions) -> Result<Self> {
        let mut connection = Connection::with_limits(stream, options.limits);
        let line = connection.read_status()?;

        if line.code.is_error() {
            return Err(ClientError::Server {
                command: "greeting",
                code: line.code,
                text: line.text,
            });
        }

        let greeting = Greeting {
            code: line.code,
            posting_allowed: line.code == codes::GREETING_POSTING_ALLOWED,
            text: line.text,
        };
        tracing::debug!(code = %greeting.code, text = %greeting.text, "connected");

        Ok(Self {
            connection,
            greeting,
            capabilities: Capabilities::new(),
            encrypted: options.encrypted,
            authenticated: false,
            group: None,
            overview_fmt: None,
            overview_style: OverviewStyle::Unknown,
        })
    }

    /// Rebuilds a client around an existing connection, without reading a greeting.
    ///
    /// Used by the `STARTTLS` upgrade: RFC 4642 §2.2 says the server does *not* repeat its
    /// greeting after the handshake, so a client that tried to read one would block until
    /// its read timeout. The session state is deliberately reset — the same section
    /// requires the client to discard everything it learned before the handshake,
    /// including the capability list, because that list was delivered in the clear and
    /// could have been tampered with.
    pub fn from_parts(connection: Connection<S>, greeting: Greeting, encrypted: bool) -> Self {
        Self {
            connection,
            greeting,
            capabilities: Capabilities::new(),
            encrypted,
            authenticated: false,
            group: None,
            overview_fmt: None,
            overview_style: OverviewStyle::Unknown,
        }
    }

    /// The greeting received on connection.
    pub fn greeting(&self) -> &Greeting {
        &self.greeting
    }

    /// Whether the transport is encrypted.
    pub fn is_encrypted(&self) -> bool {
        self.encrypted
    }

    /// Records that the transport is now encrypted, after a `STARTTLS` upgrade.
    pub fn set_encrypted(&mut self, encrypted: bool) {
        self.encrypted = encrypted;
    }

    /// Whether this session has authenticated successfully.
    pub fn is_authenticated(&self) -> bool {
        self.authenticated
    }

    /// The capabilities last reported, empty until [`Self::handshake`] or
    /// [`Self::refresh_capabilities`] runs.
    pub fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    /// The currently selected group, if any.
    pub fn selected_group(&self) -> Option<&GroupSummary> {
        self.group.as_ref()
    }

    /// Mutable access to the framing layer, for transport-level work.
    pub fn connection_mut(&mut self) -> &mut Connection<S> {
        &mut self.connection
    }

    /// Unwraps the client, returning the framed connection.
    pub fn into_connection(self) -> Connection<S> {
        self.connection
    }

    /// Runs the opening negotiation: read capabilities, enter reader mode if the server
    /// is in transit mode, then re-read capabilities if that changed anything.
    ///
    /// A server that does not implement `CAPABILITIES` is not an error: the client falls
    /// back to the RFC 2980 command set, which is what those servers speak.
    ///
    /// # Errors
    ///
    /// Returns an error only if the connection fails or `MODE READER` is refused with
    /// something other than "not supported".
    pub fn handshake(&mut self) -> Result<()> {
        self.refresh_capabilities()?;

        if self.capabilities.needs_mode_reader() || self.capabilities.is_empty() {
            self.enter_reader_mode()?;
            // Reader mode usually changes the advertised set, so ask again.
            self.refresh_capabilities()?;
        }

        Ok(())
    }

    /// Issues `CAPABILITIES` and stores the result.
    ///
    /// # Errors
    ///
    /// Connection failures only. A server that rejects the command leaves the stored
    /// capabilities empty.
    pub fn refresh_capabilities(&mut self) -> Result<&Capabilities> {
        let line = self.connection.command(&Command::Capabilities(None))?;

        self.capabilities = if line.code == codes::CAPABILITIES_FOLLOW {
            Capabilities::parse(&self.connection.read_block()?)
        } else if line.code.is_error() {
            tracing::debug!(
                code = %line.code,
                "server does not support CAPABILITIES; assuming RFC 2980 command set"
            );
            Capabilities::new()
        } else {
            return Err(ClientError::UnexpectedResponse {
                command: "CAPABILITIES",
                expected: "101",
                code: line.code,
                text: line.text,
            });
        };

        Ok(&self.capabilities)
    }

    /// Issues `MODE READER`.
    ///
    /// Servers answer `200`/`201` when they switch, and `500` when they were already a
    /// reader server and have no such command. Both are success as far as a reader is
    /// concerned.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::Server`] if the server refuses for any other reason.
    pub fn enter_reader_mode(&mut self) -> Result<()> {
        let line = self.connection.command(&Command::ModeReader)?;

        match line.code {
            codes::GREETING_POSTING_ALLOWED => {
                self.greeting.posting_allowed = true;
                Ok(())
            }
            codes::GREETING_NO_POSTING => {
                self.greeting.posting_allowed = false;
                Ok(())
            }
            codes::UNKNOWN_COMMAND | codes::SYNTAX_ERROR | codes::FEATURE_NOT_SUPPORTED => {
                tracing::debug!("server has no MODE READER; it is already a reader server");
                Ok(())
            }
            _ if line.code.is_error() => Err(ClientError::from_status("MODE READER", &line)),
            _ => Ok(()),
        }
    }

    /// Authenticates with `AUTHINFO USER`/`PASS` (RFC 4643).
    ///
    /// Credentials are refused on an unencrypted connection unless `allow_plaintext` is
    /// set. This is a deliberate obstacle: `AUTHINFO PASS` sends the password as clear
    /// text, and a reader that does it silently trains people to leak passwords.
    ///
    /// A server that accepts the username outright (`281`) does not get a password.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::PlaintextAuthenticationRefused`] if the link is
    /// unencrypted and plaintext was not allowed, and
    /// [`ClientError::AuthenticationRejected`] if the server refuses the credentials.
    pub fn authenticate(
        &mut self,
        username: &str,
        password: Option<&str>,
        allow_plaintext: bool,
    ) -> Result<()> {
        if !self.encrypted && !allow_plaintext {
            return Err(ClientError::PlaintextAuthenticationRefused);
        }

        let line = self
            .connection
            .command(&Command::AuthInfoUser(username.to_owned()))?;

        match line.code {
            codes::AUTH_ACCEPTED => {
                self.authenticated = true;
                tracing::debug!("authenticated with username alone");
                return Ok(());
            }
            codes::AUTH_PASSWORD_REQUIRED => {}
            _ => return Err(ClientError::from_status("AUTHINFO USER", &line)),
        }

        let password = password.ok_or_else(|| ClientError::AuthenticationRejected {
            text: "the server asked for a password and none was supplied".to_owned(),
        })?;

        let line = self
            .connection
            .command(&Command::AuthInfoPass(password.to_owned()))?;

        if line.code != codes::AUTH_ACCEPTED {
            return Err(ClientError::from_status("AUTHINFO PASS", &line));
        }

        self.authenticated = true;
        tracing::debug!("authenticated");

        // RFC 4643 §2.1: the capability list may change once authenticated.
        let _ = self.refresh_capabilities();
        Ok(())
    }

    /// Selects a group with `GROUP`.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::NoSuchGroup`] if the server does not carry it, and
    /// [`ClientError::AuthenticationRequired`] if it wants credentials first.
    pub fn select_group(&mut self, group: &GroupName) -> Result<GroupSummary> {
        let line = self.connection.command(&Command::Group(group.clone()))?;

        if line.code != codes::GROUP_SELECTED {
            // The response is not required to name the group and servers do not, so the
            // requested name is always substituted -- never scraped out of the text.
            // Reading the first word of INN's `411 No such newsgroup` gave "No".
            return Err(match ClientError::from_status("GROUP", &line) {
                ClientError::NoSuchGroup { text, .. } => ClientError::NoSuchGroup {
                    group: group.to_string(),
                    text,
                },
                other => other,
            });
        }

        let summary = GroupSummary::parse(&line, Some(group))?;
        tracing::debug!(
            group = %summary.name,
            low = summary.low,
            high = summary.high,
            estimated = summary.estimated_count,
            "group selected"
        );
        self.group = Some(summary.clone());
        Ok(summary)
    }

    /// Lists groups with watermarks and posting status (`LIST ACTIVE`).
    ///
    /// The response is large — a full feed is several megabytes — so callers wanting
    /// progress should use [`Self::list_groups_streaming`].
    ///
    /// # Errors
    ///
    /// Connection failures, or a server refusal.
    pub fn list_groups(&mut self, pattern: Option<&Wildmat>) -> Result<ListResult<ActiveEntry>> {
        let mut entries = Vec::new();
        let mut skipped = Vec::new();

        self.list_groups_streaming(pattern, |entry| match entry {
            Ok(entry) => entries.push(entry),
            Err(line) => skipped.push(line),
        })?;

        Ok(ListResult { entries, skipped })
    }

    /// Lists groups, passing each entry to `on_entry` as it arrives.
    ///
    /// Lines that do not parse are passed as `Err(raw_line)` rather than dropped, so a
    /// caller can log them.
    ///
    /// # Errors
    ///
    /// Connection failures, or a server refusal.
    pub fn list_groups_streaming(
        &mut self,
        pattern: Option<&Wildmat>,
        mut on_entry: impl FnMut(core::result::Result<ActiveEntry, String>),
    ) -> Result<()> {
        let command = Command::List(ListKeyword::Active(pattern.cloned()));
        let line = self.connection.command(&command)?;
        Self::require_block("LIST ACTIVE", &line, codes::INFORMATION_FOLLOWS)?;

        self.connection.read_block_streaming(|raw| {
            on_entry(
                ActiveEntry::parse(raw).map_err(|_| String::from_utf8_lossy(raw).into_owned()),
            );
        })?;

        Ok(())
    }

    /// Lists group descriptions (`LIST NEWSGROUPS`).
    ///
    /// # Errors
    ///
    /// Connection failures, or a server refusal.
    pub fn list_group_descriptions(
        &mut self,
        pattern: Option<&Wildmat>,
    ) -> Result<ListResult<NewsgroupEntry>> {
        let command = Command::List(ListKeyword::Newsgroups(pattern.cloned()));
        let line = self.connection.command(&command)?;
        Self::require_block("LIST NEWSGROUPS", &line, codes::INFORMATION_FOLLOWS)?;

        let mut entries = Vec::new();
        let mut skipped = Vec::new();
        self.connection
            .read_block_streaming(|raw| match NewsgroupEntry::parse(raw) {
                Ok(entry) => entries.push(entry),
                Err(_) => skipped.push(String::from_utf8_lossy(raw).into_owned()),
            })?;

        Ok(ListResult { entries, skipped })
    }

    /// The overview field layout, from `LIST OVERVIEW.FMT`, cached for the session.
    ///
    /// Falls back to [`OverviewFmt::standard`] when the server will not say, which is
    /// safe: the first seven fields are fixed by RFC 3977 §8.3 and the rest are then
    /// reported under positional names.
    ///
    /// # Errors
    ///
    /// Connection failures only.
    pub fn overview_format(&mut self) -> Result<OverviewFmt> {
        if let Some(fmt) = &self.overview_fmt {
            return Ok(fmt.clone());
        }

        let line = self
            .connection
            .command(&Command::List(ListKeyword::OverviewFmt))?;

        let fmt = if line.code == codes::INFORMATION_FOLLOWS {
            OverviewFmt::parse(&self.connection.read_block()?)
        } else {
            tracing::debug!(code = %line.code, "no LIST OVERVIEW.FMT; assuming the standard layout");
            OverviewFmt::standard()
        };

        if !fmt.is_standard_prefix() {
            tracing::warn!(
                "server's OVERVIEW.FMT does not begin with the seven RFC 3977 fields; \
                 fields will be mapped by name"
            );
        }

        self.overview_fmt = Some(fmt.clone());
        Ok(fmt)
    }

    /// Fetches overview records for a range of article numbers in the selected group.
    ///
    /// Uses `OVER` where available and falls back to `XOVER`, remembering which worked.
    /// The server may return fewer records than the range spans: expiry and cancellation
    /// leave gaps.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::NoGroupSelected`] if no group is selected, and
    /// [`ClientError::CommandNotSupported`] if the server has neither `OVER` nor `XOVER`.
    pub fn overview(&mut self, range: Range) -> Result<ListResult<OverviewRecord>> {
        if self.group.is_none() {
            return Err(ClientError::NoGroupSelected { command: "OVER" });
        }

        let fmt = self.overview_format()?;
        let mut entries = Vec::new();
        let mut skipped = Vec::new();

        self.overview_streaming(range, &fmt, |record| match record {
            Ok(record) => entries.push(record),
            Err(line) => skipped.push(line),
        })?;

        Ok(ListResult { entries, skipped })
    }

    /// Fetches overview records, passing each to `on_record` as it arrives.
    ///
    /// # Errors
    ///
    /// As [`Self::overview`].
    pub fn overview_streaming(
        &mut self,
        range: Range,
        fmt: &OverviewFmt,
        mut on_record: impl FnMut(core::result::Result<OverviewRecord, String>),
    ) -> Result<()> {
        let style = self.resolve_overview_style();
        let (command, name) = match style {
            OverviewStyle::XOver => (Command::XOver(range), "XOVER"),
            OverviewStyle::Unavailable => {
                return Err(ClientError::CommandNotSupported { command: "OVER" });
            }
            // An unknown style is tried as OVER; the fallback below handles a refusal.
            OverviewStyle::Over | OverviewStyle::Unknown => {
                (Command::Over(RangeOrId::Range(range)), "OVER")
            }
        };

        let line = self.connection.command(&command)?;

        if line.code == codes::OVERVIEW_FOLLOWS {
            self.overview_style = match style {
                OverviewStyle::XOver => OverviewStyle::XOver,
                _ => OverviewStyle::Over,
            };
            self.connection.read_block_streaming(|raw| {
                on_record(
                    OverviewRecord::parse(raw, fmt)
                        .map_err(|_| String::from_utf8_lossy(raw).into_owned()),
                );
            })?;
            return Ok(());
        }

        // Only 500 ("command not recognised") and 503 ("feature not supported") say
        // anything about the command. A 501 is a complaint about *these arguments* —
        // refusing an open-ended range is the common case — so it must leave the
        // remembered style alone, or one awkward request would disable overview for the
        // rest of the session.
        let verb_unsupported = matches!(
            line.code,
            codes::UNKNOWN_COMMAND | codes::FEATURE_NOT_SUPPORTED
        );

        if verb_unsupported && name == "OVER" {
            tracing::debug!("server does not implement OVER; falling back to XOVER");
            self.overview_style = OverviewStyle::XOver;
            return self.overview_streaming(range, fmt, on_record);
        }
        if verb_unsupported {
            tracing::warn!("server implements neither OVER nor XOVER");
            self.overview_style = OverviewStyle::Unavailable;
        }

        Err(ClientError::from_status(name, &line))
    }

    /// Fetches a whole article with `ARTICLE`.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::NoSuchArticle`] if it is not available, and
    /// [`ClientError::NoGroupSelected`] if a number or the current article was requested
    /// without a group.
    pub fn article(&mut self, spec: ArticleSpec) -> Result<Article> {
        let (id, block) =
            self.fetch_block("ARTICLE", Command::Article(spec), codes::ARTICLE_FOLLOWS)?;
        let mut article = Article::from_block(&block);
        article.number = id.number;
        Ok(article)
    }

    /// Fetches only the headers, with `HEAD`.
    ///
    /// # Errors
    ///
    /// As [`Self::article`].
    pub fn head(&mut self, spec: ArticleSpec) -> Result<Article> {
        let (id, block) = self.fetch_block("HEAD", Command::Head(spec), codes::HEAD_FOLLOWS)?;
        let mut article = Article::from_head_block(&block);
        article.number = id.number;
        Ok(article)
    }

    /// Fetches only the body, with `BODY`.
    ///
    /// # Errors
    ///
    /// As [`Self::article`].
    pub fn body(&mut self, spec: ArticleSpec) -> Result<DataBlock> {
        let (_, block) = self.fetch_block("BODY", Command::Body(spec), codes::BODY_FOLLOWS)?;
        Ok(block)
    }

    /// Checks whether an article exists, with `STAT`, without transferring it.
    ///
    /// # Errors
    ///
    /// As [`Self::article`].
    pub fn stat(&mut self, spec: ArticleSpec) -> Result<ArticleId> {
        let needs_group = spec.needs_selected_group();
        if needs_group && self.group.is_none() {
            return Err(ClientError::NoGroupSelected { command: "STAT" });
        }

        let line = self.connection.command(&Command::Stat(spec))?;
        if line.code != codes::ARTICLE_EXISTS {
            return Err(ClientError::from_status("STAT", &line));
        }
        Ok(ArticleId::parse(&line))
    }

    /// Asks the server for its clock with `DATE`.
    ///
    /// Useful for `NEWGROUPS` and `NEWNEWS`, which are relative to the *server's* idea of
    /// the time, and as a cheap liveness probe.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::Proto`] if the timestamp is not the fourteen digits
    /// RFC 3977 §7.1 requires.
    pub fn server_date(&mut self) -> Result<DateTime<Utc>> {
        let line = self.connection.command(&Command::Date)?;
        if line.code != codes::SERVER_DATE {
            return Err(ClientError::from_status("DATE", &line));
        }
        let raw = line.arg(0, "DATE response", "timestamp")?;
        Ok(date::parse_server_date(raw)?)
    }

    /// Fetches the server's help text.
    ///
    /// # Errors
    ///
    /// Connection failures, or a server refusal.
    pub fn help(&mut self) -> Result<String> {
        let line = self.connection.command(&Command::Help)?;
        if line.code != codes::HELP_TEXT_FOLLOWS {
            return Err(ClientError::from_status("HELP", &line));
        }
        Ok(self.connection.read_block()?.to_text())
    }

    /// Whether this connection can post at all, and why not if it cannot.
    ///
    /// `Ok(())` is not a promise the server will take any particular article — only that
    /// offering one is not pointless. The greeting's `200`/`201` is the primary signal
    /// (RFC 3977 §5.1.1) and `CAPABILITIES` the secondary; a server that says neither is
    /// given the benefit of the doubt, because a reader that refuses to try is worse than
    /// one that relays a refusal.
    ///
    /// # Errors
    ///
    /// [`ClientError::PostingNotAllowed`] with the reason.
    pub fn check_can_post(&self) -> Result<()> {
        if !self.greeting.posting_allowed {
            return Err(ClientError::PostingNotAllowed {
                reason: "the server greeted with 201, posting prohibited".to_owned(),
            });
        }

        // An empty capability list means CAPABILITIES was never answered, which is not the
        // same as being answered without POST.
        if !self.capabilities.is_empty() && !self.capabilities.has_post() {
            return Err(ClientError::PostingNotAllowed {
                reason: "the server does not advertise POST".to_owned(),
            });
        }

        Ok(())
    }

    /// Posts an article (RFC 3977 §6.3.1).
    ///
    /// The two-step exchange: `POST`, then the article as a data block only once the
    /// server has answered `340`. Sending the article with the command would mean offering
    /// it to a server that has just said it will not take one.
    ///
    /// The draft is validated before anything is written, so a missing `Newsgroups` is a
    /// message in front of the user rather than a `441` after a round trip; and the lines
    /// are dot-stuffed on the way out, so a body line beginning with `.` survives.
    ///
    /// Returns the server's own success text, which some servers use to report the
    /// message-id they assigned.
    ///
    /// # Errors
    ///
    /// [`ClientError::PostingNotAllowed`] before anything is sent;
    /// [`ClientError::Proto`] wrapping [`nntp_proto::ProtoError::UnpostableDraft`] if the
    /// draft is incomplete; [`ClientError::PostingRejected`] carrying the server's own
    /// words if it refuses the article; and connection failures.
    pub fn post(&mut self, draft: &Draft) -> Result<String> {
        self.check_can_post()?;

        // Encoded first: a draft that cannot be encoded must not cost a POST command, and
        // a server that counts refused offers should not be given one for our mistake.
        let lines = draft.to_lines()?;

        let line = self.connection.command(&Command::Post)?;
        if line.code != codes::SEND_ARTICLE {
            return Err(match line.code {
                codes::POSTING_NOT_PERMITTED => ClientError::PostingNotAllowed {
                    reason: line.text.clone(),
                },
                _ => ClientError::from_status("POST", &line),
            });
        }

        self.connection.send_block(&lines)?;

        let line = self.connection.read_status()?;
        if line.code == codes::ARTICLE_POSTED {
            tracing::info!(text = %line.text, "article posted");
            return Ok(line.text);
        }

        Err(match line.code {
            codes::POSTING_NOT_PERMITTED | codes::POSTING_FAILED => ClientError::PostingRejected {
                code: line.code,
                text: line.text,
            },
            _ => ClientError::from_status("POST", &line),
        })
    }

    /// Sends `QUIT` and consumes the client.
    ///
    /// # Errors
    ///
    /// Connection failures. A server that closes the socket without answering is not
    /// treated as an error: the session was ending anyway.
    pub fn quit(mut self) -> Result<()> {
        match self.connection.command(&Command::Quit) {
            Ok(line) => {
                tracing::debug!(code = %line.code, "disconnected");
                Ok(())
            }
            Err(ClientError::ConnectionClosed(_)) => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Sends a command that returns a status line naming an article, then its block.
    fn fetch_block(
        &mut self,
        name: &'static str,
        command: Command,
        expected: ResponseCode,
    ) -> Result<(ArticleId, DataBlock)> {
        if let Command::Article(spec) | Command::Head(spec) | Command::Body(spec) = &command
            && spec.needs_selected_group()
            && self.group.is_none()
        {
            return Err(ClientError::NoGroupSelected { command: name });
        }

        let line = self.connection.command(&command)?;
        if line.code != expected {
            return Err(ClientError::from_status(name, &line));
        }

        let id = ArticleId::parse(&line);
        let block = self.connection.read_block()?;
        Ok((id, block))
    }

    /// Decides which overview command to use, from the advertised capabilities.
    fn resolve_overview_style(&self) -> OverviewStyle {
        match self.overview_style {
            OverviewStyle::Unknown if self.capabilities.has_over() => OverviewStyle::Over,
            // A non-empty capability list that omits OVER is a server that means it.
            OverviewStyle::Unknown if !self.capabilities.is_empty() => OverviewStyle::XOver,
            other => other,
        }
    }

    fn require_block(name: &'static str, line: &StatusLine, expected: ResponseCode) -> Result<()> {
        if line.code == expected {
            return Ok(());
        }
        Err(ClientError::from_status(name, line))
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    /// A stream that replays a canned server script and records what the client sent.
    ///
    /// The client is deterministic, so a pre-loaded script is enough: if the client sends
    /// a different command than expected, the assertion on `written` catches it.
    #[derive(Debug)]
    struct Script {
        input: Cursor<Vec<u8>>,
        written: Vec<u8>,
    }

    impl Read for Script {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.input.read(buf)
        }
    }

    impl Write for Script {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.written.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Builds a client over a scripted stream, consuming the greeting.
    fn client(script: &str) -> Client<Script> {
        Client::new(Script {
            input: Cursor::new(script.as_bytes().to_vec()),
            written: Vec::new(),
        })
        .expect("greeting")
    }

    /// What the client sent, as text.
    fn sent(client: &mut Client<Script>) -> String {
        String::from_utf8_lossy(&client.connection_mut().get_mut().written).into_owned()
    }

    const GREETING: &str = "200 news.example.org ready\r\n";

    const CAPS_MODERN: &str = "101 capabilities\r\n\
        VERSION 2\r\n\
        READER\r\n\
        OVER MSGID\r\n\
        HDR\r\n\
        LIST ACTIVE NEWSGROUPS OVERVIEW.FMT\r\n\
        AUTHINFO USER\r\n\
        .\r\n";

    const OVERVIEW_FMT: &str = "215 order\r\n\
        Subject:\r\nFrom:\r\nDate:\r\nMessage-ID:\r\nReferences:\r\n:bytes\r\n:lines\r\n\
        .\r\n";

    fn group(name: &str) -> GroupName {
        GroupName::parse(name).unwrap()
    }

    #[test]
    fn reads_the_greeting_and_notes_whether_posting_is_allowed() {
        let posting = client("200 ready, posting allowed\r\n");
        assert!(posting.greeting().posting_allowed);
        assert_eq!(posting.greeting().text, "ready, posting allowed");

        let no_posting = client("201 ready, no posting\r\n");
        assert!(!no_posting.greeting().posting_allowed);
    }

    #[test]
    fn a_refusing_greeting_is_an_error() {
        let stream = Script {
            input: Cursor::new(b"502 access denied\r\n".to_vec()),
            written: Vec::new(),
        };
        let error = Client::new(stream).unwrap_err();
        assert!(matches!(
            error,
            ClientError::Server { code, .. } if code.as_u16() == 502
        ));
    }

    #[test]
    fn handshake_skips_mode_reader_when_the_server_already_offers_reader() {
        let mut client = client(&format!("{GREETING}{CAPS_MODERN}"));
        client.handshake().unwrap();

        assert_eq!(sent(&mut client), "CAPABILITIES\r\n");
        assert!(client.capabilities().has_reader());
        assert!(client.capabilities().has_over());
    }

    #[test]
    fn handshake_enters_reader_mode_on_a_transit_server() {
        // A transit server advertises MODE-READER without READER; after the switch it
        // advertises a different set, so capabilities are read again.
        let script = format!(
            "{GREETING}\
             101 capabilities\r\nVERSION 2\r\nIHAVE\r\nMODE-READER\r\n.\r\n\
             200 reader mode, posting allowed\r\n\
             {CAPS_MODERN}"
        );
        let mut client = client(&script);
        client.handshake().unwrap();

        assert_eq!(
            sent(&mut client),
            "CAPABILITIES\r\nMODE READER\r\nCAPABILITIES\r\n"
        );
        assert!(client.capabilities().has_reader());
        assert!(client.greeting().posting_allowed);
    }

    #[test]
    fn handshake_copes_with_a_server_that_has_no_capabilities_command() {
        // Pre-RFC-3977 servers answer 500. That is not an error; it means the RFC 2980
        // command set, which the client discovers by trying.
        let script = format!(
            "{GREETING}\
             500 what?\r\n\
             500 what?\r\n\
             500 what?\r\n"
        );
        let mut client = client(&script);
        client.handshake().unwrap();

        assert_eq!(
            sent(&mut client),
            "CAPABILITIES\r\nMODE READER\r\nCAPABILITIES\r\n"
        );
        assert!(client.capabilities().is_empty());
    }

    #[test]
    fn mode_reader_failure_that_is_not_about_support_is_reported() {
        let mut client = client(&format!("{GREETING}400 go away\r\n"));
        let error = client.enter_reader_mode().unwrap_err();
        assert!(matches!(error, ClientError::Server { .. }));
        assert!(error.is_transient());
    }

    #[test]
    fn selects_a_group() {
        let mut client = client(&format!("{GREETING}211 6 4237 4242 comp.lang.rust\r\n"));
        let summary = client.select_group(&group("comp.lang.rust")).unwrap();

        assert_eq!(sent(&mut client), "GROUP comp.lang.rust\r\n");
        assert_eq!(summary.range(), Some((4237, 4242)));
        assert_eq!(
            client.selected_group().map(|g| g.name.as_str()),
            Some("comp.lang.rust")
        );
    }

    #[test]
    fn a_missing_group_is_named_from_the_request_not_the_response() {
        // INN answers `411 No such newsgroup` with no group name in it. The reader has to
        // report the group the *user* asked for, or the message is useless.
        for reply in [
            "411 No such newsgroup",        // INN 2.8.0, observed
            "411 no.such.group is invalid", // some servers do name it
            "411",                          // and some say nothing at all
        ] {
            let mut client = client(&format!("{GREETING}{reply}\r\n"));
            let error = client.select_group(&group("no.such.group")).unwrap_err();

            match &error {
                ClientError::NoSuchGroup { group, .. } => assert_eq!(
                    group, "no.such.group",
                    "reply {reply:?} produced the wrong group name"
                ),
                other => panic!("expected NoSuchGroup for {reply:?}, got {other:?}"),
            }
            assert!(
                error.to_string().contains("no.such.group"),
                "reply {reply:?} gave an unhelpful message: {error}"
            );
            assert!(client.selected_group().is_none());
        }
    }

    #[test]
    fn a_411_that_means_something_more_specific_keeps_the_explanation() {
        let mut client = client(&format!("{GREETING}411 Access denied to that group\r\n"));
        let error = client.select_group(&group("secret.group")).unwrap_err();

        let message = error.to_string();
        assert!(message.contains("secret.group"), "{message}");
        assert!(message.contains("Access denied"), "{message}");
    }

    #[test]
    fn lists_groups_and_keeps_the_lines_it_could_not_parse() {
        let script = format!(
            "{GREETING}\
             215 groups\r\n\
             misc.test 3002322 3000234 y\r\n\
             this line is broken\r\n\
             comp.lang.rust 4242 4237 y\r\n\
             .\r\n"
        );
        let mut client = client(&script);
        let result = client.list_groups(None).unwrap();

        assert_eq!(sent(&mut client), "LIST ACTIVE\r\n");
        assert_eq!(result.len(), 2);
        assert_eq!(result.skipped, ["this line is broken"]);
        assert_eq!(result.entries[0].high, 3_002_322);
    }

    #[test]
    fn lists_groups_with_a_wildmat() {
        let mut client = client(&format!("{GREETING}215 groups\r\n.\r\n"));
        let pattern = Wildmat::hierarchy("comp").unwrap();
        client.list_groups(Some(&pattern)).unwrap();
        assert_eq!(sent(&mut client), "LIST ACTIVE comp.*\r\n");
    }

    #[test]
    fn lists_group_descriptions() {
        let script = format!("{GREETING}215 descriptions\r\nmisc.test\tFor testing\r\n.\r\n");
        let mut client = client(&script);
        let result = client.list_group_descriptions(None).unwrap();

        assert_eq!(sent(&mut client), "LIST NEWSGROUPS\r\n");
        assert_eq!(result.entries[0].description, "For testing");
    }

    #[test]
    fn uses_over_when_the_server_advertises_it() {
        let script = format!(
            "{GREETING}{CAPS_MODERN}\
             211 2 1 2 misc.test\r\n\
             {OVERVIEW_FMT}\
             224 overview\r\n\
             1\tfirst\ta@x\t\t<1@x>\t\t10\t1\r\n\
             2\tsecond\tb@x\t\t<2@x>\t\t20\t2\r\n\
             .\r\n"
        );
        let mut client = client(&script);
        client.handshake().unwrap();
        client.select_group(&group("misc.test")).unwrap();

        let result = client.overview(Range::between(1, 2)).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result.entries[1].subject, "second");
        assert!(sent(&mut client).contains("OVER 1-2\r\n"));
        assert!(!sent(&mut client).contains("XOVER"));
    }

    #[test]
    fn uses_xover_when_the_capability_list_omits_over() {
        // A non-empty capability list without OVER means the server means it, so there is
        // no point spending a round trip discovering that.
        let script = format!(
            "{GREETING}\
             101 capabilities\r\nVERSION 2\r\nREADER\r\n.\r\n\
             211 1 1 1 misc.test\r\n\
             {OVERVIEW_FMT}\
             224 overview\r\n1\tonly\ta@x\t\t<1@x>\t\t10\t1\r\n.\r\n"
        );
        let mut client = client(&script);
        client.handshake().unwrap();
        client.select_group(&group("misc.test")).unwrap();

        let result = client.overview(Range::Single(1)).unwrap();
        assert_eq!(result.len(), 1);
        let sent = sent(&mut client);
        assert!(sent.contains("XOVER 1\r\n"), "sent: {sent:?}");
        assert!(!sent.contains("\nOVER"), "sent: {sent:?}");
    }

    #[test]
    fn falls_back_from_over_to_xover_and_remembers() {
        // No CAPABILITIES at all, so the client tries OVER, is refused, and switches.
        let script = format!(
            "{GREETING}\
             500 no capabilities\r\n\
             500 no mode reader\r\n\
             500 no capabilities\r\n\
             211 2 1 2 misc.test\r\n\
             500 no overview.fmt\r\n\
             500 unknown command\r\n\
             224 overview\r\n1\tfirst\ta@x\t\t<1@x>\t\t10\t1\r\n.\r\n\
             224 overview\r\n2\tsecond\tb@x\t\t<2@x>\t\t20\t2\r\n.\r\n"
        );
        let mut client = client(&script);
        client.handshake().unwrap();
        client.select_group(&group("misc.test")).unwrap();

        assert_eq!(client.overview(Range::Single(1)).unwrap().len(), 1);
        assert_eq!(client.overview(Range::Single(2)).unwrap().len(), 1);

        let sent = sent(&mut client);
        // OVER was tried once; after the refusal only XOVER is used.
        assert_eq!(sent.matches("OVER 1\r\n").count(), 2, "sent: {sent:?}");
        assert_eq!(sent.matches("XOVER").count(), 2, "sent: {sent:?}");
    }

    #[test]
    fn overview_without_a_selected_group_does_not_reach_the_socket() {
        let mut client = client(GREETING);
        let error = client.overview(Range::Single(1)).unwrap_err();
        assert!(matches!(error, ClientError::NoGroupSelected { .. }));
        assert!(sent(&mut client).is_empty());
    }

    #[test]
    fn the_overview_format_is_fetched_once_and_cached() {
        let script = format!("{GREETING}{OVERVIEW_FMT}");
        let mut client = client(&script);

        let first = client.overview_format().unwrap();
        let second = client.overview_format().unwrap();
        assert_eq!(first, second);
        assert_eq!(sent(&mut client), "LIST OVERVIEW.FMT\r\n");
    }

    #[test]
    fn a_server_without_overview_fmt_gets_the_standard_layout() {
        let mut client = client(&format!("{GREETING}503 not supported\r\n"));
        assert_eq!(client.overview_format().unwrap(), OverviewFmt::standard());
    }

    #[test]
    fn fetches_an_article() {
        let script = format!(
            "{GREETING}\
             211 1 1 1 misc.test\r\n\
             220 1 <a@b> article follows\r\n\
             From: a@b\r\nSubject: hello\r\n\r\nbody line\r\n..dotted\r\n.\r\n"
        );
        let mut client = client(&script);
        client.select_group(&group("misc.test")).unwrap();

        let article = client.article(ArticleSpec::Number(1)).unwrap();
        assert_eq!(article.number, Some(1));
        assert_eq!(article.subject(), "hello");
        assert_eq!(article.body_text(), "body line\n.dotted");
        assert!(sent(&mut client).contains("ARTICLE 1\r\n"));
    }

    #[test]
    fn fetches_an_article_by_message_id_without_a_group() {
        let script = format!("{GREETING}220 0 <a@b>\r\nFrom: a@b\r\n\r\nbody\r\n.\r\n");
        let mut client = client(&script);
        let id = MessageId::parse("<a@b>").unwrap();
        let article = client.article(ArticleSpec::MessageId(id)).unwrap();

        // The server reported article number 0, meaning "not applicable".
        assert_eq!(article.number, None);
        assert_eq!(sent(&mut client), "ARTICLE <a@b>\r\n");
    }

    #[test]
    fn fetching_by_number_without_a_group_does_not_reach_the_socket() {
        let mut client = client(GREETING);
        let error = client.article(ArticleSpec::Number(1)).unwrap_err();
        assert!(matches!(
            error,
            ClientError::NoGroupSelected { command: "ARTICLE" }
        ));
        assert!(sent(&mut client).is_empty());
    }

    #[test]
    fn a_missing_article_is_reported_as_such() {
        let script = format!("{GREETING}211 1 1 1 misc.test\r\n423 no such article\r\n");
        let mut client = client(&script);
        client.select_group(&group("misc.test")).unwrap();

        let error = client.article(ArticleSpec::Number(999)).unwrap_err();
        assert!(matches!(error, ClientError::NoSuchArticle { .. }));
        // Recoverable: the connection is still usable.
        assert!(!error.is_connection_fatal());
    }

    #[test]
    fn fetches_head_and_body_separately() {
        let script = format!(
            "{GREETING}\
             221 0 <a@b>\r\nSubject: just headers\r\n.\r\n\
             222 0 <a@b>\r\nbody only\r\n.\r\n"
        );
        let mut client = client(&script);
        let id = MessageId::parse("<a@b>").unwrap();

        let head = client.head(ArticleSpec::MessageId(id.clone())).unwrap();
        assert_eq!(head.subject(), "just headers");
        assert!(!head.has_body());

        let body = client.body(ArticleSpec::MessageId(id)).unwrap();
        assert_eq!(body.to_text(), "body only");
    }

    #[test]
    fn stat_reports_the_article_identity() {
        let script = format!("{GREETING}223 42 <a@b> status\r\n");
        let mut client = client(&script);
        let id = MessageId::parse("<a@b>").unwrap();
        let found = client.stat(ArticleSpec::MessageId(id)).unwrap();

        assert_eq!(found.number, Some(42));
        assert_eq!(
            found.message_id.map(|m| m.to_string()),
            Some("<a@b>".to_owned())
        );
    }

    #[test]
    fn reads_the_server_clock() {
        let mut client = client(&format!("{GREETING}111 20260917080910\r\n"));
        let date = client.server_date().unwrap();
        assert_eq!(date.to_rfc3339(), "2026-09-17T08:09:10+00:00");
        assert_eq!(sent(&mut client), "DATE\r\n");
    }

    #[test]
    fn a_malformed_server_clock_is_a_protocol_error() {
        let mut client = client(&format!("{GREETING}111 not-a-date\r\n"));
        assert!(matches!(
            client.server_date().unwrap_err(),
            ClientError::Proto(_)
        ));
    }

    #[test]
    fn refuses_to_send_credentials_over_a_plaintext_link() {
        let mut client = client(GREETING);
        assert!(!client.is_encrypted());

        let error = client
            .authenticate("bob", Some("hunter2"), false)
            .unwrap_err();
        assert!(matches!(error, ClientError::PlaintextAuthenticationRefused));
        // Nothing was sent: the password never reached the socket.
        assert!(sent(&mut client).is_empty());
        assert!(!client.is_authenticated());
    }

    #[test]
    fn authenticates_with_username_and_password() {
        let script =
            format!("{GREETING}381 password required\r\n281 authenticated\r\n{CAPS_MODERN}");
        let mut client = client(&script);
        client.set_encrypted(true);
        client.authenticate("bob", Some("hunter2"), false).unwrap();

        assert!(client.is_authenticated());
        let sent = sent(&mut client);
        assert!(sent.starts_with("AUTHINFO USER bob\r\nAUTHINFO PASS hunter2\r\n"));
        // RFC 4643 §2.1: capabilities may change after authentication.
        assert!(sent.ends_with("CAPABILITIES\r\n"));
    }

    #[test]
    fn accepts_a_server_that_needs_no_password() {
        let mut client = client(&format!("{GREETING}281 welcome\r\n"));
        client.authenticate("bob", None, true).unwrap();
        assert!(client.is_authenticated());
        assert_eq!(sent(&mut client), "AUTHINFO USER bob\r\n");
    }

    #[test]
    fn reports_rejected_credentials() {
        let script = format!("{GREETING}381 password required\r\n481 bad password\r\n");
        let mut client = client(&script);
        let error = client.authenticate("bob", Some("wrong"), true).unwrap_err();

        assert!(matches!(error, ClientError::AuthenticationRejected { .. }));
        assert!(!client.is_authenticated());
        assert!(!error.is_connection_fatal());
    }

    #[test]
    fn reports_a_missing_password_without_guessing() {
        let mut client = client(&format!("{GREETING}381 password required\r\n"));
        let error = client.authenticate("bob", None, true).unwrap_err();
        assert!(matches!(error, ClientError::AuthenticationRejected { .. }));
    }

    #[test]
    fn a_command_needing_authentication_says_so() {
        let mut client = client(&format!("{GREETING}480 authentication required\r\n"));
        let error = client.select_group(&group("misc.test")).unwrap_err();
        assert!(error.needs_authentication());
        assert!(!error.is_connection_fatal());
    }

    #[test]
    fn quits_cleanly() {
        let session = client(&format!("{GREETING}205 closing\r\n"));
        session.quit().unwrap();

        // A server that drops the connection instead of answering QUIT is not an error:
        // the session was ending anyway.
        client(GREETING).quit().unwrap();
    }

    #[test]
    fn fetches_help_text() {
        let script = format!("{GREETING}100 help\r\nARTICLE\r\nGROUP\r\n.\r\n");
        let mut client = client(&script);
        assert_eq!(client.help().unwrap(), "ARTICLE\nGROUP");
    }
}
