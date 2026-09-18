//! Commands: the typed form of everything this client can send, and its encoding.
//!
//! Encoding goes through [`Command::to_wire`], which validates before it serialises.
//! That is the single place where untrusted text becomes a command line, so it is also
//! where CR and LF are refused: a group name or password containing CRLF would otherwise
//! let whoever supplied it append commands of their own choosing to the session.

use chrono::{DateTime, Utc};

use crate::group::GroupName;
use crate::headers::HeaderName;
use crate::spec::{ArticleSpec, Range, RangeOrId};
use crate::{ProtoError, Result};

/// The maximum length of a command line in octets, including the terminating CRLF
/// (RFC 3977 §3.1).
pub const MAX_COMMAND_LEN: usize = 512;

/// A wildmat pattern, used to narrow `LIST` output (RFC 3977 §4).
///
/// Validated so it is safe to send: printable US-ASCII only, no whitespace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wildmat(String);

impl Wildmat {
    /// The longest pattern that may be sent.
    ///
    /// Well inside the 512-octet command line of RFC 3977 §3.1, which also has to carry
    /// the command itself.
    pub const MAX_LEN: usize = 400;

    /// Validates and wraps a pattern.
    ///
    /// # Errors
    ///
    /// Returns [`ProtoError::InvalidWildmat`] if the pattern is empty, longer than
    /// [`Self::MAX_LEN`], or contains a byte outside printable US-ASCII.
    pub fn parse(pattern: &str) -> Result<Self> {
        if pattern.is_empty()
            || pattern.len() > Self::MAX_LEN
            || !pattern.bytes().all(|b| b.is_ascii_graphic())
        {
            return Err(ProtoError::InvalidWildmat(pattern.to_owned()));
        }
        Ok(Self(pattern.to_owned()))
    }

    /// Builds a pattern matching everything under a hierarchy: `comp` becomes `comp.*`.
    ///
    /// # Errors
    ///
    /// Returns [`ProtoError::InvalidWildmat`] if the resulting pattern is not valid.
    pub fn hierarchy(prefix: &str) -> Result<Self> {
        Self::parse(&format!("{prefix}.*"))
    }

    /// Builds a pattern matching any name containing `text`: `rust` becomes `*rust*`.
    ///
    /// This is the wildmat spelling of the substring search a reader's filter box does
    /// locally, so that the same typing can be handed to the server instead.
    ///
    /// # Errors
    ///
    /// Returns [`ProtoError::InvalidWildmat`] if the resulting pattern is not valid —
    /// which is what a space or a non-ASCII character in `text` produces, since neither
    /// can be sent in a wildmat.
    pub fn containing(text: &str) -> Result<Self> {
        Self::parse(&format!("*{text}*"))
    }

    /// Joins patterns into one, in the comma-separated form RFC 3977 §4.2 defines.
    ///
    /// Returns `None` for an empty list, because a wildmat that matches nothing cannot be
    /// spelled and "no patterns" means "do not narrow the response" everywhere it is used.
    ///
    /// # Errors
    ///
    /// Returns [`ProtoError::InvalidWildmat`] if the joined pattern exceeds the length a
    /// wildmat may have. Callers with a long list should send several commands rather than
    /// one — see [`Self::MAX_LEN`].
    pub fn join(patterns: &[Self]) -> Result<Option<Self>> {
        if patterns.is_empty() {
            return Ok(None);
        }
        let joined = patterns
            .iter()
            .map(Self::as_str)
            .collect::<Vec<_>>()
            .join(",");
        Self::parse(&joined).map(Some)
    }

