//! Drawing. No state of its own, and no decisions: it renders whatever [`App`] holds.
//!
//! Keeping the decisions in `app` and the pixels here is what lets the behaviour be
//! tested without a terminal.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize as _};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Clear, List, ListItem, ListState, Paragraph, Wrap};

use crate::tui::app::{App, Overlay, Pane};
use crate::tui::protocol::GroupScope;

/// The colour of a focused border, and of the selected row.
const ACCENT: Color = Color::Cyan;

/// Draws the whole interface.
pub fn draw(frame: &mut Frame<'_>, app: &mut App) {
    let [body, status] =
        Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).areas(frame.area());

    let [groups, articles, article] = Layout::horizontal([
        Constraint::Percentage(26),
        Constraint::Percentage(34),
        Constraint::Percentage(40),
    ])
    .areas(body);

    // Two rows of the pane go to the border, so the usable height is what page keys and
    // the scroll clamp must work from.
    app.set_pane_heights(
        usize::from(articles.height.saturating_sub(2)),
        usize::from(article.height.saturating_sub(2)),
    );

    draw_groups(frame, app, groups);
    draw_articles(frame, app, articles);
    draw_article(frame, app, article);
    draw_status(frame, app, status);

    // The overlay clamps its own scroll, because how many lines a text occupies depends on
    // wrapping and so on the width — which only the renderer knows. Reported back so that
    // `End` leaves the position somewhere the next key press can move from.
    match app.overlay {
        Overlay::None => {}
        Overlay::Help => {
            let scroll = draw_overlay(frame, app, "Keys", help_text(), body);
            app.set_overlay_scroll(scroll);
        }
        Overlay::Messages => {
            let text = messages_text(app);
            let scroll = draw_overlay(frame, app, "Messages", text, body);
            app.set_overlay_scroll(scroll);
        }
    }
}

/// A block whose border shows whether its pane has focus.
fn pane_block(app: &App, pane: Pane, subtitle: Option<String>) -> Block<'static> {
    let focused = app.focus == pane && app.overlay == Overlay::None;

    let title = match subtitle {
        Some(subtitle) => format!(" {} — {subtitle} ", pane.title()),
        None => format!(" {} ", pane.title()),
    };

    let block = Block::bordered()
        .border_type(if focused {
            BorderType::Thick
        } else {
            BorderType::Plain
        })
        .title(title);

    if focused {
        block.border_style(Style::new().fg(ACCENT))
    } else {
        block.border_style(Style::new().fg(Color::DarkGray))
    }
}

fn selection_style(focused: bool) -> Style {
    if focused {
        Style::new()
            .bg(ACCENT)
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD)
    } else {
        // Still visible when the pane is not focused, so the reader can see where it
        // will return to, but not competing for attention.
        Style::new().add_modifier(Modifier::REVERSED | Modifier::DIM)
    }
}

fn draw_groups(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let visible = app.visible_groups();

    // What the count means depends on what was asked for, and this pane is two dozen
    // columns wide, so it is one word: a list narrowed by subscriptions or by a search is
    // not the server's whole catalogue, and a reader looking for a missing group needs to
    // know which of the two they are looking at.
    let scope = match &app.group_scope {
        GroupScope::Subscribed(patterns) if patterns.is_empty() => "",
        GroupScope::Subscribed(_) => " subscribed",
        GroupScope::Search(_) => " found",
    };

    let subtitle = if app.editing_filter {
        Some(format!("/{}", app.filter))
    } else if app.filter.is_empty() {
        (!app.groups.is_empty()).then(|| format!("{}{scope}", app.groups.len()))
    } else {
        Some(format!(
            "/{} — {} of {}{scope}",
            app.filter,
            visible.len(),
            app.groups.len()
        ))
    };

    let items: Vec<ListItem<'_>> = visible
        .iter()
        .filter_map(|index| app.groups.get(*index))
        .map(|group| {
            // The number here is *unread*, not the total: it is what a reader opens the
            // list to find out, and this pane is two dozen columns wide, so showing both
            // meant showing neither — the first attempt rendered "≤18342 unre". The
            // total is in the status bar the moment the group is opened. "≤" because the
            // watermarks bound the count rather than stating it, as everywhere else.
            let unread = app.unread_in(group);
            let count = if group.is_empty() {
                Span::from("  empty").dim()
            } else if unread == 0 {
                Span::from("  read").dim()
            } else {
                Span::from(format!("  \u{2264}{unread}")).dim()
            };
            // Bold marks a group with something in it, so the shape of the list answers
            // "where is there anything new" without reading any numbers.
            let name = if unread > 0 {
                Span::from(group.name.to_string()).bold()
            } else {
                Span::from(group.name.to_string())
            };
            ListItem::new(Line::from(vec![name, count]))
        })
        .collect();

    let mut state = ListState::default();
    if !visible.is_empty() {
        state.select(Some(app.group_cursor.min(visible.len() - 1)));
    }

    let list = List::new(items)
        .block(pane_block(app, Pane::Groups, subtitle))
        .highlight_style(selection_style(app.focus == Pane::Groups));

    frame.render_stateful_widget(list, area, &mut state);
}

