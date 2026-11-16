// SPDX-FileCopyrightText: 2026 Chris Palmer <chris@red-oxide.org>
// SPDX-License-Identifier: GPL-3.0-only

use std::{
    io::Write,
    path::{Path, PathBuf},
    process,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use ratatui::crossterm::event::{self, Event, KeyEventKind};

mod app;
mod client;
mod player;
#[cfg(test)]
mod test_server;
mod ui;

use app::{Action, App};
use client::{Client, ClientConfig};

/// Terminal client for the pressbox feed aggregator.
#[derive(Parser)]
#[command(version)]
struct Args {
    /// Server URL, e.g. `http://127.0.0.1:8470`. Overrides the config file.
    #[arg(short, long, env = "PRESSBOX_SERVER", global = true)]
    server: Option<String>,
    /// File holding the API token. Overrides the config file.
    #[arg(short, long, env = "PRESSBOX_TOKEN_FILE", global = true)]
    token_file: Option<PathBuf>,
    /// Client config (JSON). Defaults to `~/.config/pressbox/client.json`.
    #[arg(short, long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Pair this machine with the server: shows a code to approve, then
    /// saves this device's token.
    Pair {
        /// Name for this device. Defaults to the hostname.
        #[arg(long)]
        name: Option<String>,
    },
    /// Approve another device's pairing code (or list pending ones).
    Approve {
        code: Option<String>,
        /// Add the device to this user instead of yours (admins only).
        #[arg(long)]
        user: Option<String>,
    },
    /// Show which user and device this machine is signed in as.
    Whoami,
}

// Not measured: process wiring around `run`.
#[cfg(not(tarpaulin_include))]
fn main() {
    if let Err(e) = run(Args::parse()) {
        eprintln!("pressbox: {e:#}");
        std::process::exit(1);
    }
}

fn hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .or_else(|| std::env::var("HOSTNAME").ok())
        .map(|h| h.trim().to_owned())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "pressbox".to_owned())
}

/// How often `pair` asks whether the code has been approved.
const PAIR_POLL: Duration = Duration::from_secs(2);

/// Shows a pairing code, waits for its approval (asking every `poll`) and
/// saves the token.
fn pair(
    server: &str,
    token_path: &Path,
    name: Option<String>,
    poll: Duration,
    out: &mut impl Write,
) -> Result<()> {
    let client = Client::new(server, None)?;
    let name = name.unwrap_or_else(hostname);
    let start = client.pair_start(&name)?;
    let code = &start.code;
    writeln!(out, "Pairing {name:?} with {server}.\n")?;
    writeln!(out, "    {code}\n")?;
    writeln!(
        out,
        "Approve it on the server with `pressboxctl approve {code}`,"
    )?;
    writeln!(
        out,
        "or from a paired device (`pressbox approve {code}`, or the Apple or Android app)."
    )?;
    out.flush()?;
    let deadline = std::time::Instant::now() + Duration::from_secs(start.expires_in);
    while std::time::Instant::now() < deadline {
        std::thread::sleep(poll);
        if let Some(approved) = client.pair_poll(&start.poll_id)? {
            if let Some(dir) = token_path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            write_private(token_path, &approved.token)?;
            writeln!(
                out,
                "\nPaired as {:?}. Token saved to {}.",
                approved.name,
                token_path.display()
            )?;
            return Ok(());
        }
    }
    anyhow::bail!("the code expired before it was approved; run `pressbox pair` again")
}

/// Writes a file only the owner can read.
fn write_private(path: &Path, contents: &str) -> Result<()> {
    use std::{
        io::Write,
        os::unix::fs::{OpenOptionsExt, PermissionsExt},
    };
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("writing {}", path.display()))?;
    // `mode` only counts when the file is created: one already there (from
    // a hand-written token, say) keeps its own until told otherwise.
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    writeln!(file, "{contents}")?;
    Ok(())
}

/// Where to reach the server and find the token: flags and environment
/// first, then `client.json`, then defaults.
fn settings(
    server: Option<String>,
    token_file: Option<PathBuf>,
    config: &ClientConfig,
) -> Result<(String, PathBuf)> {
    let server = server
        .or_else(|| config.server.clone())
        .unwrap_or_else(|| "http://127.0.0.1:8470".to_owned());
    let token_path = token_file
        .or_else(|| config.token_file.clone())
        .or_else(ClientConfig::default_token_path)
        .context("no token file configured and no state directory")?;
    Ok((server, token_path))
}