    /// The pattern as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether `name` matches this pattern, by the rules of RFC 3977 §4.2.
    ///
    /// The pattern is a comma-separated list of sub-patterns, each optionally preceded by
    /// `!` to negate it, and **the last sub-pattern that matches decides**: `comp.*` keeps
    /// every `comp` group, `comp.*,!comp.os.*` keeps all but the `comp.os` ones, and
    /// `comp.*,!comp.os.*,comp.os.linux.*` puts that one hierarchy back. A name that no
    /// sub-pattern matches does not match.
    ///
    /// Within a sub-pattern, `*` matches any run of characters including none, `?` matches
    /// exactly one, and everything else matches itself. RFC 3977's grammar deliberately
    /// excludes `[`, `]` and `\` from a wildmat, so — unlike the original wildmat, and
    /// unlike some servers — this implements no character classes and no escaping, and
    /// treats those three characters as ordinary text.
    ///
    /// Matching is case-sensitive, as newsgroup names are.
    ///
    /// This is here for the two callers that must filter a group list themselves: a server
    /// that refuses a pattern on `LIST` and has to be filtered after the fact, and a fake
    /// server that would otherwise teach a client that patterns are ignored.
    pub fn matches(&self, name: &str) -> bool {
        let name: Vec<char> = name.chars().collect();
        let mut matched = false;

        for item in self.0.split(',') {
            let (negated, pattern) = match item.strip_prefix('!') {
                Some(rest) => (true, rest),
                None => (false, item),
            };
            if matches_one(&pattern.chars().collect::<Vec<_>>(), &name) {
                matched = !negated;
            }
        }

        matched
    }
}

/// Matches one `*`/`?` sub-pattern against a name.
///
/// Iterative with one backtracking point rather than recursive: `*` against a long name is
/// where a recursive matcher becomes exponential, and a pattern arriving from a
/// configuration file is not something to hand a stack overflow.
fn matches_one(pattern: &[char], name: &[char]) -> bool {
    let (mut p, mut n) = (0usize, 0usize);
    // Where to resume from if the current `*` turns out to have consumed too little.
    let mut star: Option<usize> = None;
    let mut resume = 0usize;

    while n < name.len() {
        match pattern.get(p) {
            Some('*') => {
                star = Some(p);
                resume = n;
                p += 1;
            }
            Some('?') => {
                p += 1;
                n += 1;
            }
            Some(character) if Some(character) == name.get(n) => {
                p += 1;
                n += 1;
            }
            // No match here: give the last `*` one more character and try again.
            _ => match star {
                Some(index) => {
                    p = index + 1;
                    resume += 1;
                    n = resume;
                }
                None => return false,
            },
        }
    }

    // Trailing stars may match nothing at all; anything else left over does not match.
    while matches!(pattern.get(p), Some('*')) {
        p += 1;
    }
    p == pattern.len()
}

/// Which `LIST` variant to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListKeyword {
    /// `LIST ACTIVE` — group names with watermarks and posting status.
    Active(Option<Wildmat>),
    /// `LIST ACTIVE.TIMES` — group creation times and creators.
    ActiveTimes(Option<Wildmat>),
    /// `LIST NEWSGROUPS` — group names with descriptions.
    Newsgroups(Option<Wildmat>),
    /// `LIST OVERVIEW.FMT` — the field order used by `OVER`.
    OverviewFmt,
    /// `LIST HEADERS` — which header names `HDR` accepts.
    Headers,
}

impl ListKeyword {
    fn keyword(&self) -> &'static str {
        match self {
            Self::Active(_) => "ACTIVE",
            Self::ActiveTimes(_) => "ACTIVE.TIMES",
            Self::Newsgroups(_) => "NEWSGROUPS",
            Self::OverviewFmt => "OVERVIEW.FMT",
            Self::Headers => "HEADERS",
        }
    }

    fn wildmat(&self) -> Option<&Wildmat> {
        match self {
            Self::Active(w) | Self::ActiveTimes(w) | Self::Newsgroups(w) => w.as_ref(),
            Self::OverviewFmt | Self::Headers => None,
        }
    }
}

