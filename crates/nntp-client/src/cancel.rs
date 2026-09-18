//! Asking a blocking read to stop.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// A flag one thread sets and another notices, to abandon a response in progress.
///
/// # Why this cannot be a message
///
/// The obvious design is a `Request::Cancel` on the channel the worker already reads. It
/// does not work: the channel is first-in-first-out and the worker is *inside* the
/// request being cancelled, so it would not see the message until that request finished —
/// which is exactly the wait the user is trying to escape. Cancellation has to be shared
/// state the reader can look at without returning to its mailbox.
///
/// # What it costs
///
/// A cancelled response leaves the connection pointing into the middle of a reply, so the
/// connection is marked desynchronised and must be discarded. That is deliberate:
/// [`crate::connection::Connection::read_block_streaming`] otherwise guarantees it always
/// reads to the terminator, precisely so that stopping early cannot desynchronise
/// anything, and pretending a half-read connection is fine would trade a slow reader for
/// a reader that answers the wrong question. Reconnecting costs one round trip and, on an
/// authenticated server, one more.
///
/// # Granularity
///
/// The flag is checked once per line of a data block, so cancelling a response that is
/// still arriving takes effect within one line. It is *not* checked while blocked waiting
/// for bytes that never come: a server that has gone silent is ended by the read timeout,
/// not by this.
#[derive(Debug, Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    /// A flag that has not been raised.
    pub fn new() -> Self {
        Self::default()
    }

    /// Asks whatever is reading to stop.
    ///
    /// Safe to call from any thread, and safe to call when nothing is reading: the flag
    /// stays raised until [`Self::reset`], so the caller does not have to know whether it
    /// won a race with the read starting.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// Lowers the flag, ready for the next request.
    pub fn reset(&self) {
        self.0.store(false, Ordering::SeqCst);
    }

    /// Whether a stop has been asked for.
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    /// Lowers the flag and reports whether it had been raised.
    ///
    /// The single operation a reader wants: it needs to know *and* to clear, and doing
    /// those separately leaves a window where a cancellation arriving between the two is
    /// lost.
    pub fn take(&self) -> bool {
        self.0.swap(false, Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_flag_is_not_raised() {
        assert!(!Cancel::new().is_cancelled());
    }

    #[test]
    fn raising_and_lowering() {
        let cancel = Cancel::new();
        cancel.cancel();
        assert!(cancel.is_cancelled());
        cancel.reset();
        assert!(!cancel.is_cancelled());
    }

    #[test]
    fn a_clone_shares_the_flag() {
        // The whole point: the run loop holds one and the worker another.
        let held_by_the_interface = Cancel::new();
        let held_by_the_worker = held_by_the_interface.clone();

        held_by_the_interface.cancel();

        assert!(held_by_the_worker.is_cancelled());
    }

    #[test]
    fn taking_reports_once_and_clears() {
        let cancel = Cancel::new();
        cancel.cancel();

        assert!(cancel.take(), "the first take sees it");
        assert!(!cancel.take(), "and it is lowered afterwards");
    }

    #[test]
    fn a_cancellation_raised_before_anything_reads_is_not_lost() {
        // A user can press the key in the gap between the request being sent and the
        // worker reaching the socket. The flag latches rather than being an edge, so the
        // read that starts afterwards still sees it.
        let cancel = Cancel::new();
        cancel.cancel();
        let seen_by_a_later_reader = cancel.clone();

        assert!(seen_by_a_later_reader.is_cancelled());
    }

    #[test]
    fn it_can_be_cancelled_from_another_thread_while_a_reader_spins() {
        let cancel = Cancel::new();
        let from_elsewhere = cancel.clone();

        let waiting = std::thread::spawn(move || {
            // Stands in for the read loop's per-line check.
            while !cancel.is_cancelled() {
                std::thread::yield_now();
            }
            "stopped"
        });

        from_elsewhere.cancel();

        assert_eq!(waiting.join().expect("the reader thread"), "stopped");
    }
}
