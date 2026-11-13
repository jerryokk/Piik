//! Peer-assisted signal relay and restart requests.

use piik_protocol::{
    ErrorCode, PunchSignal, SdpType, ServerMessage, SignalPayload, candidate_signal_origin_of,
};

use crate::router::{PeerSignalInput, SignalAuthorization};
use crate::server::Core;
use crate::session::AuthState;

impl Core {
    pub(super) fn route_peer_assisted_signal(
        &mut self,
        key: u64,
        source: &AuthState,
        target_peer_id: String,
        payload: SignalPayload,
    ) {
        if target_peer_id.is_empty() {
            self.send_error(
                key,
                ErrorCode::Forbidden,
                "Peer-assisted signals must target an assigned peer",
            );
            return;
        }
        let parent_to_child =
            self.is_active_peer_parent_of(&source.room_id, &source.peer_id, &target_peer_id);
        let child_to_parent =
            self.is_active_peer_parent_of(&source.room_id, &target_peer_id, &source.peer_id);
        let Some(target) = self.connected_peer(&source.room_id, &target_peer_id) else {
            if !parent_to_child && !child_to_parent {
                self.send_error(key, ErrorCode::PeerNotFound, "Target peer is not connected");
            }
            return;
        };
        let (signal_kind, description_type, punch_phase, punched) = match &payload {
            SignalPayload::Punch { punch, .. } => (
                "punch",
                "",
                match punch {
                    PunchSignal::Prepare { .. } => "prepare",
                    PunchSignal::Ready { .. } => "ready",
                    PunchSignal::Start { .. } => "start",
                    PunchSignal::Complete { .. } => "complete",
                },
                false,
            ),
            SignalPayload::Description {
                description,
                punched,
                ..
            } => (
                "description",
                match description.kind {
                    SdpType::Offer => "offer",
                    SdpType::Answer => "answer",
                },
                "",
                *punched,
            ),
            SignalPayload::Candidate { .. } => ("candidate", "", "", false),
        };
        let input = PeerSignalInput {
            room_id: source.room_id.clone(),
            source_peer_id: source.peer_id.clone(),
            source_session_id: self.session_id_of(key),
            target_peer_id: target.peer_id.clone(),
            target_session_id: target.session_id.clone(),
            connection_id: payload.connection_id().to_owned(),
            signal_kind,
            description_type,
            punch_phase,
            punched,
        };
        let authorization = self.peer_signal_authorization(&input);
        let candidate_origin = match &payload {
            SignalPayload::Candidate { candidate, .. } => Some(candidate_signal_origin_of(
                candidate
                    .as_ref()
                    .map_or("", |candidate| candidate.candidate.as_str()),
            )),
            _ => None,
        };
        self.debug_peer_signal(&input, candidate_origin, authorization);
        let assigned = match description_type {
            "offer" => parent_to_child,
            "" => parent_to_child || child_to_parent,
            _ => child_to_parent,
        };
        let authorized = match authorization {
            SignalAuthorization::Unassigned => assigned,
            SignalAuthorization::Denied => false,
            SignalAuthorization::Allowed | SignalAuthorization::Probe => true,
        };
        if !authorized {
            return;
        }
        // An authorized non-probe offer matches the committed connection or
        // was adopted into it by the authorization.
        self.send_to_session(
            &target.session_id,
            &ServerMessage::Signal {
                from_peer_id: source.peer_id.clone(),
                payload,
            },
        );
    }

    pub(super) fn route_peer_assisted_restart(
        &mut self,
        key: u64,
        source: &AuthState,
        target_peer_id: String,
        connection_id: String,
        rebuild: bool,
    ) {
        let Some(parent) = self.connected_peer(&source.room_id, &target_peer_id) else {
            self.send_error(
                key,
                ErrorCode::PeerNotFound,
                "Media parent is not connected",
            );
            return;
        };
        let input = PeerSignalInput {
            room_id: source.room_id.clone(),
            source_peer_id: source.peer_id.clone(),
            source_session_id: self.session_id_of(key),
            target_peer_id,
            target_session_id: parent.session_id.clone(),
            connection_id: connection_id.clone(),
            ..PeerSignalInput::default()
        };
        if !self.peer_restart_authorized(&input) {
            self.send_error(
                key,
                ErrorCode::Forbidden,
                "Restart target is not the assigned media parent",
            );
            return;
        }
        self.send_to_session(
            &parent.session_id,
            &ServerMessage::RestartRequest {
                from_peer_id: source.peer_id.clone(),
                connection_id,
                rebuild,
            },
        );
    }
}
