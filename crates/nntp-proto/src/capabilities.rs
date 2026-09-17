//! The `CAPABILITIES` response (RFC 3977 §5.2).
//!
//! Capability advertisement is how a client avoids guessing. It is also routinely
//! incomplete: servers omit `OVER` while implementing it, advertise `READER` only after
//! `MODE READER`, and predate the command entirely. So this type answers questions
//! ("can I use `OVER` with a message-id?") rather than exposing a set of strings, and the
//! client is expected to fall back on the RFC 2980 commands when the answer is no.

use crate::block::DataBlock;
use crate::mime::decode_8bit_lossy;

/// One advertised capability: a label and its arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capability {
    /// The capability label, as sent. Compare case-insensitively.
    pub label: String,
    /// The arguments that followed the label, if any.
    pub args: Vec<String>,
}

/// The parsed capability list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Capabilities {
    entries: Vec<Capability>,
}

impl Capabilities {
    /// An empty capability list, equivalent to a server that does not support the command.
    pub fn new() -> Self {
        Self::default()
    }

    /// Parses the data block returned by `CAPABILITIES`.
    ///
    /// Unknown labels are kept: a capability this crate does not understand may still
    /// matter to a caller, and discarding it would make the omission invisible.
    pub fn parse(block: &DataBlock) -> Self {
        let entries = block
            .lines()
            .iter()
            .filter_map(|line| {
                let text = decode_8bit_lossy(line);
                let mut tokens = text.split_ascii_whitespace();
                let label = tokens.next()?.to_owned();
                Some(Capability {
                    label,
                    args: tokens.map(str::to_owned).collect(),
                })
            })
            .collect();

        Self { entries }
    }

    /// Every advertised capability.
    pub fn entries(&self) -> &[Capability] {
        &self.entries
    }

    /// Whether the list is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Looks up a capability by label, case-insensitively.
    pub fn get(&self, label: &str) -> Option<&Capability> {
        self.entries
            .iter()
            .find(|entry| entry.label.eq_ignore_ascii_case(label))
    }

    /// Whether a label was advertised.
    pub fn supports(&self, label: &str) -> bool {
        self.get(label).is_some()
    }

    /// The arguments of a capability, or an empty slice if it was not advertised.
    pub fn args(&self, label: &str) -> &[String] {
        self.get(label).map_or(&[], |entry| entry.args.as_slice())
    }

