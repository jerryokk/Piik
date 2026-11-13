// Native Viewer edge contracts shared by the router and its Native backend.

import type { NativeClient } from "../client";
import type { NativeVideoCodec } from "../wire";

export interface NativeViewerSource {
  client: NativeClient;
  sessionId: string;
  connectionId: string;
  generation: number;
  codec: NativeVideoCodec;
}

export type PeerIceConfig = Pick<RTCConfiguration, "iceServers"> & {
  natPredictionStunUrls?: readonly string[];
};
