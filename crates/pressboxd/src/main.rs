// SPDX-FileCopyrightText: 2026 Chris Palmer <chris@red-oxide.org>
// SPDX-License-Identifier: AGPL-3.0-only

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use anyhow::{Context, Result};
use clap::Parser;
use tokio::sync::{Notify, Semaphore};
use tracing::info;

mod admin;
mod api;
mod config;
mod db;
mod fetch;
mod media;
mod openapi;
mod pairing;
mod sanitize;
mod systemd;

use config::Config;
use db::Db;

/// pressbox RSS/Atom aggregation server.
#[derive(Parser)]
#[command(version)]
struct Args {
    /// Path to the TOML configuration.
    #[arg(short, long, env = "PRESSBOX_CONFIG", default_value = "pressbox.toml")]
    config: PathBuf,
    /// Validate the configuration and exit.
    #[arg(long)]
    check: bool,
}

/// Read-only connections in the pool; reads are short, so a few suffice.
const READERS: usize = 4;

pub struct AppState {
    /// The one writer connection. Never hold the guard across an `.await`.
    db: Mutex<Db>,
    /// Read-only connections; empty for in-memory databases (tests).
    readers: Vec<Mutex<Db>>,
    next_reader: std::sync::atomic::AtomicUsize,
    /// Fetches the feeds the configuration names.
    http: reqwest::Client,
    /// Fetches what feeds link to (articles, chapters, transcripts): public
    /// addresses only, unless `fetch_private_addresses` is set.
    links: fetch::Links,
    /// The static token from `token_file` or the systemd credential.
    token: Option<String>,
    refresh: Notify,
    max_items_per_feed: u32,
    /// Items published longer ago than this are removed (unless starred).
    max_age_days: Option<u32>,
    pairings: pairing::Pairings,
    /// Feeds whose full articles are being fetched, so a feed has at most
    /// one such task at a time.
    articles_running: Mutex<HashSet<String>>,
    /// Limits article fetching across feeds, apart from feed polling.
    article_slots: Semaphore,
    backup_dir: PathBuf,
    /// Trust `X-Forwarded-For` from a reverse proxy for client addresses.
    behind_proxy: bool,
    /// Chapters and transcript URLs that couldn't be fetched or read, and
    /// when, so asking again doesn't fetch them again straight away.
    media_failures: Mutex<HashMap<String, i64>>,
}

impl AppState {
    pub fn new(db: Db, token: Option<String>, max_items_per_feed: u32) -> Result<Self> {
        Ok(Self {
            db: Mutex::new(db),
            readers: Vec::new(),
            next_reader: std::sync::atomic::AtomicUsize::new(0),
            http: fetch::client()?,
            links: fetch::Links::public()?,
            token,
            refresh: Notify::new(),
            max_items_per_feed,
            max_age_days: None,
            pairings: pairing::Pairings::default(),
            articles_running: Mutex::default(),
            article_slots: Semaphore::const_new(fetch::ARTICLE_TASKS),
            backup_dir: PathBuf::from("backups"),
            behind_proxy: false,
            media_failures: Mutex::default(),
        })
    }

    /// Makes feeds due (all but the ones fetched a moment ago; see
    /// `Db::force_refresh`) and wakes the scheduler.
    pub async fn request_refresh(self: &Arc<Self>) {
        if let Err(e) = self.write(|db| db.force_refresh(fetch::now())).await {
            tracing::warn!("forcing a refresh: {e:#}");
        }
        self.refresh.notify_one();
    }

    /// Opens `count` read-only connections to the database at `path`.
    pub fn with_readers(mut self, path: &std::path::Path, count: usize) -> Result<Self> {
        self.readers = (0..count)
            .map(|_| Db::open_reader(path).map(Mutex::new))
            .collect::<Result<_>>()?;
        Ok(self)
    }

