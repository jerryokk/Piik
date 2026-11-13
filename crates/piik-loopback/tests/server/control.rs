//! Strict `/control` WebSocket handshake, framing and session lifecycle.

use super::*;

#[tokio::test]
async fn control_sessions_are_bounded_and_retire_independently() {
    for configured in [0, 1] {
        let limit = if configured == 0 { 2 } else { configured };
        let (factory, mut sessions_created) = recording_factory(None);
        let server = start(Options {
            max_control_sessions: configured,
            allowed_origins: vec![TEST_ORIGIN.to_owned()],
            new_control: Some(factory),
            ..Options::default()
        });
        let mut sockets = Vec::new();
        let mut sessions = Vec::new();
        for _ in 0..limit {
            let mut socket = dial(&server, TEST_ORIGIN).await;
            hello(&mut socket).await;
            sockets.push(socket);
            sessions.push(created(&mut sessions_created).await.0);
        }
        match try_dial(&server, TEST_ORIGIN).await {
            Err(tungstenite::Error::Http(response)) => {
                assert_eq!(response.status(), StatusCode::CONFLICT);
            }
            other => panic!("excess control dial: {:?}", other.map(drop)),
        }
        let mut first = sockets.remove(0);
        first.close(None).await.unwrap();
        drop(first);
        assert!(
            signalled(&sessions[0].closed).await,
            "closed room retained its session"
        );
        if let Some(sibling) = sockets.first_mut() {
            write(sibling, "request_ping", "ping").await;
            assert_eq!(
                read(sibling).await.kind,
                "pong",
                "closing one room affected its sibling"
            );
        }
        server.close().await.unwrap();
        for session in &sessions {
            assert!(
                session.closed.is_cancelled(),
                "App close returned before native session cleanup"
            );
        }
    }
}

#[tokio::test]
async fn control_session_shares_one_bounded_socket_for_responses_and_events() {
    let (factory, mut sessions_created) = recording_factory(None);
    let server = start(Options {
        allowed_origins: vec![TEST_ORIGIN.to_owned()],
        new_control: Some(factory),
        ..Options::default()
    });
    let mut socket = dial(&server, TEST_ORIGIN).await;
    hello(&mut socket).await;
    let (session, sink) = created(&mut sessions_created).await;
    // The old owner's event was queued while valid, but retired before delivery.
    let current = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let flag = current.clone();
    let retired = event("", "retired-event")
        .with_current(move || flag.load(std::sync::atomic::Ordering::SeqCst));
    current.store(false, std::sync::atomic::Ordering::SeqCst);
    assert!(sink.send(retired).await);
    let prepared = CancellationToken::new();
    let released = prepared.clone();
    assert!(
        sink.send(
            event("", "current-event")
                .with_current(|| true)
                .with_before_write(move || released.cancel())
        )
        .await
    );
    let current_event = read(&mut socket).await;
    assert_eq!(
        current_event.kind, "current-event",
        "retired event escaped the fence"
    );
    assert!(
        prepared.is_cancelled(),
        "response was visible before its operation released admission"
    );
    assert!(sink.send(event("event_123456", "extension-event")).await);
    write(&mut socket, "request_extension", "extension").await;
    let mut received = std::collections::HashSet::new();
    while received.len() < 2 {
        received.insert(read(&mut socket).await.kind);
    }
    assert!(received.contains("extension-event") && received.contains("extension-response"));
    drop(socket);
    assert!(
        signalled(&session.closed).await,
        "extension was not closed with its socket"
    );
    assert!(
        tokio::time::timeout(SECOND, sink.closed()).await.is_ok(),
        "event sink outlived its connection"
    );
}

#[tokio::test]
async fn control_closes_the_socket_when_its_reader_rejects_a_frame() {
    let server = start_with_origin(TEST_ORIGIN);
    let mut socket = dial(&server, TEST_ORIGIN).await;
    hello(&mut socket).await;
    socket
        .send(Message::binary(b"invalid".to_vec()))
        .await
        .unwrap();
    assert!(
        ends(&mut socket).await,
        "server did not close its rejected control socket"
    );
}

#[tokio::test]
async fn protocol_violations_close_with_policy_violation() {
    let server = start_with_origin(TEST_ORIGIN);
    for (first, second) in [
        (request("request_ping", "ping"), None),
        (
            request("request_hello", "hello"),
            Some(request("request_other", "other")),
        ),
        (request("request_hello", "hello"), Some("{}".to_owned())),
    ] {
        let mut socket = dial(&server, TEST_ORIGIN).await;
        socket.send(Message::text(first)).await.unwrap();
        if let Some(second) = second {
            read(&mut socket).await;
            socket.send(Message::text(second)).await.unwrap();
        }
        let close = tokio::time::timeout(SECOND, socket.next()).await.unwrap();
        let Some(Ok(Message::Close(Some(frame)))) = close else {
            panic!("expected a close frame, got {close:?}");
        };
        assert_eq!(u16::from(frame.code), 1008);
    }
}

