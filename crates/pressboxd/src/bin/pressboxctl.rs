// SPDX-FileCopyrightText: 2026 Chris Palmer <chris@red-oxide.org>
// SPDX-License-Identifier: AGPL-3.0-only

//! `pressboxctl`: manage a running pressboxd through its control socket,
//! find a site's feeds, and convert OPML subscription lists to Nix.

use std::{
    collections::{BTreeMap, HashSet},
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use pressbox_api::{
    ApproveRequest, Approved, BackupRequest, BackupResult, Device, NewDevice, NewDeviceRequest,
    PendingPairing, Status, UserInfo,
};
use serde::{Serialize, de::DeserializeOwned};

/// Manage a running pressboxd.
///
/// Commands other than `discover` and `opml-to-nix` talk to pressboxd's
/// control socket, which only root and the service user can open.
#[derive(Parser)]
#[command(version)]
struct Args {
    /// pressboxd's control socket.
    #[arg(
        short,
        long,
        env = "PRESSBOX_CONTROL_SOCKET",
        default_value = "/run/pressbox/control.sock",
        global = true
    )]
    socket: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Feeds, items, users, devices and pending pairings at a glance.
    Status,
    /// List users (declared in the NixOS config) and their devices.
    Users,
    /// List paired devices.
    Devices {
        /// Only this user's devices.
        #[arg(long)]
        user: Option<String>,
    },
    /// Revoke a device's token.
    Revoke { name: String },
    /// Create a device and print its token, for clients that cannot pair.
    AddDevice {
        name: String,
        /// The user it belongs to; required with more than one user.
        #[arg(long)]
        user: Option<String>,
    },
    /// List pairing requests waiting for approval.
    Pairings,
    /// Approve a pairing request by the code the new client shows.
    Approve {
        code: String,
        /// The user the device belongs to; required with more than one user.
        #[arg(long)]
        user: Option<String>,
    },
    /// Refuse a pairing request.
    Deny { code: String },
    /// Write a consistent copy of the database to the backup directory.
    Backup {
        /// Backups to keep; older ones are deleted.
        #[arg(long, default_value_t = 14)]
        keep: u32,
    },
    /// Fetch every feed now.
    Refresh,
    /// Print a user's feeds as OPML.
    OpmlExport {
        /// Whose feeds; required with more than one user.
        #[arg(long)]
        user: Option<String>,
    },
    /// Convert an OPML file (from another reader) to a Nix attrset for
    /// `services.pressbox.feeds`. Works offline.
    OpmlToNix { file: PathBuf },
    /// Find the feeds a web page or site offers and print them as Nix for
    /// `services.pressbox.feeds`. Talks to the site, not to pressboxd.
    Discover {
        /// A page of the site, its home page, or a feed URL.
        url: String,
    },
}

// Not measured: process wiring around `run`, which the tests call.
#[cfg(not(tarpaulin_include))]
fn main() {
    if let Err(e) = run(Args::parse(), &mut std::io::stdout().lock()) {
        eprintln!("pressboxctl: {e:#}");
        std::process::exit(1);
    }
}