    /// Locks the writer connection. Never hold the guard across an `.await`.
    pub fn db(&self) -> MutexGuard<'_, Db> {
        self.db
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// A read-only connection: a free one if there is, else the next in
    /// turn. Reads go here so they never wait behind a write. Falls back to
    /// the writer when there is no pool. Never hold the guard across an
    /// `.await`.
    pub fn reader(&self) -> MutexGuard<'_, Db> {
        if self.readers.is_empty() {
            return self.db();
        }
        let start = self
            .next_reader
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        for i in 0..self.readers.len() {
            if let Ok(guard) = self.readers[(start + i) % self.readers.len()].try_lock() {
                return guard;
            }
        }
        self.readers[start % self.readers.len()]
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Runs `f` on a read connection on the blocking pool. SQLite calls
    /// block, and a lock may have to wait, so async code goes through here
    /// (or `write`) rather than holding up a runtime thread.
    pub async fn read<T, F>(self: &Arc<Self>, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Db) -> Result<T> + Send + 'static,
    {
        let state = self.clone();
        tokio::task::spawn_blocking(move || f(&state.reader())).await?
    }

    /// Runs `f` on the writer connection on the blocking pool; see `read`.
    pub async fn write<T, F>(self: &Arc<Self>, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Db) -> Result<T> + Send + 'static,
    {
        let state = self.clone();
        tokio::task::spawn_blocking(move || f(&mut state.db())).await?
    }
}

// Not measured: sets the process-wide logger, which a test process can
// only do once, and it decides where every other test's logs go.
#[cfg(not(tarpaulin_include))]
fn init_logging() {
    use tracing_subscriber::{EnvFilter, Registry, layer::SubscriberExt, util::SubscriberInitExt};

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| "pressboxd=info".into());
    // Under systemd, log to the journal directly so levels become priorities
    // (journalctl -p warning) and fields stay searchable.
    if systemd::stderr_is_journal() {
        match tracing_journald::layer() {
            Ok(journal) => {
                Registry::default()
                    .with(filter)
                    .with(journal.with_syslog_identifier("pressboxd".into()))
                    .init();
                return;
            }
            Err(e) => eprintln!("cannot connect to the journal, logging to stderr: {e}"),
        }
    }
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

// Not measured: only wires the process (logging, arguments, signals) to
// `Server`, which the tests start and stop themselves.
#[cfg(not(tarpaulin_include))]
#[tokio::main]
async fn main() -> Result<()> {
    init_logging();
    let args = Args::parse();
    let config = Config::load(&args.config)?;
    if args.check {
        println!("{}", check_summary(&args.config, &config));
        return Ok(());
    }
    Server::start(&config, systemd::listener()?)
        .await?
        .run(shutdown())
        .await
}

/// What `pressboxd --check` prints for a valid configuration.
fn check_summary(path: &std::path::Path, config: &Config) -> String {
    format!("{}: ok, {} feeds", path.display(), config.feed_count())
}

/// A server ready to run: its database open and in step with the
/// configuration, and its sockets bound.
struct Server {
    state: Arc<AppState>,
    listener: tokio::net::TcpListener,
    address: std::net::SocketAddr,
    control: tokio::net::UnixListener,
    control_path: PathBuf,
    feeds: usize,
    refresh_interval: u64,
}