#[tokio::test]
async fn control_requires_the_instance_subprotocol() {
    let server = start_with_origin(TEST_ORIGIN);
    let url = server.endpoint().url.replacen("http://", "ws://", 1) + "/control";
    let mut request = url.into_client_request().unwrap();
    request
        .headers_mut()
        .insert("Origin", HeaderValue::from_static(TEST_ORIGIN));
    // No subprotocol offered: the upgrade completes and is closed as a violation.
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    let close = tokio::time::timeout(SECOND, socket.next()).await.unwrap();
    let Some(Ok(Message::Close(Some(frame)))) = close else {
        panic!("expected a close frame, got {close:?}");
    };
    assert_eq!(u16::from(frame.code), 1008);
    assert_eq!(frame.reason.as_str(), "unsupported control protocol");
}

/// A control session whose picker waits for cancellation.
struct BlockingSession {
    started: CancellationToken,
    closed: CancellationToken,
}

impl ControlSession for BlockingSession {
    fn handle<'a>(
        &'a self,
        cancel: &'a CancellationToken,
        _: &'a [u8],
    ) -> BoxFuture<'a, Result<Option<ControlEvent>, ProtocolViolation>> {
        Box::pin(async move {
            self.started.cancel();
            cancel.cancelled().await;
            Err(ProtocolViolation::new("cancelled"))
        })
    }

    fn close(&self) -> BoxFuture<'_, ()> {
        self.closed.cancel();
        Box::pin(async {})
    }
}

#[tokio::test]
async fn control_close_cancels_a_handler_waiting_for_picker() {
    let started = CancellationToken::new();
    let closed = CancellationToken::new();
    let (session_started, session_closed) = (started.clone(), closed.clone());
    let server = start(Options {
        allowed_origins: vec![TEST_ORIGIN.to_owned()],
        max_control_sessions: 1,
        new_control: Some(Arc::new(move |_| {
            Some(Box::new(BlockingSession {
                started: session_started.clone(),
                closed: session_closed.clone(),
            }) as Box<dyn ControlSession>)
        })),
        ..Options::default()
    });
    let mut socket = dial(&server, TEST_ORIGIN).await;
    hello(&mut socket).await;
    write(&mut socket, "request_extension", "extension").await;
    assert!(signalled(&started).await, "control handler did not start");
    drop(socket);
    assert!(
        signalled(&closed).await,
        "closing the socket did not release the picker"
    );
    let deadline = tokio::time::Instant::now() + SECOND;
    loop {
        if try_dial(&server, TEST_ORIGIN).await.is_ok() {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "control session was not released"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn control_session_processes_a_request_burst_in_order() {
    let (factory, _created) = recording_factory(None);
    let server = start(Options {
        allowed_origins: vec![TEST_ORIGIN.to_owned()],
        new_control: Some(factory),
        ..Options::default()
    });
    let mut socket = dial(&server, TEST_ORIGIN).await;
    hello(&mut socket).await;
    const BURST: usize = 64 * 2;
    for index in 0..BURST {
        write(
            &mut socket,
            &format!("request_extension_{index}"),
            "extension",
        )
        .await;
    }
    for index in 0..BURST {
        let response = read(&mut socket).await;
        assert_eq!(response.id, format!("request_extension_{index}"));
    }
}

#[tokio::test]
async fn deferred_response_keeps_control_responsive_and_request_identity() {
    let (factory, mut sessions_created) = recording_factory(None);
    let server = start(Options {
        allowed_origins: vec![TEST_ORIGIN.to_owned()],
        new_control: Some(factory),
        ..Options::default()
    });
    let mut socket = dial(&server, TEST_ORIGIN).await;
    hello(&mut socket).await;
    let (_session, sink) = created(&mut sessions_created).await;
    write(&mut socket, "request_pending", "deferred").await;
    write(&mut socket, "request_ping", "ping").await;
    let value = read(&mut socket).await;
    assert_eq!(
        (value.kind.as_str(), value.id.as_str()),
        ("pong", "request_ping")
    );
    assert!(
        sink.send(event("request_pending", "extension-response"))
            .await
    );
    let value = read(&mut socket).await;
    assert_eq!(
        (value.kind.as_str(), value.id.as_str()),
        ("extension-response", "request_pending")
    );
}

#[tokio::test]
async fn hello_must_arrive_within_its_deadline() {
    let server = start_with_origin(TEST_ORIGIN);
    let mut socket = dial(&server, TEST_ORIGIN).await;
    tokio::time::sleep(Duration::from_millis(5200)).await;
    assert!(
        ends(&mut socket).await,
        "silent control outlived its hello deadline"
    );
}

#[tokio::test]
async fn close_stops_the_control_session() {
    let server = start_with_origin(TEST_ORIGIN);
    let mut socket = dial(&server, TEST_ORIGIN).await;
    server.close().await.unwrap();
    assert!(
        ends(&mut socket).await,
        "control connection remained open after close"
    );
    assert!(server.done().await.is_ok());
}

#[tokio::test]
async fn parent_cancellation_closes_the_server() {
    let parent = CancellationToken::new();
    let server = Server::start(
        &parent,
        Options {
            port_start: 39_000,
            port_end: 39_999,
            ..Options::default()
        },
    )
    .unwrap();
    parent.cancel();
    tokio::time::timeout(SECOND * 3, server.done())
        .await
        .expect("cancelled server kept serving")
        .unwrap();
}