    /// Whether a capability was advertised with a given argument, case-insensitively.
    pub fn has_arg(&self, label: &str, arg: &str) -> bool {
        self.args(label)
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(arg))
    }

    /// The protocol versions listed by `VERSION`.
    pub fn versions(&self) -> Vec<u32> {
        self.args("VERSION")
            .iter()
            .filter_map(|arg| arg.parse().ok())
            .collect()
    }

    /// Whether the server is in, or can enter, reader mode.
    pub fn has_reader(&self) -> bool {
        self.supports("READER")
    }

    /// Whether `MODE READER` is likely to be needed.
    ///
    /// A server that advertises `MODE-READER` but not `READER` is in transit mode and
    /// wants the command before it will serve articles.
    pub fn needs_mode_reader(&self) -> bool {
        self.supports("MODE-READER") && !self.has_reader()
    }

    /// Whether `POST` is available.
    pub fn has_post(&self) -> bool {
        self.supports("POST")
    }

    /// Whether `OVER` is available.
    pub fn has_over(&self) -> bool {
        self.supports("OVER")
    }

    /// Whether `OVER` accepts a message-id as well as a range.
    ///
    /// Advertised as `OVER MSGID`. Without it, overview data can only be requested by
    /// article number, which means a group must be selected first.
    pub fn over_accepts_message_id(&self) -> bool {
        self.has_arg("OVER", "MSGID")
    }

    /// Whether `HDR` is available.
    pub fn has_hdr(&self) -> bool {
        self.supports("HDR")
    }

    /// Whether `NEWNEWS` is available. Frequently disabled for load reasons.
    pub fn has_newnews(&self) -> bool {
        self.supports("NEWNEWS")
    }

    /// Whether `STARTTLS` is offered (RFC 4642).
    pub fn has_starttls(&self) -> bool {
        self.supports("STARTTLS")
    }

    /// Whether `AUTHINFO USER`/`PASS` is offered (RFC 4643).
    ///
    /// `AUTHINFO` with no arguments is treated as offering both `USER` and `SASL`, which
    /// is how RFC 4643 §2.1 defines the bare form.
    pub fn has_authinfo_user(&self) -> bool {
        match self.get("AUTHINFO") {
            Some(entry) => {
                entry.args.is_empty()
                    || entry
                        .args
                        .iter()
                        .any(|arg| arg.eq_ignore_ascii_case("USER"))
            }
            None => false,
        }
    }

    /// The SASL mechanisms advertised by the `SASL` capability.
    pub fn sasl_mechanisms(&self) -> &[String] {
        self.args("SASL")
    }

    /// Whether `COMPRESS DEFLATE` is offered (RFC 8054).
    pub fn has_compress_deflate(&self) -> bool {
        self.has_arg("COMPRESS", "DEFLATE")
    }

    /// The `LIST` keywords the server accepts.
    pub fn list_keywords(&self) -> &[String] {
        self.args("LIST")
    }

    /// Whether a particular `LIST` keyword is accepted.
    pub fn has_list_keyword(&self, keyword: &str) -> bool {
        self.has_arg("LIST", keyword)
    }

    /// The server's self-description from the `IMPLEMENTATION` capability.
    ///
    /// Free-form text. Useful in bug reports and for working around known server bugs,
    /// never for deciding whether a command is available — that is what the other
    /// capabilities are for.
    pub fn implementation(&self) -> Option<String> {
        self.get("IMPLEMENTATION").map(|entry| entry.args.join(" "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capabilities(text: &str) -> Capabilities {
        Capabilities::parse(&DataBlock::parse(text.as_bytes()))
    }

    const INN: &str = "VERSION 2\r\n\
        IMPLEMENTATION INN 2.7.1\r\n\
        READER\r\n\
        POST\r\n\
        NEWNEWS\r\n\
        HDR\r\n\
        OVER MSGID\r\n\
        LIST ACTIVE ACTIVE.TIMES NEWSGROUPS OVERVIEW.FMT HEADERS\r\n\
        AUTHINFO USER\r\n\
        STARTTLS\r\n\
        COMPRESS DEFLATE\r\n\
        .\r\n";

    #[test]
    fn parses_a_realistic_capability_list() {
        let caps = capabilities(INN);
        assert_eq!(caps.versions(), [2]);
        assert_eq!(caps.implementation().as_deref(), Some("INN 2.7.1"));
        assert!(caps.has_reader());
        assert!(caps.has_post());
        assert!(caps.has_over());
        assert!(caps.over_accepts_message_id());
        assert!(caps.has_hdr());
        assert!(caps.has_newnews());
        assert!(caps.has_starttls());
        assert!(caps.has_authinfo_user());
        assert!(caps.has_compress_deflate());
        assert!(caps.has_list_keyword("OVERVIEW.FMT"));
        assert!(caps.has_list_keyword("overview.fmt"));
        assert!(!caps.has_list_keyword("DISTRIB.PATS"));
        assert!(!caps.needs_mode_reader());
    }

    #[test]
    fn labels_are_case_insensitive() {
        let caps = capabilities("reader\r\nOver msgid\r\n.\r\n");
        assert!(caps.supports("READER"));
        assert!(caps.has_over());
        assert!(caps.over_accepts_message_id());
    }

    #[test]
    fn over_without_msgid_is_range_only() {
        let caps = capabilities("OVER\r\n.\r\n");
        assert!(caps.has_over());
        assert!(!caps.over_accepts_message_id());
    }

    #[test]
    fn detects_a_transit_server_that_needs_mode_reader() {
        let caps = capabilities("VERSION 2\r\nIHAVE\r\nMODE-READER\r\n.\r\n");
        assert!(caps.needs_mode_reader());
        assert!(!caps.has_reader());

        // Once READER is advertised the command is unnecessary.
        let after = capabilities("VERSION 2\r\nREADER\r\nMODE-READER\r\n.\r\n");
        assert!(!after.needs_mode_reader());
    }

    #[test]
    fn bare_authinfo_offers_user_authentication() {
        // RFC 4643 §2.1: AUTHINFO with no arguments means both USER and SASL.
        assert!(capabilities("AUTHINFO\r\n.\r\n").has_authinfo_user());
        assert!(capabilities("AUTHINFO USER SASL\r\n.\r\n").has_authinfo_user());
        assert!(!capabilities("AUTHINFO SASL\r\n.\r\n").has_authinfo_user());
        assert!(!capabilities("READER\r\n.\r\n").has_authinfo_user());
    }

    #[test]
    fn reports_sasl_mechanisms() {
        let caps = capabilities("SASL PLAIN CRAM-MD5\r\n.\r\n");
        assert_eq!(caps.sasl_mechanisms(), ["PLAIN", "CRAM-MD5"]);
        assert!(capabilities("READER\r\n.\r\n").sasl_mechanisms().is_empty());
    }

    #[test]
    fn an_empty_list_denies_everything() {
        let caps = Capabilities::new();
        assert!(caps.is_empty());
        assert!(!caps.has_over());
        assert!(!caps.has_reader());
        assert!(caps.versions().is_empty());
        assert!(caps.implementation().is_none());
        assert!(caps.args("LIST").is_empty());
    }

    #[test]
    fn keeps_unknown_capabilities() {
        let caps = capabilities("X-SERVER-EXTENSION alpha beta\r\n.\r\n");
        assert!(caps.supports("X-SERVER-EXTENSION"));
        assert_eq!(caps.args("X-SERVER-EXTENSION"), ["alpha", "beta"]);
    }

    #[test]
    fn ignores_blank_lines_and_extra_whitespace() {
        let caps = capabilities("\r\n   READER   \r\n\r\nOVER\r\n.\r\n");
        assert_eq!(caps.entries().len(), 2);
        assert!(caps.has_reader());
    }

    #[test]
    fn ignores_an_unparseable_version() {
        let caps = capabilities("VERSION 2 three 4\r\n.\r\n");
        assert_eq!(caps.versions(), [2, 4]);
    }
}
