//! The terminal user interface.
//!
//! Three parts, deliberately separated:
//!
//! - [`app`] is the state machine. It takes key events and worker events and produces
//!   requests. It touches neither the terminal nor the network, so the whole behaviour of
//!   the reader is testable as plain functions.
//! - [`ui`] draws whatever the state machine holds, and decides nothing.
//! - [`worker`] owns the client and does every blocking thing on its own thread.
//!
//! The loop below is the only part that needs a real terminal, and it is deliberately
//! thin. See
//! [ADR-0003](https://github.com/edusouza/rust-nntp/blob/main/docs/adr/0003-blocking-io-on-a-worker-thread.md).

pub mod app;
pub mod protocol;
pub mod ui;
pub mod worker;

use std::sync::mpsc::{self, Sender, TryRecvError};
use std::time::Duration;

use anyhow::Context as _;
use ratatui::crossterm::event::{self, Event as TerminalEvent, KeyEventKind};

use crate::config::Config;
use crate::readstate::ReadStore;
use crate::session::Target;
use crate::tui::app::App;
use crate::tui::protocol::{Event, Request};

/// How long to wait for a terminal event before ticking the spinner.
const TICK: Duration = Duration::from_millis(120);

/// How long to wait for the worker to finish after the interface exits.
const SHUTDOWN_GRACE: Duration = Duration::from_millis(750);

/// Runs the terminal interface until the user quits.
///
/// # Errors
///
/// Returns an error if the terminal cannot be set up, if drawing fails, or if the worker
/// thread cannot be spawned. Network failures are shown in the interface instead: a
/// dropped connection should not end the session.
pub fn run(config: &Config, target: Target) -> anyhow::Result<()> {
    let (request_tx, request_rx) = mpsc::channel::<Request>();
    let (event_tx, event_rx) = mpsc::channel::<Event>();

    // Read state is per server, because article numbers are the server's own. Loading it
    // before the terminal is taken over keeps any problem with it on ordinary stderr, and
    // loading never fails: a missing or corrupt store means "everything unread", never a
    // reader that will not start (ADR-0009).
    let (read_state, read_problems) = match ReadStore::default_path(&target.server.host) {
        Ok(path) => ReadStore::load(path),
        Err(error) => {
            tracing::warn!(%error, "no data directory; read state will not be saved");
            (ReadStore::empty(std::path::PathBuf::new()), Vec::new())
        }
    };

    // Who anything written in this session is posted as. Read before the target is moved
    // into the worker, since the worker has no use for it.
    let from = target.server.from.clone();

    // Which groups to ask for. Parsed here rather than in the state machine because a
    // pattern that cannot be sent is a configuration problem, and this is the last place
    // that can still report one on ordinary stderr before the terminal is taken over.
    // `Config::validate` has already refused an unusable pattern, so this only warns.
    let subscriptions: Vec<nntp_proto::Wildmat> = target
        .server
        .subscriptions
        .iter()
        .filter_map(|pattern| match nntp_proto::Wildmat::parse(pattern) {
            Ok(pattern) => Some(pattern),
            Err(error) => {
                tracing::warn!(%pattern, %error, "ignoring an unusable subscription");
                None
            }
        })
        .collect();

    // The flag the interface raises and the worker watches. Created here because both
    // sides need it and it outlives neither on its own.
    let cancel = nntp_client::Cancel::new();

    let worker = worker::spawn(
        target,
        config.ui.overview_chunk,
        request_rx,
        event_tx,
        cancel.clone(),
    )
    .context("starting the network worker")?;

    // `init` enables raw mode, switches to the alternate screen, and installs a panic
    // hook that restores both. Without that hook a panic leaves the user with an
    // unusable terminal and no error message.
    let mut terminal = ratatui::try_init().context("setting up the terminal")?;

    let mut app = App::new(&config.ui, read_state);
    app.cancel = cancel;
    app.from = from;
    app.subscriptions = subscriptions;
    for problem in read_problems {
        app.note(problem.to_string());
        tracing::warn!(%problem, "read state");
    }

    let outcome = event_loop(&mut terminal, &mut app, &request_tx, &event_rx);

    // Read state is saved here rather than inside the state machine, which has no IO by
    // design (ADR-0008). Before the terminal is restored so that a failure to save is
    // reported the same way as any other, and unconditionally: the session is over, and
    // a save that only happens on a clean exit is the one that loses a day's reading.
    if let Err(error) = app.read.save_if_dirty() {
        tracing::error!(%error, "could not save read state");
    }

    // Restore the terminal before reporting anything, so an error message is readable.
    let restored = ratatui::try_restore();

    // Ask the worker to stop, then give it a moment. A worker blocked in a read on a
    // dead socket would otherwise hold the process open until its read timeout expires.
    let _ = request_tx.send(Request::Shutdown);
    drop(request_tx);
    let deadline = std::time::Instant::now() + SHUTDOWN_GRACE;
    while !worker.is_finished() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    if worker.is_finished() {
        if worker.join().is_err() {
            tracing::error!("the network worker panicked");
        }
    } else {
        // Joining now would block until the socket's read timeout expires, which is a
        // minute of a hung terminal for no benefit. The process is exiting anyway.
        tracing::warn!("the network worker did not stop in time; leaving it to the process exit");
    }

    outcome?;
    restored.context("restoring the terminal")?;
    Ok(())
}

