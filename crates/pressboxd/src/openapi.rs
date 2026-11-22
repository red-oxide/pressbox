// SPDX-FileCopyrightText: 2026 Chris Palmer <chris@red-oxide.org>
// SPDX-License-Identifier: AGPL-3.0-only

//! The API's OpenAPI description, built from the routes themselves (see
//! [`crate::api`]) and the types in `pressbox-api`.
//!
//! It is served at `GET /api/v1/openapi.json` and checked in as
//! `docs/openapi.json`, so the Swift and Kotlin clients can be generated or
//! tested against it. A test fails when the checked-in copy is out of date;
//! `just openapi` rewrites it.

use std::sync::OnceLock;

use pressbox_api::{ErrorResponse, PairApproved, PairPending};
use utoipa::openapi::{
    self, ComponentsBuilder, InfoBuilder, LicenseBuilder, PathItem, RefOr,
    path::Operation,
    response::ResponseBuilder,
    security::{HttpAuthScheme, HttpBuilder, SecurityRequirement, SecurityScheme},
    tag::TagBuilder,
};
use utoipa_axum::router::OpenApiRouter;

use crate::api::{self, tag};

/// The security scheme every authenticated route names.
const TOKEN: &str = "token";

/// The description of the whole API.
pub fn spec() -> openapi::OpenApi {
    let mut authenticated = api::authenticated_routes().into_openapi();
    for operation in authenticated.paths.paths.values_mut().flat_map(operations) {
        operation.security = Some(vec![SecurityRequirement::new(TOKEN, Vec::<String>::new())]);
        operation.responses.responses.insert(
            "401".to_owned(),
            RefOr::T(
                ResponseBuilder::new()
                    .description("A missing or unknown token")
                    .content(
                        "application/json",
                        openapi::ContentBuilder::new()
                            .schema(Some(openapi::Ref::from_schema_name("ErrorResponse")))
                            .build(),
                    )
                    .build(),
            ),
        );
    }
    let mut spec = api::root_routes()
        .nest(
            "/api/v1",
            api::public_routes().merge(OpenApiRouter::with_openapi(authenticated)),
        )
        .into_openapi();
    spec.info = InfoBuilder::new()
        .title("pressbox")
        // The API's version, not pressboxd's: the description only changes
        // when the API does, and `GET /api/v1/version` says which server
        // it is.
        .version(pressbox_api::API_VERSION.to_string())
        .description(Some(DESCRIPTION))
        .license(Some(
            LicenseBuilder::new()
                .name("AGPL-3.0-only")
                .identifier(Some("AGPL-3.0-only"))
                .build(),
        ))
        .build();
    spec.tags = Some(
        [
            (tag::FEEDS, "The user's feeds"),
            (
                tag::ITEMS,
                "Entries from the feeds, and their read and starred state",
            ),
            (
                tag::PODCASTS,
                "Playback positions, Up Next, chapters and transcripts",
            ),
            (
                tag::PAIRING,
                "How a device gets a token, and approving others",
            ),
            (tag::SERVER, "About the server and the token"),
        ]
        .map(|(name, about)| {
            TagBuilder::new()
                .name(name)
                .description(Some(about))
                .build()
        })
        .into(),
    );
    let components = spec.components.get_or_insert_with(Default::default);
    components.add_security_scheme(
        TOKEN,
        SecurityScheme::Http(
            HttpBuilder::new()
                .scheme(HttpAuthScheme::Bearer)
                .description(Some(
                    "A paired device's token, or the static `tokenFile` token",
                ))
                .build(),
        ),
    );
    // Types only named by hand-written responses (`openapi_json` and the
    // 401s above) or by status codes `#[utoipa::path]` doesn't collect.
    let extra = ComponentsBuilder::new()
        .schema_from::<ErrorResponse>()
        .schema_from::<PairApproved>()
        .schema_from::<PairPending>()
        .build();
    components.schemas.extend(extra.schemas);
    spec
}

const DESCRIPTION: &str = "The HTTP API of pressboxd, the pressbox server. \
A token belongs to one user, and every answer is about that user's feeds and \
state. Timestamps are Unix seconds (UTC). Responses are compressed (gzip or \
brotli) when the request's `Accept-Encoding` allows it. Breaking changes will \
go to a new version (`/api/v2/`), served alongside `v1` for a while. See \
docs/api.md for more.";

fn operations(item: &mut PathItem) -> impl Iterator<Item = &mut Operation> {
    [
        &mut item.get,
        &mut item.put,
        &mut item.post,
        &mut item.delete,
        &mut item.patch,
        &mut item.head,
        &mut item.options,
        &mut item.trace,
    ]
    .into_iter()
    .flatten()
}

/// [`spec`] as pretty-printed JSON, built once.
pub fn json() -> &'static str {
    static JSON: OnceLock<String> = OnceLock::new();
    JSON.get_or_init(|| {
        let mut json = spec()
            .to_pretty_json()
            .expect("the OpenAPI description serializes");
        json.push('\n');
        json
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHECKED_IN: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/openapi.json");

    /// Compared as JSON values, so a formatter may lay the file out its own
    /// way. `just openapi` (`UPDATE_OPENAPI=1`) rewrites it instead.
    #[test]
    fn the_checked_in_description_is_current() {
        if std::env::var_os("UPDATE_OPENAPI").is_some() {
            std::fs::write(CHECKED_IN, json()).unwrap();
        }
        let checked_in: serde_json::Value = std::fs::read_to_string(CHECKED_IN)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let current: serde_json::Value = serde_json::from_str(json()).unwrap();
        assert!(
            checked_in == current,
            "docs/openapi.json is out of date with the API: run `just openapi`"
        );
    }

    #[test]
    fn only_the_public_routes_need_no_token() {
        let spec = spec();
        let mut public = Vec::new();
        for (path, item) in &mut spec.paths.paths.clone() {
            for op in operations(item) {
                if op.security.is_none() {
                    public.push(path.clone());
                }
            }
        }
        assert_eq!(
            public,
            [
                "/api/v1/openapi.json",
                "/api/v1/pair",
                "/api/v1/pair/{poll_id}",
                "/api/v1/version",
                "/health",
            ]
        );
    }

    #[test]
    fn every_referenced_schema_is_defined() {
        let json = json();
        let schemas = &spec().components.unwrap().schemas;
        for name in json.split("\"#/components/schemas/").skip(1) {
            let name = &name[..name.find('"').unwrap()];
            assert!(
                schemas.contains_key(name),
                "{name} is referenced but not defined"
            );
        }
    }

    #[tokio::test]
    async fn it_is_served_without_a_token() {
        use axum::{body::Body, http::Request};
        use http_body_util::BodyExt;
        use tower::ServiceExt;

        let db = crate::db::Db::open_in_memory().unwrap();
        let state = std::sync::Arc::new(crate::AppState::new(db, None, 500).unwrap());
        let resp = api::router(state)
            .oneshot(
                Request::get("/api/v1/openapi.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.headers()["content-type"], "application/json");
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(body, json().as_bytes());
    }
}
