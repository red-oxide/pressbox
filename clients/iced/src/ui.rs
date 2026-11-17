// SPDX-FileCopyrightText: 2026 Chris Palmer <chris@red-oxide.org>
// SPDX-License-Identifier: GPL-3.0-only

//! Layout: the pairing screen, and the reader's three panes (sources,
//! items, article).

use iced::{
    Alignment, Element, Fill, Font, Length, Theme,
    widget::{button, column, container, row, rule, scrollable, slider, space, text, text_input},
};
use pressbox_api::{Chapter, Item, Media, Transcript};

use crate::{
    app::{App, Flag, Message, Playback, QueueChange, Reader, Screen, Setup, Source},
    controls,
    theme::Colors,
};

const SIDEBAR: f32 = 230.0;
const LIST: f32 = 360.0;

pub fn view(app: &App) -> Element<'_, Message> {
    match &app.screen {
        Screen::Setup(setup) => setup_view(setup),
        Screen::Reader(reader) => reader_view(reader),
    }
}

fn setup_view(setup: &Setup) -> Element<'_, Message> {
    let mut form = column![
        text("pressbox").size(32),
        text("Pair this computer with your pressbox server.").style(muted),
        space().height(8),
        text("Server").size(14),
        text_input("https://rss.example.org", &setup.server)
            .on_input_maybe((!setup.busy).then_some(Message::ServerChanged))
            .on_submit(Message::Pair)
            .padding(8),
        text("Device name").size(14),
        text_input("laptop", &setup.device)
            .on_input_maybe((!setup.busy).then_some(Message::DeviceChanged))
            .on_submit(Message::Pair)
            .padding(8),
        button(text("Pair this device"))
            .on_press_maybe((!setup.busy).then_some(Message::Pair))
            .padding([8, 16]),
    ]
    .spacing(8)
    .max_width(420);

    if let Some(code) = &setup.code {
        form = form.push(space().height(8)).push(
            column![
                text("Approve this code:").style(muted),
                text(code).size(36).font(Font::MONOSPACE),
                text(format!(
                    "On the server: sudo pressboxctl approve {code}\n\
                     Or from a paired device: pressbox approve {code}"
                ))
                .size(13)
                .font(Font::MONOSPACE)
                .style(muted),
                text("Waiting for approval…").size(13).style(muted),
            ]
            .spacing(6),
        );
    }
    if let Some(error) = &setup.error {
        form = form.push(text(error).style(error_text));
    }

    container(form).center(Fill).padding(24).into()
}

fn reader_view(reader: &Reader) -> Element<'_, Message> {
    let body = row![
        sidebar(reader),
        rule::vertical(1),
        item_list(reader),
        rule::vertical(1),
        article(reader),
    ]
    .height(Fill);

    let mut page = column![body];
    if let Some(playback) = &reader.playback {
        page = page.push(rule::horizontal(1)).push(player_bar(playback));
    }
    if let Some(banner) = banner(reader) {
        page = page.push(rule::horizontal(1)).push(banner);
    }
    container(page).style(background).into()
}

fn banner(reader: &Reader) -> Option<Element<'_, Message>> {
    let (message, style): (&str, fn(&Theme) -> text::Style) = match (&reader.error, &reader.status)
    {
        (Some(e), _) => (e, error_text),
        (None, Some(s)) => (s, muted),
        (None, None) => return None,
    };
    Some(
        row![
            text(message).style(style).width(Fill),
            button(text("Dismiss").size(13))
                .style(button::text)
                .on_press(Message::Dismiss),
        ]
        .align_y(Alignment::Center)
        .padding([4, 12])
        .into(),
    )
}

fn sidebar(reader: &Reader) -> Element<'_, Message> {
    let total_unread: u32 = reader.feeds.iter().map(|f| f.unread).sum();
    let mut list = column![
        source_button("Unread", Some(total_unread), Source::Unread, reader),
        source_button("All items", None, Source::All, reader),
        source_button("Starred", None, Source::Starred, reader),
    ]
    .spacing(2);
    // Servers without a queue: no Up Next.
    if let Some(queue) = &reader.queue {
        let n = u32::try_from(queue.len()).unwrap_or(u32::MAX);
        list = list.push(source_button("Up Next", Some(n), Source::UpNext, reader));
    }

    let tags = reader.tags();
    if !tags.is_empty() {
        list = list.push(heading("Tags"));
        for (tag, unread) in tags {
            list = list.push(source_button(
                format!("#{tag}"),
                Some(unread),
                Source::Tag(tag),
                reader,
            ));
        }
    }

    list = list.push(heading("Feeds"));
    for feed in reader.feeds.iter().filter(|f| !f.archived) {
        let label = if feed.last_error.is_some() {
            format!("{} ⚠", feed.title)
        } else {
            feed.title.clone()
        };
        list = list.push(source_button(
            label,
            Some(feed.unread),
            Source::Feed(feed.id.clone()),
            reader,
        ));
    }
    let archived: Vec<_> = reader.feeds.iter().filter(|f| f.archived).collect();
    if !archived.is_empty() {
        list = list.push(heading("Archived"));
        for feed in archived {
            list = list.push(source_button(
                feed.title.clone(),
                None,
                Source::Feed(feed.id.clone()),
                reader,
            ));
        }
    }

    let footer = column![
        row![
            text_input("Approve code", &reader.approve_code)
                .on_input(Message::ApproveCodeChanged)
                .on_submit(Message::Approve)
                .size(13)
                .padding(6),
            button(text("Approve").size(13))
                .on_press(Message::Approve)
                .padding([6, 10]),
        ]
        .spacing(6),
        text(identity(reader)).size(12).style(muted),
        row![
            button(text("Refresh").size(13))
                .style(button::text)
                .on_press(Message::Refresh),
            button(text("Sign out").size(13))
                .style(button::text)
                .on_press(Message::SignOut),
        ],
    ]
    .spacing(6);

    column![
        scrollable(list.padding([8, 8])).height(Fill),
        container(footer).padding(8),
    ]
    .width(SIDEBAR)
    .into()
}

