//! Intentional Host pause snapshot and update symmetry.

use super::*;

#[test]
fn signal_snapshots_intentional_pause_across_viewer_replacement_and_clears_it_on_resume_and_stop() {
    let h = Harness::start(HarnessOptions::default());
    let generation = "pause_share_generation_12345678";
    let host = h.open_client();
    let host_auth = authenticate(
        &host,
        &h.room,
        "host",
        "pause-host-client",
        1,
        generation,
        presence(),
    );
    assert!(
        !has_key(&host_auth.raw, "hostPaused"),
        "hostPaused must be absent for the host"
    );
    let viewer = h.open_client();
    let viewer_auth = authenticate(
        &viewer,
        &h.room,
        "viewer",
        "pause-viewer-client",
        1,
        "",
        presence(),
    );
    assert_eq!(viewer_auth.host_paused(), Some(false));
    commit_prepared_route(&viewer);
    set_sharing_paused(&host, generation, true);
    expect_equal(
        &viewer.next("host-status").raw,
        r#"{"type":"host-status","online":true,"paused":true}"#,
    );
    h.close_client(&host);
    expect_match(
        &viewer.next("host-status").raw,
        r#"{"online":false,"paused":false}"#,
    );
    let active = h.open_client();
    authenticate(
        &active,
        &h.room,
        "host",
        "pause-host-client",
        1,
        generation,
        paused(true),
    );
    expect_match(
        &viewer.next("host-status").raw,
        r#"{"online":true,"paused":true}"#,
    );
    let replacement = h.open_client();
    let replacement_auth = authenticate(
        &replacement,
        &h.room,
        "viewer",
        "pause-viewer-client",
        1,
        "",
        presence(),
    );
    assert_eq!(replacement_auth.host_paused(), Some(true));
    set_sharing_paused(&active, generation, false);
    expect_match(
        &replacement.next("host-status").raw,
        r#"{"online":true,"paused":false}"#,
    );
    set_sharing_paused(&active, generation, true);
    expect_match(&replacement.next("host-status").raw, r#"{"paused":true}"#);
    active.send_json(json!({"type": "stop-sharing", "shareGeneration": generation}));
    replacement.next("sharing-stopped");
    expect_match(
        &replacement.next("host-status").raw,
        r#"{"online":false,"paused":false}"#,
    );
    let next_host = h.open_client();
    authenticate(
        &next_host,
        &h.room,
        "host",
        "pause-host-client",
        1,
        "next_pause_share_generation_12345678",
        presence(),
    );
    replacement.next("sharing-stopped");
    expect_match(
        &replacement.next("host-status").raw,
        r#"{"online":false,"paused":false}"#,
    );
    expect_match(
        &replacement.next("host-status").raw,
        r#"{"online":true,"paused":false}"#,
    );
}

#[test]
fn signal_applies_symmetric_pause_updates_only_to_exact_current_host_share() {
    let h = Harness::start(HarnessOptions::default());
    let generation = "hybrid_pause_share_generation_12345678";
    let host = h.open_client();
    authenticate(
        &host,
        &h.room,
        "host",
        "hybrid-pause-host-client",
        1,
        generation,
        presence(),
    );
    let viewer = h.open_client();
    authenticate(
        &viewer,
        &h.room,
        "viewer",
        "hybrid-pause-viewer-client",
        1,
        "",
        presence(),
    );
    set_sharing_paused(&host, generation, true);
    expect_match(
        &viewer.next("host-status").raw,
        r#"{"online":true,"paused":true}"#,
    );
    set_sharing_paused(&host, generation, false);
    expect_match(
        &viewer.next("host-status").raw,
        r#"{"online":true,"paused":false}"#,
    );
    set_sharing_paused(&host, "stale_pause_share_generation_12345678", true);
    expect_close_code(&host, 4001);
}

#[test]
fn signal_retains_authoritative_pause_when_reconnecting_host_advertises_unpaused() {
    let h = Harness::start(HarnessOptions::default());
    let generation = "reconnect_resume_share_generation_12345678";
    let client_id = "reconnect-resume-host-client";
    let host = h.open_client();
    authenticate(&host, &h.room, "host", client_id, 1, generation, presence());
    let viewer = h.open_client();
    authenticate(
        &viewer,
        &h.room,
        "viewer",
        "reconnect-resume-viewer-client",
        1,
        "",
        presence(),
    );
    commit_prepared_route(&viewer);
    set_sharing_paused(&host, generation, true);
    expect_match(
        &viewer.next("host-status").raw,
        r#"{"online":true,"paused":true}"#,
    );
    h.close_client(&host);
    expect_match(
        &viewer.next("host-status").raw,
        r#"{"online":false,"paused":false}"#,
    );
    let reconnected = h.open_client();
    authenticate(
        &reconnected,
        &h.room,
        "host",
        client_id,
        1,
        generation,
        paused(false),
    );
    expect_equal(
        &reconnected.next("pause-sharing-source").raw,
        &json!({"type": "pause-sharing-source", "shareGeneration": generation}).to_string(),
    );
    expect_match(
        &viewer.next("host-status").raw,
        r#"{"online":true,"paused":true}"#,
    );
    set_sharing_paused(&reconnected, generation, false);
    expect_match(
        &viewer.next("host-status").raw,
        r#"{"online":true,"paused":false}"#,
    );
}
