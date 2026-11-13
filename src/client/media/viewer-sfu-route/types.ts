import type {
  ClientMessage,
  ParticipantRouteAssignment,
  PreparedRouteCandidate,
  SfuSignalMessage,
} from "../../../shared/protocol";
import type { ConnectionMetrics } from "../../types";
import type { SfuConnectionConfig } from "../../sfu/publisher";
import type {
  CandidateQualityProbeResult,
  SfuQualityProbe,
} from "../candidate-quality-probe";
import type { RouteOperationToken } from "../route-transition";

// Shared subscriber shape for the Viewer SFU route.

export interface ViewerSubscriberTransport {
  getRemoteInput?(): RTCDataChannel | null;
  connect(config: SfuConnectionConfig): Promise<boolean>;
  updateConfig(config: SfuConnectionConfig): void;
  isConnected(): boolean;
  acceptSignal(message: SfuSignalMessage): Promise<void>;
  reconnect(): boolean;
  resync(): void;
  activate(): boolean;
  deactivate(): boolean;
  armDecodedFrameProof(requireProgress?: boolean): void;
  stopDecodedFrameProof(): void;
  disconnect(): Promise<void>;
}

export interface ViewerSubscriberSlot {
  connectionId: string;
  revision: number;
  publicationGeneration: string;
  subscriber: ViewerSubscriberTransport;
  connected: boolean;
  activated: boolean;
  stream: MediaStream | null;
  decodedFrame: boolean;
  readySent: boolean;
  qualityProbe: SfuQualityProbe | null;
  qualityResult: CandidateQualityProbeResult;
  failed: boolean;
  activationToken: RouteOperationToken | null;
}

export interface ViewerSfuRouteEvents {
  onInputChanged?: () => void;
  activatePeer: (
    assignment: ParticipantRouteAssignment,
    revision?: number,
  ) => boolean | "waiting" | void | Promise<boolean | "waiting" | void>;
  preparePeer?: (
    assignment: ParticipantRouteAssignment | null,
    revision?: number,
    candidate?: PreparedRouteCandidate,
  ) => void;
  prepareChild?: (
    candidate: PreparedRouteCandidate | null,
    childPeerIds?: readonly string[],
    revision?: number,
  ) => void;
  activateChildren?: (
    childPeerIds: readonly string[],
    revision: number,
  ) => void;
  resetMedia?: () => void;
  reconcileSfuChildren?: (childPeerIds: string[]) => void;
  onSfuStream: (
    stream: MediaStream,
    assignment: ParticipantRouteAssignment,
    initialVideoStream: boolean,
    revision: number,
  ) => void;
  onSfuVideoAvailability?: (available: boolean, revision: number) => void;
  onSfuUpdate?: (metrics: ConnectionMetrics | null, revision: number) => void;
  currentPeerMetrics?: () => ConnectionMetrics | null;
  qualityProbeEligible?: () => boolean;
  onSfuState?: (
    state: "connected" | "reconnecting",
    revision: number,
  ) => void;
  onSfuFailure?: (failure: "VIDEO_REJECTED" | null, revision: number) => void;
  send: (message: ClientMessage) => boolean;
  createSubscriber?: (
    events: {
      onStream: (stream: MediaStream | null) => void;
      onVideoAvailability: (available: boolean) => void;
      onStats: (metrics: ConnectionMetrics) => void;
      onFirstDecodedFrame: () => boolean;
      onState: (state: "connected" | "reconnecting") => void;
      onDisconnected: (failure?: "VIDEO_REJECTED") => void;
    },
  ) => ViewerSubscriberTransport;
}
