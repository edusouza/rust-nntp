//! Framing: turning a byte stream into status lines and data blocks.
//!
//! This is the only layer that touches the socket. It is generic over `Read + Write` so
//! the same code runs against a TCP socket, a TLS stream and an in-memory cursor, which is
//! what makes the framing testable without a network.

use std::io::{BufRead, BufReader, Read, Write};

use nntp_proto::block::{self, DataBlock};
use nntp_proto::{Command, StatusLine};

use crate::cancel::Cancel;
use crate::error::{ClientError, Result};
use crate::limits::Limits;

/// How many octets to buffer from the socket at a time.
const READ_BUFFER_SIZE: usize = 64 * 1024;

/// A framed NNTP connection.
///
/// Holds no protocol state beyond the framing itself: whether a group is selected, what
/// the server can do and whether the session is authenticated all live in
/// [`crate::Client`].
#[derive(Debug)]
pub struct Connection<S> {
    stream: BufReader<S>,
    limits: Limits,
    /// Set when the stream position is no longer known — for instance after a line-length
    /// violation, where the rest of the over-long line is still queued. Any further use
    /// would read one response as another, so it is refused.
    desynchronised: bool,
    /// Checked once per line of a data block, so a caller on another thread can abandon a
    /// long response. `None` means nothing can cancel this connection.
    cancel: Option<Cancel>,
}

impl<S: Read + Write> Connection<S> {
    /// Wraps a stream with the default limits.
    pub fn new(stream: S) -> Self {
        Self::with_limits(stream, Limits::default())
    }

    /// Wraps a stream with explicit limits.
    pub fn with_limits(stream: S, limits: Limits) -> Self {
        Self {
            stream: BufReader::with_capacity(READ_BUFFER_SIZE, stream),
            limits,
            desynchronised: false,
            cancel: None,
        }
    }

    /// Lets `cancel` abandon a data block this connection is reading.
    ///
    /// Without one, a response is always read to its terminator. With one, a raised flag
    /// stops the read within a line and leaves the connection desynchronised, because
    /// stopping mid-response is exactly what desynchronised means. See [`Cancel`].
    pub fn set_cancel(&mut self, cancel: Cancel) {
        self.cancel = Some(cancel);
    }

    /// The flag that can cancel this connection's reads, if one was given.
    pub fn cancel_flag(&self) -> Option<&Cancel> {
        self.cancel.as_ref()
    }

    /// The limits in force.
    pub fn limits(&self) -> Limits {
        self.limits
    }

    /// Whether the connection has lost track of the stream and must be discarded.
    pub fn is_desynchronised(&self) -> bool {
        self.desynchronised
    }

    /// Access to the underlying stream, for transport-level operations such as setting a
    /// timeout or starting a TLS handshake.
    pub fn get_mut(&mut self) -> &mut S {
        self.stream.get_mut()
    }

    /// Unwraps the connection.
    ///
    /// Any bytes already read into the buffer are discarded, so this must not be called
    /// mid-response. It is used to hand the socket to a TLS handshake, where RFC 4642
    /// §2.2 requires that no data be pending.
    pub fn into_inner(self) -> S {
        self.stream.into_inner()
    }

    /// Whether the read buffer holds unconsumed data.
    ///
    /// Checked before a `STARTTLS` handshake: buffered plaintext after the `382` response
    /// would mean the server sent data that must not exist, and continuing would hand
    /// injected bytes to the TLS layer.
    pub fn has_buffered_data(&self) -> bool {
        !self.stream.buffer().is_empty()
    }

    /// Sends a command.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::Proto`] if the command cannot be encoded — which is how a
    /// CRLF-injection attempt surfaces — and [`ClientError::Io`] if the write fails.
    pub fn send(&mut self, command: &Command) -> Result<()> {
        self.ensure_synchronised()?;

        let bytes = command.to_wire()?;
        tracing::trace!(command = %command.to_log_string(), ">>");

        self.stream
            .get_mut()
            .write_all(&bytes)
            .map_err(ClientError::from_io)?;
        self.stream
            .get_mut()
            .flush()
            .map_err(ClientError::from_io)?;
        Ok(())
    }

