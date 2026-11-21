// SPDX-FileCopyrightText: 2026 Chris Palmer <chris@red-oxide.org>
// SPDX-License-Identifier: AGPL-3.0-only

//! Admin API on a Unix socket, for `pressboxctl`. There is no token: access is
//! controlled by the socket's file permissions (the service user and root).
//!
//! | Method | Path                           | Body            | Response           |
//! |--------|--------------------------------|-----------------|--------------------|
//! | GET    | /admin/status                  |                 | `Status`           |
//! | GET    | /admin/users                   |                 | `[UserInfo]`       |
//! | GET    | /admin/devices                 |                 | `[Device]`         |
//! | POST   | /admin/devices                 | `NewDeviceRequest` | `NewDevice`     |
//! | DELETE | /admin/devices/{name}          |                 | 204                |
//! | GET    | /admin/pairings                |                 | `[PendingPairing]` |
//! | POST   | /admin/pairings/{code}/approve | `ApproveRequest`| `Approved`         |
//! | DELETE | /admin/pairings/{code}         |                 | 204                |
//! | POST   | /admin/backup                  | `BackupRequest` | `BackupResult`     |
//! | GET    | /admin/opml?user=NAME          |                 | OPML document      |
//! | POST   | /admin/refresh                 |                 | 202                |

use std::{
    fmt::Write as _,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result};
use axum::{
    Json, Router,
    extract::{Path as UrlPath, State},
    http::{StatusCode, header},
    response::IntoResponse,
    routing::{delete, get, post},
};
use pressbox_api::{
    ApproveRequest, Approved, BackupRequest, BackupResult, Device, Feed, NewDevice,
    NewDeviceRequest, Status, UserInfo,
};

use crate::{
    AppState,
    api::{self, ApiError, ApiResult, found},
    db::AuthUser,
    fetch::now,
    pairing,
};

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/admin/status", get(status))
        .route("/admin/users", get(users))
        .route("/admin/devices", get(devices).post(add_device))
        .route("/admin/devices/{name}", delete(remove_device))
        .route("/admin/pairings", get(api::pairings))
        .route("/admin/pairings/{code}/approve", post(approve))
        .route("/admin/pairings/{code}", delete(api::deny))
        .route("/admin/backup", post(backup))
        .route("/admin/opml", get(opml))
        .route(
            "/admin/refresh",
            post(|State(state): State<Arc<AppState>>| async move {
                state.request_refresh().await;
                StatusCode::ACCEPTED
            }),
        )
        .with_state(state)
}

/// Binds the control socket, replacing a stale one, usable only by us.
///
/// A socket is created with the process's umask, and anyone who may write
/// to it may connect. So it is bound inside a new directory only we can
/// enter, made private there, and only then moved into place: there is no
/// moment when it is at `path` with looser permissions, whatever the umask.
pub fn bind(path: &Path) -> Result<tokio::net::UnixListener> {
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            return Err(e).with_context(|| format!("removing stale {}", path.display()));
        }
        _ => {}
    }
    let name = path
        .file_name()
        .with_context(|| format!("{} names no file", path.display()))?;
    let mut staging_name = std::ffi::OsString::from(".");
    staging_name.push(name);
    staging_name.push(format!(".{}", std::process::id()));
    let staging = dir.join(staging_name);
    // Left over from a crash of an earlier process with this id.
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&staging)
        .with_context(|| format!("creating {}", staging.display()))?;
    let bound = (|| {
        let inside = staging.join("control.sock");
        let listener = tokio::net::UnixListener::bind(&inside)
            .with_context(|| format!("binding control socket {}", path.display()))?;
        std::fs::set_permissions(&inside, std::fs::Permissions::from_mode(0o600))?;
        std::fs::rename(&inside, path)
            .with_context(|| format!("moving the control socket to {}", path.display()))?;
        Ok(listener)
    })();
    let _ = std::fs::remove_dir_all(&staging);
    bound
}

async fn status(State(state): State<Arc<AppState>>) -> ApiResult<Json<Status>> {
    let counts = state.read(|db| db.counts()).await?;
    Ok(Json(Status {
        feeds: counts.feeds,
        archived_feeds: counts.archived_feeds,
        failing_feeds: counts.failing_feeds,
        items: counts.items,
        users: counts.users,
        devices: counts.devices,
        pending_pairings: u32::try_from(state.pairings.count(now())).unwrap_or(u32::MAX),
    }))
}

async fn users(State(state): State<Arc<AppState>>) -> ApiResult<Json<Vec<UserInfo>>> {
    Ok(Json(state.read(|db| db.users()).await?))
}

async fn devices(State(state): State<Arc<AppState>>) -> ApiResult<Json<Vec<Device>>> {
    Ok(Json(state.read(|db| db.devices()).await?))
}

