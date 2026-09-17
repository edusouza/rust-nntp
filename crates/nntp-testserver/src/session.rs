//! One client session: the command loop.
//!
//! Written against `BufRead + Write` rather than `TcpStream` so the server's own logic can
//! be tested over in-memory buffers, the same trick the client uses.

use std::io::{BufRead, BufReader, Read, Write};

use crate::config::{CapabilityProfile, GreetingMode, ServerConfig};
use crate::corpus::{Corpus, Group};

/// Whether the session continues after a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    /// Read another command.
    Continue,
    /// Close the connection.
    Close,
    /// The client sent `STARTTLS` and has been told to proceed; the caller must now
    /// perform the handshake and start a fresh command phase over the encrypted stream.
    UpgradeToTls,
}

/// Why a session stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The client quit, or the connection ended.
    Closed,
    /// A `STARTTLS` upgrade is pending.
    UpgradeToTls,
}

/// Per-connection state.
#[derive(Debug, Default)]
struct State {
    authenticated: bool,
    pending_user: Option<String>,
    reader_mode: bool,
    current_group: Option<String>,
    current_article: Option<u64>,
    commands_served: usize,
    truncate_next_block: bool,
    encrypted: bool,
    /// Set when a quirk has left the stream in a state the session cannot continue from,
    /// such as a block written without its terminator.
    close_requested: bool,
}

/// A single client session.
#[derive(Debug)]
pub struct Session<'a> {
    corpus: &'a Corpus,
    config: &'a ServerConfig,
    state: State,
}

impl<'a> Session<'a> {
    /// Starts a session.
    pub fn new(corpus: &'a Corpus, config: &'a ServerConfig) -> Self {
        Self {
            corpus,
            config,
            state: State {
                truncate_next_block: config.quirks.truncate_next_block,
                // A server that advertises READER is already in reader mode.
                reader_mode: !matches!(config.capabilities, CapabilityProfile::Transit),
                ..State::default()
            },
        }
    }

    /// Whether this session is running over an encrypted transport.
    pub fn is_encrypted(&self) -> bool {
        self.state.encrypted
    }

    /// Records that the transport is encrypted, so `STARTTLS` is no longer offered.
    pub fn set_encrypted(&mut self, encrypted: bool) {
        self.state.encrypted = encrypted;
    }

    /// Runs the session over separate read and write halves: greeting, then commands
    /// until `QUIT` or end of input.
    ///
    /// # Errors
    ///
    /// Propagates IO errors from the transport. A client that disconnects abruptly is
    /// reported as `UnexpectedEof` or `ConnectionReset`, which the caller normally
    /// ignores.
    pub fn run(
        &mut self,
        input: &mut impl BufRead,
        output: &mut impl Write,
    ) -> std::io::Result<Outcome> {
        if self.greet(output)? == Flow::Close {
            return Ok(Outcome::Closed);
        }

        let mut line = Vec::new();
        loop {
            line.clear();
            if input.read_until(b'\n', &mut line)? == 0 {
                return Ok(Outcome::Closed);
            }

            let command = trim_eol(&line).to_vec();
            match self.handle(&command, output)? {
                Flow::Continue => {}
                Flow::Close => return Ok(Outcome::Closed),
                Flow::UpgradeToTls => return Ok(Outcome::UpgradeToTls),
            }
        }
    }

    /// Runs the session over a single duplex stream, such as a socket or a TLS stream.
    ///
    /// A TLS stream cannot be split into independent halves, so the buffered reader and
    /// the writer share one handle.
    ///
    /// # Errors
    ///
    /// As [`Self::run`].
    pub fn run_duplex<S: Read + Write>(&mut self, stream: &mut S) -> std::io::Result<Outcome> {
        let mut reader = BufReader::new(stream);
        if self.greet(reader.get_mut())? == Flow::Close {
            return Ok(Outcome::Closed);
        }
        self.command_loop_duplex(&mut reader)
    }

    /// Runs the command loop over a duplex stream *without* sending a greeting.
    ///
    /// Used for the phase after a `STARTTLS` handshake: RFC 4642 §2.2 says the server does
    /// not repeat its greeting, so sending one would desynchronise every client that
    /// follows the specification.
    ///
    /// # Errors
    ///
    /// As [`Self::run`].
    pub fn resume_duplex<S: Read + Write>(&mut self, stream: &mut S) -> std::io::Result<Outcome> {
        let mut reader = BufReader::new(stream);
        self.command_loop_duplex(&mut reader)
    }

