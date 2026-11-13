//! Types the Viewer page and its session hook share: the server quality
//! evidence message, the SFU upstream state, an in-flight peer route and the
//! accepted route assignment.

import type {
  ParticipantRouteAssignment,
  PreparedRouteCandidate,
  ServerMessage,
} from "../../../shared/protocol";
import type {
  CandidateQualityProbeResult,
  P2pQualityProbe,
} from "../../media/candidate-quality-probe";
import type { ConnectionMetrics, PeerSnapshot } from "../../types";
import type { ViewerMediaPeer } from "../../webrtc/viewer-peer";

export type ViewerQualityEvidence = Extract<
  ServerMessage,
  { type: "viewer-quality-evidence" }
>;

export interface SfuUpstreamState {
  connectionState: "connected" | "reconnecting";
  metrics: ConnectionMetrics | null;
}

export interface PendingPeerRoute {
  revision: number;
  parentPeerId: string;
  peer: ViewerMediaPeer | null;
  decodedFrame: boolean;
  connectedSent: boolean;
  readySent: boolean;
  candidateConnectionId: string;
  stream: MediaStream | null;
  snapshot: PeerSnapshot | null;
  qualityProbe: P2pQualityProbe | null;
  qualityResult: CandidateQualityProbeResult;
}

export interface ViewerAssignedRoute {
  revision: number;
  phase: "prepare" | "active";
  upstream: ParticipantRouteAssignment["upstream"];
  connectionAttempt?: PreparedRouteCandidate["connectionAttempt"];
}
