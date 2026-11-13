import { FakePeerConnection, createTrack, createStream, createPeer, acceptPeerAnswer, installHostPeerBrowser, restoreHostPeerBrowser } from "./support.ts";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { QUALITY_PROFILES } from "../../src/client/media/quality.ts";
import { HostPeer, RENEGOTIATION_ANSWER_TIMEOUT_MS } from "../../src/client/webrtc/host-peer";

beforeEach(installHostPeerBrowser);
afterEach(restoreHostPeerBrowser);
describe("HostPeer source replacement", () => {
  it("keeps a pre-negotiated audio sender inactive until a real track exists", async () => {
    const peer = createPeer(
      createStream(createTrack("video", "old-video"), null),
    );

    await expect(peer.start()).resolves.toBe(true);
    await acceptPeerAnswer(peer);
    const connection = FakePeerConnection.latest!;
    expect(connection.transceiverInputs[1]?.trackOrKind).toBe("audio");
    expect(connection.senders[1]?.track).toBeNull();
    expect(connection.transceivers[1]?.direction).toBe("inactive");
    expect(connection.senders[1]?.appliedMaxBitrates).toEqual([]);

    const nextAudio = createTrack("audio", "next-audio");
    await expect(
      peer.replaceStream(
        createStream(createTrack("video", "next-video"), nextAudio),
      ),
    ).resolves.toBe(true);
    expect(connection.senders[1]?.track).toBe(nextAudio);
    expect(connection.transceivers[1]?.direction).toBe("sendonly");
    expect(connection.createOfferCallCount).toBe(2);
    expect(connection.senders[1]?.appliedMaxBitrates).toEqual([128_000]);
  });

  it("retires an absent audio track from the negotiated direction", async () => {
    const peer = createPeer(
      createStream(
        createTrack("video", "old-video"),
        createTrack("audio", "old-audio"),
      ),
    );

    await expect(peer.start()).resolves.toBe(true);
    await acceptPeerAnswer(peer);
    const connection = FakePeerConnection.latest!;
    await expect(
      peer.replaceStream(
        createStream(createTrack("video", "next-video"), null),
      ),
    ).resolves.toBe(true);

    expect(connection.senders[1]?.track).toBeNull();
    expect(connection.transceivers[1]?.direction).toBe("inactive");
    expect(connection.createOfferCallCount).toBe(2);
    expect(connection.senders[1]?.appliedMaxBitrates).toEqual([128_000]);
  });

  it("applies an answer's audio limit on the existing sender without touching video", async () => {
    const peer = createPeer(createStream(createTrack("video", "video"), createTrack("audio", "audio")),
      undefined, undefined, { ...QUALITY_PROFILES["720p30"], maxAudioBitrate: 320_000 });
    await peer.start();
    const connection = FakePeerConnection.latest!;
    const videoCalls = connection.senders[0]!.setParameters.mock.calls.length;
    const audio = connection.senders[1]!;
    audio.getParameters().codecs = [{ mimeType: "audio/opus", clockRate: 48000, payloadType: 111,
      sdpFmtpLine: "stereo=1;maxaveragebitrate=192000" }];
    await peer.acceptSignal({ kind: "description", connectionId: peer.connectionId,
      description: { type: "answer", sdp: "test-answer" } });
    await vi.waitFor(() => expect(audio.getParameters().encodings[0]?.maxBitrate).toBe(192_000));
    expect(connection.senders[0]!.setParameters.mock.calls).toHaveLength(videoCalls);
    expect(connection.createOfferCallCount).toBe(1);
    expect(peer.getSnapshot().audioSenderParameters).toMatchObject({
      requestedMaxBitrate: 320_000, appliedMaxBitrate: 192_000, mismatch: true,
    });
    peer.dispose();
  });

  it("keeps the negotiated edge after an audio ceiling rejection and retries the same explicit profile", async () => {
    const video = createTrack("video", "video"), sourceAudio = createTrack("audio", "audio");
    const profile = { ...QUALITY_PROFILES["720p30"], maxAudioBitrate: 320_000 };
    const peer = createPeer(createStream(video, sourceAudio), undefined, undefined, profile);
    try {
      await peer.start();
      const connection = FakePeerConnection.latest!;
      const videoCalls = connection.senders[0]!.setParameters.mock.calls.length;
      const audio = connection.senders[1]!;
      const previous = peer.getSnapshot().audioSenderParameters;
      audio.getParameters().codecs = [{ mimeType: "audio/opus", clockRate: 48000, payloadType: 111,
        sdpFmtpLine: "stereo=1;maxaveragebitrate=192000" }];
      audio.failNextSetParameters = true;
      await peer.acceptSignal({ kind: "description", connectionId: peer.connectionId,
        description: { type: "answer", sdp: "test-answer" } });
      await vi.waitFor(() => expect(audio.setParameters).toHaveBeenCalledTimes(2));
      expect(connection.connectionState).not.toBe("closed");
      expect(connection.senders[0]!.setParameters).toHaveBeenCalledTimes(videoCalls);
      expect(audio.getParameters().encodings[0]?.maxBitrate).toBe(320_000);
      expect(peer.getSnapshot().audioSenderParameters).toEqual(previous);
      expect(video.stop).not.toHaveBeenCalled();
      expect(sourceAudio.stop).not.toHaveBeenCalled();
      expect(connection.senders[0]!.track!.stop).not.toHaveBeenCalled();

      connection.connectionState = "connected";
      await expect(peer.updateProfile(profile)).resolves.toBe(true);
      expect(audio.getParameters().encodings[0]?.maxBitrate).toBe(192_000);
      expect(peer.getSnapshot().audioSenderParameters).toMatchObject({
        requestedMaxBitrate: 320_000, appliedMaxBitrate: 192_000, mismatch: true,
      });
      expect(connection.connectionState).toBe("connected");
    } finally { peer.dispose(); }
  });

  it("updates quality parameters without replacing media tracks", async () => {
    const video = createTrack("video", "video");
    const audio = createTrack("audio", "audio");
    const peer = createPeer(createStream(video, audio));

    await expect(peer.start()).resolves.toBe(true);
    await acceptPeerAnswer(peer);
    const connection = FakePeerConnection.latest!;
    await expect(
      peer.updateProfile(QUALITY_PROFILES["1080p60"]),
    ).resolves.toBe(true);

    expect(connection.senders[0]?.replaceTrack).not.toHaveBeenCalled();
    expect(connection.senders[1]?.replaceTrack).not.toHaveBeenCalled();
    expect(connection.senders[0]?.setParameters).toHaveBeenCalledTimes(3);
    expect(
      connection.senders[0]?.setParameters.mock.calls.at(-1)?.[0],
    ).toMatchObject({
      degradationPreference: "balanced",
      encodings: [{ maxBitrate: 8_000_000, maxFramerate: 60 }],
    });
    expect(connection.senders[1]?.setParameters).toHaveBeenCalledOnce();

    await expect(
      peer.updateProfile({
        ...QUALITY_PROFILES["1080p60"],
        maxAudioBitrate: 192000,
      }),
    ).resolves.toBe(true);
    expect(connection.senders[0]?.setParameters).toHaveBeenCalledTimes(3);
    expect(connection.senders[1]?.setParameters).toHaveBeenCalledTimes(2);
    expect(connection.senders[1]?.appliedMaxBitrates).toEqual([
      128_000,
      192_000,
    ]);
    expect(peer.getSnapshot().audioSenderParameters).toEqual({
      requestedMaxBitrate: 192_000,
      appliedMaxBitrate: 192_000,
      mismatch: false,
    });
  });

  it("keeps media and prior audio readback when a live ceiling fails", async () => {
    const video = createTrack("video", "video");
    const audio = createTrack("audio", "audio");
    const peer = createPeer(createStream(video, audio));

    await expect(peer.start()).resolves.toBe(true);
    const connection = FakePeerConnection.latest!;
    const audioSender = connection.senders[1]!;
    audioSender.failNextSetParameters = true;

    await expect(
      peer.updateProfile({
        ...QUALITY_PROFILES["720p30"],
        maxAudioBitrate: 192000,
      }),
    ).resolves.toBe(false);

    expect(audioSender.track).toBe(audio);
    expect(audioSender.getParameters().encodings[0]?.maxBitrate).toBe(128_000);
    expect(peer.getSnapshot().audioSenderParameters?.appliedMaxBitrate).toBe(
      128_000,
    );

    await expect(
      peer.updateProfile({
        ...QUALITY_PROFILES["720p30"],
        maxAudioBitrate: 192000,
      }),
    ).resolves.toBe(true);
    expect(audioSender.getParameters().encodings[0]?.maxBitrate).toBe(192_000);
  });

  it("keeps rapid audio ceiling changes last-wins", async () => {
    const peer = createPeer(
      createStream(createTrack("video", "video"), createTrack("audio", "audio")),
    );
    await expect(peer.start()).resolves.toBe(true);
    const audioSender = FakePeerConnection.latest!.senders[1]!;
    audioSender.deferNextSetParameters = true;

    const saver = peer.updateProfile({
      ...QUALITY_PROFILES["720p30"],
      maxAudioBitrate: 64000,
    });
    await vi.waitFor(() =>
      expect(audioSender.setParameters).toHaveBeenCalledTimes(2),
    );
    const veryHigh = peer.updateProfile({
      ...QUALITY_PROFILES["720p30"],
      maxAudioBitrate: 192000,
    });
    audioSender.releaseDeferredSetParameters();

    await expect(saver).resolves.toBe(false);
    await expect(veryHigh).resolves.toBe(true);
    expect(audioSender.appliedMaxBitrates).toEqual([
      128_000,
      64_000,
      192_000,
    ]);
    expect(peer.getSnapshot().audioSenderParameters?.appliedMaxBitrate).toBe(
      192_000,
    );
  });

  it("retires a stalled audio transaction, not the initial candidate", async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    const onUpdate = vi.fn();
    const video = createTrack("video", "video");
    const peer = createPeer(createStream(video, null), onUpdate);
    try {
      await peer.start();
      const connection = FakePeerConnection.latest!;
      await vi.advanceTimersByTimeAsync(RENEGOTIATION_ANSWER_TIMEOUT_MS * 2);
      expect(connection.connectionState).not.toBe("closed");
      await acceptPeerAnswer(peer);
      await peer.replaceStream(createStream(video, createTrack("audio", "microphone")));
      await vi.advanceTimersByTimeAsync(RENEGOTIATION_ANSWER_TIMEOUT_MS - 1);
      expect(peer.isConnected()).toBe(true);
      // Another microphone change coalesces without extending the first wait.
      await peer.replaceStream(createStream(video, null));
      await vi.advanceTimersByTimeAsync(1);
      expect(connection.connectionState).toBe("closed");
      expect(onUpdate.mock.calls.at(-1)?.[0]).toMatchObject({ connectionState: "failed" });
      const updates = onUpdate.mock.calls.length;
      await peer.acceptSignal({ kind: "description", connectionId: peer.connectionId,
        description: { type: "answer", sdp: "late-answer" } });
      expect(onUpdate).toHaveBeenCalledTimes(updates);
    } finally { peer.dispose(); }
  });

  it.each(["answer", "dispose"])("cancels the renegotiation deadline on %s", async (completion) => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    const onUpdate = vi.fn();
    const video = createTrack("video", "video");
    const peer = createPeer(createStream(video, null), onUpdate);
    try {
      await peer.start();
      await acceptPeerAnswer(peer);
      await peer.replaceStream(createStream(video, createTrack("audio", "microphone")));
      if (completion === "answer") await acceptPeerAnswer(peer);
      else peer.dispose();
      const updates = onUpdate.mock.calls.length;
      await vi.advanceTimersByTimeAsync(RENEGOTIATION_ANSWER_TIMEOUT_MS * 2);
      expect(onUpdate).toHaveBeenCalledTimes(updates);
      if (completion === "answer") expect(peer.isConnected()).toBe(true);
    } finally { peer.dispose(); }
  });

  it("releases a hung local-description operation at the renegotiation deadline", async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    const video = createTrack("video", "video");
    const peer = createPeer(createStream(video, null));
    let finish!: () => void;
    try {
      await peer.start();
      await acceptPeerAnswer(peer);
      const connection = FakePeerConnection.latest!;
      const local = vi.spyOn(connection, "setLocalDescription").mockImplementationOnce(() =>
        new Promise<void>((resolve) => { finish = resolve; }));
      const replaced = peer.replaceStream(createStream(video, createTrack("audio", "microphone")));
      await vi.waitFor(() => expect(local).toHaveBeenCalledOnce());
      await vi.advanceTimersByTimeAsync(RENEGOTIATION_ANSWER_TIMEOUT_MS);
      await expect(replaced).resolves.toBe(false);
      expect(connection.connectionState).toBe("closed");
      finish();
      await Promise.resolve();
      expect(connection.createOfferCallCount).toBe(2);
    } finally { peer.dispose(); }
  });

  it("retires a connected sender whose renegotiation answer cannot be applied", async () => {
    const onUpdate = vi.fn();
    const video = createTrack("video", "video");
    const peer = createPeer(createStream(video, null), onUpdate);
    try {
      await peer.start();
      await acceptPeerAnswer(peer);
      await peer.replaceStream(createStream(video, createTrack("audio", "microphone")));
      const connection = FakePeerConnection.latest!;
      vi.spyOn(connection, "setRemoteDescription").mockRejectedValueOnce(new DOMException("Rejected SDP", "OperationError"));
      await peer.acceptSignal({ kind: "description", connectionId: peer.connectionId,
        description: { type: "answer", sdp: "invalid-answer" } });
      expect(connection.connectionState).toBe("closed");
      expect(onUpdate.mock.calls.at(-1)?.[0]).toMatchObject({ connectionState: "failed" });
    } finally { peer.dispose(); }
  });

  it("gives a coalesced transaction its own deadline after the previous answer", async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    const video = createTrack("video", "video");
    const peer = createPeer(createStream(video, null));
    try {
      await peer.start();
      await acceptPeerAnswer(peer);
      await peer.replaceStream(createStream(video, createTrack("audio", "microphone")));
      await peer.replaceStream(createStream(video, null));
      await vi.advanceTimersByTimeAsync(10_000);
      await peer.acceptSignal({ kind: "description", connectionId: peer.connectionId,
        description: { type: "answer", sdp: "audio-answer" } });
      expect(FakePeerConnection.latest!.createOfferCallCount).toBe(3);
      await vi.advanceTimersByTimeAsync(RENEGOTIATION_ANSWER_TIMEOUT_MS - 1);
      expect(peer.isConnected()).toBe(true);
      await vi.advanceTimersByTimeAsync(1);
      expect(FakePeerConnection.latest!.connectionState).toBe("closed");
    } finally { peer.dispose(); }
  });

  it.each([false, true])("defers audio renegotiation until the current answer (restart=%s)", async (restart) => {
    const video = createTrack("video", "shared-video");
    const peer = createPeer(createStream(video, null));
    const connection = FakePeerConnection.latest!;
    const answer = (sdp: string) => peer.acceptSignal({ kind: "description", connectionId: peer.connectionId,
      description: { type: "answer", sdp } });
    try {
      await expect(peer.start()).resolves.toBe(true);
      if (restart) {
        await answer("initial-answer");
        await expect(peer.restartIce()).resolves.toBe(true);
      }
      const offers = connection.createOfferCallCount;
      // Rapid microphone toggles coalesce, rather than superseding the offer.
      for (const enabled of [true, false, true]) {
        await expect(peer.replaceStream(createStream(video,
          enabled ? createTrack("audio", "microphone") : null))).resolves.toBe(true);
      }
      expect(connection.createOfferCallCount).toBe(offers);
      await expect(peer.restartIce()).resolves.toBe(false);
      await answer("previous-answer-audio-inactive");
      expect(connection.createOfferCallCount).toBe(offers + 1);
      expect(connection.transceivers[1]?.direction).toBe("sendonly");
      await answer("latest-answer-audio-recvonly");
      expect(connection.remoteDescription?.sdp).toBe("latest-answer-audio-recvonly");
      expect(connection.signalingState).toBe("stable");
      expect(connection.createOfferCallCount).toBe(offers + 1);
    } finally { peer.dispose(); }
  });

  it.each(["dispose", "offer-failure", "signal-failure"] as const)("retires deferred renegotiation on %s", async (ending) => {
    const video = createTrack("video", "shared-video");
    const sendSignal = vi.fn(() => true);
    const onUpdate = vi.fn();
    const peer = new HostPeer("viewer", { iceServers: [] }, createStream(video, null),
      QUALITY_PROFILES["720p30"], { sendSignal, onUpdate });
    const connection = FakePeerConnection.latest!;
    await peer.start();
    await peer.replaceStream(createStream(video, createTrack("audio", "microphone")));
    if (ending === "dispose") peer.dispose();
    if (ending === "offer-failure") FakePeerConnection.offersFailing = 1;
    if (ending === "signal-failure") sendSignal.mockReturnValue(false);
    await peer.acceptSignal({ kind: "description", connectionId: peer.connectionId,
      description: { type: "answer", sdp: "previous-answer" } });
    const failures = onUpdate.mock.calls.filter(([snapshot]) => snapshot.connectionState === "failed");
    expect(failures).toHaveLength(ending === "dispose" ? 0 : 1);
    expect(connection.connectionState).toBe("closed");
    expect(connection.createOfferCallCount).toBe(ending === "dispose" ? 1 : 2);
    peer.dispose();
  });
});
