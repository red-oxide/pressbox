// SPDX-FileCopyrightText: 2026 Chris Palmer <chris@red-oxide.org>
// SPDX-License-Identifier: AGPL-3.0-only

//! Cleaning feed HTML before it is stored, so every client gets safe markup.
//!
//! Feeds can contain anything. Clients render this HTML (the TUI as text,
//! iOS in a web view), so the server keeps only ordinary markup:
//! scripts, styles, iframes, forms and event handlers are dropped, links get
//! `rel="noopener noreferrer"`, relative URLs are made absolute, and tracking
//! pixels (1×1 images and known tracker hosts) are removed.

use std::{borrow::Cow, sync::LazyLock};

use ammonia::{Builder, Url, UrlRelative};
use regex::Regex;

/// Changes whenever sanitizing might: stored entries are re-sanitized once
/// after an upgrade rather than kept as an older version cleaned them.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// `<img>` tags declaring a zero or one pixel width or height.
static PIXEL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)<img\b[^>]*\b(?:width|height)\s*=\s*["']?\s*[01](?:px)?\s*["']?(?:[\s/>])[^>]*>"#,
    )
    .expect("valid regex")
});

/// Image hosts and paths that only exist to track reads.
const TRACKERS: &[&str] = &[
    "feeds.feedburner.com/~r/",
    "feeds.feedburner.com/~ff/",
    "feedproxy.google.com/~r/",
    "stats.wordpress.com/",
    "pixel.wp.com/",
    "feedblitz.com/~/i/",
    "pixel.quantserve.com/",
    "www.google-analytics.com/",
    "/open.aspx?",
    "/track/open",
];

fn builder(base: Option<&str>) -> Builder<'static> {
    let mut b = Builder::default();
    b.link_rel(Some("noopener noreferrer"))
        .attribute_filter(|element, attribute, value| {
            if element == "img" && attribute == "src" && TRACKERS.iter().any(|t| value.contains(t))
            {
                return None;
            }
            Some(Cow::Borrowed(value))
        });
    if let Some(base) = base.and_then(|b| Url::parse(b).ok()) {
        b.url_relative(UrlRelative::RewriteWithBase(base));
    }
    b
}

/// The URL to resolve an item's relative links against: its own link if
/// that is absolute, otherwise the feed's URL.
pub fn base_url(item_url: Option<&str>, feed_url: &str) -> String {
    let feed = Url::parse(feed_url).ok();
    item_url
        .and_then(|u| match &feed {
            Some(f) => f.join(u).ok(),
            None => Url::parse(u).ok(),
        })
        .map_or_else(|| feed_url.to_owned(), Into::into)
}

/// Cleans HTML from a feed. `base` resolves relative links and images.
pub fn html(dirty: &str, base: Option<&str>) -> String {
    let without_pixels = PIXEL.replace_all(dirty, "");
    let mut clean = builder(base).clean(&without_pixels).to_string();
    // Trimmed in place: item HTML can be large, so don't copy it again.
    clean.truncate(clean.trim_end().len());
    clean.drain(..clean.len() - clean.trim_start().len());
    clean
}

/// The entities ammonia writes, and what they stand for. `&amp;` comes
/// last only for readability: each `&` is decoded once, left to right, so
/// `&amp;lt;` stays `&lt;`.
const ENTITIES: [(&str, char); 6] = [
    ("&lt;", '<'),
    ("&gt;", '>'),
    ("&quot;", '"'),
    ("&#39;", '\''),
    ("&nbsp;", ' '),
    ("&amp;", '&'),
];

/// Plain text of an HTML fragment, for the search index: tags become
/// spaces, the entities above are decoded, and runs of whitespace become
/// one space. Only used on HTML already cleaned by [`html`], so skipping
/// from `<` to `>` is enough and much cheaper than parsing it again. One
/// pass, as this runs for every item stored.
pub fn text(html: &str) -> String {
    plain(html, |_| true)
}

