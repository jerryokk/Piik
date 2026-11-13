//! Runtime capability reporting over HTTP.

use super::*;
use crate::tests::CLOSE_TIMEOUT;

fn capabilities(head: &str) -> String {
    format!("{{{head}}}")
}

#[tokio::test]
async fn application_pages_cannot_be_embedded() {
    let server = new_server(options(test_config()));
    for path in ["/", "/api/missing", "/healthz"] {
        let reply = call(server.inner(), &Req::get(path)).await;
        reply.expect_status(503).expect_header("retry-after", "1");
        reply
            .expect_header("content-security-policy", "frame-ancestors 'none'")
            .expect_header("x-frame-options", "DENY");
    }
    server.listen().await.expect("listen");
    for path in ["/", "/api/missing", "/healthz"] {
        call(server.inner(), &Req::get(path))
            .await
            .expect_header("content-security-policy", "frame-ancestors 'none'")
            .expect_header("x-frame-options", "DENY");
    }
    server.close(CLOSE_TIMEOUT).await.expect("close");
}

#[tokio::test]
async fn capabilities_report_services_disabled_by_default() {
    let h = start(options(test_config())).await;
    h.send(Req::get("/api/capabilities"))
        .await
        .expect(200, &capabilities(r#""sfu":false,"natPrediction":false"#));
    h.close().await;
}

#[tokio::test]
async fn capabilities_report_the_sfu_only_policy() {
    let mut config = test_config();
    config.sfu = Some(sfu_config("", true));
    config.nat_prediction_enabled = true;
    let mut options = options(config);
    options.sfu = Some(no_media());
    let h = start(options).await;
    h.send(Req::get("/api/capabilities")).await.expect(
        200,
        &capabilities(r#""sfu":true,"sfuOnly":true,"natPrediction":false"#),
    );
    h.close().await;
}

#[tokio::test]
async fn capabilities_report_optional_nat_prediction_without_configuration() {
    let mut config = test_config();
    config.stun_urls = vec!["stun:share.example.test:3478".to_owned()];
    config.nat_prediction_enabled = true;
    config.stun_listen_addresses = vec!["127.0.0.1:0".parse().expect("address"); 3];
    let h = start(options(config)).await;
    h.send(Req::get("/api/capabilities"))
        .await
        .expect(200, &capabilities(r#""sfu":false,"natPrediction":true"#))
        .expect_header("cache-control", "no-store")
        .expect_header("x-content-type-options", "nosniff");
    h.send(Req::post("/api/capabilities"))
        .await
        .expect(405, r#"{"error":"Method not allowed"}"#)
        .expect_header("allow", "GET");
    h.close().await;
}

#[tokio::test]
async fn sfu_media_starts_signaling_with_server_media() {
    for ending in [false, true] {
        let mut config = test_config();
        config.sfu = Some(sfu_config("", false));
        let mut options = options(config);
        options.sfu = Some(no_media());
        let h = start(options).await;
        assert!(h.server.inner().has_signaling());
        h.send(Req::get("/api/capabilities"))
            .await
            .expect(200, &capabilities(r#""sfu":true,"natPrediction":false"#));
        let closed = if ending {
            h.server.end(CLOSE_TIMEOUT).await
        } else {
            h.server.close(CLOSE_TIMEOUT).await
        };
        closed.expect("shutdown");
    }
}

#[test]
fn injected_sfu_media_requires_an_sfu_configuration() {
    let mut config = test_config();
    config.sfu = Some(sfu_config("", false));
    assert!(
        crate::Server::new(options(config)).is_ok(),
        "SFU config without injected media embeds the SFU"
    );
    let mut options = options(test_config());
    options.sfu = Some(no_media());
    assert!(
        crate::Server::new(options).is_err(),
        "SFU media without config"
    );
}
