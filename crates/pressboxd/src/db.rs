// SPDX-FileCopyrightText: 2026 Chris Palmer <chris@red-oxide.org>
// SPDX-License-Identifier: AGPL-3.0-only

//! SQLite storage for users, feeds, items, read state and paired devices.
//!
//! Each feed URL is a *source*, fetched once however many users follow it.
//! A user's feeds are *subscriptions* to sources (with their own id, title
//! and tags), and read/starred flags live in `item_state`, per user.

use std::{
    collections::{BTreeSet, HashMap},
    path::Path,
};

use anyhow::{Context, Result, bail};
use pressbox_api::{Device, Feed, Item, ItemPage, ItemQuery, Media, UserInfo};
use rusqlite::{Connection, OptionalExtension, ToSql, params, params_from_iter};

use crate::{config::Config, sanitize};

pub const DEFAULT_LIMIT: u32 = 50;
pub const MAX_LIMIT: u32 = 500;

/// Schema changes, applied in order. `PRAGMA user_version` records how many
/// have run. Never edit one that has shipped; append a new one.
const MIGRATIONS: &[&str] = &[
    // 1: the original schema.
    r"
    CREATE TABLE IF NOT EXISTS feeds (
        id               TEXT PRIMARY KEY,
        url              TEXT NOT NULL,
        configured_title TEXT,
        title            TEXT,
        site_url         TEXT,
        tags             TEXT NOT NULL DEFAULT '[]',
        etag             TEXT,
        last_modified    TEXT,
        last_fetched     INTEGER,
        last_error       TEXT
    );
    CREATE TABLE IF NOT EXISTS items (
        id         INTEGER PRIMARY KEY,
        feed_id    TEXT NOT NULL REFERENCES feeds(id) ON DELETE CASCADE,
        guid       TEXT NOT NULL,
        title      TEXT NOT NULL,
        url        TEXT,
        author     TEXT,
        summary    TEXT,
        content    TEXT,
        published  INTEGER NOT NULL,
        read       INTEGER NOT NULL DEFAULT 0,
        starred    INTEGER NOT NULL DEFAULT 0,
        UNIQUE (feed_id, guid)
    );
    CREATE INDEX IF NOT EXISTS items_by_date ON items (published DESC, id DESC);
    CREATE INDEX IF NOT EXISTS items_by_feed ON items (feed_id, published DESC, id DESC);
    ",
    // 2: archived feeds, per-feed scheduling, devices, full-text search.
    // Existing items are cleaned and indexed by `reindex_all` afterwards.
    r"
    ALTER TABLE feeds ADD COLUMN archived INTEGER NOT NULL DEFAULT 0;
    ALTER TABLE feeds ADD COLUMN refresh_interval INTEGER;
    ALTER TABLE feeds ADD COLUMN next_fetch INTEGER NOT NULL DEFAULT 0;
    ALTER TABLE feeds ADD COLUMN fail_count INTEGER NOT NULL DEFAULT 0;
    CREATE TABLE devices (
        id         INTEGER PRIMARY KEY,
        name       TEXT NOT NULL UNIQUE,
        token_hash TEXT NOT NULL UNIQUE,
        created    INTEGER NOT NULL,
        last_seen  INTEGER
    );
    CREATE VIRTUAL TABLE items_fts USING fts5(title, body, tokenize = 'unicode61 remove_diacritics 2');
    CREATE TRIGGER items_fts_delete AFTER DELETE ON items BEGIN
        DELETE FROM items_fts WHERE rowid = old.id;
    END;
    ",
    // 3: multiple users. Items are stored once per feed URL (`sources`);
    // users subscribe to sources, and read/starred state is per user. The
    // existing data goes to user 1, a placeholder `sync` renames to the
    // first configured user. Runs with foreign keys off.
    r"
    CREATE TABLE users (
        id    INTEGER PRIMARY KEY,
        name  TEXT NOT NULL UNIQUE,
        admin INTEGER NOT NULL DEFAULT 0
    );
    INSERT INTO users (id, name, admin) VALUES (1, '', 1);

    CREATE TABLE sources (
        url           TEXT PRIMARY KEY,
        title         TEXT,
        site_url      TEXT,
        etag          TEXT,
        last_modified TEXT,
        last_fetched  INTEGER,
        last_error    TEXT,
        next_fetch    INTEGER NOT NULL DEFAULT 0,
        fail_count    INTEGER NOT NULL DEFAULT 0
    );
    INSERT OR IGNORE INTO sources
        (url, title, site_url, etag, last_modified, last_fetched, last_error, next_fetch, fail_count)
        SELECT url, title, site_url, etag, last_modified, last_fetched, last_error, next_fetch, fail_count
        FROM feeds;

    CREATE TABLE subscriptions (
        user_id          INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
        feed_id          TEXT NOT NULL,
        source           TEXT NOT NULL REFERENCES sources(url) ON DELETE CASCADE,
        title            TEXT,
        tags             TEXT NOT NULL DEFAULT '[]',
        refresh_interval INTEGER,
        archived         INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (user_id, feed_id)
    );
    CREATE INDEX subscriptions_by_source ON subscriptions (source, archived);
    INSERT INTO subscriptions (user_id, feed_id, source, title, tags, refresh_interval, archived)
        SELECT 1, id, url, configured_title, tags, refresh_interval, archived FROM feeds;

    CREATE TABLE items_new (
        id        INTEGER PRIMARY KEY,
        source    TEXT NOT NULL REFERENCES sources(url) ON DELETE CASCADE,
        guid      TEXT NOT NULL,
        title     TEXT NOT NULL,
        url       TEXT,
        author    TEXT,
        summary   TEXT,
        content   TEXT,
        published INTEGER NOT NULL,
        UNIQUE (source, guid)
    );
    INSERT OR IGNORE INTO items_new (id, source, guid, title, url, author, summary, content, published)
        SELECT i.id, f.url, i.guid, i.title, i.url, i.author, i.summary, i.content, i.published
        FROM items i JOIN feeds f ON f.id = i.feed_id;

    CREATE TABLE item_state (
        user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
        item_id INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
        read    INTEGER NOT NULL DEFAULT 0,
        starred INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (user_id, item_id)
    ) WITHOUT ROWID;
    INSERT INTO item_state (user_id, item_id, read, starred)
        SELECT 1, id, read, starred FROM items
        WHERE (read = 1 OR starred = 1) AND id IN (SELECT id FROM items_new);
    CREATE INDEX item_state_by_item ON item_state (item_id, starred);

    DROP TRIGGER items_fts_delete;
    DROP TABLE items;
    ALTER TABLE items_new RENAME TO items;
    CREATE INDEX items_by_date ON items (published DESC, id DESC);
    CREATE INDEX items_by_source ON items (source, published DESC, id DESC);
    CREATE TRIGGER items_fts_delete AFTER DELETE ON items BEGIN
        DELETE FROM items_fts WHERE rowid = old.id;
    END;
    DELETE FROM items_fts WHERE rowid NOT IN (SELECT id FROM items);

    ALTER TABLE devices ADD COLUMN user_id INTEGER NOT NULL DEFAULT 1 REFERENCES users(id) ON DELETE CASCADE;
    DROP TABLE feeds;
    ",
    // 4: faster Starred lists, and a digest of each item's text in a
    // covering index, so a re-fetch can tell unchanged items without reading
    // their (large) rows. `rebuild_search` then fills the digests and
    // rebuilds the search index smaller (content only, not content + summary).
    r"
    CREATE INDEX item_state_starred ON item_state (user_id, starred);
    ALTER TABLE items ADD COLUMN digest TEXT NOT NULL DEFAULT '';
    CREATE INDEX items_lookup ON items (source, guid, digest, published);
    ",
    // 5: full articles. A subscription can ask for each item's page to be
    // fetched and its main content extracted into `full_content` (NULL: not
    // tried yet, '': tried and failed), kept apart from the feed's content
    // so re-fetching the feed doesn't overwrite it.
    r"
    ALTER TABLE subscriptions ADD COLUMN full_text INTEGER NOT NULL DEFAULT 0;
    ALTER TABLE items ADD COLUMN full_content TEXT;
    ",
    // 6: a digest of each item as the feed sent it, before sanitizing, so a
    // re-fetch can skip sanitizing entries it has already stored. It joins
    // the covering lookup index; '' (older rows) never matches, so those are
    // sanitized once more and then get one.
    r"
    ALTER TABLE items ADD COLUMN source_digest TEXT NOT NULL DEFAULT '';
    DROP INDEX items_lookup;
    CREATE INDEX items_lookup ON items (source, guid, digest, published, source_digest);
    ",
    // 7: podcasts. An item's audio/video and what describes it, as the API's
    // `Media` in JSON (NULL: none); a feed's artwork and `podcast:medium`;
    // each user's playback position (seconds; 0: none) beside their flags;
    // and chapters and transcripts read from their files, by URL, as the
    // API's JSON, so they are fetched once rather than per request.
    r"
    ALTER TABLE items ADD COLUMN media TEXT;
    ALTER TABLE sources ADD COLUMN image TEXT;
    ALTER TABLE sources ADD COLUMN medium TEXT;
    ALTER TABLE item_state ADD COLUMN position INTEGER NOT NULL DEFAULT 0;
    CREATE INDEX item_state_in_progress ON item_state (user_id, position) WHERE position > 0;
    CREATE TABLE media_files (
        url     TEXT PRIMARY KEY,
        json    TEXT NOT NULL,
        fetched INTEGER NOT NULL
    );
    ",
    // 8: fetch every feed in full once, and soon. Items stored before 7 have
    // no media, and a feed only gets read again when it changes: most
    // podcast hosts answer the cached ETag with 304, which would leave
    // existing episodes without their audio until the next one comes out.
    r"
    UPDATE sources SET etag = NULL, last_modified = NULL, next_fetch = 0;
    ",
    // 9: each user's Up Next queue of episodes, in `rank` order. Entries go
    // with their item or user.
    r"
    CREATE TABLE queue (
        user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
        item_id INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
        rank    INTEGER NOT NULL,
        PRIMARY KEY (user_id, item_id)
    ) WITHOUT ROWID;
    CREATE INDEX queue_by_item ON queue (item_id);
    ",
    // 10: fetch feeds with no artwork in full once. A 304 used to clear a
    // feed's artwork and medium, so feeds that answer their ETag lost them
    // after the first unchanged fetch.
    r"
    UPDATE sources SET etag = NULL, last_modified = NULL, next_fetch = 0
    WHERE image IS NULL;
    ",
];

/// The most episodes a user's queue holds.
pub const MAX_QUEUE: usize = 500;

/// An entry parsed (and sanitized) from a feed, ready to store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewItem {
    pub guid: String,
    pub title: String,
    pub url: Option<String>,
    pub author: Option<String>,
    pub summary: Option<String>,
    pub content: Option<String>,
    pub published: Option<i64>,
    pub media: Option<Media>,
}

/// What the fetcher needs to poll a feed URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchTarget {
    /// The feed URL, which is also its key.
    pub url: String,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    /// Seconds between fetches: the shortest any follower asked for.
    pub interval: u64,
    /// Consecutive failed fetches so far.
    pub fail_count: u32,
    /// Whether any follower wants full articles fetched.
    pub full_text: bool,
}

/// The outcome of polling a feed.
#[derive(Debug, Clone, Default)]
pub struct FetchResult {
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub title: Option<String>,
    pub site_url: Option<String>,
    /// The feed's artwork and `podcast:medium`.
    pub image: Option<String>,
    pub medium: Option<String>,
    pub error: Option<String>,
    /// The server answered 304: the feed is as last read, so its artwork and
    /// medium stay as stored rather than being cleared.
    pub not_modified: bool,
    /// When to fetch next (Unix seconds).
    pub next_fetch: i64,
    pub fail_count: u32,
}

/// Counts for `pressboxctl status` and the systemd status line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    /// Feed URLs being fetched.
    pub feeds: u32,
    /// Removed feeds kept for someone's starred items.
    pub archived_feeds: u32,
    pub failing_feeds: u32,
    pub items: u32,
    pub users: u32,
    pub devices: u32,
}