/// Runs a command, printing its output to `out` (notes go to stderr).
fn run(args: Args, out: &mut impl Write) -> Result<()> {
    let ctl = Control {
        socket: args.socket,
    };
    match args.command {
        Command::Status => {
            let s: Status = ctl.json("GET", "/admin/status", None::<()>)?;
            writeln!(
                out,
                "feeds:            {} ({} failing)",
                s.feeds, s.failing_feeds
            )?;
            if s.archived_feeds > 0 {
                writeln!(
                    out,
                    "archived feeds:   {} (removed, kept for starred items)",
                    s.archived_feeds
                )?;
            }
            writeln!(out, "items:            {}", s.items)?;
            writeln!(out, "users:            {}", s.users)?;
            writeln!(out, "devices:          {}", s.devices)?;
            writeln!(out, "pending pairings: {}", s.pending_pairings)?;
        }
        Command::Users => {
            let users: Vec<UserInfo> = ctl.json("GET", "/admin/users", None::<()>)?;
            for u in users {
                let admin = if u.admin { "admin" } else { "" };
                writeln!(
                    out,
                    "{:<20} {:<6} {:>3} feeds  {:>2} devices",
                    u.name, admin, u.feeds, u.devices
                )?;
            }
        }
        Command::Devices { user } => {
            let devices: Vec<Device> = ctl.json("GET", "/admin/devices", None::<()>)?;
            let devices: Vec<Device> = devices
                .into_iter()
                .filter(|d| user.as_ref().is_none_or(|u| &d.user == u))
                .collect();
            if devices.is_empty() {
                writeln!(out, "no devices paired")?;
            }
            for d in devices {
                let seen = d.last_seen.map_or_else(|| "never".to_owned(), ago);
                writeln!(
                    out,
                    "{:<24} {:<16} paired {:<10} last seen {seen}",
                    d.name,
                    d.user,
                    ago(d.created)
                )?;
            }
        }
        Command::Revoke { name } => {
            ctl.call(
                "DELETE",
                &format!("/admin/devices/{}", encode(&name)),
                None::<()>,
            )
            .with_context(|| format!("revoking {name:?}"))?;
            writeln!(out, "revoked {name}")?;
        }
        Command::AddDevice { name, user } => {
            let d: NewDevice = ctl.json(
                "POST",
                "/admin/devices",
                Some(NewDeviceRequest { name, user }),
            )?;
            eprintln!(
                "created device {} for {}; its token (shown only now):",
                d.name, d.user
            );
            writeln!(out, "{}", d.token)?;
        }
        Command::Pairings => {
            let pending: Vec<PendingPairing> = ctl.json("GET", "/admin/pairings", None::<()>)?;
            if pending.is_empty() {
                writeln!(out, "no pairing requests")?;
            }
            for p in pending {
                writeln!(
                    out,
                    "{}  {:<24} requested {}",
                    p.code,
                    p.name,
                    ago(p.requested)
                )?;
            }
        }
        Command::Approve { code, user } => {
            let path = format!("/admin/pairings/{}/approve", encode(&code));
            let a: Approved = ctl
                .json("POST", &path, Some(ApproveRequest { user }))
                .with_context(|| format!("approving {code}"))?;
            writeln!(out, "paired {} for {}", a.name, a.user)?;
        }
        Command::Deny { code } => {
            ctl.call(
                "DELETE",
                &format!("/admin/pairings/{}", encode(&code)),
                None::<()>,
            )
            .with_context(|| format!("denying {code}"))?;
            writeln!(out, "denied {code}")?;
        }
        Command::Backup { keep } => {
            let r: BackupResult =
                ctl.json("POST", "/admin/backup", Some(BackupRequest { keep }))?;
            writeln!(out, "{}", r.path)?;
            for old in r.removed {
                eprintln!("removed {old}");
            }
        }
        Command::Refresh => {
            ctl.call("POST", "/admin/refresh", None::<()>)?;
            writeln!(out, "refreshing all feeds")?;
        }
        Command::OpmlExport { user } => {
            let path = match user {
                Some(u) => format!("/admin/opml?user={}", encode(&u)),
                None => "/admin/opml".to_owned(),
            };
            write!(out, "{}", ctl.call("GET", &path, None::<()>)?)?;
        }
        Command::OpmlToNix { file } => {
            let text = std::fs::read_to_string(&file)
                .with_context(|| format!("reading {}", file.display()))?;
            write!(out, "{}", opml_to_nix(&text)?)?;
        }
        Command::Discover { url } => {
            let feeds = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
                .block_on(discover(&url))?;
            if feeds.is_empty() {
                bail!("no feeds found at {url}");
            }
            write!(out, "{}", discovered_to_nix(&feeds))?;
        }
    }
    Ok(())
}

fn ago(ts: i64) -> String {
    let secs = jiff::Timestamp::now().as_second() - ts;
    match secs {
        ..60 => "just now".to_owned(),
        60..3600 => format!("{}m ago", secs / 60),
        3600..86400 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86400),
    }
}

fn encode(segment: &str) -> String {
    segment
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// A minimal HTTP/1.1 client over the control socket.
struct Control {
    socket: PathBuf,
}

impl Control {
    fn call(&self, method: &str, path: &str, body: Option<impl Serialize>) -> Result<String> {
        let mut stream = UnixStream::connect(&self.socket).with_context(|| {
            format!(
                "connecting to {} (is pressboxd running, and are you root?)",
                self.socket.display()
            )
        })?;
        stream.set_read_timeout(Some(Duration::from_secs(120)))?;
        let body = body
            .map(|b| serde_json::to_string(&b))
            .transpose()?
            .unwrap_or_default();
        write!(
            stream,
            "{method} {path} HTTP/1.1\r\nHost: pressboxd\r\nConnection: close\r\n\
             Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )?;
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw)?;
        let (status, body) = parse_response(&raw)?;
        if !(200..300).contains(&status) {
            let message = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v["error"].as_str().map(str::to_owned))
                .unwrap_or(body);
            bail!("pressboxd: {message} (HTTP {status})");
        }
        Ok(body)
    }

    fn json<T: DeserializeOwned>(
        &self,
        method: &str,
        path: &str,
        body: Option<impl Serialize>,
    ) -> Result<T> {
        let text = self.call(method, path, body)?;
        serde_json::from_str(&text).with_context(|| format!("unexpected response: {text}"))
    }
}