/// Every command this crate can encode.
///
/// The set is deliberately limited to what a reader needs; transit commands (`IHAVE`,
/// `CHECK`, `TAKETHIS`) are out of scope. `POST` is here because following up is part of
/// reading a newsgroup; injecting other people's articles is not.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Command {
    /// `CAPABILITIES [keyword]`
    Capabilities(Option<String>),
    /// `MODE READER` — ask a server that defaults to transit mode to serve a reader.
    ModeReader,
    /// `QUIT`
    Quit,
    /// `STARTTLS` (RFC 4642)
    StartTls,
    /// `AUTHINFO USER username` (RFC 4643)
    AuthInfoUser(String),
    /// `AUTHINFO PASS password` (RFC 4643). Redacted by [`Command::to_log_string`].
    AuthInfoPass(String),
    /// `GROUP group`
    Group(GroupName),
    /// `LISTGROUP [group [range]]`
    ListGroup {
        /// The group to list, or the selected group if `None`.
        group: Option<GroupName>,
        /// Restrict to this range of article numbers.
        range: Option<Range>,
    },
    /// `LAST`
    Last,
    /// `NEXT`
    Next,
    /// `ARTICLE [message-id|number]`
    Article(ArticleSpec),
    /// `HEAD [message-id|number]`
    Head(ArticleSpec),
    /// `BODY [message-id|number]`
    Body(ArticleSpec),
    /// `STAT [message-id|number]`
    Stat(ArticleSpec),
    /// `DATE`
    Date,
    /// `HELP`
    Help,
    /// `NEWGROUPS yyyymmdd hhmmss GMT`
    NewGroups(DateTime<Utc>),
    /// `LIST ...`
    List(ListKeyword),
    /// `OVER [range|message-id]`
    Over(RangeOrId),
    /// `XOVER range` — the RFC 2980 predecessor of `OVER`, still the only option on some
    /// servers.
    XOver(Range),
    /// `HDR field [range|message-id]`
    Hdr {
        /// The header field to retrieve.
        field: HeaderName,
        /// Which articles to retrieve it for.
        target: RangeOrId,
    },
    /// `POST` — the first half of RFC 3977 §6.3.1's two-step exchange.
    ///
    /// The article itself is *not* part of the command: the server answers `340` first,
    /// and only then is the article sent as a data block. Encoding the two together would
    /// make it possible to send an article to a server that has just refused to take one.
    Post,
    /// `XHDR field [range|message-id]` — the RFC 2980 predecessor of `HDR`.
    XHdr {
        /// The header field to retrieve.
        field: HeaderName,
        /// Which articles to retrieve it for.
        target: RangeOrId,
    },
}

