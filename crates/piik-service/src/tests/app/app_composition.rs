//! Frontend composition and health endpoints.

use super::*;

#[tokio::test]
async fn configured_empty_room_expiry_runs_from_the_owned_heartbeat() {
    let now = Arc::new(AtomicI64::new(0));
    let mut config = test_config();
    config.site_access_password = String::new();
    config.room_empty_timeout_seconds = 3600;
    let (ticks, tick) = std::sync::mpsc::channel::<Box<dyn FnOnce() + Send>>();
    let ticks = std::sync::Mutex::new(ticks);
    let mut options = options(config);
    options.now = Some(clock(&now));
    options.after_func = Some(Arc::new(move |_, callback| {
        let _ = ticks.lock().expect("ticks").send(callback);
        Box::new(|| true)
    }));
    let h = start(options).await;
    let created = h
        .create_room(RoomRequest::default())
        .await
        .expect_status(201)
        .room();
    let next = || {
        tick.recv_timeout(Duration::from_secs(5))
            .expect("heartbeat armed")
    };
    let private = r#"{"action":"set-code-entry-policy","policy":"private"}"#;
    now.store(3_599_999, Ordering::SeqCst);
    next()();
    h.update_access(&created.room_id, &created.host_token, private, "")
        .await
        .expect_status(200);
    now.store(3_600_000, Ordering::SeqCst);
    next()();
    h.update_access(&created.room_id, &created.host_token, private, "")
        .await
        .expect_status(404);
    h.create_room(RoomRequest::default())
        .await
        .expect_status(201);
    h.close().await;
}

#[tokio::test]
async fn serves_an_explicit_static_frontend_independently_of_the_environment() {
    let mut options = options(test_config());
    options.assets = Some(Arc::new(
        crate::MemoryAssets::new()
            .with("index.html", b"<main>local-static</main>".to_vec())
            .with("assets/app.js", b"export const marker = 1;\n".to_vec()),
    ));
    let h = start(options).await;
    h.send(Req::get("/local-route"))
        .await
        .expect(200, "<main>local-static</main>")
        .expect_header("content-type", "text/html;charset=utf-8")
        .expect_header("x-content-type-options", "nosniff")
        .expect_header("referrer-policy", "no-referrer");
    h.send(Req::get("/assets/app.js"))
        .await
        .expect(200, "export const marker = 1;\n")
        .expect_header("content-type", "text/javascript");
    // /api/** never falls through to the frontend.
    h.send(Req::get("/api/unknown"))
        .await
        .expect(404, r#"{"error":"Not found"}"#)
        .expect_header("cache-control", "no-store");
    // A missing asset with an extension keeps the JSON 404.
    h.send(Req::get("/assets/missing.js"))
        .await
        .expect(404, r#"{"error":"Not found"}"#);
    h.close().await;
}

#[tokio::test]
async fn api_only_composition_answers_json_not_found() {
    let h = start(options(test_config())).await;
    h.send(Req::get("/local-route"))
        .await
        .expect(404, r#"{"error":"Not found"}"#)
        .expect_header("cache-control", "");
    h.send(Req::get("/api/unknown"))
        .await
        .expect(404, r#"{"error":"Not found"}"#)
        .expect_header("cache-control", "no-store")
        .expect_header("x-content-type-options", "");
    h.close().await;
}

#[tokio::test]
async fn health_reports_liveness_without_access_checks() {
    let h = start(options(test_config())).await;
    h.send(Req::get("/healthz").origin("https://foreign.test"))
        .await
        .expect(200, r#"{"status":"ok"}"#)
        .expect_header("cache-control", "no-store")
        .expect_header("content-type", "application/json; charset=utf-8");
    h.send(Req::post("/healthz"))
        .await
        .expect(405, r#"{"error":"Method not allowed"}"#)
        .expect_header("allow", "GET")
        .expect_header("cache-control", "no-store");
    h.close().await;
}
