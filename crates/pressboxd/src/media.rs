// SPDX-FileCopyrightText: 2026 Chris Palmer <chris@red-oxide.org>
// SPDX-License-Identifier: AGPL-3.0-only

//! Podcasts: an item's audio and video, and the chapters and transcripts
//! that go with it.
//!
//! feed-rs knows enclosures but drops most of the iTunes and Podcasting 2.0
//! (`podcast:`) tags, so RSS feeds are read a second time with roxmltree
//! for them. Atom enclosure links and JSON Feed attachments come from
//! feed-rs. Chapters and transcripts are separate files in several
//! formats; [`chapters`] and [`transcript`] turn them into the API's one
//! shape for each, so clients need not parse them.

use pressbox_api::{Chapter, Chapters, Enclosure, Media, Segment, Transcript, TranscriptFile};
use roxmltree::{Document, Node, ParsingOptions};
use url::Url;

use crate::sanitize;

const ITUNES: &str = "http://www.itunes.com/dtds/podcast-1.0.dtd";
/// The namespace URI the spec gives, and an older one feeds still use.
const PODCAST: [&str; 2] = [
    "https://podcastindex.org/namespace/1.0",
    "https://github.com/Podcastindex-org/podcast-namespace/blob/main/docs/1.0.md",
];

/// What an RSS channel says about itself and each `<item>`, in document
/// order (feed-rs keeps that order too, so the two line up by index).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct RssMedia {
    pub image: Option<String>,
    pub medium: Option<String>,
    pub items: Vec<Option<Media>>,
}

/// Reads the media tags of an RSS 2.0 feed. `None` if it isn't RSS 2.0 or
/// isn't well-formed enough for roxmltree (feed-rs is more forgiving; such
/// feeds just get no media).
pub fn rss(body: &[u8], feed_url: &str) -> Option<RssMedia> {
    let text = std::str::from_utf8(body).ok()?;
    let options = ParsingOptions {
        allow_dtd: true,
        ..ParsingOptions::default()
    };
    let doc = Document::parse_with_options(text, options).ok()?;
    let root = doc.root_element();
    if !root.has_tag_name("rss") {
        return None;
    }
    let channel = child(root, None, "channel")?;
    let base = Url::parse(feed_url).ok();
    let base = base.as_ref();
    let image = child(channel, Some(&[ITUNES]), "image")
        .and_then(|i| i.attribute("href"))
        .or_else(|| child(channel, None, "image").and_then(|i| child_text(i, None, "url")))
        .and_then(|u| media_url(u, base));
    let medium = child_text(channel, Some(&PODCAST), "medium")
        .map(|m| m.trim().to_lowercase())
        .filter(|m| !m.is_empty());
    let items = channel
        .children()
        .filter(|n| is(*n, None, "item"))
        .map(|item| rss_item(item, base))
        .collect();
    Some(RssMedia {
        image,
        medium,
        items,
    })
}

fn rss_item(item: Node<'_, '_>, base: Option<&Url>) -> Option<Media> {
    let mut enclosures: Vec<Enclosure> = children(item, None, "enclosure")
        .filter_map(|e| {
            Some(Enclosure {
                url: media_url(e.attribute("url")?, base)?,
                mime: mime(e.attribute("type")),
                length: number(e.attribute("length")),
                ..Enclosure::default()
            })
        })
        .take(1)
        .collect();
    for alt in children(item, Some(&PODCAST), "alternateEnclosure") {
        // Each source is the same file somewhere else; the first one a
        // player can fetch over HTTP will do.
        let Some(url) = children(alt, Some(&PODCAST), "source")
            .filter_map(|s| s.attribute("uri"))
            .find_map(|u| media_url(u, base))
        else {
            continue;
        };
        let alt = Enclosure {
            url,
            mime: mime(alt.attribute("type")),
            length: number(alt.attribute("length")),
            bitrate: alt
                .attribute("bitrate")
                .and_then(|b| b.trim().parse::<f64>().ok())
                .filter(|b| b.is_finite() && *b > 0.0)
                .map(|b| b.round().min(f64::from(u32::MAX)) as u32),
            height: number(alt.attribute("height")),
            title: alt
                .attribute("title")
                .map(plain_title)
                .filter(|t| !t.is_empty()),
        };
        if !enclosures.iter().any(|e| e.url == alt.url) {
            enclosures.push(alt);
        }
    }
    if enclosures.is_empty() {
        return None;
    }
    let chapters = child(item, Some(&PODCAST), "chapters")
        .and_then(|c| c.attribute("url"))
        .and_then(|u| media_url(u, base));
    let transcripts = children(item, Some(&PODCAST), "transcript")
        .filter_map(|t| {
            Some(TranscriptFile {
                url: media_url(t.attribute("url")?, base)?,
                mime: mime(t.attribute("type"))?,
                lang: t.attribute("language").map(str::to_owned),
                rel: t.attribute("rel").map(str::to_owned),
            })
        })
        .collect();
    let either = |name| {
        child_text(item, Some(&PODCAST), name)
            .or_else(|| child_text(item, Some(&[ITUNES]), name))
            .and_then(|n| number(Some(n)))
    };
    Some(Media {
        enclosures,
        duration: child_text(item, Some(&[ITUNES]), "duration").and_then(duration),
        image: child(item, Some(&[ITUNES]), "image")
            .and_then(|i| i.attribute("href"))
            .and_then(|u| media_url(u, base)),
        season: either("season"),
        episode: either("episode"),
        chapters,
        transcripts,
    })
}