fn identity(reader: &Reader) -> String {
    let mut parts = vec![reader.client.server().to_owned()];
    if let Some(me) = &reader.me {
        parts.push(match &me.device {
            Some(d) => format!("{} on {d}", me.user),
            None => me.user.clone(),
        });
    }
    if let Some(v) = &reader.version {
        parts.push(format!("pressboxd {}", v.version));
    }
    parts.join("\n")
}

fn heading(label: &str) -> Element<'_, Message> {
    container(text(label).size(12).style(muted))
        .padding(iced::Padding {
            top: 14.0,
            bottom: 4.0,
            left: 8.0,
            right: 8.0,
        })
        .into()
}

fn source_button<'a>(
    label: impl text::IntoFragment<'a>,
    count: Option<u32>,
    source: Source,
    reader: &Reader,
) -> Element<'a, Message> {
    let selected = reader.source == source;
    let mut content = row![
        text(label)
            .size(14)
            .width(Fill)
            .wrapping(text::Wrapping::None)
    ]
    .align_y(Alignment::Center);
    if let Some(n) = count.filter(|n| *n > 0) {
        content = content.push(text(n.to_string()).size(12).style(muted));
    }
    button(content)
        .width(Fill)
        .padding([5, 8])
        .style(move |theme, status| row_style(theme, status, selected))
        .on_press(Message::Select(source))
        .into()
}

fn item_list(reader: &Reader) -> Element<'_, Message> {
    let header = column![
        row![
            text(reader.source_title()).size(18).width(Fill),
            button(text("Mark all read").size(13))
                .style(button::text)
                .on_press_maybe(
                    (!matches!(reader.source, Source::Starred | Source::UpNext))
                        .then_some(Message::MarkAllRead)
                ),
        ]
        .align_y(Alignment::Center),
        text_input("Search", &reader.search)
            .on_input(Message::SearchChanged)
            .on_submit(Message::SearchSubmit)
            .padding(6),
    ]
    .spacing(8)
    .padding(10);

    let mut items = column![].spacing(1);
    for item in &reader.items {
        items = items.push(item_row(item, reader));
    }
    if reader.items.is_empty() && !reader.loading {
        items = items.push(
            container(text("Nothing here.").style(muted))
                .padding(20)
                .center_x(Fill),
        );
    }
    if reader.loading {
        items = items.push(container(text("Loading…").style(muted)).padding(12));
    } else if reader.next_cursor.is_some() {
        items = items.push(
            container(button(text("Load more")).on_press(Message::LoadMore))
                .padding(12)
                .center_x(Fill),
        );
    }

    column![header, rule::horizontal(1), scrollable(items).height(Fill)]
        .width(LIST)
        .into()
}

fn item_row<'a>(item: &'a Item, reader: &'a Reader) -> Element<'a, Message> {
    let selected = reader.selected == Some(item.id);
    let unread = !item.read;
    let mut meta = vec![reader.feed_title(&item.feed_id)];
    meta.push(date(item.published));
    let title = text(&item.title)
        .size(14)
        .font(if unread {
            Font {
                weight: iced::font::Weight::Bold,
                ..Font::DEFAULT
            }
        } else {
            Font::DEFAULT
        })
        .style(move |theme: &Theme| text::Style {
            color: Some(if unread {
                Colors::of(theme).text
            } else {
                Colors::of(theme).muted
            }),
        });
    let mut top = row![title.width(Fill)].spacing(6);
    if let Some(mark) = media_mark(item, reader) {
        top = top.push(text(mark).size(13).style(muted));
    }
    if item.starred {
        top = top.push(text("★").size(14).style(star));
    }
    button(column![top, text(meta.join(" · ")).size(12).style(muted)].spacing(2))
        .width(Fill)
        .padding([8, 10])
        .style(move |theme, status| row_style(theme, status, selected))
        .on_press(Message::OpenItem(item.id))
        .into()
}