impl Command {
    /// The command verb, for error messages and logs.
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Capabilities(_) => "CAPABILITIES",
            Self::ModeReader => "MODE READER",
            Self::Quit => "QUIT",
            Self::StartTls => "STARTTLS",
            Self::AuthInfoUser(_) => "AUTHINFO USER",
            Self::AuthInfoPass(_) => "AUTHINFO PASS",
            Self::Group(_) => "GROUP",
            Self::ListGroup { .. } => "LISTGROUP",
            Self::Last => "LAST",
            Self::Next => "NEXT",
            Self::Article(_) => "ARTICLE",
            Self::Head(_) => "HEAD",
            Self::Body(_) => "BODY",
            Self::Stat(_) => "STAT",
            Self::Date => "DATE",
            Self::Help => "HELP",
            Self::NewGroups(_) => "NEWGROUPS",
            Self::List(_) => "LIST",
            Self::Over(_) => "OVER",
            Self::XOver(_) => "XOVER",
            Self::Hdr { .. } => "HDR",
            Self::XHdr { .. } => "XHDR",
            Self::Post => "POST",
        }
    }

    /// Whether a successful response to this command is followed by a multi-line data
    /// block.
    ///
    /// This cannot be decided from the status code alone: `211` introduces a block after
    /// `LISTGROUP` but not after `GROUP`.
    pub const fn expects_data_block(&self) -> bool {
        match self {
            Self::Capabilities(_)
            | Self::ListGroup { .. }
            | Self::Article(_)
            | Self::Head(_)
            | Self::Body(_)
            | Self::Help
            | Self::NewGroups(_)
            | Self::List(_)
            | Self::Over(_)
            | Self::XOver(_)
            | Self::Hdr { .. }
            | Self::XHdr { .. } => true,

            Self::ModeReader
            | Self::Quit
            | Self::StartTls
            | Self::AuthInfoUser(_)
            | Self::AuthInfoPass(_)
            | Self::Group(_)
            | Self::Last
            | Self::Next
            | Self::Stat(_)
            // `340` introduces nothing; the *client* sends the block next.
            | Self::Post
            | Self::Date => false,
        }
    }

    /// Whether the command carries a secret that must not reach a log file.
    pub const fn is_sensitive(&self) -> bool {
        matches!(self, Self::AuthInfoPass(_))
    }

    /// The command as it would be logged, with any secret replaced.
    pub fn to_log_string(&self) -> String {
        if self.is_sensitive() {
            return format!("{} <redacted>", self.name());
        }
        match self.to_wire() {
            Ok(bytes) => String::from_utf8_lossy(&bytes)
                .trim_end_matches(['\r', '\n'])
                .to_owned(),
            Err(_) => format!("{} <unencodable>", self.name()),
        }
    }

    /// Encodes the command, terminated by CRLF.
    ///
    /// # Errors
    ///
    /// Returns [`ProtoError::IllegalCommandArgument`] if an argument contains CR, LF, NUL
    /// or another control character, and [`ProtoError::CommandTooLong`] if the encoded
    /// line would exceed [`MAX_COMMAND_LEN`] octets.
    pub fn to_wire(&self) -> Result<Vec<u8>> {
        let mut line = String::new();

        match self {
            Self::Capabilities(keyword) => {
                line.push_str("CAPABILITIES");
                if let Some(keyword) = keyword {
                    line.push(' ');
                    line.push_str(keyword);
                }
            }
            Self::ModeReader => line.push_str("MODE READER"),
            Self::Quit => line.push_str("QUIT"),
            Self::StartTls => line.push_str("STARTTLS"),
            Self::AuthInfoUser(user) => {
                line.push_str("AUTHINFO USER ");
                line.push_str(user);
            }
            Self::AuthInfoPass(pass) => {
                line.push_str("AUTHINFO PASS ");
                line.push_str(pass);
            }
            Self::Group(group) => {
                line.push_str("GROUP ");
                line.push_str(group.as_str());
            }
            Self::ListGroup { group, range } => {
                line.push_str("LISTGROUP");
                if let Some(group) = group {
                    line.push(' ');
                    line.push_str(group.as_str());
                    // A range without a group is not valid syntax, so it is only
                    // appended when a group is present.
                    if let Some(range) = range {
                        line.push(' ');
                        line.push_str(&range.to_argument());
                    }
                }
            }
            Self::Last => line.push_str("LAST"),
            Self::Next => line.push_str("NEXT"),
            Self::Article(spec) => push_with_optional_arg(&mut line, "ARTICLE", spec.to_argument()),
            Self::Head(spec) => push_with_optional_arg(&mut line, "HEAD", spec.to_argument()),
            Self::Body(spec) => push_with_optional_arg(&mut line, "BODY", spec.to_argument()),
            Self::Stat(spec) => push_with_optional_arg(&mut line, "STAT", spec.to_argument()),
            Self::Date => line.push_str("DATE"),
            Self::Post => line.push_str("POST"),
            Self::Help => line.push_str("HELP"),
            Self::NewGroups(since) => {
                // RFC 3977 §7.3.1 permits a four-digit year and recommends GMT.
                line.push_str(&since.format("NEWGROUPS %Y%m%d %H%M%S GMT").to_string());
            }
            Self::List(keyword) => {
                line.push_str("LIST ");
                line.push_str(keyword.keyword());
                if let Some(wildmat) = keyword.wildmat() {
                    line.push(' ');
                    line.push_str(wildmat.as_str());
                }
            }
            Self::Over(target) => push_with_optional_arg(&mut line, "OVER", target.to_argument()),
            Self::XOver(range) => {
                line.push_str("XOVER ");
                line.push_str(&range.to_argument());
            }
            Self::Hdr { field, target } => push_field_command(&mut line, "HDR", field, target),
            Self::XHdr { field, target } => push_field_command(&mut line, "XHDR", field, target),
        }

        self.finish(line)
    }

    /// Validates an assembled command line and appends CRLF.
    fn finish(&self, line: String) -> Result<Vec<u8>> {
        if line.bytes().any(|b| b.is_ascii_control()) {
            return Err(ProtoError::IllegalCommandArgument {
                command: self.name(),
            });
        }

        let encoded_len = line.len() + 2;
        if encoded_len > MAX_COMMAND_LEN {
            return Err(ProtoError::CommandTooLong {
                command: self.name(),
                len: encoded_len,
            });
        }

        let mut bytes = line.into_bytes();
        bytes.extend_from_slice(b"\r\n");
        Ok(bytes)
    }
}