/// Media from links: Atom's `rel="enclosure"` and JSON Feed attachments
/// (which feed-rs turns into links with a type but no `rel`).
pub fn from_links(links: &[feed_rs::model::Link], base: &str) -> Option<Media> {
    let base = Url::parse(base).ok();
    let enclosures: Vec<_> = links
        .iter()
        .filter(|l| {
            l.rel.as_deref() == Some("enclosure")
                || l.rel.is_none()
                    && l.media_type
                        .as_deref()
                        .is_some_and(|t| t.starts_with("audio/") || t.starts_with("video/"))
        })
        .filter_map(|l| {
            Some(Enclosure {
                url: media_url(&l.href, base.as_ref())?,
                mime: mime(l.media_type.as_deref()),
                length: l.length,
                title: l
                    .title
                    .as_deref()
                    .map(plain_title)
                    .filter(|t| !t.is_empty()),
                ..Enclosure::default()
            })
        })
        .collect();
    (!enclosures.is_empty()).then(|| Media {
        enclosures,
        ..Media::default()
    })
}

/// The title of an enclosure or a chapter, which, like an RSS title, may or
/// may not carry HTML.
fn plain_title(raw: &str) -> String {
    sanitize::title(raw, sanitize::Markup::Unknown)
}

/// `itunes:duration`: seconds, `MM:SS` or `HH:MM:SS`, maybe with a
/// fraction.
pub fn duration(text: &str) -> Option<u32> {
    let mut total = 0.0;
    let parts: Vec<&str> = text.trim().split(':').collect();
    if parts.len() > 3 {
        return None;
    }
    for part in parts {
        let n: f64 = part.trim().parse().ok()?;
        if !n.is_finite() || n < 0.0 {
            return None;
        }
        total = total * 60.0 + n;
    }
    (total < f64::from(u32::MAX)).then_some(total.round() as u32)
}

/// `raw` as an absolute `http(s)` URL, resolved against `base`.
pub fn http_url(raw: &str, base: &str) -> Option<String> {
    media_url(raw, Url::parse(base).ok().as_ref())
}

/// An absolute `http(s)` URL, resolved against the feed's. Anything else
/// (`ipfs:`, `javascript:`, garbage) is dropped: clients hand these
/// straight to a player.
fn media_url(raw: &str, base: Option<&Url>) -> Option<String> {
    let url = Url::options().base_url(base).parse(raw.trim()).ok()?;
    matches!(url.scheme(), "http" | "https").then(|| url.into())
}

fn mime(raw: Option<&str>) -> Option<String> {
    raw.map(|m| m.trim().to_owned()).filter(|m| {
        !m.is_empty()
            && m.len() <= 100
            && m.contains('/')
            && m.bytes().all(|b| b.is_ascii_graphic() || b == b' ')
    })
}

fn number<T: std::str::FromStr>(raw: Option<&str>) -> Option<T> {
    raw?.trim().parse().ok()
}

