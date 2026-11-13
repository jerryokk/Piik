import { expect, vi } from "vitest";
import { QUALITY_PROFILES, type QualityProfile } from "../../src/client/media/quality.ts";
import { type PeerSnapshot } from "../../src/client/types.ts";
import { HostPeer } from "../../src/client/webrtc/host-peer";
import { VP8_ONLY_VIDEO_CODEC, type BrowserVideoCodecPreference } from "../../src/client/webrtc/video-codec.ts";
import { setCopy } from "../../src/client/ui/copy.ts";
import * as encodingOutput from "../../src/client/media/browser-encoding-output";
import { type BrowserEncodingPool } from "../../src/client/media/browser-encoding-pool";
import { type IceConfig, type ParticipantRouteAssignment } from "../../src/shared/protocol.ts";

const codecProbe = vi.hoisted(() => ({
  probe: vi.fn(async (): Promise<"h264" | "vp8"> => "vp8"),
}));

vi.mock("../../src/client/webrtc/video-codec-preflight.ts", () => ({
  preferredVideoCodecForTrack: codecProbe.probe,
}));

export const codecPreflight = codecProbe;

export const statsCallbacks: Array<() => void> = [];

export const timeouts: Array<ReturnType<typeof setTimeout>> = [];

export class FakeSender {
  failNextReplace = false;
  deferReplaceCall: number | null = null;
  failNextSetParameters = false;
  deferNextSetParameters = false;
  readonly appliedMaxBitrates: Array<number | undefined> = [];
  private parameters = { encodings: [{}] } as RTCRtpSendParameters;
  private replaceCallCount = 0;
  private releaseReplaceTrack: (() => void) | null = null;
  private releaseSetParameters: (() => void) | null = null;
  readonly setParameters = vi.fn(
    async (parameters: RTCRtpSendParameters) => {
      if (this.failNextSetParameters) {
        this.failNextSetParameters = false;
        throw new Error("setParameters failed");
      }
      if (this.deferNextSetParameters) {
        this.deferNextSetParameters = false;
        await new Promise<void>((resolve) => {
          this.releaseSetParameters = resolve;
        });
      }
      this.appliedMaxBitrates.push(parameters.encodings[0]?.maxBitrate);
      this.parameters = parameters;
    },
  );
  readonly replaceTrack = vi.fn(async (track: MediaStreamTrack | null) => {
    this.replaceCallCount += 1;
    if (this.failNextReplace) {
      this.failNextReplace = false;
      throw new Error("replaceTrack failed");
    }
    if (this.deferReplaceCall === this.replaceCallCount) {
      this.deferReplaceCall = null;
      await new Promise<void>((resolve) => {
        this.releaseReplaceTrack = resolve;
      });
    }
    this.track = track;
  });

  constructor(public track: MediaStreamTrack | null) {}

  getParameters(): RTCRtpSendParameters {
    return this.parameters;
  }

  releaseDeferredSetParameters(): void {
    this.releaseSetParameters?.();
    this.releaseSetParameters = null;
  }

  releaseDeferredReplaceTrack(): void {
    this.releaseReplaceTrack?.();
    this.releaseReplaceTrack = null;
  }
}

export class FakePeerConnection {
  static latest: FakePeerConnection | null = null;
  static instances: FakePeerConnection[] = [];
  static activeCount = 0;
  static peakActiveCount = 0;
  static offersFailing = 0;
  static constructorsFailing = 0;
  static omitCodecPreferenceSetter = false;
  static codecPreferenceCallsFailing = 0;

  readonly configurations: RTCConfiguration[] = [];
  readonly senders: FakeSender[] = [];
  readonly transceivers: RTCRtpTransceiver[] = [];
  readonly transceiverInputs: Array<{
    trackOrKind: MediaStreamTrack | string;
    init?: RTCRtpTransceiverInit;
  }> = [];
  readonly codecPreferenceCalls: RTCRtpCodec[][] = [];
  createOfferCallCount = 0;
  remoteDescriptionCallCount = 0;
  deferRemoteDescriptionCall: number | null = null;
  private releaseRemoteDescription: (() => void) | null = null;
  connectionState: RTCPeerConnectionState = "new";
  iceConnectionState: RTCIceConnectionState = "new";
  iceGatheringState: RTCIceGatheringState = "new";
  signalingState: RTCSignalingState = "stable";
  localDescription: RTCSessionDescription | null = null;
  remoteDescription: RTCSessionDescription | null = null;
  readonly addedIceCandidates: Array<RTCIceCandidateInit | null> = [];
  readonly statsReports: Array<RTCStatsReport | Promise<RTCStatsReport>> = [];
  private readonly eventListeners = new Map<
    string,
    Array<(event: Event) => void>
  >();