/// The named user, or the only user when none is named.
async fn pick_user(state: &Arc<AppState>, name: Option<String>) -> ApiResult<AuthUser> {
    let named = name.is_some();
    let user = state
        .read(move |db| match name {
            Some(name) => db.user(&name),
            None => db.only_user(),
        })
        .await?;
    user.ok_or(ApiError::BadRequest(if named {
        "no such user"
    } else {
        "there is more than one user; say which with --user"
    }))
}

async fn add_device(
    State(state): State<Arc<AppState>>,
    Json(req): Json<NewDeviceRequest>,
) -> ApiResult<Json<NewDevice>> {
    let name = pairing::device_name(&req.name).ok_or(ApiError::BadRequest(
        "name must be 1-40 letters, digits, '-', '_' or '.'",
    ))?;
    let user = pick_user(&state, req.user).await?;
    let device = state
        .write(move |db| pairing::create_device(db, &user, &name, now()))
        .await?;
    Ok(Json(device))
}

async fn approve(
    State(state): State<Arc<AppState>>,
    UrlPath(code): UrlPath<String>,
    body: Option<Json<ApproveRequest>>,
) -> ApiResult<Json<Approved>> {
    let wanted = body.and_then(|Json(b)| b.user);
    let user = pick_user(&state, wanted).await?;
    api::approve_for(&state, code, user).await
}

async fn remove_device(
    State(state): State<Arc<AppState>>,
    UrlPath(name): UrlPath<String>,
) -> ApiResult<StatusCode> {
    let target = name.clone();
    let removed = state.write(move |db| db.remove_device(&target)).await?;
    if removed {
        tracing::info!("revoked device {name:?}");
    }
    found(removed)
}

async fn backup(
    State(state): State<Arc<AppState>>,
    Json(req): Json<BackupRequest>,
) -> ApiResult<Json<BackupResult>> {
    let dir = state.backup_dir.clone();
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    // Milliseconds keep names unique; they still sort by time.
    let name = format!(
        "pressbox-{}.db",
        jiff::ZonedDateTime::now().strftime("%Y%m%dT%H%M%S%.3f")
    );
    let path = dir.join(name);
    let target = path.clone();
    state.write(move |db| db.backup_to(&target)).await?;
    let removed = prune_backups(&dir, req.keep.max(1))?;
    tracing::info!("backed up to {}", path.display());
    Ok(Json(BackupResult {
        path: path.display().to_string(),
        removed: removed.iter().map(|p| p.display().to_string()).collect(),
    }))
}

/// Deletes all but the newest `keep` backups in `dir`.
fn prune_backups(dir: &Path, keep: u32) -> Result<Vec<PathBuf>> {
    let mut backups: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("pressbox-") && n.ends_with(".db"))
        })
        .collect();
    // The names sort by time.
    backups.sort_unstable_by(|a, b| b.cmp(a));
    let old = backups.split_off(backups.len().min(keep as usize));
    for path in &old {
        std::fs::remove_file(path)?;
    }
    Ok(old)
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[derive(serde::Deserialize)]
struct OpmlQuery {
    user: Option<String>,
}

async fn opml(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(query): axum::extract::Query<OpmlQuery>,
) -> ApiResult<impl IntoResponse> {
    let user = pick_user(&state, query.user).await?;
    let feeds = state.read(move |db| db.feeds(user.id)).await?;
    Ok((
        [(header::CONTENT_TYPE, "text/x-opml; charset=utf-8")],
        opml_document(&feeds),
    ))
}

/// An OPML 2.0 list of the feeds still followed (not archived ones).
fn opml_document(feeds: &[Feed]) -> String {
    let mut doc = String::from(concat!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
        "<opml version=\"2.0\">\n",
        "  <head><title>pressbox feeds</title></head>\n",
        "  <body>\n",
    ));
    // Writing to a String cannot fail.
    for feed in feeds.iter().filter(|f| !f.archived) {
        let title = xml_escape(&feed.title);
        let _ = write!(
            doc,
            "    <outline text=\"{title}\" title=\"{title}\" type=\"rss\" xmlUrl=\"{}\"",
            xml_escape(&feed.url)
        );
        if let Some(site) = &feed.site_url {
            let _ = write!(doc, " htmlUrl=\"{}\"", xml_escape(site));
        }
        if !feed.tags.is_empty() {
            let _ = write!(doc, " category=\"{}\"", xml_escape(&feed.tags.join(",")));
        }
        doc.push_str("/>\n");
    }
    doc.push_str("  </body>\n</opml>\n");
    doc
}

#[cfg(test)]
mod tests {
    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use super::*;
    use crate::{config::Config, db::Db};