/// Who a request is from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthUser {
    pub id: i64,
    pub name: String,
    pub admin: bool,
    /// The device's name, or `None` for the static token.
    pub device: Option<String>,
}

pub struct Db {
    conn: Connection,
}

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        Self::init(Connection::open(path)?)
    }

    /// A read-only connection to an existing, migrated database, for the
    /// read pool. With WAL, readers never wait for the writer.
    pub fn open_reader(path: &Path) -> Result<Self> {
        let conn = Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.pragma_update(None, "query_only", "ON")?;
        Ok(Self { conn })
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        // With WAL, NORMAL is crash-safe (a power cut can lose only the last
        // commits, never corrupt the database) and skips an fsync per commit.
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let mut db = Self { conn };
        db.migrate()?;
        db.conn.pragma_update(None, "foreign_keys", "ON")?;
        Ok(db)
    }

    /// Runs pending migrations with foreign keys off (table rebuilds need
    /// that), then checks the result is consistent.
    fn migrate(&mut self) -> Result<()> {
        let version: i64 = self
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))?;
        let version = usize::try_from(version).unwrap_or(0);
        if version > MIGRATIONS.len() {
            bail!(
                "database schema version {version} is newer than this pressboxd ({}); upgrade pressboxd",
                MIGRATIONS.len()
            );
        }
        self.conn.pragma_update(None, "foreign_keys", "OFF")?;
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(version) {
            let tx = self.conn.transaction()?;
            tx.execute_batch(sql)
                .with_context(|| format!("database migration {}", i + 1))?;
            let number = i + 1;
            tx.pragma_update(None, "user_version", i64::try_from(number)?)?;
            tx.commit()?;
            // Data work a migration's comment promises, done in Rust.
            match number {
                2 => self.reindex_all()?,
                4 => self.rebuild_search()?,
                _ => {}
            }
        }
        let broken: Option<String> = self
            .conn
            .query_row("PRAGMA foreign_key_check", [], |r| r.get(0))
            .optional()?;
        if let Some(table) = broken {
            bail!("database is inconsistent after migrating (table {table})");
        }
        Ok(())
    }

    /// Re-sanitizes every stored item and rebuilds the search index. Run once
    /// when upgrading from a version that stored feed HTML as-is.
    fn reindex_all(&mut self) -> Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut select = tx.prepare(
                "SELECT i.id, i.title, i.summary, i.content, i.url, f.url
                 FROM items i JOIN feeds f ON f.id = i.feed_id",
            )?;
            type Row = (
                i64,
                String,
                Option<String>,
                Option<String>,
                Option<String>,
                String,
            );
            let rows: Vec<Row> = select
                .query_map([], |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                })?
                .collect::<Result<_, _>>()?;
            let mut update =
                tx.prepare("UPDATE items SET summary = ?2, content = ?3 WHERE id = ?1")?;
            tx.execute("DELETE FROM items_fts", [])?;
            let mut index = tx.prepare(FTS_INSERT)?;
            for (id, title, summary, content, item_url, feed_url) in rows {
                let base = sanitize::base_url(item_url.as_deref(), &feed_url);
                let summary = summary.map(|s| sanitize::html(&s, Some(&base)));
                let content = content.map(|c| sanitize::html(&c, Some(&base)));
                update.execute(params![id, summary, content])?;
                index.execute(params![
                    id,
                    title,
                    search_body(summary.as_deref(), content.as_deref())
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Fills item digests and rebuilds the search index from the stored items.
    fn rebuild_search(&mut self) -> Result<()> {
        let tx = self.conn.transaction()?;
        {
            tx.execute("DELETE FROM items_fts", [])?;
            let mut select =
                tx.prepare("SELECT id, title, url, author, summary, content FROM items")?;
            let mut index = tx.prepare(FTS_INSERT)?;
            let mut set_digest = tx.prepare("UPDATE items SET digest = ?2 WHERE id = ?1")?;
            let mut rows = select.query([])?;
            while let Some(r) = rows.next()? {
                let item = NewItem {
                    guid: String::new(),
                    title: r.get(1)?,
                    url: r.get(2)?,
                    author: r.get(3)?,
                    summary: r.get(4)?,
                    content: r.get(5)?,
                    published: None,
                    media: None,
                };
                let id: i64 = r.get(0)?;
                set_digest.execute(params![id, digest(&item)])?;
                index.execute(params![
                    id,
                    item.title,
                    search_body(item.summary.as_deref(), item.content.as_deref())
                ])?;
            }
        }
        tx.execute("INSERT INTO items_fts (items_fts) VALUES ('optimize')", [])?;
        tx.commit()?;
        Ok(())
    }

    /// Makes users and their subscriptions match the configuration.
    ///
    /// A feed removed from a user's config loses its read and unread items
    /// for that user; if they starred some, it is kept as archived (not
    /// fetched for them) until they unstar them. Removing a user deletes
    /// their devices and state.
    pub fn sync(&mut self, config: &Config) -> Result<()> {
        let users = config.users();
        let tx = self.conn.transaction()?;
        {
            // The placeholder from migration 3 becomes the first admin (or
            // first user), keeping the state of the single-user days.
            let heir = users
                .iter()
                .find(|(_, u)| u.admin)
                .or_else(|| users.iter().next())
                .map(|(name, _)| name.clone());
            if let Some(heir) = heir {
                let taken: bool = tx
                    .query_row("SELECT 1 FROM users WHERE name = ?1", [&heir], |_| Ok(()))
                    .optional()?
                    .is_some();
                if !taken {
                    tx.execute("UPDATE users SET name = ?1 WHERE name = ''", [&heir])?;
                }
            }
            tx.execute("DELETE FROM users WHERE name = ''", [])?;

            let names: Vec<&String> = users.keys().collect();
            tx.execute(
                &format!(
                    "DELETE FROM users WHERE name NOT IN ({})",
                    vec!["?"; names.len()].join(",")
                ),
                params_from_iter(&names),
            )?;

            for (name, user) in &users {
                tx.execute(
                    "INSERT INTO users (name, admin) VALUES (?1, ?2)
                     ON CONFLICT (name) DO UPDATE SET admin = excluded.admin",
                    params![name, user.admin],
                )?;
                let uid: i64 =
                    tx.query_row("SELECT id FROM users WHERE name = ?1", [name], |r| r.get(0))?;

                for (feed_id, feed) in &user.feeds {
                    let followed: bool = tx
                        .query_row(
                            "SELECT 1 FROM subscriptions WHERE source = ?1 AND archived = 0",
                            [&feed.url],
                            |_| Ok(()),
                        )
                        .optional()?
                        .is_some();
                    tx.execute(
                        "INSERT OR IGNORE INTO sources (url) VALUES (?1)",
                        [&feed.url],
                    )?;
                    if !followed {
                        // Not fetched until now: its cached headers would
                        // hide items deleted meanwhile, so fetch it fully.
                        tx.execute(
                            "UPDATE sources SET etag = NULL, last_modified = NULL, next_fetch = 0,
                                fail_count = 0
                             WHERE url = ?1",
                            [&feed.url],
                        )?;
                    }
                    let tags = serde_json::to_string(&feed.tags)?;
                    let interval = feed.refresh_interval.map(i64::try_from).transpose()?;
                    tx.execute(
                        "INSERT INTO subscriptions
                            (user_id, feed_id, source, title, tags, refresh_interval, full_text)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                         ON CONFLICT (user_id, feed_id) DO UPDATE SET
                            source = excluded.source, title = excluded.title, tags = excluded.tags,
                            refresh_interval = excluded.refresh_interval,
                            full_text = excluded.full_text, archived = 0",
                        params![
                            uid,
                            feed_id,
                            feed.url,
                            feed.title,
                            tags,
                            interval,
                            feed.full_text
                        ],
                    )?;
                }

                let configured: BTreeSet<&String> = user.feeds.keys().collect();
                let existing: Vec<String> = tx
                    .prepare("SELECT feed_id FROM subscriptions WHERE user_id = ?1")?
                    .query_map([uid], |r| r.get(0))?
                    .collect::<Result<_, _>>()?;
                for feed_id in existing.iter().filter(|f| !configured.contains(f)) {
                    // Archive it; garbage collection drops it again if the
                    // user starred nothing in it.
                    tx.execute(
                        "UPDATE subscriptions SET archived = 1 WHERE user_id = ?1 AND feed_id = ?2",
                        params![uid, feed_id],
                    )?;
                }
            }
            collect_garbage(&tx)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// A trivial query, used by the systemd watchdog to see the database answers.
    pub fn ping(&self) -> Result<()> {
        self.conn.query_row("SELECT 1", [], |_| Ok(()))?;
        Ok(())
    }

    /// Feed URLs whose next fetch is due at `now`.
    pub fn due_targets(&self, now: i64, default_interval: u64) -> Result<Vec<FetchTarget>> {
        let default_interval = i64::try_from(default_interval)?;
        let mut stmt = self.conn.prepare(
            "SELECT src.url, src.etag, src.last_modified,
                    (SELECT MIN(COALESCE(refresh_interval, ?2)) FROM subscriptions
                     WHERE source = src.url AND archived = 0),
                    src.fail_count,
                    (SELECT MAX(full_text) FROM subscriptions
                     WHERE source = src.url AND archived = 0)
             FROM sources src
             WHERE src.next_fetch <= ?1
               AND EXISTS (SELECT 1 FROM subscriptions WHERE source = src.url AND archived = 0)
             ORDER BY src.next_fetch, src.url",
        )?;
        let rows = stmt.query_map(params![now, default_interval], |r| {
            Ok(FetchTarget {
                url: r.get(0)?,
                etag: r.get(1)?,
                last_modified: r.get(2)?,
                interval: r.get::<_, i64>(3)?.try_into().unwrap_or(1800),
                fail_count: r.get(4)?,
                full_text: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// When the next feed is due, if any feed is followed at all.
    pub fn next_due(&self) -> Result<Option<i64>> {
        Ok(self.conn.query_row(
            "SELECT MIN(next_fetch) FROM sources
             WHERE url IN (SELECT source FROM subscriptions WHERE archived = 0)",
            [],
            |r| r.get(0),
        )?)
    }

    /// Makes every followed feed due now (for `POST /api/v1/refresh`).
    pub fn force_refresh(&self) -> Result<()> {
        self.conn.execute(
            "UPDATE sources SET next_fetch = 0
             WHERE url IN (SELECT source FROM subscriptions WHERE archived = 0)",
            [],
        )?;
        Ok(())
    }

    /// Records a poll. On success the etag and last-modified are replaced;
    /// on failure they are kept so the next poll is still conditional.
    pub fn record_fetch(&self, url: &str, now: i64, result: &FetchResult) -> Result<()> {
        if result.error.is_some() {
            self.conn.execute(
                "UPDATE sources SET last_fetched = ?2, last_error = ?3, next_fetch = ?4,
                    fail_count = ?5
                 WHERE url = ?1",
                params![url, now, result.error, result.next_fetch, result.fail_count],
            )?;
        } else {
            self.conn.execute(
                "UPDATE sources SET last_fetched = ?2, last_error = NULL,
                    etag = ?3, last_modified = ?4,
                    title = COALESCE(?5, title), site_url = COALESCE(?6, site_url),
                    next_fetch = ?7, fail_count = 0,
                    image = CASE WHEN ?10 THEN image ELSE ?8 END,
                    medium = CASE WHEN ?10 THEN medium ELSE ?9 END
                 WHERE url = ?1",
                params![
                    url,
                    now,
                    result.etag,
                    result.last_modified,
                    result.title,
                    result.site_url,
                    result.next_fetch,
                    result.image,
                    result.medium,
                    result.not_modified,
                ],
            )?;
        }
        Ok(())
    }

    /// Inserts new items and refreshes the text of changed ones, keeping the
    /// search index in step. Unchanged items (most of them, on most fetches)
    /// are not written at all. Returns how many items were new.
    pub fn upsert_items(&mut self, source: &str, now: i64, items: &[NewItem]) -> Result<usize> {
        self.store_items(source, now, items.iter().map(|i| (i, "")))
    }

    /// The source digest of each stored item of a feed URL, by guid, to tell
    /// which fetched entries are unchanged before sanitizing them.
    pub fn source_digests(&self, source: &str) -> Result<HashMap<String, String>> {
        let mut stmt = self.conn.prepare(
            "SELECT guid, source_digest FROM items INDEXED BY items_lookup
             WHERE source = ?1 AND source_digest != ''",
        )?;
        let rows = stmt.query_map([source], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Like `upsert_items`, also recording each item's source digest (see
    /// `source_digests`).
    pub fn upsert_fetched(
        &mut self,
        source: &str,
        now: i64,
        items: &[(NewItem, String)],
    ) -> Result<usize> {
        self.store_items(source, now, items.iter().map(|(i, d)| (i, d.as_str())))
    }

    fn store_items<'a>(
        &mut self,
        source: &str,
        now: i64,
        items: impl Iterator<Item = (&'a NewItem, &'a str)>,
    ) -> Result<usize> {
        let tx = self.conn.transaction()?;
        let mut inserted = 0;
        {
            // Answered from the items_lookup index alone. SQLite would rather
            // use the unique (source, guid) index and then read the row, whose
            // later columns sit behind the large text in overflow pages.
            let mut existing = tx.prepare(
                "SELECT id, digest, published, source_digest FROM items INDEXED BY items_lookup
                 WHERE source = ?1 AND guid = ?2",
            )?;
            let mut insert = tx.prepare(
                "INSERT INTO items
                    (source, guid, title, url, author, summary, content, published, digest,
                     source_digest, media)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11) RETURNING id",
            )?;
            let mut update = tx.prepare(
                "UPDATE items SET title = ?2, url = ?3, author = ?4, summary = ?5, content = ?6,
                    published = ?7, digest = ?8, source_digest = ?9, media = ?10
                 WHERE id = ?1 RETURNING NULLIF(full_content, '')",
            )?;
            let mut set_source_digest =
                tx.prepare("UPDATE items SET source_digest = ?2 WHERE id = ?1")?;
            let mut unindex = tx.prepare(FTS_DELETE)?;
            let mut index = tx.prepare(FTS_INSERT)?;
            for (item, source_digest) in items {
                let stored: Option<(i64, String, i64, String)> = existing
                    .query_row(params![source, item.guid], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                    })
                    .optional()?;
                let digest = digest(item);
                let media = media_json(item.media.as_ref());
                let (id, article) = match stored {
                    None => {
                        inserted += 1;
                        let published = item.published.unwrap_or(now);
                        let row = (
                            source,
                            &item.guid,
                            &item.title,
                            &item.url,
                            &item.author,
                            &item.summary,
                            &item.content,
                            published,
                            &digest,
                            source_digest,
                            &media,
                        );
                        let id = insert.query_row(row, |r| r.get::<_, i64>(0))?;
                        (id, None)
                    }
                    Some((id, stored_digest, published, stored_source)) => {
                        // An undated item keeps the time it was first seen.
                        let new_published = item.published.unwrap_or(published);
                        if stored_digest == digest && published == new_published {
                            if !source_digest.is_empty() && stored_source != source_digest {
                                set_source_digest.execute(params![id, source_digest])?;
                            }
                            continue;
                        }
                        // The extracted article (if any) is kept, and stays
                        // what search sees.
                        let row = (
                            id,
                            &item.title,
                            &item.url,
                            &item.author,
                            &item.summary,
                            &item.content,
                            new_published,
                            &digest,
                            source_digest,
                            &media,
                        );
                        let article: Option<String> = update.query_row(row, |r| r.get(0))?;
                        unindex.execute([id])?;
                        (id, article)
                    }
                };
                let body = match article {
                    Some(article) => sanitize::text(&article),
                    None => search_body(item.summary.as_deref(), item.content.as_deref()),
                };
                index.execute(params![id, item.title, body])?;
            }
        }
        tx.commit()?;
        Ok(inserted)
    }

    /// Deletes the oldest items beyond `max` for a feed URL, and any
    /// published before `older_than`, except ones anybody starred or queued.
    pub fn prune(&self, source: &str, max: u32, older_than: Option<i64>) -> Result<usize> {
        Ok(self.conn.execute(
            "DELETE FROM items WHERE source = ?1
               AND id NOT IN (SELECT item_id FROM item_state WHERE starred = 1)
               AND id NOT IN (SELECT item_id FROM queue)
               AND (published < ?3
                    OR id NOT IN (SELECT id FROM items WHERE source = ?1
                                  ORDER BY published DESC, id DESC LIMIT ?2))",
            params![source, max, older_than.unwrap_or(i64::MIN)],
        )?)
    }

    /// The newest items of a feed URL whose full article hasn't been
    /// fetched yet: (id, page URL).
    pub fn full_text_pending(&self, source: &str, limit: u32) -> Result<Vec<(i64, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, url FROM items
             WHERE source = ?1 AND full_content IS NULL AND url IS NOT NULL
             ORDER BY published DESC, id DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![source, limit], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Stores an item's extracted article (already sanitized), or `None` if
    /// it couldn't be had, so it isn't tried again. The article replaces the
    /// feed's text in the search index.
    pub fn store_full_text(&mut self, id: i64, article: Option<&str>) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE items SET full_content = ?2 WHERE id = ?1",
            params![id, article.unwrap_or_default()],
        )?;
        if let Some(article) = article {
            let title: Option<String> = tx
                .query_row("SELECT title FROM items WHERE id = ?1", [id], |r| r.get(0))
                .optional()?;
            if let Some(title) = title {
                tx.execute(FTS_DELETE, [id])?;
                tx.execute(FTS_INSERT, params![id, title, sanitize::text(article)])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// A user's feeds with their unread counts.
    pub fn feeds(&self, user: i64) -> Result<Vec<Feed>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.feed_id, COALESCE(s.title, src.title, s.feed_id), src.url, src.site_url,
                    s.tags, COUNT(i.id) - COALESCE(SUM(st.read), 0), COUNT(i.id),
                    src.last_fetched, CASE WHEN s.archived = 0 THEN src.last_error END,
                    s.archived, src.image, src.medium, COUNT(i.media),
                    COUNT(i.media) - COALESCE(SUM(CASE WHEN i.media IS NOT NULL THEN st.read END), 0)
             FROM subscriptions s
             JOIN sources src ON src.url = s.source
             LEFT JOIN items i ON i.source = s.source
             LEFT JOIN item_state st ON st.item_id = i.id AND st.user_id = s.user_id
             WHERE s.user_id = ?1 AND (s.archived = 0 OR st.starred = 1 OR i.id IS NULL)
             GROUP BY s.feed_id
             ORDER BY s.archived, 2 COLLATE NOCASE",
        )?;
        let rows = stmt.query_map([user], |r| {
            let tags: String = r.get(4)?;
            Ok(Feed {
                id: r.get(0)?,
                title: r.get(1)?,
                url: r.get(2)?,
                site_url: r.get(3)?,
                tags: serde_json::from_str(&tags).unwrap_or_default(),
                unread: r.get(5)?,
                total: r.get(6)?,
                last_fetched: r.get(7)?,
                last_error: r.get(8)?,
                archived: r.get(9)?,
                image: r.get(10)?,
                medium: r.get(11)?,
                episodes: r.get(12)?,
                unread_episodes: r.get(13)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn items(&self, user: i64, query: &ItemQuery) -> Result<ItemPage> {
        let limit = query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
        let offset = query.offset.unwrap_or(0);
        let Filters {
            mut clauses,
            mut args,
            index,
        } = Filters::new(user, query, true);
        // A cursor continues from where the last page ended, walking the
        // index from there instead of counting past every earlier item.
        let cursor = query.cursor.as_deref().map(parse_cursor).transpose()?;
        let offset = if cursor.is_some() { 0 } else { offset };
        if let Some((published, id)) = cursor {
            clauses.push("(i.published, i.id) < (?, ?)");
            args.push(Box::new(published));
            args.push(Box::new(id));
        }
        // Fetch one extra row to learn whether there is another page.
        args.push(Box::new(limit + 1));
        args.push(Box::new(offset));
        let sql = format!(
            "SELECT {ITEM_COLUMNS}, NULL FROM items i {index} {USER_JOINS}
             WHERE {} ORDER BY i.published DESC, i.id DESC LIMIT ? OFFSET ?",
            clauses.join(" AND ")
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut items = stmt
            .query_map(params_from_iter(args.iter()), item_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        let more = items.len() > limit as usize;
        items.truncate(limit as usize);
        let next_cursor = more
            .then(|| items.last())
            .flatten()
            .map(|last| format!("{}.{}", last.published, last.id));
        let next_offset = (more && cursor.is_none()).then_some(offset + limit);
        Ok(ItemPage {
            items,
            next_cursor,
            next_offset,
        })
    }

    /// Marks read every unread item a list shows (its filters, not its
    /// paging), or, given `from`, that item and the ones after it in the
    /// list. Returns how many were unread; 0 if the user can't see `from`.
    pub fn mark_read(&self, user: i64, query: &ItemQuery, from: Option<i64>) -> Result<u32> {
        let Filters {
            mut clauses,
            mut args,
            index,
        } = Filters::new(user, query, false);
        if let Some(from) = from {
            let Some(published) = self.item(user, from)?.map(|i| i.published) else {
                return Ok(0);
            };
            // The list is newest first: "after" is older, or as old with a
            // lower id.
            clauses.push("(i.published, i.id) <= (?, ?)");
            args.push(Box::new(published));
            args.push(Box::new(from));
        }
        // Touches only the unread items, so the rows changed are the count.
        clauses.push("COALESCE(st.read, 0) = 0");
        let marked = self.conn.execute(
            &format!(
                "INSERT INTO item_state (user_id, item_id, read)
                 SELECT ?1, i.id, 1 FROM items i {index} {USER_JOINS}
                 WHERE {}
                 ON CONFLICT (user_id, item_id) DO UPDATE SET read = 1",
                clauses.join(" AND ")
            ),
            params_from_iter(args.iter()),
        )?;
        Ok(u32::try_from(marked)?)
    }

    /// One item with its content, if the user may see it.
    pub fn item(&self, user: i64, id: i64) -> Result<Option<Item>> {
        Ok(self
            .conn
            .prepare_cached(&format!(
                "SELECT {ITEM_COLUMNS}, COALESCE(NULLIF(i.full_content, ''), i.content)
                 FROM items i {USER_JOINS}
                 WHERE i.id = ?2 AND {VISIBLE}"
            ))?
            .query_row(params![user, id], item_from_row)
            .optional()?)
    }

    /// Sets `read` or `starred` for one item in one statement, which writes
    /// nothing (and returns false) if the user cannot see the item. Checking
    /// visibility with `item` first would read the item's whole text.
    fn set_flag(&self, user: i64, id: i64, column: Flag, value: bool) -> Result<bool> {
        let column = column.column();
        // The WHERE clause also keeps SQLite from reading `ON CONFLICT` as
        // part of the join.
        let changed = self
            .conn
            .prepare_cached(&format!(
                "INSERT INTO item_state (user_id, item_id, {column})
                 SELECT ?1, i.id, ?3 FROM items i {USER_JOINS}
                 WHERE i.id = ?2 AND {VISIBLE}
                 ON CONFLICT (user_id, item_id) DO UPDATE SET {column} = excluded.{column}"
            ))?
            .execute(params![user, id, value])?;
        Ok(changed > 0)
    }

    /// Returns false if the user cannot see that item. Marking an item read
    /// (finishing an episode, say) takes it out of the user's queue.
    pub fn set_read(&self, user: i64, id: i64, read: bool) -> Result<bool> {
        let changed = self.set_flag(user, id, Flag::Read, read)?;
        if changed && read {
            self.conn
                .prepare_cached("DELETE FROM queue WHERE user_id = ?1 AND item_id = ?2")?
                .execute(params![user, id])?;
        }
        Ok(changed)
    }

    /// Returns false if the user cannot see that item. Unstarring the last
    /// starred item of a removed feed drops the feed for that user.
    pub fn set_starred(&self, user: i64, id: i64, starred: bool) -> Result<bool> {
        let changed = self.set_flag(user, id, Flag::Starred, starred)?;
        if changed && !starred {
            collect_garbage(&self.conn)?;
        }
        Ok(changed)
    }

    /// Saves where the user stopped playing an item's media. Returns false
    /// if they cannot see the item.
    pub fn set_position(&self, user: i64, id: i64, position: u32) -> Result<bool> {
        let changed = self
            .conn
            .prepare_cached(&format!(
                "INSERT INTO item_state (user_id, item_id, position)
                 SELECT ?1, i.id, ?3 FROM items i {USER_JOINS}
                 WHERE i.id = ?2 AND {VISIBLE}
                 ON CONFLICT (user_id, item_id) DO UPDATE SET position = excluded.position"
            ))?
            .execute(params![user, id, position])?;
        Ok(changed > 0)
    }

    /// The user's Up Next queue, in order: the queued items they can still
    /// see, without content.
    pub fn queue(&self, user: i64) -> Result<Vec<Item>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {ITEM_COLUMNS}, NULL FROM queue q JOIN items i ON i.id = q.item_id
             {USER_JOINS}
             WHERE q.user_id = ?1 AND {VISIBLE}
             ORDER BY q.rank"
        ))?;
        let items = stmt
            .query_map(params![user], item_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(items)
    }

    /// Replaces the user's queue with `ids`, in that order, leaving out items
    /// they can't see, items without media and repeats. Returns the new queue.
    pub fn set_queue(&mut self, user: i64, ids: &[i64]) -> Result<Vec<Item>> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM queue WHERE user_id = ?1", params![user])?;
        {
            // `OR IGNORE` drops repeats: the first place an item is listed
            // wins.
            let mut insert = tx.prepare_cached(&format!(
                "INSERT OR IGNORE INTO queue (user_id, item_id, rank)
                 SELECT ?1, i.id, ?3 FROM items i {USER_JOINS}
                 WHERE i.id = ?2 AND i.media IS NOT NULL AND {VISIBLE}"
            ))?;
            for (rank, id) in ids.iter().enumerate() {
                insert.execute(params![user, id, i64::try_from(rank)?])?;
            }
        }
        tx.commit()?;
        self.queue(user)
    }

    /// An item's media, if the user can see the item. `Some(None)`: they
    /// can, and it has none.
    pub fn item_media(&self, user: i64, id: i64) -> Result<Option<Option<Media>>> {
        Ok(self.item(user, id)?.map(|i| i.media))
    }

    /// A chapters or transcript file read before, as the API's JSON, if it
    /// was fetched at `since` or later.
    pub fn media_file(&self, url: &str, since: i64) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT json FROM media_files WHERE url = ?1 AND fetched >= ?2",
                params![url, since],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Keeps a chapters or transcript file, and forgets ones fetched before
    /// `forget_before`, which nothing is likely to ask for again.
    pub fn store_media_file(
        &self,
        url: &str,
        json: &str,
        now: i64,
        forget_before: i64,
    ) -> Result<()> {
        self.conn.execute(
            "DELETE FROM media_files WHERE fetched < ?1",
            [forget_before],
        )?;
        self.conn.execute(
            "INSERT INTO media_files (url, json, fetched) VALUES (?1, ?2, ?3)
             ON CONFLICT (url) DO UPDATE SET json = excluded.json, fetched = excluded.fetched",
            params![url, json, now],
        )?;
        Ok(())
    }

    /// Marks every item of one of the user's feeds read. `None` if they have
    /// no such feed.
    pub fn mark_feed_read(&self, user: i64, feed_id: &str) -> Result<Option<u32>> {
        let source: Option<String> = self
            .conn
            .query_row(
                "SELECT source FROM subscriptions WHERE user_id = ?1 AND feed_id = ?2",
                params![user, feed_id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(source) = source else {
            return Ok(None);
        };
        // Touches only the unread items, so the rows changed are the count.
        let marked = self.conn.execute(
            "INSERT INTO item_state (user_id, item_id, read)
             SELECT ?1, i.id, 1 FROM items i
             LEFT JOIN item_state st ON st.item_id = i.id AND st.user_id = ?1
             WHERE i.source = ?2 AND COALESCE(st.read, 0) = 0
             ON CONFLICT (user_id, item_id) DO UPDATE SET read = 1",
            params![user, source],
        )?;
        Ok(Some(u32::try_from(marked)?))
    }

    pub fn counts(&self) -> Result<Counts> {
        Ok(self.conn.query_row(
            "SELECT
                (SELECT COUNT(DISTINCT source) FROM subscriptions WHERE archived = 0),
                (SELECT COUNT(*) FROM subscriptions WHERE archived = 1),
                (SELECT COUNT(*) FROM sources WHERE last_error IS NOT NULL
                   AND url IN (SELECT source FROM subscriptions WHERE archived = 0)),
                (SELECT COUNT(*) FROM items),
                (SELECT COUNT(*) FROM users),
                (SELECT COUNT(*) FROM devices)",
            [],
            |r| {
                Ok(Counts {
                    feeds: r.get(0)?,
                    archived_feeds: r.get(1)?,
                    failing_feeds: r.get(2)?,
                    items: r.get(3)?,
                    users: r.get(4)?,
                    devices: r.get(5)?,
                })
            },
        )?)
    }

    pub fn users(&self) -> Result<Vec<UserInfo>> {
        let mut stmt = self.conn.prepare(
            "SELECT u.name, u.admin,
                    (SELECT COUNT(*) FROM devices WHERE user_id = u.id),
                    (SELECT COUNT(*) FROM subscriptions WHERE user_id = u.id AND archived = 0)
             FROM users u ORDER BY u.name",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(UserInfo {
                name: r.get(0)?,
                admin: r.get(1)?,
                devices: r.get(2)?,
                feeds: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// A user by name.
    pub fn user(&self, name: &str) -> Result<Option<AuthUser>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, name, admin FROM users WHERE name = ?1",
                [name],
                user_from_row,
            )
            .optional()?)
    }

    /// Who the static token acts as: the first admin by name, else the
    /// first user.
    pub fn static_token_user(&self) -> Result<Option<AuthUser>> {
        Ok(self
            .conn
            .prepare_cached("SELECT id, name, admin FROM users ORDER BY admin DESC, name LIMIT 1")?
            .query_row([], user_from_row)
            .optional()?)
    }

    /// The only user, if there is exactly one.
    pub fn only_user(&self) -> Result<Option<AuthUser>> {
        let users: Vec<AuthUser> = self
            .conn
            .prepare("SELECT id, name, admin FROM users LIMIT 2")?
            .query_map([], user_from_row)?
            .collect::<Result<_, _>>()?;
        Ok(<[_; 1]>::try_from(users).ok().map(|[only]| only))
    }

    /// Registers a device for a user. If `name` is taken, `name-2`,
    /// `name-3`, ... is used; the name actually stored is returned.
    pub fn add_device(&self, user: i64, name: &str, token_hash: &str, now: i64) -> Result<String> {
        for n in 1..1000 {
            let candidate = if n == 1 {
                name.to_owned()
            } else {
                format!("{name}-{n}")
            };
            let inserted = self.conn.execute(
                "INSERT INTO devices (name, token_hash, created, user_id) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (name) DO NOTHING",
                params![candidate, token_hash, now, user],
            )?;
            if inserted == 1 {
                return Ok(candidate);
            }
        }
        bail!("too many devices named {name}");
    }

    /// Who a device token belongs to, and whether its `last_seen` is stale
    /// (older than a minute) and worth updating. Read-only, so it can run on
    /// the read pool for every request.
    pub fn find_device(&self, token_hash: &str, now: i64) -> Result<Option<(AuthUser, bool)>> {
        // Runs on every request, so the statement is kept prepared.
        Ok(self
            .conn
            .prepare_cached(
                "SELECT u.id, u.name, u.admin, d.name, d.last_seen FROM devices d
                 JOIN users u ON u.id = d.user_id WHERE d.token_hash = ?1",
            )?
            .query_row([token_hash], |r| {
                let last_seen: Option<i64> = r.get(4)?;
                let user = AuthUser {
                    device: Some(r.get(3)?),
                    ..user_from_row(r)?
                };
                Ok((user, last_seen.is_none_or(|seen| seen < now - 60)))
            })
            .optional()?)
    }

    /// Records that a device was seen.
    pub fn touch_device(&self, token_hash: &str, now: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE devices SET last_seen = ?2 WHERE token_hash = ?1",
            params![token_hash, now],
        )?;
        Ok(())
    }

    /// [`Self::find_device`] and, when stale, [`Self::touch_device`] on one
    /// connection.
    pub fn authenticate(&self, token_hash: &str, now: i64) -> Result<Option<AuthUser>> {
        let found = self.find_device(token_hash, now)?;
        if let Some((_, true)) = &found {
            self.touch_device(token_hash, now)?;
        }
        Ok(found.map(|(user, _)| user))
    }

    pub fn devices(&self) -> Result<Vec<Device>> {
        let mut stmt = self.conn.prepare(
            "SELECT d.name, u.name, d.created, d.last_seen FROM devices d
             JOIN users u ON u.id = d.user_id ORDER BY u.name, d.name",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Device {
                name: r.get(0)?,
                user: r.get(1)?,
                created: r.get(2)?,
                last_seen: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Returns false if there is no such device.
    pub fn remove_device(&self, name: &str) -> Result<bool> {
        Ok(self
            .conn
            .execute("DELETE FROM devices WHERE name = ?1", [name])?
            > 0)
    }

    /// Runs SQL on the connection, for tests that need a broken database
    /// (a dropped table, a trigger that fails writes) to reach error paths.
    #[cfg(test)]
    pub fn break_with(&self, sql: &str) {
        self.conn.execute_batch(sql).unwrap();
    }

    /// Writes a consistent copy of the database to `path`.
    pub fn backup_to(&self, path: &Path) -> Result<()> {
        let path = path.to_str().context("backup path is not valid UTF-8")?;
        self.conn.execute("VACUUM INTO ?1", [path])?;
        Ok(())
    }
}

/// A fingerprint of an item's text, to skip rewriting unchanged items.
fn digest(item: &NewItem) -> String {
    let media = media_json(item.media.as_ref());
    let mut fields = vec![
        Some(item.title.as_str()),
        item.url.as_deref(),
        item.author.as_deref(),
        item.summary.as_deref(),
        item.content.as_deref(),
    ];
    // Only when there is some, so items without media keep the digests
    // stored before pressbox knew about media.
    if media.is_some() {
        fields.push(media.as_deref());
    }
    hash_fields(&fields)
}

/// How an item's media is stored: the API's JSON.
pub fn media_json(media: Option<&Media>) -> Option<String> {
    // Serializing plain structs of strings and numbers can't fail.
    media.map(|m| serde_json::to_string(m).unwrap_or_default())
}

/// A short hex digest of a list of optional fields.
pub fn hash_fields(parts: &[Option<&str>]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for part in parts {
        // A marker per field keeps ("ab", None) and ("a", "b") apart.
        match part {
            Some(text) => {
                hasher.update([1]);
                hasher.update(text.len().to_le_bytes());
                hasher.update(text.as_bytes());
            }
            None => hasher.update([0]),
        }
    }
    hex(&hasher.finalize()[..16])
}

/// Lowercase hex of some bytes.
pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(char::from(DIGITS[usize::from(b >> 4)]));
        s.push(char::from(DIGITS[usize::from(b & 0xf)]));
    }
    s
}

/// A `next_cursor` from `items`: the (published, id) of the last item sent.
fn parse_cursor(cursor: &str) -> Result<(i64, i64)> {
    cursor
        .rsplit_once('.')
        .and_then(|(p, i)| Some((p.parse().ok()?, i.parse().ok()?)))
        .ok_or_else(|| anyhow::Error::new(BadCursor))
}

/// A `cursor` that isn't one `items` gave out.
#[derive(Debug)]
pub struct BadCursor;

impl std::fmt::Display for BadCursor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("invalid cursor")
    }
}

impl std::error::Error for BadCursor {}

/// Text indexed for search: the content without markup, or the summary
/// when there is no content (a summary is usually the start of the content,
/// so indexing both doubles the index for nothing).
fn search_body(summary: Option<&str>, content: Option<&str>) -> String {
    content
        .filter(|c| !c.trim().is_empty())
        .or(summary)
        .map(sanitize::text)
        .unwrap_or_default()
}

/// Turns user input into an FTS5 query: every word must appear, as a prefix.
/// Quoting each word means punctuation and FTS5 syntax are matched literally
/// instead of causing errors.
fn fts_query(input: &str) -> Option<String> {
    let words: Vec<String> = input
        .split_whitespace()
        .map(|w| w.replace('"', ""))
        .filter(|w| !w.is_empty())
        .map(|w| format!("\"{w}\"*"))
        .collect();
    (!words.is_empty()).then(|| words.join(" "))
}

/// SQL conditions for an item query's filters (not its paging), joined
/// with `AND` after `items i {index} {USER_JOINS}`. `args` starts with the
/// user's id, for `?1`; the rest follow the clauses' `?`s in order.
struct Filters {
    clauses: Vec<&'static str>,
    args: Vec<Box<dyn ToSql>>,
    /// An index hint for `items i`, or "".
    index: &'static str,
}

impl Filters {
    /// `paged`: the query reads one page, newest first, rather than every
    /// match, which changes the best plan for a search.
    fn new(user: i64, query: &ItemQuery, paged: bool) -> Self {
        let mut clauses = vec![VISIBLE];
        let mut args: Vec<Box<dyn ToSql>> = vec![Box::new(user)];
        let mut index = "";
        if let Some(feed) = &query.feed {
            clauses.push("s.feed_id = ?");
            args.push(Box::new(feed.clone()));
        }
        if let Some(tag) = &query.tag {
            clauses.push("EXISTS (SELECT 1 FROM json_each(s.tags) WHERE value = ?)");
            args.push(Box::new(tag.clone()));
        }
        if let Some(unread) = query.unread {
            clauses.push("COALESCE(st.read, 0) = ?");
            args.push(Box::new(!unread));
        }
        match query.starred {
            // Start from the user's stars instead of scanning every item.
            Some(true) => clauses.push(
                "i.id IN (SELECT item_id FROM item_state WHERE user_id = ?1 AND starred = 1)",
            ),
            Some(false) => clauses.push("COALESCE(st.starred, 0) = 0"),
            None => {}
        }
        match query.media {
            Some(true) => clauses.push("i.media IS NOT NULL"),
            Some(false) => clauses.push("i.media IS NULL"),
            None => {}
        }
        match query.in_progress {
            // Start from the user's positions instead of scanning every item.
            Some(true) => clauses.push(
                "i.id IN (SELECT item_id FROM item_state
                          WHERE user_id = ?1 AND position > 0 AND read = 0)",
            ),
            Some(false) => {
                clauses.push("(COALESCE(st.position, 0) = 0 OR COALESCE(st.read, 0) = 1)");
            }
            None => {}
        }
        if let Some(fts) = query.q.as_deref().and_then(fts_query) {
            if paged && query.feed.is_none() && query.starred != Some(true) {
                // Walk items newest first and stop after a page, checking
                // each against the matches, rather than sorting every match:
                // a common word matches most items (300ms -> 16ms on 100k
                // items), and no match at all costs one index scan (~5ms).
                // A feed or starred filter has a narrower index to start from.
                index = "INDEXED BY items_by_date";
                clauses.push("+i.id IN (SELECT rowid FROM items_fts WHERE items_fts MATCH ?)");
            } else {
                clauses.push("i.id IN (SELECT rowid FROM items_fts WHERE items_fts MATCH ?)");
            }
            args.push(Box::new(fts));
        }
        Self {
            clauses,
            args,
            index,
        }
    }
}

/// Item columns as one user sees them, from `items i` with [`USER_JOINS`].
const ITEM_COLUMNS: &str =
    "i.id, COALESCE(s.feed_id, i.source), i.title, i.url, i.author, i.summary,
     i.published, COALESCE(st.read, 0), COALESCE(st.starred, 0), i.media,
     COALESCE(st.position, 0)";

/// Follows `items i`: joins each item with one user's subscription (`s`)
/// and state (`st`). Needs `?1` bound to the user's id.
const USER_JOINS: &str = "
     LEFT JOIN subscriptions s ON s.source = i.source AND s.user_id = ?1
     LEFT JOIN item_state st ON st.item_id = i.id AND st.user_id = ?1";

/// Adds an item's text to the search index: (rowid, title, body).
const FTS_INSERT: &str = "INSERT INTO items_fts (rowid, title, body) VALUES (?1, ?2, ?3)";
/// Removes an item from the search index before it is indexed again.
const FTS_DELETE: &str = "DELETE FROM items_fts WHERE rowid = ?1";

/// The per-user flags on an item.
#[derive(Debug, Clone, Copy)]
enum Flag {
    Read,
    Starred,
}

impl Flag {
    /// Its `item_state` column. Only these fixed names are ever put in SQL.
    const fn column(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Starred => "starred",
        }
    }
}

/// A user sees items of the feeds they follow, of removed feeds only their
/// starred ones, and every item they starred.
const VISIBLE: &str =
    "((s.feed_id IS NOT NULL AND (s.archived = 0 OR st.starred = 1)) OR st.starred = 1)";

/// Removes what nobody needs any more: removed feeds a user has no starred
/// items in, items no followed feed has and nobody starred, and sources
/// with neither subscribers nor items.
fn collect_garbage(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "DELETE FROM subscriptions WHERE archived = 1 AND NOT EXISTS (
             SELECT 1 FROM items i JOIN item_state st ON st.item_id = i.id
             WHERE i.source = subscriptions.source AND st.user_id = subscriptions.user_id
               AND st.starred = 1);
         DELETE FROM items
         WHERE source NOT IN (SELECT source FROM subscriptions WHERE archived = 0)
           AND id NOT IN (SELECT item_id FROM item_state WHERE starred = 1);
         DELETE FROM sources
         WHERE url NOT IN (SELECT source FROM subscriptions)
           AND NOT EXISTS (SELECT 1 FROM items WHERE source = sources.url);",
    )?;
    Ok(())
}

/// An [`AuthUser`] from `id, name, admin` columns, without a device.
fn user_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<AuthUser> {
    Ok(AuthUser {
        id: r.get(0)?,
        name: r.get(1)?,
        admin: r.get(2)?,
        device: None,
    })
}

fn item_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Item> {
    Ok(Item {
        id: r.get(0)?,
        feed_id: r.get(1)?,
        title: r.get(2)?,
        url: r.get(3)?,
        author: r.get(4)?,
        summary: r.get(5)?,
        published: r.get(6)?,
        read: r.get(7)?,
        starred: r.get(8)?,
        // Written by `media_json`; unreadable JSON (from a newer pressboxd)
        // is left out rather than failing the list.
        media: r
            .get::<_, Option<String>>(9)?
            .and_then(|m| serde_json::from_str(&m).ok()),
        position: r.get(10)?,
        content: r.get(11)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(text: &str) -> Config {
        Config::parse(text).unwrap()
    }

    fn item(guid: &str, published: Option<i64>) -> NewItem {
        NewItem {
            guid: guid.into(),
            title: format!("title {guid}"),
            url: Some(format!("https://example.com/{guid}")),
            author: None,
            summary: Some("<p>summary</p>".into()),
            content: Some("<p>content</p>".into()),
            published,
            media: None,
        }
    }

    #[test]
    fn pages_by_cursor() {
        let mut db = Db::open_in_memory().unwrap();
        db.sync(&config(&format!("[feeds.a]\nurl = \"{A}\"")))
            .unwrap();
        // Several items share each time, so the id has to break ties.
        let items: Vec<_> = (0..25).map(|n| item(&n.to_string(), Some(n / 3))).collect();
        db.upsert_items(A, 0, &items).unwrap();
        let user = db.only_user().unwrap().unwrap().id;
        let all: Vec<i64> = db
            .items(
                user,
                &ItemQuery {
                    limit: Some(100),
                    ..ItemQuery::default()
                },
            )
            .unwrap()
            .items
            .iter()
            .map(|i| i.id)
            .collect();
        assert_eq!(all.len(), 25);

        let (mut seen, mut cursor, mut pages) = (Vec::new(), None, 0);
        loop {
            let page = db
                .items(
                    user,
                    &ItemQuery {
                        limit: Some(4),
                        // Ignored when there is a cursor.
                        offset: cursor.is_some().then_some(3),
                        cursor,
                        ..ItemQuery::default()
                    },
                )
                .unwrap();
            pages += 1;
            if pages > 1 {
                assert_eq!(page.next_offset, None, "cursor pages give cursors");
            }
            seen.extend(page.items.iter().map(|i| i.id));
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        assert_eq!(seen, all, "every item once, in order");
        assert_eq!(pages, 7);

        let first = db
            .items(
                user,
                &ItemQuery {
                    limit: Some(4),
                    ..ItemQuery::default()
                },
            )
            .unwrap();
        assert_eq!(first.next_offset, Some(4), "offset paging still works");
        assert!(first.next_cursor.is_some());

        for bad in ["", "x", "1", "1.x", "a.2"] {
            let err = db
                .items(
                    user,
                    &ItemQuery {
                        cursor: Some(bad.into()),
                        ..ItemQuery::default()
                    },
                )
                .unwrap_err();
            assert!(err.is::<BadCursor>(), "{bad:?}: {err:#}");
        }
    }

    #[test]
    fn marks_a_list_read_from_an_item_down() {
        let (mut db, chris, anna) = two_users();
        // a1..a5, newest first is a5; a2 and a3 share a time, so the id
        // decides their order.
        let items: Vec<_> = [1, 2, 2, 4, 5]
            .iter()
            .enumerate()
            .map(|(n, &t)| item(&format!("a{}", n + 1), Some(t)))
            .collect();
        db.upsert_items(A, 0, &items).unwrap();
        db.upsert_items(B, 0, &[item("b1", Some(3))]).unwrap();
        db.upsert_items(C, 0, &[item("c1", Some(9))]).unwrap();
        let list = |user, query: &ItemQuery| -> Vec<String> {
            titles(&db.items(user, query).unwrap())
                .into_iter()
                .map(|t| t.trim_start_matches("title ").to_owned())
                .collect()
        };
        let feed_a = ItemQuery {
            feed: Some("a".into()),
            ..q()
        };
        assert_eq!(list(chris, &feed_a), ["a5", "a4", "a3", "a2", "a1"]);
        let id = |title: &str| {
            let page = db.items(chris, &q()).unwrap();
            page.items
                .iter()
                .find(|i| i.title == format!("title {title}"))
                .unwrap()
                .id
        };

        // From a3 down, in feed a only: a3 and the older a2 (same time,
        // lower id) and a1. Already-read items don't count.
        db.set_read(chris, id("a1"), true).unwrap();
        assert_eq!(db.mark_read(chris, &feed_a, Some(id("a3"))).unwrap(), 2);
        let unread = ItemQuery {
            unread: Some(true),
            ..q()
        };
        assert_eq!(
            list(chris, &unread),
            ["a5", "a4", "b1"],
            "b1 is another feed"
        );
        assert_eq!(list(anna, &unread).len(), 7, "anna's state is her own");

        // Every list filter applies; search too.
        let search = ItemQuery {
            q: Some("b1".into()),
            ..q()
        };
        assert_eq!(db.mark_read(chris, &search, None).unwrap(), 1);
        assert_eq!(list(chris, &unread), ["a5", "a4"]);

        // An item the user can't see marks nothing.
        let c1 = db.items(anna, &q()).unwrap().items[0].id;
        assert_eq!(db.mark_read(chris, &q(), Some(c1)).unwrap(), 0);
        assert_eq!(list(chris, &unread), ["a5", "a4"]);

        // No `from`: the whole list, however many pages.
        assert_eq!(db.mark_read(chris, &q(), None).unwrap(), 2);
        assert!(list(chris, &unread).is_empty());
        assert!(db.feeds(chris).unwrap().iter().all(|f| f.unread == 0));
    }

    #[test]
    fn source_digests_are_kept() {
        let mut db = Db::open_in_memory().unwrap();
        db.sync(&config(&format!("[feeds.a]\nurl = \"{A}\"")))
            .unwrap();
        let one = (item("1", Some(1)), "d1".to_owned());
        db.upsert_fetched(A, 0, std::slice::from_ref(&one)).unwrap();
        assert_eq!(db.source_digests(A).unwrap()["1"], "d1");

        // Same text, new source digest (say, after an upgrade): recorded
        // without rewriting the item.
        let again = (one.0.clone(), "d2".to_owned());
        assert_eq!(db.upsert_fetched(A, 0, &[again]).unwrap(), 0);
        assert_eq!(db.source_digests(A).unwrap()["1"], "d2");

        // Items stored without one aren't listed, so they get sanitized.
        db.upsert_items(A, 0, &[item("2", Some(2))]).unwrap();
        assert!(!db.source_digests(A).unwrap().contains_key("2"));
    }

    const A: &str = "https://a.example/feed";
    const B: &str = "https://b.example/feed";
    const C: &str = "https://c.example/feed";

    /// chris (admin) and anna share feed `a`; chris also has `b`, anna `c`.
    const TWO_USERS: &str = r#"
        [feeds.a]
        url = "https://a.example/feed"
        tags = ["tech"]
        [users.chris]
        admin = true
        [users.chris.feeds.b]
        url = "https://b.example/feed"
        title = "Bee"
        refresh_interval = 600
        [users.anna]
        [users.anna.feeds.c]
        url = "https://c.example/feed"
        [users.anna.feeds.also-b]
        url = "https://b.example/feed"
        refresh_interval = 3600
    "#;

    fn two_users() -> (Db, i64, i64) {
        let mut db = Db::open_in_memory().unwrap();
        db.sync(&config(TWO_USERS)).unwrap();
        let chris = db.user("chris").unwrap().unwrap().id;
        let anna = db.user("anna").unwrap().unwrap().id;
        (db, chris, anna)
    }

    fn q() -> ItemQuery {
        ItemQuery::default()
    }

    fn titles(page: &ItemPage) -> Vec<String> {
        page.items.iter().map(|i| i.title.clone()).collect()
    }

    #[test]
    fn single_user_by_default() {
        let mut db = Db::open_in_memory().unwrap();
        db.sync(&config("[feeds.a]\nurl = \"https://a.example/feed\""))
            .unwrap();
        let users = db.users().unwrap();
        assert_eq!(users.len(), 1);
        assert_eq!((users[0].name.as_str(), users[0].admin), ("default", true));
        let only = db.only_user().unwrap().unwrap();
        assert_eq!(db.feeds(only.id).unwrap()[0].id, "a");
        assert_eq!(db.static_token_user().unwrap().unwrap().name, "default");
    }

    #[test]
    fn users_see_only_their_feeds_and_state() {
        let (mut db, chris, anna) = two_users();
        db.upsert_items(A, 0, &[item("a1", Some(3)), item("a2", Some(2))])
            .unwrap();
        db.upsert_items(B, 0, &[item("b1", Some(1))]).unwrap();
        db.upsert_items(C, 0, &[item("c1", Some(4))]).unwrap();

        let ids = |u| {
            db.feeds(u)
                .unwrap()
                .into_iter()
                .map(|f| f.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(chris), ["a", "b"]);
        assert_eq!(ids(anna), ["a", "also-b", "c"]);
        assert_eq!(
            titles(&db.items(chris, &q()).unwrap()),
            ["title a1", "title a2", "title b1"]
        );
        let anna_items = db.items(anna, &q()).unwrap();
        assert_eq!(
            titles(&anna_items),
            ["title c1", "title a1", "title a2", "title b1"]
        );
        assert_eq!(
            anna_items.items[3].feed_id, "also-b",
            "each user's own feed id"
        );

        // anna's private feed is invisible to chris, even by id.
        let c1 = anna_items.items[0].id;
        assert!(db.item(chris, c1).unwrap().is_none());
        assert!(!db.set_read(chris, c1, true).unwrap());
        assert!(!db.set_starred(chris, c1, true).unwrap());

        // Read state is per user.
        let a1 = anna_items.items[1].id;
        db.set_read(chris, a1, true).unwrap();
        assert!(db.item(chris, a1).unwrap().unwrap().read);
        assert!(!db.item(anna, a1).unwrap().unwrap().read);
        let unread = |u| {
            db.feeds(u)
                .unwrap()
                .into_iter()
                .find(|f| f.id == "a")
                .unwrap()
                .unread
        };
        assert_eq!((unread(chris), unread(anna)), (1, 2));
        assert_eq!(db.mark_feed_read(anna, "a").unwrap(), Some(2));
        assert_eq!(
            db.mark_feed_read(anna, "b").unwrap(),
            None,
            "not anna's feed id"
        );
        assert_eq!((unread(chris), unread(anna)), (1, 0));
        // Only what was still unread counts: chris had read one of two.
        assert_eq!(db.mark_feed_read(chris, "a").unwrap(), Some(1));
        assert_eq!(db.mark_feed_read(chris, "a").unwrap(), Some(0));
        // Marking read keeps a star.
        db.set_starred(anna, a1, true).unwrap();
        db.set_read(anna, a1, false).unwrap();
        assert_eq!(db.mark_feed_read(anna, "a").unwrap(), Some(1));
        let starred = db.item(anna, a1).unwrap().unwrap();
        assert!(starred.read && starred.starred);
        db.set_starred(anna, a1, false).unwrap();
        db.set_read(chris, a1, false).unwrap();

        // Filters work per user.
        let unread_a = ItemQuery {
            feed: Some("a".into()),
            unread: Some(true),
            ..q()
        };
        assert_eq!(db.items(chris, &unread_a).unwrap().items.len(), 1);
        assert_eq!(db.items(anna, &unread_a).unwrap().items.len(), 0);
        let tagged = ItemQuery {
            tag: Some("tech".into()),
            ..q()
        };
        assert_eq!(db.items(anna, &tagged).unwrap().items.len(), 2);
        let search = ItemQuery {
            q: Some("c1".into()),
            ..q()
        };
        assert_eq!(db.items(anna, &search).unwrap().items.len(), 1);
        assert!(
            db.items(chris, &search).unwrap().items.is_empty(),
            "search respects visibility"
        );
    }

    #[test]
    fn a_url_is_fetched_once_at_the_shortest_interval() {
        let (db, _, _) = two_users();
        let due = db.due_targets(0, 1800).unwrap();
        let urls: Vec<&str> = due.iter().map(|t| t.url.as_str()).collect();
        assert_eq!(urls, [A, B, C]);
        let b = due.iter().find(|t| t.url == B).unwrap();
        assert_eq!(b.interval, 600, "chris asked for 600, anna 3600");
        assert_eq!(due.iter().find(|t| t.url == A).unwrap().interval, 1800);
        assert_eq!(db.counts().unwrap().feeds, 3);
    }

    #[test]
    fn removed_feeds_are_archived_per_user() {
        let (mut db, chris, anna) = two_users();
        db.upsert_items(A, 0, &[item("a1", Some(2)), item("a2", Some(1))])
            .unwrap();
        let a1 = db.items(chris, &q()).unwrap().items[0].id;
        db.set_starred(anna, a1, true).unwrap();
        db.record_fetch(
            A,
            1,
            &FetchResult {
                etag: Some("\"e\"".into()),
                next_fetch: 100,
                ..Default::default()
            },
        )
        .unwrap();

        // Feed `a` stops being shared: chris keeps it as his own, anna drops it.
        let without_a = TWO_USERS.replace(
            "[feeds.a]\n        url = \"https://a.example/feed\"\n        tags = [\"tech\"]\n",
            "",
        ) + "\n[users.chris.feeds.a]\nurl = \"https://a.example/feed\"\n";
        db.sync(&config(&without_a)).unwrap();
        let anna_a = db
            .feeds(anna)
            .unwrap()
            .into_iter()
            .find(|f| f.id == "a")
            .unwrap();
        assert!(anna_a.archived);
        assert_eq!(anna_a.total, 1, "only her starred item");
        assert_eq!(
            titles(
                &db.items(
                    anna,
                    &ItemQuery {
                        feed: Some("a".into()),
                        ..q()
                    }
                )
                .unwrap()
            ),
            ["title a1"]
        );
        assert_eq!(
            db.items(
                chris,
                &ItemQuery {
                    feed: Some("a".into()),
                    ..q()
                }
            )
            .unwrap()
            .items
            .len(),
            2
        );
        let due = db.due_targets(100, 60).unwrap();
        assert_eq!(
            due.iter().find(|t| t.url == A).unwrap().etag.as_deref(),
            Some("\"e\""),
            "still followed by chris"
        );

        // Unstarring her last item drops the feed for anna only.
        db.set_starred(anna, a1, false).unwrap();
        assert!(db.feeds(anna).unwrap().iter().all(|f| f.id != "a"));
        assert_eq!(
            db.items(chris, &q())
                .unwrap()
                .items
                .iter()
                .filter(|i| i.feed_id == "a")
                .count(),
            2
        );
    }

    #[test]
    fn unfollowed_urls_are_cleaned_up_and_refetched_fully_when_followed_again() {
        let mut db = Db::open_in_memory().unwrap();
        let one = "[feeds.a]\nurl = \"https://a.example/feed\"";
        db.sync(&config(one)).unwrap();
        let user = db.only_user().unwrap().unwrap().id;
        db.upsert_items(A, 0, &[item("1", Some(1)), item("2", Some(2))])
            .unwrap();
        let starred = db.items(user, &q()).unwrap().items[0].id;
        db.set_starred(user, starred, true).unwrap();
        db.record_fetch(
            A,
            1,
            &FetchResult {
                etag: Some("\"e\"".into()),
                last_modified: Some("yesterday".into()),
                next_fetch: 100,
                ..Default::default()
            },
        )
        .unwrap();

        db.sync(&config("")).unwrap();
        let feeds = db.feeds(user).unwrap();
        assert!(feeds[0].archived);
        assert_eq!(db.counts().unwrap().items, 1, "unstarred item deleted");
        assert!(
            db.due_targets(i64::MAX, 60).unwrap().is_empty(),
            "archived feeds are not fetched"
        );

        db.sync(&config(one)).unwrap();
        let target = &db.due_targets(0, 60).unwrap()[0];
        assert_eq!(
            (target.etag.clone(), target.last_modified.clone()),
            (None, None)
        );
        assert!(!db.feeds(user).unwrap()[0].archived);

        db.sync(&config("")).unwrap();
        db.set_starred(user, starred, false).unwrap();
        assert!(db.feeds(user).unwrap().is_empty());
        assert_eq!(db.counts().unwrap().items, 0);
        let sources: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM sources", [], |r| r.get(0))
            .unwrap();
        assert_eq!(sources, 0, "unused URLs are forgotten");
    }

    #[test]
    fn removing_a_user_removes_their_devices_and_state() {
        let (mut db, chris, anna) = two_users();
        db.upsert_items(A, 0, &[item("a1", Some(1))]).unwrap();
        let a1 = db.items(anna, &q()).unwrap().items[0].id;
        db.set_starred(anna, a1, true).unwrap();
        db.add_device(anna, "phone", "h1", 0).unwrap();
        db.add_device(chris, "laptop", "h2", 0).unwrap();

        let only_chris = TWO_USERS[..TWO_USERS.find("[users.anna]").unwrap()].to_owned();
        db.sync(&config(&only_chris)).unwrap();
        assert!(db.user("anna").unwrap().is_none());
        assert_eq!(db.authenticate("h1", 1).unwrap(), None);
        assert_eq!(db.devices().unwrap().len(), 1);
        let states: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM item_state", [], |r| r.get(0))
            .unwrap();
        assert_eq!(states, 0);
        assert_eq!(db.feeds(chris).unwrap().len(), 2);
    }

    #[test]
    fn devices_belong_to_users() {
        let (db, chris, anna) = two_users();
        assert_eq!(db.add_device(chris, "phone", "h1", 1).unwrap(), "phone");
        assert_eq!(
            db.add_device(anna, "phone", "h2", 2).unwrap(),
            "phone-2",
            "names are global"
        );
        let who = db.authenticate("h2", 100).unwrap().unwrap();
        assert_eq!(
            (who.name.as_str(), who.admin, who.device.as_deref()),
            ("anna", false, Some("phone-2"))
        );
        assert!(db.authenticate("h1", 100).unwrap().unwrap().admin);
        assert_eq!(db.authenticate("nope", 100).unwrap(), None);
        db.authenticate("h2", 130).unwrap();
        let devices = db.devices().unwrap();
        assert_eq!(
            (devices[0].user.as_str(), devices[0].last_seen),
            ("anna", Some(100))
        );
        assert!(db.remove_device("phone").unwrap());
        assert!(!db.remove_device("phone").unwrap());
        assert_eq!(db.static_token_user().unwrap().unwrap().name, "chris");
        assert!(db.only_user().unwrap().is_none());
        let users = db.users().unwrap();
        assert_eq!(
            (users[0].name.as_str(), users[0].devices, users[0].feeds),
            ("anna", 1, 3)
        );
        assert_eq!(db.counts().unwrap().users, 2);
    }

    #[test]
    fn upsert_and_prune() {
        let (mut db, chris, anna) = two_users();
        assert_eq!(
            db.upsert_items(A, 100, &[item("1", Some(10)), item("2", None)])
                .unwrap(),
            2
        );
        let page = db.items(chris, &q()).unwrap();
        assert_eq!(page.items[0].published, 100, "undated item uses first-seen");
        let mut renamed = item("1", Some(10));
        renamed.title = "renamed".into();
        assert_eq!(db.upsert_items(A, 200, &[renamed]).unwrap(), 0);
        assert_eq!(
            db.items(
                chris,
                &ItemQuery {
                    q: Some("renamed".into()),
                    ..q()
                }
            )
            .unwrap()
            .items
            .len(),
            1
        );
        assert_eq!(page.items[0].content, None, "lists leave out content");
        assert_eq!(
            db.item(chris, page.items[1].id)
                .unwrap()
                .unwrap()
                .content
                .as_deref(),
            Some("<p>content</p>")
        );

        let many: Vec<_> = (0..10)
            .map(|n| item(&format!("m{n}"), Some(1000 + n)))
            .collect();
        db.upsert_items(A, 0, &many).unwrap();
        let oldest = db.items(chris, &q()).unwrap().items.last().unwrap().id;
        db.set_starred(anna, oldest, true).unwrap();
        db.prune(A, 3, None).unwrap();
        let left = db.items(chris, &q()).unwrap().items;
        assert_eq!(left.len(), 4, "3 newest + one anna starred");
        assert!(left.iter().any(|i| i.id == oldest));
    }

    #[test]
    fn full_articles() {
        let mut db = Db::open_in_memory().unwrap();
        db.sync(&config(&format!(
            "[feeds.a]\nurl = \"{A}\"\nfull_text = true\n[feeds.b]\nurl = \"{B}\""
        )))
        .unwrap();
        let user = db.only_user().unwrap().unwrap().id;
        let due = db.due_targets(i64::MAX, 1800).unwrap();
        let wants = |url| due.iter().find(|t| t.url == url).unwrap().full_text;
        assert!(wants(A) && !wants(B));

        db.upsert_items(A, 0, &[item("1", Some(10)), item("2", Some(20))])
            .unwrap();
        let pending = db.full_text_pending(A, 10).unwrap();
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].1, "https://example.com/2", "newest first");
        let (one, two) = (pending[1].0, pending[0].0);
        db.store_full_text(one, Some("<p>the whole zeppelin story</p>"))
            .unwrap();
        db.store_full_text(two, None).unwrap();
        assert!(
            db.full_text_pending(A, 10).unwrap().is_empty(),
            "failures aren't retried"
        );

        let content = |db: &Db, id| db.item(user, id).unwrap().unwrap().content;
        assert_eq!(
            content(&db, one).as_deref(),
            Some("<p>the whole zeppelin story</p>")
        );
        assert_eq!(
            content(&db, two).as_deref(),
            Some("<p>content</p>"),
            "fell back"
        );

        // An edited entry keeps its article, and search still finds it.
        let mut edited = item("1", Some(10));
        edited.content = Some("<p>edited</p>".into());
        db.upsert_items(A, 0, &[edited]).unwrap();
        assert_eq!(
            content(&db, one).as_deref(),
            Some("<p>the whole zeppelin story</p>")
        );
        let found = db
            .items(
                user,
                &ItemQuery {
                    q: Some("zeppelin".into()),
                    ..q()
                },
            )
            .unwrap();
        assert_eq!(found.items.len(), 1);
    }

    #[test]
    fn prune_by_age() {
        let (mut db, chris, anna) = two_users();
        let items: Vec<_> = (0..6)
            .map(|n| item(&format!("a{n}"), Some(n * 100)))
            .collect();
        db.upsert_items(A, 0, &items).unwrap();
        let page = db.items(chris, &q()).unwrap().items;
        let old_starred = page.last().unwrap().id; // published 0
        db.set_starred(anna, old_starred, true).unwrap();
        // Older than 300 goes, unless starred; the count limit still applies.
        assert_eq!(db.prune(A, 500, Some(300)).unwrap(), 2);
        let left: Vec<_> = db
            .items(chris, &q())
            .unwrap()
            .items
            .iter()
            .map(|i| i.published)
            .collect();
        assert_eq!(left, [500, 400, 300, 0]);
        assert_eq!(db.prune(A, 2, Some(300)).unwrap(), 1);
        assert_eq!(db.items(chris, &q()).unwrap().items.len(), 3);
    }

    #[test]
    fn feeds_count_episodes_apart() {
        let (mut db, chris, anna) = two_users();
        let mut items: Vec<_> = (0..3)
            .map(|n| NewItem {
                media: Some(Media::default()),
                ..item(&format!("e{n}"), Some(n * 100))
            })
            .collect();
        items.push(item("news", Some(500)));
        db.upsert_items(A, 0, &items).unwrap();
        let newest_episode = db
            .items(chris, &q())
            .unwrap()
            .items
            .into_iter()
            .find(|i| i.media.is_some())
            .unwrap()
            .id;
        db.set_read(chris, newest_episode, true).unwrap();
        let counts = |user| {
            let feed = db
                .feeds(user)
                .unwrap()
                .into_iter()
                .find(|f| f.url == A)
                .unwrap();
            (feed.total, feed.episodes, feed.unread, feed.unread_episodes)
        };
        assert_eq!(counts(chris), (4, 3, 3, 2));
        assert_eq!(counts(anna), (4, 3, 4, 3), "read state is per user");
    }

    #[test]
    fn queues_are_per_user_and_kept_from_pruning() {
        let (mut db, chris, anna) = two_users();
        let episodes: Vec<_> = (0..4)
            .map(|n| NewItem {
                media: Some(Media::default()),
                ..item(&format!("e{n}"), Some(n * 100))
            })
            .collect();
        db.upsert_items(A, 0, &episodes).unwrap();
        let ids: Vec<_> = db
            .items(chris, &q())
            .unwrap()
            .items
            .iter()
            .map(|i| i.id)
            .collect();
        let (newest, oldest) = (ids[0], ids[3]);

        let queued = db.set_queue(chris, &[oldest, newest]).unwrap();
        assert_eq!(
            queued.iter().map(|i| i.id).collect::<Vec<_>>(),
            [oldest, newest]
        );
        assert_eq!(queued[0].content, None, "like lists, without content");
        assert!(
            db.queue(anna).unwrap().is_empty(),
            "each user has their own"
        );

        // The oldest would go, but it is queued.
        assert_eq!(db.prune(A, 2, None).unwrap(), 1);
        assert_eq!(db.queue(chris).unwrap().len(), 2);
        assert!(db.item(chris, oldest).unwrap().is_some());

        // Emptied, it can be pruned.
        db.set_queue(chris, &[]).unwrap();
        assert_eq!(db.prune(A, 2, None).unwrap(), 1);
        assert!(db.item(chris, oldest).unwrap().is_none());
    }

    #[test]
    fn not_modified_keeps_artwork() {
        let (db, chris, _) = two_users();
        let image = |db: &Db| {
            let feeds = db.feeds(chris).unwrap();
            let feed = feeds.iter().find(|f| f.url == A).unwrap();
            (feed.image.clone(), feed.medium.clone())
        };
        db.record_fetch(
            A,
            1,
            &FetchResult {
                etag: Some("\"e\"".into()),
                image: Some("https://a.example/art.png".into()),
                medium: Some("podcast".into()),
                ..Default::default()
            },
        )
        .unwrap();
        db.record_fetch(
            A,
            2,
            &FetchResult {
                etag: Some("\"e\"".into()),
                not_modified: true,
                ..Default::default()
            },
        )
        .unwrap();
        let kept = (
            Some("https://a.example/art.png".to_owned()),
            Some("podcast".to_owned()),
        );
        assert_eq!(image(&db), kept);
        // A fresh read without them clears them.
        db.record_fetch(A, 3, &FetchResult::default()).unwrap();
        assert_eq!(image(&db), (None, None));
    }

    #[test]
    fn scheduling_state() {
        let (db, _, _) = two_users();
        db.record_fetch(
            A,
            10,
            &FetchResult {
                error: Some("HTTP 500".into()),
                next_fetch: 500,
                fail_count: 3,
                ..Default::default()
            },
        )
        .unwrap();
        for url in [B, C] {
            db.record_fetch(
                url,
                10,
                &FetchResult {
                    next_fetch: 300,
                    ..Default::default()
                },
            )
            .unwrap();
        }
        assert_eq!(db.next_due().unwrap(), Some(300));
        assert_eq!(db.counts().unwrap().failing_feeds, 1);
        assert_eq!(
            db.due_targets(500, 1800)
                .unwrap()
                .iter()
                .find(|t| t.url == A)
                .unwrap()
                .fail_count,
            3
        );
        db.force_refresh().unwrap();
        assert_eq!(db.due_targets(0, 1800).unwrap().len(), 3);
    }

    #[test]
    fn upgrading_from_the_first_schema_cleans_stored_html() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(MIGRATIONS[0]).unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
        conn.execute_batch(
            r#"INSERT INTO feeds (id, url) VALUES ('blog', 'https://blog.example/feed.xml');
               INSERT INTO items (id, feed_id, guid, title, url, summary, content, published)
               VALUES (1, 'blog', 'g', 'Post', '/posts/1',
                       '<p onclick="x()">Hi</p>',
                       '<p>Unique <script>bad()</script><a href="/about">words</a></p>', 5);"#,
        )
        .unwrap();
        // As `open` does, minus the file.
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        let mut db = Db { conn };
        db.migrate().unwrap();
        db.conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        db.sync(&config(
            "[feeds.blog]\nurl = \"https://blog.example/feed.xml\"",
        ))
        .unwrap();

        let user = db.only_user().unwrap().unwrap().id;
        let item = db.item(user, 1).unwrap().unwrap();
        let content = item.content.unwrap();
        assert!(
            !content.contains("script") && !content.contains("bad()"),
            "{content}"
        );
        assert!(
            content.contains(r#"href="https://blog.example/about""#),
            "relative to the item's link: {content}"
        );
        assert!(!item.summary.unwrap().contains("onclick"));
        let search = ItemQuery {
            q: Some("unique".into()),
            ..q()
        };
        assert_eq!(db.items(user, &search).unwrap().items.len(), 1, "indexed");
    }

    #[test]
    fn refuses_an_inconsistent_database() {
        let conn = Connection::open_in_memory().unwrap();
        for sql in MIGRATIONS {
            conn.execute_batch(sql).unwrap();
        }
        let current = i64::try_from(MIGRATIONS.len()).unwrap();
        conn.pragma_update(None, "user_version", current).unwrap();
        // State for an item that doesn't exist, as foreign keys allow
        // while they are off.
        conn.execute_batch(
            "PRAGMA foreign_keys = OFF;
             INSERT INTO users (id, name) VALUES (2, 'x');
             INSERT INTO item_state (user_id, item_id, read) VALUES (2, 99, 1);",
        )
        .unwrap();
        let error = Db::init(conn).err().unwrap();
        assert_eq!(
            error.to_string(),
            "database is inconsistent after migrating (table item_state)"
        );
    }

    #[test]
    fn migration_8_fetches_every_feed_in_full() {
        let conn = Connection::open_in_memory().unwrap();
        for sql in &MIGRATIONS[..7] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 7).unwrap();
        conn.execute(
            "INSERT INTO sources (url, etag, last_modified, next_fetch)
             VALUES ('https://pod.example/', '\"v1\"', 'Tue, 03 Nov 2026 10:00:00 GMT', 9999)",
            [],
        )
        .unwrap();
        let db = Db::init(conn).unwrap();
        let row: (Option<String>, Option<String>, i64) = db
            .conn
            .query_row(
                "SELECT etag, last_modified, next_fetch FROM sources",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(row, (None, None, 0));
    }

    #[test]
    fn refuses_a_newer_schema() {
        let conn = Connection::open_in_memory().unwrap();
        let newer = i64::try_from(MIGRATIONS.len()).unwrap() + 1;
        conn.pragma_update(None, "user_version", newer).unwrap();
        let error = Db::init(conn).err().unwrap();
        assert!(format!("{error}").contains("upgrade pressboxd"), "{error}");
    }

    #[test]
    fn filters_unstarred_and_names_devices() {
        let (mut db, chris, _) = two_users();
        db.upsert_items(A, 0, &[item("a1", Some(1)), item("a2", Some(2))])
            .unwrap();
        let a1 = db.items(chris, &q()).unwrap().items[1].id;
        db.set_starred(chris, a1, true).unwrap();
        let unstarred = ItemQuery {
            starred: Some(false),
            ..q()
        };
        assert_eq!(titles(&db.items(chris, &unstarred).unwrap()), ["title a2"]);

        for (n, expected) in ["pc", "pc-2", "pc-3"].iter().enumerate() {
            let name = db.add_device(chris, "pc", &format!("h{n}"), 0).unwrap();
            assert_eq!(&name, expected);
        }
        assert!(db.remove_device("pc-2").unwrap());
        assert_eq!(
            db.add_device(chris, "pc", "h9", 0).unwrap(),
            "pc-2",
            "freed"
        );
        assert!(
            db.add_device(chris, "pc", "h9", 0).is_err(),
            "tokens are unique"
        );
        // Every numbered name taken: give up rather than loop forever.
        db.break_with(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 999)
             INSERT INTO devices (name, token_hash, created, user_id)
             SELECT CASE WHEN i = 1 THEN 'tv' ELSE 'tv-' || i END, 'tv' || i, 0, 1 FROM n;",
        );
        let error = db.add_device(chris, "tv", "fresh", 0).unwrap_err();
        assert_eq!(error.to_string(), "too many devices named tv");
        assert_eq!(BadCursor.to_string(), "invalid cursor");
    }

    #[test]
    fn migrates_a_single_user_database() {
        let path =
            std::env::temp_dir().join(format!("pressboxd-migrate3-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(MIGRATIONS[0]).unwrap();
            conn.execute_batch(MIGRATIONS[1]).unwrap();
            conn.pragma_update(None, "user_version", 2).unwrap();
            conn.execute_batch(
                "INSERT INTO feeds (id, url, configured_title) VALUES
                    ('lwn', 'https://lwn.net/rss', 'LWN'), ('gone', 'https://gone.example/', NULL);
                 INSERT INTO items (id, feed_id, guid, title, published, read, starred) VALUES
                    (1, 'lwn', 'g1', 'Read one', 1, 1, 0),
                    (2, 'lwn', 'g2', 'Unread one', 2, 0, 0),
                    (3, 'gone', 'g3', 'Starred elsewhere', 3, 0, 1);
                 INSERT INTO items_fts (rowid, title, body) VALUES (1, 'Read one', ''), (2, 'Unread one', ''), (3, 'Starred elsewhere', '');
                 INSERT INTO devices (name, token_hash, created) VALUES ('laptop', 'h', 0);",
            )
            .unwrap();
        }
        let mut db = Db::open(&path).unwrap();
        db.sync(&config(
            r#"
            [users.anna]
            [users.anna.feeds.lwn]
            url = "https://lwn.net/rss"
            [users.chris]
            admin = true
            [users.chris.feeds.lwn]
            url = "https://lwn.net/rss"
            "#,
        ))
        .unwrap();

        // The single user's state and devices went to chris, the admin.
        let chris = db.user("chris").unwrap().unwrap().id;
        let anna = db.user("anna").unwrap().unwrap().id;
        assert_eq!(db.authenticate("h", 1).unwrap().unwrap().name, "chris");
        let feeds = db.feeds(chris).unwrap();
        let lwn = feeds.iter().find(|f| f.id == "lwn").unwrap();
        assert_eq!((lwn.title.as_str(), lwn.unread, lwn.total), ("lwn", 1, 2));
        let gone = feeds.iter().find(|f| f.id == "gone").unwrap();
        assert!(gone.archived, "removed feed kept for chris's starred item");
        assert_eq!(
            db.items(
                chris,
                &ItemQuery {
                    starred: Some(true),
                    ..q()
                }
            )
            .unwrap()
            .items
            .len(),
            1
        );
        // anna starts fresh on the shared URL.
        assert_eq!(db.feeds(anna).unwrap()[0].unread, 2);
        assert_eq!(
            db.items(
                anna,
                &ItemQuery {
                    q: Some("unread".into()),
                    ..q()
                }
            )
            .unwrap()
            .items
            .len(),
            1
        );
        drop(db);
        Db::open(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
    }
}

#[cfg(test)]
mod bench {
    use std::time::Instant;

    use super::*;

    /// Not a test: `cargo test --release -p pressboxd bench -- --ignored --nocapture`.
    #[test]
    #[ignore = "timing only"]
    fn timings() {
        let mut feeds = String::new();
        for f in 0..200 {
            feeds.push_str(&format!("[feeds.f{f}]\nurl = \"https://f{f}.example/\"\n"));
        }
        for u in 0..5 {
            feeds.push_str(&format!("[users.u{u}]\nadmin = {}\n", u == 0));
        }
        let path = std::env::temp_dir().join("pressboxd-bench.db");
        let _ = std::fs::remove_file(&path);
        let mut db = Db::open(&path).unwrap();
        db.sync(&Config::parse(&feeds).unwrap()).unwrap();
        let body = "<p>".to_owned() + &"lorem ipsum dolor sit amet ".repeat(80) + "</p>";
        let t = Instant::now();
        for f in 0..200 {
            let items: Vec<NewItem> = (0..500)
                .map(|n| NewItem {
                    guid: format!("{f}-{n}"),
                    title: format!("Item {n} of feed {f} about rust and nix"),
                    url: Some(format!("https://f{f}.example/{n}")),
                    author: None,
                    summary: Some(body.clone()),
                    content: Some(body.clone()),
                    published: Some(i64::from(n)),
                    media: None,
                })
                .collect();
            db.upsert_items(&format!("https://f{f}.example/"), 0, &items)
                .unwrap();
        }
        println!("insert 100k items: {:?}", t.elapsed());
        let t = Instant::now();
        let again: Vec<NewItem> = (0..500)
            .map(|n| NewItem {
                guid: format!("0-{n}"),
                title: format!("Item {n} of feed 0 about rust and nix"),
                url: Some(format!("https://f0.example/{n}")),
                author: None,
                summary: Some(body.clone()),
                content: Some(body.clone()),
                published: Some(i64::from(n)),
                media: None,
            })
            .collect();
        db.upsert_items("https://f0.example/", 0, &again).unwrap();
        println!("re-store one unchanged 500-item feed: {:?}", t.elapsed());
        let mut changed = again.clone();
        for item in changed.iter_mut().take(5) {
            item.title.push_str(" (updated)");
        }
        let t = Instant::now();
        db.upsert_items("https://f0.example/", 0, &changed).unwrap();
        println!("re-store it with 5 of 500 items changed: {:?}", t.elapsed());
        let u = db.user("u1").unwrap().unwrap().id;
        let t = Instant::now();
        for i in 0..20_000 {
            db.set_read(u, i * 5 + 1, true).unwrap();
        }
        println!("set_read 20k items: {:?}", t.elapsed());
        let time = |name: &str, f: &mut dyn FnMut()| {
            f();
            let t = Instant::now();
            for _ in 0..10 {
                f();
            }
            println!("{name}: {:?}", t.elapsed() / 10);
        };
        time("feeds (200, unread counts)", &mut || {
            db.feeds(u).unwrap();
        });
        time("items page 1, all", &mut || {
            db.items(
                u,
                &ItemQuery {
                    limit: Some(100),
                    ..ItemQuery::default()
                },
            )
            .unwrap();
        });
        time("items page 1, unread", &mut || {
            db.items(
                u,
                &ItemQuery {
                    limit: Some(100),
                    unread: Some(true),
                    ..ItemQuery::default()
                },
            )
            .unwrap();
        });
        time("items page 1, one feed unread", &mut || {
            db.items(
                u,
                &ItemQuery {
                    feed: Some("f7".into()),
                    unread: Some(true),
                    limit: Some(100),
                    ..ItemQuery::default()
                },
            )
            .unwrap();
        });
        time("items page 50 (offset 4900)", &mut || {
            db.items(
                u,
                &ItemQuery {
                    limit: Some(100),
                    offset: Some(4900),
                    ..ItemQuery::default()
                },
            )
            .unwrap();
        });
        let cursor = db
            .items(
                u,
                &ItemQuery {
                    limit: Some(4900),
                    ..ItemQuery::default()
                },
            )
            .unwrap()
            .next_cursor;
        time("items page 50 (cursor after 4900)", &mut || {
            db.items(
                u,
                &ItemQuery {
                    limit: Some(100),
                    cursor: cursor.clone(),
                    ..ItemQuery::default()
                },
            )
            .unwrap();
        });
        time("starred", &mut || {
            db.items(
                u,
                &ItemQuery {
                    starred: Some(true),
                    ..ItemQuery::default()
                },
            )
            .unwrap();
        });
        time("search 'rust nix'", &mut || {
            db.items(
                u,
                &ItemQuery {
                    q: Some("rust nix".into()),
                    limit: Some(100),
                    ..ItemQuery::default()
                },
            )
            .unwrap();
        });
        time("search with no match", &mut || {
            db.items(
                u,
                &ItemQuery {
                    q: Some("zeppelin".into()),
                    ..ItemQuery::default()
                },
            )
            .unwrap();
        });
        time("search one feed", &mut || {
            db.items(
                u,
                &ItemQuery {
                    q: Some("rust".into()),
                    feed: Some("f7".into()),
                    ..ItemQuery::default()
                },
            )
            .unwrap();
        });
        time("search rare word", &mut || {
            db.items(
                u,
                &ItemQuery {
                    q: Some("499".into()),
                    ..ItemQuery::default()
                },
            )
            .unwrap();
        });
        time("mark feed read", &mut || {
            db.mark_feed_read(u, "f3").unwrap();
        });
        time("prune feed", &mut || {
            db.prune("https://f9.example/", 500, None).unwrap();
        });
        println!(
            "db size: {} MB",
            std::fs::metadata(&path).unwrap().len() / 1_000_000
        );
    }
}
