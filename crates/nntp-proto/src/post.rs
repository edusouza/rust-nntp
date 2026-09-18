//! An article on its way out.
//!
//! Everything about posting that does not need a socket: taking the text a person typed,
//! saying what is wrong with it before anything is sent, and turning it into the lines of
//! a data block.
//!
//! The order matters. A draft is checked *here*, in front of the user, rather than by the
//! server — a `441` arrives after the article has been offered, usually says something
//! terse, and is the worst moment to discover that the `Newsgroups` line was empty.
//! Nothing in this module can be skipped by a caller in a hurry: [`Draft::to_lines`]
//! validates and refuses.

use crate::error::{ProtoError, Result};
use crate::headers::HeaderName;
use crate::message_id::MessageId;
use crate::mime::encode_header_value;

/// Headers the posting agent must not set, because the server sets them.
///
/// `Path` and `Xref` are added by the injecting and serving agents (RFC 5536 §3.1.5,
/// §3.2.13); a client that sends its own is at best ignored and at worst refused. `Lines`
/// and `Bytes` are the server's count of what it received, so a client's guess can only be
/// wrong.
const SERVER_OWNED: [&str; 4] = ["path", "xref", "lines", "bytes"];

/// Headers an article cannot be posted without.
const REQUIRED: [&str; 3] = ["from", "newsgroups", "subject"];

/// What is wrong with a draft.
///
/// A list rather than one error: somebody who has just written an article wants to be told
/// everything that needs fixing before they open the editor again, not one thing per
/// attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Problem {
    /// A header the article cannot be posted without is missing or empty.
    MissingHeader(&'static str),
    /// A header line that is not `Name: value`.
    MalformedHeader(String),
    /// A header name that is not a header name at all.
    InvalidHeaderName(String),
    /// A header the server owns.
    ServerOwnedHeader(String),
    /// `Newsgroups` is present but names no group.
    NoNewsgroups,
    /// The article has headers and nothing else.
    EmptyBody,
}

impl std::fmt::Display for Problem {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingHeader(name) => write!(formatter, "{name} is missing"),
            Self::MalformedHeader(line) => {
                write!(formatter, "not a header line: {line}")
            }
            Self::InvalidHeaderName(name) => write!(formatter, "not a header name: {name}"),
            Self::ServerOwnedHeader(name) => {
                write!(formatter, "{name} is set by the server, not by the poster")
            }
            Self::NoNewsgroups => write!(formatter, "Newsgroups names no group"),
            Self::EmptyBody => write!(formatter, "the article has no body"),
        }
    }
}

/// An article being composed.
///
/// Built from the text an editor produced — headers, a blank line, then the body, which is
/// the shape of an article and so the shape a person editing one expects.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Draft {
    /// Header fields in the order they were written.
    pub headers: Vec<(String, String)>,
    /// The body, with `\n` line endings.
    pub body: String,
}

impl Draft {
    /// Reads a draft out of the text an editor produced.
    ///
    /// The first empty line separates headers from body, per RFC 5322 §2.1. Everything
    /// after it is body, including further empty lines. CRLF, LF and a stray CR are all
    /// accepted, because an editor on any of three platforms may produce any of them.
    ///
    /// Parsing never fails: a line that is not a header becomes a [`Problem`] rather than
    /// an error, so that everything wrong with the draft can be reported at once.
    pub fn parse(text: &str) -> Self {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let mut headers = Vec::new();
        let mut lines = text.split('\n').peekable();
        let mut unfolded: Vec<String> = Vec::new();

        while let Some(line) = lines.next() {
            if line.trim().is_empty() {
                break;
            }

            // A continuation line belongs to the header above it (RFC 5322 §2.2.3).
            if line.starts_with([' ', '\t']) && !unfolded.is_empty() {
                if let Some(last) = unfolded.last_mut() {
                    last.push(' ');
                    last.push_str(line.trim());
                }
                continue;
            }

            unfolded.push(line.to_owned());
            // Nothing to do with the peeked line; it is read on the next turn.
            let _ = lines.peek();
        }

        for line in unfolded {
            match line.split_once(':') {
                Some((name, value)) => {
                    headers.push((name.trim().to_owned(), value.trim().to_owned()))
                }
                // Kept verbatim under an empty name so that `problems` can quote it back.
                None => headers.push((String::new(), line)),
            }
        }

        let body = text
            .split_once("\n\n")
            .map_or(String::new(), |(_, body)| body.to_owned());

        Self { headers, body }
    }

