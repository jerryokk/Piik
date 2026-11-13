import { statsCallbacks, FakePeerConnection, sendStatsReport, createTrack, createStream, completeVideoStartup, installHostPeerBrowser, restoreHostPeerBrowser } from "./support.ts";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { QUALITY_PROFILES } from "../../src/client/media/quality.ts";
import { ViewerRelay } from "../../src/client/webrtc/viewer-relay.ts";

beforeEach(installHostPeerBrowser);
afterEach(restoreHostPeerBrowser);
  const routeCandidate = (
    revision: number,
    childPeerId: string,
  ) => ({
    childPeerId,
    connectionId: `relay-candidate-${revision}`,
    transport: "direct" as const,
    qualityProbe: false,
  });

describe("ViewerRelay downstream ownership", () => {
  it("reports relay transport progress once and retires its reporting scope", async () => {
    const progress = vi.fn().mockReturnValueOnce(false).mockReturnValue(true);
    const relay = new ViewerRelay(
      { iceServers: [] }, QUALITY_PROFILES["720p30"],
      { sendSignal: () => true, onPreparedChildConnected: progress },
    );
    relay.setStream(createStream(createTrack("video", "relay-progress"), null));
    relay.prepareChild(7, routeCandidate(7, "child"), ["child"]);
    await vi.waitFor(() => expect(FakePeerConnection.latest).not.toBeNull());
    const first = FakePeerConnection.latest!;
    first.connectionState = "connected";
    first.dispatchEvent(new Event("connectionstatechange"));
    first.dispatchEvent(new Event("connectionstatechange"));
    first.dispatchEvent(new Event("connectionstatechange"));
    expect(progress).toHaveBeenCalledTimes(2);
    expect(progress).toHaveBeenLastCalledWith(7, "relay-candidate-7");
    relay.prepareChild(8, routeCandidate(8, "replacement"), ["replacement"]);
    first.dispatchEvent(new Event("connectionstatechange"));
    expect(progress).toHaveBeenCalledTimes(2);
    const second = FakePeerConnection.latest!;
    second.connectionState = "connected";
    second.dispatchEvent(new Event("connectionstatechange"));
    expect(progress).toHaveBeenLastCalledWith(8, "relay-candidate-8");
    relay.dispose();
    second.dispatchEvent(new Event("connectionstatechange"));
    expect(progress).toHaveBeenCalledTimes(3);
  });

  it.each([false, true])("bounds ordinary relay construction retries; retired=%s", async (retired) => {
    vi.useFakeTimers();
    const relay = new ViewerRelay({ iceServers: [] }, QUALITY_PROFILES["720p30"], { sendSignal: () => true });
    const source = createTrack("video", "relay-retry");
    try {
      relay.setStream(createStream(source, null));
      await vi.advanceTimersByTimeAsync(0);
      FakePeerConnection.constructorsFailing = 3;
      relay.activateChildren(1, ["child"]);
      expect(source.clone).toHaveBeenCalledTimes(1);
      if (retired) relay.dispose();
      await vi.advanceTimersByTimeAsync(2000);
      expect(source.clone).toHaveBeenCalledTimes(retired ? 1 : 2);
      for (const result of vi.mocked(source.clone).mock.results) {
        expect((result.value as MediaStreamTrack).stop).toHaveBeenCalledOnce();
      }
      expect(source.stop).not.toHaveBeenCalled();
      expect(FakePeerConnection.activeCount).toBe(0);
    } finally {
      relay.dispose();
      vi.useRealTimers();
    }
  });

  it("reports deferred prepared construction failure and accepts the next candidate", async () => {
    const onPreparedChildFailed = vi.fn();
    const sendSignal = vi.fn(() => true);
    const relay = new ViewerRelay({ iceServers: [] }, QUALITY_PROFILES["720p30"],
      { sendSignal, onPreparedChildFailed });
    const source = createTrack("video", "relay-construction");
    relay.setStream(createStream(source, null));
    FakePeerConnection.constructorsFailing = 1;
    expect(relay.prepareChild(1, routeCandidate(1, "child"), ["child"])).toBe(true);
    await vi.waitFor(() => expect(onPreparedChildFailed).toHaveBeenCalledExactlyOnceWith(1, "relay-candidate-1"));
    expect(sendSignal).not.toHaveBeenCalled();
    expect(vi.mocked(source.clone).mock.results[0]!.value.stop).toHaveBeenCalledOnce();
    expect(source.stop).not.toHaveBeenCalled();
    expect(relay.prepareChild(2, routeCandidate(2, "child"), ["child"])).toBe(true);
    await vi.waitFor(() => expect(sendSignal).toHaveBeenCalledOnce());
    relay.dispose();
    expect(FakePeerConnection.activeCount).toBe(0);
  });

  it("reports a failed prepared transport once without blaming retained children", async () => {
    const onPreparedChildFailed = vi.fn();
    const relay = new ViewerRelay(
      { iceServers: [] },
      QUALITY_PROFILES["720p30"],
      { sendSignal: () => true, onPreparedChildFailed },
    );
    relay.setStream(createStream(createTrack("video", "failure-video"), null));
    relay.prepareChild(1, routeCandidate(1, "retained"), ["retained"]);
    await vi.waitFor(() => expect(FakePeerConnection.latest).not.toBeNull());
    const retained = FakePeerConnection.latest!;
    retained.connectionState = "connected";
    relay.activateChildren(1, ["retained"]);
    relay.prepareChild(2, routeCandidate(2, "candidate"), ["retained", "candidate"]);
    const candidate = FakePeerConnection.latest!;
    candidate.connectionState = "failed";
    candidate.dispatchEvent(new Event("connectionstatechange"));
    candidate.dispatchEvent(new Event("connectionstatechange"));
    expect(onPreparedChildFailed).toHaveBeenCalledExactlyOnceWith(2, "relay-candidate-2");
    expect(retained.connectionState).toBe("connected");
    relay.activateChildren(3, ["retained"]);
    candidate.dispatchEvent(new Event("connectionstatechange"));
    expect(onPreparedChildFailed).toHaveBeenCalledTimes(1);
    relay.dispose();
  });

  it("keeps the prepared connection when its stream replacement succeeds", async () => {
    const signals: Array<{ peerId: string; connectionId: string }> = [];
    const relay = new ViewerRelay(
      { iceServers: [] },
      QUALITY_PROFILES["720p30"],
      {
        sendSignal: (peerId, payload) => {
          signals.push({ peerId, connectionId: payload.connectionId });
          return true;
        },
      },
    );
    relay.setStream(createStream(createTrack("video", "initial-video"), null));
    expect(relay.prepareChild(7, routeCandidate(7, "prepared-child"), ["prepared-child"])).toBe(true);
    await vi.waitFor(() => expect(signals).toHaveLength(1));
    const preparedConnection = FakePeerConnection.latest!;
    const preparedConnectionId = signals[0]!.connectionId;
    const instanceCount = FakePeerConnection.instances.length;

    const replacementVideo = createTrack("video", "replacement-video");
    relay.setStream(createStream(replacementVideo, null));
    await vi.waitFor(() =>
      expect(preparedConnection.senders[0]!.track?.id).toBe(replacementVideo.id),
    );
    expect(FakePeerConnection.instances).toHaveLength(instanceCount);
    preparedConnection.connectionState = "connected";
    relay.activateChildren(7, ["prepared-child"]);
    expect(relay.getSnapshot("prepared-child")?.connectionId).toBe(
      preparedConnectionId,
    );
    expect(FakePeerConnection.instances).toHaveLength(instanceCount);
    relay.dispose();
  });

  it("keeps a prepared child when a superseded stream replacement fails", async () => {
    const onPreparedChildFailed = vi.fn();
    const signals: Array<{ connectionId: string }> = [];
    const relay = new ViewerRelay(
      { iceServers: [] },
      QUALITY_PROFILES["720p30"],
      { sendSignal: (_peerId, payload) => { signals.push(payload); return true; }, onPreparedChildFailed },
    );
    let finish!: () => void;
    const paused = new Promise<void>(resolve => { finish = resolve; });
    try {
      relay.setStream(createStream(createTrack("video", "initial-video"), null));
      expect(relay.prepareChild(7, routeCandidate(7, "prepared-child"), ["prepared-child"])).toBe(true);
      await vi.waitFor(() => expect(signals).toHaveLength(1));
      const prepared = FakePeerConnection.latest!;
      const sender = prepared.senders[0]!;
      const replace = vi.spyOn(sender, "replaceTrack").mockImplementationOnce(async () => {
        await paused;
        throw new Error("superseded stream was rejected");
      });
      relay.setStream(createStream(createTrack("video", "superseded-video"), null));
      await vi.waitFor(() => expect(replace).toHaveBeenCalledOnce());
      const finalVideo = createTrack("video", "current-video");
      relay.setStream(createStream(finalVideo, null));
      finish();
      await vi.waitFor(() => expect(sender.track?.id).toBe(finalVideo.id));
      expect(onPreparedChildFailed).not.toHaveBeenCalled();
      expect(prepared.connectionState).not.toBe("closed");
      expect(FakePeerConnection.instances).toHaveLength(1);
      prepared.connectionState = "connected";
      relay.activateChildren(7, ["prepared-child"]);
      expect(relay.getSnapshot("prepared-child")?.connectionId).toBe(signals[0]!.connectionId);
    } finally { finish(); relay.dispose(); }
  });

  it("syncs same-stream audio changes into the prepared connection", async () => {
    const relay = new ViewerRelay(
      { iceServers: [] },
      QUALITY_PROFILES["720p30"],
      { sendSignal: () => true },
    );
    const video = createTrack("video", "persistent-video");
    let audio: MediaStreamTrack | null = null;
    const persistentStream = {
      getTracks: () => (audio ? [video, audio] : [video]),
      getVideoTracks: () => [video],
      getAudioTracks: () => (audio ? [audio] : []),
    } as unknown as MediaStream;

    relay.setStream(persistentStream);
    expect(
      relay.prepareChild(
        7,
        routeCandidate(7, "prepared-child"),
        ["prepared-child"],
      ),
    ).toBe(true);
    await vi.waitFor(() => expect(FakePeerConnection.latest).not.toBeNull());
    const preparedConnection = FakePeerConnection.latest!;
    expect(preparedConnection.senders[1]?.track).toBeNull();

    audio = createTrack("audio", "late-audio");
    relay.setStream(persistentStream);
    await vi.waitFor(() =>
      expect(preparedConnection.senders[1]?.track).toBe(audio),
    );

    audio = null;
    relay.setStream(persistentStream);
    await vi.waitFor(() =>
      expect(preparedConnection.senders[1]?.track).toBeNull(),
    );
    expect(FakePeerConnection.latest).toBe(preparedConnection);
    relay.dispose();
  });

  it("admits provisional children until endpoint cap three is full", async () => {
    const relay = new ViewerRelay(
      { iceServers: [] },
      QUALITY_PROFILES["720p30"],
      { sendSignal: () => true },
    );
    relay.setStream(createStream(createTrack("video", "cap-video"), null));
    expect(relay.prepareChild(1, routeCandidate(1, "child-0"), ["child-0"])).toBe(true);
    await vi.waitFor(() => expect(FakePeerConnection.latest).not.toBeNull());
    const preparedConnection = FakePeerConnection.latest!;
    preparedConnection.connectionState = "connected";
    relay.activateChildren(1, ["child-0"]);
    expect(FakePeerConnection.activeCount).toBe(1);
    expect(relay.prepareChild(2, routeCandidate(2, "child-1"), ["child-0", "child-1"])).toBe(true);
    FakePeerConnection.latest!.connectionState = "connected";
    relay.activateChildren(2, ["child-0", "child-1"]);
    expect(relay.prepareChild(3, routeCandidate(3, "child-2"), ["child-0", "child-1", "child-2"])).toBe(
      true,
    );
    FakePeerConnection.latest!.connectionState = "connected";
    relay.activateChildren(3, ["child-0", "child-1", "child-2"]);
    expect(
      relay.prepareChild(4, routeCandidate(4, "child-3"), ["child-0", "child-1", "child-2", "child-3"]),
    ).toBe(false);
    expect(FakePeerConnection.activeCount).toBe(3);
    relay.dispose();
  });

  it("reconciles a reauthenticated endpoint cap without rebuilding retained children", async () => {
    const relay = new ViewerRelay(
      { iceServers: [] },
      QUALITY_PROFILES["720p30"],
      { sendSignal: () => true },
      3,
    );
    relay.setStream(createStream(createTrack("video", "cap-update-video"), null));
    relay.setChildren(["child-0", "child-1", "child-2"]);
    await vi.waitFor(() => expect(FakePeerConnection.instances).toHaveLength(3));
    const retained = FakePeerConnection.instances.slice(0, 2);
    const removed = FakePeerConnection.instances[2]!;
    retained.forEach((connection) => {
      connection.connectionState = "connected";
    });

    relay.updateCapacity(2);

    expect(FakePeerConnection.instances.slice(0, 2)).toEqual(retained);
    expect(removed.connectionState).toBe("closed");
    expect(FakePeerConnection.activeCount).toBe(2);
    relay.dispose();
  });

  it("exposes a defensive snapshot of current downstream send metrics", async () => {
    const relay = new ViewerRelay(
      { iceServers: [] },
      QUALITY_PROFILES["720p30"],
      { sendSignal: () => true },
    );
    relay.setChildren(["metrics-child"]);
    relay.setStream(
      createStream(createTrack("video", "metrics-video"), null),
    );

    await vi.waitFor(() =>
      expect(relay.getSnapshot()?.peerId).toBe("metrics-child"),
    );
    const connection = FakePeerConnection.latest!;
    connection.statsReports.push(
      sendStatsReport({
        bytesSent: 1_000_000,
        framesEncoded: 30,
        timestamp: 1_000,
        totalEncodeTime: 0.15,
        qualityLimitationReason: "cpu",
        trackIdentifier: "metrics-video",
      }),
      sendStatsReport({
        bytesSent: 1_500_000,
        framesEncoded: 60,
        timestamp: 2_000,
        totalEncodeTime: 0.75,
        qualityLimitationReason: "bandwidth",
        trackIdentifier: "metrics-video",
      }),
    );

    statsCallbacks[0]!();
    await vi.waitFor(() =>
      expect(relay.getSnapshot()?.metrics.qualityLimitationReason).toBe("cpu"),
    );
    expect(relay.getSnapshot()?.metrics.intervalEncodeMs).toBeNull();
    statsCallbacks[0]!();
    await vi.waitFor(() =>
      expect(relay.getSnapshot()?.metrics.intervalEncodeMs).toBe(20),
    );

    const snapshot = relay.getSnapshot()!;
    expect(snapshot.metrics).toMatchObject({
      intervalEncodeMs: 20,
      bitrateKbps: 4_000,
      encoderImplementation: "test-encoder",
      qualityLimitationReason: "bandwidth",
    });
    snapshot.metrics.qualityLimitationReason = "mutated";
    expect(relay.getSnapshot()?.metrics.qualityLimitationReason).toBe(
      "bandwidth",
    );
  });

  it("clears snapshots across child replacement, stop, and dispose", async () => {
    const relay = new ViewerRelay(
      { iceServers: [] },
      QUALITY_PROFILES["720p30"],
      { sendSignal: () => true },
    );
    const stream = createStream(createTrack("video", "relay-video"), null);
    relay.setChildren(["first-child"]);
    relay.setStream(stream);
    await vi.waitFor(() =>
      expect(relay.getSnapshot()?.peerId).toBe("first-child"),
    );

    relay.setChildren(["second-child"]);
    expect(relay.getSnapshot()).toBeNull();
    await vi.waitFor(() =>
      expect(relay.getSnapshot()?.peerId).toBe("second-child"),
    );

    relay.stop();
    expect(relay.getSnapshot()).toBeNull();
    relay.setStream(stream);
    await vi.waitFor(() =>
      expect(relay.getSnapshot()?.peerId).toBe("second-child"),
    );

    relay.dispose();
    expect(relay.getSnapshot()).toBeNull();
    relay.setStream(stream);
    expect(relay.getSnapshot()).toBeNull();
  });

  it("does not publish a stale stats result after replacing its child", async () => {
    let resolveOldStats!: (report: RTCStatsReport) => void;
    const pendingOldStats = new Promise<RTCStatsReport>((resolve) => {
      resolveOldStats = resolve;
    });
    const relay = new ViewerRelay(
      { iceServers: [] },
      QUALITY_PROFILES["720p30"],
      { sendSignal: () => true },
    );
    relay.setChildren(["old-child"]);
    relay.setStream(
      createStream(createTrack("video", "relay-video"), null),
    );
    await vi.waitFor(() =>
      expect(relay.getSnapshot()?.peerId).toBe("old-child"),
    );
    FakePeerConnection.latest!.statsReports.push(pendingOldStats);
    statsCallbacks[0]!();

    relay.setChildren(["current-child"]);
    await vi.waitFor(() =>
      expect(relay.getSnapshot()?.peerId).toBe("current-child"),
    );
    resolveOldStats(
      sendStatsReport({
        bytesSent: 999_999,
        framesEncoded: 30,
        timestamp: 1_000,
        qualityLimitationReason: "old-peer",
      }),
    );
    await Promise.resolve();
    await Promise.resolve();

    expect(relay.getSnapshot()).toMatchObject({
      peerId: "current-child",
      metrics: { qualityLimitationReason: null },
    });
  });

  it("creates a downstream peer without secure-context randomUUID", async () => {
    vi.stubGlobal("crypto", {
      getRandomValues: (bytes: Uint8Array) => {
        bytes.fill(7);
        return bytes;
      },
    });
    const targets: string[] = [];
    const relay = new ViewerRelay(
      { iceServers: [] },
      QUALITY_PROFILES["720p30"],
      {
        sendSignal: (peerId) => {
          targets.push(peerId);
          return true;
        },
      },
    );

    relay.setChildren(["lan-child-peer"]);
    relay.setStream(
      createStream(createTrack("video", "lan-video"), null),
    );

    await vi.waitFor(() => expect(targets).toEqual(["lan-child-peer"]));
    expect(FakePeerConnection.latest?.connectionState).toBe("new");
  });

  it("retries a missing peer when the same assignment is reconciled", async () => {
    vi.useFakeTimers();
    try {
      FakePeerConnection.offersFailing = 2;
      const targets: string[] = [];
      const relay = new ViewerRelay(
        { iceServers: [] },
        QUALITY_PROFILES["720p30"],
        {
          sendSignal: (peerId) => {
            targets.push(peerId);
            return true;
          },
        },
      );
      const stream = createStream(createTrack("video", "relay-video"), null);
      relay.setChildren(["same-child-peer"]);
      relay.setStream(stream);

      await vi.runAllTimersAsync();
      const failedPeer = FakePeerConnection.latest;
      expect(targets).toEqual([]);

      relay.setChildren(["same-child-peer"]);
      await vi.runAllTimersAsync();

      expect(FakePeerConnection.latest).not.toBe(failedPeer);
      expect(targets).toEqual(["same-child-peer"]);
    } finally {
      vi.useRealTimers();
    }
  });

  it("preserves a connecting replacement when topology reaffirms the same child", async () => {
    const connectionIds: string[] = [];
    const relay = new ViewerRelay(
      { iceServers: [] },
      QUALITY_PROFILES["720p30"],
      {
        sendSignal: (_peerId, payload) => {
          connectionIds.push(payload.connectionId);
          return true;
        },
      },
    );
    relay.setChildren(["reconciled-child"]);
    relay.setStream(
      createStream(createTrack("video", "reconciled-video"), null),
    );
    try {
      await vi.waitFor(() => expect(connectionIds).toHaveLength(1));
      const original = FakePeerConnection.latest!;
      original.connectionState = "connected";

      await relay.recover("reconciled-child", connectionIds[0]!, true);
      await vi.waitFor(() => expect(connectionIds).toHaveLength(2));
      const replacement = FakePeerConnection.latest!;
      expect(replacement).not.toBe(original);
      expect(original.connectionState).toBe("closed");
      replacement.connectionState = "connecting";

      relay.activateChildren(2, ["reconciled-child"]);
      relay.setChildren(["reconciled-child"]);
      relay.updateCapacity(1);

      await expect(relay.acceptSignal("reconciled-child", {
        kind: "description",
        connectionId: connectionIds[1]!,
        description: { type: "answer", sdp: "replacement-answer" },
      }, 2)).resolves.toBe(true);
      expect(FakePeerConnection.latest).toBe(replacement);
      expect(replacement.connectionState).toBe("connecting");
      expect(replacement.remoteDescription?.sdp).toBe("replacement-answer");
      expect(connectionIds).toHaveLength(2);
    } finally {
      relay.dispose();
    }
  });

  it("rebuilds only unfinished negotiations after signaling resynchronizes", async () => {
    const signals: Array<{ peerId: string; connectionId: string }> = [];
    const relay = new ViewerRelay(
      { iceServers: [] },
      QUALITY_PROFILES["720p30"],
      {
        sendSignal: (peerId, payload) => {
          signals.push({ peerId, connectionId: payload.connectionId });
          return true;
        },
      },
    );
    const children = ["healthy-child", "unfinished-child"];
    relay.setChildren(children);
    relay.setStream(createStream(createTrack("video", "resync-video"), null));
    try {
      await vi.waitFor(() => expect(signals).toHaveLength(2));
      const [healthy, unfinished] = FakePeerConnection.instances;
      healthy!.connectionState = "connected";
      unfinished!.connectionState = "connecting";
      const oldId = signals.find(({ peerId }) => peerId === "unfinished-child")!.connectionId;

      relay.resyncSignaling();
      relay.activateChildren(2, children);
      await vi.waitFor(() => expect(signals).toHaveLength(3));
      expect(healthy!.connectionState).toBe("connected");
      expect(unfinished!.connectionState).toBe("closed");
      expect(signals.filter(({ peerId }) => peerId === "healthy-child")).toHaveLength(1);
      const replacement = FakePeerConnection.latest!;
      const newId = signals[2]!.connectionId;
      expect(newId).not.toBe(oldId);
      await expect(relay.acceptSignal("unfinished-child", {
        kind: "description",
        connectionId: oldId,
        description: { type: "answer", sdp: "stale-answer" },
      }, 2)).resolves.toBe(false);
      await expect(relay.acceptSignal("unfinished-child", {
        kind: "description",
        connectionId: newId,
        description: { type: "answer", sdp: "resynced-answer" },
      }, 2)).resolves.toBe(true);
      expect(replacement.remoteDescription?.sdp).toBe("resynced-answer");
    } finally {
      relay.dispose();
    }
  });

  it("keeps one downstream connection across upstream stream replacement", async () => {
    const targets: string[] = [];
    const relay = new ViewerRelay(
      { iceServers: [] },
      QUALITY_PROFILES["1080p60"],
      {
        sendSignal: (peerId) => {
          targets.push(peerId);
          return true;
        },
      },
    );
    const firstStream = createStream(
      createTrack("video", "first-video"),
      createTrack("audio", "first-audio"),
    );

    relay.setChildren(["child-peer-one"]);
    relay.setStream(firstStream);
    await vi.waitFor(() => expect(FakePeerConnection.latest).not.toBeNull());
    const connection = FakePeerConnection.latest!;
    await vi.waitFor(() => expect(targets).toEqual(["child-peer-one"]));

    const nextVideo = createTrack("video", "next-video");
    const nextAudio = createTrack("audio", "next-audio");
    relay.setStream(createStream(nextVideo, nextAudio));
    await vi.waitFor(() =>
      expect(connection.senders[0]?.track?.id).toBe(nextVideo.id),
    );
    expect(connection.senders[0]?.track).not.toBe(nextVideo);

    expect(FakePeerConnection.latest).toBe(connection);
    expect(connection.senders[1]?.track).toBe(nextAudio);
    expect(connection.transceiverInputs).toHaveLength(2);

    relay.setChildren(["child-peer-two"]);
    await vi.waitFor(() => expect(FakePeerConnection.latest).not.toBe(connection));
    expect(connection.connectionState).toBe("closed");
    await vi.waitFor(() => expect(targets.at(-1)).toBe("child-peer-two"));
  });

  it("applies the latest profile to the current and future child", async () => {
    const customSettings = {
      resolution: { width: 2560, height: 1440 },
      maxFramerate: 45,
      maxBitrate: 10_500_000,
      degradationPreference: "balanced",
    } as const;
    const relay = new ViewerRelay(
      { iceServers: [] },
      QUALITY_PROFILES["1080p60"],
      { sendSignal: () => true },
    );
    relay.setChildren(["first-profile-child"]);
    relay.setStream(createStream(createTrack("video", "profile-video"), null));
    await vi.waitFor(() =>
      expect(relay.getSnapshot("first-profile-child")).not.toBeNull(),
    );
    const firstConnectionId = relay.getSnapshot("first-profile-child")!.connectionId;
    const firstConnection = FakePeerConnection.latest!;
    await expect(
      relay.acceptSignal(
        "first-profile-child",
        {
          kind: "description",
          connectionId: firstConnectionId,
          description: { type: "answer", sdp: "first-answer" },
        },
        0,
      ),
    ).resolves.toBe(true);
    firstConnection.connectionState = "connected";
    firstConnection.dispatchEvent(new Event("connectionstatechange"));
    await completeVideoStartup(firstConnection);

    await expect(
      relay.updateProfile(customSettings),
    ).resolves.toBe(true);
    expect(
      firstConnection.senders[0]?.setParameters.mock.calls.at(-1)?.[0],
    ).toMatchObject({
      degradationPreference: "balanced",
      encodings: [{ maxBitrate: 10_500_000, maxFramerate: 45 }],
    });

    relay.setChildren(["second-profile-child"]);
    await vi.waitFor(() =>
      expect(FakePeerConnection.latest).not.toBe(firstConnection),
    );
    const secondConnection = FakePeerConnection.latest!;
    await vi.waitFor(() =>
      expect(secondConnection.localDescription?.type).toBe("offer"),
    );
    await vi.waitFor(() =>
      expect(relay.getSnapshot("second-profile-child")).not.toBeNull(),
    );
    const secondConnectionId = relay.getSnapshot("second-profile-child")!.connectionId;
    await expect(
      relay.acceptSignal(
        "second-profile-child",
        {
          kind: "description",
          connectionId: secondConnectionId,
          description: { type: "answer", sdp: "second-answer" },
        },
        0,
      ),
    ).resolves.toBe(true);
    secondConnection.connectionState = "connected";
    secondConnection.dispatchEvent(new Event("connectionstatechange"));
    await completeVideoStartup(secondConnection);
    expect(
      secondConnection.senders[0]?.setParameters.mock.calls[0]?.[0],
    ).toMatchObject({
      degradationPreference: "balanced",
      encodings: [{ maxBitrate: 10_500_000, maxFramerate: 45 }],
    });
  });

  it("applies the latest audio ceiling to current and future children", async () => {
    const initialProfile = {
      ...QUALITY_PROFILES["1080p60"],
      maxAudioBitrate: 64000,
    } as const;
    const relay = new ViewerRelay(
      { iceServers: [] },
      initialProfile,
      { sendSignal: () => true },
    );
    relay.setChildren(["first-audio-child"]);
    relay.setStream(
      createStream(
        createTrack("video", "relay-video"),
        createTrack("audio", "relay-audio"),
      ),
    );
    await vi.waitFor(() =>
      expect(FakePeerConnection.latest?.senders[1]?.appliedMaxBitrates).toEqual([
        64_000,
      ]),
    );
    const firstConnection = FakePeerConnection.latest!;

    await expect(
      relay.updateProfile({
        ...initialProfile,
        maxAudioBitrate: 192000,
      }),
    ).resolves.toBe(true);
    expect(firstConnection.senders[1]?.appliedMaxBitrates).toEqual([
      64_000,
      192_000,
    ]);

    relay.setChildren(["second-audio-child"]);
    await vi.waitFor(() =>
      expect(FakePeerConnection.latest).not.toBe(firstConnection),
    );
    await vi.waitFor(() =>
      expect(FakePeerConnection.latest?.senders[1]?.appliedMaxBitrates).toEqual([
        192_000,
      ]),
    );
  });
});
