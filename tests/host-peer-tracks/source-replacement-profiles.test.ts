import { statsCallbacks, FakePeerConnection, emptyStatsReport, sendStatsReport, withOutboundAudio, createTrack, createConfiguredVideoTrack, createStream, createPeer, acceptPeerAnswer, completeVideoStartup, installHostPeerBrowser, restoreHostPeerBrowser } from "./support.ts";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { QUALITY_PROFILES } from "../../src/client/media/quality.ts";
import { type PeerSnapshot } from "../../src/client/types.ts";

beforeEach(installHostPeerBrowser);
afterEach(restoreHostPeerBrowser);
describe("HostPeer source replacement", () => {
  it("retains restart candidates that arrive before the new answer", async () => {
    const peer = createPeer(createStream(createTrack("video", "video"), null));
    await peer.start();
    const connection = FakePeerConnection.latest!;
    await peer.acceptSignal({
      kind: "description", connectionId: peer.connectionId,
      description: { type: "answer", sdp: "previous-answer" },
    });
    await expect(peer.restartIce()).resolves.toBe(true);
    const nextCandidate = {
      candidate: "candidate:next 1 udp 2122260223 192.0.2.8 50001 typ host ufrag next",
      usernameFragment: "next", sdpMid: "0", sdpMLineIndex: 0,
    };
    await peer.acceptSignal({
      kind: "candidate", connectionId: peer.connectionId, candidate: nextCandidate,
    });
    // Native receiver events can precede the receive-offer RPC response.
    expect(connection.addedIceCandidates).toEqual([]);
    connection.deferRemoteDescriptionCall = 2;
    const answer = peer.acceptSignal({
      kind: "description", connectionId: peer.connectionId,
      description: { type: "answer", sdp: "next-answer" },
    });
    await vi.waitFor(() => expect(connection.remoteDescriptionCallCount).toBe(2));
    const end = peer.acceptSignal({
      kind: "candidate", connectionId: peer.connectionId, candidate: null,
    });
    connection.releaseDeferredRemoteDescription();
    await Promise.all([answer, end]);
    expect(connection.remoteDescription?.sdp).toBe("next-answer");
    expect(connection.addedIceCandidates).toEqual([nextCandidate, null]);
    peer.dispose();
  });

  it("drains valid restart candidates after an old-generation candidate is rejected", async () => {
    const peer = createPeer(createStream(createTrack("video", "video"), null));
    await peer.start();
    const connection = FakePeerConnection.latest!;
    await peer.acceptSignal({ kind: "description", connectionId: peer.connectionId,
      description: { type: "answer", sdp: "previous-answer" } });
    await peer.restartIce();
    const stale = { candidate: "stale-candidate", sdpMid: "0", usernameFragment: "previous" };
    const current = { candidate: "current-candidate", sdpMid: "0", usernameFragment: "current" };
    for (const candidate of [stale, current]) {
      await peer.acceptSignal({ kind: "candidate", connectionId: peer.connectionId, candidate });
    }
    vi.spyOn(connection, "addIceCandidate").mockRejectedValueOnce(
      new DOMException("Unknown ICE username fragment", "OperationError"),
    );
    await peer.acceptSignal({ kind: "description", connectionId: peer.connectionId,
      description: { type: "answer", sdp: "current-answer" } });
    expect(connection.addedIceCandidates).toEqual([current]);
    expect(peer.getSnapshot().error).toBeNull();
    peer.dispose();
  });

  it("enables the selected balanced profile after startup frames", async () => {
    const video = createTrack("video", "video");
    const peer = createPeer(createStream(video, createTrack("audio", "audio")));

    await expect(peer.start()).resolves.toBe(true);
    const connection = FakePeerConnection.latest!;
    expect(connection.senders[0]?.setParameters).toHaveBeenCalledOnce();
    expect(
      connection.senders[0]?.setParameters.mock.calls[0]?.[0],
    ).toMatchObject({ degradationPreference: "maintain-resolution" });
    const pendingCandidate = { candidate: "candidate-before-answer" };
    await peer.acceptSignal({
      kind: "candidate",
      connectionId: peer.connectionId,
      candidate: pendingCandidate,
    });
    expect(connection.addedIceCandidates).toEqual([]);
    await peer.acceptSignal({
      kind: "description",
      connectionId: peer.connectionId,
      description: { type: "answer", sdp: "test-answer" },
    });

    expect(connection.remoteDescription?.type).toBe("answer");
    expect(connection.addedIceCandidates).toEqual([pendingCandidate]);
    expect(connection.senders[0]?.track).not.toBe(video);
    expect(connection.senders[0]?.track?.id).toBe(video.id);
    expect(connection.senders[0]?.setParameters).toHaveBeenCalledOnce();

    connection.connectionState = "connected";
    connection.dispatchEvent(new Event("connectionstatechange"));
    await Promise.resolve();
    expect(connection.senders[0]?.setParameters).toHaveBeenCalledOnce();

    await completeVideoStartup(connection, 4);
    expect(connection.senders[0]?.setParameters).toHaveBeenCalledOnce();

    await completeVideoStartup(connection, 5);
    expect(connection.senders[0]?.setParameters).toHaveBeenCalledTimes(2);
    expect(
      connection.senders[0]?.setParameters.mock.calls.at(-1)?.[0],
    ).toMatchObject({
      degradationPreference: "balanced",
      encodings: [{ maxBitrate: 3_000_000, maxFramerate: 30 }],
    });
    expect(connection.senders[1]?.setParameters).toHaveBeenCalledOnce();
  });

  it.each([0, 250])("counts new source frames from a valid baseline of %s", async (baseline) => {
    const video = createTrack("video", "video");
    const peer = createPeer(createStream(video, createTrack("audio", "audio")));

    await expect(peer.start()).resolves.toBe(true);
    const connection = FakePeerConnection.latest!;
    await acceptPeerAnswer(peer);
    expect(
      connection.senders[0]?.setParameters.mock.calls.at(-1)?.[0],
    ).toMatchObject({ degradationPreference: "balanced" });

    const nextVideo = createTrack("video", "next-video");
    await expect(
      peer.replaceStream(createStream(nextVideo, null)),
    ).resolves.toBe(true);
    expect(
      connection.senders[0]?.setParameters.mock.calls.at(-1)?.[0],
    ).toMatchObject({ degradationPreference: "maintain-resolution" });
    const configureCalls =
      connection.senders[0]!.setParameters.mock.calls.length;

    const staleReport = sendStatsReport({
      bytesSent: 10_000,
      framesEncoded: 250,
      timestamp: 1_000,
      qualityLimitationReason: "none",
      trackIdentifier: video.id,
    });
    const missingCount = sendStatsReport({
      bytesSent: 10_000,
      framesEncoded: 0,
      timestamp: 1_000,
      qualityLimitationReason: "none",
      trackIdentifier: connection.senders[0]!.track!.id,
    });
    delete missingCount.get("outbound-video").framesEncoded;
    // Missing/currently unmatched stats cannot seed the new baseline as zero.
    for (const report of [emptyStatsReport(), staleReport, missingCount]) {
      connection.statsReports.push(report);
      statsCallbacks.at(-1)!();
      await new Promise<void>((resolve) => setImmediate(resolve));
    }

    // A valid zero is different from missing stats. An existing RTP stream may
    // instead retain a large cumulative count across replaceTrack.
    await completeVideoStartup(connection, baseline);
    expect(connection.senders[0]!.setParameters.mock.calls.length).toBe(
      configureCalls,
    );
    await completeVideoStartup(connection, baseline + 4);
    expect(connection.senders[0]!.setParameters.mock.calls.length).toBe(
      configureCalls,
    );
    await completeVideoStartup(connection, baseline + 5);
    expect(connection.senders[0]!.setParameters.mock.calls.length).toBe(
      configureCalls + 1,
    );
    expect(
      connection.senders[0]?.setParameters.mock.calls.at(-1)?.[0],
    ).toMatchObject({ degradationPreference: "balanced" });
  });

  it("applies the latest pre-answer profile once negotiation completes", async () => {
    const peer = createPeer(
      createStream(
        createTrack("video", "video"),
        createTrack("audio", "audio"),
      ),
    );

    await expect(peer.start()).resolves.toBe(true);
    const videoSender = FakePeerConnection.latest!.senders[0]!;
    expect(videoSender.setParameters).toHaveBeenCalledOnce();

    await expect(
      peer.updateProfile(QUALITY_PROFILES["1080p60"]),
    ).resolves.toBe(true);
    expect(videoSender.setParameters).toHaveBeenCalledOnce();

    await acceptPeerAnswer(peer);
    await vi.waitFor(() =>
      expect(videoSender.setParameters).toHaveBeenCalledTimes(2),
    );
    expect(videoSender.appliedMaxBitrates).toEqual([3_000_000, 8_000_000]);
  });

  it.each([false, true])("applies connecting profile changes without canceling an in-flight constraint: %s", async (holdConstraints) => {
    const peer = createPeer(createStream(createTrack("video", "video"), createTrack("audio", "audio")));
    await expect(peer.start()).resolves.toBe(true);
    const connection = FakePeerConnection.latest!;
    const sender = connection.senders[0]!;
    await peer.acceptSignal({
      kind: "description", connectionId: peer.connectionId,
      description: { type: "answer", sdp: "test-answer" },
    });
    await completeVideoStartup(connection, 5);
    const before = sender.setParameters.mock.calls.length;
    let release: (() => void) | undefined;
    if (holdConstraints) {
      vi.mocked(sender.track!.applyConstraints).mockImplementationOnce(() =>
        new Promise<void>((resolve) => { release = resolve; }),
      );
    }
    const changing = holdConstraints
      ? peer.updateCaptureProfile(QUALITY_PROFILES["1080p60"])
      : peer.updateProfile(QUALITY_PROFILES["1080p60"]);
    if (holdConstraints) {
      await vi.waitFor(() => expect(release).toBeTypeOf("function"));
    } else {
      await expect(changing).resolves.toBe(true);
    }
    expect(sender.setParameters).toHaveBeenCalledTimes(before);
    connection.connectionState = "connected";
    connection.dispatchEvent(new Event("connectionstatechange"));
    release?.();
    await expect(changing).resolves.toBe(true);
    await vi.waitFor(() => expect(sender.appliedMaxBitrates.at(-1)).toBe(8_000_000));
    expect(sender.setParameters).toHaveBeenCalledTimes(before + 1);
  });

  it("replays the desired profile only after a connected peer reconnects", async () => {
    const peer = createPeer(
      createStream(
        createTrack("video", "video"),
        createTrack("audio", "audio"),
      ),
    );

    await expect(peer.start()).resolves.toBe(true);
    await acceptPeerAnswer(peer);
    const connection = FakePeerConnection.latest!;
    const videoSender = connection.senders[0]!;
    const audioSender = connection.senders[1]!;
    const videoCalls = videoSender.setParameters.mock.calls.length;
    const audioCalls = audioSender.setParameters.mock.calls.length;

    connection.dispatchEvent(new Event("connectionstatechange"));
    await Promise.resolve();
    expect(videoSender.setParameters).toHaveBeenCalledTimes(videoCalls);
    expect(audioSender.setParameters).toHaveBeenCalledTimes(audioCalls);

    connection.connectionState = "disconnected";
    connection.dispatchEvent(new Event("connectionstatechange"));
    connection.connectionState = "connected";
    connection.dispatchEvent(new Event("connectionstatechange"));

    await vi.waitFor(() =>
      expect(videoSender.setParameters).toHaveBeenCalledTimes(videoCalls + 1),
    );
    expect(audioSender.setParameters).toHaveBeenCalledTimes(audioCalls + 1);
  });

  it("continues queued profile updates after initial configuration rejects", async () => {
    const peer = createPeer(
      createStream(
        createTrack("video", "video"),
        createTrack("audio", "audio"),
      ),
    );

    const starting = peer.start();
    const videoSender = FakePeerConnection.latest!.senders[0]!;
    videoSender.failNextSetParameters = true;
    await expect(starting).resolves.toBe(true);
    await acceptPeerAnswer(peer);

    await expect(
      peer.updateProfile(QUALITY_PROFILES["1080p60"]),
    ).resolves.toBe(true);
    expect(videoSender.setParameters).toHaveBeenCalledTimes(3);
    expect(videoSender.appliedMaxBitrates).toEqual([3_000_000, 8_000_000]);
  });

  it("can retry the selected quality after a sender update fails", async () => {
    const peer = createPeer(
      createStream(
        createTrack("video", "video"),
        createTrack("audio", "audio"),
      ),
    );

    await expect(peer.start()).resolves.toBe(true);
    await acceptPeerAnswer(peer);
    const videoSender = FakePeerConnection.latest!.senders[0]!;
    videoSender.failNextSetParameters = true;

    await expect(
      peer.updateProfile(QUALITY_PROFILES["1080p60"]),
    ).resolves.toBe(false);
    await expect(
      peer.updateProfile(QUALITY_PROFILES["1080p60"]),
    ).resolves.toBe(true);

    expect(videoSender.setParameters).toHaveBeenCalledTimes(4);
    expect(videoSender.setParameters.mock.calls.at(-1)?.[0]).toMatchObject({
      degradationPreference: "balanced",
      encodings: [{ maxBitrate: 8_000_000, maxFramerate: 60 }],
    });
  });

  it("discards an in-flight stats sample after a profile reset", async () => {
    let resolveOldStats!: (report: RTCStatsReport) => void;
    const oldStats = new Promise<RTCStatsReport>((resolve) => {
      resolveOldStats = resolve;
    });
    const updates: PeerSnapshot[] = [];
    const peer = createPeer(
      createStream(createTrack("video", "video"), null),
      (snapshot) => updates.push(snapshot),
    );

    await expect(peer.start()).resolves.toBe(true);
    await acceptPeerAnswer(peer);
    const connection = FakePeerConnection.latest!;
    connection.statsReports.push(oldStats);
    statsCallbacks[0]!();
    await vi.waitFor(() => expect(connection.statsReports).toHaveLength(0));
    statsCallbacks[0]!();
    expect(connection.getStats).toHaveBeenCalledTimes(2);

    await expect(
      peer.updateProfile(QUALITY_PROFILES["1080p60"]),
    ).resolves.toBe(true);
    resolveOldStats(
      sendStatsReport({
        bytesSent: 1_000_000,
        framesEncoded: 30,
        timestamp: 1_000,
        totalEncodeTime: 0.15,
        qualityLimitationReason: "cpu",
      }),
    );
    await new Promise<void>((resolve) => setImmediate(resolve));
    expect(
      updates.some(
        ({ metrics }) => metrics.qualityLimitationReason === "cpu",
      ),
    ).toBe(false);

    connection.statsReports.push(
      sendStatsReport({
        bytesSent: 2_000_000,
        framesEncoded: 60,
        timestamp: 2_000,
        totalEncodeTime: 0.3,
        qualityLimitationReason: "bandwidth",
      }),
      sendStatsReport({
        bytesSent: 2_500_000,
        framesEncoded: 90,
        timestamp: 3_000,
        totalEncodeTime: 0.9,
        qualityLimitationReason: "bandwidth",
      }),
    );
    statsCallbacks[0]!();
    await vi.waitFor(() =>
      expect(updates.at(-1)?.metrics.qualityLimitationReason).toBe(
        "bandwidth",
      ),
    );
    expect(updates.at(-1)?.metrics.intervalEncodeMs).toBeNull();

    statsCallbacks[0]!();
    await vi.waitFor(() =>
      expect(updates.at(-1)?.metrics.intervalEncodeMs).toBeCloseTo(20),
    );
    expect(
      updates.some(
        ({ metrics }) => metrics.qualityLimitationReason === "cpu",
      ),
    ).toBe(false);
  });

  it("publishes capture settings with outbound evidence from the same stats tick", async () => {
    const getSettings = vi.fn(() => ({
      width: 1920,
      height: 1080,
      frameRate: 59.94,
    }));
    const videoTrack = createTrack("video", "capture-video");
    videoTrack.getSettings = getSettings;
    const updates: PeerSnapshot[] = [];
    const peer = createPeer(
      createStream(videoTrack, null),
      (snapshot) => updates.push(snapshot),
    );
    await expect(peer.start()).resolves.toBe(true);
    getSettings.mockClear();
    const connection = FakePeerConnection.latest!;
    connection.statsReports.push(
      sendStatsReport({
        bytesSent: 1_000_000,
        framesEncoded: 30,
        timestamp: 1_234,
        qualityLimitationReason: "none",
        trackIdentifier: "capture-video",
      }),
    );

    expect(getSettings).not.toHaveBeenCalled();
    statsCallbacks[0]!();
    await vi.waitFor(() =>
      expect(updates.at(-1)?.metrics.sampleTimestampMs).toBe(1_234),
    );

    expect(connection.getStats).toHaveBeenCalledOnce();
    expect(getSettings).toHaveBeenCalledOnce();
    expect(updates.at(-1)?.metrics).toMatchObject({
      sampleTimestampMs: 1_234,
      rtpStatsId: "outbound-video",
      resolution: "1280x720",
      captureWidth: 1920,
      captureHeight: 1080,
      captureFramesPerSecond: 59.94,
    });
  });

  it("samples only the current audio sender and excludes old audio after removal", async () => {
    const video = createConfiguredVideoTrack("capture-video", 1920, 1080, 60);
    const audio = createTrack("audio", "current-audio");
    const updates: PeerSnapshot[] = [];
    const peer = createPeer(
      createStream(video, audio),
      (snapshot) => updates.push(snapshot),
    );
    await expect(peer.start()).resolves.toBe(true);
    const connection = FakePeerConnection.latest!;
    const report = (timestamp: number, currentBytes: number, oldBytes: number) =>
      withOutboundAudio(
        sendStatsReport({
          bytesSent: timestamp * 1_000,
          framesEncoded: timestamp / 20,
          timestamp,
          qualityLimitationReason: "none",
          trackIdentifier: video.id,
        }),
        [
          {
            id: "current-audio-out",
            trackIdentifier: audio.id,
            bytesSent: currentBytes,
          },
          {
            id: "old-audio-out",
            trackIdentifier: "old-audio",
            bytesSent: oldBytes,
          },
        ],
      );
    connection.statsReports.push(
      report(1_000, 10_000, 100_000),
      report(3_000, 50_000, 300_000),
    );

    statsCallbacks[0]!();
    await vi.waitFor(() =>
      expect(updates.at(-1)?.metrics.sampleTimestampMs).toBe(1_000),
    );
    statsCallbacks[0]!();
    await vi.waitFor(() =>
      expect(updates.at(-1)?.metrics.audioBitrateKbps).toBe(160),
    );

    const nextVideo = createConfiguredVideoTrack("next-video", 1280, 720, 30);
    await expect(
      peer.replaceStream(createStream(nextVideo, null)),
    ).resolves.toBe(true);
    connection.statsReports.push(
      withOutboundAudio(
        sendStatsReport({
          bytesSent: 4_000_000,
          framesEncoded: 200,
          timestamp: 4_000,
          qualityLimitationReason: "none",
          trackIdentifier: nextVideo.id,
        }),
        [
          {
            id: "old-audio-out",
            trackIdentifier: audio.id,
            bytesSent: 400_000,
          },
        ],
      ),
    );
    statsCallbacks[0]!();
    await vi.waitFor(() =>
      expect(updates.at(-1)?.metrics.sampleTimestampMs).toBe(4_000),
    );
    expect(updates.at(-1)?.metrics).toMatchObject({
      trackIdentifier: nextVideo.id,
      audioBitrateKbps: null,
      audioCodec: null,
    });
  });

  it.each([
    {
      outcome: "succeeds",
      rollback: false,
      deferredCall: 1,
      result: true,
      committed: "next" as const,
    },
    {
      outcome: "rolls back",
      rollback: true,
      deferredCall: 2,
      result: false,
      committed: "old" as const,
    },
  ])("blocks stale stats while replacement $outcome", async ({
    rollback,
    deferredCall,
    result,
    committed,
  }) => {
    const scenario = rollback ? "rollback" : "success";
    let resolveOldStats!: (report: RTCStatsReport) => void;
    const oldStats = new Promise<RTCStatsReport>((resolve) => {
      resolveOldStats = resolve;
    });
    const oldVideo = createConfiguredVideoTrack(
      `${scenario}-old-capture`,
      1280,
      720,
      30,
    );
    const nextVideo = createConfiguredVideoTrack(
      `${scenario}-next-capture`,
      1920,
      1080,
      60,
    );
    const oldAudio = rollback ? createTrack("audio", "old-audio") : null;
    const nextAudio = rollback ? createTrack("audio", "next-audio") : null;
    const updates: PeerSnapshot[] = [];
    const peer = createPeer(
      createStream(oldVideo, oldAudio),
      (snapshot) => updates.push(snapshot),
    );
    await expect(peer.start()).resolves.toBe(true);
    const connection = FakePeerConnection.latest!;
    const videoSender = connection.senders[0]!;
    const audioSender = connection.senders[1]!;
    const initialSenderVideo = videoSender.track;
    connection.statsReports.push(
      sendStatsReport({
        bytesSent: 500_000,
        framesEncoded: 15,
        timestamp: 500,
        qualityLimitationReason: "none",
        trackIdentifier: oldVideo.id,
      }),
    );
    statsCallbacks[0]!();
    await vi.waitFor(() =>
      expect(updates.at(-1)?.metrics.sampleTimestampMs).toBe(500),
    );
    connection.statsReports.push(oldStats);
    statsCallbacks[0]!();
    await vi.waitFor(() => expect(connection.getStats).toHaveBeenCalledTimes(2));

    videoSender.deferReplaceCall = deferredCall;
    audioSender.failNextReplace = rollback;
    const replacing = peer.replaceStream(
      createStream(nextVideo, nextAudio),
    );
    await vi.waitFor(() =>
      expect(videoSender.replaceTrack).toHaveBeenCalledTimes(deferredCall),
    );

    resolveOldStats(
      sendStatsReport({
        bytesSent: 1_000_000,
        framesEncoded: 30,
        timestamp: 1_000,
        qualityLimitationReason: "none",
        trackIdentifier: oldVideo.id,
      }),
    );
    await new Promise<void>((resolve) => setImmediate(resolve));
    expect(
      updates.some(({ metrics }) => metrics.sampleTimestampMs === 1_000),
    ).toBe(false);
    statsCallbacks[0]!();
    expect(connection.getStats).toHaveBeenCalledTimes(2);

    videoSender.releaseDeferredReplaceTrack();
    await expect(replacing).resolves.toBe(result);
    const committedVideo = committed === "old" ? oldVideo : nextVideo;
    if (committed === "old") {
      expect(videoSender.track).toBe(initialSenderVideo);
    } else {
      expect(videoSender.track).not.toBe(nextVideo);
      expect(videoSender.track?.id).toBe(nextVideo.id);
    }
    expect(updates.at(-1)?.metrics.sampleTimestampMs).toBe(
      committed === "old" ? 500 : null,
    );

    connection.statsReports.push(
      sendStatsReport({
        bytesSent: 2_000_000,
        framesEncoded: 60,
        timestamp: 2_000,
        qualityLimitationReason: "none",
        trackIdentifier: committedVideo.id,
      }),
    );
    statsCallbacks[0]!();
    await vi.waitFor(() =>
      expect(updates.at(-1)?.metrics.sampleTimestampMs).toBe(2_000),
    );
    expect(updates.at(-1)?.metrics).toMatchObject({
      trackIdentifier: committedVideo.id,
      captureWidth: committed === "old" ? 1280 : 1920,
      captureHeight: committed === "old" ? 720 : 1080,
      captureFramesPerSecond: committed === "old" ? 30 : 60,
    });
  });

  it("rolls the first sender back when the second replacement fails", async () => {
    const oldVideo = createTrack("video", "old-video");
    const oldAudio = createTrack("audio", "old-audio");
    const peer = createPeer(createStream(oldVideo, oldAudio));
    await expect(peer.start()).resolves.toBe(true);
    const connection = FakePeerConnection.latest!;
    const videoSender = connection.senders[0]!;
    const audioSender = connection.senders[1]!;
    const oldSenderVideo = videoSender.track;
    audioSender.failNextReplace = true;

    const nextVideo = createTrack("video", "next-video");
    const nextAudio = createTrack("audio", "next-audio");
    await expect(
      peer.replaceStream(createStream(nextVideo, nextAudio)),
    ).resolves.toBe(false);

    const nextSenderVideo = videoSender.replaceTrack.mock.calls[0]?.[0];
    expect(nextSenderVideo).not.toBe(nextVideo);
    expect(nextSenderVideo?.id).toBe(nextVideo.id);
    expect(videoSender.replaceTrack).toHaveBeenNthCalledWith(2, oldSenderVideo);
    expect(audioSender.replaceTrack).toHaveBeenNthCalledWith(1, nextAudio);
    expect(audioSender.replaceTrack).toHaveBeenNthCalledWith(2, oldAudio);
    expect(videoSender.track).toBe(oldSenderVideo);
    expect(nextSenderVideo?.stop).toHaveBeenCalledOnce();
    expect(oldSenderVideo?.stop).not.toHaveBeenCalled();
    expect(oldVideo.stop).not.toHaveBeenCalled();
    expect(audioSender.track).toBe(oldAudio);
  });
});
