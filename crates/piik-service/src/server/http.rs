//! Listener accept loops, HTTP connection serving and retry classification.

use std::convert::Infallible;
use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::Request;
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper_util::rt::{TokioIo, TokioTimer};
use piik_tls::TlsAcceptor;
use tokio_util::sync::CancellationToken;

use crate::server::{HEADERS_TIMEOUT, Inner};

pub(crate) fn remaining(deadline: Instant) -> Duration {
    deadline.saturating_duration_since(Instant::now())
}

/// Accept failures that are retried with a backoff.
pub(crate) fn temporary(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionRefused
            | io::ErrorKind::Interrupted
            | io::ErrorKind::WouldBlock
            | io::ErrorKind::TimedOut
    ) || matches!(error.raw_os_error(), Some(23 | 24 | 10024 | 10055))
}

pub(crate) async fn accept_loop(
    inner: Arc<Inner>,
    listener: tokio::net::TcpListener,
    tls: Option<TlsAcceptor>,
    fault: CancellationToken,
) {
    let mut backoff = Duration::ZERO;
    loop {
        let accepted = tokio::select! {
            () = inner.stop.cancelled() => return,
            () = fault.cancelled() => Err(io::Error::new(io::ErrorKind::NotConnected, "use of closed network connection")),
            accepted = listener.accept() => accepted,
        };
        let stream = match accepted {
            Ok((stream, _)) => stream,
            Err(error) if temporary(&error) => {
                backoff = (backoff * 2).clamp(Duration::from_millis(5), Duration::from_secs(1));
                tokio::time::sleep(backoff).await;
                continue;
            }
            Err(error) => {
                inner.listener_failed(error);
                return;
            }
        };
        backoff = Duration::ZERO;
        let connection = inner.clone();
        let acceptor = tls.clone();
        inner.connections.spawn(async move {
            match acceptor {
                None => serve_connection(connection, TokioIo::new(stream), false).await,
                Some(acceptor) => {
                    let stop = connection.stop.clone();
                    let handshake = tokio::time::timeout(HEADERS_TIMEOUT, acceptor.accept(stream));
                    let stream = tokio::select! {
                        () = stop.cancelled() => return,
                        stream = handshake => match stream {
                            Ok(Ok(stream)) => stream,
                            _ => return,
                        },
                    };
                    serve_connection(connection, TokioIo::new(stream), true).await;
                }
            }
        });
    }
}

pub(crate) async fn serve_connection<I>(inner: Arc<Inner>, io: I, tls: bool)
where
    I: hyper::rt::Read + hyper::rt::Write + Unpin + Send + 'static,
{
    let stop = inner.stop.clone();
    let service = hyper::service::service_fn(move |request: Request<Incoming>| {
        let inner = inner.clone();
        async move { Ok::<_, Infallible>(inner.dispatch(request.map(Body::new), tls).await) }
    });
    let mut builder = http1::Builder::new();
    builder
        .timer(TokioTimer::new())
        .header_read_timeout(HEADERS_TIMEOUT);
    let connection = builder.serve_connection(io, service).with_upgrades();
    let mut connection = std::pin::pin!(connection);
    tokio::select! {
        _ = connection.as_mut() => return,
        () = stop.cancelled() => connection.as_mut().graceful_shutdown(),
    }
    let _ = connection.await;
}
