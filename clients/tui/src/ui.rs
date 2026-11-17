// SPDX-FileCopyrightText: 2026 Chris Palmer <chris@red-oxide.org>
// SPDX-License-Identifier: GPL-3.0-only

//! Drawing.

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span, Text},
    widgets::{Block, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::app::{ALL_ROW, App, FIXED_ROWS, Pane, STARRED_ROW, UP_NEXT_ROW};

const HELP: &str = "j/k move  enter open  / search  m read  M read below  s star  o browser  p play  a up next  A all read  u unread/all/read  r reload  R refresh  q quit";
const UP_NEXT_HELP: &str =
    "j/k move  J/K reorder  a remove  n play next  p play  enter open  h back  q quit";
const PLAYER_HELP: &str = "p pause  ,/. seek  [/] chapter  +/- speed  x stop";

pub fn draw(frame: &mut Frame, app: &App) {
    let bar = u16::from(app.playing.is_some());
    let [body, player, status] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(bar),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    draw_player(frame, app, player);
    let [feeds, right] =
        Layout::horizontal([Constraint::Percentage(25), Constraint::Min(20)]).areas(body);

    draw_feeds(frame, app, feeds);
    if app.reader.is_some() {
        let [items, reader] =
            Layout::vertical([Constraint::Percentage(35), Constraint::Min(5)]).areas(right);
        draw_items(frame, app, items);
        draw_reader(frame, app, reader);
    } else {
        draw_items(frame, app, right);
    }

    let line = if let Some(text) = &app.editing {
        Line::from(vec![
            Span::raw("/").cyan().bold(),
            Span::raw(text.clone()),
            Span::raw("█").cyan(),
            Span::raw("   enter search  esc cancel").dark_gray(),
        ])
    } else if app.status.is_empty() && app.showing_queue() && app.focus == Pane::Items {
        Line::from(UP_NEXT_HELP).dark_gray()
    } else if app.status.is_empty() {
        Line::from(HELP).dark_gray()
    } else if app.status.starts_with("error") {
        Line::from(app.status.as_str()).red()
    } else {
        Line::from(app.status.as_str()).yellow()
    };
    frame.render_widget(line, status);
}

fn block(title: String, focused: bool) -> Block<'static> {
    let style = if focused {
        Style::new().fg(Color::Cyan)
    } else {
        Style::new().fg(Color::DarkGray)
    };
    Block::bordered().title(title).border_style(style)
}

fn highlight(focused: bool) -> Style {
    if focused {
        Style::new().bg(Color::Cyan).fg(Color::Black)
    } else {
        Style::new().add_modifier(Modifier::REVERSED)
    }
}

fn draw_feeds(frame: &mut Frame, app: &App, area: Rect) {
    let mut rows = Vec::with_capacity(app.feeds.len() + FIXED_ROWS);
    rows.insert(
        ALL_ROW,
        ListItem::new(format!("All items ({})", app.total_unread())).bold(),
    );
    rows.insert(STARRED_ROW, ListItem::new("★ Starred").bold());
    rows.insert(
        UP_NEXT_ROW,
        ListItem::new(format!("≡ Up Next ({})", app.queue.len())).bold(),
    );
    for feed in &app.feeds {
        let title = Span::raw(feed.title.clone());
        let mut spans = vec![if feed.archived {
            title.dark_gray()
        } else {
            title
        }];
        if feed.archived {
            spans.push(Span::raw(" (removed)").dark_gray().italic());
        }
        if feed.unread > 0 {
            spans.push(Span::raw(format!(" ({})", feed.unread)).cyan());
        }
        if feed.last_error.is_some() {
            spans.push(Span::raw(" !").red().bold());
        }
        rows.push(ListItem::new(Line::from(spans)));
    }
    let focused = app.focus == Pane::Feeds;
    let list = List::new(rows)
        .block(block(" Feeds ".into(), focused))
        .highlight_style(highlight(focused));
    let mut state = ListState::default().with_selected(Some(app.feed_row));
    frame.render_stateful_widget(list, area, &mut state);
}

