import { vi } from "vitest";
import { type IceConfig, type SignalPayload } from "../../src/shared/protocol.ts";
import { type PeerSnapshot } from "../../src/client/types.ts";
import { ViewerPeer } from "../../src/client/webrtc/viewer-peer.ts";


export interface Deferred<T> {
  promise: Promise<T>;
  resolve: (value: T) => void;
}

export function createDeferred<T>(): Deferred<T> {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((resolvePromise) => {
    resolve = resolvePromise;
  });
  return { promise, resolve };
}

export const ANSWER_SDP = [
  "v=0",
  "o=- 1 1 IN IP4 127.0.0.1",
  "s=-",
  "t=0 0",
  "m=audio 9 UDP/TLS/RTP/SAVPF 111",
  "c=IN IP4 0.0.0.0",
  "a=rtpmap:111 opus/48000/2",
  "a=fmtp:111 minptime=10;useinbandfec=1",
  "a=recvonly",
  "",
].join("\r\n");

export interface ConnectionPlan {
  constructorError?: Error;
  answerError?: Error;
  answerSdp?: string;
  candidateGates?: Promise<void>[];
  localDescriptionGate?: Promise<void>;
  statsGate?: Promise<RTCStatsReport>;
}

export class FakeMediaStream {
  private readonly tracks: MediaStreamTrack[] = [];

  getTrackById(id: string): MediaStreamTrack | null {
    return this.tracks.find((track) => track.id === id) ?? null;
  }

  addTrack(track: MediaStreamTrack): void {
    this.tracks.push(track);
  }

  getVideoTracks(): MediaStreamTrack[] {
    return this.tracks.filter((track) => track.kind === "video");
  }

  getAudioTracks(): MediaStreamTrack[] {
    return this.tracks.filter((track) => track.kind === "audio");
  }
}

export class FakePeerConnection extends EventTarget {
  static readonly instances: FakePeerConnection[] = [];
  static readonly plans: ConnectionPlan[] = [];

  readonly configurations: RTCConfiguration[] = [];
  connectionState: RTCPeerConnectionState = "new";
  iceConnectionState: RTCIceConnectionState = "new";
  iceGatheringState: RTCIceGatheringState = "new";
  remoteDescription: RTCSessionDescription | null = null;
  localDescription: RTCSessionDescription | null = null;
  statsGate: Promise<RTCStatsReport> | null = null;

  private readonly candidateGates: Promise<void>[];
  private readonly localDescriptionGate: Promise<void> | null;
  private readonly answerError: Error | null;
  private readonly answerSdp: string;

  readonly addIceCandidate = vi.fn(async (_candidate: RTCIceCandidateInit | null) => {
    const gate = this.candidateGates.shift();
    if (gate) {
      await gate;
    }
  });
  readonly getStats = vi.fn(async () => {
    if (this.statsGate) {
      return this.statsGate;
    }
    return new Map() as unknown as RTCStatsReport;
  });
  readonly setLocalDescription = vi.fn(
    async (description: RTCSessionDescriptionInit) => {
      if (this.localDescriptionGate) {
        await this.localDescriptionGate;
      }
      this.localDescription = description as RTCSessionDescription;
    },
  );

  constructor(configuration?: RTCConfiguration) {
    super();
    if (configuration) {
      this.configurations.push(configuration);
    }
    const plan = FakePeerConnection.plans.shift() ?? {};
    if (plan.constructorError) throw plan.constructorError;
    this.candidateGates = [...(plan.candidateGates ?? [])];
    this.localDescriptionGate = plan.localDescriptionGate ?? null;
    this.answerError = plan.answerError ?? null;
    this.answerSdp = plan.answerSdp ?? ANSWER_SDP;
    this.statsGate = plan.statsGate ?? null;
    FakePeerConnection.instances.push(this);
  }

  readonly setRemoteDescription = vi.fn(
    async (description: RTCSessionDescriptionInit): Promise<void> => {
      this.remoteDescription = description as RTCSessionDescription;
    },
  );

  async createAnswer(): Promise<RTCSessionDescriptionInit> {
    if (this.answerError) {
      throw this.answerError;
    }
    return {
      type: "answer",
      sdp: this.answerSdp,
    };
  }

  readonly setConfiguration = vi.fn((configuration: RTCConfiguration) => {
    this.configurations.push(configuration);
  });

  close(): void {
    this.connectionState = "closed";
  }

  emitTrack(track: MediaStreamTrack, streams: MediaStream[] = []): void {
    this.dispatchEvent(
      Object.assign(new Event("track"), { track, streams }),
    );
  }
}

export const intervalCallbacks = new Map<number, () => void>();

let nextIntervalId = 1;

export const timeoutCallbacks = new Map<number, () => void>();

export const timeoutDelays = new Map<number, number>();

let nextTimeoutId = 1;

export function offer(connectionId: string): SignalPayload {
  return {
    kind: "description",
    connectionId,
    description: { type: "offer", sdp: `offer-${connectionId}` },
  };
}

export function candidate(connectionId: string, value: string): SignalPayload {
  return {
    kind: "candidate",
    connectionId,
    candidate: {
      candidate: value,
      sdpMid: "0",
      sdpMLineIndex: 0,
    },
  };
}

export function decodedReport(framesDecoded: number): RTCStatsReport {
  return new Map([
    [
      "video-in",
      {
        id: "video-in",
        type: "inbound-rtp",
        timestamp: 1_000,
        kind: "video",
        framesDecoded,
      },
    ],
  ]) as unknown as RTCStatsReport;
}

export function createPeer(
  signals: SignalPayload[],
  snapshots: PeerSnapshot[],
  signalPeers: string[] = [],
  iceConfig: IceConfig = { iceServers: [] },
  natPrediction = false,
): ViewerPeer {
  return new ViewerPeer(
    iceConfig,
    {
      sendSignal: (peerId, payload) => {
        signalPeers.push(peerId);
        signals.push(payload);
        return true;
      },
      sendRestartRequest: () => true,
      onStream: () => undefined,
      onUpdate: (snapshot) => snapshots.push(snapshot),
    },
    { natPredictionEnabled: natPrediction },
  );
}

export async function flushAsyncWork(): Promise<void> {
  await new Promise<void>((resolve) => setImmediate(resolve));
}

export const installFakeBrowser = () => {
  FakePeerConnection.instances.length = 0;
  FakePeerConnection.plans.length = 0;
  intervalCallbacks.clear();
  nextIntervalId = 1;
  timeoutCallbacks.clear();
  timeoutDelays.clear();
  nextTimeoutId = 1;
  vi.stubGlobal("MediaStream", FakeMediaStream);
  vi.stubGlobal("RTCPeerConnection", FakePeerConnection);
  vi.stubGlobal("window", {
    setInterval: (callback: () => void) => {
      const id = nextIntervalId++;
      intervalCallbacks.set(id, callback);
      return id;
    },
    clearInterval: (id: number) => intervalCallbacks.delete(id),
    setTimeout: (callback: () => void, delay: number) => {
      const id = nextTimeoutId++;
      timeoutCallbacks.set(id, callback);
      timeoutDelays.set(id, delay);
      return id;
    },
    clearTimeout: (id: number) => {
      timeoutCallbacks.delete(id);
      timeoutDelays.delete(id);
    },
  });
};

export const restoreBrowserGlobals = () => {
  vi.unstubAllGlobals();
};
