//! The network worker thread.
//!
//! Owns the client and does every blocking thing, so the render loop never waits on a
//! socket. It reconnects on demand: the interface stays usable after a dropped
//! connection, and the next request re-establishes it rather than the user having to
//! restart.

use std::sync::mpsc::{Receiver, Sender};
use std::thread::JoinHandle;

use nntp_client::{Cancel, Client, ClientError, Transport};
use nntp_proto::{ActiveEntry, GroupName, Range};

use crate::session::{self, Target};
use crate::tui::protocol::{Event, FetchToken, GroupRow, Request};

/// Starts the worker.
///
/// # Errors
///
/// Returns an error only if the thread cannot be spawned.
pub fn spawn(
    target: Target,
    overview_chunk: u64,
    requests: Receiver<Request>,
    events: Sender<Event>,
    cancel: Cancel,
) -> std::io::Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("nntp-worker".to_owned())
        .spawn(move || {
            Worker {
                target,
                overview_chunk: overview_chunk.max(1),
                client: None,
                events,
                cancel,
            }
            .run(&requests);
        })
}

struct Worker {
    target: Target,
    overview_chunk: u64,
    client: Option<Client<Transport>>,
    events: Sender<Event>,
    /// Raised by the interface to abandon the response being read.
    ///
    /// Shared state rather than a `Request`, because the request channel is
    /// first-in-first-out and this thread is *inside* the request being cancelled: a
    /// message would not be seen until the wait the user is escaping had ended.
    cancel: Cancel,
}

impl Worker {
    fn run(&mut self, requests: &Receiver<Request>) {
        // The render loop has gone if the channel closes, so there is nothing left to do.
        while let Ok(request) = requests.recv() {
            if matches!(request, Request::Shutdown) {
                break;
            }
            self.handle(request);
        }

        if let Some(client) = self.client.take() {
            let _ = client.quit();
        }
        let _ = self.events.send(Event::Stopped);
        tracing::debug!("worker stopped");
    }

    fn handle(&mut self, request: Request) {
        self.dispatch(request);

        // Cleared *after* the request, never before. Clearing first looks tidier and is
        // wrong: the interface raises the flag the moment the user presses the key, which
        // can be before this thread has taken the request off the channel — so a reset
        // here would throw away the cancellation and the keystroke would do nothing. A
        // flag still raised at this point was either noticed (and the request is already
        // reported as cancelled) or arrived too late to matter, and in both cases it must
        // not leak into whatever runs next.
        self.cancel.reset();
    }

    fn dispatch(&mut self, request: Request) {
        let outcome = match &request {
            Request::LoadGroups => self.load_groups(),
            Request::OpenGroup {
                group,
                count,
                token,
            } => self.open_group(group, *count, *token),
            Request::LoadOverview {
                group,
                range,
                token,
            } => self.load_overview(group, *range, *token),
            Request::LoadArticle { group, spec } => self.load_article(group.as_ref(), spec.clone()),
            Request::Shutdown => Ok(()),
        };

        if let Err(error) = outcome {
            let fatal = error.is_connection_fatal();
            let context = describe(&request);

            if error.is_cancelled() {
                // The user's own doing, so not a failure — and deliberately not a
                // `Disconnected` either, because the interface would show the connection
                // as lost when nothing is wrong with the server. The connection *is*
                // finished (it stopped mid-response), so it is dropped here and the next
                // request reconnects; the reconnection is visible as a progress line.
                tracing::debug!(%context, "request cancelled");
                self.client = None;
                self.send(Event::Cancelled { context });
                return;
            }

            tracing::warn!(%context, %error, fatal, "request failed");

            self.send(Event::Failed {
                context,
                message: error.to_string(),
            });

            if fatal {
                // The connection is unusable; drop it so the next request reconnects.
                self.client = None;
                self.send(Event::Disconnected(error.to_string()));
            }
        }
    }