fn article(reader: &Reader) -> Element<'_, Message> {
    let Some(item) = &reader.article else {
        return container(text("Select an item to read it.").style(muted))
            .center(Fill)
            .into();
    };

    let mut meta = vec![reader.feed_title(&item.feed_id)];
    if let Some(author) = &item.author {
        meta.push(author.clone());
    }
    meta.push(date(item.published));

    let actions = row![
        button(
            text(if item.starred {
                "★ Starred"
            } else {
                "☆ Star"
            })
            .size(13)
        )
        .style(button::secondary)
        .on_press(Message::Toggle(Flag::Starred)),
        button(
            text(if item.read {
                "Mark unread"
            } else {
                "Mark read"
            })
            .size(13)
        )
        .style(button::secondary)
        .on_press(Message::Toggle(Flag::Read)),
        button(text("Open in browser").size(13))
            .style(button::secondary)
            .on_press_maybe(item.url.as_ref().map(|_| Message::OpenInBrowser)),
    ]
    .spacing(8);

    let mut body = column![
        text(&item.title).size(24),
        text(meta.join(" · ")).size(13).style(muted),
        actions,
    ]
    .spacing(12)
    .padding(24)
    .max_width(760);
    let playing = reader
        .playback
        .as_ref()
        .filter(|p| p.item.id == item.id && p.player.is_some())
        .map(|p| p.state.position);
    if let Some(media) = &item.media {
        body = body.push(episode(item, media, reader));
    }
    body = body.push(rule::horizontal(1));
    if reader.show_transcript {
        body = body.push(transcript(reader.transcript.as_ref(), playing));
    } else {
        if let Some(chapters) = chapter_list(&reader.chapters, playing) {
            body = body.push(chapters);
        }
        body = body.push(text(&reader.article_text).size(15).line_height(1.5));
    }

    let article = scrollable(container(body).center_x(Fill))
        .width(Length::Fill)
        .height(Fill);
    with_video(reader, article.into())
}

/// Video plays above the article, whichever article is open.
// Not measured: draws GStreamer frames, which only a real player has;
// tests' players have no video.
#[cfg(not(tarpaulin_include))]
fn with_video<'a>(reader: &'a Reader, article: Element<'a, Message>) -> Element<'a, Message> {
    let Some(video) = reader
        .playback
        .as_ref()
        .and_then(|p| p.player.as_ref())
        .and_then(|p| p.video())
    else {
        return article;
    };
    let video = container(
        iced_video_player::VideoPlayer::new(video)
            .width(Fill)
            .height(Fill),
    )
    .height(Length::FillPortion(3));
    column![video, rule::horizontal(1), article].into()
}

/// The list's mark for an episode: playing, started, or not yet.
fn media_mark(item: &Item, reader: &Reader) -> Option<&'static str> {
    item.media.as_ref()?;
    Some(
        if reader
            .playback
            .as_ref()
            .is_some_and(|p| p.item.id == item.id)
        {
            "▶"
        } else if item.position > 0 {
            "◐"
        } else {
            "♪"
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

/// The episode's facts and its play buttons.
fn episode<'a>(item: &'a Item, media: &'a Media, reader: &'a Reader) -> Element<'a, Message> {
    let mut facts = Vec::new();
    if let Some(d) = media.duration {
        facts.push(clock(f64::from(d)));
    }
    match (media.season, media.episode) {
        (Some(s), Some(e)) => facts.push(format!("Season {s}, episode {e}")),
        (None, Some(e)) => facts.push(format!("Episode {e}")),
        _ => {}
    }
    let playing = reader.playback.as_ref().filter(|p| p.item.id == item.id);
    let play_label = match playing {
        Some(p) if !p.video && !p.state.paused => "⏸ Pause".to_owned(),
        Some(p) if !p.video => "▶ Resume".to_owned(),
        _ if item.position > 0 => format!("▶ Resume from {}", clock(f64::from(item.position))),
        _ => "▶ Play".to_owned(),
    };
    let mut buttons = row![
        button(text(play_label).size(13))
            .style(button::primary)
            .on_press(Message::Play(false))
    ]
    .spacing(8);
    if media.video().is_some() {
        let label = match playing {
            Some(p) if p.video && !p.state.paused => "⏸ Pause video",
            _ => "Watch video",
        };
        buttons = buttons.push(
            button(text(label).size(13))
                .style(button::secondary)
                .on_press(Message::Play(true)),
        );
    }
    if !media.transcripts.is_empty() {
        let label = if reader.show_transcript {
            "Show text"
        } else {
            "Transcript"
        };
        buttons = buttons.push(
            button(text(label).size(13))
                .style(button::secondary)
                .on_press(Message::ToggleTranscript),
        );
    }
    if reader.queue.is_some() {
        let queued = reader.is_queued(item.id);
        let label = if queued {
            "Remove from Up Next"
        } else {
            "Add to Up Next"
        };
        buttons = buttons.push(
            button(text(label).size(13))
                .style(button::secondary)
                .on_press(Message::Queue(QueueChange::Toggle)),
        );
        let first = reader
            .queue
            .iter()
            .flatten()
            .next()
            .is_some_and(|i| i.id == item.id);
        if !first {
            buttons = buttons.push(
                button(text("Play next").size(13))
                    .style(button::secondary)
                    .on_press(Message::Queue(QueueChange::PlayNext)),
            );
        }
        if queued && reader.showing_queue() {
            buttons = buttons.push(
                button(text("↑").size(13))
                    .style(button::secondary)
                    .on_press(Message::Queue(QueueChange::Move(-1))),
            );
            buttons = buttons.push(
                button(text("↓").size(13))
                    .style(button::secondary)
                    .on_press(Message::Queue(QueueChange::Move(1))),
            );
        }
    }
    let mut part = column![buttons].spacing(6);
    if !facts.is_empty() {
        part = part.push(text(facts.join(" · ")).size(13).style(muted));
    }
    part.into()
}

