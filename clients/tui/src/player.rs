// SPDX-FileCopyrightText: 2026 Chris Palmer <chris@red-oxide.org>
// SPDX-License-Identifier: GPL-3.0-only

//! Playing podcasts with mpv, driven over its JSON IPC socket.
//!
//! mpv runs as its own process, without the terminal (which the reader
//! owns): audio plays in the background and video opens mpv's window.
//! `--keep-open` leaves it paused at the end, so the app can tell a
//! finished episode from one the user closed.

use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

/// What the player is doing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayerState {
    /// Seconds into the media.
    pub position: f64,
    pub duration: Option<f64>,
    pub paused: bool,
    pub speed: f64,
    /// Played to the end.
    pub ended: bool,
}

/// The player the app drives, so tests can swap in a fake.
pub trait Player {
    /// Starts playing `url` from `start` seconds, replacing whatever was
    /// playing. `video` shows a window; otherwise only the audio plays.
    fn load(&mut self, url: &str, title: &str, start: u32, video: bool) -> Result<()>;
    /// `None` when nothing is playing (never started, stopped, or the user
    /// closed the window).
    fn state(&mut self) -> Option<PlayerState>;
    fn toggle_pause(&mut self) -> Result<()>;
    /// Seeks by `seconds`, or to `seconds` if `absolute`.
    fn seek(&mut self, seconds: f64, absolute: bool) -> Result<()>;
    fn set_speed(&mut self, speed: f64) -> Result<()>;
    fn stop(&mut self);
}

/// A connection to mpv's IPC socket: one JSON command per line, answered by
/// a line with the same `request_id`. Event lines in between are skipped.
pub struct Ipc {
    reader: BufReader<UnixStream>,
    next_id: u64,
}

impl Ipc {
    pub fn connect(path: &Path) -> Result<Self> {
        let stream = UnixStream::connect(path)?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        Ok(Self {
            reader: BufReader::new(stream),
            next_id: 1,
        })
    }

    /// Runs a command and returns its `data`.
    pub fn command(&mut self, args: &Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let mut line = json!({ "command": args, "request_id": id }).to_string();
        line.push('\n');
        self.reader.get_mut().write_all(line.as_bytes())?;
        loop {
            let mut reply = String::new();
            if self.reader.read_line(&mut reply)? == 0 {
                bail!("mpv went away");
            }
            let Ok(reply) = serde_json::from_str::<Value>(&reply) else {
                continue;
            };
            if reply["request_id"] != id {
                continue;
            }
            return match reply["error"].as_str() {
                Some("success") => Ok(reply["data"].clone()),
                error => bail!("mpv: {}", error.unwrap_or("no answer")),
            };
        }
    }

    fn get(&mut self, property: &str) -> Option<Value> {
        self.command(&json!(["get_property", property])).ok()
    }

    /// The player's state. `None` if mpv doesn't answer.
    pub fn state(&mut self) -> Option<PlayerState> {
        let paused = self.get("pause")?.as_bool().unwrap_or(false);
        // Unavailable while the file is still loading.
        let position = self.get("time-pos").and_then(|v| v.as_f64()).unwrap_or(0.0);
        Some(PlayerState {
            position,
            duration: self.get("duration").and_then(|v| v.as_f64()),
            paused,
            speed: self.get("speed").and_then(|v| v.as_f64()).unwrap_or(1.0),
            ended: self.get("eof-reached").and_then(|v| v.as_bool()) == Some(true),
        })
    }
}

/// mpv in its own process.
pub struct Mpv {
    program: String,
    socket: PathBuf,
    child: Option<Child>,
    ipc: Option<Ipc>,
}

/// Where mpv's socket goes: the user's runtime directory (`runtime`, from
/// `XDG_RUNTIME_DIR`), which only they can reach. Whoever can connect to the
/// socket controls mpv, so the shared temporary directory, where another
/// user could have put a socket of the same name first, is only the
/// fallback.
fn socket_dir(runtime: Option<std::ffi::OsString>) -> PathBuf {
    runtime
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .unwrap_or_else(std::env::temp_dir)
}

impl Mpv {
    pub fn new(program: &str) -> Self {
        let socket = socket_dir(std::env::var_os("XDG_RUNTIME_DIR"))
            .join(format!("pressbox-mpv-{}.sock", std::process::id()));
        Self {
            program: program.to_owned(),
            socket,
            child: None,
            ipc: None,
        }
    }