  constructor(configuration?: RTCConfiguration) {
    if (FakePeerConnection.constructorsFailing > 0) {
      FakePeerConnection.constructorsFailing--;
      throw new DOMException("Allocation failed", "UnknownError");
    }
    FakePeerConnection.latest = this;
    FakePeerConnection.instances.push(this);
    FakePeerConnection.activeCount += 1;
    FakePeerConnection.peakActiveCount = Math.max(
      FakePeerConnection.peakActiveCount,
      FakePeerConnection.activeCount,
    );
    if (configuration) {
      this.configurations.push(configuration);
    }
  }

  readonly setConfiguration = vi.fn((configuration: RTCConfiguration) => {
    this.configurations.push(configuration);
  });

  readonly restartIce = vi.fn();

  addTransceiver(
    trackOrKind: MediaStreamTrack | string,
    init?: RTCRtpTransceiverInit,
  ): RTCRtpTransceiver {
    const sender = new FakeSender(
      typeof trackOrKind === "string" ? null : trackOrKind,
    );
    this.senders.push(sender);
    this.transceiverInputs.push({ trackOrKind, init });
    const transceiver = {
      sender,
      direction: init?.direction ?? "sendrecv",
      ...(FakePeerConnection.omitCodecPreferenceSetter
        ? {}
        : {
            setCodecPreferences: (codecs: RTCRtpCodec[]) => {
              if (FakePeerConnection.codecPreferenceCallsFailing > 0) {
                FakePeerConnection.codecPreferenceCallsFailing -= 1;
                throw new Error("setCodecPreferences failed");
              }
              this.codecPreferenceCalls.push([...codecs]);
            },
          }),
    } as unknown as RTCRtpTransceiver;
    this.transceivers.push(transceiver);
    return transceiver;
  }

  addEventListener(type: string, listener: (event: Event) => void): void {
    const listeners = this.eventListeners.get(type) ?? [];
    listeners.push(listener);
    this.eventListeners.set(type, listeners);
  }

  dispatchEvent(event: Event): boolean {
    for (const listener of this.eventListeners.get(event.type) ?? []) {
      listener(event);
    }
    return true;
  }

  async createOffer(): Promise<RTCSessionDescriptionInit> {
    this.createOfferCallCount += 1;
    if (FakePeerConnection.offersFailing > 0) {
      FakePeerConnection.offersFailing -= 1;
      throw new Error("createOffer failed");
    }
    return { type: "offer", sdp: "test-offer" };
  }

  async setLocalDescription(
    description: RTCSessionDescriptionInit,
  ): Promise<void> {
    if (description.type === "rollback") {
      this.localDescription = null;
      this.signalingState = "stable";
      return;
    }
    this.localDescription = description as RTCSessionDescription;
    this.signalingState = "have-local-offer";
  }

  async setRemoteDescription(
    description: RTCSessionDescriptionInit,
  ): Promise<void> {
    this.remoteDescriptionCallCount += 1;
    if (this.deferRemoteDescriptionCall === this.remoteDescriptionCallCount) {
      this.deferRemoteDescriptionCall = null;
      await new Promise<void>((resolve) => {
        this.releaseRemoteDescription = resolve;
      });
    }
    this.remoteDescription = description as RTCSessionDescription;
    this.signalingState = "stable";
  }

  releaseDeferredRemoteDescription(): void {
    this.releaseRemoteDescription?.();
    this.releaseRemoteDescription = null;
  }

  async addIceCandidate(candidate: RTCIceCandidateInit | null): Promise<void> {
    this.addedIceCandidates.push(candidate);
  }

  readonly getStats = vi.fn(async (): Promise<RTCStatsReport> =>
    await (this.statsReports.shift() ?? emptyStatsReport())
  );

  close(): void {
    if (this.connectionState !== "closed") {
      FakePeerConnection.activeCount -= 1;
    }
    this.connectionState = "closed";
  }
}

export function emptyStatsReport(): RTCStatsReport {
  return new Map() as unknown as RTCStatsReport;
}