fn read_token(path: &Path) -> Result<String> {
    match std::fs::read_to_string(path) {
        Ok(t) => Ok(t.trim().to_owned()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => anyhow::bail!(
            "no token at {}: run `pressbox pair` to pair this machine",
            path.display()
        ),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

fn whoami(client: &Client, server: &str, out: &mut impl Write) -> Result<()> {
    let me = client.me()?;
    let admin = if me.admin { " (admin)" } else { "" };
    let device = me.device.unwrap_or_else(|| "the static token".to_owned());
    writeln!(out, "{}{admin} on {device}, at {server}", me.user)?;
    match client.version() {
        Ok(v) => writeln!(out, "pressboxd {} (API v{})", v.version, v.api)?,
        // Servers from before the endpoint existed.
        Err(_) => writeln!(out, "pressboxd version unknown")?,
    }
    Ok(())
}

/// Approves `code`, or lists the codes waiting without one.
fn approve(
    client: &Client,
    code: Option<String>,
    user: Option<String>,
    out: &mut impl Write,
) -> Result<()> {
    match code {
        Some(code) => {
            let approved = client.approve(&code, user)?;
            writeln!(out, "paired {} for {}", approved.name, approved.user)?;
        }
        None => {
            let pending = client.pairings()?;
            if pending.is_empty() {
                writeln!(out, "no pairing requests")?;
            }
            for p in pending {
                writeln!(out, "{}  {}", p.code, p.name)?;
            }
        }
    }
    Ok(())
}

// Not measured: reads the real config and token and takes over the
// terminal; the commands and app it drives are tested on their own.
#[cfg(not(tarpaulin_include))]
fn run(args: Args) -> Result<()> {
    let config = match args.config.or_else(ClientConfig::default_path) {
        Some(path) => ClientConfig::load(&path)?,
        None => ClientConfig::default(),
    };
    let (server, token_path) = settings(args.server, args.token_file, &config)?;
    let out = &mut std::io::stdout().lock();
    let client = match args.command {
        Some(Command::Pair { name }) => {
            return pair(&server, &token_path, name, PAIR_POLL, out);
        }
        _ => Client::new(&server, Some(read_token(&token_path)?))?,
    };
    match args.command {
        Some(Command::Whoami) => return whoami(&client, &server, out),
        Some(Command::Approve { code, user }) => return approve(&client, code, user, out),
        Some(Command::Pair { .. }) | None => {}
    }

    let mut app = App::new();
    app.player = Box::new(player::Mpv::new(config.player.as_deref().unwrap_or("mpv")));
    app.reload(&client);
    let result = ratatui::run(|terminal| -> Result<()> {
        let mut reloaded = Instant::now();
        let mut ticked = Instant::now();
        loop {
            // While playing, follow the player about once a second.
            if app.playing.is_some() && ticked.elapsed() >= TICK {
                app.tick(&client);
                ticked = Instant::now();
            }
            terminal.draw(|frame| ui::draw(frame, &app))?;
            let wait = if app.playing.is_some() { TICK } else { IDLE };
            if !event::poll(wait)? {
                // Idle for a minute: pick up whatever the server fetched.
                if reloaded.elapsed() >= IDLE {
                    app.reload(&client);
                    reloaded = Instant::now();
                }
                continue;
            }
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match app.handle(key.code, &client) {
                Action::None => {}
                Action::Quit => return Ok(()),
                Action::Open(url) => open(OPENER, &url, &mut app),
            }
        }
    });
    // Saves where playback stopped.
    app.stop(&client)?;
    result
}

/// How often the player is followed while playing, and how long the app
/// waits idle before reloading.
const TICK: Duration = Duration::from_secs(1);
const IDLE: Duration = Duration::from_secs(60);

/// What opens a link in the browser.
const OPENER: &str = if cfg!(target_os = "macos") {
    "open"
} else {
    "xdg-open"
};

/// Opens `url` with `opener` without waiting, and says so in the status line.
fn open(opener: &str, url: &str, app: &mut App) {
    let spawned = process::Command::new(opener)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    app.status = match spawned {
        Ok(_) => format!("opened {url}"),
        Err(e) => format!("error: could not run {opener}: {e}"),
    };
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::test_server::server;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pressbox-tui-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn flags_beat_the_config_file() {
        let config = ClientConfig {
            server: Some("https://from.config".into()),
            token_file: Some("/config/token".into()),
            player: None,
        };
        assert_eq!(
            settings(None, None, &config).unwrap(),
            (
                "https://from.config".to_owned(),
                PathBuf::from("/config/token")
            )
        );
        assert_eq!(
            settings(
                Some("https://flag".into()),
                Some("/flag/token".into()),
                &config
            )
            .unwrap(),
            ("https://flag".to_owned(), PathBuf::from("/flag/token"))
        );
        let (server, _) = settings(None, Some("/t".into()), &ClientConfig::default()).unwrap();
        assert_eq!(server, "http://127.0.0.1:8470");
    }

    #[test]
    fn pairs_and_saves_a_private_token() {
        let (base, requests) = server(vec![
            (201, r#"{"code":"K7W-4QX","poll_id":"p1","expires_in":60}"#),
            (202, r#"{"status":"pending"}"#),
            (200, r#"{"token":"secret","name":"laptop"}"#),
        ]);
        let dir = temp_dir("pair");
        let token = dir.join("state/pressbox/token");
        let mut out = Vec::new();
        pair(
            &base,
            &token,
            Some("laptop".into()),
            Duration::from_millis(1),
            &mut out,
        )
        .unwrap();
        let out = String::from_utf8(out).unwrap();
        assert!(out.contains("    K7W-4QX\n"), "{out}");
        assert!(out.contains("pressboxctl approve K7W-4QX"), "{out}");
        assert!(out.contains("Paired as \"laptop\""), "{out}");
        assert!(requests.recv().unwrap().starts_with("post /api/v1/pair "));
        assert_eq!(read_token(&token).unwrap(), "secret");
        let mode = std::fs::metadata(&token).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "only the owner can read it");

        // A token file that was readable by others is closed when rewritten.
        std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_private(&token, "newer").unwrap();
        assert_eq!(read_token(&token).unwrap(), "newer");
        let mode = std::fs::metadata(&token).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_denied_pairing_is_an_error() {
        let (base, _requests) = server(vec![
            (201, r#"{"code":"K7W-4QX","poll_id":"p1","expires_in":60}"#),
            (404, "{}"),
        ]);
        let token = temp_dir("denied").join("token");
        let error = pair(&base, &token, None, Duration::ZERO, &mut Vec::new()).unwrap_err();
        assert!(
            format!("{error:#}").contains("denied or expired"),
            "{error:#}"
        );
        assert!(!token.exists());
    }

    #[test]
    fn an_unapproved_code_expires() {
        let (base, _requests) = server(vec![(
            201,
            r#"{"code":"K7W-4QX","poll_id":"p1","expires_in":0}"#,
        )]);
        let token = temp_dir("expired").join("token");
        let error = pair(&base, &token, None, Duration::ZERO, &mut Vec::new()).unwrap_err();
        assert!(
            format!("{error:#}").starts_with("the code expired"),
            "{error:#}"
        );
    }

    #[test]
    fn opens_links_and_says_when_it_cant() {
        let mut app = App::new();
        open("true", "https://example.com/1", &mut app);
        assert_eq!(app.status, "opened https://example.com/1");
        open("/nonexistent/opener", "https://example.com/1", &mut app);
        assert!(
            app.status
                .starts_with("error: could not run /nonexistent/opener"),
            "{}",
            app.status
        );
        assert!(!hostname().is_empty());
    }

    #[test]
    fn a_missing_token_says_to_pair() {
        let error = read_token(Path::new("/nonexistent/token")).unwrap_err();
        assert!(
            format!("{error:#}").contains("run `pressbox pair`"),
            "{error:#}"
        );
        let error = read_token(Path::new("/")).unwrap_err();
        assert!(format!("{error:#}").starts_with("reading /"), "{error:#}");
    }

    #[test]
    fn whoami_and_approve() {
        let (base, _requests) = server(vec![
            (200, r#"{"user":"chris","admin":true,"device":"laptop"}"#),
            (200, r#"{"version":"0.1.0","api":1}"#),
            (200, r#"{"user":"anna","admin":false,"device":null}"#),
            (404, "{}"),
            (200, r#"{"name":"phone","user":"anna"}"#),
            (200, r#"[{"code":"K7W-4QX","name":"tablet","requested":0}]"#),
            (200, "[]"),
        ]);
        let client = Client::new(&base, Some("t".into())).unwrap();
        let output = |f: &mut dyn FnMut(&mut Vec<u8>) -> Result<()>| {
            let mut out = Vec::new();
            f(&mut out).unwrap();
            String::from_utf8(out).unwrap()
        };
        assert_eq!(
            output(&mut |out| whoami(&client, "https://rss", out)),
            "chris (admin) on laptop, at https://rss\npressboxd 0.1.0 (API v1)\n"
        );
        assert_eq!(
            output(&mut |out| whoami(&client, "https://rss", out)),
            "anna on the static token, at https://rss\npressboxd version unknown\n"
        );
        assert_eq!(
            output(&mut |out| approve(&client, Some("K7W-4QX".into()), None, out)),
            "paired phone for anna\n"
        );
        assert_eq!(
            output(&mut |out| approve(&client, None, None, out)),
            "K7W-4QX  tablet\n"
        );
        assert_eq!(
            output(&mut |out| approve(&client, None, None, out)),
            "no pairing requests\n"
        );
    }
}