fn draw_articles(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let subtitle = app.group.as_ref().map(|summary| {
        if app.unread_only {
            // "shown/total" rather than "n unread of m", and no trailing word: this
            // pane is a third of the screen, and both longer forms were clipped
            // mid-word by the block title. The placeholder below says what the numbers
            // mean when the list is empty, and the help overlay says it always.
            format!(
                "{} ({}/{})",
                summary.name,
                app.visible_articles().len(),
                app.articles.len()
            )
        } else {
            format!("{} ({})", summary.name, app.articles.len())
        }
    });

    let rows = app.article_rows();
    let visible: Vec<usize> = rows.iter().map(|row| row.index).collect();

    let items: Vec<ListItem<'_>> = rows
        .iter()
        .filter_map(|row| app.articles.get(row.index).map(|record| (row, record)))
        .map(|(row, record)| {
            let subject = if record.subject.is_empty() {
                "(no subject)".to_owned()
            } else {
                record.subject.clone()
            };

            // The indent is the thread. It is capped well below the algorithm's own
            // depth limit because this pane is a third of the screen: past a few levels
            // the subject would be pushed off the right-hand edge, and an unreadable
            // subject costs more than a lost level of nesting.
            const MAX_INDENT: usize = 6;
            let indent = "  ".repeat(row.depth.min(MAX_INDENT));

            // A folded thread says how much it is hiding. Without the number a fold is
            // indistinguishable from a thread that simply has no replies.
            let marker = if row.collapsed {
                format!("+{} ", row.hidden)
            } else if row.depth > 0 {
                "\u{203a} ".to_owned()
            } else {
                "  ".to_owned()
            };

            // Unread is marked, read is not. The other way round would put a mark on
            // almost every line in a group you follow, which is no mark at all.
            let unread = app.is_unread(record.number);
            let mark = if unread { "•" } else { " " };
            let subject = if unread {
                Span::from(subject).bold()
            } else {
                Span::from(subject).dim()
            };

            ListItem::new(Line::from(vec![
                Span::from(mark).fg(ACCENT),
                Span::from(indent).dim(),
                Span::from(marker).dim(),
                subject,
            ]))
        })
        .collect();

    let mut state = ListState::default();
    if !visible.is_empty() {
        state.select(Some(app.article_cursor.min(visible.len() - 1)));
    }

    let placeholder = if visible.is_empty() {
        Some(if app.group.is_none() {
            "select a group and press Enter"
        } else if app.unread_only && !app.articles.is_empty() {
            // Distinguishing the two matters: "nothing unread" is a finished group and
            // "no articles" is an empty one, and telling a reader the wrong one of those
            // sends them looking for a bug.
            "nothing unread — u: show all"
        } else {
            "no articles"
        })
    } else {
        None
    };

    if let Some(text) = placeholder {
        let paragraph = Paragraph::new(Text::from(text).dim())
            .block(pane_block(app, Pane::Articles, subtitle))
            .alignment(Alignment::Center);
        frame.render_widget(paragraph, area);
        return;
    }

    let list = List::new(items)
        .block(pane_block(app, Pane::Articles, subtitle))
        .highlight_style(selection_style(app.focus == Pane::Articles));

    frame.render_stateful_widget(list, area, &mut state);
}