    /// The value of a header, ignoring case.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(field, _)| field.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// Sets a header, replacing any existing one of that name.
    pub fn set_header(&mut self, name: &str, value: impl Into<String>) {
        let value = value.into();
        if let Some(existing) = self
            .headers
            .iter_mut()
            .find(|(field, _)| field.eq_ignore_ascii_case(name))
        {
            existing.1 = value;
            return;
        }
        self.headers.push((name.to_owned(), value));
    }

    /// The groups this article is addressed to.
    pub fn newsgroups(&self) -> Vec<&str> {
        self.header("Newsgroups")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|group| !group.is_empty())
            .collect()
    }

    /// Everything wrong with the draft, in the order a person would fix it.
    ///
    /// Empty means it can be sent — as far as anything on this side can tell. The server
    /// still has the last word, which is why [`crate::response::codes::POSTING_FAILED`]
    /// text is worth showing verbatim.
    pub fn problems(&self) -> Vec<Problem> {
        let mut problems = Vec::new();

        for (name, value) in &self.headers {
            if name.is_empty() {
                problems.push(Problem::MalformedHeader(value.clone()));
                continue;
            }
            if HeaderName::parse(name).is_err() {
                problems.push(Problem::InvalidHeaderName(name.clone()));
                continue;
            }
            if SERVER_OWNED
                .iter()
                .any(|owned| name.eq_ignore_ascii_case(owned))
            {
                problems.push(Problem::ServerOwnedHeader(name.clone()));
            }
        }

        for required in REQUIRED {
            let present = self
                .header(required)
                .is_some_and(|value| !value.trim().is_empty());
            if !present {
                // `REQUIRED` holds the canonical spellings; reporting the lowercase form
                // would send someone looking for a header they did write.
                problems.push(Problem::MissingHeader(canonical(required)));
            }
        }

        if self.header("Newsgroups").is_some() && self.newsgroups().is_empty() {
            problems.push(Problem::NoNewsgroups);
        }

        if self.body.trim().is_empty() {
            problems.push(Problem::EmptyBody);
        }

        problems
    }

    /// The article as the lines of a data block, ready to be dot-stuffed and sent.
    ///
    /// Header values that are not ASCII become RFC 2047 encoded words, and a body that is
    /// not ASCII gets the MIME headers that say so — a reader on the other end has no way
    /// to guess UTF-8 from a bare 8-bit body, and this project spent a good deal of effort
    /// on not showing people mojibake.
    ///
    /// # Errors
    ///
    /// Returns [`ProtoError::UnpostableDraft`] listing everything [`Self::problems`]
    /// found. Sending an article that is known to be wrong is worse
    /// than refusing to: the server's refusal is terser and arrives later.
    pub fn to_lines(&self) -> Result<Vec<Vec<u8>>> {
        let problems = self.problems();
        if !problems.is_empty() {
            let summary = problems
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ");
            return Err(ProtoError::UnpostableDraft { problems: summary });
        }

        let mut lines: Vec<Vec<u8>> = Vec::new();

        for (name, value) in &self.headers {
            lines.push(format!("{name}: {}", encode_header_value(value)).into_bytes());
        }

        if !self.body.is_ascii() {
            if self.header("MIME-Version").is_none() {
                lines.push(b"MIME-Version: 1.0".to_vec());
            }
            if self.header("Content-Type").is_none() {
                lines.push(b"Content-Type: text/plain; charset=utf-8".to_vec());
            }
            if self.header("Content-Transfer-Encoding").is_none() {
                lines.push(b"Content-Transfer-Encoding: 8bit".to_vec());
            }
        }

        lines.push(Vec::new());

        for line in self.body.replace("\r\n", "\n").split('\n') {
            lines.push(line.trim_end_matches('\r').as_bytes().to_vec());
        }

        // A body ending in a newline produces a trailing empty line, which would be sent
        // as a blank line at the end of the article. Harmless, but every other article on
        // the group does not have one.
        while lines.last().is_some_and(Vec::is_empty) && lines.len() > 1 {
            lines.pop();
        }

        Ok(lines)
    }

    /// A draft that follows up to an article.
    ///
    /// The conventions a newsreader is expected to get right, which is most of why this is
    /// a function rather than something each caller assembles:
    ///
    /// - `Subject` gains one `Re: `, and only one: `Re: Re: Re:` is what happens when
    ///   every reader adds its own.
    /// - `References` is the parent's chain plus the parent, which is what makes the reply
    ///   land in the right place in everybody else's threading.
    /// - `Followup-To` wins over `Newsgroups` when the parent set it, since that is
    ///   exactly what it is for (RFC 5536 §3.2.6). `poster` means a mail reply, which this
    ///   reader cannot send, so it is reported rather than silently posted to the group.
    pub fn follow_up(parent: &FollowUp<'_>, from: &str) -> Self {
        let mut draft = Self::default();

        draft.set_header("From", from);
        draft.set_header("Newsgroups", parent.newsgroups.join(","));

        let subject = parent.subject.trim();
        let subject = if crate::thread::normalise_subject(subject).1 {
            subject.to_owned()
        } else {
            format!("Re: {subject}")
        };
        draft.set_header("Subject", subject);

        let mut references: Vec<String> = parent
            .references
            .iter()
            .map(|id| id.as_str().to_owned())
            .collect();
        if let Some(id) = parent.message_id {
            references.push(id.as_str().to_owned());
        }
        if !references.is_empty() {
            draft.set_header("References", references.join(" "));
        }

        draft.body = quote(parent.author, parent.body);
        draft
    }
}

