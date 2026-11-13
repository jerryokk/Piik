use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use piik_input_route::{MAX_PACKET, Packet};

use crate::cipher::{FROM_VIEWER, InputCipher};
use crate::event::InputEvent;
use crate::text::{MAX_INPUT_BYTES, MAX_TEXT_BYTES};

#[test]
fn routed_input_rejects_forgery_reflection_and_replay() {
    let mut host = InputCipher::new(&"ab".repeat(32)).unwrap();
    let epoch = "cd".repeat(16);
    // Escaped quotes/backslashes at the text limit must survive the same relay
    // envelope as keys, with no alternate channel or widened queue budget.
    let event = InputEvent {
        grant: "a".repeat(64),
        ..InputEvent::text(&"\\\"".repeat(MAX_TEXT_BYTES / 2))
    };
    let plain = serde_json::to_vec(&event).unwrap();
    let packet = host.seal_with(FROM_VIEWER, 1, &plain, &epoch);
    let envelope = serde_json::to_vec(&Packet {
        id: "b".repeat(32),
        epoch: epoch.clone(),
        data: packet.clone(),
        ..Packet::default()
    })
    .unwrap();
    assert!(
        plain.len() <= MAX_INPUT_BYTES && envelope.len() <= MAX_PACKET,
        "valid text does not fit the input transport"
    );
    let mut forged = STANDARD.decode(&packet).unwrap();
    *forged.last_mut().unwrap() ^= 1;
    assert!(
        host.open(&STANDARD.encode(forged), &epoch).is_none(),
        "relay modification accepted"
    );
    let reflected = host.seal(&plain, &epoch);
    assert!(
        host.open(&reflected, &epoch).is_none(),
        "Host reply reflected as input"
    );
    assert!(
        host.open(&packet, &"ef".repeat(16)).is_none(),
        "relay replaced the input channel epoch"
    );
    assert_eq!(
        host.open(&packet, &epoch).as_deref(),
        Some(&plain[..]),
        "valid endpoint input rejected"
    );
    assert!(
        host.open(&packet, &epoch).is_none(),
        "delayed input replay accepted"
    );
}

#[test]
fn invalid_keys_are_rejected() {
    assert!(InputCipher::new("ab").is_none());
    assert!(InputCipher::new(&"zz".repeat(32)).is_none());
}
