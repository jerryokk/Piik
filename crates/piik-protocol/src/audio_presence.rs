//! Host audio input availability.

#[cfg(test)]
mod tests {
    use crate::client::decode_client_message;
    use crate::server::{decode_server_message, encode_server_message};

    #[test]
    fn audio_presence_wire() {
        for raw in [
            r#"{"type":"audio-presence","available":null}"#,
            r#"{"type":"audio-presence","available":false}"#,
            r#"{"type":"audio-presence","available":true}"#,
        ] {
            let message = decode_server_message(raw.as_bytes()).expect(raw);
            assert_eq!(
                String::from_utf8(encode_server_message(&message)).unwrap(),
                raw
            );
        }
        for raw in [
            r#"{"type":"audio-presence"}"#,
            r#"{"type":"audio-presence","available":"true"}"#,
        ] {
            assert!(
                decode_server_message(raw.as_bytes()).is_err(),
                "accepted {raw}"
            );
        }
        for raw in [
            r#"{"type":"set-audio-presence","shareGeneration":"audio_share_generation","available":null}"#,
            r#"{"type":"set-audio-presence","shareGeneration":"bad","available":true}"#,
        ] {
            assert!(
                decode_client_message(raw.as_bytes()).is_err(),
                "accepted {raw}"
            );
        }
    }
}