/// Splits a raw HTTP/1.1 response into status and body, decoding chunked
/// transfer encoding if used.
fn parse_response(raw: &[u8]) -> Result<(u16, String)> {
    let text = String::from_utf8_lossy(raw);
    let (head, body) = text
        .split_once("\r\n\r\n")
        .context("malformed HTTP response")?;
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .context("malformed HTTP status line")?;
    let chunked = head.lines().any(|l| {
        let l = l.to_ascii_lowercase();
        l.starts_with("transfer-encoding:") && l.contains("chunked")
    });
    if !chunked {
        return Ok((status, body.to_owned()));
    }
    let mut out = String::new();
    let mut rest = body;
    loop {
        let (size, after) = rest.split_once("\r\n").context("malformed chunk")?;
        let size = usize::from_str_radix(size.trim(), 16).context("malformed chunk size")?;
        if size == 0 {
            break;
        }
        out.push_str(after.get(..size).context("truncated chunk")?);
        rest = after.get(size + 2..).unwrap_or_default();
    }
    Ok((status, out))
}

#[derive(Debug, PartialEq, Eq)]
struct OpmlFeed {
    title: Option<String>,
    url: String,
    tags: Vec<String>,
}

fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-')
        .chars()
        .take(40)
        .collect::<String>()
        .trim_end_matches('-')
        .to_owned()
}

fn opml_feeds(text: &str) -> Result<Vec<OpmlFeed>> {
    let doc = roxmltree::Document::parse(text).context("not valid OPML/XML")?;
    let body = doc
        .descendants()
        .find(|n| n.has_tag_name("body"))
        .context("OPML has no <body>")?;
    let mut feeds = Vec::new();
    for node in body.descendants().filter(|n| n.has_tag_name("outline")) {
        let Some(url) = node.attribute("xmlUrl") else {
            continue;
        };
        let mut tags: Vec<String> = node
            .ancestors()
            .skip(1)
            .filter(|a| a.has_tag_name("outline") && a.attribute("xmlUrl").is_none())
            .filter_map(|a| a.attribute("text").or_else(|| a.attribute("title")))
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(slug)
            .collect();
        tags.extend(
            node.attribute("category")
                .unwrap_or_default()
                .split([',', '/'])
                .map(slug),
        );
        tags.retain(|t| !t.is_empty());
        let mut seen = HashSet::new();
        tags.retain(|t| seen.insert(t.clone()));
        let title = node
            .attribute("title")
            .or_else(|| node.attribute("text"))
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_owned);
        feeds.push(OpmlFeed {
            title,
            url: url.trim().to_owned(),
            tags,
        });
    }
    Ok(feeds)
}

fn nix_string(s: &str) -> String {
    let escaped = s
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace("${", "\\${");
    format!("\"{escaped}\"")
}

fn nix_attr(name: &str) -> String {
    let plain = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if plain {
        name.to_owned()
    } else {
        nix_string(name)
    }
}

fn opml_to_nix(text: &str) -> Result<String> {
    let feeds = opml_feeds(text)?;
    if feeds.is_empty() {
        bail!("no feeds (outlines with xmlUrl) found");
    }
    let mut by_id: BTreeMap<String, OpmlFeed> = BTreeMap::new();
    for feed in feeds {
        if by_id.values().any(|f| f.url == feed.url) {
            continue;
        }
        let host = feed
            .url
            .split("://")
            .nth(1)
            .and_then(|r| r.split('/').next())
            .unwrap_or("feed")
            .trim_start_matches("www.");
        let mut base = slug(feed.title.as_deref().unwrap_or(host));
        if base.is_empty() {
            base = slug(host);
        }
        let mut id = base.clone();
        let mut n = 2;
        while by_id.contains_key(&id) {
            id = format!("{base}-{n}");
            n += 1;
        }
        by_id.insert(id, feed);
    }
    let mut out = String::from(
        "# Generated by `pressboxctl opml-to-nix`. Use it as:\n#   services.pressbox.feeds = import ./feeds.nix;\n{\n",
    );
    for (id, feed) in by_id {
        let attr = nix_attr(&id);
        let title = feed.title.filter(|t| slug(t) != id || t != &id);
        if title.is_none() && feed.tags.is_empty() {
            out.push_str(&format!("  {attr} = {};\n", nix_string(&feed.url)));
            continue;
        }
        out.push_str(&format!(
            "  {attr} = {{\n    url = {};\n",
            nix_string(&feed.url)
        ));
        if let Some(title) = title {
            out.push_str(&format!("    title = {};\n", nix_string(&title)));
        }
        if !feed.tags.is_empty() {
            let tags: Vec<String> = feed.tags.iter().map(|t| nix_string(t)).collect();
            out.push_str(&format!("    tags = [ {} ];\n", tags.join(" ")));
        }
        out.push_str("  };\n");
    }
    out.push_str("}\n");
    Ok(out)
}

/// A feed found by `discover`.
#[derive(Debug, PartialEq, Eq)]
struct Discovered {
    url: String,
    title: Option<String>,
    /// RSS, Atom or JSON Feed.
    format: &'static str,
}