fn draw_article(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let Some(view) = &app.article else {
        let hint = if app.articles.is_empty() {
            "nothing to show yet"
        } else {
            "press Enter on an article"
        };
        let paragraph = Paragraph::new(Text::from(hint).dim())
            .block(pane_block(app, Pane::Body, None))
            .alignment(Alignment::Center);
        frame.render_widget(paragraph, area);
        return;
    };

    let mut lines: Vec<Line<'_>> = view
        .headers
        .iter()
        .map(|(name, value)| {
            Line::from(vec![
                Span::from(format!("{name}: ")).bold().fg(ACCENT),
                Span::from(value.clone()),
            ])
        })
        .collect();

    // A signature is a fact about the article rather than a part of it: one dim line, and
    // deliberately not a claim that it was checked. Nothing in this project does
    // cryptography.
    if view.signed {
        lines.push(Line::from(
            Span::from("signed (signature not checked)").dim(),
        ));
    }

    // Attachments go above the body, not below it: a reader who has to scroll to the end
    // of a long article to find out something was attached has already been misled.
    if !view.attachments.is_empty() {
        let label = if view.attachments.len() == 1 {
            "1 other part".to_owned()
        } else {
            format!("{} other parts", view.attachments.len())
        };
        lines.push(Line::from(
            Span::from(format!("{label}:")).bold().fg(ACCENT),
        ));
        for summary in &view.attachments {
            lines.push(Line::from(
                Span::from(format!("  \u{2022} {summary}")).dim(),
            ));
        }
    }

    lines.push(Line::default());

    for line in view.body.iter().skip(app.body_scroll) {
        // Quoted text dimmed: on Usenet most of a follow-up is quotation, and dimming it
        // is the difference between a readable article and a wall of text.
        let style = if line.starts_with('>') || line.starts_with('|') {
            Style::new().fg(Color::Green).dim()
        } else if line == "-- " || line == "--" {
            Style::new().dim()
        } else {
            Style::new()
        };
        lines.push(Line::from(Span::styled(line.clone(), style)));
    }

    let position = if view.body.is_empty() {
        "empty".to_owned()
    } else {
        format!(
            "{}–{} of {}",
            app.body_scroll + 1,
            (app.body_scroll + app.body_height).min(view.body.len()),
            view.body.len()
        )
    };

    let paragraph = Paragraph::new(Text::from(lines))
        .block(pane_block(app, Pane::Body, Some(position)))
        .wrap(Wrap { trim: false });

    frame.render_widget(paragraph, area);
}

fn draw_status(frame: &mut Frame<'_>, app: &App, area: Rect) {
    // An error replaces the status line entirely: a failure the user cannot see is a
    // failure they will report as "it just did nothing".
    if let Some(error) = &app.error {
        let line = Line::from(vec![
            Span::from(" error ").bg(Color::Red).fg(Color::White).bold(),
            Span::from(format!(" {error}")),
        ]);
        frame.render_widget(Paragraph::new(line), area);
        return;
    }

    let connection = if app.connected {
        let label = format!(
            " {} [{}] ",
            app.server,
            if app.encrypted { "TLS" } else { "plain" }
        );
        Span::from(label)
            .bg(if app.encrypted {
                Color::Green
            } else {
                Color::Yellow
            })
            .fg(Color::Black)
    } else {
        Span::from(" disconnected ").bg(Color::Red).fg(Color::White)
    };

    // Laid out rather than concatenated: on a narrow terminal the hint gives up its
    // space and then disappears, instead of pushing the status message off the edge or
    // being cut mid-word.
    // While something is outstanding the hint says how to stop it: a cancel key nobody
    // can find is a cancel key nobody has.
    const IDLE_HINT: &str = " ?: help  m: messages  q: quit";
    const BUSY_HINT: &str = " Esc: stop  ?: help  q: quit";
    let hint = if app.inflight > 0 {
        BUSY_HINT
    } else {
        IDLE_HINT
    };
    let hint_width = u16::try_from(hint.chars().count()).unwrap_or(u16::MAX);
    let connection_width =
        u16::try_from(connection.content.chars().count() + 2).unwrap_or(u16::MAX);

    let [left, middle, right] = Layout::horizontal([
        Constraint::Length(connection_width.min(area.width)),
        Constraint::Min(0),
        Constraint::Length(hint_width),
    ])
    .areas(area);

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            connection,
            Span::from(format!(" {} ", app.spinner_frame())),
        ])),
        left,
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::from(app.status.clone()))),
        middle,
    );
    // Rendered last and right-aligned, so it is the part that vanishes first.
    frame.render_widget(
        Paragraph::new(Line::from(Span::from(hint).dim())).alignment(Alignment::Right),
        right,
    );
}