/// The chapters, as buttons that seek when the episode is playing.
fn chapter_list(chapters: &[Chapter], playing: Option<f64>) -> Option<Element<'_, Message>> {
    let visible: Vec<_> = chapters.iter().filter(|c| !c.hidden).collect();
    if visible.is_empty() {
        return None;
    }
    let current = playing.and_then(|at| visible.iter().rposition(|c| c.start <= at));
    let mut list = column![text("Chapters").size(15)].spacing(2);
    for (i, chapter) in visible.into_iter().enumerate() {
        let title = chapter.title.as_deref().unwrap_or_default();
        let label = row![
            text(clock(chapter.start)).size(13).style(muted).width(64),
            text(title).size(13),
        ];
        let now = current == Some(i);
        list = list.push(
            button(label)
                .width(Fill)
                .padding([3, 6])
                .style(move |theme, status| row_style(theme, status, now))
                .on_press_maybe(playing.map(|_| Message::SeekTo(chapter.start))),
        );
    }
    Some(list.into())
}

/// The transcript, a segment a row, the one being heard highlighted.
fn transcript(
    transcript: Option<&Option<Transcript>>,
    playing: Option<f64>,
) -> Element<'_, Message> {
    let segments = match transcript {
        None => return text("Loading the transcript…").style(muted).into(),
        Some(None) => return text("No transcript.").style(muted).into(),
        Some(Some(t)) => &t.segments,
    };
    let mut list = column![].spacing(2);
    for segment in segments {
        let mut line = row![text(clock(segment.start)).size(13).style(muted).width(64)];
        if let Some(speaker) = &segment.speaker {
            line = line.push(text(format!("{speaker}: ")).size(14).font(Font {
                weight: iced::font::Weight::Bold,
                ..Font::DEFAULT
            }));
        }
        line = line.push(text(&segment.text).size(14).line_height(1.4).width(Fill));
        let now = playing.is_some_and(|at| segment.start <= at && at < segment.end);
        list = list.push(
            button(line)
                .width(Fill)
                .padding([3, 6])
                .style(move |theme, status| row_style(theme, status, now))
                .on_press_maybe(playing.map(|_| Message::SeekTo(segment.start))),
        );
    }
    list.into()
}

/// The bar along the bottom while an episode plays.
fn player_bar(playback: &Playback) -> Element<'_, Message> {
    let state = &playback.state;
    let loading = playback.player.is_none();
    let control = |label: String, message: Message| {
        button(text(label).size(13))
            .style(button::text)
            .on_press_maybe((!loading).then_some(message))
    };
    let mut title = playback.item.title.clone();
    if let Some(chapter) = playback.chapter().and_then(|c| c.title.as_ref()) {
        title = format!("{title} · {chapter}");
    }
    if loading {
        title = format!("{title} (loading…)");
    }
    let position = playback.scrubbing.unwrap_or(state.position);
    let duration = state.duration.unwrap_or(position.max(1.0));
    let time = match state.duration {
        Some(d) => format!("{} / {}", clock(position), clock(d)),
        None => clock(position),
    };
    let progress = slider(0.0..=duration, position, Message::Scrub)
        .on_release(Message::ScrubDone)
        .width(Fill);
    row![
        control("⏮".into(), Message::Chapter(false)),
        control("−10".into(), Message::SeekBy(-controls::BACK)),
        control(
            if state.paused { "▶" } else { "⏸" }.into(),
            Message::PauseToggle
        ),
        control("+30".into(), Message::SeekBy(controls::FORWARD)),
        control("⏭".into(), Message::Chapter(true)),
        column![
            text(title).size(13),
            row![progress, text(time).size(12).style(muted)]
                .spacing(8)
                .align_y(Alignment::Center),
        ]
        .spacing(2)
        .width(Fill),
        control(format!("{}×", state.speed), Message::NextSpeed),
        button(text("■").size(13))
            .style(button::text)
            .on_press(Message::Stop),
    ]
    .spacing(4)
    .padding([6, 12])
    .align_y(Alignment::Center)
    .into()
}