/// The render loop: drain worker events, poll the terminal, draw if anything changed.
fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    requests: &Sender<Request>,
    events: &std::sync::mpsc::Receiver<Event>,
) -> anyhow::Result<()> {
    for request in app.initial_requests() {
        send(requests, app, request);
    }

    loop {
        // Every pending event first: a burst of them should cost one redraw, not one
        // redraw each.
        loop {
            match events.try_recv() {
                Ok(event) => {
                    // An event can produce work of its own — a posting lands in the group
                    // on screen and the list has to be fetched again.
                    for request in app.on_event(event) {
                        send(requests, app, request);
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    app.on_event(Event::Stopped);
                    break;
                }
            }
        }

        if app.dirty {
            terminal
                .draw(|frame| ui::draw(frame, app))
                .context("drawing")?;
            app.dirty = false;
        }

        if app.should_quit {
            return Ok(());
        }

        // A timeout rather than a blocking read, so the spinner turns and a worker event
        // that arrives while the user is idle is still picked up promptly.
        if event::poll(TICK).context("polling for terminal events")? {
            match event::read().context("reading a terminal event")? {
                // Only key *presses*: on Windows crossterm also reports releases, and
                // acting on both would move the cursor two rows per keystroke.
                TerminalEvent::Key(key) if key.kind == KeyEventKind::Press => {
                    for request in app.on_key(key) {
                        send(requests, app, request);
                    }
                }
                TerminalEvent::Resize(_, _) => app.dirty = true,
                _ => {}
            }
        } else {
            app.tick();
        }

        // After the keystroke, not during it: composing takes the terminal away from this
        // loop, and the state machine is not allowed to know that terminals exist.
        if let Some(request) = app.take_compose() {
            compose(terminal, app, requests, &request)?;
        }
    }
}

/// Hands the terminal to the user's editor and takes it back afterwards.
///
/// The only place in the reader where another program owns the screen. Everything here is
/// about handing it over cleanly and getting it back whatever happens: an editor that
/// cannot start, one that exits non-zero, one that leaves the draft untouched.
fn compose(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    requests: &Sender<Request>,
    request: &crate::tui::app::ComposeRequest,
) -> anyhow::Result<()> {
    // Raw mode and the alternate screen both have to go: an editor drawing into them
    // produces a screen neither program can clean up.
    ratatui::try_restore().context("releasing the terminal for the editor")?;

    let outcome = crate::compose::compose(&request.template);

    // Back, whatever the editor did. Failing to re-init is fatal — there is no interface
    // to report it in — but the editor's own failure is not, and is reported below.
    *terminal = ratatui::try_init().context("taking the terminal back after the editor")?;
    terminal.clear().context("clearing after the editor")?;
    app.dirty = true;

    match outcome {
        Ok(crate::compose::Composed::Edited { text, path }) => {
            for outgoing in app.on_composed(Some((text, path))) {
                send(requests, app, outgoing);
            }
        }
        Ok(crate::compose::Composed::Abandoned) => {
            app.on_composed(None);
            app.note(format!("{} was abandoned", request.what));
        }
        Err(error) => {
            app.on_composed(None);
            // `{:#}` so the context chain — which names the editor and the draft's path —
            // is on one line rather than only in the log.
            app.note(format!("could not compose: {error:#}"));
            tracing::warn!(%error, "composing failed");
        }
    }

    Ok(())
}

/// Sends a request, noting in the interface if the worker has gone.
fn send(requests: &Sender<Request>, app: &mut App, request: Request) {
    let shutting_down = matches!(request, Request::Shutdown);

    if requests.send(request).is_err() && !shutting_down {
        app.on_event(Event::Disconnected(
            "the network worker is no longer running".to_owned(),
        ));
    }
}
