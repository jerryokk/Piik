import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { type SignalPayload } from "../../src/shared/protocol.ts";
import { ViewerPeer } from "../../src/client/webrtc/viewer-peer.ts";
import { ViewerSfuRoute } from "../../src/client/media/viewer-sfu-route";
import { NativeCapableViewerPeer } from "../../src/client/native/native-viewer";
import { createDeferred, FakePeerConnection, timeoutCallbacks, offer, createPeer, flushAsyncWork, installFakeBrowser, restoreBrowserGlobals } from "./support.ts";

beforeEach(installFakeBrowser);
afterEach(restoreBrowserGlobals);
describe("ViewerPeer connection generations", () => {
  it.each(["dispose", "replace"])("settles pending answer work on %s without a browser completion", async (retirement) => {
    const gate = createDeferred<void>();
    FakePeerConnection.plans.push({ localDescriptionGate: gate.promise });
    const signals: SignalPayload[] = [];
    const peer = createPeer(signals, []);
    let settled = false;
    const old = peer.acceptSignal("parent", offer("old"));
    void old.then(() => { settled = true; });
    await flushAsyncWork();
    expect(FakePeerConnection.instances[0]!.setLocalDescription).toHaveBeenCalledOnce();
    if (retirement === "dispose") peer.dispose();
    else await peer.acceptSignal("parent", offer("new"));
    await flushAsyncWork();
    const settledByRetirement = settled;
    gate.resolve();
    await old;
    peer.dispose();
    expect(signals.filter(value => value.connectionId === "old")).toEqual([]);
    expect(settledByRetirement).toBe(true);
  });

  it("releases authoritative route resync when its old browser operation never completes", async () => {
    const gate = createDeferred<void>();
    FakePeerConnection.plans.push({ localDescriptionGate: gate.promise });
    const signals: SignalPayload[] = [];
    let peer: ViewerPeer | null = createPeer(signals, []);
    let connection = 0;
    const route = new ViewerSfuRoute("viewer-old", {
      activatePeer: async () => {
        peer ??= createPeer(signals, []);
        await peer.acceptSignal("parent", offer(`connection-${++connection}`));
      },
      resetMedia: () => { peer?.dispose(); peer = null; },
      onSfuStream: () => undefined,
      send: () => true,
    });
    const assignment = { upstream: { kind: "peer" as const, peerId: "parent" }, childPeerIds: [], sfuPublicationGeneration: null };
    route.accept({ revision: 1, phase: "active", assignment });
    await flushAsyncWork();
    expect(FakePeerConnection.instances[0]!.setLocalDescription).toHaveBeenCalledOnce();
    let settled = false;
    const resync = route.resyncAuthoritative({ revision: 2, phase: "active", assignment }, "viewer-new");
    void resync.then(() => { settled = true; });
    await flushAsyncWork();
    const resyncedWithoutRetiredWork = settled;
    gate.resolve();
    await resync;
    await route.disconnect();
    peer?.dispose();
    expect(signals.filter(value => value.connectionId === "connection-1")).toEqual([]);
    expect(resyncedWithoutRetiredWork).toBe(true);
  });

  it.each(["route", "viewer"] as const)("handles construction failure with the %s recovery owner and exact identity", async (recoveryOwner) => {
    const sendSignal = vi.fn(() => true);
    const sendRestartRequest = vi.fn(() => true);
    const exhausted = vi.fn((_parent: string, id: string) => {
      expect(peer.hasConnectionId(id)).toBe(true);
      return true;
    });
    const peer = new NativeCapableViewerPeer({ iceServers: [] }, { sendSignal, sendRestartRequest,
      onStream: () => undefined, onUpdate: () => undefined, onRecoveryExhausted: exhausted },
      { recoveryOwner }, async () => null, () => undefined, "session", 2);
    FakePeerConnection.plans.push({ constructorError: new DOMException("Allocation failed", "UnknownError") });
    await expect(peer.acceptSignal("host", offer("first"))).resolves.toBeUndefined();
    expect(peer.hasConnection()).toBe(false);
    expect(peer.getConnectionIdentity()).toEqual({ parentPeerId: "host", connectionId: "first" });
    if (recoveryOwner === "route") {
      expect(sendRestartRequest).not.toHaveBeenCalled();
      expect(exhausted).toHaveBeenCalledExactlyOnceWith("host", "first");
    } else {
      expect(sendRestartRequest).toHaveBeenCalledExactlyOnceWith("host", "first", true);
      FakePeerConnection.plans.push({ constructorError: new DOMException("Allocation failed", "UnknownError") });
      await peer.acceptSignal("host", offer("second"));
      expect(sendRestartRequest).toHaveBeenCalledOnce();
      expect(exhausted).toHaveBeenCalledExactlyOnceWith("host", "second");
    }
    expect(timeoutCallbacks.size).toBe(0);
    await peer.acceptSignal("host", offer("working"));
    expect(sendSignal).toHaveBeenCalledOnce();
    expect(peer.hasConnectionId("working")).toBe(true);
    expect(peer.hasConnectionId("first")).toBe(false);
    peer.dispose();
    expect(peer.hasConnectionId("working")).toBe(false);
  });

  it("returns failed prepared SDP to its route owner without requesting a rebuild", async () => {
    const restart = vi.fn(() => true), exhausted = vi.fn(() => true);
    const peer = new ViewerPeer({ iceServers: [] }, { sendSignal: () => true, sendRestartRequest: restart,
      onStream: () => undefined, onUpdate: () => undefined, onRecoveryExhausted: exhausted },
      { recoveryOwner: "route" });
    FakePeerConnection.plans.push({ answerError: new Error("answer failed") });
    await peer.acceptSignal("host", offer("prepared"));
    expect(restart).not.toHaveBeenCalled();
    expect(exhausted).toHaveBeenCalledExactlyOnceWith("host", "prepared");
    expect(timeoutCallbacks.size).toBe(0);
    peer.dispose();
  });
});