/// Whether a node is the element `name` in one of `ns` (`None`: in no
/// namespace, as plain RSS elements are).
fn is(n: Node<'_, '_>, ns: Option<&[&str]>, name: &str) -> bool {
    n.is_element()
        && n.tag_name().name() == name
        && match ns {
            None => n.tag_name().namespace().is_none(),
            Some(ns) => n.tag_name().namespace().is_some_and(|u| ns.contains(&u)),
        }
}

fn children<'a, 'i: 'a>(
    n: Node<'a, 'i>,
    ns: Option<&'a [&'a str]>,
    name: &'a str,
) -> impl Iterator<Item = Node<'a, 'i>> {
    n.children().filter(move |c| is(*c, ns, name))
}

fn child<'a, 'i>(n: Node<'a, 'i>, ns: Option<&[&str]>, name: &str) -> Option<Node<'a, 'i>> {
    n.children().find(|c| is(*c, ns, name))
}

fn child_text<'a>(n: Node<'a, '_>, ns: Option<&[&str]>, name: &str) -> Option<&'a str> {
    child(n, ns, name)?.text()
}

/// Reads a Podcasting 2.0 chapters file (JSON). Chapter artwork and links
/// are resolved against the file's URL and kept only if `http(s)`.
pub fn chapters(body: &[u8], file_url: &str) -> Option<Chapters> {
    #[derive(serde::Deserialize)]
    struct File {
        chapters: Vec<Raw>,
    }
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Raw {
        start_time: f64,
        end_time: Option<f64>,
        title: Option<String>,
        img: Option<String>,
        url: Option<String>,
        toc: Option<bool>,
    }
    let file: File = serde_json::from_slice(body).ok()?;
    let base = Url::parse(file_url).ok();
    let mut chapters: Vec<Chapter> = file
        .chapters
        .into_iter()
        .filter(|c| c.start_time.is_finite() && c.start_time >= 0.0)
        .map(|c| Chapter {
            start: c.start_time,
            end: c.end_time.filter(|e| e.is_finite() && *e >= c.start_time),
            title: c.title.map(|t| plain_title(&t)).filter(|t| !t.is_empty()),
            image: c.img.and_then(|u| media_url(&u, base.as_ref())),
            url: c.url.and_then(|u| media_url(&u, base.as_ref())),
            hidden: c.toc == Some(false),
        })
        .collect();
    chapters.sort_by(|a, b| a.start.total_cmp(&b.start));
    Some(Chapters { chapters })
}

/// The transcript clients get when an item has several: JSON (timed, with
/// speakers), then WebVTT, SRT, HTML and plain text.
pub fn best_transcript(files: &[TranscriptFile]) -> Option<&TranscriptFile> {
    files
        .iter()
        .filter_map(|f| Some((transcript_rank(&f.mime)?, f)))
        .min_by_key(|(rank, _)| *rank)
        .map(|(_, f)| f)
}

fn transcript_rank(mime: &str) -> Option<u8> {
    match mime.split(';').next()?.trim().to_ascii_lowercase().as_str() {
        "application/json" => Some(0),
        "text/vtt" => Some(1),
        "application/x-subrip" | "application/srt" | "text/srt" => Some(2),
        "text/html" => Some(3),
        "text/plain" => Some(4),
        _ => None,
    }
}

/// Reads a transcript file of the given MIME type into segments.
pub fn transcript(body: &[u8], mime: &str) -> Option<Transcript> {
    let text = String::from_utf8_lossy(body);
    let text = text.trim_start_matches('\u{feff}');
    let segments = match transcript_rank(mime)? {
        0 => json_segments(text)?,
        1 | 2 => cue_segments(text),
        3 => untimed(&sanitize::text(&sanitize::html(text, None))),
        _ => untimed(text),
    };
    Some(Transcript {
        segments: join_short(segments),
    })
}

fn untimed(text: &str) -> Vec<Segment> {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        return Vec::new();
    }
    vec![Segment {
        text,
        ..Segment::default()
    }]
}

/// The Podcasting 2.0 JSON transcript format.
fn json_segments(text: &str) -> Option<Vec<Segment>> {
    #[derive(serde::Deserialize)]
    struct File {
        segments: Vec<Raw>,
    }
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Raw {
        start_time: f64,
        end_time: f64,
        speaker: Option<String>,
        body: String,
    }
    let file: File = serde_json::from_str(text).ok()?;
    Some(
        file.segments
            .into_iter()
            .map(|s| Segment {
                start: s.start_time,
                end: s.end_time,
                speaker: s.speaker.filter(|s| !s.trim().is_empty()),
                text: s.body.trim().to_owned(),
            })
            .collect(),
    )
}