/// Where sites commonly put a feed without announcing it.
const COMMON_FEED_PATHS: &[&str] = &[
    "/feed",
    "/feed.xml",
    "/rss",
    "/rss.xml",
    "/atom.xml",
    "/index.xml",
    "/feed.json",
];

async fn discover(url: &str) -> Result<Vec<Discovered>> {
    let page = url::Url::parse(url).with_context(|| format!("{url:?} is not a URL"))?;
    let http = reqwest::Client::builder()
        .user_agent(concat!("pressboxctl/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(20))
        .build()?;
    let get = |url: String| {
        let http = http.clone();
        async move {
            let resp = http.get(&url).send().await?.error_for_status()?;
            let final_url = resp.url().to_string();
            anyhow::Ok((final_url, resp.bytes().await?))
        }
    };
    let (page_url, body) = get(page.to_string())
        .await
        .with_context(|| format!("fetching {url}"))?;
    // Already a feed?
    if let Some(feed) = as_feed(&page_url, &body) {
        return Ok(vec![feed]);
    }
    let html = String::from_utf8_lossy(&body);
    let mut candidates = feed_links(&html, &page_url);
    if candidates.is_empty() {
        let base = url::Url::parse(&page_url)?;
        candidates = COMMON_FEED_PATHS
            .iter()
            .filter_map(|p| base.join(p).ok().map(String::from))
            .collect();
    }
    let mut found = Vec::new();
    for candidate in candidates {
        let Ok((final_url, body)) = get(candidate).await else {
            continue;
        };
        if let Some(feed) = as_feed(&final_url, &body)
            && !found.iter().any(|f: &Discovered| f.url == feed.url)
        {
            found.push(feed);
        }
    }
    Ok(found)
}

/// The body as a feed, if it parses as one.
fn as_feed(url: &str, body: &[u8]) -> Option<Discovered> {
    let feed = feed_rs::parser::parse(body).ok()?;
    let format = match feed.feed_type {
        feed_rs::model::FeedType::Atom => "Atom",
        feed_rs::model::FeedType::JSON => "JSON Feed",
        _ => "RSS",
    };
    Some(Discovered {
        url: url.to_owned(),
        title: feed
            .title
            .map(|t| t.content.trim().to_owned())
            .filter(|t| !t.is_empty()),
        format,
    })
}

/// Feed URLs a page announces with `<link rel="alternate" type="…">`,
/// resolved against the page's URL.
fn feed_links(html: &str, page_url: &str) -> Vec<String> {
    const FEED_TYPES: &[&str] = &[
        "application/rss+xml",
        "application/atom+xml",
        "application/feed+json",
        "application/json",
        "application/rdf+xml",
    ];
    let Ok(page) = url::Url::parse(page_url) else {
        return Vec::new();
    };
    let doc = dom_query::Document::from(html);
    let base = doc
        .select("base[href]")
        .attr("href")
        .and_then(|b| page.join(&b).ok())
        .unwrap_or(page);
    let mut links = Vec::new();
    for link in doc.select("link[href]").iter() {
        let rel = link.attr("rel").unwrap_or_default().to_ascii_lowercase();
        let kind = link.attr("type").unwrap_or_default().to_ascii_lowercase();
        if !rel.split_whitespace().any(|r| r == "alternate") || !FEED_TYPES.contains(&kind.trim()) {
            continue;
        }
        if let Some(url) = link
            .attr("href")
            .and_then(|h| base.join(h.trim()).ok())
            .map(String::from)
            && !links.contains(&url)
        {
            links.push(url);
        }
    }
    links
}

fn discovered_to_nix(feeds: &[Discovered]) -> String {
    let mut out =
        String::from("# Found by `pressboxctl discover`. Add to services.pressbox.feeds:\n");
    let mut ids = HashSet::new();
    for feed in feeds {
        let host = url::Url::parse(&feed.url)
            .ok()
            .and_then(|u| {
                u.host_str()
                    .map(|h| h.trim_start_matches("www.").to_owned())
            })
            .unwrap_or_else(|| "feed".to_owned());
        let mut base = slug(feed.title.as_deref().unwrap_or(&host));
        if base.is_empty() {
            base = slug(&host);
        }
        let mut id = base.clone();
        let mut n = 2;
        while !ids.insert(id.clone()) {
            id = format!("{base}-{n}");
            n += 1;
        }
        out.push_str(&format!(
            "# {}\n{} = {{\n  url = {};\n",
            feed.format,
            nix_attr(&id),
            nix_string(&feed.url)
        ));
        if let Some(title) = &feed.title {
            out.push_str(&format!("  title = {};\n", nix_string(title)));
        }
        out.push_str("};\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use std::{
        io::{BufRead, BufReader},
        os::unix::net::UnixListener,
        sync::mpsc,
    };

    use super::*;

    /// A stand-in control socket: answers each connection with the next
    /// canned `(status, body)` and reports the request line and body.
    struct FakeSocket {
        dir: PathBuf,
        path: PathBuf,
        requests: mpsc::Receiver<(String, String)>,
    }

    impl FakeSocket {
        /// Lives in its own directory, which it deletes when dropped, so
        /// `name` must be unique among the tests (and its own prefix keeps
        /// it apart from other tests' directories).
        fn new(name: &str, responses: Vec<(u16, &str)>) -> Self {
            let responses: Vec<(u16, String)> = responses
                .into_iter()
                .map(|(status, body)| (status, body.to_owned()))
                .collect();
            let dir = std::env::temp_dir()
                .join(format!("pressboxctl-socket-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("control.sock");
            let listener = UnixListener::bind(&path).unwrap();
            let (tx, requests) = mpsc::channel();
            std::thread::spawn(move || {
                for (status, body) in responses {
                    let (stream, _) = listener.accept().unwrap();
                    let mut reader = BufReader::new(stream);
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    let mut length = 0;
                    loop {
                        let mut header = String::new();
                        reader.read_line(&mut header).unwrap();
                        if header.trim().is_empty() {
                            break;
                        }
                        if let Some(n) = header.to_lowercase().strip_prefix("content-length:") {
                            length = n.trim().parse().unwrap();
                        }
                    }
                    let mut sent = vec![0; length];
                    reader.read_exact(&mut sent).unwrap();
                    tx.send((line.trim().to_owned(), String::from_utf8(sent).unwrap()))
                        .unwrap();
                    write!(
                        reader.get_mut(),
                        "HTTP/1.1 {status} X\r\ncontent-length: {}\r\n\r\n{body}",
                        body.len()
                    )
                    .unwrap();
                }
            });
            Self {
                dir,
                path,
                requests,
            }
        }

        /// Runs `pressboxctl -s <socket> <args>` and returns its output.
        fn run(&self, args: &[&str]) -> Result<String> {
            ctl(&[&["-s", self.path.to_str().unwrap()], args].concat())
        }

        fn request(&self) -> (String, String) {
            self.requests.recv().unwrap()
        }
    }

    impl Drop for FakeSocket {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn ctl(args: &[&str]) -> Result<String> {
        let args = Args::try_parse_from([&["pressboxctl"], args].concat())?;
        let mut out = Vec::new();
        run(args, &mut out)?;
        Ok(String::from_utf8(out).unwrap())
    }

    #[test]
    fn reports_status_users_and_devices() {
        let now = jiff::Timestamp::now().as_second();
        let devices = format!(
            r#"[{{"name":"laptop","user":"chris","created":{},"last_seen":{}}},
                {{"name":"phone","user":"anna","created":{},"last_seen":null}}]"#,
            now - 3 * 86_400,
            now - 120,
            now - 7200
        );
        let socket = FakeSocket::new(
            "status",
            vec![
                (
                    200,
                    r#"{"feeds":12,"archived_feeds":1,"failing_feeds":2,"items":3400,"users":2,"devices":3,"pending_pairings":1}"#,
                ),
                (
                    200,
                    r#"[{"name":"anna","admin":false,"devices":1,"feeds":4},{"name":"chris","admin":true,"devices":2,"feeds":8}]"#,
                ),
                (200, &devices),
                (200, &devices),
                (200, "[]"),
            ],
        );
        let status = socket.run(&["status"]).unwrap();
        assert_eq!(socket.request().0, "GET /admin/status HTTP/1.1");
        assert_eq!(
            status,
            "feeds:            12 (2 failing)\n\
             archived feeds:   1 (removed, kept for starred items)\n\
             items:            3400\n\
             users:            2\n\
             devices:          3\n\
             pending pairings: 1\n"
        );
        let users = socket.run(&["users"]).unwrap();
        assert_eq!(
            users,
            concat!(
                "anna                          4 feeds   1 devices\n",
                "chris                admin    8 feeds   2 devices\n",
            )
        );
        let all = socket.run(&["devices"]).unwrap();
        assert!(all.contains("laptop") && all.contains("phone"), "{all}");
        assert!(
            all.contains("paired 3d ago") && all.contains("last seen 2m ago"),
            "{all}"
        );
        assert!(
            all.contains("paired 2h ago") && all.contains("last seen never"),
            "{all}"
        );
        let annas = socket.run(&["devices", "--user", "anna"]).unwrap();
        assert!(
            !annas.contains("laptop") && annas.contains("phone"),
            "{annas}"
        );
        assert_eq!(socket.run(&["devices"]).unwrap(), "no devices paired\n");
    }

    #[test]
    fn manages_devices_and_pairings() {
        let socket = FakeSocket::new(
            "devices",
            vec![
                (204, ""),
                (200, r#"{"name":"tv","user":"anna","token":"secret"}"#),
                (200, r#"[{"code":"K7W-4QX","name":"phone","requested":0}]"#),
                (200, "[]"),
                (200, r#"{"name":"phone","user":"anna"}"#),
                (204, ""),
                (
                    200,
                    r#"{"path":"/var/backups/pressbox-1.db","removed":["old.db"]}"#,
                ),
                (202, ""),
                (200, "<opml/>\n"),
            ],
        );
        assert_eq!(socket.run(&["revoke", "my pc"]).unwrap(), "revoked my pc\n");
        assert_eq!(socket.request().0, "DELETE /admin/devices/my%20pc HTTP/1.1");

        let token = socket.run(&["add-device", "tv", "--user", "anna"]).unwrap();
        assert_eq!(token, "secret\n", "only the token goes to stdout");
        let (line, body) = socket.request();
        assert_eq!(line, "POST /admin/devices HTTP/1.1");
        assert_eq!(body, r#"{"name":"tv","user":"anna"}"#);

        let pending = socket.run(&["pairings"]).unwrap();
        assert!(pending.starts_with("K7W-4QX  phone"), "{pending}");
        assert_eq!(socket.run(&["pairings"]).unwrap(), "no pairing requests\n");
        let _ = (socket.request(), socket.request());

        let approved = socket
            .run(&["approve", "K7W-4QX", "--user", "anna"])
            .unwrap();
        assert_eq!(approved, "paired phone for anna\n");
        let (line, body) = socket.request();
        assert_eq!(line, "POST /admin/pairings/K7W-4QX/approve HTTP/1.1");
        assert_eq!(body, r#"{"user":"anna"}"#);

        assert_eq!(
            socket.run(&["deny", "ABC-DEF"]).unwrap(),
            "denied ABC-DEF\n"
        );
        assert_eq!(
            socket.request().0,
            "DELETE /admin/pairings/ABC-DEF HTTP/1.1"
        );

        let backup = socket.run(&["backup", "--keep", "3"]).unwrap();
        assert_eq!(backup, "/var/backups/pressbox-1.db\n");
        assert_eq!(socket.request().1, r#"{"keep":3}"#);

        assert_eq!(socket.run(&["refresh"]).unwrap(), "refreshing all feeds\n");
        assert_eq!(socket.request().0, "POST /admin/refresh HTTP/1.1");

        assert_eq!(
            socket.run(&["opml-export", "--user", "a b"]).unwrap(),
            "<opml/>\n"
        );
        assert_eq!(socket.request().0, "GET /admin/opml?user=a%20b HTTP/1.1");
    }

    #[test]
    fn explains_errors() {
        let socket = FakeSocket::new(
            "errors",
            vec![
                (400, r#"{"error":"no such user"}"#),
                (500, "plain text"),
                (404, r#"{"error":"not found"}"#),
                (200, "not json"),
            ],
        );
        let error = |args: &[&str]| format!("{:#}", socket.run(args).unwrap_err());
        assert_eq!(
            error(&["add-device", "tv", "--user", "nobody"]),
            "pressboxd: no such user (HTTP 400)"
        );
        assert_eq!(error(&["status"]), "pressboxd: plain text (HTTP 500)");
        assert_eq!(
            error(&["revoke", "ghost"]),
            "revoking \"ghost\": pressboxd: not found (HTTP 404)"
        );
        assert!(error(&["users"]).starts_with("unexpected response: not json"));

        let missing = ctl(&["-s", "/nonexistent/control.sock", "status"]).unwrap_err();
        assert!(
            format!("{missing:#}").contains("is pressboxd running"),
            "{missing:#}"
        );
    }

    #[test]
    fn converts_an_opml_file() {
        let dir = std::env::temp_dir().join(format!("pressboxctl-opml-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("feeds.opml");
        std::fs::write(&file, OPML).unwrap();
        let nix = ctl(&["opml-to-nix", file.to_str().unwrap()]).unwrap();
        assert_eq!(nix, opml_to_nix(OPML).unwrap());
        let missing = ctl(&["opml-to-nix", "/nonexistent.opml"]).unwrap_err();
        assert!(format!("{missing:#}").starts_with("reading /nonexistent.opml"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn names_odd_feeds() {
        let opml = r#"<opml><body>
            <outline title="!!!" xmlUrl="https://www.odd.example/feed"/>
            <outline title="Blog" xmlUrl="https://a.example/feed"/>
            <outline title="Blog" xmlUrl="https://b.example/feed"/>
            <outline title="Blog" xmlUrl="https://c.example/feed"/>
          </body></opml>"#;
        let nix = opml_to_nix(opml).unwrap();
        for line in ["odd-example = {", "blog = {", "blog-2 = {", "blog-3 = {"] {
            assert!(nix.contains(line), "{line} in {nix}");
        }
        assert!(feed_links("<link>", "not a url").is_empty());
        let untitled = Discovered {
            url: "https://www.example.org/feed".into(),
            title: Some("???".into()),
            format: "RSS",
        };
        assert!(discovered_to_nix(&[untitled]).contains("example-org = {"));
    }

    #[test]
    fn exports_the_only_users_opml() {
        let socket = FakeSocket::new("opml-export", vec![(200, "<opml/>")]);
        assert_eq!(socket.run(&["opml-export"]).unwrap(), "<opml/>");
        assert_eq!(socket.request().0, "GET /admin/opml HTTP/1.1");
    }

    #[test]
    fn says_how_long_ago() {
        let now = jiff::Timestamp::now().as_second();
        assert_eq!(ago(now - 5), "just now");
        assert_eq!(ago(now - 600), "10m ago");
        assert_eq!(ago(now - 3 * 3600), "3h ago");
        assert_eq!(ago(now - 2 * 86_400), "2d ago");
    }

    /// Serves `routes` (path, content type, body) on a local port, from its
    /// own thread and runtime, as `discover` blocks on its own.
    fn site(routes: Vec<(&'static str, &'static str, String)>) -> String {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async move {
                    let mut app = axum::Router::new();
                    for (path, kind, body) in routes {
                        app = app.route(
                            path,
                            axum::routing::get(move || async move {
                                ([(axum::http::header::CONTENT_TYPE, kind)], body)
                            }),
                        );
                    }
                    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                    tx.send(listener.local_addr().unwrap()).unwrap();
                    axum::serve(listener, app).await.unwrap();
                });
        });
        format!("http://{}", rx.recv().unwrap())
    }

    #[test]
    fn discovers_feeds_on_a_site() {
        const RSS: &str = r#"<rss version="2.0"><channel><title>Blog</title></channel></rss>"#;
        const ATOM: &str =
            r#"<feed xmlns="http://www.w3.org/2005/Atom"><title>Blog (Atom)</title></feed>"#;
        let html = "text/html";
        let page = r#"<html><head>
            <link rel="alternate" type="application/rss+xml" href="/posts.rss">
            <link rel="alternate" type="application/atom+xml" href="/missing.xml">
            </head></html>"#;
        let base = site(vec![
            ("/post", html, page.into()),
            ("/posts.rss", "application/rss+xml", RSS.into()),
            ("/plain", html, "<html><body>no links</body></html>".into()),
            ("/atom.xml", "application/atom+xml", ATOM.into()),
        ]);

        // Announced feeds; one announced link that 404s is skipped.
        let found = ctl(&["discover", &format!("{base}/post")]).unwrap();
        assert!(
            found.contains(&format!("url = \"{base}/posts.rss\"")),
            "{found}"
        );
        assert!(
            found.contains("# RSS") && !found.contains("missing"),
            "{found}"
        );

        // No links: the usual paths are tried.
        let guessed = ctl(&["discover", &format!("{base}/plain")]).unwrap();
        assert!(
            guessed.contains(&format!("url = \"{base}/atom.xml\"")),
            "{guessed}"
        );
        assert!(
            guessed.contains("# Atom") && guessed.contains("Blog (Atom)"),
            "{guessed}"
        );

        // A feed URL is its own answer.
        let direct = ctl(&["discover", &format!("{base}/posts.rss")]).unwrap();
        assert_eq!(direct.matches("url =").count(), 1, "{direct}");

        let empty = site(vec![("/", html, "<html></html>".into())]);
        let none = ctl(&["discover", &empty]).unwrap_err();
        assert!(
            format!("{none:#}").starts_with("no feeds found"),
            "{none:#}"
        );
        let bad = ctl(&["discover", "not a url"]).unwrap_err();
        assert!(format!("{bad:#}").contains("is not a URL"), "{bad:#}");
        let down = ctl(&["discover", &format!("{empty}/nothing-here")]).unwrap_err();
        assert!(format!("{down:#}").starts_with("fetching"), "{down:#}");
    }

    const OPML: &str = r#"<?xml version="1.0"?>
        <opml version="2.0"><head><title>Feedly</title></head><body>
          <outline text="Tech" title="Tech">
            <outline type="rss" text="LWN.net" title="LWN.net" xmlUrl="https://lwn.net/headlines/rss"/>
            <outline type="rss" text="Hacker News" xmlUrl="https://news.ycombinator.com/rss" category="news,Link Aggregators"/>
            <outline text="Nix Stuff">
              <outline type="rss" title="NixOS &amp; ${friends}" xmlUrl="https://nixos.org/blog/announcements-rss.xml"/>
            </outline>
          </outline>
          <outline type="rss" text="" xmlUrl="https://www.example.com/feed"/>
          <outline type="rss" text="lwn" xmlUrl="https://lwn.net/other"/>
          <outline type="rss" text="Dupe" xmlUrl="https://lwn.net/headlines/rss"/>
          <outline type="rss" text="1Password Blog" xmlUrl="https://blog.1password.com/index.xml"/>
        </body></opml>"#;

    #[test]
    fn reads_opml() {
        let feeds = opml_feeds(OPML).unwrap();
        assert_eq!(feeds.len(), 7);
        assert_eq!(feeds[1].tags, ["tech", "news", "link-aggregators"]);
        assert_eq!(feeds[2].tags, ["tech", "nix-stuff"]);
        assert_eq!(feeds[3].title, None);
    }

    #[test]
    fn converts_to_nix() {
        let nix = opml_to_nix(OPML).unwrap();
        let expected = r#"# Generated by `pressboxctl opml-to-nix`. Use it as:
#   services.pressbox.feeds = import ./feeds.nix;
{
  "1password-blog" = {
    url = "https://blog.1password.com/index.xml";
    title = "1Password Blog";
  };
  example-com = "https://www.example.com/feed";
  hacker-news = {
    url = "https://news.ycombinator.com/rss";
    title = "Hacker News";
    tags = [ "tech" "news" "link-aggregators" ];
  };
  lwn = "https://lwn.net/other";
  lwn-net = {
    url = "https://lwn.net/headlines/rss";
    title = "LWN.net";
    tags = [ "tech" ];
  };
  nixos-friends = {
    url = "https://nixos.org/blog/announcements-rss.xml";
    title = "NixOS & \${friends}";
    tags = [ "tech" "nix-stuff" ];
  };
}
"#;
        assert_eq!(nix, expected);
    }

    #[test]
    fn rejects_bad_opml() {
        assert!(opml_to_nix("<opml><body></body></opml>").is_err());
        assert!(opml_to_nix("not xml").is_err());
    }

    #[test]
    fn parses_http() {
        let plain = b"HTTP/1.1 404 Not Found\r\ncontent-length: 2\r\n\r\n{}";
        assert_eq!(parse_response(plain).unwrap(), (404, "{}".to_owned()));
        let chunked =
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n";
        assert_eq!(
            parse_response(chunked).unwrap(),
            (200, "hello world".to_owned())
        );
        assert!(parse_response(b"garbage").is_err());
    }

    #[test]
    fn helpers() {
        assert_eq!(slug("  Hello, World!  "), "hello-world");
        assert_eq!(nix_attr("lwn"), "lwn");
        assert_eq!(nix_attr("9to5"), "\"9to5\"");
        assert_eq!(encode("K7W-4QX"), "K7W-4QX");
        assert_eq!(encode("a b/c"), "a%20b%2Fc");
    }

    #[test]
    fn finds_announced_feeds() {
        let html = r#"<html><head>
            <link rel="stylesheet" href="/style.css">
            <link rel="alternate" type="application/rss+xml" title="Posts" href="/posts.rss">
            <link rel="Alternate" type="application/atom+xml" href="https://cdn.example.org/atom.xml">
            <link rel="alternate" type="text/html" hreflang="de" href="/de/">
            <link rel="alternate" type="application/feed+json" href="feed.json">
            <link rel="alternate" type="application/rss+xml" href="/posts.rss">
            </head><body><a href="/other.rss">not announced</a></body></html>"#;
        assert_eq!(
            feed_links(html, "https://example.org/blog/post"),
            [
                "https://example.org/posts.rss",
                "https://cdn.example.org/atom.xml",
                "https://example.org/blog/feed.json",
            ]
        );
        let with_base = r#"<base href="https://other.example/x/"><link rel="alternate" type="application/rss+xml" href="rss">"#;
        assert_eq!(
            feed_links(with_base, "https://example.org/"),
            ["https://other.example/x/rss"]
        );
    }

    #[test]
    fn recognizes_feeds_and_prints_nix() {
        let rss = br#"<rss version="2.0"><channel><title> Example "Blog" </title></channel></rss>"#;
        let atom =
            br#"<feed xmlns="http://www.w3.org/2005/Atom"><title>Example Blog</title></feed>"#;
        assert_eq!(
            as_feed("https://example.org/", b"<html><body>hi</body></html>"),
            None
        );
        let feeds = [
            as_feed("https://example.org/rss", rss).unwrap(),
            as_feed("https://example.org/atom", atom).unwrap(),
            as_feed(
                "https://www.example.org/notitle",
                br#"{"version":"https://jsonfeed.org/version/1.1","title":"","items":[]}"#,
            )
            .unwrap(),
        ];
        assert_eq!(feeds[1].format, "Atom");
        assert_eq!(
            discovered_to_nix(&feeds),
            "# Found by `pressboxctl discover`. Add to services.pressbox.feeds:\n\
             # RSS\nexample-blog = {\n  url = \"https://example.org/rss\";\n  title = \"Example \\\"Blog\\\"\";\n};\n\
             # Atom\nexample-blog-2 = {\n  url = \"https://example.org/atom\";\n  title = \"Example Blog\";\n};\n\
             # JSON Feed\nexample-org = {\n  url = \"https://www.example.org/notitle\";\n};\n"
        );
    }
}