/// How a title is written, which decides what a `<` in it means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Markup {
    /// Plain text (Atom `type="text"`, JSON Feed): kept as it is.
    Text,
    /// HTML (Atom `type="html"` and `"xhtml"`): every tag goes.
    Html,
    /// The feed doesn't say (RSS, chapter files): some titles carry
    /// escaped HTML and some are plain text that happens to contain `<`.
    Unknown,
}

/// Lowercase HTML elements that turn up in titles. Only these are taken to
/// be tags in a title whose markup is [`Markup::Unknown`], so `Vec<T>` and
/// `a < b and c > d` stay whole. Uppercase names are left alone: HTML in
/// feeds is lowercase, and type parameters (`<A>`, `<B>`, `<S>`) are not.
const TITLE_TAGS: &[&str] = &[
    "a", "abbr", "b", "bdi", "bdo", "big", "br", "cite", "code", "del", "dfn", "div", "em", "font",
    "h1", "h2", "h3", "h4", "h5", "h6", "i", "img", "ins", "kbd", "mark", "p", "q", "s", "samp",
    "small", "span", "strike", "strong", "sub", "sup", "time", "tt", "u", "var", "wbr",
];

/// Whether what stands between a `<` and the next `>` is one of
/// [`TITLE_TAGS`], opening or closing, with or without attributes.
fn is_title_tag(inner: &str) -> bool {
    let inner = inner.strip_prefix('/').unwrap_or(inner);
    let name = inner
        .split(|c: char| c.is_whitespace() || c == '/')
        .next()
        .unwrap_or_default();
    TITLE_TAGS.contains(&name)
}

/// A title as plain text on one line. Titles are shown as text by every
/// client, so markup is removed rather than cleaned; `markup` says how
/// much of what looks like a tag really is one.
pub fn title(raw: &str, markup: Markup) -> String {
    match markup {
        Markup::Text => raw.split_whitespace().collect::<Vec<_>>().join(" "),
        Markup::Html => text(raw),
        Markup::Unknown => plain(raw, is_title_tag),
    }
}

