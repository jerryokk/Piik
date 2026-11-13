//! Wire contract cases, following `tests/protocol/`.

use piik_protocol::*;

/// One JSON object key with its raw encoded value, so the tests reproduce
/// exact wire shapes including key presence.
type Member = (&'static str, String);

fn object(members: &[Member]) -> Vec<u8> {
    let body: Vec<String> = members
        .iter()
        .map(|(key, raw)| format!("{key:?}:{raw}"))
        .collect();
    format!("{{{}}}", body.join(",")).into_bytes()
}

fn with(base: &[Member], changes: &[(&'static str, &str)]) -> Vec<Member> {
    let mut out = base.to_vec();
    for (key, raw) in changes {
        match out.iter_mut().find(|(existing, _)| existing == key) {
            Some(entry) => entry.1 = (*raw).to_owned(),
            None => out.push((key, (*raw).to_owned())),
        }
    }
    out
}

fn drop_keys(base: &[Member], keys: &[&str]) -> Vec<Member> {
    base.iter()
        .filter(|(key, _)| !keys.contains(key))
        .cloned()
        .collect()
}

const ASSIGNMENT_NONE: &str =
    r#"{"upstream":{"kind":"none"},"childPeerIds":[],"sfuPublicationGeneration":null}"#;
const QUALITY_SETTINGS: &str = r#"{"resolution":{"width":1920,"height":1080},"maxFramerate":60,"maxBitrate":8000000,"degradationPreference":"maintain-resolution"}"#;
const ROUTE_POLICY: &str =
    r#"{"peerOnly":false,"topologyOptimization":true,"natPrediction":false}"#;
const ICE_CONFIG: &str = r#"{"iceServers":[],"natPredictionStunUrls":[]}"#;

fn authenticated_host() -> Vec<Member> {
    [
        ("type", r#""authenticated""#),
        ("protocol", r#""piik-v24""#),
        ("peerId", r#""host_12345678""#),
        ("maxViewers", "8"),
        ("endpointMediaCopyCapacity", "2"),
        ("hostOnline", "true"),
        ("connectionId", "null"),
        ("iceConfig", ICE_CONFIG),
        ("routePolicy", ROUTE_POLICY),
        ("codeEntryPolicy", r#""open""#),
        (
            "viewerAuthorizationGeneration",
            r#""viewer_generation_12345678""#,
        ),
        ("mediaMode", r#""peer-assisted""#),
        ("shareGeneration", r#""share_generation_12345678""#),
        ("routeRevision", "0"),
        ("routeAssignment", ASSIGNMENT_NONE),
        ("qualitySettings", QUALITY_SETTINGS),
        ("role", r#""host""#),
        ("viewerPasswordEnabled", "false"),
    ]
    .into_iter()
    .map(|(key, raw)| (key, raw.to_owned()))
    .collect()
}

#[track_caller]
fn assert_server(name: &str, data: &[u8], want: bool) {
    let result = decode_server_message(data);
    let text = String::from_utf8_lossy(data);
    match result {
        Err(error) if want => panic!("{name}: expected {text} to decode, got {error}"),
        Ok(_) if !want => panic!("{name}: expected {text} to be rejected"),
        _ => {}
    }
}

#[track_caller]
fn assert_client(name: &str, data: &[u8], want: bool) {
    let result = decode_client_message(data);
    let text = String::from_utf8_lossy(data);
    match result {
        Err(error) if want => panic!("{name}: expected {text} to decode, got {error}"),
        Ok(_) if !want => panic!("{name}: expected {text} to be rejected"),
        _ => {}
    }
}

#[test]
fn runtime_capabilities() {
    let caps = |f: fn(&mut RuntimeCapabilities)| {
        let mut value = RuntimeCapabilities::default();
        f(&mut value);
        value
    };
    let cases: [(&str, RuntimeCapabilities); 3] = [
        ("{}", RuntimeCapabilities::default()),
        (
            r#"{"sfu":true,"natPrediction":true}"#,
            caps(|c| {
                c.sfu = true;
                c.nat_prediction = true;
            }),
        ),
        (
            r#"{"sfu":true,"sfuOnly":true}"#,
            caps(|c| {
                c.sfu = true;
                c.sfu_only = true;
            }),
        ),
    ];
    for (data, want) in cases {
        let got: RuntimeCapabilities = decode(data.as_bytes()).expect(data);
        assert_eq!(got, want, "{data}");
    }
    for data in [
        "null",
        "[]",
        "true",
        r#"{"sfu":true,"extra":{"enabled":true}}"#,
        r#"{"sfu":null}"#,
        r#"{"sfu":"true"}"#,
        r#"{"sfuOnly":null}"#,
        r#"{"sfuOnly":"true"}"#,
        r#"{"natPrediction":null}"#,
        r#"{"natPrediction":1}"#,
        r#"{"natPrediction":true,"SFU":true}"#,
        r#"{"unknown":true}"#,
    ] {
        assert!(
            decode::<RuntimeCapabilities>(data.as_bytes()).is_err(),
            "expected {data} to be rejected"
        );
    }
}

#[test]
fn registered_reactions_are_the_only_commands_and_events() {
    for id in ROOM_REACTION_IDS.iter().copied().chain(["future-effect"]) {
        let target = if id == "tomato" || id == "poop" {
            r#","targetPeerId":"target_peer_1234""#
        } else {
            ""
        };
        let raw = format!(
            r#"{{"type":"send-room-interaction","requestId":"request_1234","payload":{{"kind":"reaction","reaction":"{id}"{target}}}}}"#
        );
        assert_client(id, raw.as_bytes(), id != "future-effect");
        let event = format!(
            r#"{{"type":"room-interaction","id":"event_1234","requestId":"request_1234","occurredAt":1,"sender":{{"peerId":"sender_peer_1234","role":"viewer","displayName":"Friend"}},"payload":{{"kind":"reaction","reaction":"{id}"{target}}}}}"#
        );
        assert_server(id, event.as_bytes(), id != "future-effect");
    }
}

#[test]
fn strict_member_names_cannot_populate_forbidden_or_optional_fields() {
    assert!(decode::<MediaRouteUpstream>(br#"{"kind":"none","peerid":""}"#).is_err());
    assert!(decode::<RouteDiagnosticParent>(br#"{"kind":"host","Ordinal":0}"#).is_err());
    assert!(decode::<ParticipantPresenceEntry>(br#"{"role":"host","peerId":"host_12345678","displayName":"Host","upstream":{"kind":"none"},"mediaready":false}"#).is_err());
    assert!(decode::<ParticipantPresenceEntry>(br#"{"role":"viewer","peerId":"viewer_12345678","displayName":"Viewer","upstream":{"kind":"none"},"mediaready":false}"#).is_err());
    assert_server(
        "folded route policy cannot be ignored by its default",
        &object(&with(
            &drop_keys(&authenticated_host(), &["routePolicy"]),
            &[("routepolicy", ROUTE_POLICY)],
        )),
        false,
    );
}

#[test]
fn reaction_events_keep_payload_and_authority_bounds() {
    for role in ["host", "viewer", "", "HOST", "admin"] {
        let raw = format!(
            r#"{{"type":"room-interaction","id":"event_1234","requestId":"request_1234","occurredAt":1,"sender":{{"peerId":"sender_peer_1234","role":"{role}","displayName":"Friend"}},"payload":{{"kind":"reaction","reaction":"wave"}}}}"#
        );
        assert_server(role, raw.as_bytes(), role == "host" || role == "viewer");
    }
    for payload in [
        r#"{"kind":"reaction","reaction":""}"#,
        r#"{"kind":"reaction","reaction":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}"#,
        r#"{"kind":"reaction","reaction":"future effect"}"#,
        r#"{"kind":"reaction","reaction":"👋"}"#,
        r#"{"kind":"reaction","reaction":"future-effect","targetPeerId":"bad"}"#,
        r#"{"kind":"reaction","reaction":"future-effect","text":"hidden command"}"#,
        r#"{"kind":"reaction","reaction":"future-effect"}"#,
        r#"{"kind":"reaction","reaction":"tomato"}"#,
        r#"{"kind":"future-kind","reaction":"wave"}"#,
    ] {
        let raw = format!(
            r#"{{"type":"room-interaction","id":"event_future_1234","requestId":"request_future_1234","occurredAt":1,"sender":{{"peerId":"sender_peer_1234","role":"viewer","displayName":"Friend"}},"payload":{payload}}}"#
        );
        assert_server(payload, raw.as_bytes(), false);
    }
    assert_server(
        "registered reaction with invalid sender",
        br#"{"type":"room-interaction","id":"event_1234","requestId":"request_1234","occurredAt":1,"sender":{"peerId":"bad","role":"viewer","displayName":"Friend"},"payload":{"kind":"reaction","reaction":"wave"}}"#,
        false,
    );
}

#[test]
fn authenticated_viewer_limits() {
    for (raw, want) in [
        ("1", true),
        ("8", true),
        ("20", true),
        ("0", false),
        ("1.5", false),
        ("21", false),
    ] {
        assert_server(
            raw,
            &object(&with(&authenticated_host(), &[("maxViewers", raw)])),
            want,
        );
    }
}

#[test]
fn authenticated_endpoint_capacity() {
    for (raw, want) in [
        ("1", true),
        ("2", true),
        ("3", true),
        ("0", false),
        ("4", false),
        ("1.5", false),
    ] {
        assert_server(
            raw,
            &object(&with(
                &authenticated_host(),
                &[("endpointMediaCopyCapacity", raw)],
            )),
            want,
        );
    }
    assert_server(
        "capacity absent",
        &object(&drop_keys(
            &authenticated_host(),
            &["endpointMediaCopyCapacity"],
        )),
        false,
    );
}

#[test]
fn authenticated_password_state_is_host_scoped() {
    assert_server(
        "host carries the flag",
        &object(&authenticated_host()),
        true,
    );
    assert_server(
        "host without the flag",
        &object(&drop_keys(
            &authenticated_host(),
            &["viewerPasswordEnabled"],
        )),
        false,
    );
    let viewer = drop_keys(
        &with(
            &authenticated_host(),
            &[("role", r#""viewer""#), ("peerId", r#""viewer_12345678""#)],
        ),
        &["viewerPasswordEnabled"],
    );
    assert_server("viewer without the flag", &object(&viewer), true);
    assert_server(
        "viewer with the flag",
        &object(&with(&viewer, &[("viewerPasswordEnabled", "true")])),
        false,
    );
    assert_server(
        "viewer with hostPaused",
        &object(&with(&viewer, &[("hostPaused", "true")])),
        true,
    );
}

#[test]
fn authenticated_room_revision() {
    let host = authenticated_host();
    assert_server(
        "revision at the ceiling",
        &object(&with(&host, &[("routeRevision", "9007199254740991")])),
        true,
    );
    assert_server(
        "revision past the ceiling",
        &object(&with(&host, &[("routeRevision", "9007199254740992")])),
        false,
    );
    assert_server(
        "absent route assignment",
        &object(&drop_keys(&host, &["routeAssignment"])),
        false,
    );
    assert_server(
        "absent media mode",
        &object(&drop_keys(&host, &["mediaMode"])),
        false,
    );
    assert_server(
        "quality settings past the numeric representation",
        &object(&with(
            &host,
            &[(
                "qualitySettings",
                r#"{"resolution":{"width":1920,"height":1080},"maxFramerate":60,"maxBitrate":4294967296,"degradationPreference":"maintain-resolution"}"#,
            )],
        )),
        false,
    );
}

#[test]
fn authenticated_ice_config() {
    for (name, raw, want) in [
        (
            "empty",
            r#"{"iceServers":[],"natPredictionStunUrls":[]}"#,
            true,
        ),
        (
            "one stun url",
            r#"{"iceServers":[{"urls":"stun:stun.example.test:3478"}],"natPredictionStunUrls":[]}"#,
            true,
        ),
        (
            "a list of stun urls",
            r#"{"iceServers":[{"urls":["stun:a.example.test:3478","stun:b.example.test:3478"]}],"natPredictionStunUrls":[]}"#,
            true,
        ),
        ("missing nat prediction urls", r#"{"iceServers":[]}"#, false),
        (
            "turn url",
            r#"{"iceServers":[{"urls":"turn:relay.example.test:3478?transport=udp"}],"natPredictionStunUrls":[]}"#,
            false,
        ),
        (
            "turn credentials",
            r#"{"iceServers":[{"urls":"turn:relay.example.test:3478","username":"1787076000","credential":"credential"}],"natPredictionStunUrls":[]}"#,
            false,
        ),
        (
            "stuns url",
            r#"{"iceServers":[{"urls":"stuns:stun.example.test:5349"}],"natPredictionStunUrls":[]}"#,
            false,
        ),
        (
            "too many nat prediction urls",
            r#"{"iceServers":[],"natPredictionStunUrls":["stun:a.example.test:3478","stun:b.example.test:3478","stun:c.example.test:3478"]}"#,
            false,
        ),
    ] {
        assert_server(
            name,
            &object(&with(&authenticated_host(), &[("iceConfig", raw)])),
            want,
        );
    }
}

#[test]
fn route_status_crossed_union() {
    for (name, raw, want) in [
        (
            "waiting on admission",
            r#"{"type":"route-status","revision":3,"state":"waiting","reason":"sfu-admission"}"#,
            true,
        ),
        (
            "failed on exhaustion",
            r#"{"type":"route-status","revision":4,"state":"failed","reason":"route-exhausted"}"#,
            true,
        ),
        (
            "waiting on exhaustion",
            r#"{"type":"route-status","revision":3,"state":"waiting","reason":"route-exhausted"}"#,
            false,
        ),
        (
            "failed on admission",
            r#"{"type":"route-status","revision":3,"state":"failed","reason":"sfu-admission"}"#,
            false,
        ),
        (
            "with a retry hint",
            r#"{"type":"route-status","revision":3,"state":"waiting","reason":"sfu-admission","retryAfterMs":1000}"#,
            false,
        ),
        (
            "with a peer id",
            r#"{"type":"route-status","revision":3,"state":"waiting","reason":"sfu-admission","peerId":"private-peer-id"}"#,
            false,
        ),
    ] {
        assert_server(name, raw.as_bytes(), want);
    }
}

#[test]
fn viewer_presence() {
    let viewer = r#"{"role":"viewer","peerId":"viewer_12345678","displayName":"小明","upstream":{"kind":"peer","peerId":"viewer_parent_12345678"}}"#;
    let cases = [
        ("sfu upstream with mediaReady".to_owned(), r#"{"type":"viewer-presence","viewers":[{"role":"viewer","peerId":"viewer_12345678","displayName":"小明","upstream":{"kind":"sfu"},"mediaReady":true}]}"#.to_owned(), true),
        ("mediaReady false".to_owned(), r#"{"type":"viewer-presence","viewers":[{"role":"viewer","peerId":"viewer_12345678","displayName":"小明","upstream":{"kind":"peer","peerId":"viewer_parent_12345678"},"mediaReady":false}]}"#.to_owned(), false),
        ("duplicate peer ids".to_owned(), format!(r#"{{"type":"viewer-presence","viewers":[{viewer},{viewer}]}}"#), false),
        ("leaked topology".to_owned(), r#"{"type":"viewer-presence","viewers":[{"role":"viewer","peerId":"viewer_12345678","displayName":"小明","upstream":{"kind":"none"},"mediaTopology":"peer-relay"}]}"#.to_owned(), false),
        ("leaked address".to_owned(), r#"{"type":"viewer-presence","viewers":[{"role":"viewer","peerId":"viewer_12345678","displayName":"小明","upstream":{"kind":"none"},"ip":"203.0.113.1"}]}"#.to_owned(), false),
        ("host entry".to_owned(), format!(r#"{{"type":"viewer-presence","viewers":[{viewer},{{"role":"host","peerId":"host_12345678","displayName":"分享者","upstream":{{"kind":"none"}}}}]}}"#), true),
        ("host entry with an upstream".to_owned(), r#"{"type":"viewer-presence","viewers":[{"role":"host","peerId":"host_12345678","displayName":"分享者","upstream":{"kind":"sfu"}}]}"#.to_owned(), false),
        ("host entry with mediaReady".to_owned(), r#"{"type":"viewer-presence","viewers":[{"role":"host","peerId":"host_12345678","displayName":"分享者","upstream":{"kind":"none"},"mediaReady":true}]}"#.to_owned(), false),
    ];
    for (name, raw, want) in cases {
        assert_server(&name, raw.as_bytes(), want);
    }
}

fn name(text: &str) -> DisplayName {
    DisplayName::new(text).expect("canonical display name")
}

#[test]
fn viewer_presence_constructors() {
    let host = ParticipantPresenceEntry::host("host_12345678", name("分享者"));
    assert_eq!(host.upstream, MediaRouteUpstream::None);
    assert!(!host.media_ready);
    let encoded = encode(&ParticipantPresenceEntry::viewer(
        "viewer_12345678",
        name("小明"),
        MediaRouteUpstream::Sfu,
        false,
    ));
    assert!(!String::from_utf8(encoded).unwrap().contains("mediaReady"));
    let encoded = encode(&ParticipantPresenceEntry::viewer(
        "viewer_12345678",
        name("小明"),
        MediaRouteUpstream::Sfu,
        true,
    ));
    assert!(
        String::from_utf8(encoded)
            .unwrap()
            .contains(r#""mediaReady":true"#)
    );
}

#[test]
fn route_diagnostic_snapshot() {
    let child_one = r#"{"ordinal":1,"parent":{"kind":"host"},"effectiveCapacity":2,"childCount":1,"demandAgeMs":100,"queueWaitMs":10,"candidateStartMs":20,"firstDecodedFrameMs":80,"finalMs":80,"finalRoute":"direct","rejectionBucket":"none","quality":{"eligibleWindows":3,"eligibleDurationMs":6000,"freezeWindows":1,"freezeCount":1,"freezeDurationMs":250,"pauseCount":0,"pauseDurationMs":0}}"#;
    let child_two = |parent: &str| {
        format!(
            r#"{{"ordinal":2,"parent":{parent},"effectiveCapacity":0,"childCount":0,"demandAgeMs":90,"queueWaitMs":null,"candidateStartMs":null,"firstDecodedFrameMs":null,"finalMs":null,"finalRoute":"waiting","rejectionBucket":"sfu-admission","quality":null}}"#
        )
    };
    let snapshot = |children: &str, operation: &str| {
        format!(
            r#"{{"type":"route-diagnostic-snapshot","snapshot":{{"children":{children},"operation":{operation}}}}}"#
        )
        .into_bytes()
    };
    let operation =
        r#"{"childOrdinal":2,"reason":"join","stage":"admission","cursor":1,"candidateCount":2}"#;
    let viewer_parent = child_two(r#"{"kind":"viewer","ordinal":1}"#);
    let pair = format!("[{child_one},{viewer_parent}]");

    assert_server("full snapshot", &snapshot(&pair, operation), true);
    assert_server("empty snapshot", &snapshot("[]", "null"), true);
    assert_server(
        "leaked peer id",
        br#"{"type":"route-diagnostic-snapshot","snapshot":{"children":[],"operation":null,"peerId":"viewer_private_12345678"}}"#,
        false,
    );
    assert_server(
        "leaked connection id",
        &snapshot(
            r#"[{"ordinal":1,"parent":{"kind":"host"},"effectiveCapacity":2,"childCount":1,"demandAgeMs":100,"queueWaitMs":10,"candidateStartMs":20,"firstDecodedFrameMs":80,"finalMs":80,"finalRoute":"direct","rejectionBucket":"none","quality":null,"connectionId":"connection_private_12345678"}]"#,
            "null",
        ),
        false,
    );
    assert_server(
        "negative freeze count",
        &snapshot(
            r#"[{"ordinal":1,"parent":{"kind":"host"},"effectiveCapacity":2,"childCount":1,"demandAgeMs":100,"queueWaitMs":10,"candidateStartMs":20,"firstDecodedFrameMs":80,"finalMs":80,"finalRoute":"direct","rejectionBucket":"none","quality":{"eligibleWindows":3,"eligibleDurationMs":6000,"freezeWindows":1,"freezeCount":-1,"freezeDurationMs":250,"pauseCount":0,"pauseDurationMs":0}}]"#,
            "null",
        ),
        false,
    );
    let self_parent = format!(
        "[{child_one},{}]",
        child_two(r#"{"kind":"viewer","ordinal":2}"#)
    );
    assert_server("self parent", &snapshot(&self_parent, "null"), false);
    let unknown = format!(
        "[{child_one},{}]",
        child_two(r#"{"kind":"viewer","ordinal":7}"#)
    );
    assert_server("unknown parent ordinal", &snapshot(&unknown, "null"), false);
    let duplicate = format!("[{child_one},{child_one}]");
    assert_server("duplicate ordinals", &snapshot(&duplicate, "null"), false);
    let only_one = format!("[{child_one}]");
    assert_server(
        "operation names an absent child",
        &snapshot(&only_one, operation),
        false,
    );
    assert_server(
        "cursor past the candidate count",
        &snapshot(
            &pair,
            r#"{"childOrdinal":2,"reason":"join","stage":"admission","cursor":2,"candidateCount":2}"#,
        ),
        false,
    );
    assert_server(
        "raw rejection bucket on the operation",
        &snapshot(
            &pair,
            r#"{"childOrdinal":2,"reason":"join","stage":"admission","cursor":1,"candidateCount":2,"rejectionBucket":"raw-error"}"#,
        ),
        false,
    );
    assert_server(
        "parent ordinal on a host parent",
        &snapshot(
            r#"[{"ordinal":1,"parent":{"kind":"host","ordinal":1},"effectiveCapacity":2,"childCount":1,"demandAgeMs":null,"queueWaitMs":null,"candidateStartMs":null,"firstDecodedFrameMs":null,"finalMs":null,"finalRoute":"direct","rejectionBucket":"none","quality":null}]"#,
            "null",
        ),
        false,
    );
}

#[test]
fn participant_route_assignment() {
    let children: Vec<String> = (0..=MAX_VIEWERS_PER_ROOM_LIMIT)
        .map(|index| format!(r#""child_{index:08}""#))
        .collect();
    let too_many = format!(
        r#"{{"upstream":{{"kind":"none"}},"childPeerIds":[{}],"sfuPublicationGeneration":null}}"#,
        children.join(",")
    );
    let route_update = |assignment: &str| {
        format!(
            r#"{{"type":"route-update","revision":9,"phase":"active","assignment":{assignment}}}"#
        )
        .into_bytes()
    };
    for (name, assignment, want) in [
        ("no upstream", ASSIGNMENT_NONE, true),
        (
            "peer upstream",
            r#"{"upstream":{"kind":"peer","peerId":"parent_12345678"},"childPeerIds":["child_12345678","child_87654321"],"sfuPublicationGeneration":null}"#,
            true,
        ),
        (
            "sfu upstream",
            r#"{"upstream":{"kind":"sfu"},"childPeerIds":[],"sfuPublicationGeneration":"generation_12345678"}"#,
            true,
        ),
        (
            "sfu upstream without a generation",
            r#"{"upstream":{"kind":"sfu"},"childPeerIds":[],"sfuPublicationGeneration":null}"#,
            false,
        ),
        (
            "peer upstream owning a generation",
            r#"{"upstream":{"kind":"peer","peerId":"parent_12345678"},"childPeerIds":[],"sfuPublicationGeneration":"generation_12345678"}"#,
            false,
        ),
        ("too many children", too_many.as_str(), false),
        (
            "duplicate children",
            r#"{"upstream":{"kind":"none"},"childPeerIds":["child_12345678","child_12345678"],"sfuPublicationGeneration":null}"#,
            false,
        ),
        (
            "absent generation key",
            r#"{"upstream":{"kind":"none"},"childPeerIds":[]}"#,
            false,
        ),
        (
            "peer id on a none upstream",
            r#"{"upstream":{"kind":"none","peerId":"parent_12345678"},"childPeerIds":[],"sfuPublicationGeneration":null}"#,
            false,
        ),
    ] {
        assert_server(name, &route_update(assignment), want);
    }
    assert_server(
        "prepare without a candidate",
        format!(r#"{{"type":"route-update","revision":9,"phase":"prepare","assignment":{ASSIGNMENT_NONE}}}"#).as_bytes(),
        false,
    );
    assert_server(
        "active with a candidate",
        format!(r#"{{"type":"route-update","revision":9,"phase":"active","assignment":{ASSIGNMENT_NONE},"candidate":{{"childPeerId":"child_12345678","connectionId":"connection_12345678","transport":"direct","qualityProbe":false}}}}"#).as_bytes(),
        false,
    );
    assert_server(
        "prepare with a room-wide field",
        format!(r#"{{"type":"route-update","revision":9,"phase":"prepare","assignment":{ASSIGNMENT_NONE},"roomAssignments":[]}}"#).as_bytes(),
        false,
    );
}

#[test]
fn prepared_route_candidate() {
    let base = r#""childPeerId":"child_12345678","connectionId":"connection_12345678","transport":"direct","qualityProbe":false"#;
    let prepared = |candidate: String| {
        format!(r#"{{"type":"route-update","revision":9,"phase":"prepare","assignment":{ASSIGNMENT_NONE},"candidate":{candidate}}}"#)
            .into_bytes()
    };
    for (attempt, want) in [
        (r#"{"current":1,"total":3}"#, true),
        (r#"{"current":2,"total":3}"#, true),
        (r#"{"current":3,"total":3}"#, true),
        (r#"{"current":0,"total":3}"#, false),
        (r#"{"current":4,"total":3}"#, false),
        (r#"{"current":1,"total":4}"#, true),
        (r#"{"current":4,"total":4}"#, true),
        (r#"{"current":5,"total":4}"#, false),
        (r#"{"current":1,"total":5}"#, true),
        (r#"{"current":1,"total":0}"#, false),
        (r#"{"current":1,"total":9007199254740992}"#, false),
        (r#"{"current":1.5,"total":3}"#, false),
    ] {
        assert_server(
            attempt,
            &prepared(format!(r#"{{{base},"connectionAttempt":{attempt}}}"#)),
            want,
        );
    }
    assert_server(
        "candidate without an attempt",
        &prepared(format!("{{{base}}}")),
        true,
    );
    assert_server(
        "candidate with an unknown key",
        &prepared(format!(r#"{{{base},"unexpected":null}}"#)),
        false,
    );
    assert_server(
        "candidate with a null attempt",
        &prepared(format!(r#"{{{base},"connectionAttempt":null}}"#)),
        false,
    );
}

#[test]
fn forwarded_evidence_and_sfu_config() {
    let metrics = r#"{"natTraversalPath":"ordinary","width":null,"height":null,"framesPerSecond":null,"bitrateKbps":null,"packetsReceivedDelta":null,"packetsLostDelta":null,"rttMs":null,"jitterMs":null,"framesDecodedDelta":null,"framesDroppedDelta":null,"decodeMsPerFrame":null,"freezeCountDelta":null,"freezeDurationMsDelta":null,"pauseCountDelta":null,"pauseDurationMsDelta":null,"codec":null,"codecProfile":null,"codecParameters":null,"audioBitrateKbps":null,"audioPacketLossPercent":null,"audioJitterMs":null,"audioVideoPlayoutDeltaMs":null,"videoJitterBufferDelayMs":null,"audioJitterBufferDelayMs":null,"audioConcealedSamplesPercent":null,"audioConcealmentEventsDelta":null,"audioCodec":null}"#;
    let forwarded = |extra: &str, upstream: &str| {
        format!(r#"{{"type":"viewer-quality-evidence","viewerPeerId":"viewer_12345678","upstream":{upstream},"guard":{{"connectionId":"connection_12345678","routeRevision":0,"presentationEpoch":0}},"sequence":0,"windowMs":2000,"metrics":{metrics}{extra}}}"#)
            .into_bytes()
    };
    assert_server(
        "peer upstream",
        &forwarded("", r#"{"kind":"peer","peerId":"host_12345678"}"#),
        true,
    );
    assert_server("sfu upstream", &forwarded("", r#"{"kind":"sfu"}"#), true);
    assert_server("no upstream", &forwarded("", r#"{"kind":"none"}"#), false);
    assert_server(
        "leaked room id",
        &forwarded(r#","roomId":"1234""#, r#"{"kind":"sfu"}"#),
        false,
    );
    assert_server(
        "missing upstream",
        format!(r#"{{"type":"viewer-quality-evidence","viewerPeerId":"viewer_12345678","guard":{{"connectionId":"connection_12345678","routeRevision":0,"presentationEpoch":0}},"sequence":0,"windowMs":2000,"metrics":{metrics}}}"#).as_bytes(),
        false,
    );
    assert_server(
        "sfu config",
        br#"{"type":"sfu-config","revision":9,"publicationGeneration":"publication_12345678","connectionId":"connection_12345678"}"#,
        true,
    );
    assert_server(
        "sfu config missing connection fence",
        br#"{"type":"sfu-config","revision":9,"publicationGeneration":"publication_12345678"}"#,
        false,
    );
}

/// zod's `z.number().int()` accepts an integral JSON number in any spelling.
#[test]
fn int_accepts_integral_number_forms() {
    for (raw, want) in [
        ("7", true),
        ("7.0", true),
        ("7e0", true),
        ("1e3", true),
        ("7.5", false),
        (r#""7""#, false),
        ("true", false),
    ] {
        assert_client(
            raw,
            format!(r#"{{"type":"route-media-unavailable","revision":{raw}}}"#).as_bytes(),
            want,
        );
    }
}

/// `JSON.parse` keeps the last occurrence of a duplicate key.
#[test]
fn duplicate_keys_keep_the_last_occurrence() {
    let message = decode_client_message(
        br#"{"type":"relay-capacity","downstreamEdges":9,"downstreamEdges":2}"#,
    )
    .expect("the last occurrence must win");
    assert_eq!(
        message,
        ClientMessage::RelayCapacity {
            downstream_edges: 2
        }
    );
    assert!(
        decode_client_message(
            br#"{"type":"relay-capacity","downstreamEdges":2,"downstreamEdges":9}"#
        )
        .is_err(),
        "the last occurrence must be the one validated"
    );
    // A duplicated object is replaced, not merged.
    let raw = format!(
        r#"{{"type":"set-quality-settings","qualitySettings":{{"resolution":{{"width":854,"height":480}}}},"qualitySettings":{QUALITY_SETTINGS}}}"#
    );
    assert!(decode_client_message(raw.as_bytes()).is_ok());
}

/// `JSON.stringify` does not escape "<", ">" or "&"; display names may hold them.
#[test]
fn encode_server_message_does_not_escape_html() {
    let encoded = encode_server_message(&ServerMessage::ViewerPresence {
        viewers: vec![ParticipantPresenceEntry::host(
            "host_12345678",
            name("a<b>&c"),
        )],
    });
    let text = String::from_utf8(encoded).unwrap();
    assert!(text.contains(r#""displayName":"a<b>&c""#), "{text}");
    assert!(!text.ends_with('\n'));
}

/// Empty collections encode as `[]` the way the TypeScript literals do.
#[test]
fn encode_server_message_emits_empty_arrays() {
    let encoded = encode_server_message(&ServerMessage::ViewerPresence { viewers: vec![] });
    assert_eq!(encoded, br#"{"type":"viewer-presence","viewers":[]}"#);
    let encoded = encode_server_message(&ServerMessage::RouteDiagnosticSnapshot(
        RouteDiagnosticSnapshot::default(),
    ));
    assert_eq!(
        encoded,
        br#"{"type":"route-diagnostic-snapshot","snapshot":{"children":[],"operation":null}}"#
    );
}

/// The authenticated key order follows the object literal in `signaling.ts`.
#[test]
fn authenticated_key_order() {
    let message = ServerMessage::Authenticated(Box::new(AuthenticatedMessage {
        peer_id: "viewer_12345678".into(),
        max_viewers: 8,
        endpoint_media_copy_capacity: 2,
        host_online: true,
        host_paused: Some(true),
        connection_id: None,
        ice_config: IceConfig::default(),
        route_policy: DEFAULT_ROUTE_POLICY,
        code_entry_policy: CodeEntryPolicy::Open,
        viewer_authorization_generation: "viewer_generation_12345678".into(),
        share_generation: None,
        route_revision: 0,
        route_assignment: ParticipantRouteAssignment::default(),
        quality_settings: DEFAULT_QUALITY_SETTINGS,
        role: Role::Viewer,
        viewer_password_enabled: None,
    }));
    let encoded: serde_json::Value =
        serde_json::from_slice(&encode_server_message(&message)).unwrap();
    let order: Vec<&str> = encoded
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        order,
        [
            "type",
            "protocol",
            "peerId",
            "maxViewers",
            "endpointMediaCopyCapacity",
            "hostOnline",
            "hostPaused",
            "connectionId",
            "iceConfig",
            "routePolicy",
            "codeEntryPolicy",
            "viewerAuthorizationGeneration",
            "mediaMode",
            "shareGeneration",
            "routeRevision",
            "routeAssignment",
            "qualitySettings",
            "role",
        ]
    );
}

/// Optional keys reject an explicit null; nullable keys reject absence.
#[test]
fn optional_and_nullable_are_exact() {
    for (name, raw, want) in [
        (
            "null optional shareGeneration",
            r#"{"type":"stop-sharing","shareGeneration":null}"#,
            false,
        ),
        (
            "absent optional shareGeneration",
            r#"{"type":"stop-sharing"}"#,
            true,
        ),
        (
            "null optional targetPeerId",
            r#"{"type":"restart-request","targetPeerId":null,"connectionId":"connection_12345678","rebuild":true}"#,
            false,
        ),
        (
            "null optional qualityApproved",
            r#"{"type":"route-ready","revision":1,"phase":"prepare","qualityApproved":null}"#,
            false,
        ),
        (
            "null nullable connectionId",
            r#"{"type":"route-failed","revision":1,"phase":"active","connectionId":null}"#,
            true,
        ),
        (
            "absent nullable connectionId",
            r#"{"type":"route-failed","revision":1,"phase":"active"}"#,
            false,
        ),
    ] {
        assert_client(name, raw.as_bytes(), want);
    }
}

fn host_token() -> String {
    "a".repeat(43)
}

/// A host authenticate without `routePolicy` takes `DEFAULT_ROUTE_POLICY`.
#[test]
fn authenticate_applies_the_route_policy_default() {
    let raw = format!(
        r#"{{"type":"authenticate","protocol":"piik-v24","roomId":"1234","role":"host","token":"{}","clientId":"client_12345678"}}"#,
        host_token()
    );
    match decode_client_message(raw.as_bytes()).expect("decode") {
        ClientMessage::AuthenticateHost(host) => {
            assert_eq!(host.route_policy, DEFAULT_ROUTE_POLICY)
        }
        other => panic!("decoded {other:?}, want AuthenticateHost"),
    }
}

#[test]
fn authenticate_rejects_removed_opt_in_fields() {
    for key in [
        r#""roomSession":true"#,
        r#""roomOnly":true"#,
        r#""viewerPresence":true"#,
    ] {
        let raw = format!(
            r#"{{"type":"authenticate","protocol":"piik-v24","roomId":"1234","role":"host","token":"{}","clientId":"client_12345678",{key}}}"#,
            host_token()
        );
        assert!(
            decode_client_message(raw.as_bytes()).is_err(),
            "accepted a removed authenticate field"
        );
    }
}

#[test]
fn start_sharing_requires_an_opaque_generation_and_route_policy() {
    let message = decode_client_message(br#"{"type":"start-sharing","shareGeneration":"share_12345678","routePolicy":{"peerOnly":true,"topologyOptimization":false,"natPrediction":false}}"#)
        .expect("decode");
    assert!(
        matches!(message, ClientMessage::StartSharing(_)),
        "{message:?}"
    );
    for raw in [
        r#"{"type":"start-sharing","shareGeneration":"short","routePolicy":{"peerOnly":true,"topologyOptimization":false,"natPrediction":false}}"#,
        r#"{"type":"start-sharing","shareGeneration":"share_12345678"}"#,
    ] {
        assert_client(raw, raw.as_bytes(), false);
    }
}

/// The sealed unions accept nothing outside their own members.
#[test]
fn decode_rejects_unknown_envelopes() {
    for raw in [
        "{}",
        "[]",
        "null",
        r#""authenticate""#,
        r#"{"type":42}"#,
        r#"{"type":"unknown"}"#,
        r#"{"kind":"signal"}"#,
    ] {
        assert_client(raw, raw.as_bytes(), false);
        assert_server(raw, raw.as_bytes(), false);
    }
}
