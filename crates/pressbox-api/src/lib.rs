// SPDX-FileCopyrightText: 2026 Chris Palmer <chris@red-oxide.org>
// SPDX-License-Identifier: GPL-3.0-only

//! Types shared by the pressbox server and its Rust clients.
//!
//! These are the JSON shapes of the HTTP API. Timestamps are Unix seconds
//! (UTC) so every client (Rust, QML, Swift) can decode them without a date
//! parser.

use serde::{Deserialize, Serialize};

/// A feed declared in the server configuration, with its current state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Feed {
    /// The attribute name used in the Nix configuration, e.g. `"lwn"`.
    pub id: String,
    /// The configured title, or the title the feed reports about itself.
    pub title: String,
    /// The URL that is polled.
    pub url: String,
    /// The website the feed belongs to, if it says.
    pub site_url: Option<String>,
    pub tags: Vec<String>,
    pub unread: u32,
    pub total: u32,
    /// How many of `total` are podcast episodes (items with media), so
    /// clients can tell podcasts from news. Absent from servers before
    /// 26.0.
    #[serde(default)]
    pub episodes: u32,
    /// How many of `unread` are episodes.
    #[serde(default)]
    pub unread_episodes: u32,
    /// When the feed was last fetched, successfully or not.
    pub last_fetched: Option<i64>,
    /// The error from the last fetch, if it failed.
    pub last_error: Option<String>,
    /// The feed was removed from the configuration but is kept because it
    /// has starred items. It is no longer fetched.
    #[serde(default)]
    pub archived: bool,
    /// The feed's artwork (`itunes:image`, the RSS `<image>`, Atom's logo).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// `podcast:medium`: `podcast`, `music`, `video`, `audiobook`, ...
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub medium: Option<String>,
}

/// One entry from a feed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Item {
    pub id: i64,
    pub feed_id: String,
    pub title: String,
    pub url: Option<String>,
    pub author: Option<String>,
    /// Short HTML summary, if the feed provides one.
    pub summary: Option<String>,
    /// Full HTML content, if the feed provides it. Only included by
    /// `GET /api/v1/items/{id}`; list responses leave it out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Publication time, or the time pressbox first saw the item.
    pub published: i64,
    pub read: bool,
    pub starred: bool,
    /// Audio or video attached to the item (a podcast episode), if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media: Option<Media>,
    /// Where this user stopped playing the media, in seconds; 0 if they
    /// haven't started it (or finished it).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub position: u32,
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde passes a reference
const fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// An item's audio or video, from RSS `<enclosure>`, Atom `rel="enclosure"`
/// links and JSON Feed attachments, with the iTunes and Podcasting 2.0
/// (`podcast:`) tags that describe it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Media {
    /// The files: the feed's own enclosure first, then any
    /// `podcast:alternateEnclosure`s (other formats, bitrates or video) in
    /// the order the feed lists them. Never empty.
    pub enclosures: Vec<Enclosure>,
    /// Length in seconds, if the feed says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<u32>,
    /// The episode's artwork, if it has its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub season: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub episode: Option<u32>,
    /// The `podcast:chapters` file. Clients use `GET
    /// /api/v1/items/{id}/chapters`, which reads it for them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chapters: Option<String>,
    /// The `podcast:transcript` files. Clients use `GET
    /// /api/v1/items/{id}/transcript`, which reads the best one for them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transcripts: Vec<TranscriptFile>,
}

impl Media {
    /// The first enclosure whose MIME type is `video/*`, if any.
    #[must_use]
    pub fn video(&self) -> Option<&Enclosure> {
        self.enclosures.iter().find(|e| e.is_video())
    }

    /// The first enclosure that isn't video (the feed's own, usually).
    #[must_use]
    pub fn audio(&self) -> Option<&Enclosure> {
        self.enclosures.iter().find(|e| !e.is_video())
    }
}

/// One media file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Enclosure {
    /// An `http(s)` URL.
    pub url: String,
    /// e.g. `audio/mpeg`, `video/mp4`, `application/x-mpegURL` (HLS).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
    /// Size in bytes, if the feed says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length: Option<u64>,
    /// Bits per second, if the feed says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bitrate: Option<u32>,
    /// Video height in pixels, if the feed says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    /// A label for choosing between enclosures, e.g. `"1080p"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

impl Enclosure {
    #[must_use]
    pub fn is_video(&self) -> bool {
        self.mime
            .as_deref()
            .is_some_and(|m| m.starts_with("video/"))
    }
}

/// A `podcast:transcript` file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TranscriptFile {
    pub url: String,
    /// e.g. `text/vtt`, `application/x-subrip`, `application/json`,
    /// `text/html`.
    pub mime: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
    /// `"captions"` for closed captions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rel: Option<String>,
}

/// Body for `PUT /api/v1/items/{id}/position`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Position {
    /// Seconds into the media. 0 forgets the position (when the media was
    /// finished, say).
    pub position: u32,
}

/// Response to `GET` and `PUT /api/v1/queue`: the user's Up Next list, the
/// episodes to play next in order. Items come as in lists, without `content`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Queue {
    pub items: Vec<Item>,
}

/// Body for `PUT /api/v1/queue`: the whole list in order, replacing the one
/// before. Clients add, remove and reorder by sending the list they want.
/// Items the user can't see, items without media, and repeats are left out.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct QueueOrder {
    pub ids: Vec<i64>,
}