/// What a follow-up needs to know about the article it answers.
#[derive(Debug, Clone)]
pub struct FollowUp<'a> {
    /// The parent's message-id, if it has one.
    pub message_id: Option<&'a MessageId>,
    /// The parent's `References`, oldest first.
    pub references: &'a [MessageId],
    /// The parent's subject, undecorated.
    pub subject: &'a str,
    /// The parent's author, for the attribution line.
    pub author: &'a str,
    /// Where the follow-up should go: `Followup-To` if the parent set one, otherwise the
    /// groups it was posted to.
    pub newsgroups: Vec<String>,
    /// The parent's body, to quote.
    pub body: &'a str,
}

/// Quotes a body under an attribution line.
///
/// Signatures are dropped: quoting somebody's `.signature` back at them is the most
/// reliable way to be told off on Usenet, and RFC 3676 §4.3 makes the cut unambiguous.
fn quote(author: &str, body: &str) -> String {
    let mut out = String::new();
    if !author.trim().is_empty() {
        out.push_str(&format!("{} wrote:\n", author.trim()));
    }

    for line in body.replace("\r\n", "\n").split('\n') {
        if line == "-- " {
            break;
        }
        if line.starts_with('>') {
            // Already quoted: no space, so that the depth stays readable.
            out.push_str(&format!(">{line}\n"));
        } else if line.is_empty() {
            out.push_str(">\n");
        } else {
            out.push_str(&format!("> {line}\n"));
        }
    }

    out.push('\n');
    out
}

