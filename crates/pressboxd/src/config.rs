// SPDX-FileCopyrightText: 2026 Chris Palmer <chris@red-oxide.org>
// SPDX-License-Identifier: AGPL-3.0-only

//! Server configuration. On NixOS this file is generated from
//! `services.pressbox` by the module in `nix/nixos-module.nix`.

use std::{collections::BTreeMap, net::SocketAddr, path::PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "default_listen")]
    pub listen: SocketAddr,
    #[serde(default = "default_database")]
    pub database: PathBuf,
    /// Seconds between refreshes of every feed.
    #[serde(default = "default_refresh_interval")]
    pub refresh_interval: u64,
    /// Items kept per feed. Starred items are never pruned.
    #[serde(default = "default_max_items")]
    pub max_items_per_feed: u32,
    /// Remove items published more than this many days ago (starred ones
    /// stay). Unset keeps them until `max_items_per_feed` pushes them out.
    #[serde(default)]
    pub max_age_days: Option<u32>,
    /// File holding an optional static token, accepted as the first admin
    /// user. If unset, the systemd credential `token` is used when present.
    #[serde(default)]
    pub token_file: Option<PathBuf>,
    /// Unix socket for `pressboxctl`. Defaults to `$RUNTIME_DIRECTORY/control.sock`
    /// under systemd, otherwise `pressbox.sock` next to the database.
    #[serde(default)]
    pub control_socket: Option<PathBuf>,
    /// Set when a reverse proxy (or `tailscale serve`) sits in front of the
    /// server: client addresses then come from its `X-Forwarded-For`.
    #[serde(default)]
    pub behind_proxy: bool,
    /// Let the URLs feeds supply (article pages for `full_text`, chapters,
    /// transcripts) be fetched from this machine and private networks too.
    /// Off, only public addresses are; the feeds themselves are fetched
    /// from wherever their `url` says either way.
    #[serde(default)]
    pub fetch_private_addresses: bool,
    /// Where `pressboxctl backup` writes. Defaults to `backups/` next to the
    /// database.
    #[serde(default)]
    pub backup_dir: Option<PathBuf>,
    /// Feeds every user gets.
    #[serde(default)]
    pub feeds: BTreeMap<String, FeedConfig>,
    /// Users and their own feeds. With none, there is one admin user named
    /// [`DEFAULT_USER`].
    #[serde(default)]
    pub users: BTreeMap<String, UserConfig>,
}

/// The implicit user when the configuration declares none.
pub const DEFAULT_USER: &str = "default";

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserConfig {
    /// Admins can approve new devices for any user.
    #[serde(default)]
    pub admin: bool,
    /// Feeds only this user gets, besides the shared ones.
    #[serde(default)]
    pub feeds: BTreeMap<String, FeedConfig>,
}

/// A user with the full set of feeds they subscribe to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveUser {
    pub admin: bool,
    pub feeds: BTreeMap<String, FeedConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeedConfig {
    pub url: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Seconds between fetches of this feed, overriding `refresh_interval`.
    #[serde(default)]
    pub refresh_interval: Option<u64>,
    /// Fetch each new item's page and keep its main content, for feeds that
    /// only carry a summary.
    #[serde(default)]
    pub full_text: bool,
}

fn default_listen() -> SocketAddr {
    ([127, 0, 0, 1], 8470).into()
}
fn default_database() -> PathBuf {
    "pressbox.db".into()
}
fn default_refresh_interval() -> u64 {
    1800
}
fn default_max_items() -> u32 {
    500
}