/// Response to `GET /api/v1/items/{id}/chapters`, from the episode's
/// Podcasting 2.0 chapters file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Chapters {
    /// In order of `start`.
    pub chapters: Vec<Chapter>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Chapter {
    /// Seconds from the start.
    pub start: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Artwork to show during the chapter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// A web page about the chapter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Only changes the artwork: leave it out of a chapter list.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
}

/// Response to `GET /api/v1/items/{id}/transcript`: the episode's best
/// transcript (JSON, WebVTT, SRT or HTML), as timed segments. Very short
/// cues (some files time every word) are joined into sentences.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Transcript {
    pub segments: Vec<Segment>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Segment {
    /// Seconds from the start; 0 for untimed (HTML or plain) transcripts.
    pub start: f64,
    pub end: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,
    /// Plain text.
    pub text: String,
}

/// A page of items, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ItemPage {
    pub items: Vec<Item>,
    /// Pass as `cursor` to get the next page; `None` on the last page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// Pass as `offset` to get the next page; `None` on the last page, and
    /// when paging by `cursor`. Kept for older clients; prefer `next_cursor`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_offset: Option<u32>,
}

/// Body for `PUT /api/v1/items/{id}/read` and `PUT /api/v1/items/{id}/starred`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Flag {
    pub value: bool,
}

/// Response for `POST /api/v1/feeds/{id}/read-all`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Updated {
    pub updated: u32,
}

/// Body of `POST /api/v1/items/read-all`: marks the items a list shows as
/// read, in one request however many pages the list has.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct MarkRead {
    /// The list's filters, as for `GET /api/v1/items`. `limit`, `offset`
    /// and `cursor` are ignored.
    #[serde(flatten)]
    pub query: ItemQuery,
    /// Only this item and the ones after it in the list (older ones), if
    /// set. Nothing is marked if the user can't see this item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<i64>,
}

/// Query parameters for `GET /api/v1/items`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema, utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct ItemQuery {
    /// Full-text search over titles and text. Words are matched as prefixes
    /// and all must appear.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub q: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feed: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unread: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub starred: Option<bool>,
    /// `true`: only items with media; `false`: only items without.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media: Option<bool>,
    /// `true`: only unread items with a saved playback position, i.e.
    /// started and not finished.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_progress: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Skips this many items. Slow deep into a long list; prefer `cursor`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<u32>,
    /// Starts after the item a previous page's `next_cursor` names. Opaque;
    /// takes precedence over `offset`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

/// Body for `POST /api/v1/pair`: a client asking to be paired.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PairRequest {
    /// A name for the device, e.g. `"laptop"`. Shown when approving.
    pub name: String,
}

/// Response to `POST /api/v1/pair`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PairStart {
    /// Short code to approve, e.g. `"K7-4QX"`. Show it to the user.
    pub code: String,
    /// Secret to poll `GET /api/v1/pair/{poll_id}` with. Keep it private.
    pub poll_id: String,
    /// Seconds until the request expires.
    pub expires_in: u64,
}

/// Response to `GET /api/v1/pair/{poll_id}` once approved. Returned only once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PairApproved {
    /// The device's API token.
    pub token: String,
    /// The name it was registered under (made unique if needed).
    pub name: String,
}

/// Response to `GET /api/v1/pair/{poll_id}` (202) while the request waits
/// for approval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PairPending {
    /// Always `"pending"`.
    pub status: String,
}

/// A pairing request waiting for approval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PendingPairing {
    pub code: String,
    pub name: String,
    /// When it was requested (Unix seconds).
    pub requested: i64,
}

/// A paired device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Device {
    pub name: String,
    /// The user it belongs to.
    pub user: String,
    pub created: i64,
    pub last_seen: Option<i64>,
}

/// Optional body for approving a pairing. Only admins may name another
/// user; by default the device goes to the approver's user.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ApproveRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
}

/// Response to approving a pairing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Approved {
    /// The device's name.
    pub name: String,
    /// The user it was added to.
    pub user: String,
}

/// A user, as `pressboxctl users` lists them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UserInfo {
    pub name: String,
    pub admin: bool,
    pub devices: u32,
    pub feeds: u32,
}

/// The body of every error response (4xx and 5xx), e.g.
/// `{"error": "not found"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ErrorResponse {
    /// What went wrong, in English, for showing to the user.
    pub error: String,
}

/// The API version this crate describes: the `1` in `/api/v1`.
pub const API_VERSION: u32 = 1;

/// `GET /api/v1/version`, which needs no token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Version {
    /// pressboxd's version, e.g. `26.0.0`.
    pub version: String,
    /// The API version, as in `/api/v1`.
    pub api: u32,
}

/// Response to `GET /api/v1/me`: who this token belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Me {
    pub user: String,
    pub admin: bool,
    /// The device's name, or `None` for the static token.
    pub device: Option<String>,
}

/// Body for `POST /admin/devices`: create a device without pairing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct NewDeviceRequest {
    pub name: String,
    /// Required when there is more than one user.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
}

/// Response to creating a device directly (control socket only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct NewDevice {
    pub name: String,
    pub user: String,
    pub token: String,
}

/// Body for `POST /admin/backup`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct BackupRequest {
    /// How many backups to keep, newest first. Older ones are deleted.
    pub keep: u32,
}

/// Response to `POST /admin/backup`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct BackupResult {
    pub path: String,
    pub removed: Vec<String>,
}

/// Response to `GET /admin/status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Status {
    /// Feed URLs being fetched (each once, however many users follow it).
    pub feeds: u32,
    pub archived_feeds: u32,
    pub failing_feeds: u32,
    pub items: u32,
    pub users: u32,
    pub devices: u32,
    pub pending_pairings: u32,
}