    /// Returns a connected client, establishing the connection if necessary.
    fn client(&mut self) -> Result<&mut Client<Transport>, ClientError> {
        if self.client.is_none() {
            self.send(Event::Progress(format!(
                "connecting to {}…",
                self.target.authority()
            )));

            let client = session::connect(&self.target).map_err(|error| {
                // session::connect returns anyhow for its context chain; the interface
                // wants one flat message, and `{:#}` is what produces it.
                ClientError::Tls(format!("{error:#}"))
            })?;

            self.send(Event::Connected {
                server: self.target.authority(),
                greeting: client.greeting().text.clone(),
                encrypted: client.is_encrypted(),
            });

            let mut client = client;
            // Every connection this worker owns can be cancelled, including the ones it
            // makes after a cancellation dropped the last one.
            client.connection_mut().set_cancel(self.cancel.clone());
            self.client = Some(client);
        }

        // Just assigned above if it was absent.
        self.client
            .as_mut()
            .ok_or(ClientError::ConnectionClosed(Some(
                "a connection that vanished",
            )))
    }

    fn load_groups(&mut self) -> Result<(), ClientError> {
        self.send(Event::Progress("fetching the group list…".to_owned()));

        let mut active: Vec<ActiveEntry> = Vec::new();
        {
            let client = self.client()?;
            client.list_groups_streaming(None, |entry| {
                if let Ok(entry) = entry {
                    active.push(entry);
                }
            })?;
        }

        // Descriptions come from a second command and are optional: a server that refuses
        // LIST NEWSGROUPS is still perfectly usable, just less informative.
        //
        // A *refusal* is optional; a dropped connection is not. Swallowing the latter
        // would leave the worker holding an unusable client and the interface believing
        // it was still connected, so the group list would arrive and every command after
        // it would fail for no visible reason.
        let descriptions = match self.client()?.list_group_descriptions(None) {
            Ok(result) => result
                .entries
                .into_iter()
                .map(|entry| (entry.name, entry.description))
                .collect::<std::collections::BTreeMap<_, _>>(),
            Err(error) if error.is_connection_fatal() => return Err(error),
            Err(error) => {
                tracing::debug!(%error, "no group descriptions available");
                std::collections::BTreeMap::new()
            }
        };

        let rows = active
            .into_iter()
            .map(|entry| GroupRow {
                description: descriptions.get(&entry.name).cloned(),
                name: entry.name,
                low: entry.low,
                high: entry.high,
                status: entry.status,
            })
            .collect();

        self.send(Event::Groups(rows));
        Ok(())
    }

    fn open_group(
        &mut self,
        group: &GroupName,
        count: u64,
        token: FetchToken,
    ) -> Result<(), ClientError> {
        self.send(Event::Progress(format!("selecting {group}…")));

        let summary = self.client()?.select_group(group)?;
        self.send(Event::GroupOpened(Box::new(summary.clone())));

        let Some((low, high)) = summary.range() else {
            // An empty group is not an error; it just has nothing to list. The completion
            // is still sent, because the reader has to be able to tell an empty group from
            // one whose records have not arrived yet.
            self.send(Event::OverviewComplete {
                group: group.clone(),
                token,
            });
            return Ok(());
        };

        let first = high.saturating_sub(count.saturating_sub(1)).max(low);
        self.fetch_overview(group, Range::between(first, high), token)
    }

    fn load_overview(
        &mut self,
        group: &GroupName,
        range: Range,
        token: FetchToken,
    ) -> Result<(), ClientError> {
        // The client tracks which group is selected, but the worker may have reconnected
        // since, so select it again rather than assuming.
        let selected = self
            .client()?
            .selected_group()
            .map(|summary| summary.name.clone());
        if selected.as_ref() != Some(group) {
            self.client()?.select_group(group)?;
        }

        self.fetch_overview(group, range, token)
    }