fn date(unix: i64) -> String {
    let Ok(at) = jiff::Timestamp::from_second(unix) else {
        return String::new();
    };
    let local = jiff::tz::TimeZone::system();
    let t = at.to_zoned_datetime(local.clone());
    if t.date() == jiff::Timestamp::now().to_zoned_datetime(local).date() {
        t.strftime("%H:%M").to_string()
    } else {
        t.strftime("%b %-d, %Y").to_string()
    }
}

fn row_style(theme: &Theme, status: button::Status, selected: bool) -> button::Style {
    let palette = theme.extended_palette();
    let background = match (selected, status) {
        (true, _) => Some(palette.background.strong.color.into()),
        (false, button::Status::Hovered) => Some(palette.background.weak.color.into()),
        _ => None,
    };
    button::Style {
        background,
        text_color: Colors::of(theme).text,
        border: iced::border::rounded(4),
        ..button::Style::default()
    }
}

fn background(theme: &Theme) -> container::Style {
    container::Style::default().background(Colors::of(theme).background)
}

fn muted(theme: &Theme) -> text::Style {
    text::Style {
        color: Some(Colors::of(theme).muted),
    }
}

fn error_text(theme: &Theme) -> text::Style {
    text::Style {
        color: Some(Colors::of(theme).error),
    }
}

fn star(theme: &Theme) -> text::Style {
    text::Style {
        color: Some(Colors::of(theme).star),
    }
}

#[cfg(test)]
mod tests {
    use iced_test::simulator;
    use pressbox_api::{Feed, Me, Version};

    use super::*;
    use crate::api::Client;

