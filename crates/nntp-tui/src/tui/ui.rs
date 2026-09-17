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

    match app.overlay {
        Overlay::None => {}
        Overlay::Help => draw_overlay(frame, "Keys", help_text(), body),
        Overlay::Messages => draw_overlay(frame, "Messages", messages_text(app), body),
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

    let subtitle = if app.editing_filter {
        Some(format!("/{}", app.filter))
    } else if app.filter.is_empty() {
        (!app.groups.is_empty()).then(|| format!("{}", app.groups.len()))
    } else {
        Some(format!(
            "/{} — {} of {}",
            app.filter,
            visible.len(),
            app.groups.len()
        ))
    };

    let items: Vec<ListItem<'_>> = visible
        .iter()
        .filter_map(|index| app.groups.get(*index))
        .map(|group| {
            let count = if group.is_empty() {
                Span::from("  empty").dim()
            } else {
                // "≤" because the watermarks bound the count rather than stating it.
                Span::from(format!("  \u{2264}{}", group.article_bound())).dim()
            };
            ListItem::new(Line::from(vec![Span::from(group.name.to_string()), count]))
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
    let subtitle = app
        .group
        .as_ref()
        .map(|summary| format!("{} ({})", summary.name, app.articles.len()));

    let items: Vec<ListItem<'_>> = app
        .articles
        .iter()
        .map(|record| {
            let subject = if record.subject.is_empty() {
                "(no subject)".to_owned()
            } else {
                record.subject.clone()
            };
            // A reply is marked rather than indented: real threads arrive out of order
            // and with missing parents, so an indent would be a lie until v0.2 builds
            // the tree properly.
            let marker = if record.is_reply() { "› " } else { "  " };

            ListItem::new(Line::from(vec![
                Span::from(marker).dim(),
                Span::from(subject),
            ]))
        })
        .collect();

    let mut state = ListState::default();
    if !app.articles.is_empty() {
        state.select(Some(app.article_cursor.min(app.articles.len() - 1)));
    }

    let placeholder = if app.articles.is_empty() {
        Some(if app.group.is_some() {
            "no articles"
        } else {
            "select a group and press Enter"
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
    const HINT: &str = " ?: help  m: messages  q: quit";
    let hint_width = u16::try_from(HINT.chars().count()).unwrap_or(u16::MAX);
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
        Paragraph::new(Line::from(Span::from(HINT).dim())).alignment(Alignment::Right),
        right,
    );
}

/// Draws a centred overlay over the panes.
fn draw_overlay(frame: &mut Frame<'_>, title: &str, text: Text<'static>, area: Rect) {
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

    // Clear first, or the panes show through the overlay.
    frame.render_widget(Clear, centred);
    frame.render_widget(
        Paragraph::new(text)
            .block(
                Block::bordered()
                    .border_type(BorderType::Double)
                    .border_style(Style::new().fg(ACCENT))
                    .title(format!(" {title} — Esc to close ")),
            )
            .wrap(Wrap { trim: false }),
        centred,
    );
}

fn help_text() -> Text<'static> {
    const ROWS: [(&str, &str); 14] = [
        ("Tab / Shift-Tab", "next / previous pane"),
        ("h l  ← →", "move focus left / right"),
        ("j k  ↓ ↑", "move down / up"),
        ("Ctrl-d / Ctrl-u", "page down / up"),
        ("PageDown / PageUp", "page down / up"),
        ("g / G", "first / last"),
        ("Enter", "open the group or article under the cursor"),
        ("n / p", "next / previous article, opening it"),
        ("/", "filter groups by name or description"),
        ("Esc", "clear the filter, or close an overlay"),
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
        Span::from("  Article counts are shown as ≤n: LIST ACTIVE reports watermarks, and expiry leaves gaps.")
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
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::config::UiConfig;
    use crate::tui::protocol::{Event, GroupRow};

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
        App::new(&UiConfig::default())
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
    fn lists_groups_with_a_bounded_count() {
        let mut app = app();
        app.on_event(Event::Groups(vec![
            group_row("comp.lang.rust", 1, 10),
            group_row("empty.group", 1, 0),
        ]));

        let screen = render(&mut app, 100, 20);
        assert!(screen.contains("comp.lang.rust"), "{screen}");
        assert!(screen.contains("≤10"), "{screen}");
        // An empty group says so rather than claiming zero articles.
        assert!(screen.contains("empty"), "{screen}");
    }

    #[test]
    fn shows_the_filter_and_how_much_it_hides() {
        let mut app = app();
        app.on_event(Event::Groups(vec![
            group_row("comp.lang.rust", 1, 10),
            group_row("misc.test", 1, 3),
        ]));
        app.filter = "misc".to_owned();

        let screen = render(&mut app, 100, 20);
        assert!(screen.contains("/misc"), "{screen}");
        assert!(screen.contains("1 of 2"), "{screen}");
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
    fn the_help_overlay_covers_the_panes() {
        let mut app = app();
        app.overlay = Overlay::Help;

        let screen = render(&mut app, 100, 24);
        assert!(screen.contains("Keys"), "{screen}");
        assert!(screen.contains("Esc to close"), "{screen}");
        assert!(screen.contains("filter groups"), "{screen}");
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
        app.on_event(Event::Groups(vec![group_row("misc.test", 1, 3)]));
        app.overlay = Overlay::Help;

        for (width, height) in [(1, 1), (2, 3), (10, 4), (20, 2), (5, 20)] {
            render(&mut app, width, height);
        }
    }
}