impl Server {
    /// Opens and syncs the database and binds the API and control sockets.
    /// `inherited` is a listening socket from systemd, used instead of
    /// binding `listen`.
    async fn start(config: &Config, inherited: Option<std::net::TcpListener>) -> Result<Self> {
        let mut db = Db::open(&config.database)
            .with_context(|| format!("opening {}", config.database.display()))?;
        db.sync(config)?;
        let mut state = AppState::new(db, config.token()?, config.max_items_per_feed)?
            .with_readers(&config.database, READERS)?;
        state.backup_dir = config.backup_dir();
        state.behind_proxy = config.behind_proxy;
        if config.fetch_private_addresses {
            state.links = fetch::Links::unrestricted(state.http.clone());
        }
        state.max_age_days = config.max_age_days;
        let state = Arc::new(state);

        let listener = match inherited {
            Some(socket) => {
                let inherited = socket.local_addr()?;
                info!("using the socket from systemd ({inherited}); `listen` is ignored");
                tokio::net::TcpListener::from_std(socket)?
            }
            None => tokio::net::TcpListener::bind(config.listen)
                .await
                .with_context(|| format!("binding {}", config.listen))?,
        };
        let address = listener.local_addr()?;
        let control_path = config.control_socket();
        let control = admin::bind(&control_path)?;
        let feeds = config.feed_count();
        let shown = control_path.display();
        info!("listening on {address} with {feeds} feeds; control socket {shown}");
        if state.token.is_none() && state.reader().counts()?.devices == 0 {
            info!(
                "no devices paired yet: run `pressboxctl pair` on a client, then `pressboxctl approve`"
            );
        }
        Ok(Self {
            state,
            listener,
            address,
            control,
            control_path,
            feeds,
            refresh_interval: config.refresh_interval,
        })
    }

    /// Serves the API and control socket and fetches feeds until `shutdown`
    /// finishes, then removes the control socket.
    async fn run(self, shutdown: impl Future<Output = ()> + Send + 'static) -> Result<()> {
        let state = self.state;
        systemd::ready(&format!(
            "listening on {}, {} feeds",
            self.address, self.feeds
        ));
        systemd::spawn_watchdog(state.clone());
        let scheduler = tokio::spawn(schedule(state.clone(), self.refresh_interval));
        let control = axum::serve(self.control, admin::router(state.clone()));
        let control = tokio::spawn(control.into_future());

        let api = api::router(state).into_make_service_with_connect_info::<std::net::SocketAddr>();
        let served = axum::serve(self.listener, api)
            .with_graceful_shutdown(shutdown)
            .await;
        scheduler.abort();
        control.abort();
        let _ = std::fs::remove_file(&self.control_path);
        Ok(served?)
    }
}

/// The shortest wait before feeds are fetched again when the database took
/// neither their entries nor when to try next. They are still due, and
/// without it their servers would be asked again every second.
const UNRECORDED_PAUSE: u64 = 60;

/// How long the scheduler sleeps: until the next feed is due, but at least a
/// second ([`UNRECORDED_PAUSE`] after a pass whose outcome went unrecorded)
/// and at most five minutes, so newly due feeds are noticed.
fn pause(next: Option<i64>, now: i64, unrecorded: bool) -> Duration {
    let shortest = if unrecorded { UNRECORDED_PAUSE } else { 1 };
    let until = next.unwrap_or(now + 300) - now;
    Duration::from_secs(u64::try_from(until).unwrap_or(0).clamp(shortest, 300))
}

/// Fetches feeds as they come due, sleeping until the next one or until a
/// refresh is requested; see [`pause`].
async fn schedule(state: Arc<AppState>, default_interval: u64) {
    let mut last = None;
    loop {
        let mut unrecorded = false;
        if let Some(stats) = fetch::run_due(state.clone(), default_interval).await {
            unrecorded = stats.unrecorded > 0;
            last = Some((stats, jiff::ZonedDateTime::now()));
        }
        if let Ok(counts) = state.read(|db| db.counts()).await {
            systemd::status(&status_line(&counts, last.as_ref()));
        }
        let now = fetch::now();
        let next = state.read(|db| db.next_due()).await.ok().flatten();
        let wait = pause(next, now, unrecorded);
        if unrecorded {
            // A requested refresh waits too; it is still pending afterwards.
            tokio::time::sleep(wait).await;
            continue;
        }
        tokio::select! {
            () = tokio::time::sleep(wait) => {}
            () = state.refresh.notified() => info!("refresh requested"),
        }
    }
}

fn status_line(
    counts: &db::Counts,
    last: Option<&(fetch::RefreshStats, jiff::ZonedDateTime)>,
) -> String {
    let mut s = format!("{} feeds, {} users", counts.feeds, counts.users);
    if let Some((stats, at)) = last {
        s.push_str(&format!(
            ", {} new at {}",
            stats.new_items,
            at.strftime("%H:%M")
        ));
    }
    if counts.failing_feeds > 0 {
        s.push_str(&format!(", {} failing", counts.failing_feeds));
    }
    s
}