    /// mpv's arguments for playing `url`.
    pub fn args(&self, url: &str, title: &str, start: u32, video: bool) -> Vec<String> {
        let mut args = vec![
            "--no-terminal".to_owned(),
            "--keep-open=yes".to_owned(),
            format!("--input-ipc-server={}", self.socket.display()),
            format!("--start={start}"),
            format!("--force-media-title={title}"),
        ];
        args.push(if video {
            "--force-window=immediate".to_owned()
        } else {
            "--no-video".to_owned()
        });
        args.push("--".to_owned());
        args.push(url.to_owned());
        args
    }

    fn ipc(&mut self) -> Result<&mut Ipc> {
        self.ipc.as_mut().context("nothing is playing")
    }
}

impl Player for Mpv {
    // Not measured: starts mpv, which tests can't count on; the IPC it
    // talks is tested against a fake socket.
    #[cfg(not(tarpaulin_include))]
    fn load(&mut self, url: &str, title: &str, start: u32, video: bool) -> Result<()> {
        self.stop();
        let _ = std::fs::remove_file(&self.socket);
        let child = Command::new(&self.program)
            .args(self.args(url, title, start, video))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("could not run {}; is mpv installed?", self.program))?;
        self.child = Some(child);
        // mpv makes the socket once it has started.
        for _ in 0..50 {
            if let Ok(ipc) = Ipc::connect(&self.socket) {
                self.ipc = Some(ipc);
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        self.stop();
        bail!("mpv didn't start")
    }

    fn state(&mut self) -> Option<PlayerState> {
        let state = self.ipc.as_mut()?.state();
        if state.is_none() {
            self.stop();
        }
        state
    }

    fn toggle_pause(&mut self) -> Result<()> {
        self.ipc()?.command(&json!(["cycle", "pause"]))?;
        Ok(())
    }

    fn seek(&mut self, seconds: f64, absolute: bool) -> Result<()> {
        let how = if absolute { "absolute" } else { "relative" };
        self.ipc()?.command(&json!(["seek", seconds, how]))?;
        Ok(())
    }

    fn set_speed(&mut self, speed: f64) -> Result<()> {
        self.ipc()?
            .command(&json!(["set_property", "speed", speed]))?;
        Ok(())
    }

    fn stop(&mut self) {
        if let Some(mut ipc) = self.ipc.take() {
            let _ = ipc.command(&json!(["quit"]));
        }
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = std::fs::remove_file(&self.socket);
    }
}

impl Drop for Mpv {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
pub mod tests {
    use std::{
        os::unix::net::UnixListener,
        sync::{Arc, Mutex},
    };

    use super::*;

    /// A fake mpv socket: answers `get_property` from `properties`, records
    /// every other command, and sends an event line before each answer.
    pub fn fake_mpv(path: &Path, properties: Value) -> Arc<Mutex<Vec<Value>>> {
        let _ = std::fs::remove_file(path);
        let listener = UnixListener::bind(path).unwrap();
        let commands = Arc::new(Mutex::new(Vec::new()));
        let seen = commands.clone();
        std::thread::spawn(move || {
            let Ok((stream, _)) = listener.accept() else {
                return;
            };
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap_or(0) > 0 {
                let request: Value = serde_json::from_str(&line).unwrap();
                line.clear();
                let id = request["request_id"].clone();
                let args = request["command"].clone();
                let reply = if args[0] == "get_property" {
                    match properties.get(args[1].as_str().unwrap()) {
                        Some(v) => json!({ "data": v, "error": "success", "request_id": id }),
                        None => json!({ "error": "property unavailable", "request_id": id }),
                    }
                } else {
                    seen.lock().unwrap().push(args.clone());
                    if args[0] == "quit" {
                        return;
                    }
                    json!({ "data": null, "error": "success", "request_id": id })
                };
                let out = reader.get_mut();
                let _ = writeln!(out, "not json\n{{\"event\":\"idle\"}}");
                let _ = writeln!(out, "{reply}");
            }
        });
        commands
    }

    fn socket(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("pressbox-test-{name}-{}.sock", std::process::id()))
    }