/// Draws a centred overlay over the panes.
fn draw_overlay(
    frame: &mut Frame<'_>,
    app: &App,
    title: &str,
    text: Text<'static>,
    area: Rect,
) -> usize {
    // The overlay takes three quarters of the area, bounded to a readable size and then
    // bounded again by the terminal itself, so a window too small for the preferred
    // minimum still renders something instead of panicking.
    const MIN_WIDTH: u16 = 20;
    const MAX_WIDTH: u16 = 80;
    const MIN_HEIGHT: u16 = 5;

    let width = area
        .width
        .saturating_sub(area.width / 4)
        // The lower bound can never exceed the upper one, so `clamp` cannot panic here.
        .clamp(MIN_WIDTH.min(area.width), MAX_WIDTH)
        .min(area.width);
    let height = area
        .height
        .saturating_sub(area.height / 5)
        .clamp(MIN_HEIGHT.min(area.height), area.height);

    let centred = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };

    // How much of it fits, so that a text taller than the overlay can be scrolled rather
    // than silently cut off at the bottom. The lines are counted after wrapping, which is
    // why this is here and not in the state machine.
    let inner_width = centred.width.saturating_sub(2).max(1);
    let inner_height = centred.height.saturating_sub(2) as usize;
    let wrapped: usize = text
        .lines
        .iter()
        .map(|line| {
            let width = line.width().max(1);
            width.div_ceil(inner_width as usize)
        })
        .sum();
    let max_scroll = wrapped.saturating_sub(inner_height);
    let scroll = app.overlay_scroll.min(max_scroll);

    // A cut-off list that does not say it is cut off is the actual problem; the title is
    // where a reader is already looking.
    let title = if max_scroll > 0 {
        format!(
            " {title} — {}/{} lines, \u{2191}\u{2193} to scroll, Esc to close ",
            (scroll + inner_height).min(wrapped),
            wrapped
        )
    } else {
        format!(" {title} — Esc to close ")
    };

    // Clear first, or the panes show through the overlay.
    frame.render_widget(Clear, centred);
    frame.render_widget(
        Paragraph::new(text)
            .block(
                Block::bordered()
                    .border_type(BorderType::Double)
                    .border_style(Style::new().fg(ACCENT))
                    .title(title),
            )
            .wrap(Wrap { trim: false })
            .scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0)),
        centred,
    );

    scroll
}

fn help_text() -> Text<'static> {
    const ROWS: [(&str, &str); 23] = [
        ("Tab / Shift-Tab", "next / previous pane"),
        ("h l  ← →", "move focus left / right"),
        ("j k  ↓ ↑", "move down / up"),
        ("Ctrl-d / Ctrl-u", "page down / up"),
        ("PageDown / PageUp", "page down / up"),
        ("g / G", "first / last"),
        ("Enter", "open the group or article under the cursor"),
        ("n / p", "next / previous article, opening it"),
        ("u", "show only unread articles, or everything"),
        ("t", "group the list into conversations, or show it flat"),
        (
            "z",
            "fold the replies under the cursor away, or bring them back",
        ),
        ("w", "write a new article in the selected group"),
        ("f", "follow up to the article on screen"),
        ("M", "mark the article under the cursor read / unread"),
        ("c", "catch up: mark the whole group read"),
        ("/", "filter the groups already fetched, by name or text"),
        ("↑ ↓ while filtering", "move through what the filter left"),
        ("S", "ask the server for groups matching the filter"),
        (
            "Esc",
            "stop a request in progress; else clear the filter or close an overlay",
        ),
        ("r", "reload the focused pane"),
        ("m", "show recent messages"),
        ("?  F1", "this help"),
        ("q  Ctrl-C", "quit"),
    ];

    let mut lines = vec![Line::default()];
    for (keys, description) in ROWS {
        lines.push(Line::from(vec![
            Span::from(format!("  {keys:<20}")).bold().fg(ACCENT),
            Span::from(description),
        ]));
    }
    lines.push(Line::default());
    lines.push(Line::from(
        Span::from("  The number beside a group is how many articles are unread; \u{2022} marks an unread article.")
            .dim(),
    ));
    lines.push(Line::from(
        Span::from(
            "  Counts are shown as ≤n: LIST ACTIVE reports watermarks, and expiry leaves gaps.",
        )
        .dim(),
    ));

    Text::from(lines)
}