// Not measured: waits for a signal to this process. `Server::run` takes any
// future, and the tests pass their own.
#[cfg(not(tarpaulin_include))]
async fn shutdown() {
    let ctrl_c = tokio::signal::ctrl_c();
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("installing SIGTERM handler");
    tokio::select! {
        _ = ctrl_c => {}
        _ = term.recv() => {}
    }
    info!("shutting down");
    systemd::stopping();
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use axum::{body::Body, http::Request};
    use pressbox_api::ItemQuery;
    use tower::ServiceExt;

    use super::*;

    /// A config in a fresh directory, listening on a free local port.
    fn server_config(name: &str, extra: &str) -> (Config, PathBuf) {
        let dir = std::env::temp_dir().join(format!("pressboxd-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let config = Config::parse(&format!(
            r#"
            database = "{db}"
            listen = "127.0.0.1:0"
            control_socket = "{sock}"
            {extra}
            [feeds.a]
            url = "https://a.example/feed"
            "#,
            db = dir.join("data/pressbox.db").display(),
            sock = dir.join("control.sock").display(),
        ))
        .unwrap();
        (config, dir)
    }

    /// Sends one request over the control socket, as pressboxctl does.
    async fn control_get(path: &std::path::Path, uri: &str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut stream = tokio::net::UnixStream::connect(path).await.unwrap();
        stream
            .write_all(
                format!("GET {uri} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").as_bytes(),
            )
            .await
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        response
    }

    /// Starts a server, uses both of its sockets, and shuts it down.
    #[test]
    fn serves_until_shut_down() {
        let _env = systemd::ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (config, dir) = server_config("serve", "");
        let token = dir.join("token");
        std::fs::write(&token, "static-secret\n").unwrap();
        let config = Config {
            token_file: Some(token),
            ..config
        };
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let server = Server::start(&config, None).await.unwrap();
                assert!(server.state.links.public_only(), "unless configured");
                let address = server.address;
                assert_ne!(address.port(), 0, "the port actually bound");
                let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
                let running = tokio::spawn(server.run(async {
                    let _ = stopped.await;
                }));

                let http = reqwest::Client::builder().no_proxy().build().unwrap();
                let base = format!("http://{address}");
                let health = http.get(format!("{base}/health")).send().await.unwrap();
                assert_eq!(health.text().await.unwrap(), "ok");
                let me = http
                    .get(format!("{base}/api/v1/me"))
                    .bearer_auth("static-secret")
                    .send()
                    .await
                    .unwrap()
                    .text()
                    .await
                    .unwrap();
                let me: pressbox_api::Me = serde_json::from_str(&me).unwrap();
                assert_eq!((me.user.as_str(), me.device), ("default", None));

                let control = dir.join("control.sock");
                let status = control_get(&control, "/admin/status").await;
                assert!(status.starts_with("HTTP/1.1 200"), "{status}");
                assert!(status.contains(r#""feeds":1"#), "{status}");

                // A second server can't take the same port.
                let taken = Config {
                    listen: address,
                    control_socket: Some(dir.join("other.sock")),
                    ..config.clone()
                };
                let error = Server::start(&taken, None).await.err().unwrap();
                assert!(format!("{error}").starts_with("binding"), "{error}");

                stop.send(()).unwrap();
                running.await.unwrap().unwrap();
                assert!(!control.exists(), "the control socket is removed");
                assert!(
                    http.get(format!("{base}/health")).send().await.is_err(),
                    "no longer listening"
                );
            });
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn uses_a_socket_from_systemd_and_reports_bad_configs() {
        let _env = systemd::ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (config, dir) = server_config("inherit", "fetch_private_addresses = true");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            socket.set_nonblocking(true).unwrap();
            let address = socket.local_addr().unwrap();
            let server = Server::start(&config, Some(socket)).await.unwrap();
            assert!(!server.state.links.public_only());
            assert_eq!(server.address, address, "`listen` is ignored");
            server.run(async {}).await.unwrap();

            let unreadable = Config {
                token_file: Some(dir.join("missing-token")),
                ..config.clone()
            };
            let error = Server::start(&unreadable, None).await.err().unwrap();
            assert!(
                format!("{error}").starts_with("reading token file"),
                "{error}"
            );
            let no_database = Config {
                database: PathBuf::from("/proc/pressbox.db"),
                ..config.clone()
            };
            let error = Server::start(&no_database, None).await.err().unwrap();
            assert!(format!("{error}").starts_with("opening /proc"), "{error}");
        });
        assert_eq!(
            check_summary(std::path::Path::new("/etc/pressbox.toml"), &config),
            "/etc/pressbox.toml: ok, 1 feeds"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn a_refresh_wakes_the_scheduler_even_if_the_database_fails() {
        let state = Arc::new(AppState::new(Db::open_in_memory().unwrap(), None, 10).unwrap());
        state.db().break_with("DROP TABLE subscriptions;");
        state.request_refresh().await;
        tokio::time::timeout(Duration::from_secs(1), state.refresh.notified())
            .await
            .expect("woken");
    }

    #[test]
    fn readers_wait_when_every_connection_is_busy() {
        let path = std::env::temp_dir().join(format!("pressboxd-busy-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let db = Db::open(&path).unwrap();
        let state = Arc::new(
            AppState::new(db, None, 10)
                .unwrap()
                .with_readers(&path, 2)
                .unwrap(),
        );
        let (first, second) = (state.reader(), state.reader());
        let (done_tx, done) = std::sync::mpsc::channel();
        let waiter = {
            let state = state.clone();
            std::thread::spawn(move || {
                state.reader().ping().unwrap();
                done_tx.send(()).unwrap();
            })
        };
        assert!(
            done.recv_timeout(Duration::from_millis(100)).is_err(),
            "both busy: it waits"
        );
        drop((first, second));
        done.recv_timeout(Duration::from_secs(5)).unwrap();
        waiter.join().unwrap();
        drop(state);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }

    #[test]
    fn pauses_until_the_next_feed_is_due() {
        let secs = |next, unrecorded| pause(next, 1000, unrecorded).as_secs();
        assert_eq!(secs(Some(1090), false), 90);
        assert_eq!(secs(Some(5000), false), 300, "at most five minutes");
        assert_eq!(secs(None, false), 300, "no feeds at all");
        assert_eq!(secs(Some(900), false), 1, "overdue: at once");
        // Still due because nothing could be written: not at once.
        assert_eq!(secs(Some(0), true), UNRECORDED_PAUSE);
        assert_eq!(secs(Some(1200), true), 200);
    }

    #[test]
    fn status_line_sums_up() {
        let counts = db::Counts {
            feeds: 12,
            users: 2,
            ..db::Counts::default()
        };
        assert_eq!(status_line(&counts, None), "12 feeds, 2 users");
        let stats = fetch::RefreshStats {
            fetched: 12,
            failed: 1,
            new_items: 5,
            unrecorded: 0,
        };
        let at = jiff::civil::date(2026, 11, 9)
            .at(14, 5, 0, 0)
            .to_zoned_datetime(jiff::tz::TimeZone::system())
            .unwrap();
        let failing = db::Counts {
            failing_feeds: 1,
            ..counts
        };
        assert_eq!(
            status_line(&failing, Some(&(stats, at))),
            "12 feeds, 2 users, 5 new at 14:05, 1 failing"
        );
    }

    /// The scheduler fetches what is due, then sleeps until a refresh is
    /// asked for (or the next feed is due). It reports to systemd, so it
    /// holds the environment lock, outside the runtime.
    #[test]
    fn schedules_fetches_and_wakes_on_refresh() {
        let _env = systemd::ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(scheduler_round_trip());
    }

    async fn scheduler_round_trip() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        // Each fetch of the feed adds an item.
        let fetches = Arc::new(AtomicUsize::new(0));
        let app = {
            let fetches = fetches.clone();
            axum::Router::new().route(
                "/feed",
                axum::routing::get(move || {
                    let n = fetches.fetch_add(1, Ordering::SeqCst) + 1;
                    let items: String = (1..=n)
                        .map(|i| format!("<item><title>Item {i}</title><guid>{i}</guid></item>"))
                        .collect();
                    async move {
                        format!(
                            r#"<rss version="2.0"><channel><title>T</title>{items}</channel></rss>"#
                        )
                    }
                }),
            )
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await });

        let mut db = Db::open_in_memory().unwrap();
        db.sync(&Config::parse(&format!("[feeds.a]\nurl = \"http://{addr}/feed\"")).unwrap())
            .unwrap();
        let user = db.only_user().unwrap().unwrap().id;
        let mut state = AppState::new(db, None, 100).unwrap();
        state.http = reqwest::Client::builder().no_proxy().build().unwrap();
        let state = Arc::new(state);
        let scheduler = tokio::spawn(schedule(state.clone(), 1800));

        let items = || {
            state
                .reader()
                .items(user, &ItemQuery::default())
                .unwrap()
                .items
                .len()
        };
        let wait_for = |n: usize| async move {
            for _ in 0..200 {
                if items() >= n {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            panic!("never got {n} items");
        };
        wait_for(1).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            fetches.load(Ordering::SeqCst),
            1,
            "not due again for 30 minutes"
        );

        // (As if the first fetch were a while ago: one just made isn't
        // repeated.)
        state.request_refresh().await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(fetches.load(Ordering::SeqCst), 1, "fetched a moment ago");
        state
            .db()
            .break_with("UPDATE sources SET last_fetched = last_fetched - 3600");
        state.request_refresh().await;
        wait_for(2).await;
        assert_eq!(fetches.load(Ordering::SeqCst), 2);
        scheduler.abort();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn reads_do_not_wait_for_the_writer() {
        let path = std::env::temp_dir().join(format!("pressboxd-pool-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut db = Db::open(&path).unwrap();
        db.sync(&Config::parse("[feeds.a]\nurl = \"https://a.example/\"").unwrap())
            .unwrap();
        let user = db.only_user().unwrap().unwrap();
        let token = pairing::create_device(&db, &user, "laptop", fetch::now())
            .unwrap()
            .token;
        db.authenticate(&pairing::hash_token(&token), fetch::now())
            .unwrap();
        let state = Arc::new(
            AppState::new(db, None, 100)
                .unwrap()
                .with_readers(&path, 2)
                .unwrap(),
        );

        // A long write holds the writer lock.
        let (held_tx, held_rx) = std::sync::mpsc::channel();
        let writer = {
            let state = state.clone();
            std::thread::spawn(move || {
                let mut db = state.db();
                let items: Vec<db::NewItem> = (0..200)
                    .map(|n| db::NewItem {
                        guid: n.to_string(),
                        title: format!("item {n}"),
                        url: None,
                        author: None,
                        summary: None,
                        content: None,
                        published: Some(n),
                        media: None,
                    })
                    .collect();
                db.upsert_items("https://a.example/", 0, &items).unwrap();
                held_tx.send(()).unwrap();
                std::thread::sleep(Duration::from_millis(800));
            })
        };
        held_rx.recv().unwrap();

        // Reads, and authenticating a request, answer while it is held.
        let started = Instant::now();
        let page = state
            .reader()
            .items(user.id, &ItemQuery::default())
            .unwrap();
        assert_eq!(page.items.len(), 50, "sees the committed write");
        let req = Request::get("/api/v1/feeds")
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        let resp = api::router(state.clone()).oneshot(req).await.unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        assert!(
            started.elapsed() < Duration::from_millis(300),
            "reads waited {:?} for the writer",
            started.elapsed()
        );
        writer.join().unwrap();
        drop(state);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }
}