/// WebVTT and SRT: a cue is a `start --> end` line and the text lines after
/// it. Cue numbers, `NOTE`s and the header have no timing line, so they
/// are skipped. A WebVTT voice tag (`<v Name>`) gives the speaker.
fn cue_segments(text: &str) -> Vec<Segment> {
    let mut segments = Vec::new();
    let mut lines = text.lines().map(str::trim);
    while let Some(line) = lines.next() {
        let Some((start, end)) = line.split_once("-->") else {
            continue;
        };
        let (Some(start), Some(end)) = (
            timestamp(start),
            end.split_whitespace().next().and_then(timestamp),
        ) else {
            continue;
        };
        let mut speaker = None;
        let mut words = Vec::new();
        for line in lines.by_ref().take_while(|l| !l.is_empty()) {
            if let Some(rest) = line.strip_prefix("<v")
                && let Some((name, _)) = rest.split_once('>')
            {
                let name = name.trim_start_matches(|c: char| c == '.' || c.is_alphanumeric());
                speaker = Some(name.trim().to_owned()).filter(|n| !n.is_empty());
            }
            words.push(sanitize::text(line));
        }
        let text = words.join(" ");
        if !text.is_empty() {
            segments.push(Segment {
                start,
                end,
                speaker,
                text,
            });
        }
    }
    segments
}

/// `HH:MM:SS.mmm`, `MM:SS.mmm` (WebVTT) or `HH:MM:SS,mmm` (SRT), in
/// seconds.
fn timestamp(text: &str) -> Option<f64> {
    let text = text.trim().replace(',', ".");
    let parts: Vec<&str> = text.split(':').collect();
    if !(2..=3).contains(&parts.len()) {
        return None;
    }
    parts.iter().try_fold(0.0, |total: f64, part| {
        let n: f64 = part.parse().ok()?;
        (n.is_finite() && n >= 0.0).then_some(total * 60.0 + n)
    })
}

/// Shortest text a segment is left at once it ends a sentence.
const SENTENCE: usize = 80;
/// Longest text cues are joined into.
const PARAGRAPH: usize = 400;
/// A pause this long (seconds) always ends a segment.
const PAUSE: f64 = 2.0;

