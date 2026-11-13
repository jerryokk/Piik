use std::fmt;

/// `RTCPeerConnectionState` with the wire strings Pion and browsers use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PcState {
    New,
    Connecting,
    Connected,
    Disconnected,
    Failed,
    Closed,
}

impl PcState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Connecting => "connecting",
            Self::Connected => "connected",
            Self::Disconnected => "disconnected",
            Self::Failed => "failed",
            Self::Closed => "closed",
        }
    }
}

impl fmt::Display for PcState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl serde::Serialize for PcState {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::PcState;

    #[test]
    fn wire_strings_match_pion() {
        let all = [
            PcState::New,
            PcState::Connecting,
            PcState::Connected,
            PcState::Disconnected,
            PcState::Failed,
            PcState::Closed,
        ];
        let wire: Vec<String> = all
            .iter()
            .map(|state| serde_json::to_string(state).unwrap())
            .collect();
        assert_eq!(
            wire,
            [
                "\"new\"",
                "\"connecting\"",
                "\"connected\"",
                "\"disconnected\"",
                "\"failed\"",
                "\"closed\""
            ]
        );
    }
}