    fn feed(id: &str, unread: u32, tags: &[&str]) -> Feed {
        Feed {
            id: id.into(),
            title: format!("Feed {id}"),
            url: format!("https://{id}.example/"),
            site_url: None,
            tags: tags.iter().map(|&t| t.into()).collect(),
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

    fn item(id: i64, read: bool, starred: bool) -> Item {
        Item {
            id,
            feed_id: "a".into(),
            title: format!("Story {id}"),
            url: Some(format!("https://a.example/{id}")),
            author: Some("Ana".into()),
            summary: None,
            content: None,
            published: 1_793_700_000,
            read,
            starred,
            media: None,
            position: 0,
        }
    }

    /// A reader with feeds (one failing, one archived) and two items.
    fn reader() -> Reader {
        let mut reader = Reader::new(Client::new("https://rss.example", Some("t".into())).unwrap());
        let mut failing = feed("b", 0, &[]);
        failing.last_error = Some("HTTP 500".into());
        let mut old = feed("old", 0, &[]);
        old.archived = true;
        reader.feeds = vec![feed("a", 3, &["tech"]), failing, old];
        reader.items = vec![item(1, false, true), item(2, true, false)];
        reader
    }

    #[test]
    fn setup_asks_for_a_server_and_shows_the_code() {
        let mut setup = Setup {
            server: "https://rss.example".into(),
            device: "laptop".into(),
            ..Setup::default()
        };
        let mut ui = simulator(setup_view(&setup));
        assert!(
            ui.find("Pair this computer with your pressbox server.")
                .is_ok()
        );
        assert!(ui.find("Approve this code:").is_err());
        ui.click("Pair this device").unwrap();
        assert!(matches!(
            ui.into_messages().collect::<Vec<_>>()[..],
            [Message::Pair]
        ));

        setup.busy = true;
        setup.code = Some("K7W-4QX".into());
        setup.error = Some("could not connect".into());
        let mut ui = simulator(setup_view(&setup));
        for shown in [
            "Approve this code:",
            "K7W-4QX",
            "Waiting for approval…",
            "could not connect",
        ] {
            assert!(ui.find(shown).is_ok(), "{shown}");
        }
        let _ = ui.click("Pair this device");
        assert_eq!(ui.into_messages().count(), 0, "disabled while pairing");
    }

    #[test]
    fn sidebar_lists_sources_and_who_we_are() {
        let mut reader = reader();
        reader.me = Some(Me {
            user: "chris".into(),
            admin: true,
            device: Some("laptop".into()),
        });
        reader.version = Some(Version {
            version: "0.1.0".into(),
            api: 1,
        });
        let mut ui = simulator(sidebar(&reader));
        for shown in [
            "Unread",
            "Tags",
            "#tech",
            "Feeds",
            "Feed a",
            "Feed b ⚠",
            "Archived",
            "Feed old",
        ] {
            assert!(ui.find(shown).is_ok(), "{shown}");
        }
        assert!(
            ui.find("https://rss.example\nchris on laptop\npressboxd 0.1.0")
                .is_ok()
        );
        ui.click("Starred").unwrap();
        ui.click("#tech").unwrap();
        ui.click("Refresh").unwrap();
        ui.click("Sign out").unwrap();
        let messages: Vec<_> = ui.into_messages().collect();
        assert!(matches!(
            &messages[..],
            [
                Message::Select(Source::Starred),
                Message::Select(Source::Tag(tag)),
                Message::Refresh,
                Message::SignOut
            ] if tag == "tech"
        ));

        reader.me.as_mut().unwrap().device = None;
        assert_eq!(
            identity(&reader),
            "https://rss.example\nchris\npressboxd 0.1.0"
        );
    }

    #[test]
    fn item_list_opens_items_and_pages() {
        let mut reader = reader();
        reader.next_cursor = Some("c".into());
        let mut ui = simulator(item_list(&reader));
        assert!(ui.find("Unread").is_ok(), "the source is the heading");
        ui.click("Story 2").unwrap();
        ui.click("Load more").unwrap();
        ui.click("Mark all read").unwrap();
        let messages: Vec<_> = ui.into_messages().collect();
        assert!(matches!(
            messages[..],
            [
                Message::OpenItem(2),
                Message::LoadMore,
                Message::MarkAllRead
            ]
        ));

        reader.items.clear();
        reader.source = Source::Starred;
        let mut ui = simulator(item_list(&reader));
        assert!(ui.find("Nothing here.").is_ok());
        let _ = ui.click("Mark all read");
        assert_eq!(
            ui.into_messages().count(),
            0,
            "starred lists can't be marked read"
        );
        reader.loading = true;
        let mut ui = simulator(item_list(&reader));
        assert!(ui.find("Loading…").is_ok() && ui.find("Nothing here.").is_err());
    }

    #[test]
    fn article_shows_the_item_and_its_actions() {
        let mut reader = reader();
        assert!(
            simulator(article(&reader))
                .find("Select an item to read it.")
                .is_ok()
        );

        reader.article = Some(item(1, false, true));
        reader.article_text = "The whole story.".into();
        let mut ui = simulator(article(&reader));
        assert!(ui.find("Story 1").is_ok() && ui.find("The whole story.").is_ok());
        ui.click("★ Starred").unwrap();
        ui.click("Mark read").unwrap();
        ui.click("Open in browser").unwrap();
        let messages: Vec<_> = ui.into_messages().collect();
        assert!(matches!(
            messages[..],
            [
                Message::Toggle(Flag::Starred),
                Message::Toggle(Flag::Read),
                Message::OpenInBrowser
            ]
        ));

        let mut read = item(1, true, false);
        read.url = None;
        reader.article = Some(read);
        let mut ui = simulator(article(&reader));
        assert!(ui.find("☆ Star").is_ok() && ui.find("Mark unread").is_ok());
        let _ = ui.click("Open in browser");
        assert_eq!(ui.into_messages().count(), 0, "no link, nothing to open");
    }

    #[test]
    fn errors_and_status_show_in_a_banner() {
        let mut reader = reader();
        assert!(banner(&reader).is_none());
        reader.status = Some("Refreshing feeds…".into());
        assert!(
            simulator(banner(&reader).unwrap())
                .find("Refreshing feeds…")
                .is_ok()
        );
        reader.error = Some("offline".into());
        let mut ui = simulator(banner(&reader).unwrap());
        assert!(ui.find("offline").is_ok() && ui.find("Refreshing feeds…").is_err());
        ui.click("Dismiss").unwrap();
        assert!(matches!(
            ui.into_messages().collect::<Vec<_>>()[..],
            [Message::Dismiss]
        ));
    }

    #[test]
    fn whole_screens_render_in_both_themes() {
        let mut reader = reader();
        reader.selected = Some(1);
        reader.article = Some(item(1, false, true));
        reader.error = Some("offline".into());
        let reading = App {
            config: crate::config::ClientConfig::default(),
            config_path: None,
            screen: Screen::Reader(Box::new(reader)),
            dark: true,
        };
        let pairing = App {
            config: crate::config::ClientConfig::default(),
            config_path: None,
            screen: Screen::Setup(Setup::default()),
            dark: false,
        };
        for app in [&reading, &pairing] {
            for dark in [true, false] {
                let theme = if dark {
                    crate::theme::DARK.theme(true)
                } else {
                    crate::theme::LIGHT.theme(false)
                };
                // Drawing runs every style function, in both palettes.
                simulator(view(app)).snapshot(&theme).unwrap();
            }
        }
    }

    #[test]
    fn rows_show_selection_and_hover() {
        let theme = crate::theme::DARK.theme(true);
        let palette = theme.extended_palette();
        let background = |selected, status| row_style(&theme, status, selected).background;
        assert_eq!(
            background(true, button::Status::Active),
            Some(palette.background.strong.color.into())
        );
        assert_eq!(
            background(false, button::Status::Hovered),
            Some(palette.background.weak.color.into())
        );
        assert_eq!(background(false, button::Status::Active), None);
        let colors = Colors::of(&theme);
        assert_eq!(muted(&theme).color, Some(colors.muted));
        assert_eq!(error_text(&theme).color, Some(colors.error));
        assert_eq!(star(&theme).color, Some(colors.star));
    }

    #[test]
    fn dates_show_the_time_today() {
        let now = jiff::Timestamp::now().as_second();
        assert_eq!(date(now).len(), 5, "like 14:05");
        assert!(date(1_793_700_000).ends_with(", 2026"));
        assert_eq!(date(i64::MAX), "");
    }

    /// Whether an element shows every one of `texts`.
    fn shows(element: Element<'_, Message>, texts: &[&str]) -> bool {
        let mut ui = simulator(element);
        texts.iter().all(|t| ui.find(*t).is_ok())
    }

    fn episode_item(id: i64) -> Item {
        use pressbox_api::{Enclosure, TranscriptFile};
        let mut episode = item(id, false, false);
        episode.media = Some(Media {
            enclosures: vec![
                Enclosure {
                    url: "https://a.example/1.mp3".into(),
                    mime: Some("audio/mpeg".into()),
                    ..Enclosure::default()
                },
                Enclosure {
                    url: "https://a.example/1.mp4".into(),
                    mime: Some("video/mp4".into()),
                    ..Enclosure::default()
                },
            ],
            duration: Some(3723),
            season: Some(2),
            episode: Some(10),
            transcripts: vec![TranscriptFile::default()],
            ..Media::default()
        });
        episode
    }

    fn chapters() -> Vec<Chapter> {
        let chapter = |start: f64, title: Option<&str>, hidden| Chapter {
            start,
            title: title.map(Into::into),
            hidden,
            ..Chapter::default()
        };
        vec![
            chapter(0.0, Some("Intro"), false),
            chapter(30.0, Some("art"), true),
            chapter(60.0, Some("News"), false),
            chapter(4000.0, None, false),
        ]
    }

    fn playback(item: Item, paused: bool, video: bool, loaded: bool) -> Playback {
        let state = crate::player::PlayerState {
            position: 70.0,
            duration: Some(3723.0),
            paused,
            speed: 1.5,
            ended: false,
        };
        let mut p = Playback::new(item, chapters(), state, video);
        p.player = loaded.then(|| crate::player::tests::Fake::default().boxed());
        p
    }

    #[test]
    fn up_next_shows_in_the_sidebar_and_on_episodes() {
        let mut reader = reader();
        assert!(
            simulator(sidebar(&reader)).find("Up Next").is_err(),
            "no queue"
        );
        reader.queue = Some(vec![episode_item(3), episode_item(1)]);
        let mut ui = simulator(sidebar(&reader));
        ui.click("Up Next").unwrap();
        assert!(matches!(
            &ui.into_messages().collect::<Vec<_>>()[..],
            [Message::Select(Source::UpNext)]
        ));

        // Queued second, listed in Up Next: it can move and play next.
        reader.source = Source::UpNext;
        reader.article = Some(episode_item(1));
        reader.selected = Some(1);
        let mut ui = simulator(article(&reader));
        for label in ["Remove from Up Next", "Play next", "↑", "↓"] {
            ui.click(label).unwrap();
        }
        assert!(matches!(
            &ui.into_messages().collect::<Vec<_>>()[..],
            [
                Message::Queue(QueueChange::Toggle),
                Message::Queue(QueueChange::PlayNext),
                Message::Queue(QueueChange::Move(-1)),
                Message::Queue(QueueChange::Move(1)),
            ]
        ));

        // Not queued, elsewhere: added, or put first.
        reader.source = Source::All;
        reader.queue = Some(vec![episode_item(3)]);
        let mut ui = simulator(article(&reader));
        assert!(ui.find("↑").is_err());
        ui.click("Add to Up Next").unwrap();
        ui.click("Play next").unwrap();
        assert_eq!(ui.into_messages().count(), 2);
    }

    #[test]
    fn episodes_show_their_controls() {
        let mut reader = reader();
        let mut started = episode_item(1);
        started.position = 65;
        reader.items = vec![started.clone(), episode_item(3), item(2, true, false)];
        reader.article = Some(started.clone());
        reader.selected = Some(1);
        reader.chapters = chapters();
        let mut ui = simulator(article(&reader));
        for shown in [
            "▶ Resume from 1:05",
            "Watch video",
            "Transcript",
            "1:02:03 · Season 2, episode 10",
            "Chapters",
            "Intro",
            "News",
            "1:06:40",
        ] {
            assert!(ui.find(shown).is_ok(), "{shown}");
        }
        assert!(ui.find("art").is_err(), "hidden chapters aren't listed");
        ui.click("▶ Resume from 1:05").unwrap();
        ui.click("Watch video").unwrap();
        ui.click("Transcript").unwrap();
        let _ = ui.click("News");
        let messages: Vec<_> = ui.into_messages().collect();
        assert!(
            matches!(
                &messages[..],
                [
                    Message::Play(false),
                    Message::Play(true),
                    Message::ToggleTranscript
                ]
            ),
            "chapters only seek while playing: {messages:?}"
        );
        assert!(shows(item_list(&reader), &["◐", "♪"]));

        // Playing it: pause buttons, chapters seek, the list marks it.
        reader.playback = Some(playback(started.clone(), false, false, true));
        let mut ui = simulator(article(&reader));
        assert!(ui.find("⏸ Pause").is_ok());
        ui.click("News").unwrap();
        assert!(matches!(
            ui.into_messages().collect::<Vec<_>>()[..],
            [Message::SeekTo(60.0)]
        ));
        assert!(shows(item_list(&reader), &["▶"]));
        reader.playback.as_mut().unwrap().state.paused = true;
        assert!(shows(article(&reader), &["▶ Resume"]));
        reader.playback = Some(playback(started.clone(), false, true, true));
        assert!(shows(
            article(&reader),
            &["⏸ Pause video", "▶ Resume from 1:05"]
        ));

        // Fewer facts: no video, no transcript, no numbers.
        let mut plain = episode_item(4);
        let media = plain.media.as_mut().unwrap();
        media.enclosures.truncate(1);
        media.transcripts.clear();
        (media.duration, media.season) = (None, None);
        reader.article = Some(plain.clone());
        reader.playback = None;
        reader.chapters.clear();
        assert!(shows(article(&reader), &["▶ Play", "Episode 10"]));
        assert!(!shows(article(&reader), &["Watch video"]));
        assert!(!shows(article(&reader), &["Chapters"]));
        plain.media.as_mut().unwrap().episode = None;
        reader.article = Some(plain);
        assert!(shows(article(&reader), &["▶ Play"]));
    }

    #[test]
    fn transcripts_show_who_says_what() {
        let mut reader = reader();
        reader.article = Some(episode_item(1));
        reader.selected = Some(1);
        reader.show_transcript = true;
        assert!(shows(
            article(&reader),
            &["Loading the transcript…", "Show text"]
        ));
        reader.transcript = Some(None);
        assert!(shows(article(&reader), &["No transcript."]));
        reader.transcript = Some(Some(Transcript {
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
        let mut ui = simulator(article(&reader));
        assert!(ui.find("Ann: ").is_ok() && ui.find("Now.").is_ok());
        let _ = ui.click("Later.");
        assert_eq!(ui.into_messages().count(), 0, "not playing: no seeking");
        reader.playback = Some(playback(episode_item(1), false, false, true));
        let mut ui = simulator(article(&reader));
        ui.click("Later.").unwrap();
        assert!(matches!(
            ui.into_messages().collect::<Vec<_>>()[..],
            [Message::SeekTo(80.0)]
        ));
    }

    #[test]
    fn the_player_bar() {
        let mut p = playback(episode_item(1), false, false, false);
        let mut ui = simulator(player_bar(&p));
        assert!(ui.find("Story 1 · News (loading…)").is_ok());
        let _ = ui.click("⏸");
        assert_eq!(ui.into_messages().count(), 0, "nothing works while loading");

        p.player = Some(crate::player::tests::Fake::default().boxed());
        let mut ui = simulator(player_bar(&p));
        assert!(ui.find("1:10 / 1:02:03").is_ok() && ui.find("1.5×").is_ok());
        for label in ["⏮", "−10", "⏸", "+30", "⏭", "1.5×", "■"] {
            ui.click(label).unwrap();
        }
        let messages: Vec<_> = ui.into_messages().collect();
        assert_eq!(
            format!("{messages:?}"),
            format!(
                "{:?}",
                [
                    Message::Chapter(false),
                    Message::SeekBy(-controls::BACK),
                    Message::PauseToggle,
                    Message::SeekBy(controls::FORWARD),
                    Message::Chapter(true),
                    Message::NextSpeed,
                    Message::Stop,
                ]
            )
        );

        // Paused, dragging the slider, no duration, no chapter.
        p.state.paused = true;
        p.state.duration = None;
        p.scrubbing = Some(30.0);
        p.chapters.clear();
        assert!(shows(player_bar(&p), &["▶", "0:30"]));

        // In the reader, above the banner.
        let mut reader = reader();
        reader.playback = Some(p);
        reader.status = Some("hello".into());
        assert!(shows(reader_view(&reader), &["Story 1", "hello"]));
    }

    #[test]
    fn clocks() {
        assert_eq!(clock(-5.0), "0:00");
        assert_eq!(clock(65.9), "1:05");
        assert_eq!(clock(3723.0), "1:02:03");
    }
}
