//! The `/signal` WebSocket endpoint on axum.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::FromRequestParts;
use axum::extract::ws::WebSocketUpgrade;
use axum::http::{HeaderValue, Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use piik_protocol::MAX_SIGNAL_BYTES;

use crate::runtime::lock;
use crate::server::Server;
use crate::session::spawn_session;

/// `HTTP/1.1 {status}` with `Connection: close` and `Content-Length: 0`.
fn reject_upgrade(status: StatusCode) -> Response {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(header::CONNECTION, HeaderValue::from_static("close"));
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from_static("0"));
    response
}

/// Whether the request target is a `//authority` form whose authority does
/// not parse.
fn invalid_request_target(uri: &axum::http::Uri) -> bool {
    let target = uri.path_and_query().map_or("", |target| target.as_str());
    target.starts_with("//") && url::Url::parse(&format!("http:{target}")).is_err()
}

impl Server {
    /// An axum router that serves only `GET /signal`. Merge it into the
    /// application router or nest it as the fallback for that path.
    pub fn router(self: &Arc<Self>) -> Router {
        let server = self.clone();
        Router::new().fallback(move |request: Request<Body>| {
            let server = server.clone();
            async move { server.serve_upgrade(request).await }
        })
    }

    /// Validates the endpoint, origin and capacity before upgrading.
    pub async fn serve_upgrade(self: &Arc<Self>, request: Request<Body>) -> Response {
        // Match the literal endpoint, rejecting encoded aliases such as
        // "/%73ignal", as the sibling HTTP router does.
        let uri = request.uri();
        // HTTP parsing accepts scheme-relative targets as paths; reject
        // malformed authorities such as "//[" before checking the endpoint.
        if invalid_request_target(uri) {
            return reject_upgrade(StatusCode::BAD_REQUEST);
        }
        if uri.path() != "/signal" || uri.query().is_some_and(|query| !query.is_empty()) {
            return reject_upgrade(StatusCode::NOT_FOUND);
        }
        let origin = request
            .headers()
            .get(header::ORIGIN)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        {
            let mut core = lock(&self.core);
            if core.closing {
                return reject_upgrade(StatusCode::SERVICE_UNAVAILABLE);
            }
            if !core.is_allowed_origin(&origin) {
                return reject_upgrade(StatusCode::FORBIDDEN);
            }
            if !core.has_connection_capacity() {
                return reject_upgrade(StatusCode::SERVICE_UNAVAILABLE);
            }
            // Reserve capacity before the upgrade completes outside the lock.
            core.pending_connections += 1;
        }
        let site_access = self
            .site_access_at_upgrade
            .as_ref()
            .is_none_or(|check| check(request.headers()));
        let (mut parts, _) = request.into_parts();
        let upgrade = match WebSocketUpgrade::from_request_parts(&mut parts, &()).await {
            Ok(upgrade) => upgrade,
            Err(rejection) => {
                lock(&self.core).pending_connections -= 1;
                return rejection.into_response();
            }
        };
        let accepted = self.clone();
        let failed = self.clone();
        upgrade
            .max_message_size(MAX_SIGNAL_BYTES)
            .max_frame_size(MAX_SIGNAL_BYTES)
            .on_failed_upgrade(move |_| {
                lock(&failed.core).pending_connections -= 1;
            })
            .on_upgrade(move |socket| async move {
                let (handles, locker) = {
                    let mut core = lock(&accepted.core);
                    core.pending_connections -= 1;
                    (core.accept(site_access), core.locker.clone())
                };
                // Over capacity after the upgrade: drop the socket at once.
                if let Some(handles) = handles {
                    spawn_session(locker, handles, socket);
                }
            })
    }
}
