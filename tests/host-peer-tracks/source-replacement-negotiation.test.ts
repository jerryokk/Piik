import { FakePeerConnection, createTrack, createConfiguredVideoTrack, createStream, createPeer, acceptPeerAnswer, installHostPeerBrowser, restoreHostPeerBrowser } from "./support.ts";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { QUALITY_PROFILES } from "../../src/client/media/quality.ts";
import { type PeerSnapshot } from "../../src/client/types.ts";
import { HostPeer } from "../../src/client/webrtc/host-peer";
import { automaticVideoCodecPreference, manualVideoCodecPreference, VP8_ONLY_VIDEO_CODEC } from "../../src/client/webrtc/video-codec.ts";
import { type SignalPayload } from "../../src/shared/protocol.ts";

beforeEach(installHostPeerBrowser);
afterEach(restoreHostPeerBrowser);
describe("HostPeer source replacement", () => {
  it("does not expose WebRTC exception text in connection errors", async () => {
    const updates: PeerSnapshot[] = [];
    FakePeerConnection.offersFailing = 1;
    const peer = createPeer(
      createStream(createTrack("video", "video"), null),
      (snapshot) => updates.push(snapshot),
    );

    await expect(peer.start()).resolves.toBe(false);

    // The snapshot carries a copy key, so raw WebRTC text cannot leak.
    expect(updates.at(-1)?.error).toEqual({ key: "host.err.createConnection" });
  });

  it("offers only advertised VP8 and repair codecs", async () => {
    vi.stubGlobal("RTCRtpSender", {
      getCapabilities: () => ({
        codecs: [
          { mimeType: "video/H264", clockRate: 90_000 },
          { mimeType: "video/rtx", clockRate: 90_000 },
          { mimeType: "video/VP9", clockRate: 90_000 },
          { mimeType: "video/RED", clockRate: 90_000 },
          { mimeType: "video/VP8", clockRate: 90_000 },
          { mimeType: "video/ulpfec", clockRate: 90_000 },
          { mimeType: "video/AV1", clockRate: 90_000 },
          { mimeType: "video/flexfec-03", clockRate: 90_000 },
        ],
        headerExtensions: [],
      }),
    });
    const peer = createPeer(createStream(createTrack("video", "video"), null));

    await expect(peer.start()).resolves.toBe(true);

    const connection = FakePeerConnection.latest!;
    expect(
      connection.codecPreferenceCalls[0]?.map(({ mimeType }) =>
        mimeType.toLowerCase(),
      ),
    ).toEqual([
      "video/vp8",
      "video/rtx",
      "video/red",
      "video/ulpfec",
      "video/flexfec-03",
    ]);
    expect(connection.createOfferCallCount).toBe(1);
  });

  it("prefers native H264 mode 1 and retains VP8 fallback", async () => {
    vi.stubGlobal("RTCRtpSender", {
      getCapabilities: () => ({
        codecs: [
          {
            mimeType: "video/H264",
            clockRate: 90_000,
            sdpFmtpLine: "packetization-mode=0;profile-level-id=42001f",
          },
          {
            mimeType: "video/H264",
            clockRate: 90_000,
            sdpFmtpLine: "packetization-mode=1;profile-level-id=42001f",
          },
          { mimeType: "video/VP8", clockRate: 90_000 },
          { mimeType: "video/rtx", clockRate: 90_000 },
          { mimeType: "video/RED", clockRate: 90_000 },
        ],
        headerExtensions: [],
      }),
    });
    const peer = createPeer(
      createStream(createTrack("video", "video"), null),
      () => undefined,
      { iceServers: [] },
      QUALITY_PROFILES["720p30"],
      automaticVideoCodecPreference("h264"),
    );

    await expect(peer.start()).resolves.toBe(true);

    expect(
      FakePeerConnection.latest!.codecPreferenceCalls[0]?.map((codec) => [
        codec.mimeType.toLowerCase(),
        codec.sdpFmtpLine ?? null,
      ]),
    ).toEqual([
      ["video/h264", "packetization-mode=1;profile-level-id=42001f"],
      ["video/vp8", null],
      ["video/rtx", null],
      ["video/red", null],
    ]);
  });

  it("keeps a manual H264 selection strict", async () => {
    vi.stubGlobal("RTCRtpSender", {
      getCapabilities: () => ({
        codecs: [
          {
            mimeType: "video/H264",
            clockRate: 90_000,
            sdpFmtpLine: "packetization-mode=1;profile-level-id=42001f",
          },
          { mimeType: "video/VP8", clockRate: 90_000 },
          { mimeType: "video/rtx", clockRate: 90_000 },
        ],
        headerExtensions: [],
      }),
    });
    const peer = createPeer(
      createStream(createTrack("video", "video"), null),
      () => undefined,
      { iceServers: [] },
      QUALITY_PROFILES["720p30"],
      manualVideoCodecPreference("h264"),
    );

    await expect(peer.start()).resolves.toBe(true);

    expect(
      FakePeerConnection.latest!.codecPreferenceCalls[0]?.map(({ mimeType }) =>
        mimeType.toLowerCase(),
      ),
    ).toEqual(["video/h264", "video/rtx"]);
  });

  it("fails before creating an offer when codec preferences are unavailable", async () => {
    FakePeerConnection.omitCodecPreferenceSetter = true;
    const updates: PeerSnapshot[] = [];
    const peer = createPeer(
      createStream(createTrack("video", "video"), null),
      (snapshot) => updates.push(snapshot),
    );

    await expect(peer.start()).resolves.toBe(false);

    expect(FakePeerConnection.latest!.createOfferCallCount).toBe(0);
    expect(FakePeerConnection.latest!.localDescription).toBeNull();
    expect(updates.at(-1)?.error).toEqual({ key: "host.err.codecUnsupported" });
  });

  it("fails before creating an offer when VP8 is unavailable", async () => {
    vi.stubGlobal("RTCRtpSender", {
      getCapabilities: () => ({
        codecs: [
          { mimeType: "video/H264", clockRate: 90_000 },
          { mimeType: "video/rtx", clockRate: 90_000 },
        ],
        headerExtensions: [],
      }),
    });
    const updates: PeerSnapshot[] = [];
    const peer = createPeer(
      createStream(createTrack("video", "video"), null),
      (snapshot) => updates.push(snapshot),
    );

    await expect(peer.start()).resolves.toBe(false);

    expect(FakePeerConnection.latest!.createOfferCallCount).toBe(0);
    expect(FakePeerConnection.latest!.localDescription).toBeNull();
    expect(updates.at(-1)?.error).toEqual({ key: "host.err.codecUnsupported" });
  });

  it("fails before creating an offer when codec preference setup fails", async () => {
    FakePeerConnection.codecPreferenceCallsFailing = 1;
    const updates: PeerSnapshot[] = [];
    const peer = createPeer(
      createStream(createTrack("video", "video"), null),
      (snapshot) => updates.push(snapshot),
    );

    await expect(peer.start()).resolves.toBe(false);

    expect(FakePeerConnection.latest!.createOfferCallCount).toBe(0);
    expect(FakePeerConnection.latest!.localDescription).toBeNull();
    expect(updates.at(-1)?.error).toEqual({ key: "host.err.codecUnsupported" });
  });

  it("applies STUN-only ICE configuration at creation and update", () => {
    const peer = createPeer(
      createStream(createTrack("video", "video"), null),
      () => undefined,
      { iceServers: [{ urls: ["stun:stun-a.example.test:3478"] }] },
    );
    const connection = FakePeerConnection.latest!;

    peer.updateIceConfig({
      iceServers: [{ urls: ["stun:stun-b.example.test:3478"] }],
    });

    expect(connection.configurations).toEqual([
      { iceServers: [{ urls: ["stun:stun-a.example.test:3478"] }] },
      { iceServers: [{ urls: ["stun:stun-b.example.test:3478"] }] },
    ]);
    expect(connection.setConfiguration).toHaveBeenCalledOnce();
    for (const configuration of connection.configurations) {
      expect(
        configuration.iceServers?.every(
          (server) =>
            !Reflect.has(server, "username") &&
            !Reflect.has(server, "credential") &&
            (typeof server.urls === "string"
              ? server.urls.startsWith("stun:")
              : server.urls.every((url) => url.startsWith("stun:"))),
        ),
      ).toBe(true);
    }
  });

  it("keeps ordinary candidates while opting into bounded NAT predictions", () => {
    const signals: SignalPayload[] = [];
    const peer = new HostPeer(
      "viewer-peer",
      {
        iceServers: [{ urls: "stun:share.example.test:3478" }],
        natPredictionStunUrls: [
          "stun:share.example.test:3479",
          "stun:share.example.test:3480",
        ],
      },
      createStream(createTrack("video", "video"), null),
      QUALITY_PROFILES["720p30"],
      {
        sendSignal: (_peerId, payload) => {
          signals.push(payload);
          return true;
        },
        onUpdate: () => undefined,
      },
      VP8_ONLY_VIDEO_CODEC,
      undefined,
      true,
    );
    const connection = FakePeerConnection.latest!;
    expect(connection.configurations[0]?.iceServers).toEqual([
      { urls: "stun:share.example.test:3478" },
      { urls: "stun:share.example.test:3479" },
      { urls: "stun:share.example.test:3480" },
    ]);

    const surveyUrls = [
      "stun:share.example.test:3478",
      "stun:share.example.test:3479",
      "stun:share.example.test:3480",
    ];
    const emitCandidate = (port: number | null, url?: string): void => {
      const event = new Event("icecandidate");
      Object.defineProperty(event, "candidate", {
        value:
          port === null
            ? null
            : {
                candidate:
                  `candidate:base 1 udp 2122260223 203.0.113.7 ${port} ` +
                  "typ srflx raddr 192.0.2.7 rport 50000 generation 0 ufrag test",
                sdpMid: "0",
                sdpMLineIndex: 0,
                usernameFragment: "test",
                url,
              },
      });
      connection.dispatchEvent(event);
    };
    emitCandidate(40_000, surveyUrls[0]);
    emitCandidate(40_003, surveyUrls[1]);
    emitCandidate(40_006, surveyUrls[2]);
    connection.iceGatheringState = "complete";
    connection.dispatchEvent(new Event("icegatheringstatechange"));
    emitCandidate(null);

    const candidateSignals = signals.filter(
      (signal): signal is Extract<SignalPayload, { kind: "candidate" }> =>
        signal.kind === "candidate",
    );
    const predictionIndex = candidateSignals.findIndex((signal) =>
      signal.candidate?.candidate.includes("candidate:s"),
    );
    const ordinaryIndex = candidateSignals.findIndex((signal) =>
      signal.candidate?.candidate.startsWith("candidate:base"),
    );
    expect(ordinaryIndex).toBe(0);
    expect(predictionIndex).toBeGreaterThanOrEqual(0);
    expect(predictionIndex).toBeGreaterThan(ordinaryIndex);
    expect(
      candidateSignals.filter((signal) =>
        signal.candidate?.candidate.startsWith("candidate:base"),
      ),
    ).toHaveLength(3);
    expect(candidateSignals.at(-1)?.candidate).toMatchObject({ candidate: "", usernameFragment: "test" });
    expect(candidateSignals.filter((signal) => signal.candidate?.candidate === "")).toHaveLength(1);
    peer.dispose();
  });

  it("reserves send-only video and audio senders and replaces both tracks", async () => {
    const oldVideo = createTrack("video", "old-video");
    const oldAudio = createTrack("audio", "old-audio");
    const peer = createPeer(
      createStream(oldVideo, oldAudio),
      () => undefined,
      { iceServers: [] },
      {
        ...QUALITY_PROFILES["720p30"],
        maxAudioBitrate: 192000,
      },
    );

    await expect(peer.start()).resolves.toBe(true);
    await acceptPeerAnswer(peer);
    const connection = FakePeerConnection.latest!;
    const oldSenderVideo = connection.senders[0]!.track!;
    expect(oldSenderVideo).not.toBe(oldVideo);
    expect(connection.transceiverInputs).toHaveLength(2);
    expect(connection.transceiverInputs.map(({ init }) => init?.direction)).toEqual([
      "sendonly",
      "sendonly",
    ]);

    const nextVideo = createTrack("video", "next-video");
    const nextAudio = createTrack("audio", "next-audio");
    await expect(
      peer.replaceStream(createStream(nextVideo, nextAudio)),
    ).resolves.toBe(true);

    const nextSenderVideo = connection.senders[0]?.track;
    expect(nextSenderVideo).not.toBe(nextVideo);
    expect(nextSenderVideo?.id).toBe(nextVideo.id);
    expect(connection.senders[1]?.track).toBe(nextAudio);
    expect(oldVideo.stop).not.toHaveBeenCalled();
    expect(oldSenderVideo.stop).toHaveBeenCalledOnce();
    expect(connection.transceiverInputs).toHaveLength(2);
    expect(connection.senders[0]?.setParameters).toHaveBeenCalledTimes(3);
    expect(connection.senders[1]?.appliedMaxBitrates).toEqual([
      192_000,
      192_000,
    ]);
    peer.dispose();
    expect(nextVideo.stop).not.toHaveBeenCalled();
    expect(nextSenderVideo?.stop).toHaveBeenCalledOnce();
  });

  it.each(["video", "audio", "rollback"])("retires a replacement blocked in %s without waiting for the browser", async (stage) => {
    const video = createTrack("video", "old-video");
    const audio = createTrack("audio", "old-audio");
    const peer = createPeer(createStream(video, audio));
    await peer.start();
    const connection = FakePeerConnection.latest!;
    const videoSender = connection.senders[0]!;
    const audioSender = connection.senders[1]!;
    const previousClone = videoSender.track!;
    videoSender.deferReplaceCall = stage === "video" ? 1 : stage === "rollback" ? 2 : null;
    audioSender.deferReplaceCall = stage === "audio" ? 1 : null;
    audioSender.failNextReplace = stage === "rollback";
    const nextVideo = createTrack("video", "next-video");
    const nextAudio = createTrack("audio", "next-audio");
    const replacing = peer.replaceStream(createStream(nextVideo, nextAudio));
    await vi.waitFor(() => expect(
      stage === "audio" ? audioSender.replaceTrack : videoSender.replaceTrack,
    ).toHaveBeenCalledTimes(stage === "rollback" ? 2 : 1));
    const replacementClone = vi.mocked(nextVideo.clone).mock.results[0]!.value;
    const queuedProfile = peer.updateProfile(QUALITY_PROFILES["1080p30"]);
    peer.dispose();
    await expect(replacing).resolves.toBe(false);
    await expect(queuedProfile).resolves.toBe(false);
    expect(previousClone.stop).toHaveBeenCalledOnce();
    expect(replacementClone.stop).toHaveBeenCalledOnce();
    for (const source of [video, audio, nextVideo, nextAudio]) expect(source.stop).not.toHaveBeenCalled();
    const audioCalls = audioSender.replaceTrack.mock.calls.length;
    const offers = connection.createOfferCallCount;
    videoSender.releaseDeferredReplaceTrack();
    audioSender.releaseDeferredReplaceTrack();
    await new Promise<void>((resolve) => setImmediate(resolve));
    expect(audioSender.replaceTrack).toHaveBeenCalledTimes(audioCalls);
    expect(connection.createOfferCallCount).toBe(offers);
  });

  it.each([false, true])("keeps the video sender and quality when only audio changes (failure=%s)", async (fail) => {
    const video = createTrack("video", "same-video");
    const peer = createPeer(createStream(video, null));
    await peer.start();
    await acceptPeerAnswer(peer);
    const connection = FakePeerConnection.latest!;
    const videoSender = connection.senders[0]!;
    const audioSender = connection.senders[1]!;
    const clone = videoSender.track!;
    const parametersBefore = videoSender.setParameters.mock.calls.length;
    const offersBefore = connection.createOfferCallCount;
    audioSender.failNextReplace = fail;
    const audio = createTrack("audio", "mixed-audio");
    expect(await peer.replaceStream(createStream(video, audio))).toBe(!fail);
    expect(videoSender.replaceTrack).not.toHaveBeenCalled();
    expect(videoSender.setParameters).toHaveBeenCalledTimes(parametersBefore);
    expect(videoSender.track).toBe(clone);
    expect(clone.stop).not.toHaveBeenCalled();
    expect(video.clone).toHaveBeenCalledOnce();
    expect(audioSender.track).toBe(fail ? null : audio);
    expect(connection.createOfferCallCount).toBe(offersBefore + (fail ? 0 : 1));
    peer.dispose();
  });

  it("keeps capture controls on the sender-owned video clone", async () => {
    const sourceVideo = createConfiguredVideoTrack(
      "capture-video",
      1920,
      1080,
      30,
    );
    sourceVideo.contentHint = "motion";
    const sourceAudio = createTrack("audio", "capture-audio");
    const peer = createPeer(createStream(sourceVideo, sourceAudio));

    await expect(peer.start()).resolves.toBe(true);
    const senderVideo = FakePeerConnection.latest!.senders[0]!.track!;
    expect(senderVideo).not.toBe(sourceVideo);
    expect(senderVideo.contentHint).toBe("motion");

    peer.setPaused(true);
    expect(senderVideo.enabled).toBe(false);
    expect(sourceVideo.enabled).toBe(true);
    peer.setPaused(false);
    expect(senderVideo.enabled).toBe(true);

    await expect(
      peer.updateCaptureProfile(QUALITY_PROFILES["1080p60"]),
    ).resolves.toBe(true);
    expect(senderVideo.applyConstraints).toHaveBeenLastCalledWith({
      width: { max: 1920 },
      height: { max: 1080 },
      frameRate: { ideal: 60, max: 60 },
    });
    expect(sourceVideo.applyConstraints).not.toHaveBeenCalled();

    peer.dispose();
    expect(senderVideo.stop).toHaveBeenCalledOnce();
    expect(sourceVideo.stop).not.toHaveBeenCalled();
  });

  it("applies pause authority to a source replacement while it is in flight", async () => {
    const peer = createPeer(
      createStream(
        createTrack("video", "old-video"),
        createTrack("audio", "old-audio"),
      ),
    );
    await expect(peer.start()).resolves.toBe(true);
    const connection = FakePeerConnection.latest!;
    const videoSender = connection.senders[0]!;
    const nextVideo = createTrack("video", "next-video");
    const nextAudio = createTrack("audio", "next-audio");
    videoSender.deferReplaceCall = 1;

    const replacing = peer.replaceStream(createStream(nextVideo, nextAudio));
    await vi.waitFor(() =>
      expect(videoSender.replaceTrack).toHaveBeenCalledOnce(),
    );
    const replacementVideo = videoSender.replaceTrack.mock.calls[0]![0]!;
    peer.setPaused(true);
    expect(replacementVideo.enabled).toBe(false);
    expect(nextAudio.enabled).toBe(false);

    videoSender.releaseDeferredReplaceTrack();
    await expect(replacing).resolves.toBe(true);
    expect(videoSender.track?.enabled).toBe(false);
    expect(connection.senders[1]!.track?.enabled).toBe(false);
    peer.setPaused(false);
    expect(videoSender.track?.enabled).toBe(true);
    expect(connection.senders[1]!.track?.enabled).toBe(true);
  });
});