    /// Sends a multi-line data block: the lines, dot-stuffed, then the terminator.
    ///
    /// The mirror of [`Self::read_block`], and the dot-stuffing is the whole reason it is
    /// a method rather than three lines at the call site. A body line that begins with `.`
    /// — a `.signature`, a quoted shell prompt, an ASCII drawing — terminates the article
    /// early unless it is doubled (RFC 3977 §3.1.1), which truncates what everybody else
    /// receives and is invisible to the sender.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::Io`] if the write fails. The caller reads the response.
    pub fn send_block(&mut self, lines: &[Vec<u8>]) -> Result<()> {
        self.ensure_synchronised()?;

        let mut out = Vec::new();
        for line in lines {
            out.extend_from_slice(&nntp_proto::block::stuff(line));
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(b".\r\n");

        tracing::trace!(lines = lines.len(), bytes = out.len(), ">> block");

        self.stream
            .get_mut()
            .write_all(&out)
            .map_err(ClientError::from_io)?;
        self.stream
            .get_mut()
            .flush()
            .map_err(ClientError::from_io)?;
        Ok(())
    }

    /// Reads a status line.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::ConnectionClosed`] at end of stream,
    /// [`ClientError::LineTooLong`] past the configured limit, and
    /// [`ClientError::Proto`] if the line is not a status line.
    pub fn read_status(&mut self) -> Result<StatusLine> {
        let line = self.read_line("a status line")?;
        tracing::trace!(status = %String::from_utf8_lossy(&line), "<<");
        Ok(StatusLine::parse(&line)?)
    }

    /// Sends a command and reads its status line.
    ///
    /// The status line is returned whatever the code, including errors: deciding which
    /// codes are acceptable belongs to the caller, because `500` means "fall back to
    /// `XOVER`" in one place and "this server is unusable" in another.
    ///
    /// # Errors
    ///
    /// As [`Self::send`] and [`Self::read_status`].
    pub fn command(&mut self, command: &Command) -> Result<StatusLine> {
        self.send(command)?;
        self.read_status()
    }

    /// Reads a multi-line data block into memory.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::BlockTooLarge`] if the block exceeds the configured limits,
    /// and the errors of [`Self::read_status`] for the individual lines.
    pub fn read_block(&mut self) -> Result<DataBlock> {
        let mut lines = Vec::new();
        self.read_block_streaming(|line| lines.push(line.to_vec()))?;
        Ok(DataBlock::from_lines(lines))
    }

    /// Reads a multi-line data block, passing each line to `on_line` as it arrives.
    ///
    /// Lines are already dot-unstuffed and have no line terminator. The block is otherwise
    /// always read to its terminator even if the caller ignores the content, because
    /// stopping early would leave the connection pointing into the middle of a response.
    ///
    /// The one exception is cancellation: if a [`Cancel`] was given to
    /// [`Self::set_cancel`] and it has been raised, the read stops and the connection is
    /// marked desynchronised. The cost is explicit rather than hidden — a cancelled
    /// connection is finished, and its owner has to discard it.
    ///
    /// Returns the number of lines read.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::BlockTooLarge`] if the configured line count or octet count
    /// is exceeded, [`ClientError::Cancelled`] if the flag was raised, and
    /// [`ClientError::ConnectionClosed`] if the stream ends before the terminator arrives.
    pub fn read_block_streaming(&mut self, mut on_line: impl FnMut(&[u8])) -> Result<usize> {
        let mut lines = 0usize;
        let mut bytes = 0usize;

        loop {
            // Before the read rather than after it: a flag raised while the previous line
            // was being handed to `on_line` should stop this block, not buy another line.
            if self.cancel.as_ref().is_some_and(Cancel::is_cancelled) {
                tracing::debug!(lines, bytes, "<< cancelled mid-block");
                self.desynchronised = true;
                return Err(ClientError::Cancelled {
                    what: "a data block",
                });
            }

            let line = self.read_line("a data block")?;
            if block::is_terminator(&line) {
                tracing::trace!(lines, bytes, "<< end of block");
                return Ok(lines);
            }

            lines += 1;
            bytes += line.len();
            if lines > self.limits.max_block_lines {
                self.desynchronised = true;
                return Err(ClientError::BlockTooLarge {
                    what: "block line count",
                    limit: self.limits.max_block_lines,
                });
            }
            if bytes > self.limits.max_block_bytes {
                self.desynchronised = true;
                return Err(ClientError::BlockTooLarge {
                    what: "block size in octets",
                    limit: self.limits.max_block_bytes,
                });
            }

            on_line(block::unstuff(&line));
        }
    }

    /// Reads one line, stripping its terminator and enforcing the length limit.
    fn read_line(&mut self, context: &'static str) -> Result<Vec<u8>> {
        self.ensure_synchronised()?;

        let mut line = Vec::new();

        loop {
            let available = self.stream.fill_buf().map_err(ClientError::from_io)?;
            if available.is_empty() {
                // End of stream. A partial line here is a truncated response, not a
                // clean close, but either way the connection is finished.
                self.desynchronised = true;
                return Err(ClientError::ConnectionClosed(Some(context)));
            }

            match memchr::memchr(b'\n', available) {
                Some(index) => {
                    let (head, _) = available.split_at(index);
                    line.extend_from_slice(head);
                    // Consume the newline as well as the content.
                    self.stream.consume(index + 1);
                    break;
                }
                None => {
                    let consumed = available.len();
                    line.extend_from_slice(available);
                    self.stream.consume(consumed);
                }
            }

            if line.len() > self.limits.max_line_len {
                // The rest of the line is still on the wire, so the stream position is
                // no longer known.
                self.desynchronised = true;
                return Err(ClientError::LineTooLong {
                    limit: self.limits.max_line_len,
                });
            }
        }

        if line.len() > self.limits.max_line_len {
            self.desynchronised = true;
            return Err(ClientError::LineTooLong {
                limit: self.limits.max_line_len,
            });
        }

        // A bare LF is accepted although the RFC requires CRLF; rejecting it would break
        // against servers that are otherwise perfectly usable.
        if line.last() == Some(&b'\r') {
            line.pop();
        }

        Ok(line)
    }

    fn ensure_synchronised(&self) -> Result<()> {
        if self.desynchronised {
            return Err(ClientError::ConnectionClosed(Some(
                "a connection that lost protocol synchronisation",
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use nntp_proto::spec::ArticleSpec;

    use super::*;

    /// A stream that reads from a script and records what was written.
    struct Fake {
        input: Cursor<Vec<u8>>,
        written: Vec<u8>,
    }

    impl Fake {
        fn new(script: &[u8]) -> Self {
            Self {
                input: Cursor::new(script.to_vec()),
                written: Vec::new(),
            }
        }
    }

    impl Read for Fake {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.input.read(buf)
        }
    }

    impl Write for Fake {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.written.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn connect(script: &[u8]) -> Connection<Fake> {
        Connection::new(Fake::new(script))
    }

    #[test]
    fn reads_a_status_line() {
        let mut conn = connect(b"200 ready\r\n");
        let status = conn.read_status().unwrap();
        assert_eq!(status.code.as_u16(), 200);
        assert_eq!(status.text, "ready");
    }

    #[test]
    fn writes_commands_with_crlf() {
        let mut conn = connect(b"205 bye\r\n");
        conn.command(&Command::Quit).unwrap();
        assert_eq!(conn.get_mut().written, b"QUIT\r\n");
    }

    #[test]
    fn refuses_to_send_a_command_it_cannot_encode() {
        let mut conn = connect(b"");
        let error = conn
            .send(&Command::AuthInfoUser("bob\r\nQUIT".to_owned()))
            .unwrap_err();
        assert!(matches!(error, ClientError::Proto(_)));
        // Nothing reached the socket.
        assert!(conn.get_mut().written.is_empty());
    }

    #[test]
    fn reads_a_data_block_and_unstuffs_it() {
        let mut conn = connect(b"215 list follows\r\nfirst\r\n..dotted\r\nlast\r\n.\r\n");
        assert_eq!(conn.read_status().unwrap().code.as_u16(), 215);
        let block = conn.read_block().unwrap();
        assert_eq!(
            block.lines(),
            [b"first".to_vec(), b".dotted".to_vec(), b"last".to_vec()]
        );
    }

    #[test]
    fn reads_an_empty_block() {
        let mut conn = connect(b".\r\n");
        assert!(conn.read_block().unwrap().is_empty());
    }

    #[test]
    fn streams_block_lines_without_buffering_them() {
        let mut conn = connect(b"a\r\nb\r\nc\r\n.\r\n");
        let mut seen = Vec::new();
        let count = conn
            .read_block_streaming(|line| seen.push(String::from_utf8_lossy(line).into_owned()))
            .unwrap();
        assert_eq!(count, 3);
        assert_eq!(seen, ["a", "b", "c"]);
    }

    #[test]
    fn continues_reading_the_next_response_after_a_block() {
        let mut conn = connect(b"215 ok\r\nx\r\n.\r\n211 3 1 3 misc.test\r\n");
        conn.read_status().unwrap();
        conn.read_block().unwrap();
        let next = conn.read_status().unwrap();
        assert_eq!(next.code.as_u16(), 211);
    }

    #[test]
    fn accepts_a_bare_lf_terminator() {
        let mut conn = connect(b"200 ready\nx\n.\n");
        assert_eq!(conn.read_status().unwrap().code.as_u16(), 200);
        assert_eq!(conn.read_block().unwrap().len(), 1);
    }

    #[test]
    fn handles_a_response_split_across_reads() {
        // Simulates TCP segmentation: the fake stream returns whatever the BufReader
        // asks for, so use a script long enough to span several fills.
        let long = "x".repeat(READ_BUFFER_SIZE * 2 + 7);
        let script = format!("200 {long}\r\n");
        let mut conn = Connection::with_limits(Fake::new(script.as_bytes()), Limits::UNLIMITED);
        let status = conn.read_status().unwrap();
        assert_eq!(status.text.len(), long.len());
    }

    #[test]
    fn reports_a_closed_connection() {
        let mut conn = connect(b"");
        let error = conn.read_status().unwrap_err();
        assert!(matches!(error, ClientError::ConnectionClosed(Some(_))));
        assert!(error.is_connection_fatal());
    }

    #[test]
    fn reports_a_truncated_block() {
        // No terminator: the server died mid-response.
        let mut conn = connect(b"one\r\ntwo\r\n");
        let error = conn.read_block().unwrap_err();
        assert!(matches!(error, ClientError::ConnectionClosed(_)));
    }

    #[test]
    fn enforces_the_line_length_limit() {
        let limits = Limits {
            max_line_len: 16,
            ..Limits::DEFAULT
        };
        let script = format!("200 {}\r\n", "x".repeat(100));
        let mut conn = Connection::with_limits(Fake::new(script.as_bytes()), limits);
        let error = conn.read_status().unwrap_err();
        assert!(matches!(error, ClientError::LineTooLong { limit: 16 }));
    }

    #[test]
    fn enforces_the_block_line_count_limit() {
        let limits = Limits {
            max_block_lines: 2,
            ..Limits::DEFAULT
        };
        let mut conn = Connection::with_limits(Fake::new(b"a\r\nb\r\nc\r\n.\r\n"), limits);
        let error = conn.read_block().unwrap_err();
        assert!(matches!(
            error,
            ClientError::BlockTooLarge {
                what: "block line count",
                limit: 2
            }
        ));
    }

    #[test]
    fn enforces_the_block_size_limit() {
        let limits = Limits {
            max_block_bytes: 4,
            ..Limits::DEFAULT
        };
        let mut conn = Connection::with_limits(Fake::new(b"aaa\r\nbbb\r\n.\r\n"), limits);
        let error = conn.read_block().unwrap_err();
        assert!(matches!(
            error,
            ClientError::BlockTooLarge {
                what: "block size in octets",
                ..
            }
        ));
    }

    #[test]
    fn a_raised_cancel_flag_stops_a_block_within_a_line() {
        let mut conn = connect(b"a\r\nb\r\nc\r\nd\r\n.\r\n");
        let cancel = Cancel::new();
        conn.set_cancel(cancel.clone());

        let mut seen = Vec::new();
        let error = conn
            .read_block_streaming(|line| {
                seen.push(String::from_utf8_lossy(line).into_owned());
                // Ask to stop while handling the second line, as a keystroke would.
                if seen.len() == 2 {
                    cancel.cancel();
                }
            })
            .unwrap_err();

        assert!(matches!(error, ClientError::Cancelled { .. }));
        // Two lines were delivered and the third was never read: the check happens before
        // the read, so a flag raised during `on_line` does not buy another line.
        assert_eq!(seen, ["a", "b"]);
    }

    #[test]
    fn a_cancelled_block_poisons_the_connection() {
        // Stopping mid-response is what desynchronised means. The alternative — pretending
        // the connection is fine — would read the tail of this block as the next response.
        let mut conn = connect(b"a\r\nb\r\n.\r\n211 3 1 3 misc.test\r\n");
        let cancel = Cancel::new();
        conn.set_cancel(cancel.clone());
        cancel.cancel();

        let error = conn.read_block_streaming(|_| {}).unwrap_err();

        assert!(matches!(error, ClientError::Cancelled { .. }));
        assert!(error.is_cancelled());
        assert!(
            error.is_connection_fatal(),
            "the owner has to discard the connection"
        );
        assert!(conn.is_desynchronised());
        assert!(conn.read_status().is_err(), "and it refuses further use");
    }

    #[test]
    fn a_cancellation_raised_before_the_block_starts_reads_nothing() {
        let mut conn = connect(b"a\r\nb\r\n.\r\n");
        let cancel = Cancel::new();
        conn.set_cancel(cancel.clone());
        cancel.cancel();

        let mut seen = 0usize;
        let error = conn.read_block_streaming(|_| seen += 1).unwrap_err();

        assert!(matches!(error, ClientError::Cancelled { .. }));
        assert_eq!(seen, 0);
    }

    #[test]
    fn a_lowered_flag_does_not_interfere() {
        // The flag is shared and long-lived, so a connection holding one that nobody has
        // raised must behave exactly as one without.
        let mut conn = connect(b"a\r\nb\r\n.\r\n");
        conn.set_cancel(Cancel::new());

        assert_eq!(conn.read_block_streaming(|_| {}).unwrap(), 2);
        assert!(!conn.is_desynchronised());
    }

    #[test]
    fn without_a_flag_a_block_is_always_read_to_the_terminator() {
        // The contract the rest of the client relies on, kept for connections that never
        // got a flag.
        let mut conn = connect(b"a\r\nb\r\n.\r\n211 3 1 3 misc.test\r\n");
        assert!(conn.cancel_flag().is_none());

        assert_eq!(conn.read_block_streaming(|_| {}).unwrap(), 2);
        assert_eq!(conn.read_status().unwrap().code.as_u16(), 211);
    }

    #[test]
    fn a_limit_violation_poisons_the_connection() {
        // The rest of the over-long line is still queued, so the next read would return
        // the tail of it and be mistaken for a status line.
        let limits = Limits {
            max_line_len: 8,
            ..Limits::DEFAULT
        };
        let script = format!("200 {}\r\n211 3 1 3 g\r\n", "x".repeat(50));
        let mut conn = Connection::with_limits(Fake::new(script.as_bytes()), limits);
        assert!(conn.read_status().is_err());
        assert!(conn.is_desynchronised());

        let error = conn.read_status().unwrap_err();
        assert!(matches!(error, ClientError::ConnectionClosed(_)));
        // And it refuses to write anything further.
        assert!(conn.send(&Command::Quit).is_err());
    }

    #[test]
    fn reports_buffered_data_for_the_starttls_check() {
        let mut conn = connect(b"382 continue\r\nunexpected\r\n");
        assert!(!conn.has_buffered_data());
        conn.read_status().unwrap();
        // The extra line was read into the buffer along with the status line, which is
        // exactly the condition STARTTLS must refuse.
        assert!(conn.has_buffered_data());
    }

    #[test]
    fn exposes_the_limits_and_the_inner_stream() {
        let conn = connect(b"");
        assert_eq!(conn.limits(), Limits::DEFAULT);
        assert!(conn.into_inner().written.is_empty());
    }

    #[test]
    fn sends_an_article_command_with_its_argument() {
        let mut conn = connect(b"220 0 <a@b>\r\n.\r\n");
        conn.command(&Command::Article(ArticleSpec::Number(42)))
            .unwrap();
        assert_eq!(conn.get_mut().written, b"ARTICLE 42\r\n");
    }
}
