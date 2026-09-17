//! How a test server should behave, including how it should misbehave.
//!
//! The point of a fake server is not only to be correct, but to be *incorrect on demand*.
//! Every quirk here was chosen because a real server does it and a client that assumes
//! otherwise breaks against it.

/// What the server sends as its opening banner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GreetingMode {
    /// `200` — service available, posting allowed.
    PostingAllowed,
    /// `201` — service available, posting prohibited.
    NoPosting,
    /// A refusal, such as `400 load shedding` or `502 access denied`.
    Refuse {
        /// The status code to send.
        code: u16,
        /// The text after the code.
        text: String,
    },
}

/// Which capability list the server advertises.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityProfile {
    /// A modern reader server: `READER`, `OVER MSGID`, `HDR`, the usual `LIST` keywords.
    Modern,
    /// A reader server that does not advertise `OVER`, so the client must use `XOVER`.
    NoOver,
    /// A transit server: advertises `MODE-READER` but not `READER` until the client
    /// sends `MODE READER`.
    Transit,
    /// A server predating RFC 3977, which answers `500` to `CAPABILITIES`.
    Legacy,
}

/// Deliberate misbehaviour, for testing how a client copes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Quirks {
    /// Answer `503` to `LIST OVERVIEW.FMT`, forcing the standard layout to be assumed.
    pub no_overview_fmt: bool,

    /// Answer `501` to an `OVER` range with an open upper bound (`low-`).
    ///
    /// Real servers do this, and a client that only ever sends the open form then sees an
    /// empty group.
    pub reject_open_ended_ranges: bool,

    /// Close the connection after this many commands, mid-session.
    pub close_after_commands: Option<usize>,

    /// Close the connection part-way through the next multi-line block, without sending
    /// the terminator.
    pub truncate_next_block: bool,

    /// Answer `HELP` with a single line far longer than any sane limit.
    pub overlong_help_line: bool,

    /// Send bare LF instead of CRLF as the line terminator.
    pub bare_lf: bool,

    /// Advertise `OVER` but answer `500` to it, so the client must fall back to `XOVER`
    /// despite what the capability list said.
    pub over_advertised_but_missing: bool,
}

/// A test server's configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerConfig {
    /// The opening banner.
    pub greeting: GreetingMode,
    /// Which capabilities to advertise.
    pub capabilities: CapabilityProfile,
    /// The credentials to accept, if authentication is required.
    pub credentials: Option<Credentials>,
    /// Whether reading commands are refused with `480` until authenticated.
    pub require_auth: bool,
    /// Deliberate misbehaviour.
    pub quirks: Quirks,
    /// The server name used in the greeting.
    pub server_name: String,
}

/// A username and password the server will accept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credentials {
    /// The accepted username.
    pub username: String,
    /// The accepted password.
    pub password: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            greeting: GreetingMode::PostingAllowed,
            capabilities: CapabilityProfile::Modern,
            credentials: None,
            require_auth: false,
            quirks: Quirks::default(),
            server_name: "test.invalid".to_owned(),
        }
    }
}

impl ServerConfig {
    /// The default configuration: a modern, open, well-behaved reader server.
    pub fn new() -> Self {
        Self::default()
    }

    /// Advertises a different capability profile.
    #[must_use]
    pub fn capabilities(mut self, profile: CapabilityProfile) -> Self {
        self.capabilities = profile;
        self
    }

    /// Requires authentication before any reading command.
    #[must_use]
    pub fn require_auth(mut self, username: &str, password: &str) -> Self {
        self.credentials = Some(Credentials {
            username: username.to_owned(),
            password: password.to_owned(),
        });
        self.require_auth = true;
        self
    }

    /// Sets the greeting.
    #[must_use]
    pub fn greeting(mut self, greeting: GreetingMode) -> Self {
        self.greeting = greeting;
        self
    }

    /// Sets the quirks.
    #[must_use]
    pub fn quirks(mut self, quirks: Quirks) -> Self {
        self.quirks = quirks;
        self
    }

    /// The line terminator this configuration uses.
    pub fn terminator(&self) -> &'static [u8] {
        if self.quirks.bare_lf { b"\n" } else { b"\r\n" }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_a_well_behaved_modern_server() {
        let config = ServerConfig::new();
        assert_eq!(config.capabilities, CapabilityProfile::Modern);
        assert_eq!(config.greeting, GreetingMode::PostingAllowed);
        assert!(!config.require_auth);
        assert_eq!(config.quirks, Quirks::default());
        assert_eq!(config.terminator(), b"\r\n");
    }

    #[test]
    fn configuration_is_chainable() {
        let config = ServerConfig::new()
            .capabilities(CapabilityProfile::Legacy)
            .require_auth("bob", "hunter2")
            .greeting(GreetingMode::NoPosting)
            .quirks(Quirks {
                bare_lf: true,
                ..Quirks::default()
            });

        assert_eq!(config.capabilities, CapabilityProfile::Legacy);
        assert!(config.require_auth);
        assert_eq!(
            config.credentials.as_ref().map(|c| c.username.as_str()),
            Some("bob")
        );
        assert_eq!(config.terminator(), b"\n");
    }
}