    /// Fetches a range in chunks, sending each one as it arrives.
    ///
    /// Chunking keeps any single response bounded; sending each chunk is what puts
    /// articles on screen while the rest is still on the wire. On a group with a hundred
    /// thousand articles the difference is a pane that fills in a second rather than one
    /// that stays empty for a minute.
    ///
    /// **Newest chunk first.** The chunks are walked backwards because a reader wants the
    /// newest articles, so those are the ones worth showing first; fetching forwards would
    /// fill the pane with the oldest end of the range and leave what the user came for
    /// until last. The interface merges each chunk into the list it already holds, which
    /// is why arriving out of ascending order is not its problem.
    fn fetch_overview(
        &mut self,
        group: &GroupName,
        range: Range,
        token: FetchToken,
    ) -> Result<(), ClientError> {
        let chunks = range.chunks(self.overview_chunk);
        let total = chunks.len();
        let fmt = self.client()?.overview_format()?;

        for (index, chunk) in chunks.iter().rev().enumerate() {
            if total > 1 {
                self.send(Event::Progress(format!(
                    "{group}: fetching {} of {total}…",
                    index + 1
                )));
            }

            let client = self.client()?;
            let mut records = Vec::new();
            let mut skipped = 0usize;
            client.overview_streaming(*chunk, &fmt, |record| match record {
                Ok(record) => records.push(record),
                Err(_) => skipped += 1,
            })?;

            records.sort_by_key(|record| record.number);
            self.send(Event::OverviewChunk {
                group: group.clone(),
                token,
                records,
                skipped,
            });
        }

        self.send(Event::OverviewComplete {
            group: group.clone(),
            token,
        });
        Ok(())
    }

    fn load_article(
        &mut self,
        group: Option<&GroupName>,
        spec: nntp_proto::ArticleSpec,
    ) -> Result<(), ClientError> {
        if let Some(group) = group {
            let selected = self
                .client()?
                .selected_group()
                .map(|summary| summary.name.clone());
            if selected.as_ref() != Some(group) {
                self.client()?.select_group(group)?;
            }
        }

        self.send(Event::Progress("fetching the article…".to_owned()));
        let article = self.client()?.article(spec)?;
        self.send(Event::Article(Box::new(article)));
        Ok(())
    }

    /// Sends an event, ignoring a closed channel.
    ///
    /// A closed channel means the interface has exited; the worker will notice on its
    /// next `recv` and stop. Treating it as an error here would produce a flurry of
    /// unsendable error events on the way out.
    fn send(&self, event: Event) {
        let _ = self.events.send(event);
    }
}

/// Describes a request, for an error message.
fn describe(request: &Request) -> String {
    match request {
        Request::LoadGroups => "fetching the group list".to_owned(),
        Request::OpenGroup { group, .. } => format!("opening {group}"),
        Request::LoadOverview { group, range, .. } => {
            format!("listing {group} {}", range.to_argument())
        }
        Request::LoadArticle { spec, .. } => match spec {
            nntp_proto::ArticleSpec::Number(number) => format!("fetching article {number}"),
            nntp_proto::ArticleSpec::MessageId(id) => format!("fetching {id}"),
            nntp_proto::ArticleSpec::Current => "fetching the current article".to_owned(),
        },
        Request::Shutdown => "shutting down".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_every_request_for_an_error_message() {
        let group = GroupName::parse("misc.test").unwrap();

        assert_eq!(describe(&Request::LoadGroups), "fetching the group list");
        assert_eq!(
            describe(&Request::OpenGroup {
                group: group.clone(),
                count: 10,
                token: FetchToken::new(1)
            }),
            "opening misc.test"
        );
        assert_eq!(
            describe(&Request::LoadOverview {
                group: group.clone(),
                range: Range::between(1, 5),
                token: FetchToken::new(2)
            }),
            "listing misc.test 1-5"
        );
        assert_eq!(
            describe(&Request::LoadArticle {
                group: Some(group),
                spec: nntp_proto::ArticleSpec::Number(7)
            }),
            "fetching article 7"
        );
        assert_eq!(
            describe(&Request::LoadArticle {
                group: None,
                spec: nntp_proto::ArticleSpec::MessageId(
                    nntp_proto::MessageId::parse("<a@b>").unwrap()
                )
            }),
            "fetching <a@b>"
        );
    }
}