export function sendStatsReport({
  bytesSent,
  framesEncoded,
  timestamp,
  totalEncodeTime,
  qualityLimitationReason,
  trackIdentifier = "video",
}: {
  bytesSent: number;
  framesEncoded: number;
  timestamp: number;
  totalEncodeTime?: number;
  qualityLimitationReason: string;
  trackIdentifier?: string;
}): RTCStatsReport {
  return new Map<string, Record<string, unknown>>([
    [
      "transport",
      {
        id: "transport",
        type: "transport",
        timestamp,
        selectedCandidatePairId: "pair",
      },
    ],
    [
      "pair",
      {
        id: "pair",
        type: "candidate-pair",
        timestamp,
        transportId: "transport",
        state: "succeeded",
        nominated: true,
        localCandidateId: "local",
        remoteCandidateId: "remote",
        currentRoundTripTime: 0.02,
        availableOutgoingBitrate: 6_000_000,
      },
    ],
    [
      "local",
      {
        id: "local",
        type: "local-candidate",
        timestamp,
        candidateType: "host",
        protocol: "udp",
      },
    ],
    [
      "remote",
      {
        id: "remote",
        type: "remote-candidate",
        timestamp,
        candidateType: "host",
        protocol: "udp",
      },
    ],
    [
      "outbound-video",
      {
        id: "outbound-video",
        type: "outbound-rtp",
        timestamp,
        kind: "video",
        transportId: "transport",
        mediaSourceId: "video-source",
        bytesSent,
        framesEncoded,
        framesPerSecond: 30,
        frameWidth: 1280,
        frameHeight: 720,
        totalEncodeTime: totalEncodeTime ?? framesEncoded * 0.005,
        qualityLimitationReason,
        encoderImplementation: "test-encoder",
        codecId: "codec",
      },
    ],
    [
      "video-source",
      {
        id: "video-source",
        type: "media-source",
        timestamp,
        kind: "video",
        trackIdentifier,
      },
    ],
    [
      "codec",
      {
        id: "codec",
        type: "codec",
        timestamp,
        transportId: "transport",
        mimeType: "video/VP8",
      },
    ],
  ]) as unknown as RTCStatsReport;
}

export function withOutboundAudio(
  report: RTCStatsReport,
  tracks: readonly {
    id: string;
    trackIdentifier: string;
    bytesSent: number;
  }[],
): RTCStatsReport {
  const merged = new Map(
    report as unknown as Map<string, Record<string, unknown>>,
  );
  for (const [index, track] of tracks.entries()) {
    const sourceId = `${track.id}-source`;
    merged.set(track.id, {
      id: track.id,
      type: "outbound-rtp",
      timestamp: Number(merged.get("outbound-video")?.timestamp),
      kind: "audio",
      ssrc: index + 1,
      mediaSourceId: sourceId,
      bytesSent: track.bytesSent,
    });
    merged.set(sourceId, {
      id: sourceId,
      type: "media-source",
      timestamp: Number(merged.get("outbound-video")?.timestamp),
      kind: "audio",
      trackIdentifier: track.trackIdentifier,
    });
  }
  return merged as unknown as RTCStatsReport;
}

export function createTrack(kind: "video" | "audio", id: string): MediaStreamTrack {
  const track = Object.assign(new EventTarget(), {
    id,
    kind,
    contentHint: "",
    enabled: true,
    getSettings: () => ({}),
    applyConstraints: vi.fn(async () => undefined),
    stop: vi.fn(),
  }) as unknown as MediaStreamTrack;
  track.clone = vi.fn(() => {
    const clone = createTrack(kind, id);
    clone.getSettings = track.getSettings.bind(track);
    return clone;
  });
  return track;
}

export function createConfiguredVideoTrack(
  id: string,
  width: number,
  height: number,
  frameRate: number,
): MediaStreamTrack {
  const track = createTrack("video", id);
  track.getSettings = () => ({ width, height, frameRate });
  track.clone = vi.fn(() =>
    createConfiguredVideoTrack(id, width, height, frameRate),
  );
  return track;
}

export function createStream(
  videoTrack: MediaStreamTrack,
  audioTrack: MediaStreamTrack | null,
): MediaStream {
  const tracks = audioTrack ? [videoTrack, audioTrack] : [videoTrack];
  return {
    getTracks: () => tracks,
    getVideoTracks: () => [videoTrack],
    getAudioTracks: () => (audioTrack ? [audioTrack] : []),
  } as unknown as MediaStream;
}

export function createPeer(
  stream: MediaStream,
  onUpdate: (snapshot: PeerSnapshot) => void = () => undefined,
  iceConfig: IceConfig = { iceServers: [] },
  profile: QualityProfile = QUALITY_PROFILES["720p30"],
  videoCodec: BrowserVideoCodecPreference = VP8_ONLY_VIDEO_CODEC,
  natPredictionEnabled = false,
): HostPeer {
  return new HostPeer(
    "viewer-peer",
    iceConfig,
    stream,
    profile,
    {
      sendSignal: () => true,
      onUpdate,
    },
    videoCodec,
    undefined,
    natPredictionEnabled,
  );
}

export async function acceptPeerAnswer(peer: HostPeer): Promise<void> {
  const connection = FakePeerConnection.latest!;
  await peer.acceptSignal({
    kind: "description",
    connectionId: peer.connectionId,
    description: { type: "answer", sdp: "test-answer" },
  });
  connection.connectionState = "connected";
  connection.dispatchEvent(new Event("connectionstatechange"));
  await completeVideoStartup(connection);
}