impl Config {
    pub fn load(path: &std::path::Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Self> {
        let config: Self = toml::from_str(text)?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        if self.refresh_interval < 60 {
            bail!("refresh_interval must be at least 60 seconds");
        }
        if self.max_items_per_feed == 0 {
            bail!("max_items_per_feed must be at least 1");
        }
        if self.max_age_days == Some(0) {
            bail!("max_age_days must be at least 1");
        }
        validate_feeds("", &self.feeds)?;
        for (name, user) in &self.users {
            if crate::pairing::device_name(name).as_deref() != Some(name.as_str()) {
                bail!("user name {name:?} may only contain letters, digits, '-', '_' and '.'");
            }
            validate_feeds(&format!("user {name}: "), &user.feeds)?;
            if let Some(id) = user.feeds.keys().find(|id| self.feeds.contains_key(*id)) {
                bail!("user {name}: feed {id} is also a shared feed; rename one of them");
            }
        }
        for (name, user) in self.users() {
            let mut urls = std::collections::HashMap::new();
            for (id, feed) in &user.feeds {
                if let Some(other) = urls.insert(feed.url.as_str(), id) {
                    bail!("user {name}: feeds {other} and {id} have the same url");
                }
            }
        }
        Ok(())
    }

    /// Every user with their shared and own feeds combined.
    pub fn users(&self) -> BTreeMap<String, EffectiveUser> {
        let declared: BTreeMap<String, UserConfig> = if self.users.is_empty() {
            BTreeMap::from([(
                DEFAULT_USER.to_owned(),
                UserConfig {
                    admin: true,
                    feeds: BTreeMap::new(),
                },
            )])
        } else {
            self.users.clone()
        };
        declared
            .into_iter()
            .map(|(name, user)| {
                let mut feeds = self.feeds.clone();
                feeds.extend(user.feeds);
                (
                    name,
                    EffectiveUser {
                        admin: user.admin,
                        feeds,
                    },
                )
            })
            .collect()
    }

    /// Every distinct feed URL, for counting.
    pub fn feed_count(&self) -> usize {
        self.users()
            .values()
            .flat_map(|u| u.feeds.values().map(|f| f.url.clone()))
            .collect::<std::collections::HashSet<_>>()
            .len()
    }

    fn database_dir(&self) -> PathBuf {
        self.database
            .parent()
            .filter(|d| !d.as_os_str().is_empty())
            .map_or_else(|| PathBuf::from("."), std::path::Path::to_path_buf)
    }

    pub fn control_socket(&self) -> PathBuf {
        self.control_socket.clone().unwrap_or_else(|| {
            std::env::var_os("RUNTIME_DIRECTORY")
                .map(|d| PathBuf::from(d).join("control.sock"))
                .unwrap_or_else(|| self.database_dir().join("pressbox.sock"))
        })
    }

    pub fn backup_dir(&self) -> PathBuf {
        self.backup_dir
            .clone()
            .unwrap_or_else(|| self.database_dir().join("backups"))
    }

    /// Reads the token from `token_file`, or else from the systemd
    /// credential, trimming the trailing newline.
    pub fn token(&self) -> Result<Option<String>> {
        let Some(path) = self
            .token_file
            .clone()
            .or_else(|| crate::systemd::credential(crate::systemd::TOKEN_CREDENTIAL))
        else {
            return Ok(None);
        };
        let token = std::fs::read_to_string(&path)
            .with_context(|| format!("reading token file {}", path.display()))?
            .trim()
            .to_owned();
        if token.is_empty() {
            bail!("token file {} is empty", path.display());
        }
        Ok(Some(token))
    }
}

fn validate_feeds(context: &str, feeds: &BTreeMap<String, FeedConfig>) -> Result<()> {
    for (id, feed) in feeds {
        if id.is_empty()
            || !id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            bail!("{context}feed id {id:?} may only contain letters, digits, '-' and '_'");
        }
        if !(feed.url.starts_with("http://") || feed.url.starts_with("https://")) {
            bail!("{context}feed {id}: url must start with http:// or https://");
        }
        if feed.refresh_interval.is_some_and(|i| i < 60) {
            bail!("{context}feed {id}: refresh_interval must be at least 60 seconds");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_config() {
        let config = Config::parse(
            r#"
            listen = "0.0.0.0:9000"
            database = "/var/lib/pressbox/pressbox.db"
            refresh_interval = 600
            max_items_per_feed = 100

            [feeds.lwn]
            url = "https://lwn.net/headlines/rss"
            tags = ["linux"]

            [feeds.nixos-blog]
            url = "https://nixos.org/blog/announcements-rss.xml"
            title = "NixOS"
            refresh_interval = 3600
            "#,
        )
        .unwrap();
        assert_eq!(config.listen.port(), 9000);
        assert_eq!(config.feeds.len(), 2);
        assert_eq!(config.feeds["lwn"].tags, ["linux"]);
        assert_eq!(config.feeds["nixos-blog"].title.as_deref(), Some("NixOS"));
        assert_eq!(config.feeds["nixos-blog"].refresh_interval, Some(3600));
        assert_eq!(
            config.backup_dir(),
            PathBuf::from("/var/lib/pressbox/backups")
        );
    }

    #[test]
    fn defaults_apply() {
        let config = Config::parse("").unwrap();
        assert_eq!(config.listen, default_listen());
        assert_eq!(config.refresh_interval, 1800);
        assert!(config.feeds.is_empty());
    }

    #[test]
    fn users_combine_shared_and_own_feeds() {
        let config = Config::parse(
            r#"
            [feeds.lwn]
            url = "https://lwn.net/headlines/rss"

            [users.chris]
            admin = true
            [users.chris.feeds.hn]
            url = "https://news.ycombinator.com/rss"

            [users.anna]
            "#,
        )
        .unwrap();
        let users = config.users();
        assert_eq!(users.keys().collect::<Vec<_>>(), ["anna", "chris"]);
        assert!(users["chris"].admin && !users["anna"].admin);
        assert_eq!(
            users["chris"].feeds.keys().collect::<Vec<_>>(),
            ["hn", "lwn"]
        );
        assert_eq!(users["anna"].feeds.keys().collect::<Vec<_>>(), ["lwn"]);
        assert_eq!(config.feed_count(), 2);

        let single = Config::parse("[feeds.a]\nurl = \"https://a.example/\"").unwrap();
        let users = single.users();
        assert_eq!(users.keys().collect::<Vec<_>>(), [DEFAULT_USER]);
        assert!(users[DEFAULT_USER].admin);
    }

    #[test]
    fn loads_files_and_tokens() {
        let dir = std::env::temp_dir().join(format!("pressboxd-config-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pressbox.toml");
        let token = dir.join("token");
        std::fs::write(&token, "  s3cret \n").unwrap();
        std::fs::write(
            &path,
            format!(
                "database = \"/var/lib/pressbox/db.sqlite\"\ntoken_file = {:?}\n",
                token.display().to_string()
            ),
        )
        .unwrap();
        let config = Config::load(&path).unwrap();
        assert_eq!(config.token().unwrap().as_deref(), Some("s3cret"));
        assert_eq!(
            config.backup_dir(),
            PathBuf::from("/var/lib/pressbox/backups")
        );

        std::fs::write(&token, "\n").unwrap();
        let error = config.token().unwrap_err();
        assert!(format!("{error}").ends_with("is empty"), "{error}");
        std::fs::remove_file(&token).unwrap();
        let error = config.token().unwrap_err();
        assert!(
            format!("{error}").starts_with("reading token file"),
            "{error}"
        );

        std::fs::write(&path, "refresh_interval = \"soon\"").unwrap();
        let error = Config::load(&path).unwrap_err();
        assert!(format!("{error}").starts_with("parsing "), "{error}");
        let error = Config::load(&dir.join("missing.toml")).unwrap_err();
        assert!(format!("{error}").starts_with("reading "), "{error}");
        std::fs::remove_dir_all(dir).unwrap();

        let explicit =
            Config::parse("control_socket = \"/run/x.sock\"\nbackup_dir = \"/srv/backups\"")
                .unwrap();
        assert_eq!(explicit.control_socket(), PathBuf::from("/run/x.sock"));
        assert_eq!(explicit.backup_dir(), PathBuf::from("/srv/backups"));
        assert_eq!(
            Config::parse("").unwrap().backup_dir(),
            PathBuf::from("./backups")
        );
        assert!(
            Config::parse("max_age_days = 0").is_err(),
            "zero days would delete everything"
        );
        assert!(Config::parse("max_items_per_feed = 0").is_err());
    }

    #[test]
    fn rejects_bad_users() {
        let shared_clash =
            "[feeds.a]\nurl = \"https://a/\"\n[users.x.feeds.a]\nurl = \"https://b/\"";
        assert!(Config::parse(shared_clash).is_err());
        let same_url = "[feeds.a]\nurl = \"https://a/\"\n[users.x.feeds.b]\nurl = \"https://a/\"";
        assert!(Config::parse(same_url).is_err());
        assert!(Config::parse("[users.\"a b\"]").is_err());
        assert!(Config::parse("[users.x.feeds.\"a/b\"]\nurl = \"https://a/\"").is_err());
        assert!(Config::parse("[users.x]\nadmin = 1").is_err());
    }

    #[test]
    fn rejects_bad_input() {
        assert!(Config::parse("refresh_interval = 5").is_err());
        assert!(Config::parse("[feeds.\"a b\"]\nurl = \"https://x\"").is_err());
        assert!(Config::parse("[feeds.a]\nurl = \"ftp://x\"").is_err());
        assert!(Config::parse("typo = 1").is_err());
        assert!(Config::parse("[feeds.a]\nurl = \"https://x\"\nrefresh_interval = 5").is_err());
    }
}
