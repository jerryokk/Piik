use super::output::{connection_registered, origin_from_log_line};
use super::*;

#[test]
fn origin_from_cloudflared_log() {
    for line in [
        r#"{"level":"info","message":"|  https://small-bright-room.trycloudflare.com  |"}"#,
        "Visit HTTPS://SMALL-BRIGHT-ROOM.TRYCLOUDFLARE.COM now",
    ] {
        assert_eq!(
            origin_from_log_line(line).as_deref(),
            Some("https://small-bright-room.trycloudflare.com"),
            "{line}"
        );
    }
    for line in [
        "https://example.com",
        "https://trycloudflare.com",
        "https://bad_.trycloudflare.com",
    ] {
        assert_eq!(origin_from_log_line(line), None, "{line}");
    }
}

#[test]
fn registered_connection_from_cloudflared_log() {
    assert!(connection_registered(
        r#"{"level":"info","message":"Registered tunnel connection","protocol":"quic"}"#
    ));
    assert!(
        !connection_registered(r#"{"message":"Requesting new quick Tunnel"}"#),
        "pre-connection log was accepted as ready"
    );
}

#[test]
fn local_origin_must_be_loopback_http() {
    for value in ["http://127.0.0.1:8787", "http://[::1]:8787"] {
        assert!(
            validate_local_origin(value).is_ok(),
            "valid Local origin {value}"
        );
    }
    for value in [
        "https://127.0.0.1:8787",
        "http://192.168.1.2:8787",
        "http://127.0.0.1:8787/path",
        "http://127.0.0.1",
        "http://user@127.0.0.1:8787",
        "http://127.0.0.1:0",
        "http://127.0.0.1:65536",
        "http://localhost:8787",
    ] {
        assert!(
            validate_local_origin(value).is_err(),
            "invalid Local origin accepted: {value}"
        );
    }
}

#[test]
fn temporary_config_is_minimal() {
    let directory = tempfile::tempdir().unwrap();
    let path = write_temporary_config(Some(directory.path())).unwrap();
    assert_eq!(
        path.parent(),
        Some(directory.path()),
        "config escaped caller directory"
    );
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        name.starts_with("piik-cloudflared-") && name.ends_with(".yml"),
        "{name}"
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "metrics: 127.0.0.1:0\n"
    );
    let kept = path.to_path_buf();
    drop(path);
    assert!(!kept.exists(), "temporary config was not removed");
}

#[test]
fn packaged_executable_is_beside_the_app() {
    let path = packaged_executable().unwrap();
    assert!(path.ends_with(std::path::Path::new("runtime/tunnel/cloudflared.exe")));
}

#[tokio::test]
async fn missing_tunnel_explains_how_to_restore_the_package() {
    let directory = tempfile::tempdir().unwrap();
    let error = Startup::new(
        directory.path().join("missing-tunnel"),
        "http://127.0.0.1:8787",
    )
    .start(&CancellationToken::new())
    .await
    .unwrap_err();
    assert!(
        error.io_error().is_some(),
        "missing tunnel lost its cause: {error}"
    );
    assert_eq!(error.reason(), Some(Reason::TunnelUnavailable));
    assert!(
        error
            .to_string()
            .contains("extract the complete Piik archive"),
        "{error}"
    );
}

#[tokio::test]
async fn invalid_origin_is_a_start_failure() {
    let executable = std::env::current_exe().unwrap();
    let error = Startup::new(executable, "http://192.168.1.2:8787")
        .start(&CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(error.reason(), Some(Reason::TunnelStart));
    assert_eq!(
        error.to_string(),
        "tunnel-start\npublic tunnel Local origin is invalid"
    );
}

#[tokio::test]
async fn canceled_parent_returns_before_starting() {
    let stop = CancellationToken::new();
    stop.cancel();
    let error = Startup::new("missing", "http://127.0.0.1:8787")
        .start(&stop)
        .await
        .unwrap_err();
    assert!(error.is_canceled() && error.reason().is_none(), "{error}");
}