/// The conventional spelling of a header name this module requires.
const fn canonical(lowercase: &str) -> &'static str {
    match lowercase.as_bytes() {
        b"from" => "From",
        b"newsgroups" => "Newsgroups",
        b"subject" => "Subject",
        _ => "a required header",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(text: &str) -> Draft {
        Draft::parse(text)
    }

    const VALID: &str = "From: A Poster <a@example.org>\n\
                         Newsgroups: misc.test\n\
                         Subject: Testing\n\
                         \n\
                         Hello, world.\n";

    #[test]
    fn splits_headers_from_the_body_at_the_first_empty_line() {
        let draft = draft(VALID);

        assert_eq!(draft.header("Subject"), Some("Testing"));
        assert_eq!(draft.header("subject"), Some("Testing"), "case insensitive");
        assert_eq!(draft.body, "Hello, world.\n");
        assert!(draft.problems().is_empty(), "{:?}", draft.problems());
    }

    #[test]
    fn accepts_whatever_line_endings_the_editor_produced() {
        let crlf = draft(&VALID.replace('\n', "\r\n"));
        assert_eq!(crlf.header("Subject"), Some("Testing"));
        assert_eq!(crlf.body, "Hello, world.\n");
        assert!(crlf.problems().is_empty());
    }

    #[test]
    fn folded_headers_are_joined() {
        let draft = draft(
            "From: A Poster <a@example.org>\n\
             Newsgroups: misc.test\n\
             Subject: A subject that was\n\
             \tfolded across two lines\n\
             \n\
             Body.\n",
        );

        assert_eq!(
            draft.header("Subject"),
            Some("A subject that was folded across two lines")
        );
    }

    #[test]
    fn a_body_containing_empty_lines_keeps_them() {
        let draft = draft("From: a@x\nNewsgroups: misc.test\nSubject: s\n\nOne.\n\nTwo.\n");
        assert_eq!(draft.body, "One.\n\nTwo.\n");
    }

    #[test]
    fn reports_every_missing_header_at_once() {
        // One per attempt would mean opening the editor three times.
        let problems = draft("Subject: only this\n\nBody.\n").problems();

        assert!(
            problems.contains(&Problem::MissingHeader("From")),
            "{problems:?}"
        );
        assert!(
            problems.contains(&Problem::MissingHeader("Newsgroups")),
            "{problems:?}"
        );
    }

    #[test]
    fn an_empty_body_is_a_problem() {
        let problems = draft("From: a@x\nNewsgroups: misc.test\nSubject: s\n\n   \n").problems();
        assert!(problems.contains(&Problem::EmptyBody), "{problems:?}");
    }

    #[test]
    fn a_newsgroups_header_that_names_nothing_is_a_problem() {
        let problems = draft("From: a@x\nNewsgroups: , ,\nSubject: s\n\nBody.\n").problems();
        assert!(problems.contains(&Problem::NoNewsgroups), "{problems:?}");
    }

    #[test]
    fn refuses_the_headers_the_server_owns() {
        for header in ["Path", "Xref", "Lines", "Bytes"] {
            let text = format!("From: a@x\nNewsgroups: misc.test\nSubject: s\n{header}: x\n\nB.\n");
            let problems = draft(&text).problems();
            assert!(
                problems.iter().any(
                    |problem| matches!(problem, Problem::ServerOwnedHeader(name) if name == header)
                ),
                "{header} was allowed: {problems:?}"
            );
        }
    }

    #[test]
    fn a_line_that_is_not_a_header_is_reported_with_its_text() {
        let problems = draft("From: a@x\nthis is not a header\n\nBody.\n").problems();
        assert!(
            problems
                .iter()
                .any(|problem| matches!(problem, Problem::MalformedHeader(line) if line.contains("not a header"))),
            "{problems:?}"
        );
    }

    #[test]
    fn a_draft_with_problems_will_not_encode() {
        // The guarantee: a caller cannot skip validation by going straight to the wire.
        let error = draft("Subject: nothing else\n\nBody.\n")
            .to_lines()
            .unwrap_err();
        assert!(error.to_string().contains("From"), "{error}");
    }

    #[test]
    fn encodes_headers_body_and_the_blank_line_between_them() {
        let lines = draft(VALID).to_lines().expect("a valid draft");
        let text: Vec<String> = lines
            .iter()
            .map(|line| String::from_utf8_lossy(line).into_owned())
            .collect();

        assert_eq!(
            text,
            vec![
                "From: A Poster <a@example.org>".to_owned(),
                "Newsgroups: misc.test".to_owned(),
                "Subject: Testing".to_owned(),
                String::new(),
                "Hello, world.".to_owned(),
            ]
        );
    }

    #[test]
    fn a_non_ascii_subject_becomes_an_encoded_word() {
        let draft = draft("From: a@x\nNewsgroups: misc.test\nSubject: ação\n\nBody.\n");
        let lines = draft.to_lines().expect("valid");

        let subject = lines
            .iter()
            .map(|line| String::from_utf8_lossy(line).into_owned())
            .find(|line| line.starts_with("Subject:"))
            .expect("a subject line");

        assert!(subject.is_ascii(), "{subject}");
        assert_eq!(
            crate::mime::decode_header_value(subject.trim_start_matches("Subject: ").as_bytes()),
            "ação"
        );
    }

    #[test]
    fn a_non_ascii_body_gets_the_headers_that_explain_it() {
        // Without these, the bytes are correct and every reader on the other end guesses.
        let draft = draft("From: a@x\nNewsgroups: misc.test\nSubject: s\n\nAção.\n");
        let text: Vec<String> = draft
            .to_lines()
            .expect("valid")
            .iter()
            .map(|line| String::from_utf8_lossy(line).into_owned())
            .collect();

        assert!(
            text.iter().any(|line| line == "MIME-Version: 1.0"),
            "{text:?}"
        );
        assert!(
            text.iter()
                .any(|line| line == "Content-Type: text/plain; charset=utf-8"),
            "{text:?}"
        );
        assert!(
            text.iter()
                .any(|line| line == "Content-Transfer-Encoding: 8bit"),
            "{text:?}"
        );
    }

    #[test]
    fn a_body_that_already_says_what_it_is_is_left_alone() {
        let draft = draft(
            "From: a@x\nNewsgroups: misc.test\nSubject: s\n\
             Content-Type: text/plain; charset=iso-8859-1\n\nAção.\n",
        );
        let text: Vec<String> = draft
            .to_lines()
            .expect("valid")
            .iter()
            .map(|line| String::from_utf8_lossy(line).into_owned())
            .collect();

        assert_eq!(
            text.iter()
                .filter(|line| line.starts_with("Content-Type:"))
                .count(),
            1,
            "{text:?}"
        );
    }

    #[test]
    fn a_body_line_that_starts_with_a_dot_survives_as_a_line_of_its_own() {
        // Dot-stuffing happens at the connection, but the line has to reach it intact.
        let draft = draft("From: a@x\nNewsgroups: misc.test\nSubject: s\n\n.signature\nend\n");
        let lines = draft.to_lines().expect("valid");

        assert!(
            lines.iter().any(|line| line == b".signature"),
            "{:?}",
            lines
                .iter()
                .map(|l| String::from_utf8_lossy(l).into_owned())
                .collect::<Vec<_>>()
        );
    }

    fn parent(subject: &str, body: &str) -> (MessageId, Vec<MessageId>) {
        let _ = (subject, body);
        (
            MessageId::parse("<parent@example.org>").expect("id"),
            vec![MessageId::parse("<root@example.org>").expect("id")],
        )
    }

    #[test]
    fn a_follow_up_carries_the_reference_chain_and_one_re() {
        let (id, references) = parent("the question", "");
        let draft = Draft::follow_up(
            &FollowUp {
                message_id: Some(&id),
                references: &references,
                subject: "the question",
                author: "A Poster <a@example.org>",
                newsgroups: vec!["misc.test".to_owned()],
                body: "What do you think?",
            },
            "Me <me@example.org>",
        );

        assert_eq!(draft.header("Subject"), Some("Re: the question"));
        assert_eq!(
            draft.header("References"),
            Some("<root@example.org> <parent@example.org>")
        );
        assert_eq!(draft.header("Newsgroups"), Some("misc.test"));
        assert_eq!(draft.header("From"), Some("Me <me@example.org>"));
    }

    #[test]
    fn a_follow_up_to_a_reply_does_not_stack_another_re() {
        let (id, references) = parent("Re: the question", "");
        let draft = Draft::follow_up(
            &FollowUp {
                message_id: Some(&id),
                references: &references,
                subject: "Re: the question",
                author: "",
                newsgroups: vec!["misc.test".to_owned()],
                body: "",
            },
            "me@example.org",
        );

        assert_eq!(draft.header("Subject"), Some("Re: the question"));
    }

    #[test]
    fn a_follow_up_quotes_the_parent_and_drops_its_signature() {
        let (id, references) = parent("s", "");
        let draft = Draft::follow_up(
            &FollowUp {
                message_id: Some(&id),
                references: &references,
                subject: "s",
                author: "A Poster",
                newsgroups: vec!["misc.test".to_owned()],
                body: "First line.\n\n> and a quote\n-- \nthe signature\n",
            },
            "me@example.org",
        );

        assert!(
            draft.body.starts_with("A Poster wrote:\n"),
            "{}",
            draft.body
        );
        assert!(draft.body.contains("> First line."), "{}", draft.body);
        assert!(draft.body.contains(">\n"), "{}", draft.body);
        assert!(draft.body.contains(">> and a quote"), "{}", draft.body);
        assert!(
            !draft.body.contains("the signature"),
            "the signature was quoted back: {}",
            draft.body
        );
    }

    #[test]
    fn setting_a_header_twice_replaces_it_rather_than_repeating_it() {
        let mut draft = Draft::default();
        draft.set_header("Subject", "first");
        draft.set_header("subject", "second");

        assert_eq!(draft.headers.len(), 1);
        assert_eq!(draft.header("Subject"), Some("second"));
    }
}
