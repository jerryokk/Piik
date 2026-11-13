import { FakePeerConnection, createTrack, createStream, installHostPeerBrowser, restoreHostPeerBrowser, codecPreflight } from "./support.ts";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { QUALITY_PROFILES } from "../../src/client/media/quality.ts";
import { ViewerSfuRoute } from "../../src/client/media/viewer-sfu-route";
import { EMPTY_METRICS, type PeerSnapshot } from "../../src/client/types.ts";
import { HostPeer, type HostMediaPeer } from "../../src/client/webrtc/host-peer";
import { ViewerRelay, type ViewerRelayPeerEvents, type ViewerRelayPeerFactory } from "../../src/client/webrtc/viewer-relay.ts";
import { type ParticipantRouteAssignment, type SignalPayload } from "../../src/shared/protocol.ts";

beforeEach(installHostPeerBrowser);
afterEach(restoreHostPeerBrowser);
describe("ViewerRelay downstream ownership", () => {
  it("applies the room NAT policy to downstream Peer connections", async () => {
    const relay = new ViewerRelay(
      {
        iceServers: [{ urls: "stun:share.example.test:3478" }],
        natPredictionStunUrls: [
          "stun:share.example.test:3479",
          "stun:share.example.test:3480",
        ],
      },
      QUALITY_PROFILES["720p30"],
      { sendSignal: () => true },
      2,
      true,
    );

    relay.setChildren(["nat-child"]);
    relay.setStream(
      createStream(createTrack("video", "nat-relay-source"), null),
    );
    await vi.waitFor(() => expect(FakePeerConnection.latest).not.toBeNull());
    expect(FakePeerConnection.latest!.configurations[0]?.iceServers).toEqual([
      { urls: "stun:share.example.test:3478" },
      { urls: "stun:share.example.test:3479" },
      { urls: "stun:share.example.test:3480" },
    ]);
    relay.dispose();
  });

  it("gives each downstream sender its own video clone", async () => {
    const sourceVideo = createTrack("video", "relay-source");
    const relay = new ViewerRelay(
      { iceServers: [] },
      QUALITY_PROFILES["720p30"],
      { sendSignal: () => true },
    );

    relay.setChildren(["child-one", "child-two"]);
    relay.setStream(createStream(sourceVideo, null));
    await vi.waitFor(() =>
      expect(FakePeerConnection.instances).toHaveLength(2),
    );
    const [firstTrack, secondTrack] = FakePeerConnection.instances.map(
      (connection) => connection.senders[0]!.track!,
    );
    expect(firstTrack).not.toBe(sourceVideo);
    expect(secondTrack).not.toBe(sourceVideo);
    expect(firstTrack).not.toBe(secondTrack);
    expect(firstTrack.contentHint).toBe("motion");
    expect(secondTrack.contentHint).toBe("motion");

    relay.stop();
    expect(firstTrack.stop).toHaveBeenCalledOnce();
    expect(secondTrack.stop).toHaveBeenCalledOnce();
    expect(sourceVideo.stop).not.toHaveBeenCalled();
  });

  const routeCandidate = (
    revision: number,
    childPeerId: string,
  ) => ({
    childPeerId,
    connectionId: `relay-candidate-${revision}`,
    transport: "direct" as const,
    qualityProbe: false,
  });

  it("admits a relay replacement after retirement while upstream activation is awaiting SDP", async () => {
    const relay = new ViewerRelay({ iceServers: [] }, QUALITY_PROFILES["720p30"], {
      sendSignal: () => true,
    });
    relay.setStream(createStream(createTrack("video", "standing-source"), null));
    let finishActivation!: () => void;
    const upstreamSdp = new Promise<void>((resolve) => { finishActivation = resolve; });
    let waitForSdp = false;
    const activating = vi.fn();
    const admissions: boolean[] = [];
    const children = vi.fn((ids: readonly string[], revision: number) => relay.activateChildren(revision, ids));
    const route = new ViewerSfuRoute("parent-viewer", {
      activatePeer: () => {
        if (waitForSdp) { activating(); return upstreamSdp; }
      },
      activateChildren: children,
      prepareChild: (candidate, planned, revision) => {
        if (candidate && planned && revision !== undefined) admissions.push(relay.prepareChild(revision, candidate, planned));
        else relay.discardPreparedChild();
      },
      onSfuStream: () => undefined,
      send: () => true,
    });
    const assignment = (childPeerIds: string[]): ParticipantRouteAssignment => ({
      upstream: { kind: "peer", peerId: "upstream" }, childPeerIds, sfuPublicationGeneration: null,
    });
    try {
      route.accept({ revision: 1, phase: "active", assignment: assignment(["retired-child"]) });
      await vi.waitFor(() => expect(children).toHaveBeenCalledWith(["retired-child"], 1));
      waitForSdp = true;
      route.accept({ revision: 2, phase: "active", assignment: assignment([]) });
      await vi.waitFor(() => expect(activating).toHaveBeenCalledOnce());
      route.accept({ revision: 3, phase: "prepare", assignment: assignment(["replacement-child"]),
        candidate: routeCandidate(3, "replacement-child") });
      expect(admissions).toEqual([true]);
      finishActivation();
      await upstreamSdp;
      await vi.waitFor(() => expect(relay.getSignalRouteRevision(
        "replacement-child", routeCandidate(3, "replacement-child").connectionId, 2,
      )).toBe(3));
    } finally {
      finishActivation();
      await route.disconnect();
      relay.dispose();
    }
  });

  it("prepares against the current source and codec after an in-flight probe is replaced", async () => {
    let finishOld!: (codec: "vp8") => void;
    let finishCurrent!: (codec: "h264") => void;
    codecPreflight.probe
      .mockImplementationOnce(() => new Promise((resolve) => { finishOld = resolve; }))
      .mockImplementationOnce(() => new Promise((resolve) => { finishCurrent = resolve; }));
    const signals: SignalPayload[] = [];
    const relay = new ViewerRelay({ iceServers: [] }, QUALITY_PROFILES["720p30"], {
      sendSignal: (_peerId, payload) => { signals.push(payload); return true; },
    });
    const current = createTrack("video", "current-source");
    try {
      relay.setStream(createStream(createTrack("video", "old-source"), null));
      expect(relay.prepareChild(7, routeCandidate(7, "child"), ["child"])).toBe(true);
      relay.setStream(createStream(current, null));
      finishOld("vp8");
      await vi.waitFor(() => expect(codecPreflight.probe).toHaveBeenCalledTimes(2));
      expect(FakePeerConnection.instances).toHaveLength(0);
      finishCurrent("h264");
      await vi.waitFor(() => expect(signals).toHaveLength(1));
      expect(signals[0]?.connectionId).toBe("relay-candidate-7");
      const connection = FakePeerConnection.latest!;
      expect(connection.senders[0]!.track!.id).toContain("current-source");
      expect(connection.codecPreferenceCalls[0]?.[0]?.mimeType.toLowerCase()).toBe("video/h264");
      expect(relay.prepareChild(7, routeCandidate(7, "child"), ["child"])).toBe(true);
      expect(FakePeerConnection.instances).toHaveLength(1);
    } finally {
      relay.dispose();
    }
  });

  it.each(["discard", "stop", "dispose"] as const)("does not resurrect preparation after %s during a codec probe", async (action) => {
    let finishProbe!: (codec: "vp8") => void;
    codecPreflight.probe.mockImplementationOnce(() => new Promise((resolve) => { finishProbe = resolve; }));
    const sendSignal = vi.fn(() => true);
    const relay = new ViewerRelay({ iceServers: [] }, QUALITY_PROFILES["720p30"], { sendSignal });
    relay.setStream(createStream(createTrack("video", "pending-source"), null));
    expect(relay.prepareChild(7, routeCandidate(7, "child"), ["child"])).toBe(true);
    if (action === "discard") relay.discardPreparedChild();
    else relay[action]();
    finishProbe("vp8");
    await new Promise<void>((resolve) => setImmediate(resolve));
    expect(FakePeerConnection.instances).toHaveLength(0);
    expect(sendSignal).not.toHaveBeenCalled();
    relay.dispose();
  });

  it("rebuilds a failed ICE restart from the source selected while it was pending", async () => {
    let finishRestart!: (result: boolean) => void;
    vi.spyOn(HostPeer.prototype, "restartIce").mockImplementationOnce(() =>
      new Promise((resolve) => { finishRestart = resolve; }));
    const signals: SignalPayload[] = [];
    const relay = new ViewerRelay({ iceServers: [] }, QUALITY_PROFILES["720p30"], {
      sendSignal: (_peerId, payload) => { signals.push(payload); return true; },
    });
    try {
      relay.setChildren(["child"]);
      relay.setStream(createStream(createTrack("video", "old-source"), null));
      await vi.waitFor(() => expect(signals).toHaveLength(1));
      const original = FakePeerConnection.latest!;
      const recovery = relay.recover("child", signals[0]!.connectionId, false);
      relay.setStream(createStream(createTrack("video", "current-source"), null));
      await vi.waitFor(() => expect(original.senders[0]!.track!.id).toContain("current-source"));
      finishRestart(false);
      await recovery;
      await vi.waitFor(() => expect(signals).toHaveLength(2));
      expect(original.connectionState).toBe("closed");
      expect(FakePeerConnection.latest!.senders[0]!.track!.id).toContain("current-source");
    } finally {
      relay.dispose();
    }
  });

  it("keeps a completed H264 decision across source replacement", async () => {
    codecPreflight.probe.mockResolvedValueOnce("h264");
    const relay = new ViewerRelay(
      { iceServers: [] },
      QUALITY_PROFILES["720p30"],
      { sendSignal: () => true },
    );
    relay.setStream(createStream(createTrack("video", "h264-source"), null));
    await vi.waitFor(() => expect(codecPreflight.probe).toHaveBeenCalledOnce());
    await Promise.resolve();
    relay.setStream(
      createStream(createTrack("video", "replacement-source"), null),
    );
    expect(codecPreflight.probe).toHaveBeenCalledOnce();
    relay.setChildren(["h264-child"]);
    await vi.waitFor(() =>
      expect(FakePeerConnection.latest?.codecPreferenceCalls).toHaveLength(1),
    );

    expect(
      FakePeerConnection.latest!.codecPreferenceCalls[0]?.map(({ mimeType }) =>
        mimeType.toLowerCase(),
      ),
    ).toEqual(["video/h264", "video/vp8"]);
    relay.dispose();
  });

  it("promotes the exact prepared child connection within the current Viewer cap", async () => {
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
    relay.setStream(createStream(createTrack("video", "prepared-video"), null));
    expect(relay.prepareChild(7, routeCandidate(7, "prepared-child"), ["prepared-child"])).toBe(true);
    await vi.waitFor(() => expect(signals).toHaveLength(1));
    const preparedConnection = FakePeerConnection.latest!;
    const preparedConnectionId = signals[0]!.connectionId;
    expect(relay.getSnapshot("prepared-child")).toBeNull();
    expect(FakePeerConnection.activeCount).toBe(1);

    await expect(
      relay.acceptSignal("prepared-child", {
        kind: "description",
        connectionId: "wrong-connection",
        description: { type: "answer", sdp: "wrong-answer" },
      }, 7),
    ).resolves.toBe(false);
    await expect(
      relay.acceptSignal("prepared-child", {
        kind: "description",
        connectionId: preparedConnectionId,
        description: { type: "answer", sdp: "right-answer" },
      }, 6),
    ).resolves.toBe(false);
    const preparedRevision = relay.getSignalRouteRevision(
      "prepared-child",
      preparedConnectionId,
      6,
    );
    expect(preparedRevision).toBe(7);
    const candidate = { candidate: "prepared-candidate" };
    await expect(
      relay.acceptSignal("prepared-child", {
        kind: "candidate",
        connectionId: preparedConnectionId,
        candidate,
      }, preparedRevision),
    ).resolves.toBe(true);
    expect(preparedConnection.addedIceCandidates).toEqual([]);
    await expect(
      relay.acceptSignal("prepared-child", {
        kind: "description",
        connectionId: preparedConnectionId,
        description: { type: "answer", sdp: "right-answer" },
      }, preparedRevision),
    ).resolves.toBe(true);
    expect(preparedConnection.remoteDescription?.sdp).toBe("right-answer");
    expect(preparedConnection.addedIceCandidates).toEqual([candidate]);

    preparedConnection.connectionState = "connected";
    relay.activateChildren(7, ["prepared-child"]);
    expect(FakePeerConnection.latest).toBe(preparedConnection);
    expect(FakePeerConnection.activeCount).toBe(1);
    expect(relay.getSnapshot("prepared-child")?.connectionId).toBe(
      preparedConnectionId,
    );
    relay.dispose();
  });

  it("binds native relay children so their signalling and snapshots stay owned", async () => {
    const stubs = new Map<
      string,
      { peer: HostMediaPeer; events: ViewerRelayPeerEvents }
    >();
    const nativeSnapshot = (peer: HostMediaPeer): PeerSnapshot => ({
      peerId: peer.peerId,
      connectionId: peer.connectionId,
      connectionState: "connected",
      iceConnectionState: "connected",
      metrics: { ...EMPTY_METRICS },
      error: null,
    });
    const peerFactory: ViewerRelayPeerFactory = {
      requiresStream: false,
      create: (childPeerId, connectionId, events) => {
        const edgeConnectionId = connectionId ?? `native-${childPeerId}`;
        const peer: HostMediaPeer = {
          peerId: childPeerId,
          connectionId: edgeConnectionId,
          start: async () =>
            events.sendSignal(childPeerId, {
              kind: "description",
              connectionId: edgeConnectionId,
              description: { type: "offer", sdp: "native-offer" },
            }),
          acceptSignal: async () => undefined,
          restartIce: async () => true,
          isConnected: () => true,
          getSnapshot: () => nativeSnapshot(peer),
          updateIceConfig: () => undefined,
          updateProfile: async () => true,
          updateCaptureProfile: async () => true,
          setPaused: () => undefined,
          replaceStream: async () => true,
          dispose: () => undefined,
        };
        stubs.set(childPeerId, { peer, events });
        return peer;
      },
    };
    const onSenderUpdate = vi.fn();
    const relay = new ViewerRelay(
      { iceServers: [] },
      QUALITY_PROFILES["720p30"],
      { sendSignal: () => true, onSenderUpdate },
      2,
      false,
      peerFactory,
    );

    relay.setChildren(["native-a"]);
    await vi.waitFor(() => expect(stubs.has("native-a")).toBe(true));
    expect(
      relay.prepareChild(7, routeCandidate(7, "native-b"), [
        "native-a",
        "native-b",
      ]),
    ).toBe(true);
    await vi.waitFor(() => expect(stubs.has("native-b")).toBe(true));
    const active = stubs.get("native-a")!;
    const prepared = stubs.get("native-b")!;

    expect(
      active.events.sendSignal("native-a", {
        kind: "candidate",
        connectionId: active.peer.connectionId,
        candidate: { candidate: "late-candidate" },
      }),
    ).toBe(true);
    prepared.events.onUpdate(nativeSnapshot(prepared.peer));
    expect(onSenderUpdate).toHaveBeenLastCalledWith(
      expect.objectContaining({ peerId: "native-b" }),
      7,
    );

    relay.activateChildren(7, ["native-a", "native-b"]);
    prepared.events.onUpdate({
      ...nativeSnapshot(prepared.peer),
      error: { key: "host.err.serverError" },
    });
    expect(relay.getSnapshot("native-b")?.error).toEqual({
      key: "host.err.serverError",
    });
    relay.dispose();
  });

  it("replaces one active child with a same-parent prepared connection", async () => {
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
    relay.setStream(createStream(createTrack("video", "same-parent-video"), null));
    expect(relay.prepareChild(1, routeCandidate(1, "same-child"), ["same-child"])).toBe(true);
    await vi.waitFor(() => expect(signals).toHaveLength(1));
    const firstConnection = FakePeerConnection.latest!;
    await relay.acceptSignal("same-child", {
      kind: "description",
      connectionId: signals[0]!.connectionId,
      description: { type: "answer", sdp: "first-answer" },
    }, 1);
    firstConnection.connectionState = "connected";
    relay.activateChildren(1, ["same-child"]);
    expect(relay.getSnapshot("same-child")?.connectionId).toBe(
      signals[0]!.connectionId,
    );

    expect(relay.prepareChild(2, routeCandidate(2, "same-child"), ["same-child"])).toBe(true);
    await vi.waitFor(() => expect(signals).toHaveLength(2));
    const secondConnection = FakePeerConnection.latest!;
    expect(firstConnection.connectionState).toBe("connected");
    await relay.acceptSignal("same-child", {
      kind: "description",
      connectionId: signals[1]!.connectionId,
      description: { type: "answer", sdp: "second-answer" },
    }, 2);
    secondConnection.connectionState = "connected";
    relay.activateChildren(2, ["same-child"]);

    expect(firstConnection.connectionState).toBe("closed");
    expect(secondConnection.connectionState).toBe("connected");
    expect(relay.getSnapshot("same-child")?.connectionId).toBe(
      signals[1]!.connectionId,
    );
    relay.dispose();
  });

  it("cleans prepared children on failure, replacement, rollback, session reset, and share stop", async () => {
    const onPreparedChildFailed = vi.fn();
    const relay = new ViewerRelay(
      { iceServers: [] },
      QUALITY_PROFILES["720p30"],
      { sendSignal: () => true, onPreparedChildFailed },
    );
    relay.setStream(createStream(createTrack("video", "rollback-video"), null));
    expect(relay.prepareChild(7, routeCandidate(7, "first-probe"), ["first-probe"])).toBe(true);
    await vi.waitFor(() => expect(FakePeerConnection.latest).not.toBeNull());
    const firstProbe = FakePeerConnection.latest!;
    expect(relay.prepareChild(8, routeCandidate(8, "second-probe"), ["second-probe"])).toBe(true);
    const secondProbe = FakePeerConnection.latest!;
    expect(firstProbe.connectionState).toBe("closed");
    relay.activateChildren(9, []);
    expect(secondProbe.connectionState).toBe("closed");
    expect(onPreparedChildFailed).not.toHaveBeenCalled();

    FakePeerConnection.offersFailing = 1;
    const instancesBeforeFailure = FakePeerConnection.instances.length;
    expect(relay.prepareChild(10, routeCandidate(10, "failed-probe"), ["failed-probe"])).toBe(true);
    const failedProbe = FakePeerConnection.latest!;
    await vi.waitFor(() => expect(failedProbe.connectionState).toBe("closed"));
    expect(FakePeerConnection.instances).toHaveLength(instancesBeforeFailure + 1);
    expect(onPreparedChildFailed).toHaveBeenCalledExactlyOnceWith(10, "relay-candidate-10");

    expect(relay.prepareChild(11, routeCandidate(11, "session-probe"), ["session-probe"])).toBe(true);
    const sessionProbe = FakePeerConnection.latest!;
    relay.discardPreparedChild();
    expect(sessionProbe.connectionState).toBe("closed");

    expect(relay.prepareChild(12, routeCandidate(12, "stopped-probe"), ["stopped-probe"])).toBe(true);
    const stoppedProbe = FakePeerConnection.latest!;
    relay.stop();
    expect(stoppedProbe.connectionState).toBe("closed");
    relay.dispose();
    expect(onPreparedChildFailed).toHaveBeenCalledTimes(1);
  });
});