/// [`text`], removing only the tags `is_tag` accepts (it is given what
/// stands between `<` and `>`); any other `<` is kept as text.
fn plain(html: &str, is_tag: impl Fn(&str) -> bool) -> String {
    let mut out = String::with_capacity(html.len());
    // Whitespace (or a tag) seen since the last character written.
    let mut gap = false;
    let mut rest = html;
    while let Some(c) = rest.chars().next() {
        let (c, len) = match c {
            '<' => match rest.find('>') {
                Some(end) if is_tag(&rest[1..end]) => (' ', end + 1),
                _ => ('<', 1),
            },
            '&' => ENTITIES
                .iter()
                .find(|(entity, _)| rest.starts_with(entity))
                .map_or(('&', 1), |&(entity, c)| (c, entity.len())),
            c => (c, c.len_utf8()),
        };
        rest = &rest[len..];
        if c.is_whitespace() {
            gap = true;
        } else {
            if gap && !out.is_empty() {
                out.push(' ');
            }
            gap = false;
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_dangerous_markup() {
        let out = html(
            r#"<p onclick="x()">Hi <script>alert(1)</script><b>there</b></p>
               <iframe src="https://evil.example"></iframe><style>p{}</style>
               <a href="javascript:alert(1)">bad</a> <form><input></form>"#,
            None,
        );
        assert!(!out.contains("script") && !out.contains("alert"), "{out}");
        assert!(!out.contains("onclick") && !out.contains("iframe"), "{out}");
        assert!(!out.contains("<style") && !out.contains("<form"), "{out}");
        assert!(out.contains("<b>there</b>"), "{out}");
    }

    #[test]
    fn rewrites_links_and_relative_urls() {
        let out = html(
            r#"<a href="/post">post</a> <img src="img/a.png" alt="a">"#,
            Some("https://blog.example/feed.xml"),
        );
        assert!(out.contains(r#"href="https://blog.example/post""#), "{out}");
        assert!(out.contains(r#"rel="noopener noreferrer""#), "{out}");
        assert!(
            out.contains(r#"src="https://blog.example/img/a.png""#),
            "{out}"
        );
    }

    #[test]
    fn strips_tracking_pixels() {
        let out = html(
            r#"<p>text</p>
               <img src="https://t.example/p.gif" width="1" height="1">
               <img height='0' src="https://t.example/q.gif"/>
               <img src="https://feeds.feedburner.com/~r/blog/~4/abc" alt="">
               <img src="https://example.com/photo.jpg" width="10" alt="photo">
               <img src="https://example.com/wide.jpg" width="100">"#,
            None,
        );
        assert!(!out.contains("p.gif") && !out.contains("q.gif"), "{out}");
        assert!(!out.contains("feedburner"), "{out}");
        assert!(
            out.contains("photo.jpg") && out.contains("wide.jpg"),
            "{out}"
        );
    }

    #[test]
    fn base_url_prefers_the_item_link() {
        let feed = "https://blog.example/feed.xml";
        assert_eq!(
            base_url(Some("https://other.example/p/1"), feed),
            "https://other.example/p/1"
        );
        assert_eq!(
            base_url(Some("/posts/2"), feed),
            "https://blog.example/posts/2"
        );
        assert_eq!(base_url(None, feed), feed);
    }

    #[test]
    fn text_for_search() {
        assert_eq!(
            text("<p>Rust &amp; <b>Nix</b></p>\n<p>tips&nbsp;here</p>"),
            "Rust & Nix tips here"
        );
    }

    /// The regex-and-replace version `text` replaced, kept to check the
    /// one-pass version against.
    fn text_reference(html: &str) -> String {
        let stripped = Regex::new(r"<[^>]*>").unwrap().replace_all(html, " ");
        stripped
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&#39;", "'")
            .replace("&nbsp;", " ")
            .replace("&amp;", "&")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn text_matches_the_reference() {
        for case in [
            "",
            "   ",
            "plain",
            "<p></p>",
            "a<br>b",
            "a <br> b",
            "  <p>  lead and trail  </p>  ",
            "&amp;lt; stays escaped once",
            "&amp;nbsp;",
            "&&lt;&",
            "& alone, &unknown; and &#40;",
            "1 < 2 but no closing bracket",
            "<a href=\"x\">link</a>&quot;q&quot; &#39;s&#39;",
            "tabs\tand\nnewlines\r\n",
            "non\u{a0}breaking and café 日本語 <i>emoji 🦀</i>",
            "<p>multi\nline\ntag</p",
        ] {
            assert_eq!(text(case), text_reference(case), "{case:?}");
        }
    }

    #[test]
    fn titles_keep_text_that_only_looks_like_a_tag() {
        for (raw, markup, expected) in [
            // RSS doesn't say whether a title is HTML: known tags go...
            ("A <b>bold</b> title", Markup::Unknown, "A bold title"),
            (
                r#"<a href="/x">Link</a> and <br/> break"#,
                Markup::Unknown,
                "Link and break",
            ),
            ("T &amp; <em>x</em>", Markup::Unknown, "T & x"),
            // ...and everything else stays.
            (
                "Why Vec<T> beats LinkedList<T>",
                Markup::Unknown,
                "Why Vec<T> beats LinkedList<T>",
            ),
            (
                "When a < b and c > d",
                Markup::Unknown,
                "When a < b and c > d",
            ),
            (
                "Result<A, B> and <S>",
                Markup::Unknown,
                "Result<A, B> and <S>",
            ),
            (
                "Vec<u8> vs <unknown>",
                Markup::Unknown,
                "Vec<u8> vs <unknown>",
            ),
            ("I <3 feeds", Markup::Unknown, "I <3 feeds"),
            // Atom says: text is kept to the letter, HTML loses every tag.
            (" Vec<b>  &amp;\n more ", Markup::Text, "Vec<b> &amp; more"),
            (
                "<span>Vec&lt;T&gt;</span> &amp; <x-y>more</x-y>",
                Markup::Html,
                "Vec<T> & more",
            ),
        ] {
            assert_eq!(title(raw, markup), expected, "{raw:?} as {markup:?}");
        }
    }

    #[test]
    fn html_is_trimmed() {
        assert_eq!(html("  \n<p>x</p>\n  ", None), "<p>x</p>");
        assert_eq!(html("   ", None), "");
    }
}