fn push_with_optional_arg(line: &mut String, verb: &str, arg: Option<String>) {
    line.push_str(verb);
    if let Some(arg) = arg {
        line.push(' ');
        line.push_str(&arg);
    }
}

fn push_field_command(line: &mut String, verb: &str, field: &HeaderName, target: &RangeOrId) {
    line.push_str(verb);
    line.push(' ');
    line.push_str(field.as_str());
    if let Some(arg) = target.to_argument() {
        line.push(' ');
        line.push_str(&arg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message_id::MessageId;

    fn wire(command: &Command) -> String {
        String::from_utf8(command.to_wire().unwrap()).unwrap()
    }

    fn group(name: &str) -> GroupName {
        GroupName::parse(name).unwrap()
    }

    #[test]
    fn encodes_commands_without_arguments() {
        assert_eq!(wire(&Command::Quit), "QUIT\r\n");
        assert_eq!(wire(&Command::ModeReader), "MODE READER\r\n");
        assert_eq!(wire(&Command::Date), "DATE\r\n");
        assert_eq!(wire(&Command::StartTls), "STARTTLS\r\n");
        assert_eq!(wire(&Command::Capabilities(None)), "CAPABILITIES\r\n");
    }

    #[test]
    fn encodes_group_and_article_commands() {
        assert_eq!(
            wire(&Command::Group(group("misc.test"))),
            "GROUP misc.test\r\n"
        );
        assert_eq!(
            wire(&Command::Article(ArticleSpec::Number(42))),
            "ARTICLE 42\r\n"
        );
        assert_eq!(wire(&Command::Article(ArticleSpec::Current)), "ARTICLE\r\n");
        assert_eq!(
            wire(&Command::Head(MessageId::parse("<a@b>").unwrap().into())),
            "HEAD <a@b>\r\n"
        );
        assert_eq!(wire(&Command::Stat(ArticleSpec::Current)), "STAT\r\n");
    }

    #[test]
    fn encodes_overview_commands() {
        assert_eq!(
            wire(&Command::Over(Range::between(1, 10).into())),
            "OVER 1-10\r\n"
        );
        assert_eq!(wire(&Command::Over(RangeOrId::Current)), "OVER\r\n");
        assert_eq!(wire(&Command::XOver(Range::From(5))), "XOVER 5-\r\n");
    }

    #[test]
    fn encodes_list_variants() {
        assert_eq!(
            wire(&Command::List(ListKeyword::Active(None))),
            "LIST ACTIVE\r\n"
        );
        assert_eq!(
            wire(&Command::List(ListKeyword::Newsgroups(Some(
                Wildmat::hierarchy("comp").unwrap()
            )))),
            "LIST NEWSGROUPS comp.*\r\n"
        );
        assert_eq!(
            wire(&Command::List(ListKeyword::OverviewFmt)),
            "LIST OVERVIEW.FMT\r\n"
        );
        assert_eq!(
            wire(&Command::List(ListKeyword::ActiveTimes(None))),
            "LIST ACTIVE.TIMES\r\n"
        );
        assert_eq!(
            wire(&Command::List(ListKeyword::Headers)),
            "LIST HEADERS\r\n"
        );
    }

    #[test]
    fn encodes_hdr_commands() {
        let field = HeaderName::parse("Subject").unwrap();
        assert_eq!(
            wire(&Command::Hdr {
                field: field.clone(),
                target: Range::between(1, 3).into()
            }),
            "HDR Subject 1-3\r\n"
        );
        assert_eq!(
            wire(&Command::XHdr {
                field,
                target: RangeOrId::Current
            }),
            "XHDR Subject\r\n"
        );
    }

    #[test]
    fn encodes_listgroup_forms() {
        assert_eq!(
            wire(&Command::ListGroup {
                group: None,
                range: None
            }),
            "LISTGROUP\r\n"
        );
        assert_eq!(
            wire(&Command::ListGroup {
                group: Some(group("misc.test")),
                range: None
            }),
            "LISTGROUP misc.test\r\n"
        );
        assert_eq!(
            wire(&Command::ListGroup {
                group: Some(group("misc.test")),
                range: Some(Range::between(1, 9))
            }),
            "LISTGROUP misc.test 1-9\r\n"
        );
        // A range is meaningless without a group and must not be emitted alone.
        assert_eq!(
            wire(&Command::ListGroup {
                group: None,
                range: Some(Range::between(1, 9))
            }),
            "LISTGROUP\r\n"
        );
    }

    #[test]
    fn encodes_newgroups_with_a_four_digit_year_in_gmt() {
        let since = DateTime::parse_from_rfc3339("2026-09-17T08:09:10Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            wire(&Command::NewGroups(since)),
            "NEWGROUPS 20260917 080910 GMT\r\n"
        );
    }

    #[test]
    fn refuses_arguments_containing_control_characters() {
        // The validated newtypes make this unreachable for group names and message-ids,
        // so the remaining risk is the free-form arguments: credentials and keywords.
        let err = Command::AuthInfoUser("bob\r\nQUIT".to_owned())
            .to_wire()
            .unwrap_err();
        assert_eq!(
            err,
            ProtoError::IllegalCommandArgument {
                command: "AUTHINFO USER"
            }
        );

        assert!(
            Command::AuthInfoPass("hunter2\r\nDATE".to_owned())
                .to_wire()
                .is_err()
        );
        assert!(
            Command::AuthInfoPass("has\0nul".to_owned())
                .to_wire()
                .is_err()
        );
        assert!(
            Command::Capabilities(Some("a\nb".to_owned()))
                .to_wire()
                .is_err()
        );
    }

    #[test]
    fn refuses_a_line_longer_than_the_protocol_limit() {
        let long = "x".repeat(MAX_COMMAND_LEN);
        let err = Command::AuthInfoPass(long).to_wire().unwrap_err();
        assert!(matches!(err, ProtoError::CommandTooLong { .. }));
    }

    #[test]
    fn accepts_a_line_exactly_at_the_limit() {
        // "AUTHINFO PASS " is 14 octets, CRLF is 2.
        let pass = "x".repeat(MAX_COMMAND_LEN - 16);
        assert_eq!(
            Command::AuthInfoPass(pass.clone()).to_wire().unwrap().len(),
            MAX_COMMAND_LEN
        );
        assert!(Command::AuthInfoPass(format!("{pass}x")).to_wire().is_err());
    }

    #[test]
    fn knows_which_commands_return_a_block() {
        assert!(Command::List(ListKeyword::Active(None)).expects_data_block());
        assert!(Command::Over(RangeOrId::Current).expects_data_block());
        assert!(Command::Article(ArticleSpec::Current).expects_data_block());
        assert!(
            Command::ListGroup {
                group: None,
                range: None
            }
            .expects_data_block()
        );

        // GROUP returns 211 with no block; LISTGROUP returns 211 with one.
        assert!(!Command::Group(group("misc.test")).expects_data_block());
        assert!(!Command::Stat(ArticleSpec::Current).expects_data_block());
        assert!(!Command::Date.expects_data_block());
        assert!(!Command::Quit.expects_data_block());
    }

    #[test]
    fn redacts_passwords_from_logs() {
        let command = Command::AuthInfoPass("hunter2".to_owned());
        assert!(command.is_sensitive());
        assert_eq!(command.to_log_string(), "AUTHINFO PASS <redacted>");
        assert!(!command.to_log_string().contains("hunter2"));

        let user = Command::AuthInfoUser("bob".to_owned());
        assert!(!user.is_sensitive());
        assert_eq!(user.to_log_string(), "AUTHINFO USER bob");
    }

    #[test]
    fn log_string_of_an_unencodable_command_does_not_panic() {
        let command = Command::AuthInfoUser("bad\r\n".to_owned());
        assert_eq!(command.to_log_string(), "AUTHINFO USER <unencodable>");
    }

    #[test]
    fn rejects_invalid_wildmats() {
        for bad in ["", "has space", "caf\u{e9}*", &"x".repeat(401)] {
            assert!(Wildmat::parse(bad).is_err(), "expected {bad:?} rejected");
        }
        assert!(Wildmat::parse("comp.lang.*").is_ok());
        assert!(Wildmat::parse("comp.*,!comp.os.*").is_ok());
    }

    fn wildmat(pattern: &str) -> Wildmat {
        Wildmat::parse(pattern).expect("a valid pattern")
    }

    #[test]
    fn a_pattern_without_wildcards_matches_only_itself() {
        let pattern = wildmat("misc.test");
        assert!(pattern.matches("misc.test"));
        assert!(!pattern.matches("misc.test.moderated"));
        assert!(!pattern.matches("alt.misc.test"));
        // Group names are case-sensitive, and so is matching them.
        assert!(!pattern.matches("Misc.Test"));
    }

    #[test]
    fn a_star_matches_any_run_of_characters_including_none() {
        assert!(wildmat("comp.*").matches("comp."));
        assert!(wildmat("comp.*").matches("comp.lang.rust"));
        assert!(!wildmat("comp.*").matches("alt.comp.lang"));

        assert!(wildmat("*rust*").matches("comp.lang.rust"));
        assert!(wildmat("*rust*").matches("rust"));
        assert!(!wildmat("*rust*").matches("comp.lang.ada"));

        assert!(wildmat("*").matches("anything.at.all"));
        assert!(wildmat("*").matches(""));
    }

    #[test]
    fn a_question_mark_matches_exactly_one_character() {
        assert!(wildmat("misc.tes?").matches("misc.test"));
        assert!(!wildmat("misc.tes?").matches("misc.tes"));
        assert!(!wildmat("misc.tes?").matches("misc.tested"));
    }

    #[test]
    fn the_last_matching_sub_pattern_decides() {
        // RFC 3977 §4.2. The rule that makes a subscription list expressive: everything
        // under a hierarchy except one branch, and then one twig of that branch back.
        let pattern = wildmat("comp.*,!comp.os.*,comp.os.linux.*");
        assert!(pattern.matches("comp.lang.rust"));
        assert!(!pattern.matches("comp.os.plan9"));
        assert!(pattern.matches("comp.os.linux.misc"));
        assert!(!pattern.matches("alt.folklore.computers"));
    }

    #[test]
    fn a_negation_alone_matches_nothing() {
        // Nothing has matched by the time the negation is applied, so there is nothing to
        // take away. A list of exclusions with no inclusion is a mistake worth behaving
        // predictably about rather than treating as "everything but".
        assert!(!wildmat("!comp.*").matches("comp.lang.rust"));
        assert!(!wildmat("!comp.*").matches("alt.test"));
    }

    #[test]
    fn stars_do_not_run_away_on_a_long_name() {
        // The pathological case for a naive recursive matcher: many stars, a long name,
        // and no match at the end. This finishes immediately or it does not finish at all.
        let pattern = wildmat("*a*a*a*a*a*a*a*a*a*a*b");
        assert!(!pattern.matches(&"a".repeat(2000)));
        assert!(pattern.matches(&format!("{}b", "a".repeat(2000))));
    }

    #[test]
    fn brackets_and_backslashes_are_ordinary_characters() {
        // RFC 3977 §4.2 excludes them from the grammar, so there are no character classes
        // and no escapes here. Documented rather than silently different.
        assert!(wildmat("comp.[ab]").matches("comp.[ab]"));
        assert!(!wildmat("comp.[ab]").matches("comp.a"));
    }

    #[test]
    fn builds_a_substring_pattern_from_typed_text() {
        assert_eq!(Wildmat::containing("rust").unwrap().as_str(), "*rust*");
        assert!(
            Wildmat::containing("rust")
                .unwrap()
                .matches("comp.lang.rust")
        );
        // What a reader's filter box can hold and a wildmat cannot.
        assert!(Wildmat::containing("two words").is_err());
    }

    #[test]
    fn joins_patterns_with_commas() {
        assert_eq!(Wildmat::join(&[]).unwrap(), None);
        assert_eq!(
            Wildmat::join(&[wildmat("comp.lang.*"), wildmat("misc.test")])
                .unwrap()
                .map(|joined| joined.as_str().to_owned()),
            Some("comp.lang.*,misc.test".to_owned())
        );

        // Too long to send is an error rather than a truncation: a silently shortened
        // subscription list would drop groups without saying so.
        let long = vec![wildmat(&"x".repeat(200)); 3];
        assert!(Wildmat::join(&long).is_err());
    }
}
