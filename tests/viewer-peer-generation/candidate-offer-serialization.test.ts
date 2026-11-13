import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { type SignalPayload } from "../../src/shared/protocol.ts";
import { type PeerSnapshot } from "../../src/client/types.ts";
import { ViewerPeer } from "../../src/client/webrtc/viewer-peer.ts";
import { createDeferred, FakePeerConnection, intervalCallbacks, timeoutCallbacks, timeoutDelays, offer, candidate, createPeer, flushAsyncWork, installFakeBrowser, restoreBrowserGlobals } from "./support.ts";

beforeEach(installFakeBrowser);
afterEach(restoreBrowserGlobals);
describe("ViewerPeer connection generations", () => {
  it("retries delivery of an exhausted edge after signaling reconnects", async () => {
    let reportAvailable = false;
    const exhausted: string[] = [];
    const peer = new ViewerPeer(
      { iceServers: [] },
      {
        sendSignal: () => true,
        sendRestartRequest: () => true,
        onStream: () => undefined,
        onUpdate: () => undefined,
        onRecoveryExhausted: (_peerId, connectionId) => {
          exhausted.push(connectionId);
          return reportAvailable;
        },
      },
    );
    await peer.acceptSignal("relay-parent", offer("failed-connection"));
    const connection = FakePeerConnection.instances[0]!;
    connection.connectionState = "failed";
    connection.dispatchEvent(new Event("connectionstatechange"));

    timeoutCallbacks.get(2)!();
    timeoutCallbacks.get(3)!();
    expect(exhausted).toEqual(["failed-connection"]);

    reportAvailable = true;
    timeoutCallbacks.get(4)!();
    expect(exhausted).toEqual(["failed-connection", "failed-connection"]);

    connection.dispatchEvent(new Event("connectionstatechange"));
    expect(exhausted).toHaveLength(2);
  });

  it("requests one bounded rebuild when answer negotiation fails", async () => {
    FakePeerConnection.plans.push(
      { answerError: new Error("first answer failed") },
      { answerError: new Error("second answer failed") },
    );
    const restartRequests: Array<{
      peerId: string;
      connectionId: string;
      rebuild: boolean;
    }> = [];
    const peer = new ViewerPeer(
      { iceServers: [] },
      {
        sendSignal: () => true,
        sendRestartRequest: (peerId, connectionId, rebuild) => {
          restartRequests.push({ peerId, connectionId, rebuild });
          return true;
        },
        onStream: () => undefined,
        onUpdate: () => undefined,
      },
    );

    await peer.acceptSignal("relay-parent", offer("connection-first"));
    await peer.acceptSignal("relay-parent", offer("connection-second"));

    expect(restartRequests).toEqual([
      {
        peerId: "relay-parent",
        connectionId: "connection-first",
        rebuild: true,
      },
    ]);
  });

  it("requests a rebuild when the completed answer cannot be signaled", async () => {
    const restartRequests: Array<{
      peerId: string;
      connectionId: string;
      rebuild: boolean;
    }> = [];
    const peer = new ViewerPeer(
      { iceServers: [] },
      {
        sendSignal: () => false,
        sendRestartRequest: (peerId, connectionId, rebuild) => {
          restartRequests.push({ peerId, connectionId, rebuild });
          return true;
        },
        onStream: () => undefined,
        onUpdate: () => undefined,
      },
    );

    await peer.acceptSignal("relay-parent", offer("unsent-answer"));

    expect(restartRequests).toEqual([
      {
        peerId: "relay-parent",
        connectionId: "unsent-answer",
        rebuild: true,
      },
    ]);
    expect([...timeoutDelays.values()]).toEqual([3_000]);
  });

  it("serializes recovery requests at the current parent", async () => {
    const signals: SignalPayload[] = [];
    const signalPeers: string[] = [];
    const restartRequests: Array<{
      peerId: string;
      connectionId: string;
      rebuild: boolean;
    }> = [];
    const peer = new ViewerPeer(
      { iceServers: [] },
      {
        sendSignal: (peerId, payload) => {
          signalPeers.push(peerId);
          signals.push(payload);
          return true;
        },
        sendRestartRequest: (peerId, connectionId, rebuild) => {
          restartRequests.push({ peerId, connectionId, rebuild });
          return true;
        },
        onStream: () => undefined,
        onUpdate: () => undefined,
      },
    );

    await peer.acceptSignal("parent-old", offer("connection-old"));
    await peer.acceptSignal("parent-new", offer("connection-new"));
    expect(signalPeers).toEqual(["parent-old", "parent-new"]);

    expect(peer.requestRecovery(true)).toBe(true);
    expect(peer.requestRecovery(true)).toBe(false);
    expect(restartRequests).toEqual([
      {
        peerId: "parent-new",
        connectionId: "connection-new",
        rebuild: true,
      },
    ]);

    const connection = FakePeerConnection.instances.at(-1)!;
    connection.connectionState = "connected";
    [...timeoutCallbacks.values()].at(-1)!();
    expect(peer.requestRecovery(true)).toBe(true);
    expect(restartRequests).toHaveLength(2);
    peer.dispose();
  });

  it("retires the recovery deadline when a rebuilt connection is answered", async () => {
    const exhausted: string[] = [];
    const peer = new ViewerPeer(
      { iceServers: [] },
      {
        sendSignal: () => true,
        sendRestartRequest: () => true,
        onStream: () => undefined,
        onUpdate: () => undefined,
        onRecoveryExhausted: (_peerId, connectionId) => {
          exhausted.push(connectionId);
          return true;
        },
      },
    );

    await peer.acceptSignal("relay-parent", offer("connection-before-rebuild"));
    expect(peer.requestRecovery(true)).toBe(true);
    await peer.acceptSignal("relay-parent", offer("connection-after-rebuild"));

    expect([...timeoutDelays.values()]).toEqual([15_000]);
    expect(exhausted).toEqual([]);
    peer.dispose();
  });

  it("does not send an old answer or candidate after replacing the connection", async () => {
    const oldLocalDescription = createDeferred<void>();
    FakePeerConnection.plans.push({
      localDescriptionGate: oldLocalDescription.promise,
    });
    const signals: SignalPayload[] = [];
    const snapshots: PeerSnapshot[] = [];
    const peer = createPeer(signals, snapshots);

    const oldAccept = peer.acceptSignal("host", offer("connection-old"));
    await vi.waitFor(() => {
      expect(FakePeerConnection.instances[0]?.setLocalDescription).toHaveBeenCalledOnce();
    });
    const oldConnection = FakePeerConnection.instances[0]!;

    await peer.acceptSignal("host", offer("connection-new"));
    const iceEvent = new Event("icecandidate");
    Object.defineProperty(iceEvent, "candidate", {
      value: {
        candidate: "candidate-from-old-connection",
        sdpMid: "0",
        sdpMLineIndex: 0,
        usernameFragment: null,
      },
    });
    oldConnection.dispatchEvent(iceEvent);
    oldLocalDescription.resolve();
    await oldAccept;

    expect(signals).toHaveLength(1);
    expect(signals[0]).toMatchObject({
      kind: "description",
      connectionId: "connection-new",
      description: { type: "answer" },
    });
    expect(snapshots.at(-1)).toMatchObject({
      connectionId: "connection-new",
      error: null,
    });
  });

  it("serializes ordinary offers for the same connection", async () => {
    const firstLocalDescription = createDeferred<void>();
    FakePeerConnection.plans.push({
      localDescriptionGate: firstLocalDescription.promise,
    });
    const signals: SignalPayload[] = [];
    const peer = createPeer(signals, []);

    const first = peer.acceptSignal("host", offer("same-connection"));
    await vi.waitFor(() =>
      expect(
        FakePeerConnection.instances[0]?.setLocalDescription,
      ).toHaveBeenCalledOnce(),
    );
    const connection = FakePeerConnection.instances[0]!;
    const second = peer.acceptSignal("host", offer("same-connection"));
    await Promise.resolve();
    expect(connection.setRemoteDescription).toHaveBeenCalledOnce();

    firstLocalDescription.resolve();
    await Promise.all([first, second]);
    expect(connection.setRemoteDescription).toHaveBeenCalledTimes(2);
    expect(
      signals.filter((signal) => signal.kind === "description"),
    ).toHaveLength(2);
  });

  it("stops flushing old candidates when the connection is replaced", async () => {
    const firstCandidate = createDeferred<void>();
    FakePeerConnection.plans.push({
      candidateGates: [firstCandidate.promise],
    });
    const peer = createPeer([], []);
    await peer.acceptSignal("host", candidate("connection-old", "candidate-1"));
    await peer.acceptSignal("host", candidate("connection-old", "candidate-2"));

    const oldAccept = peer.acceptSignal("host", offer("connection-old"));
    await vi.waitFor(() => {
      expect(FakePeerConnection.instances[0]?.addIceCandidate).toHaveBeenCalledOnce();
    });
    const oldConnection = FakePeerConnection.instances[0]!;

    await peer.acceptSignal("host", offer("connection-new"));
    const newConnection = FakePeerConnection.instances[1]!;
    // Retirement must settle the old flush without awaiting the browser gate.
    await oldAccept;
    firstCandidate.resolve();

    expect(oldConnection.addIceCandidate).toHaveBeenCalledTimes(1);
    expect(newConnection.addIceCandidate).not.toHaveBeenCalled();
  });

  it("does not apply stats that resolve after the connection is replaced", async () => {
    const signals: SignalPayload[] = [];
    const snapshots: PeerSnapshot[] = [];
    const peer = createPeer(signals, snapshots);
    await peer.acceptSignal("host", offer("connection-old"));
    const oldConnection = FakePeerConnection.instances[0]!;
    const oldStats = createDeferred<RTCStatsReport>();
    oldConnection.statsGate = oldStats.promise;
    const oldStatsCallback = intervalCallbacks.get(1)!;

    oldStatsCallback();
    await vi.waitFor(() => expect(oldConnection.getStats).toHaveBeenCalledOnce());
    await peer.acceptSignal("host", offer("connection-new"));
    const newSnapshotCount = snapshots.filter(
      ({ connectionId }) => connectionId === "connection-new",
    ).length;

    oldStats.resolve(
      new Map([
        [
          "inbound-old",
          {
            id: "inbound-old",
            type: "inbound-rtp",
            timestamp: 2_000,
            kind: "video",
            bytesReceived: 1_000,
            framesDecoded: 60,
            frameWidth: 1_920,
            frameHeight: 1_080,
          },
        ],
      ]) as unknown as RTCStatsReport,
    );
    await flushAsyncWork();

    const newSnapshots = snapshots.filter(
      ({ connectionId }) => connectionId === "connection-new",
    );
    expect(newSnapshots).toHaveLength(newSnapshotCount);
    expect(newSnapshots.at(-1)?.metrics.resolution).toBeNull();
  });

  it("skips overlapping stats ticks on the same connection", async () => {
    const peer = createPeer([], []);
    await peer.acceptSignal("host", offer("connection"));
    const connection = FakePeerConnection.instances[0]!;
    const stats = createDeferred<RTCStatsReport>();
    connection.statsGate = stats.promise;
    const statsCallback = intervalCallbacks.get(1)!;

    statsCallback();
    statsCallback();
    await vi.waitFor(() => expect(connection.getStats).toHaveBeenCalledOnce());

    stats.resolve(new Map() as unknown as RTCStatsReport);
    await flushAsyncWork();
    connection.statsGate = null;
    statsCallback();
    await vi.waitFor(() =>
      expect(connection.getStats).toHaveBeenCalledTimes(2),
    );
  });
});
