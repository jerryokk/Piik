import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { ViewerPeer, type ViewerPeerEvents } from "../../src/client/webrtc/viewer-peer.ts";
import { NativeCapableViewerPeer } from "../../src/client/native/native-viewer";
import { FakePeerConnection, timeoutCallbacks, timeoutDelays, offer, createPeer, installFakeBrowser, restoreBrowserGlobals } from "./support.ts";

beforeEach(installFakeBrowser);
afterEach(restoreBrowserGlobals);
describe("ViewerPeer connection generations", () => {
  it("cancels the initial deadline after connecting", async () => {
    const restartRequests: string[] = [];
    const peer = new ViewerPeer(
      { iceServers: [] },
      {
        sendSignal: () => true,
        sendRestartRequest: (_peerId, connectionId) => {
          restartRequests.push(connectionId);
          return true;
        },
        onStream: () => undefined,
        onUpdate: () => undefined,
      },
    );

    await peer.acceptSignal("host", offer("connected-generation"));
    const initialDeadline = timeoutCallbacks.get(1)!;
    const connection = FakePeerConnection.instances[0]!;
    connection.connectionState = "connected";
    connection.dispatchEvent(new Event("connectionstatechange"));

    expect(timeoutCallbacks.has(1)).toBe(false);
    initialDeadline();
    expect(restartRequests).toEqual([]);
  });

  it("ignores an initial deadline from an old connection generation", async () => {
    const restartRequests: string[] = [];
    const peer = new ViewerPeer(
      { iceServers: [] },
      {
        sendSignal: () => true,
        sendRestartRequest: (_peerId, connectionId) => {
          restartRequests.push(connectionId);
          return true;
        },
        onStream: () => undefined,
        onUpdate: () => undefined,
      },
    );

    await peer.acceptSignal("host", offer("connection-old"));
    const oldDeadline = timeoutCallbacks.get(1)!;
    await peer.acceptSignal("host", offer("connection-new"));

    expect(timeoutCallbacks.has(1)).toBe(false);
    expect(timeoutDelays.get(2)).toBe(15_000);
    oldDeadline();
    expect(restartRequests).toEqual([]);

    const currentConnection = FakePeerConnection.instances[1]!;
    currentConnection.connectionState = "connected";
    currentConnection.dispatchEvent(new Event("connectionstatechange"));
    expect(timeoutCallbacks.has(2)).toBe(false);
  });

  it("clears the initial deadline when disposed", async () => {
    const peer = createPeer([], []);
    await peer.acceptSignal("host", offer("disposed-connection"));

    peer.dispose();

    expect(timeoutCallbacks).toEqual(new Map());
    expect(timeoutDelays).toEqual(new Map());
  });

  it.each([false, true])("bounds recovery across rebuilt Browser connections (native wrapper=%s)", async (wrapped) => {
    const restartRequests: Array<{
      peerId: string;
      connectionId: string;
      rebuild: boolean;
    }> = [];
    const exhausted: Array<{ peerId: string; connectionId: string }> = [];
    const events: ViewerPeerEvents = {
      sendSignal: () => true,
      sendRestartRequest: (peerId, connectionId, rebuild) => {
        restartRequests.push({ peerId, connectionId, rebuild });
        return true;
      },
      onStream: () => undefined,
      onUpdate: () => undefined,
      onRecoveryExhausted: (peerId, connectionId) => {
        exhausted.push({ peerId, connectionId });
        return true;
      },
    };
    const peer = wrapped
      ? new NativeCapableViewerPeer({ iceServers: [] }, events, {}, async () => null,
          () => undefined, "session", 2)
      : new ViewerPeer({ iceServers: [] }, events);
    await peer.acceptSignal("relay-parent", offer("failed-connection"));
    const connection = FakePeerConnection.instances[0]!;
    connection.connectionState = "failed";
    connection.dispatchEvent(new Event("connectionstatechange"));

    expect(restartRequests).toEqual([
      {
        peerId: "relay-parent",
        connectionId: "failed-connection",
        rebuild: false,
      },
    ]);
    timeoutCallbacks.get(2)!();
    expect(restartRequests.at(-1)).toEqual({
      peerId: "relay-parent",
      connectionId: "failed-connection",
      rebuild: true,
    });
    await peer.acceptSignal("relay-parent", offer("rebuilt-connection"));
    expect(timeoutCallbacks.has(3)).toBe(false);
    connection.connectionState = "connected";
    connection.dispatchEvent(new Event("connectionstatechange"));
    const rebuilt = FakePeerConnection.instances.at(-1)!;
    rebuilt.connectionState = "failed";
    rebuilt.dispatchEvent(new Event("connectionstatechange"));
    expect(exhausted).toEqual([
      { peerId: "relay-parent", connectionId: "rebuilt-connection" },
    ]);

    rebuilt.dispatchEvent(new Event("connectionstatechange"));
    expect(restartRequests).toHaveLength(2);
    expect(exhausted).toHaveLength(1);
    expect(peer.requestRecovery(true)).toBe(true);
    [...timeoutCallbacks.values()].at(-1)!();
    expect(restartRequests).toHaveLength(3);
    expect(exhausted).toHaveLength(2);
    peer.dispose();
  });
});