/// Joins runs of short cues by the same speaker into sentence-sized
/// segments: some transcripts time every word, which no reader wants.
fn join_short(segments: Vec<Segment>) -> Vec<Segment> {
    let mut out: Vec<Segment> = Vec::with_capacity(segments.len());
    for s in segments {
        if let Some(last) = out.last_mut() {
            let ended = last.text.len() >= SENTENCE
                && last.text.ends_with(['.', '?', '!', '"', '\u{201d}']);
            if last.speaker == s.speaker
                && s.start - last.end < PAUSE
                && !ended
                && last.text.len() + s.text.len() < PARAGRAPH
            {
                last.text.push(' ');
                last.text.push_str(&s.text);
                last.end = last.end.max(s.end);
                continue;
            }
        }
        out.push(s);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const FEED: &str = "https://pod.example/feed.xml";

    fn podcast_rss() -> String {
        r#"<?xml version="1.0" encoding="UTF-8"?>
        <!DOCTYPE rss>
        <rss version="2.0" xmlns:itunes="http://www.itunes.com/dtds/podcast-1.0.dtd"
             xmlns:podcast="https://podcastindex.org/namespace/1.0">
          <channel>
            <title>Pod</title>
            <itunes:image href="art/show.jpg"/>
            <podcast:medium> Video </podcast:medium>
            <item>
              <title>One</title>
              <enclosure url="ep1.mp3" type="audio/mpeg" length="1234"/>
              <enclosure url="https://pod.example/second.mp3" type="audio/mpeg"/>
              <itunes:duration>1:02:03</itunes:duration>
              <itunes:image href="https://pod.example/ep1.jpg"/>
              <itunes:season>2</itunes:season>
              <itunes:episode>9</itunes:episode>
              <podcast:episode>10</podcast:episode>
              <podcast:chapters url="ep1.json" type="application/json+chapters"/>
              <podcast:transcript url="ep1.vtt" type="text/vtt" language="en" rel="captions"/>
              <podcast:transcript url="javascript:alert(1)" type="text/html"/>
              <podcast:transcript url="ep1.txt"/>
              <podcast:alternateEnclosure type="video/mp4" length="9999" bitrate="2490000.5"
                  height="1080" title="1080p &lt;b&gt;HD&lt;/b&gt;">
                <podcast:source uri="ipfs://QmX"/>
                <podcast:source uri="https://cdn.example/ep1.mp4"/>
              </podcast:alternateEnclosure>
              <podcast:alternateEnclosure type="audio/mpeg">
                <podcast:source uri="https://pod.example/ep1.mp3"/>
              </podcast:alternateEnclosure>
              <podcast:alternateEnclosure type="audio/opus" bitrate="-1">
                <podcast:source uri="ipfs://only"/>
              </podcast:alternateEnclosure>
            </item>
            <item><title>Text only</title></item>
            <item>
              <podcast:alternateEnclosure type="application/x-mpegURL" title="">
                <podcast:source uri="https://live.example/hls.m3u8"/>
              </podcast:alternateEnclosure>
              <itunes:duration>soon</itunes:duration>
            </item>
          </channel>
        </rss>"#
            .to_owned()
    }

    #[test]
    fn reads_podcast_rss() {
        let media = rss(podcast_rss().as_bytes(), FEED).unwrap();
        assert_eq!(
            media.image.as_deref(),
            Some("https://pod.example/art/show.jpg")
        );
        assert_eq!(media.medium.as_deref(), Some("video"));
        assert_eq!(media.items.len(), 3);
        let one = media.items[0].clone().unwrap();
        assert_eq!(
            one.enclosures,
            [
                Enclosure {
                    url: "https://pod.example/ep1.mp3".into(),
                    mime: Some("audio/mpeg".into()),
                    length: Some(1234),
                    ..Enclosure::default()
                },
                Enclosure {
                    url: "https://cdn.example/ep1.mp4".into(),
                    mime: Some("video/mp4".into()),
                    length: Some(9999),
                    bitrate: Some(2_490_001),
                    height: Some(1080),
                    title: Some("1080p HD".into()),
                },
            ],
            "only the first <enclosure>; duplicates and IPFS-only sources are skipped"
        );
        assert_eq!(one.duration, Some(3723));
        assert_eq!(one.image.as_deref(), Some("https://pod.example/ep1.jpg"));
        assert_eq!(
            (one.season, one.episode),
            (Some(2), Some(10)),
            "podcast: wins"
        );
        assert_eq!(
            one.chapters.as_deref(),
            Some("https://pod.example/ep1.json")
        );
        assert_eq!(
            one.transcripts,
            [TranscriptFile {
                url: "https://pod.example/ep1.vtt".into(),
                mime: "text/vtt".into(),
                lang: Some("en".into()),
                rel: Some("captions".into()),
            }],
            "unsafe URLs and files without a type are dropped"
        );
        assert_eq!(media.items[1], None);
        let hls = media.items[2].clone().unwrap();
        assert_eq!(hls.enclosures[0].url, "https://live.example/hls.m3u8");
        assert_eq!(hls.enclosures[0].title, None);
        assert_eq!(hls.duration, None);
        assert_eq!(one.video().unwrap().height, Some(1080));
        assert_eq!(one.audio().unwrap().length, Some(1234));
        assert_eq!(hls.video(), None);
    }

    #[test]
    fn rss_channel_fallbacks_and_non_rss() {
        let plain = br#"<rss version="2.0"><channel><title>T</title>
            <image><url>https://x.example/logo.png</url></image>
            <podcast:medium xmlns:podcast="https://github.com/Podcastindex-org/podcast-namespace/blob/main/docs/1.0.md"> </podcast:medium>
            <item><enclosure url="a.mp3"/><enclosure type="audio/mpeg"/></item>
            <item><enclosure type="audio/mpeg"/></item>
        </channel></rss>"#;
        let media = rss(plain, "not a url").unwrap();
        assert_eq!(media.image.as_deref(), Some("https://x.example/logo.png"));
        assert_eq!(media.medium, None, "blank");
        assert_eq!(media.items, [None, None], "relative URLs need a base");

        let atom = br#"<feed xmlns="http://www.w3.org/2005/Atom"><title>A</title></feed>"#;
        assert_eq!(rss(atom, FEED), None);
        assert_eq!(
            rss(b"<rss version=\"2.0\"></rss>", FEED),
            None,
            "no channel"
        );
        assert_eq!(rss(b"<rss><unclosed></rss>", FEED), None, "malformed");
        assert_eq!(rss(b"\xff\xfe<rss/>", FEED), None, "not UTF-8");
    }

    #[test]
    fn media_from_links() {
        let link = |href: &str, rel: Option<&str>, ty: Option<&str>| feed_rs::model::Link {
            href: href.into(),
            target: None,
            rel: rel.map(Into::into),
            media_type: ty.map(Into::into),
            href_lang: None,
            title: Some("Episode <i>1</i>".into()),
            length: Some(10),
        };
        let links = [
            link(
                "https://x.example/post",
                Some("alternate"),
                Some("text/html"),
            ),
            link("/ep.mp3", Some("enclosure"), Some("audio/mpeg")),
            link("https://x.example/ep.mp4", None, Some("video/mp4")),
            link("https://x.example/page", None, Some("text/html")),
            link("ftp://x.example/ep.ogg", Some("enclosure"), None),
        ];
        let media = from_links(&links, "https://x.example/feed.json").unwrap();
        assert_eq!(
            media
                .enclosures
                .iter()
                .map(|e| e.url.as_str())
                .collect::<Vec<_>>(),
            ["https://x.example/ep.mp3", "https://x.example/ep.mp4"]
        );
        assert_eq!(media.enclosures[0].title.as_deref(), Some("Episode 1"));
        assert_eq!(media.enclosures[0].length, Some(10));
        assert_eq!(from_links(&links[..1], "https://x.example/"), None);
    }

    #[test]
    fn parses_durations() {
        assert_eq!(duration("3600"), Some(3600));
        assert_eq!(duration(" 59:30 "), Some(3570));
        assert_eq!(duration("1:00:00.6"), Some(3601));
        assert_eq!(duration("1:2:3:4"), None);
        assert_eq!(duration("-5"), None);
        assert_eq!(duration("inf"), None);
        assert_eq!(duration("99999999999"), None);
    }

    #[test]
    fn filters_mime_types() {
        assert_eq!(mime(Some(" audio/mpeg ")).as_deref(), Some("audio/mpeg"));
        assert_eq!(mime(Some("")), None);
        assert_eq!(mime(Some("mpeg")), None);
        assert_eq!(mime(Some("audio/\u{7}")), None);
        assert_eq!(mime(Some(&format!("audio/{}", "x".repeat(100)))), None);
        assert_eq!(mime(None), None);
    }

    #[test]
    fn reads_chapters() {
        let file = br#"{"version": "1.2.0", "chapters": [
            {"startTime": 120.5, "title": "Second &amp; last", "img": "img/2.jpg",
             "url": "https://x.example/", "endTime": 100},
            {"startTime": 0, "title": "Intro", "endTime": 120.5, "img": "data:x"},
            {"startTime": 60, "toc": false, "img": "img/art.jpg", "title": " "},
            {"startTime": -1, "title": "Broken"}
        ]}"#;
        let ch = chapters(file, "https://cdn.example/ep/chapters.json").unwrap();
        assert_eq!(
            ch.chapters,
            [
                Chapter {
                    start: 0.0,
                    end: Some(120.5),
                    title: Some("Intro".into()),
                    ..Chapter::default()
                },
                Chapter {
                    start: 60.0,
                    image: Some("https://cdn.example/ep/img/art.jpg".into()),
                    hidden: true,
                    ..Chapter::default()
                },
                Chapter {
                    start: 120.5,
                    end: None,
                    title: Some("Second & last".into()),
                    image: Some("https://cdn.example/ep/img/2.jpg".into()),
                    url: Some("https://x.example/".into()),
                    hidden: false,
                },
            ]
        );
        assert_eq!(chapters(b"{}", "https://x.example/"), None);
    }

    fn file(mime: &str) -> TranscriptFile {
        TranscriptFile {
            url: format!("https://x.example/{mime}"),
            mime: mime.into(),
            ..TranscriptFile::default()
        }
    }

    #[test]
    fn picks_the_best_transcript() {
        let files = [
            file("text/html"),
            file("application/x-subrip"),
            file("application/pdf"),
            file("text/vtt; charset=utf-8"),
        ];
        assert_eq!(
            best_transcript(&files).unwrap().mime,
            "text/vtt; charset=utf-8"
        );
        assert_eq!(best_transcript(&files[2..3]), None);
        assert_eq!(best_transcript(&[]), None);
        for (mime, rank) in [
            ("application/json", 0),
            ("text/srt", 2),
            ("application/srt", 2),
            ("text/plain", 4),
        ] {
            assert_eq!(transcript_rank(mime), Some(rank), "{mime}");
        }
    }

    #[test]
    fn reads_webvtt() {
        let vtt = "\u{feff}WEBVTT\n\nNOTE a comment\n\n1\n00:00:00.000 --> 00:00:02.500 align:start\n\
                   <v.loud Alice>Hello there,</v>\n<i>friend.</i>\n\n\
                   00:02.600 --> 00:04.000\n<v Alice>How are you?\n\n\
                   00:00:10.000 --> 00:00:12.000\n<v Bob>Fine.\n\n\
                   bad --> 00:00:13.000\nignored\n\n\
                   00:00:13.000 --> 00:00:14.000\n\n\
                   00:00:15.000 --> 00:00:16.000\n<v >Nobody\n";
        let t = transcript(vtt.as_bytes(), "text/vtt").unwrap();
        assert_eq!(
            t.segments,
            [
                Segment {
                    start: 0.0,
                    end: 4.0,
                    speaker: Some("Alice".into()),
                    text: "Hello there, friend. How are you?".into(),
                },
                Segment {
                    start: 10.0,
                    end: 12.0,
                    speaker: Some("Bob".into()),
                    text: "Fine.".into(),
                },
                Segment {
                    start: 15.0,
                    end: 16.0,
                    speaker: None,
                    text: "Nobody".into(),
                },
            ]
        );
    }

    #[test]
    fn reads_srt_json_html_and_text() {
        let srt = "1\n00:00:01,000 --> 00:00:02,000\nOne\n\n2\n1:2:3:4 --> 00:00:03,000\nNo\n";
        let t = transcript(srt.as_bytes(), "application/x-subrip").unwrap();
        assert_eq!(t.segments.len(), 1);
        assert_eq!(
            (t.segments[0].start, t.segments[0].text.as_str()),
            (1.0, "One")
        );

        let json = br#"{"version":"1.0.0","segments":[
            {"speaker":"Ann","startTime":0,"endTime":0.4,"body":"Word"},
            {"speaker":"Ann","startTime":0.4,"endTime":0.8,"body":"by"},
            {"speaker":"Ann","startTime":0.8,"endTime":1.2,"body":"word."},
            {"speaker":" ","startTime":1.2,"endTime":2,"body":" Next "}
        ]}"#;
        let t = transcript(json, "application/json").unwrap();
        assert_eq!(
            t.segments,
            [
                Segment {
                    start: 0.0,
                    end: 1.2,
                    speaker: Some("Ann".into()),
                    text: "Word by word.".into()
                },
                Segment {
                    start: 1.2,
                    end: 2.0,
                    speaker: None,
                    text: "Next".into()
                },
            ]
        );
        assert_eq!(transcript(b"[]", "application/json"), None);

        let html = b"<p>Hi <b>all</b></p><script>x()</script><p>bye</p>";
        let t = transcript(html, "text/html").unwrap();
        assert_eq!(t.segments[0].text, "Hi all bye");
        let t = transcript(b"  plain\n text ", "text/plain").unwrap();
        assert_eq!(t.segments[0].text, "plain text");
        assert!(transcript(b" ", "text/plain").unwrap().segments.is_empty());
        assert_eq!(transcript(b"x", "application/pdf"), None);
    }

    #[test]
    fn joins_short_cues_into_sentences() {
        let cue = |start: f64, text: &str| Segment {
            start,
            end: start + 1.0,
            speaker: None,
            text: text.into(),
        };
        let long = "x".repeat(SENTENCE);
        let joined = join_short(vec![
            cue(0.0, &format!("{long}.")),
            cue(1.0, "a"),
            cue(2.0, "b"),
            cue(10.0, "after a pause"),
            cue(11.0, &"y".repeat(PARAGRAPH)),
        ]);
        let texts: Vec<_> = joined.iter().map(|s| s.text.len()).collect();
        assert_eq!(texts, [SENTENCE + 1, 3, 13, PARAGRAPH]);
        assert_eq!(joined[1].end, 3.0);
    }
}
