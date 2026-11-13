//! HTTP plumbing shared by the App's loopback servers: one HTTP/1.1 accept
//! loop with upgrades, a bounded graceful shutdown, and plain-text error
//! responses.

use std::io;
use std::pin::pin;
use std::time::Duration;

use axum::Router;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use hyper::server::conn::http1;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(5);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

/// Serves `router` until `shutdown` fires or accepting fails, then gives open
/// requests a bounded time to finish. Upgraded WebSockets are owned by their
/// handlers and are not part of this wait.
pub async fn serve(
    listener: TcpListener,
    router: Router,
    shutdown: CancellationToken,
) -> io::Result<()> {
    let connections = TaskTracker::new();
    let result = loop {
        let accepted = tokio::select! {
            () = shutdown.cancelled() => break Ok(()),
            accepted = listener.accept() => accepted,
        };
        let stream = match accepted {
            Ok((stream, _)) => stream,
            Err(error) if transient(&error) => continue,
            Err(error) => break Err(error),
        };
        let service = TowerToHyperService::new(router.clone());
        let shutdown = shutdown.clone();
        connections.spawn(async move {
            let mut builder = http1::Builder::new();
            builder
                .timer(TokioTimer::new())
                .header_read_timeout(HEADER_READ_TIMEOUT);
            let mut connection = pin!(
                builder
                    .serve_connection(TokioIo::new(stream), service)
                    .with_upgrades()
            );
            tokio::select! {
                _ = connection.as_mut() => return,
                () = shutdown.cancelled() => connection.as_mut().graceful_shutdown(),
            }
            let _ = connection.await;
        });
    };
    drop(listener);
    connections.close();
    let _ = tokio::time::timeout(SHUTDOWN_TIMEOUT, connections.wait()).await;
    result
}

fn transient(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionRefused
            | io::ErrorKind::Interrupted
    )
}

/// A `nosniff` plain-text error whose body is `message` plus a newline.
pub fn text_error(status: StatusCode, message: &str) -> Response {
    let mut response = (status, format!("{message}\n")).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

pub fn not_found() -> Response {
    text_error(StatusCode::NOT_FOUND, "404 page not found")
}

pub fn method_not_allowed(allow: &'static str) -> Response {
    let mut response = text_error(StatusCode::METHOD_NOT_ALLOWED, "method not allowed");
    response
        .headers_mut()
        .insert(header::ALLOW, HeaderValue::from_static(allow));
    response
}

/// A JSON document with the shared content type.
pub fn json(status: StatusCode, payload: impl Into<axum::body::Body>) -> Response {
    let mut response = (status, payload.into()).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    response
}

/// The first value of a header as text; absent or non-text is empty.
pub fn header_text(headers: &axum::http::HeaderMap, name: header::HeaderName) -> &str {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
}

/// The request's `Host`, falling back to an absolute-form authority.
pub fn request_host<B>(request: &axum::http::Request<B>) -> String {
    let host = header_text(request.headers(), header::HOST);
    if !host.is_empty() {
        return host.to_owned();
    }
    request
        .uri()
        .authority()
        .map(|authority| authority.as_str().to_owned())
        .unwrap_or_default()
}