fn date(ts: i64) -> String {
    jiff::Timestamp::from_second(ts).map_or_else(
        |_| String::new(),
        |t| {
            t.to_zoned_datetime(jiff::tz::TimeZone::system())
                .strftime("%b %d")
                .to_string()
        },
    )
}

/// `m:ss`, or `h:mm:ss` from an hour.
pub fn clock(seconds: f64) -> String {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let s = seconds.max(0.0) as u64;
    let (h, m, s) = (s / 3600, s / 60 % 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// The line about the episode playing: title, time, chapter and speed.
fn draw_player(frame: &mut Frame, app: &App, area: Rect) {
    let Some(playing) = &app.playing else { return };
    let state = &playing.state;
    let mut spans = vec![
        Span::raw(if state.paused { "⏸ " } else { "▶ " })
            .green()
            .bold(),
        Span::raw(playing.item.title.clone()).bold(),
        Span::raw("  "),
        Span::raw(match state.duration {
            Some(d) => format!("{} / {}", clock(state.position), clock(d)),
            None => clock(state.position),
        })
        .cyan(),
    ];
    if let Some(chapter) = playing.chapter().and_then(|c| c.title.as_ref()) {
        spans.push(Span::raw(format!("  · {chapter}")).magenta());
    }
    if (state.speed - 1.0).abs() > f64::EPSILON {
        spans.push(Span::raw(format!("  {}x", state.speed)).yellow());
    }
    if playing.video {
        spans.push(Span::raw("  (video)").dark_gray());
    }
    spans.push(Span::raw(format!("   {PLAYER_HELP}")).dark_gray());
    frame.render_widget(Line::from(spans), area);
}

/// The column marking episodes: playing, started, or not yet.
fn media_mark(app: &App, item: &pressbox_api::Item) -> &'static str {
    if item.media.is_none() {
        "  "
    } else if app.playing.as_ref().is_some_and(|p| p.item.id == item.id) {
        "▶ "
    } else if item.position > 0 {
        "◐ "
    } else {
        "♪ "
    }
}

fn draw_items(frame: &mut Frame, app: &App, area: Rect) {
    let show_feed = app.feed_row < FIXED_ROWS;
    let rows: Vec<ListItem> = app
        .items
        .iter()
        .map(|item| {
            let mut spans = vec![
                Span::raw(if item.read { "  " } else { "● " }).cyan(),
                Span::raw(if item.starred { "★ " } else { "  " }).yellow(),
                Span::raw(media_mark(app, item)).green(),
                Span::raw(format!("{:<7}", date(item.published))).dark_gray(),
            ];
            if show_feed {
                let title: String = app.feed_title(&item.feed_id).chars().take(16).collect();
                spans.push(Span::raw(format!("{title:<17}")).magenta());
            }
            let title = Span::raw(item.title.clone());
            spans.push(if item.read { title } else { title.bold() });
            ListItem::new(Line::from(spans))
        })
        .collect();
    let focused = app.focus == Pane::Items;
    let more = if app.next_page.is_some() { "+" } else { "" };
    let title = if let Some(q) = &app.search {
        format!(" Search {q:?}: {}{more} (esc clears) ", app.items.len())
    } else if app.showing_queue() {
        format!(" Up Next: {} ", app.items.len())
    } else {
        let what = if app.filters_by_read() {
            app.show.label()
        } else {
            "all"
        };
        format!(" Items: {} {what}{more} ", app.items.len())
    };
    let list = List::new(rows)
        .block(block(title, focused))
        .highlight_style(highlight(focused));
    let mut state =
        ListState::default().with_selected((!app.items.is_empty()).then_some(app.item_row));
    frame.render_stateful_widget(list, area, &mut state);
}

fn draw_reader(frame: &mut Frame, app: &App, area: Rect) {
    let Some(reader) = &app.reader else { return };
    let item = &reader.item;
    let width = usize::from(area.width.saturating_sub(2)).max(20);
    let mut meta = vec![app.feed_title(&item.feed_id).to_owned()];
    if let Some(author) = &item.author {
        meta.push(author.clone());
    }
    meta.push(date(item.published));
    let mut lines = vec![
        Line::from(item.title.clone()).bold(),
        Line::from(meta.join(" · ")).dark_gray(),
    ];
    if let Some(url) = &item.url {
        lines.push(Line::from(url.clone()).blue().underlined());
    }
    let playing = app
        .playing
        .as_ref()
        .filter(|p| p.item.id == item.id)
        .map(|p| p.state.position);
    if let Some(media) = &item.media {
        lines.push(media_line(item, media).green());
        let mut keys = "p play".to_owned();
        if media.video().is_some() {
            keys.push_str("  V video");
        }
        if !media.transcripts.is_empty() {
            keys.push_str(if reader.show_transcript {
                "  t text"
            } else {
                "  t transcript"
            });
        }
        keys.push_str(if app.is_queued(item.id) {
            "  a remove from up next  n play next"
        } else {
            "  a up next  n play next"
        });
        lines.push(Line::from(keys).dark_gray());
    }
    lines.push(Line::default());
    if reader.show_transcript {
        lines.extend(transcript_lines(reader, playing));
    } else {
        lines.extend(chapter_lines(&reader.chapters, playing));
        let html = item
            .content
            .as_deref()
            .or(item.summary.as_deref())
            .unwrap_or("");
        let body = html2text::from_read(html.as_bytes(), width).unwrap_or_else(|_| html.to_owned());
        lines.extend(body.lines().map(|l| Line::from(l.to_owned())));
    }

    let flags = match (item.read, item.starred) {
        (_, true) => " ★ ",
        (false, false) => " unread ",
        (true, false) => "",
    };
    let paragraph = Paragraph::new(Text::from(lines))
        .block(block(format!(" Reader{flags}"), app.focus == Pane::Reader))
        .wrap(Wrap { trim: false })
        .scroll((reader.scroll, 0));
    frame.render_widget(paragraph, area);
}

/// "♪ 1:02:03 · S2E10 · 23:10 played" for the reader.
fn media_line(item: &pressbox_api::Item, media: &pressbox_api::Media) -> Line<'static> {
    let mut parts = vec![if media.video().is_some() {
        "▶ video".to_owned()
    } else {
        "♪ audio".to_owned()
    }];
    if let Some(d) = media.duration {
        parts.push(clock(f64::from(d)));
    }
    match (media.season, media.episode) {
        (Some(s), Some(e)) => parts.push(format!("S{s}E{e}")),
        (None, Some(e)) => parts.push(format!("episode {e}")),
        _ => {}
    }
    if item.position > 0 {
        parts.push(format!("{} played", clock(f64::from(item.position))));
    }
    Line::from(parts.join(" · "))
}