    const TWO_USERS: &str = r#"
        [users.chris]
        admin = true
        [users.chris.feeds.blog]
        url = "https://blog.example/feed"
        title = "Tom & Jerry's <blog>"
        tags = ["fun", "cats"]
        [users.anna]
    "#;

    fn state(config: &str, backup_dir: &Path) -> Arc<AppState> {
        let mut db = Db::open_in_memory().unwrap();
        db.sync(&Config::parse(config).unwrap()).unwrap();
        let mut state = AppState::new(db, None, 100).unwrap();
        state.backup_dir = backup_dir.to_owned();
        Arc::new(state)
    }

    async fn call(
        state: &Arc<AppState>,
        method: &str,
        uri: &str,
        body: Option<&str>,
    ) -> (StatusCode, String) {
        let mut req = Request::builder().method(method).uri(uri);
        if body.is_some() {
            req = req.header(header::CONTENT_TYPE, "application/json");
        }
        let body = body.map_or_else(Body::empty, |b| Body::from(b.to_owned()));
        let resp = router(state.clone())
            .oneshot(req.body(body).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8(bytes.to_vec()).unwrap())
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pressboxd-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[tokio::test]
    async fn devices_and_status() {
        let s = state(TWO_USERS, &temp_dir("unused"));
        let (status, body) = call(&s, "GET", "/admin/users", None).await;
        assert_eq!(status, StatusCode::OK);
        let users: Vec<UserInfo> = serde_json::from_str(&body).unwrap();
        assert_eq!(users.len(), 2);

        // With two users, one has to be named.
        let (status, body) = call(&s, "POST", "/admin/devices", Some(r#"{"name":"pc"}"#)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body.contains("--user"), "{body}");
        for (request, why) in [
            (r#"{"name":"pc","user":"nobody"}"#, "unknown user"),
            (r#"{"name":"a/b","user":"anna"}"#, "bad name"),
        ] {
            let (status, _) = call(&s, "POST", "/admin/devices", Some(request)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{why}");
        }
        let add = Some(r#"{"name":"my pc","user":"anna"}"#);
        let (status, body) = call(&s, "POST", "/admin/devices", add).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let device: NewDevice = serde_json::from_str(&body).unwrap();
        assert_eq!(
            (device.name.as_str(), device.user.as_str()),
            ("my-pc", "anna")
        );
        let who = s
            .reader()
            .authenticate(&pairing::hash_token(&device.token), now())
            .unwrap()
            .unwrap();
        assert_eq!(who.name, "anna", "the token works");
        let (_, body) = call(&s, "POST", "/admin/devices", add).await;
        let again: NewDevice = serde_json::from_str(&body).unwrap();
        assert_eq!(again.name, "my-pc-2", "names are made unique");

        let (_, body) = call(&s, "GET", "/admin/devices", None).await;
        let devices: Vec<Device> = serde_json::from_str(&body).unwrap();
        assert_eq!(devices.len(), 2);

        let (_, body) = call(&s, "GET", "/admin/status", None).await;
        let status: Status = serde_json::from_str(&body).unwrap();
        assert_eq!(
            (
                status.feeds,
                status.users,
                status.devices,
                status.pending_pairings
            ),
            (1, 2, 2, 0)
        );

        let (status, _) = call(&s, "DELETE", "/admin/devices/my-pc", None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _) = call(&s, "DELETE", "/admin/devices/my-pc", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let (status, _) = call(&s, "POST", "/admin/refresh", None).await;
        assert_eq!(status, StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn approves_pairings_for_a_user() {
        let s = state(TWO_USERS, &temp_dir("unused"));
        let start = s.pairings.start("phone", "10.0.0.2", now()).unwrap();
        let (_, body) = call(&s, "GET", "/admin/pairings", None).await;
        assert!(body.contains("phone"), "{body}");

        let uri = format!("/admin/pairings/{}/approve", start.code);
        let (status, _) = call(&s, "POST", &uri, None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "two users: say which");
        let (status, body) = call(&s, "POST", &uri, Some(r#"{"user":"anna"}"#)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let approved: Approved = serde_json::from_str(&body).unwrap();
        assert_eq!(approved.user, "anna");
        let (status, _) = call(&s, "POST", &uri, Some(r#"{"user":"anna"}"#)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "only once");

        let denied = s.pairings.start("tablet", "10.0.0.2", now()).unwrap();
        let uri = format!("/admin/pairings/{}", denied.code);
        assert_eq!(
            call(&s, "DELETE", &uri, None).await.0,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            call(&s, "DELETE", &uri, None).await.0,
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn exports_opml() {
        let s = state(TWO_USERS, &temp_dir("unused"));
        let (status, body) = call(&s, "GET", "/admin/opml?user=chris", None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.starts_with("<?xml"), "{body}");
        assert!(
            body.contains(
                r#"<outline text="Tom &amp; Jerry's &lt;blog&gt;" title="Tom &amp; Jerry's &lt;blog&gt;" type="rss" xmlUrl="https://blog.example/feed" category="fun,cats"/>"#
            ),
            "{body}"
        );
        let (_, body) = call(&s, "GET", "/admin/opml?user=anna", None).await;
        assert!(!body.contains("<outline"), "anna follows nothing: {body}");
        let (status, _) = call(&s, "GET", "/admin/opml", None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "two users: say which");
    }

    #[test]
    fn opml_leaves_out_archived_feeds_and_escapes() {
        let feed = |id: &str, archived| Feed {
            id: id.into(),
            title: format!("\"{id}\""),
            url: format!("https://{id}.example/?a=1&b=2"),
            site_url: Some(format!("https://{id}.example/")),
            tags: Vec::new(),
            unread: 0,
            total: 0,
            last_fetched: None,
            last_error: None,
            archived,
            image: None,
            medium: None,
            episodes: 0,
            unread_episodes: 0,
        };
        let doc = opml_document(&[feed("kept", false), feed("old", true)]);
        assert!(doc.contains(r#"title="&quot;kept&quot;""#), "{doc}");
        assert!(
            doc.contains(r#"xmlUrl="https://kept.example/?a=1&amp;b=2""#),
            "{doc}"
        );
        assert!(doc.contains(r#"htmlUrl="https://kept.example/""#), "{doc}");
        assert!(!doc.contains("category"), "no tags, no category: {doc}");
        assert!(!doc.contains("old"), "{doc}");
        assert!(doc.ends_with("</opml>\n"));
    }

    #[tokio::test]
    async fn backs_up_and_keeps_the_newest() {
        let dir = temp_dir("backup");
        let s = state("[feeds.a]\nurl = \"https://a.example/\"", &dir);
        let (status, body) = call(&s, "POST", "/admin/backup", Some(r#"{"keep":1}"#)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let first: BackupResult = serde_json::from_str(&body).unwrap();
        // Named for the local time to the millisecond, so names sort by time:
        // pressbox-20261117T084200.123.db
        let name = Path::new(&first.path)
            .file_name()
            .unwrap()
            .to_str()
            .unwrap();
        let stamp = name
            .strip_prefix("pressbox-")
            .and_then(|n| n.strip_suffix(".db"))
            .unwrap();
        assert_eq!(stamp.len(), "20261117T084200.123".len(), "{name}");
        assert!(
            stamp.chars().enumerate().all(|(i, c)| match i {
                8 => c == 'T',
                15 => c == '.',
                _ => c.is_ascii_digit(),
            }),
            "{name}"
        );
        assert!(first.removed.is_empty());
        let copy = Db::open(Path::new(&first.path)).unwrap();
        assert_eq!(copy.counts().unwrap().feeds, 1, "a working database");
        drop(copy);

        let (_, body) = call(&s, "POST", "/admin/backup", Some(r#"{"keep":0}"#)).await;
        let second: BackupResult = serde_json::from_str(&body).unwrap();
        assert_ne!(second.path, first.path);
        assert_eq!(second.removed, [first.path], "keep 0 still keeps one");
        assert!(Path::new(&second.path).exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn binds_a_private_socket_over_a_stale_one() {
        let dir = temp_dir("socket");
        let path = dir.join("sub/control.sock");
        drop(bind(&path).unwrap());
        assert!(path.exists(), "left behind, as after a crash");
        let _listener = bind(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1,
            "no staging directory is left"
        );
        // Moved into place, it still takes connections.
        tokio::net::UnixStream::connect(&path).await.unwrap();

        // Something that isn't a stale socket is left alone.
        let occupied = dir.join("occupied");
        std::fs::create_dir_all(occupied.join("inside")).unwrap();
        let error = bind(&occupied).unwrap_err();
        assert!(format!("{error}").starts_with("removing stale"), "{error}");
        assert!(occupied.join("inside").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn prunes_oldest_backups() {
        let dir = std::env::temp_dir().join(format!("pressboxd-backups-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for stamp in ["20260101T000000", "20260102T000000", "20260103T000000"] {
            std::fs::write(dir.join(format!("pressbox-{stamp}.db")), "").unwrap();
        }
        std::fs::write(dir.join("unrelated.txt"), "").unwrap();
        let removed = prune_backups(&dir, 2).unwrap();
        assert_eq!(removed, [dir.join("pressbox-20260101T000000.db")]);
        assert!(dir.join("unrelated.txt").exists());
        assert!(prune_backups(&dir, 5).unwrap().is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
