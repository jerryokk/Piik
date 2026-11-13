// Host direct-edge contracts: the peer lifecycle, its events and ICE config.

import type { IceConfig, SignalPayload } from "../../../shared/protocol";
import type { QualityProfile } from "../../media/quality";
import type { PeerSnapshot } from "../../types";

export type PeerIceConfig = Pick<RTCConfiguration, "iceServers"> & {
  natPredictionStunUrls?: readonly string[];
};

export interface HostPeerEvents {
  sendSignal: (peerId: string, payload: SignalPayload) => boolean;
  onUpdate: (snapshot: PeerSnapshot) => void;
  onInputChannel?: (channel: RTCDataChannel, connection: RTCPeerConnection) => void;
}

/** Common lifecycle contract for Browser and native Host direct edges. */
export interface HostMediaPeer {
  readonly peerId: string;
  readonly connectionId: string;
  start(): Promise<boolean>;
  acceptSignal(payload: SignalPayload): Promise<void>;
  restartIce(): Promise<boolean>;
  isConnected(): boolean;
  getSnapshot(): PeerSnapshot;
  updateIceConfig(iceConfig: IceConfig): void;
  updateProfile(profile: QualityProfile): Promise<boolean>;
  updateCaptureProfile(profile: QualityProfile): Promise<boolean>;
  setPaused(paused: boolean): void;
  replaceStream(stream: MediaStream): Promise<boolean>;
  dispose(): void;
}