/// The chapter list, the current one marked while the episode plays.
fn chapter_lines(chapters: &[pressbox_api::Chapter], playing: Option<f64>) -> Vec<Line<'static>> {
    let visible: Vec<_> = chapters.iter().filter(|c| !c.hidden).collect();
    if visible.is_empty() {
        return Vec::new();
    }
    let current = playing.and_then(|at| visible.iter().rposition(|c| c.start <= at));
    let mut lines = vec![Line::from("Chapters").bold()];
    for (i, c) in visible.iter().enumerate() {
        let title = c.title.clone().unwrap_or_default();
        let line = Line::from(format!("  {:>8}  {title}", clock(c.start)));
        lines.push(if current == Some(i) {
            line.green().bold()
        } else {
            line
        });
    }
    lines.push(Line::default());
    lines
}

/// The transcript, one segment a line, the one being heard highlighted.
fn transcript_lines(reader: &crate::app::Reader, playing: Option<f64>) -> Vec<Line<'static>> {
    let Some(Some(transcript)) = &reader.transcript else {
        return vec![Line::from("No transcript.").dark_gray()];
    };
    transcript
        .segments
        .iter()
        .map(|seg| {
            let mut spans = vec![Span::raw(format!("{:>8}  ", clock(seg.start))).dark_gray()];
            if let Some(speaker) = &seg.speaker {
                spans.push(Span::raw(format!("{speaker}: ")).magenta().bold());
            }
            spans.push(Span::raw(seg.text.clone()));
            let line = Line::from(spans);
            if playing.is_some_and(|at| seg.start <= at && at < seg.end) {
                line.yellow()
            } else {
                line
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use pressbox_api::{Feed, Item};
    use ratatui::{Terminal, backend::TestBackend};

    use super::*;
    use crate::app::{Reader, Show};

    fn feed(id: &str, unread: u32) -> Feed {
        Feed {
            id: id.into(),
            title: format!("Feed {id}"),
            url: format!("https://{id}.example/"),
            site_url: None,
            tags: Vec::new(),
            unread,
            total: unread,
            last_fetched: None,
            last_error: None,
            archived: false,
            image: None,
            medium: None,
            episodes: 0,
            unread_episodes: 0,
        }
    }

    fn item(id: i64, feed: &str, read: bool, starred: bool) -> Item {
        Item {
            id,
            feed_id: feed.into(),
            title: format!("Story {id}"),
            url: Some(format!("https://{feed}.example/{id}")),
            author: Some("Ana".into()),
            summary: Some("<p>The summary.</p>".into()),
            content: None,
            published: 1_793_700_000,
            read,
            starred,
            media: None,
            position: 0,
        }
    }

    /// An app with two feeds (one failing, one removed) and two items.
    fn app() -> App {
        let mut app = App::new();
        let mut failing = feed("a", 2);
        failing.last_error = Some("HTTP 500".into());
        let mut removed = feed("old", 0);
        removed.archived = true;
        app.feeds = vec![failing, feed("b", 0), removed];
        app.items = vec![item(1, "a", false, true), item(2, "b", true, false)];
        app
    }

    /// The screen as text, one string per row.
    fn screen(app: &App, width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();
        let buffer = terminal.backend().buffer();
        buffer
            .content()
            .chunks(usize::from(width))
            .map(|row| row.iter().map(|cell| cell.symbol()).collect())
            .collect()
    }

    fn contains(rows: &[String], text: &str) -> bool {
        rows.iter().any(|r| r.contains(text))
    }

    #[test]
    fn draws_feeds_items_and_help() {
        let rows = screen(&app(), 120, 12);
        for text in [
            " Feeds ",
            "All items (2)",
            "★ Starred",
            "≡ Up Next (0)",
            "Feed a (2) !",
            "Feed old (removed)",
            " Items: 2 unread ",
            "● ★ ",
            "Feed a",
            "Story 1",
            "Story 2",
            "j/k move",
        ] {
            assert!(
                contains(&rows, text),
                "{text:?} missing:\n{}",
                rows.join("\n")
            );
        }
        assert!(
            !contains(&rows, "Feed b ("),
            "no count when nothing is unread"
        );
    }

    #[test]
    fn titles_follow_the_view() {
        let mut app = app();
        app.next_page = Some(crate::app::NextPage::Cursor("c".into()));
        app.show = Show::Read;
        assert!(contains(&screen(&app, 120, 8), " Items: 2 read+ "));
        app.feed_row = STARRED_ROW;
        assert!(contains(&screen(&app, 120, 8), " Items: 2 all+ "));
        app.search = Some("rust".into());
        assert!(contains(
            &screen(&app, 120, 8),
            " Search \"rust\": 2+ (esc clears) "
        ));

        // One feed: no feed column.
        app.search = None;
        app.feed_row = FIXED_ROWS;
        let rows = screen(&app, 120, 8);
        assert!(
            !rows
                .iter()
                .any(|r| r.contains("Feed a") && r.contains("Story 1"))
        );
    }

    #[test]
    fn draws_the_reader() {
        let mut app = app();
        app.reader = Some(Reader::new(item(1, "a", false, false)));
        app.focus = Pane::Reader;
        let rows = screen(&app, 100, 24);
        for text in [
            " Reader unread ",
            "Story 1",
            "Feed a · Ana · ",
            "https://a.example/1",
            "The summary.",
        ] {
            assert!(
                contains(&rows, text),
                "{text:?} missing:\n{}",
                rows.join("\n")
            );
        }
        app.reader.as_mut().unwrap().item.starred = true;
        assert!(contains(&screen(&app, 100, 24), " Reader ★ "));
        let reader = app.reader.as_mut().unwrap();
        (reader.item.starred, reader.item.read) = (false, true);
        let rows = screen(&app, 100, 24);
        let title = rows.iter().find(|r| r.contains(" Reader")).unwrap();
        assert!(
            !title.contains("unread") && !title.contains('★'),
            "nothing to flag: {title}"
        );
    }

    #[test]
    fn status_line_shows_prompts_and_messages() {
        let mut app = app();
        app.editing = Some("nix".into());
        let rows = screen(&app, 100, 6);
        assert!(
            rows[5].starts_with("/nix█   enter search  esc cancel"),
            "{}",
            rows[5]
        );

        app.editing = None;
        app.status = "error: offline".into();
        let mut terminal = Terminal::new(TestBackend::new(100, 6)).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let cell = &terminal.backend().buffer()[(0, 5)];
        assert_eq!((cell.symbol(), cell.fg), ("e", Color::Red));

        app.status = "reloaded".into();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let cell = &terminal.backend().buffer()[(0, 5)];
        assert_eq!((cell.symbol(), cell.fg), ("r", Color::Yellow));
    }

    #[test]
    fn dates_are_short() {
        assert_eq!(date(i64::MAX), "");
        assert_eq!(date(1_793_700_000).len(), 6, "like \"Nov 03\"");
    }

    fn episode(id: i64) -> Item {
        use pressbox_api::{Enclosure, Media, TranscriptFile};
        let mut item = item(id, "a", false, false);
        item.media = Some(Media {
            enclosures: vec![Enclosure {
                url: "https://a.example/1.mp4".into(),
                mime: Some("video/mp4".into()),
                ..Enclosure::default()
            }],
            duration: Some(3723),
            season: Some(2),
            episode: Some(10),
            transcripts: vec![TranscriptFile::default()],
            ..Media::default()
        });
        item.position = 65;
        item
    }

    fn chapters() -> Vec<pressbox_api::Chapter> {
        let chapter = |start: f64, title: Option<&str>, hidden| pressbox_api::Chapter {
            start,
            title: title.map(Into::into),
            hidden,
            ..pressbox_api::Chapter::default()
        };
        vec![
            chapter(0.0, Some("Intro"), false),
            chapter(30.0, None, true),
            chapter(60.0, Some("News"), false),
            chapter(4000.0, None, false),
        ]
    }

    fn playing(item: Item, paused: bool, speed: f64, video: bool) -> crate::app::Playing {
        crate::app::Playing::new(
            item,
            chapters(),
            crate::player::PlayerState {
                position: 70.0,
                duration: Some(3723.0),
                paused,
                speed,
                ended: false,
            },
            video,
        )
    }

    #[test]
    fn draws_episodes_and_the_player() {
        let mut app = app();
        let mut started = episode(3);
        started.title = "Started".into();
        let mut fresh = episode(4);
        fresh.position = 0;
        app.items = vec![episode(1), item(2, "a", false, false), started, fresh];
        app.playing = Some(playing(episode(1), false, 1.5, true));
        let rows = screen(&app, 160, 12);
        for text in [
            "▶ Story 1  1:10 / 1:02:03  · News  1.5x  (video)   p pause",
            "▶ Nov",
            "◐ Nov",
            "♪ Nov",
        ] {
            assert!(
                contains(&rows, text),
                "{text:?} missing:\n{}",
                rows.join("\n")
            );
        }

        // Paused, normal speed, audio, a long way in, no chapter title.
        let mut p = playing(episode(1), true, 1.0, false);
        p.state.duration = None;
        p.chapters.clear();
        app.playing = Some(p);
        let rows = screen(&app, 160, 12);
        assert!(
            contains(&rows, "⏸ Story 1  1:10   p pause"),
            "{}",
            rows.join("\n")
        );
    }

    #[test]
    fn up_next_has_its_own_title_and_keys() {
        let mut app = app();
        app.queue = vec![episode(1)];
        app.items = vec![episode(1)];
        app.feed_row = crate::app::UP_NEXT_ROW;
        app.focus = Pane::Items;
        let rows = screen(&app, 160, 12);
        for text in ["≡ Up Next (1)", " Up Next: 1 ", "J/K reorder  a remove"] {
            assert!(
                contains(&rows, text),
                "{text:?} missing:\n{}",
                rows.join("\n")
            );
        }
        app.reader = Some(Reader::new(episode(1)));
        app.focus = Pane::Reader;
        let rows = screen(&app, 160, 30);
        assert!(contains(&rows, "a remove from up next  n play next"));
    }

    #[test]
    fn the_reader_shows_episodes() {
        let mut app = app();
        let mut reader = Reader::new(episode(1));
        reader.chapters = chapters();
        app.reader = Some(reader);
        app.focus = Pane::Reader;
        app.playing = Some(playing(episode(1), false, 1.0, false));
        let rows = screen(&app, 100, 30);
        for text in [
            "▶ video · 1:02:03 · S2E10 · 1:05 played",
            "p play  V video  t transcript  a up next  n play next",
            "Chapters",
            "    0:00  Intro",
            "    1:00  News",
            "1:06:40",
        ] {
            assert!(
                contains(&rows, text),
                "{text:?} missing:\n{}",
                rows.join("\n")
            );
        }
        assert!(!contains(&rows, "0:30"), "hidden chapters aren't listed");

        // The transcript, the segment being heard highlighted.
        let reader = app.reader.as_mut().unwrap();
        reader.show_transcript = true;
        reader.transcript = Some(Some(pressbox_api::Transcript {
            segments: vec![
                pressbox_api::Segment {
                    start: 60.0,
                    end: 80.0,
                    speaker: Some("Ann".into()),
                    text: "Now.".into(),
                },
                pressbox_api::Segment {
                    start: 80.0,
                    end: 90.0,
                    speaker: None,
                    text: "Later.".into(),
                },
            ],
        }));
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let rows = screen(&app, 100, 30);
        assert!(contains(&rows, "t text"));
        let now = rows.iter().position(|r| r.contains("Ann: Now.")).unwrap();
        let later = rows.iter().position(|r| r.contains("Later.")).unwrap();
        let buffer = terminal.backend().buffer();
        // The colour of the first cell of `text` in row `row`.
        let fg = |row: usize, text: &str| {
            let byte = rows[row].find(text).unwrap();
            let col = u16::try_from(rows[row][..byte].chars().count()).unwrap();
            buffer[(col, u16::try_from(row).unwrap())].fg
        };
        assert_eq!(fg(now, "Now."), Color::Yellow);
        assert_ne!(fg(later, "Later."), Color::Yellow);

        app.reader.as_mut().unwrap().transcript = Some(None);
        assert!(contains(&screen(&app, 100, 30), "No transcript."));

        // Audio-only, no numbers: the short forms.
        let mut plain = episode(5);
        let media = plain.media.as_mut().unwrap();
        (media.season, media.duration, media.enclosures[0].mime) = (None, None, None);
        media.transcripts.clear();
        plain.position = 0;
        app.reader = Some(Reader::new(plain.clone()));
        let rows = screen(&app, 100, 30);
        assert!(
            contains(&rows, "♪ audio · episode 10"),
            "{}",
            rows.join("\n")
        );
        assert!(!contains(&rows, "V video") && !contains(&rows, "Chapters"));
        plain.media.as_mut().unwrap().episode = None;
        app.reader = Some(Reader::new(plain));
        assert!(contains(&screen(&app, 100, 30), "♪ audio"));
    }

    #[test]
    fn clocks() {
        assert_eq!(clock(0.0), "0:00");
        assert_eq!(clock(-5.0), "0:00");
        assert_eq!(clock(65.9), "1:05");
        assert_eq!(clock(3723.0), "1:02:03");
    }
}