    #[test]
    fn the_socket_goes_in_the_runtime_directory() {
        assert_eq!(
            socket_dir(Some("/run/user/1000".into())),
            PathBuf::from("/run/user/1000")
        );
        // Unset, or not a place: the temporary directory will have to do.
        assert_eq!(socket_dir(None), std::env::temp_dir());
        assert_eq!(socket_dir(Some("".into())), std::env::temp_dir());
        assert_eq!(socket_dir(Some("relative".into())), std::env::temp_dir());
        assert!(Mpv::new("mpv").socket.is_absolute());
    }

    #[test]
    fn talks_to_mpv() {
        let path = socket("ipc");
        let commands = fake_mpv(
            &path,
            json!({ "pause": false, "time-pos": 61.5, "duration": 3600.0, "speed": 1.5,
                    "eof-reached": false }),
        );
        let mut mpv = Mpv::new("mpv");
        assert!(mpv.toggle_pause().is_err(), "nothing playing yet");
        assert_eq!(mpv.state(), None);
        mpv.socket = path.clone();
        mpv.ipc = Some(Ipc::connect(&path).unwrap());
        assert_eq!(
            mpv.state(),
            Some(PlayerState {
                position: 61.5,
                duration: Some(3600.0),
                paused: false,
                speed: 1.5,
                ended: false,
            })
        );
        mpv.toggle_pause().unwrap();
        mpv.seek(30.0, false).unwrap();
        mpv.seek(5.0, true).unwrap();
        mpv.set_speed(2.0).unwrap();
        mpv.stop();
        assert_eq!(
            *commands.lock().unwrap(),
            [
                json!(["cycle", "pause"]),
                json!(["seek", 30.0, "relative"]),
                json!(["seek", 5.0, "absolute"]),
                json!(["set_property", "speed", 2.0]),
                json!(["quit"]),
            ]
        );
        assert!(!path.exists(), "the socket is removed");
    }

    #[test]
    fn a_loading_file_and_a_vanished_mpv() {
        let path = socket("loading");
        fake_mpv(&path, json!({ "pause": true }));
        let mut ipc = Ipc::connect(&path).unwrap();
        let state = ipc.state().unwrap();
        assert_eq!(
            (state.position, state.duration, state.speed),
            (0.0, None, 1.0)
        );
        assert!(state.paused && !state.ended);
        assert_eq!(
            ipc.command(&json!(["get_property", "nope"]))
                .unwrap_err()
                .to_string(),
            "mpv: property unavailable"
        );
        ipc.command(&json!(["quit"])).unwrap_err();
        assert_eq!(ipc.state(), None, "mpv went away");

        // The app's player stops when mpv stops answering.
        let path = socket("gone");
        fake_mpv(&path, json!({}));
        let mut mpv = Mpv::new("mpv");
        mpv.ipc = Some(Ipc::connect(&path).unwrap());
        assert_eq!(mpv.state(), None);
        assert!(mpv.ipc.is_none());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn answers_without_an_error_field_fail() {
        let path = socket("odd");
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            writeln!(reader.get_mut(), "{{\"request_id\":1}}").unwrap();
        });
        let mut ipc = Ipc::connect(&path).unwrap();
        assert_eq!(
            ipc.command(&json!(["x"])).unwrap_err().to_string(),
            "mpv: no answer"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn stopping_ends_the_process() {
        let mut mpv = Mpv::new("mpv");
        mpv.child = Some(Command::new("sleep").arg("30").spawn().unwrap());
        mpv.stop();
        assert!(mpv.child.is_none());
    }

    #[test]
    fn mpv_arguments() {
        let mpv = Mpv::new("mpv");
        let audio = mpv.args("https://x/a.mp3", "Ep 1", 90, false);
        assert!(audio.contains(&"--no-video".to_owned()));
        assert!(audio.contains(&"--start=90".to_owned()));
        assert!(audio.contains(&"--force-media-title=Ep 1".to_owned()));
        assert_eq!(audio[audio.len() - 2..], ["--", "https://x/a.mp3"]);
        let video = mpv.args("https://x/v.mp4", "Ep 1", 0, true);
        assert!(video.contains(&"--force-window=immediate".to_owned()));
        assert!(!video.contains(&"--no-video".to_owned()));
    }
}