fn messages_text(app: &App) -> Text<'static> {
    if app.messages.is_empty() {
        return Text::from(Line::from(Span::from("  nothing to report").dim()));
    }

    Text::from(
        app.messages
            .iter()
            .map(|message| Line::from(format!("  {message}")))
            .collect::<Vec<_>>(),
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ratatui::Terminal;

    use crate::readstate::ReadStore;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::config::UiConfig;
    use crate::tui::protocol::{Event, GroupRow};

    /// A group list as the worker delivers one: everything the server carries.
    fn groups_arrived(rows: Vec<GroupRow>) -> Event {
        Event::Groups {
            rows,
            scope: GroupScope::everything(),
            filtered_locally: false,
        }
    }

    /// Renders into an in-memory terminal and returns the screen as text.
    ///
    /// `TestBackend` is what makes the drawing code testable at all: without it these
    /// paths would only ever be exercised by a human looking at a terminal.
    fn render(app: &mut App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        terminal.draw(|frame| draw(frame, app)).expect("draw");

        terminal
            .backend()
            .buffer()
            .content()
            .chunks(usize::from(width))
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn app() -> App {
        App::new(
            &UiConfig::default(),
            ReadStore::empty(PathBuf::from("unused")),
        )
    }

    fn group_summary(name: &str, low: u64, high: u64) -> nntp_proto::GroupSummary {
        let line = nntp_proto::StatusLine::parse(
            format!("211 {} {low} {high} {name}", high - low + 1).as_bytes(),
        )
        .unwrap();
        nntp_proto::GroupSummary::parse(&line, None).unwrap()
    }

    fn overview_record(number: u64, subject: &str) -> nntp_proto::OverviewRecord {
        let line = format!("{number}\t{subject}\ta@x\t\t<{number}@x>\t\t10\t1");
        nntp_proto::OverviewRecord::parse(line.as_bytes(), &nntp_proto::OverviewFmt::standard())
            .unwrap()
    }

    /// Delivers a whole overview fetch the way the worker does: one chunk, then the
    /// completion.
    ///
    /// Most tests care that the records end up listed, not about how many pieces they
    /// arrived in; the tests that care about the pieces build the events themselves.
    fn deliver_overview(
        app: &mut App,
        group: &str,
        records: Vec<nntp_proto::OverviewRecord>,
        skipped: usize,
    ) {
        let group = nntp_proto::GroupName::parse(group).unwrap();
        let token = app.begin_overview_fetch();
        app.on_event(Event::OverviewChunk {
            group: group.clone(),
            token,
            records,
            skipped,
        });
        app.on_event(Event::OverviewComplete { group, token });
    }

    fn group_row(name: &str, low: u64, high: u64) -> GroupRow {
        GroupRow {
            name: nntp_proto::GroupName::parse(name).unwrap(),
            low,
            high,
            status: nntp_proto::PostingStatus::Permitted,
            description: None,
        }
    }

    #[test]
    fn draws_the_three_panes_and_the_status_bar() {
        let mut app = app();
        let screen = render(&mut app, 100, 20);

        assert!(screen.contains("Groups"), "{screen}");
        assert!(screen.contains("Articles"), "{screen}");
        assert!(screen.contains("Article"), "{screen}");
        assert!(screen.contains("q: quit"), "{screen}");
    }

    #[test]
    fn shows_the_connection_state_in_the_status_bar() {
        let mut app = app();
        assert!(render(&mut app, 100, 20).contains("disconnected"));

        app.on_event(Event::Connected {
            server: "news.example.org:563".to_owned(),
            greeting: "ready".to_owned(),
            encrypted: true,
        });
        let screen = render(&mut app, 100, 20);
        assert!(screen.contains("news.example.org:563"), "{screen}");
        assert!(screen.contains("TLS"), "{screen}");
    }

    #[test]
    fn an_error_takes_over_the_status_bar() {
        let mut app = app();
        app.on_event(Event::Failed {
            context: "LIST".to_owned(),
            message: "timed out".to_owned(),
        });

        let screen = render(&mut app, 100, 20);
        assert!(screen.contains("error"), "{screen}");
        assert!(screen.contains("timed out"), "{screen}");
        // The usual status content is gone, so the error cannot be missed.
        assert!(!screen.contains("q: quit"), "{screen}");
    }

    #[test]
    fn lists_groups_with_a_bounded_unread_count() {
        let mut app = app();
        app.on_event(groups_arrived(vec![
            group_row("comp.lang.rust", 1, 10),
            group_row("empty.group", 1, 0),
        ]));

        let screen = render(&mut app, 100, 20);
        assert!(screen.contains("comp.lang.rust"), "{screen}");
        // Nothing has been read, so all ten numbers are unread.
        assert!(screen.contains("≤10"), "{screen}");
        // An empty group says so rather than claiming zero articles.
        assert!(screen.contains("empty"), "{screen}");
    }

    #[test]
    fn a_group_with_nothing_left_says_read_rather_than_zero() {
        let mut app = app();
        app.read.mark_range_read("comp.lang.rust", 1, 10);
        app.on_event(groups_arrived(vec![group_row("comp.lang.rust", 1, 10)]));

        let screen = render(&mut app, 100, 20);
        assert!(screen.contains("read"), "{screen}");
        // "≤0" would be technically true and useless.
        assert!(!screen.contains("≤0"), "{screen}");
    }

    #[test]
    fn unread_articles_are_marked_and_read_ones_are_not() {
        let mut app = app();
        app.on_event(Event::GroupOpened(Box::new(group_summary(
            "misc.test",
            1,
            3,
        ))));
        app.read.mark_read("misc.test", 2);
        deliver_overview(
            &mut app,
            "misc.test",
            vec![
                overview_record(1, "unread one"),
                overview_record(2, "already read"),
                overview_record(3, "unread two"),
            ],
            0,
        );

        let screen = render(&mut app, 100, 20);
        assert!(screen.contains("•  unread one"), "{screen}");
        assert!(screen.contains("•  unread two"), "{screen}");
        // The read one is on screen, without a mark.
        assert!(screen.contains("already read"), "{screen}");
        assert!(!screen.contains("•  already read"), "{screen}");
    }

    #[test]
    fn a_thread_is_drawn_indented_and_a_fold_says_what_it_hides() {
        let mut app = app();
        app.on_event(Event::GroupOpened(Box::new(group_summary(
            "misc.test",
            1,
            3,
        ))));

        let group = nntp_proto::GroupName::parse("misc.test").unwrap();
        let token = app.begin_overview_fetch();
        let reply = |number: u64, subject: &str, references: &str| {
            let line = format!("{number}\t{subject}\ta@x\t\t<{number}@x>\t{references}\t10\t1");
            nntp_proto::OverviewRecord::parse(line.as_bytes(), &nntp_proto::OverviewFmt::standard())
                .unwrap()
        };
        app.on_event(Event::OverviewChunk {
            group: group.clone(),
            token,
            records: vec![
                reply(1, "the question", ""),
                reply(2, "Re: the question", "<1@x>"),
            ],
            skipped: 0,
        });
        app.on_event(Event::OverviewComplete { group, token });

        let screen = render(&mut app, 100, 20);
        assert!(
            screen.contains("\u{2022}  \u{203a} Re: the question"),
            "the reply should be indented under its parent\n{screen}"
        );

        // Folded, the reply is gone and the count is on the parent's line.
        app.focus = Pane::Articles;
        app.article_cursor = 0;
        app.on_key(ratatui::crossterm::event::KeyEvent::new(
            ratatui::crossterm::event::KeyCode::Char('z'),
            ratatui::crossterm::event::KeyModifiers::NONE,
        ));

        let screen = render(&mut app, 100, 20);
        assert!(screen.contains("+1 the question"), "{screen}");
        assert!(!screen.contains("Re: the question"), "{screen}");
    }

    #[test]
    fn a_finished_group_under_the_unread_filter_says_so() {
        let mut app = app();
        app.on_event(Event::GroupOpened(Box::new(group_summary(
            "misc.test",
            1,
            2,
        ))));
        deliver_overview(
            &mut app,
            "misc.test",
            vec![overview_record(1, "one"), overview_record(2, "two")],
            0,
        );
        app.read.mark_range_read("misc.test", 1, 2);
        app.unread_only = true;

        let screen = render(&mut app, 100, 20);
        // "no articles" here would send the reader looking for a bug in a group that is
        // simply finished.
        assert!(screen.contains("nothing unread"), "{screen}");
        assert!(screen.contains("(0/2)"), "{screen}");
    }

    #[test]
    fn shows_the_filter_and_how_much_it_hides() {
        let mut app = app();
        app.on_event(groups_arrived(vec![
            group_row("comp.lang.rust", 1, 10),
            group_row("misc.test", 1, 3),
        ]));
        app.filter = "misc".to_owned();

        let screen = render(&mut app, 100, 20);
        assert!(screen.contains("/misc"), "{screen}");
        assert!(screen.contains("1 of 2"), "{screen}");
    }

    #[test]
    fn says_when_the_list_is_narrower_than_the_server() {
        // A reader hunting for a group that is not on screen has to be able to tell
        // "this server does not carry it" from "you did not ask for it".
        let mut app = app();
        app.on_event(Event::Groups {
            rows: vec![group_row("comp.lang.rust", 1, 10)],
            scope: GroupScope::Subscribed(vec![
                nntp_proto::Wildmat::parse("comp.lang.*").expect("a valid pattern"),
            ]),
            filtered_locally: false,
        });

        let screen = render(&mut app, 100, 20);
        assert!(screen.contains("1 subscribed"), "{screen}");
    }

    #[test]
    fn says_when_the_list_came_from_a_search() {
        let mut app = app();
        app.on_event(Event::Groups {
            rows: vec![group_row("comp.lang.rust", 1, 10)],
            scope: GroupScope::Search(
                nntp_proto::Wildmat::parse("*rust*").expect("a valid pattern"),
            ),
            filtered_locally: false,
        });

        let screen = render(&mut app, 100, 20);
        assert!(screen.contains("1 found"), "{screen}");
    }

    #[test]
    fn empty_panes_say_what_to_do_next() {
        let mut app = app();
        let screen = render(&mut app, 120, 20);
        assert!(screen.contains("select a group"), "{screen}");
        assert!(screen.contains("nothing to show yet"), "{screen}");
    }

    #[test]
    fn renders_an_article_with_its_headers_and_body() {
        let mut app = app();
        let block = nntp_proto::DataBlock::parse(
            b"From: a@example.net\r\nSubject: =?UTF-8?Q?caf=C3=A9?=\r\n\
              Date: Wed, 17 Sep 2026 08:00:00 +0000\r\n\r\n\
              > quoted line\r\nplain line\r\n.\r\n",
        );
        app.on_event(Event::Article(Box::new(nntp_proto::Article::from_block(
            &block,
        ))));

        let screen = render(&mut app, 100, 20);
        assert!(screen.contains("Subject: café"), "{screen}");
        assert!(screen.contains("a@example.net"), "{screen}");
        assert!(screen.contains("quoted line"), "{screen}");
        assert!(screen.contains("plain line"), "{screen}");
        // The body position is in the pane title.
        assert!(screen.contains("of 2"), "{screen}");
    }

    #[test]
    fn a_multipart_article_shows_the_text_and_lists_the_other_parts() {
        let mut app = app();
        let block = nntp_proto::DataBlock::parse(
            b"From: a@example.net\r\nSubject: multipart\r\n\
              Content-Type: multipart/mixed; boundary=\"b\"\r\n\r\n\
              --b\r\nContent-Type: text/plain\r\n\r\nthe readable part\r\n\
              --b\r\nContent-Type: text/x-patch\r\n\
              Content-Disposition: attachment; filename=\"fix.patch\"\r\n\r\n\
              --- a/x\r\n--b--\r\n.\r\n",
        );
        app.on_event(Event::Article(Box::new(nntp_proto::Article::from_block(
            &block,
        ))));

        let screen = render(&mut app, 100, 20);
        assert!(screen.contains("the readable part"), "{screen}");
        // Named above the body, so a reader does not have to scroll to the end of a long
        // article to discover that something was attached.
        assert!(screen.contains("1 other part"), "{screen}");
        assert!(screen.contains("fix.patch"), "{screen}");
        // And no MIME machinery on screen.
        assert!(!screen.contains("--b"), "{screen}");
    }

    #[test]
    fn the_help_overlay_covers_the_panes() {
        let mut app = app();
        app.overlay = Overlay::Help;

        let screen = render(&mut app, 100, 24);
        assert!(screen.contains("Keys"), "{screen}");
        assert!(screen.contains("Esc to close"), "{screen}");
        assert!(screen.contains("next / previous pane"), "{screen}");
    }

    #[test]
    fn a_help_list_taller_than_the_terminal_can_be_scrolled_and_says_so() {
        // The key list outgrew a 24-line terminal, which plenty of people still use. An
        // overlay that cuts off its bottom third without a word is worse than no help.
        let mut app = app();
        app.overlay = Overlay::Help;

        let screen = render(&mut app, 100, 24);
        assert!(screen.contains("to scroll"), "{screen}");
        // The last row of the list. Not "quit": the status bar says that too.
        let last_row = "Ctrl-C";
        assert!(
            !screen.contains(last_row),
            "the end should be off-screen: {screen}"
        );

        app.on_key(ratatui::crossterm::event::KeyEvent::new(
            ratatui::crossterm::event::KeyCode::End,
            ratatui::crossterm::event::KeyModifiers::NONE,
        ));
        let screen = render(&mut app, 100, 24);
        assert!(
            screen.contains(last_row),
            "scrolling to the end shows it: {screen}"
        );
    }

    #[test]
    fn a_help_list_that_fits_says_nothing_about_scrolling() {
        let mut app = app();
        app.overlay = Overlay::Help;

        // Tall enough for every key: the help is past thirty lines now, and the point
        // here is what the overlay does when it *does* fit, not how tall it happens to be.
        let screen = render(&mut app, 100, 46);
        assert!(!screen.contains("to scroll"), "{screen}");
        assert!(screen.contains("Ctrl-C"), "{screen}");
    }

    #[test]
    fn the_message_overlay_shows_recent_messages_newest_first() {
        let mut app = app();
        app.on_event(Event::Failed {
            context: "first".to_owned(),
            message: "one".to_owned(),
        });
        app.on_event(Event::Failed {
            context: "second".to_owned(),
            message: "two".to_owned(),
        });
        app.overlay = Overlay::Messages;

        let screen = render(&mut app, 100, 24);
        let first = screen.find("first").expect("first message");
        let second = screen.find("second").expect("second message");
        assert!(second < first, "newest should be first:\n{screen}");
    }

    #[test]
    fn an_empty_message_overlay_says_so() {
        let mut app = app();
        app.overlay = Overlay::Messages;
        assert!(render(&mut app, 100, 24).contains("nothing to report"));
    }

    #[test]
    fn drawing_records_the_pane_heights_for_the_page_keys() {
        let mut app = app();
        render(&mut app, 100, 30);
        // 30 rows, minus one for the status bar, minus two for the borders.
        assert_eq!(app.list_height, 27);
        assert_eq!(app.body_height, 27);
    }

    #[test]
    fn survives_a_terminal_far_too_small_to_use() {
        // Not useful at this size, but it must not panic: a user will resize a terminal
        // to something absurd sooner or later.
        let mut app = app();
        app.on_event(groups_arrived(vec![group_row("misc.test", 1, 3)]));
        app.overlay = Overlay::Help;

        for (width, height) in [(1, 1), (2, 3), (10, 4), (20, 2), (5, 20)] {
            render(&mut app, width, height);
        }
    }
}