export async function completeVideoStartup(
  connection: FakePeerConnection,
  framesEncoded = 5,
): Promise<void> {
  const statsCalls = connection.getStats.mock.calls.length;
  connection.statsReports.push(
    sendStatsReport({
      bytesSent: 10_000,
      framesEncoded,
      timestamp: 1_000,
      qualityLimitationReason: "none",
      trackIdentifier: connection.senders[0]?.track?.id ?? "video",
    }),
  );
  statsCallbacks.at(-1)!();
  await vi.waitFor(() =>
    expect(connection.getStats).toHaveBeenCalledTimes(statsCalls + 1),
  );
  await new Promise<void>((resolve) => setImmediate(resolve));
}

export const installHostPeerBrowser = () => {
  setCopy({ lang: "zh", vis: false });
  codecPreflight.probe.mockReset();
  codecPreflight.probe.mockResolvedValue("vp8");
  FakePeerConnection.latest = null;
  FakePeerConnection.instances = [];
  FakePeerConnection.activeCount = 0;
  FakePeerConnection.peakActiveCount = 0;
  FakePeerConnection.offersFailing = 0;
  FakePeerConnection.constructorsFailing = 0;
  FakePeerConnection.omitCodecPreferenceSetter = false;
  FakePeerConnection.codecPreferenceCallsFailing = 0;
  statsCallbacks.length = 0;
  vi.stubGlobal("RTCPeerConnection", FakePeerConnection);
  vi.stubGlobal("RTCRtpSender", {
    getCapabilities: () => ({
      codecs: [
        { mimeType: "video/VP8", clockRate: 90_000 },
        { mimeType: "video/H264", clockRate: 90_000 },
      ],
      headerExtensions: [],
    }),
  });
  vi.stubGlobal("window", {
    setInterval: vi.fn((callback: () => void) => {
      statsCallbacks.push(callback);
      return statsCallbacks.length;
    }),
    clearInterval: vi.fn(),
    setTimeout: (callback: () => void, delay: number) => {
      const timer = globalThis.setTimeout(callback, delay);
      timeouts.push(timer);
      return timer;
    },
    clearTimeout: (timer: ReturnType<typeof setTimeout>) =>
      globalThis.clearTimeout(timer),
  });
};

export const restoreHostPeerBrowser = () => {
  for (const timer of timeouts.splice(0)) globalThis.clearTimeout(timer);
  vi.useRealTimers();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
};

export function encodedPeerFixture() {
  let failVideo!: () => void;
  let audio!: ReadableStreamDefaultController<RTCEncodedAudioFrame>;
  const output = { track: createTrack("video", "carrier"), dispose: vi.fn(), passthrough: vi.fn(), setPaused: vi.fn() };
  const binding = { dispose: vi.fn(), carrierScale: vi.fn<() => number | undefined>(() => undefined),
    setPaused: vi.fn(), updateProfile: vi.fn(async () => undefined), metrics: (value: unknown) => value };
  const create = vi.fn<BrowserEncodingPool["create"]>(() => binding as never);
  const pool = { create } as unknown as BrowserEncodingPool;
  vi.spyOn(encodingOutput, "supportsBrowserEncoding").mockReturnValue(true);
  vi.spyOn(encodingOutput, "BrowserEncodingOutput").mockImplementation(function (_sender, onFailure) {
    failVideo = onFailure;
    return output as unknown as encodingOutput.BrowserEncodingOutput;
  });
  vi.spyOn(encodingOutput, "encodedStreams").mockReturnValue({
    readable: new ReadableStream({ start(controller) { audio = controller; } }),
    writable: new WritableStream(),
  } as never);
  return { pool, create, binding, output, failVideo: () => failVideo(),
    failAudio: () => audio.error(new Error("audio transform failed")) };
}

export function hostAssignment(
  childPeerIds: string[],
  publicationGeneration: string | null = null,
): ParticipantRouteAssignment {
  return {
    upstream: { kind: "none" },
    childPeerIds,
    sfuPublicationGeneration: publicationGeneration,
  };
}

export function hostProvisionalInput(
  revision: number,
  childPeerIds: string[],
  stream: MediaStream,
  overrides: {
    activeChildPeerIds?: string[];
    publicationGeneration?: string | null;
  } = {},
) {
  return {
    revision,
    candidate: {
      childPeerId: childPeerIds.at(-1) ?? "candidate-child",
      connectionId: `candidate-connection-${revision}`,
      transport: "direct" as const,
      qualityProbe: false,
    },
    assignment: hostAssignment(
      childPeerIds,
      overrides.publicationGeneration ?? null,
    ),
    activeChildPeerIds: overrides.activeChildPeerIds ?? [],
    maxMediaEdges: 2,
    iceConfig: { iceServers: [] },
    stream,
    profile: QUALITY_PROFILES["720p30"],
    videoCodec: VP8_ONLY_VIDEO_CODEC,
    natPredictionEnabled: false,
  };
}