    fn command_loop_duplex<S: Read + Write>(
        &mut self,
        reader: &mut BufReader<&mut S>,
    ) -> std::io::Result<Outcome> {
        let mut line = Vec::new();
        loop {
            line.clear();
            if reader.read_until(b'\n', &mut line)? == 0 {
                return Ok(Outcome::Closed);
            }

            let command = trim_eol(&line).to_vec();
            match self.handle(&command, reader.get_mut())? {
                Flow::Continue => {}
                Flow::Close => return Ok(Outcome::Closed),
                Flow::UpgradeToTls => {
                    // RFC 4642 §2.2 forbids the client from sending anything between the
                    // 382 and the handshake. Anything buffered here would be thrown away
                    // by the upgrade, so refuse rather than lose it silently.
                    if !reader.buffer().is_empty() {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "client sent data after STARTTLS before the handshake",
                        ));
                    }
                    return Ok(Outcome::UpgradeToTls);
                }
            }
        }
    }

    /// Sends the greeting.
    ///
    /// # Errors
    ///
    /// Propagates IO errors.
    pub fn greet(&mut self, output: &mut impl Write) -> std::io::Result<Flow> {
        match &self.config.greeting {
            GreetingMode::PostingAllowed => {
                let name = self.config.server_name.clone();
                self.status(output, 200, &format!("{name} ready, posting allowed"))?;
                Ok(Flow::Continue)
            }
            GreetingMode::NoPosting => {
                let name = self.config.server_name.clone();
                self.status(output, 201, &format!("{name} ready, posting prohibited"))?;
                Ok(Flow::Continue)
            }
            GreetingMode::Refuse { code, text } => {
                let (code, text) = (*code, text.clone());
                self.status(output, code, &text)?;
                Ok(Flow::Close)
            }
        }
    }

    /// Handles one command line.
    ///
    /// # Errors
    ///
    /// Propagates IO errors.
    pub fn handle(&mut self, line: &[u8], output: &mut impl Write) -> std::io::Result<Flow> {
        let flow = self.dispatch(line, output)?;
        if self.state.close_requested {
            // A block was written without its terminator. A real server in that state has
            // crashed or been killed, so the connection goes too: leaving it open would
            // make the client wait for a read timeout instead of seeing the truncation.
            return Ok(Flow::Close);
        }
        Ok(flow)
    }

    fn dispatch(&mut self, line: &[u8], output: &mut impl Write) -> std::io::Result<Flow> {
        self.state.commands_served += 1;
        if let Some(limit) = self.config.quirks.close_after_commands
            && self.state.commands_served > limit
        {
            // Vanish without a word, as an overloaded server does.
            return Ok(Flow::Close);
        }

        let text = String::from_utf8_lossy(line).into_owned();
        let mut tokens = text.split_ascii_whitespace();
        let verb = tokens.next().unwrap_or_default().to_ascii_uppercase();
        let args: Vec<&str> = tokens.collect();

        match verb.as_str() {
            "" => self
                .status(output, 500, "empty command")
                .map(|()| Flow::Continue),
            "QUIT" => {
                self.status(output, 205, "closing connection")?;
                Ok(Flow::Close)
            }
            "CAPABILITIES" => self.capabilities(output).map(|()| Flow::Continue),
            "MODE" => self.mode(output, &args).map(|()| Flow::Continue),
            "AUTHINFO" => self.authinfo(output, &args).map(|()| Flow::Continue),
            "DATE" => self.date(output).map(|()| Flow::Continue),
            "HELP" => self.help(output).map(|()| Flow::Continue),
            "STARTTLS" => self.starttls(output),
            _ if self.needs_auth() => self
                .status(output, 480, "authentication required")
                .map(|()| Flow::Continue),
            "GROUP" => self.group(output, &args).map(|()| Flow::Continue),
            "LISTGROUP" => self.listgroup(output, &args).map(|()| Flow::Continue),
            "LIST" => self.list(output, &args).map(|()| Flow::Continue),
            "ARTICLE" | "HEAD" | "BODY" | "STAT" => {
                self.fetch(output, &verb, &args).map(|()| Flow::Continue)
            }
            "OVER" | "XOVER" => self.over(output, &verb, &args).map(|()| Flow::Continue),
            "NEXT" | "LAST" => self.step(output, &verb).map(|()| Flow::Continue),
            other => self
                .status(output, 500, &format!("command {other} not recognised"))
                .map(|()| Flow::Continue),
        }
    }

    fn starttls(&mut self, output: &mut impl Write) -> std::io::Result<Flow> {
        if !self.config.starttls {
            self.status(output, 500, "command not recognised")?;
            return Ok(Flow::Continue);
        }
        if self.state.encrypted {
            // RFC 4642 §2.2: a second STARTTLS on the same connection is an error.
            self.status(output, 502, "TLS is already active")?;
            return Ok(Flow::Continue);
        }
        if self.state.authenticated {
            // §2.2 again: the upgrade must not be attempted after authentication.
            self.status(
                output,
                502,
                "STARTTLS is not permitted after authentication",
            )?;
            return Ok(Flow::Continue);
        }

        self.status(output, 382, "continue with TLS negotiation")?;
        Ok(Flow::UpgradeToTls)
    }

    fn needs_auth(&self) -> bool {
        self.config.require_auth && !self.state.authenticated
    }

    // -- individual commands ------------------------------------------------------------

    fn capabilities(&mut self, output: &mut impl Write) -> std::io::Result<()> {
        if self.config.capabilities == CapabilityProfile::Legacy {
            return self.status(output, 500, "command not recognised");
        }

        let mut lines: Vec<String> = vec!["VERSION 2".to_owned()];
        match self.config.capabilities {
            CapabilityProfile::Modern => {
                lines.push("READER".to_owned());
                lines.push("OVER MSGID".to_owned());
                lines.push("HDR".to_owned());
            }
            CapabilityProfile::NoOver => {
                lines.push("READER".to_owned());
            }
            CapabilityProfile::Transit if self.state.reader_mode => {
                lines.push("READER".to_owned());
                lines.push("OVER MSGID".to_owned());
            }
            CapabilityProfile::Transit => {
                lines.push("IHAVE".to_owned());
                lines.push("MODE-READER".to_owned());
            }
            CapabilityProfile::Legacy => {}
        }
        lines.push("LIST ACTIVE ACTIVE.TIMES NEWSGROUPS OVERVIEW.FMT".to_owned());
        if self.config.starttls && !self.state.encrypted {
            lines.push("STARTTLS".to_owned());
        }
        if self.config.credentials.is_some() {
            lines.push("AUTHINFO USER".to_owned());
        }
        lines.push(format!("IMPLEMENTATION nntp-testserver {}", crate::VERSION));

        self.status(output, 101, "capability list follows")?;
        self.block(output, lines.iter().map(String::as_bytes))
    }

    fn mode(&mut self, output: &mut impl Write, args: &[&str]) -> std::io::Result<()> {
        if !args
            .first()
            .is_some_and(|a| a.eq_ignore_ascii_case("READER"))
        {
            return self.status(output, 501, "MODE READER is the only mode supported");
        }
        if self.config.capabilities == CapabilityProfile::Legacy {
            return self.status(output, 500, "command not recognised");
        }

        self.state.reader_mode = true;
        match self.config.greeting {
            GreetingMode::NoPosting => self.status(output, 201, "reader mode, posting prohibited"),
            _ => self.status(output, 200, "reader mode, posting allowed"),
        }
    }

    fn authinfo(&mut self, output: &mut impl Write, args: &[&str]) -> std::io::Result<()> {
        let Some(credentials) = self.config.credentials.clone() else {
            return self.status(output, 502, "authentication is not available");
        };

        match args.first().map(|a| a.to_ascii_uppercase()).as_deref() {
            Some("USER") => {
                let user = args.get(1).unwrap_or(&"").to_string();
                self.state.pending_user = Some(user);
                self.status(output, 381, "password required")
            }
            Some("PASS") => {
                let password = args.get(1).unwrap_or(&"");
                let user_ok = self.state.pending_user.as_deref() == Some(&credentials.username);

                if self.state.pending_user.is_none() {
                    return self.status(output, 482, "AUTHINFO USER required first");
                }
                if user_ok && *password == credentials.password {
                    self.state.authenticated = true;
                    self.status(output, 281, "authentication accepted")
                } else {
                    self.state.pending_user = None;
                    self.status(output, 481, "authentication rejected")
                }
            }
            _ => self.status(output, 501, "AUTHINFO USER or AUTHINFO PASS"),
        }
    }

    fn date(&mut self, output: &mut impl Write) -> std::io::Result<()> {
        // A fixed clock: a test server with a real clock produces tests that change
        // behaviour at midnight.
        self.status(output, 111, "20260917080910")
    }

    fn help(&mut self, output: &mut impl Write) -> std::io::Result<()> {
        self.status(output, 100, "help text follows")?;

        if self.config.quirks.overlong_help_line {
            let monster = vec![b'x'; 200_000];
            return self.block(output, [monster.as_slice()]);
        }

        self.block(
            output,
            [
                &b"ARTICLE BODY CAPABILITIES DATE GROUP HEAD HELP"[..],
                b"LIST LISTGROUP MODE NEXT LAST OVER QUIT STAT XOVER",
            ],
        )
    }

    fn group(&mut self, output: &mut impl Write, args: &[&str]) -> std::io::Result<()> {
        let Some(name) = args.first() else {
            return self.status(output, 501, "GROUP needs a newsgroup name");
        };
        let Some(group) = self.corpus.find(name) else {
            // INN 2.8.0's exact wording, chosen deliberately: it carries no group name.
            // The previous message here began with the group name, which happened to match
            // what the client's parser assumed a 411 looked like -- so the fake server was
            // confirming the client's mistake instead of exposing it. A fake server must
            // not be more convenient than the real one.
            return self.status(output, 411, "No such newsgroup");
        };

        let (low, high) = group.watermarks();
        let count = group.articles.len();
        let name = group.name.clone();
        let first = group.low();

        self.state.current_group = Some(name.clone());
        self.state.current_article = first;
        self.status(output, 211, &format!("{count} {low} {high} {name}"))
    }

    fn listgroup(&mut self, output: &mut impl Write, args: &[&str]) -> std::io::Result<()> {
        let name = match args.first() {
            Some(name) => (*name).to_owned(),
            None => match &self.state.current_group {
                Some(name) => name.clone(),
                None => return self.status(output, 412, "no newsgroup selected"),
            },
        };

        let Some(group) = self.corpus.find(&name) else {
            return self.status(output, 411, "No such newsgroup");
        };

        let (low, high) = group.watermarks();
        let count = group.articles.len();
        let numbers: Vec<Vec<u8>> = group
            .articles
            .keys()
            .map(|number| number.to_string().into_bytes())
            .collect();

        self.state.current_group = Some(name.clone());
        self.status(output, 211, &format!("{count} {low} {high} {name}"))?;
        self.block(output, numbers.iter().map(Vec::as_slice))
    }

    fn list(&mut self, output: &mut impl Write, args: &[&str]) -> std::io::Result<()> {
        let keyword = args
            .first()
            .map_or_else(|| "ACTIVE".to_owned(), |k| k.to_ascii_uppercase());

        match keyword.as_str() {
            "ACTIVE" => {
                let lines: Vec<Vec<u8>> = self
                    .corpus
                    .groups()
                    .iter()
                    .map(|group| {
                        let (low, high) = group.watermarks();
                        format!("{} {} {} {}", group.name, high, low, group.posting.flag())
                            .into_bytes()
                    })
                    .collect();
                self.status(output, 215, "list of newsgroups follows")?;
                self.block(output, lines.iter().map(Vec::as_slice))
            }
            "NEWSGROUPS" => {
                let lines: Vec<Vec<u8>> = self
                    .corpus
                    .groups()
                    .iter()
                    .map(|group| {
                        let mut line = group.name.clone().into_bytes();
                        line.push(b'\t');
                        line.extend_from_slice(group.description.as_bytes());
                        line
                    })
                    .collect();
                self.status(output, 215, "descriptions follow")?;
                self.block(output, lines.iter().map(Vec::as_slice))
            }
            "ACTIVE.TIMES" => {
                let lines: Vec<Vec<u8>> = self
                    .corpus
                    .groups()
                    .iter()
                    .map(|group| {
                        format!("{} {} tester@test.invalid", group.name, group.created).into_bytes()
                    })
                    .collect();
                self.status(output, 215, "creation times follow")?;
                self.block(output, lines.iter().map(Vec::as_slice))
            }
            "OVERVIEW.FMT" => {
                if self.config.quirks.no_overview_fmt {
                    return self.status(output, 503, "overview format not available");
                }
                self.status(output, 215, "overview format follows")?;
                self.block(
                    output,
                    [
                        &b"Subject:"[..],
                        b"From:",
                        b"Date:",
                        b"Message-ID:",
                        b"References:",
                        b":bytes",
                        b":lines",
                        b"Xref:full",
                    ],
                )
            }
            other => self.status(output, 501, &format!("LIST {other} is not supported")),
        }
    }

    fn fetch(&mut self, output: &mut impl Write, verb: &str, args: &[&str]) -> std::io::Result<()> {
        let found = match args.first() {
            Some(arg) if arg.starts_with('<') => self
                .corpus
                .find_by_id(arg)
                .map(|(_, _, article)| (0u64, article.clone())),
            Some(arg) => {
                let Ok(number) = arg.parse::<u64>() else {
                    return self.status(output, 501, "bad article number");
                };
                match self.selected_group() {
                    None => return self.status(output, 412, "no newsgroup selected"),
                    Some(group) => group
                        .article_by_number(number)
                        .map(|article| (number, article.clone())),
                }
            }
            None => {
                let Some(number) = self.state.current_article else {
                    return self.status(output, 420, "no article selected");
                };
                match self.selected_group() {
                    None => return self.status(output, 412, "no newsgroup selected"),
                    Some(group) => group
                        .article_by_number(number)
                        .map(|article| (number, article.clone())),
                }
            }
        };

        let Some((number, article)) = found else {
            let by_id = args.first().is_some_and(|arg| arg.starts_with('<'));
            let code = if by_id { 430 } else { 423 };
            return self.status(output, code, "no such article");
        };

        if number != 0 {
            self.state.current_article = Some(number);
        }

        let id = article.message_id.clone();
        match verb {
            "STAT" => self.status(output, 223, &format!("{number} {id}")),
            "HEAD" => {
                self.status(output, 221, &format!("{number} {id} head follows"))?;
                self.block(output, article.head.iter().map(Vec::as_slice))
            }
            "BODY" => {
                self.status(output, 222, &format!("{number} {id} body follows"))?;
                self.block(output, article.body.iter().map(Vec::as_slice))
            }
            _ => {
                self.status(output, 220, &format!("{number} {id} article follows"))?;
                let lines: Vec<&[u8]> = article
                    .head
                    .iter()
                    .map(Vec::as_slice)
                    .chain([&b""[..]])
                    .chain(article.body.iter().map(Vec::as_slice))
                    .collect();
                self.block(output, lines)
            }
        }
    }

    fn over(&mut self, output: &mut impl Write, verb: &str, args: &[&str]) -> std::io::Result<()> {
        if verb == "OVER" && self.config.quirks.over_advertised_but_missing {
            return self.status(output, 500, "command not recognised");
        }
        if verb == "OVER" && self.config.capabilities == CapabilityProfile::NoOver {
            return self.status(output, 500, "command not recognised");
        }

        // The message-id form works without a selected group.
        if let Some(arg) = args.first()
            && arg.starts_with('<')
        {
            let Some((group, number, article)) = self.corpus.find_by_id(arg) else {
                return self.status(output, 430, "no such article");
            };
            let line = article.overview_line(number, &group.name);
            self.status(output, 224, "overview information follows")?;
            return self.block(output, [line.as_slice()]);
        }

        let Some(group) = self.selected_group().cloned() else {
            return self.status(output, 412, "no newsgroup selected");
        };

        let range = match args.first() {
            None => match self.state.current_article {
                Some(number) => (number, number),
                None => return self.status(output, 420, "no article selected"),
            },
            Some(arg) => match parse_range(arg) {
                None => return self.status(output, 501, "bad range"),
                Some((low, None)) => {
                    if self.config.quirks.reject_open_ended_ranges {
                        return self.status(output, 501, "open-ended ranges are not supported");
                    }
                    (low, group.high().unwrap_or(low))
                }
                Some((low, Some(high))) => (low, high),
            },
        };

        let lines: Vec<Vec<u8>> = group
            .articles
            .iter()
            .filter(|(number, _)| **number >= range.0 && **number <= range.1)
            .map(|(number, article)| article.overview_line(*number, &group.name))
            .collect();

        if lines.is_empty() {
            return self.status(output, 423, "no articles in that range");
        }

        self.status(output, 224, "overview information follows")?;
        self.block(output, lines.iter().map(Vec::as_slice))
    }

    fn step(&mut self, output: &mut impl Write, verb: &str) -> std::io::Result<()> {
        let Some(group) = self.selected_group().cloned() else {
            return self.status(output, 412, "no newsgroup selected");
        };
        let Some(current) = self.state.current_article else {
            return self.status(output, 420, "no article selected");
        };

        let next = if verb == "NEXT" {
            group.articles.range(current + 1..).next()
        } else {
            group.articles.range(..current).next_back()
        };

        match next {
            Some((number, article)) => {
                let (number, id) = (*number, article.message_id.clone());
                self.state.current_article = Some(number);
                self.status(output, 223, &format!("{number} {id}"))
            }
            None if verb == "NEXT" => self.status(output, 421, "no next article"),
            None => self.status(output, 422, "no previous article"),
        }
    }

    fn selected_group(&self) -> Option<&Group> {
        self.state
            .current_group
            .as_ref()
            .and_then(|name| self.corpus.find(name))
    }

    // -- output helpers -----------------------------------------------------------------

    fn status(&mut self, output: &mut impl Write, code: u16, text: &str) -> std::io::Result<()> {
        output.write_all(format!("{code} {text}").as_bytes())?;
        output.write_all(self.config.terminator())?;
        output.flush()
    }

    /// Writes a multi-line data block, dot-stuffing as RFC 3977 §3.1.1 requires.
    fn block<'l>(
        &mut self,
        output: &mut impl Write,
        lines: impl IntoIterator<Item = &'l [u8]>,
    ) -> std::io::Result<()> {
        let terminator = self.config.terminator();
        let truncate = std::mem::take(&mut self.state.truncate_next_block);

        if truncate {
            self.state.close_requested = true;
        }

        let delay = self.config.quirks.line_delay;

        for (index, line) in lines.into_iter().enumerate() {
            if let Some(delay) = delay {
                // Flush first: a client waiting for the previous line must actually have
                // it, or the delay would only fill the socket buffer and arrive in one
                // burst at the end, which is the opposite of what this simulates.
                output.flush()?;
                std::thread::sleep(delay);
            }
            if truncate && index >= 1 {
                // Stop without the terminator: exactly what a server that dies mid-block
                // leaves on the wire.
                output.flush()?;
                return Ok(());
            }
            if line.first() == Some(&b'.') {
                output.write_all(b".")?;
            }
            output.write_all(line)?;
            output.write_all(terminator)?;
        }

        if truncate {
            output.flush()?;
            return Ok(());
        }

        output.write_all(b".")?;
        output.write_all(terminator)?;
        output.flush()
    }
}

/// Parses `n`, `low-high` or `low-`.
fn parse_range(arg: &str) -> Option<(u64, Option<u64>)> {
    match arg.split_once('-') {
        None => arg.parse().ok().map(|n| (n, Some(n))),
        Some((low, "")) => low.parse().ok().map(|low| (low, None)),
        Some((low, high)) => Some((low.parse().ok()?, Some(high.parse().ok()?))),
    }
}

fn trim_eol(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Quirks;

    /// Runs a script of commands against a session and returns everything it wrote.
    fn converse(config: &ServerConfig, commands: &str) -> String {
        let corpus = Corpus::sample();
        let mut session = Session::new(&corpus, config);
        let mut input = std::io::Cursor::new(commands.as_bytes().to_vec());
        let mut output = Vec::new();
        session.run(&mut input, &mut output).unwrap();
        String::from_utf8_lossy(&output).into_owned()
    }

    fn default_conversation(commands: &str) -> String {
        converse(&ServerConfig::new(), commands)
    }

    #[test]
    fn greets_and_quits() {
        let transcript = default_conversation("QUIT\r\n");
        assert!(transcript.starts_with("200 test.invalid ready, posting allowed\r\n"));
        assert!(transcript.ends_with("205 closing connection\r\n"));
    }

    #[test]
    fn stops_reading_after_quit() {
        let transcript = default_conversation("QUIT\r\nDATE\r\n");
        assert!(!transcript.contains("111"));
    }

    #[test]
    fn a_refusing_greeting_closes_immediately() {
        let config = ServerConfig::new().greeting(GreetingMode::Refuse {
            code: 502,
            text: "access denied".to_owned(),
        });
        let transcript = converse(&config, "DATE\r\n");
        assert_eq!(transcript, "502 access denied\r\n");
    }

    #[test]
    fn advertises_capabilities() {
        let transcript = default_conversation("CAPABILITIES\r\nQUIT\r\n");
        assert!(transcript.contains("101 capability list follows\r\n"));
        assert!(transcript.contains("\r\nREADER\r\n"));
        assert!(transcript.contains("\r\nOVER MSGID\r\n"));
        assert!(transcript.contains("\r\n.\r\n"));
    }

    #[test]
    fn a_legacy_server_has_no_capabilities_command() {
        let config = ServerConfig::new().capabilities(CapabilityProfile::Legacy);
        let transcript = converse(&config, "CAPABILITIES\r\nQUIT\r\n");
        assert!(transcript.contains("500 command not recognised"));
    }

    #[test]
    fn a_transit_server_advertises_reader_only_after_mode_reader() {
        let config = ServerConfig::new().capabilities(CapabilityProfile::Transit);
        let transcript = converse(
            &config,
            "CAPABILITIES\r\nMODE READER\r\nCAPABILITIES\r\nQUIT\r\n",
        );

        let (before, after) = transcript.split_once("200 reader mode").unwrap();
        assert!(before.contains("MODE-READER"));
        assert!(!before.contains("\r\nREADER\r\n"));
        assert!(after.contains("\r\nREADER\r\n"));
    }

    #[test]
    fn selects_a_group_and_reports_its_watermarks() {
        let transcript = default_conversation("GROUP comp.lang.rust\r\nQUIT\r\n");
        assert!(transcript.contains("211 2 4237 4242 comp.lang.rust\r\n"));
    }

    #[test]
    fn an_empty_group_reports_high_zero_low_one() {
        let transcript = default_conversation("GROUP empty.group\r\nQUIT\r\n");
        assert!(transcript.contains("211 0 1 0 empty.group\r\n"));
    }

    #[test]
    fn rejects_an_unknown_group() {
        let transcript = default_conversation("GROUP no.such.group\r\nQUIT\r\n");
        // INN's wording, which carries no group name. A client that reads the group name
        // out of this response gets "No".
        assert!(transcript.contains("411 No such newsgroup"), "{transcript}");
        assert!(
            !transcript.contains("411 no.such.group"),
            "the fake server must not name the group in a 411, because INN does not:\n{transcript}"
        );
    }

    #[test]
    fn dot_stuffs_a_body_line_that_starts_with_a_dot() {
        let transcript = default_conversation("GROUP misc.test\r\nARTICLE 2\r\nQUIT\r\n");
        // The article contains ".signature-like ..."; on the wire it must be doubled.
        assert!(
            transcript.contains("\r\n...signature-like line that starts with a dot\r\n")
                || transcript.contains("\r\n..signature-like line that starts with a dot\r\n"),
            "transcript: {transcript}"
        );
        // Precisely: one extra dot, so the line begins with exactly two.
        assert!(transcript.contains("\r\n..signature-like"));
        assert!(!transcript.contains("\r\n...signature-like"));
    }

    #[test]
    fn serves_an_article_by_message_id_without_a_group() {
        let transcript = default_conversation("ARTICLE <qp@test.invalid>\r\nQUIT\r\n");
        // Article number 0 means "not applicable".
        assert!(transcript.contains("220 0 <qp@test.invalid> article follows\r\n"));
        assert!(transcript.contains("Subject: =?UTF-8?Q?caf=C3=A9_and_crates?=\r\n"));
    }

    #[test]
    fn separates_head_from_body_with_a_blank_line() {
        let transcript = default_conversation("GROUP misc.test\r\nARTICLE 1\r\nQUIT\r\n");
        assert!(transcript.contains("\r\n\r\nThis is the first article.\r\n"));
    }

    #[test]
    fn head_and_body_return_only_their_half() {
        let head = default_conversation("GROUP misc.test\r\nHEAD 1\r\nQUIT\r\n");
        assert!(head.contains("221 1 <root@test.invalid> head follows"));
        assert!(head.contains("Subject: A plain test article"));
        assert!(!head.contains("This is the first article."));

        let body = default_conversation("GROUP misc.test\r\nBODY 1\r\nQUIT\r\n");
        assert!(body.contains("222 1 <root@test.invalid> body follows"));
        assert!(body.contains("This is the first article."));
        assert!(!body.contains("Subject:"));
    }

    #[test]
    fn stat_reports_without_transferring() {
        let transcript = default_conversation("GROUP misc.test\r\nSTAT 1\r\nQUIT\r\n");
        assert!(transcript.contains("223 1 <root@test.invalid>"));
        assert!(!transcript.contains("Subject:"));
    }

    #[test]
    fn reports_a_missing_article_by_number_and_by_id() {
        let by_number = default_conversation("GROUP misc.test\r\nARTICLE 999\r\nQUIT\r\n");
        assert!(by_number.contains("423 no such article"));

        let by_id = default_conversation("ARTICLE <nope@test.invalid>\r\nQUIT\r\n");
        assert!(by_id.contains("430 no such article"));
    }

    #[test]
    fn refuses_an_article_number_with_no_group_selected() {
        let transcript = default_conversation("ARTICLE 1\r\nQUIT\r\n");
        assert!(transcript.contains("412 no newsgroup selected"));
    }

    #[test]
    fn overview_returns_only_the_articles_that_exist_in_the_range() {
        // comp.lang.rust holds 4237 and 4242 only.
        let transcript = default_conversation("GROUP comp.lang.rust\r\nOVER 4237-4242\r\nQUIT\r\n");
        assert!(transcript.contains("224 overview information follows"));
        assert_eq!(transcript.matches("Xref: test.invalid").count(), 2);
    }

    #[test]
    fn overview_honours_an_open_ended_range() {
        let transcript = default_conversation("GROUP comp.lang.rust\r\nOVER 4238-\r\nQUIT\r\n");
        assert_eq!(transcript.matches("Xref: test.invalid").count(), 1);
    }

    #[test]
    fn a_server_that_rejects_open_ended_ranges_says_so() {
        let config = ServerConfig::new().quirks(Quirks {
            reject_open_ended_ranges: true,
            ..Quirks::default()
        });
        let transcript = converse(&config, "GROUP misc.test\r\nOVER 1-\r\nQUIT\r\n");
        assert!(transcript.contains("501 open-ended ranges are not supported"));
    }

    #[test]
    fn a_server_without_over_only_answers_xover() {
        let config = ServerConfig::new().capabilities(CapabilityProfile::NoOver);
        let transcript = converse(
            &config,
            "GROUP misc.test\r\nOVER 1-3\r\nXOVER 1-3\r\nQUIT\r\n",
        );
        let (before_xover, after_xover) =
            transcript.split_once("XOVER").unwrap_or((&transcript, ""));
        let _ = after_xover;
        assert!(before_xover.contains("500 command not recognised"));
        assert!(transcript.contains("224 overview information follows"));
    }

    #[test]
    fn over_works_by_message_id_without_a_group() {
        let transcript = default_conversation("OVER <qp@test.invalid>\r\nQUIT\r\n");
        assert!(transcript.contains("224 overview information follows"));
        assert!(transcript.contains("comp.lang.rust:4237"));
    }

    #[test]
    fn lists_groups_with_high_before_low() {
        let transcript = default_conversation("LIST\r\nQUIT\r\n");
        assert!(transcript.contains("comp.lang.rust 4242 4237 y\r\n"));
        assert!(transcript.contains("de.comp.test 1 1 m\r\n"));
        assert!(transcript.contains("empty.group 0 1 y\r\n"));
    }

    #[test]
    fn lists_descriptions_separated_by_a_tab() {
        let transcript = default_conversation("LIST NEWSGROUPS\r\nQUIT\r\n");
        assert!(transcript.contains("misc.test\tFor testing purposes only\r\n"));
    }

    #[test]
    fn lists_the_overview_format_unless_told_not_to() {
        let transcript = default_conversation("LIST OVERVIEW.FMT\r\nQUIT\r\n");
        assert!(transcript.contains("Xref:full\r\n"));

        let config = ServerConfig::new().quirks(Quirks {
            no_overview_fmt: true,
            ..Quirks::default()
        });
        let refused = converse(&config, "LIST OVERVIEW.FMT\r\nQUIT\r\n");
        assert!(refused.contains("503 overview format not available"));
    }

    #[test]
    fn listgroup_returns_the_numbers_that_exist() {
        let transcript = default_conversation("LISTGROUP comp.lang.rust\r\nQUIT\r\n");
        assert!(transcript.contains("\r\n4237\r\n4242\r\n.\r\n"));
    }

    #[test]
    fn next_and_last_walk_the_sparse_numbering() {
        let transcript = default_conversation(
            "GROUP comp.lang.rust\r\nSTAT\r\nNEXT\r\nNEXT\r\nLAST\r\nQUIT\r\n",
        );
        assert!(transcript.contains("223 4237 <qp@test.invalid>"));
        assert!(transcript.contains("223 4242 <b64@test.invalid>"));
        assert!(transcript.contains("421 no next article"));
    }

    #[test]
    fn requires_authentication_when_configured() {
        let config = ServerConfig::new().require_auth("bob", "hunter2");
        let transcript = converse(&config, "GROUP misc.test\r\nQUIT\r\n");
        assert!(transcript.contains("480 authentication required"));

        // DATE and CAPABILITIES stay available before authentication, as RFC 4643 allows.
        let probe = converse(&config, "DATE\r\nQUIT\r\n");
        assert!(probe.contains("111 20260917080910"));
    }

    #[test]
    fn accepts_the_configured_credentials() {
        let config = ServerConfig::new().require_auth("bob", "hunter2");
        let transcript = converse(
            &config,
            "AUTHINFO USER bob\r\nAUTHINFO PASS hunter2\r\nGROUP misc.test\r\nQUIT\r\n",
        );
        assert!(transcript.contains("381 password required"));
        assert!(transcript.contains("281 authentication accepted"));
        assert!(transcript.contains("211 3 1 3 misc.test"));
    }

    #[test]
    fn rejects_wrong_credentials_and_out_of_order_commands() {
        let config = ServerConfig::new().require_auth("bob", "hunter2");

        let wrong = converse(
            &config,
            "AUTHINFO USER bob\r\nAUTHINFO PASS nope\r\nQUIT\r\n",
        );
        assert!(wrong.contains("481 authentication rejected"));

        let out_of_order = converse(&config, "AUTHINFO PASS hunter2\r\nQUIT\r\n");
        assert!(out_of_order.contains("482 AUTHINFO USER required first"));
    }

    #[test]
    fn closes_the_connection_mid_session_when_asked_to() {
        let config = ServerConfig::new().quirks(Quirks {
            close_after_commands: Some(1),
            ..Quirks::default()
        });
        let transcript = converse(&config, "DATE\r\nDATE\r\nQUIT\r\n");
        assert_eq!(transcript.matches("111 ").count(), 1);
        assert!(!transcript.contains("205"));
    }

    #[test]
    fn truncates_a_block_and_then_hangs_up() {
        let config = ServerConfig::new().quirks(Quirks {
            truncate_next_block: true,
            ..Quirks::default()
        });
        let transcript = converse(&config, "LIST\r\nQUIT\r\n");

        assert!(transcript.contains("215 list of newsgroups follows"));
        // No terminator line, which is what makes the client report a truncated response.
        assert!(!transcript.contains("\r\n.\r\n"));
        // And the session ends, so the client sees end-of-stream rather than waiting for
        // a read timeout. A server that can no longer finish a block has crashed.
        assert!(!transcript.contains("205"));
    }

    #[test]
    fn can_speak_bare_lf() {
        let config = ServerConfig::new().quirks(Quirks {
            bare_lf: true,
            ..Quirks::default()
        });
        let transcript = converse(&config, "DATE\nQUIT\n");
        assert!(transcript.contains("111 20260917080910\n"));
        assert!(!transcript.contains('\r'));
    }

    #[test]
    fn unknown_commands_get_500() {
        let transcript = default_conversation("FROBNICATE now\r\nQUIT\r\n");
        assert!(transcript.contains("500 command FROBNICATE not recognised"));
    }

    #[test]
    fn command_verbs_are_case_insensitive() {
        let transcript = default_conversation("date\r\nQuIt\r\n");
        assert!(transcript.contains("111 "));
        assert!(transcript.contains("205 "));
    }

    #[test]
    fn parses_the_three_range_forms() {
        assert_eq!(parse_range("42"), Some((42, Some(42))));
        assert_eq!(parse_range("1-10"), Some((1, Some(10))));
        assert_eq!(parse_range("7-"), Some((7, None)));
        assert_eq!(parse_range("x"), None);
        assert_eq!(parse_range("1-x"), None);
    }
}
