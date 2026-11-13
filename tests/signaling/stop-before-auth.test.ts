import { afterEach, describe, expect, it, vi } from "vitest";
import { DEFAULT_QUALITY_SETTINGS, DEFAULT_ROUTE_POLICY, SIGNALING_PROTOCOL } from "../../src/shared/protocol.ts";
import { REACTION_IDS } from "../../src/shared/room-interactions.ts";
import { shouldReconnectSignaling, SignalingClient } from "../../src/client/lib/signaling";
import { restoreClientState, routeAuthenticated } from "../support/client-state.ts";

afterEach(restoreClientState);
describe("client signaling recovery policy", () => {
  it("reconciles a stop requested before Host authentication", () => {
    vi.useFakeTimers();
    const sockets: FakeWebSocket[] = [];
    class FakeWebSocket extends EventTarget {
      static readonly OPEN = 1;
      static readonly CLOSING = 2;
      readyState = FakeWebSocket.OPEN;
      readonly send = vi.fn();
      readonly close = vi.fn();
      constructor() { super(); sockets.push(this); }
    }
    vi.stubGlobal("WebSocket", FakeWebSocket);
    vi.stubGlobal("window", {
      location: new URL("https://share.test/"),
      setTimeout,
      clearTimeout,
    });
    const signal = new SignalingClient(
      {
        roomId: "1234",
        role: "host",
        token: "h".repeat(43),
        clientId: "host-client",
        shareGeneration: "share_generation_12345678",
        routePolicy: DEFAULT_ROUTE_POLICY,
      },
      {
        onMessage: () => undefined,
        onStatus: () => undefined,
        onTerminated: () => undefined,
        onAccessRequired: () => undefined,
      },
    );
    const receive = (value: object) => {
      const event = new Event("message");
      Object.defineProperty(event, "data", { value: JSON.stringify(value) });
      sockets[0]!.dispatchEvent(event);
    };

    signal.start();
    sockets[0]!.dispatchEvent(new Event("open"));
    expect(signal.stopSharing("share_generation_12345678")).toBe(false);
    receive({
      type: "authenticated",
      protocol: SIGNALING_PROTOCOL,
      role: "host",
      peerId: "host_12345678",
      maxViewers: 20,
      endpointMediaCopyCapacity: 2,
      hostOnline: true,
      connectionId: null,
      ...routeAuthenticated("share_generation_12345678"),
      iceConfig: { iceServers: [], natPredictionStunUrls: [] },
      codeEntryPolicy: "open",
      viewerPasswordEnabled: false,
      viewerAuthorizationGeneration: "viewer_generation_12345678",
    });
    expect(
      sockets[0]!.send.mock.calls.map(([value]) => JSON.parse(String(value))),
    ).toEqual([
      expect.objectContaining({ type: "authenticate", shareGeneration: "share_generation_12345678" }),
      { type: "stop-sharing", shareGeneration: "share_generation_12345678" },
    ]);
    signal.stop();
  });

  it("does not resend a start already claimed by Host authentication", () => {
    vi.useFakeTimers();
    const sockets: FakeWebSocket[] = [];
    class FakeWebSocket extends EventTarget {
      static readonly OPEN = 1;
      static readonly CLOSING = 2;
      readyState = FakeWebSocket.OPEN;
      readonly send = vi.fn();
      readonly close = vi.fn();
      constructor() { super(); sockets.push(this); }
    }
    vi.stubGlobal("WebSocket", FakeWebSocket);
    vi.stubGlobal("window", {
      location: new URL("https://share.test/"),
      setTimeout,
      clearTimeout,
    });
    const signal = new SignalingClient(
      {
        roomId: "1234",
        role: "host",
        token: "h".repeat(43),
        clientId: "host-client",
        routePolicy: DEFAULT_ROUTE_POLICY,
      },
      {
        onMessage: () => undefined,
        onStatus: () => undefined,
        onTerminated: () => undefined,
        onAccessRequired: () => undefined,
      },
    );
    const receive = (value: object) => {
      const event = new Event("message");
      Object.defineProperty(event, "data", { value: JSON.stringify(value) });
      sockets[0]!.dispatchEvent(event);
    };
    const generation = "share_generation_12345678";
    signal.startSharing(generation, DEFAULT_QUALITY_SETTINGS, DEFAULT_ROUTE_POLICY);
    signal.start();
    sockets[0]!.dispatchEvent(new Event("open"));
    receive({
      type: "authenticated",
      protocol: SIGNALING_PROTOCOL,
      role: "host",
      peerId: "host_12345678",
      maxViewers: 20,
      endpointMediaCopyCapacity: 2,
      hostOnline: true,
      connectionId: null,
      ...routeAuthenticated(generation),
      iceConfig: { iceServers: [], natPredictionStunUrls: [] },
      codeEntryPolicy: "open",
      viewerPasswordEnabled: false,
      viewerAuthorizationGeneration: "viewer_generation_12345678",
    });
    expect(
      sockets[0]!.send.mock.calls.map(([value]) => JSON.parse(String(value))).map((value: { type: string }) => value.type),
    ).toEqual(["authenticate", "set-co-play-presence"]);
    expect(JSON.parse(String(sockets[0]!.send.mock.calls[0]![0]))).toMatchObject({
      type: "authenticate",
      shareGeneration: generation,
    });
    signal.stop();
  });

  it.each([false, true])("serializes a stop/start when an old Host authentication reply arrives late (rejected=%s)", (rejected) => {
    vi.useFakeTimers();
    const sockets: FakeWebSocket[] = [];
    class FakeWebSocket extends EventTarget {
      static readonly OPEN = 1;
      static readonly CLOSING = 2;
      readyState = FakeWebSocket.OPEN;
      readonly send = vi.fn();
      readonly close = vi.fn();
      constructor() { super(); sockets.push(this); }
    }
    vi.stubGlobal("WebSocket", FakeWebSocket);
    vi.stubGlobal("window", {
      location: new URL("https://share.test/"),
      setTimeout,
      clearTimeout,
    });
    const signal = new SignalingClient(
      {
        roomId: "1234",
        role: "host",
        token: "h".repeat(43),
        clientId: "host-client",
        routePolicy: DEFAULT_ROUTE_POLICY,
      },
      {
        onMessage: () => undefined,
        onStatus: () => undefined,
        onTerminated: () => undefined,
        onAccessRequired: () => undefined,
      },
    );
    const receive = (value: object) => {
      const event = new Event("message");
      Object.defineProperty(event, "data", { value: JSON.stringify(value) });
      sockets[0]!.dispatchEvent(event);
    };
    const first = "first_share_generation_12345678";
    const next = "next_share_generation_12345678";
    signal.startSharing(first, DEFAULT_QUALITY_SETTINGS, DEFAULT_ROUTE_POLICY);
    signal.start();
    sockets[0]!.dispatchEvent(new Event("open"));
    expect(signal.stopSharing(first)).toBe(false);
    expect(signal.startSharing(next, DEFAULT_QUALITY_SETTINGS, DEFAULT_ROUTE_POLICY)).toBe(true);
    receive({
      type: "authenticated",
      protocol: SIGNALING_PROTOCOL,
      role: "host",
      peerId: "host_12345678",
      maxViewers: 20,
      endpointMediaCopyCapacity: 2,
      hostOnline: true,
      connectionId: null,
      ...routeAuthenticated(first),
      iceConfig: { iceServers: [], natPredictionStunUrls: [] },
      codeEntryPolicy: "open",
      viewerPasswordEnabled: false,
      viewerAuthorizationGeneration: "viewer_generation_12345678",
    });
    const messages = sockets[0]!.send.mock.calls.map(([value]) => JSON.parse(String(value)));
    expect(messages).toEqual([
      expect.objectContaining({ type: "authenticate", shareGeneration: first }),
      { type: "stop-sharing", shareGeneration: first },
      expect.objectContaining({ type: "start-sharing", shareGeneration: next }),
    ]);
    if (rejected) {
      receive({ type: "sharing-start-failed", shareGeneration: first, code: "FORBIDDEN" });
      expect(signal.wantsHostPublication(next)).toBe(true);
      receive({ type: "sharing-start-failed", shareGeneration: next, code: "FORBIDDEN" });
      expect(signal.wantsHostPublication(next)).toBe(false);
      expect(signal.stopSharing(next)).toBe(false);
      expect(sockets[0]!.send).toHaveBeenCalledTimes(messages.length);
      expect(sockets[0]!.close).not.toHaveBeenCalled();
    } else {
      expect(signal.stopSharing(next)).toBe(true);
      expect(JSON.parse(String(sockets[0]!.send.mock.calls.at(-1)![0]))).toEqual({
        type: "stop-sharing", shareGeneration: next,
      });
    }
    signal.stop();
  });

  it("starts room interactions with the authenticated session and keeps signaling", () => {
    vi.useFakeTimers();
    const sockets: FakeWebSocket[] = [];
    class FakeWebSocket extends EventTarget {
      static readonly OPEN = 1;
      static readonly CLOSING = 2;
      readyState = 1;
      bufferedAmount = 0;
      send = vi.fn();
      close = vi.fn();
      constructor() { super(); sockets.push(this); }
    }
    vi.stubGlobal("WebSocket", FakeWebSocket);
    vi.stubGlobal("window", { location: new URL("https://share.test/r/1234"), setTimeout, clearTimeout });
    const onMessage = vi.fn();
    const signal = new SignalingClient({ roomId: "1234", role: "viewer", clientId: "viewer-client" }, {
      onMessage, onStatus: vi.fn(), onTerminated: vi.fn(), onAccessRequired: vi.fn(),
    });
    const receive = (body: object) => {
      const event = new Event("message");
      Object.defineProperty(event, "data", { value: JSON.stringify(body) });
      sockets[0].dispatchEvent(event);
    };
    expect(signal.interactions.getSnapshot().reactionIds).toEqual(REACTION_IDS);
    signal.start();
    sockets[0].dispatchEvent(new Event("open"));
    receive({ type: "authenticated", protocol: SIGNALING_PROTOCOL, role: "viewer", peerId: "viewer_12345678",
      maxViewers: 20, endpointMediaCopyCapacity: 2, hostOnline: false, connectionId: null,
      iceConfig: { iceServers: [], natPredictionStunUrls: [] }, codeEntryPolicy: "open",
      viewerAuthorizationGeneration: "viewer_generation_12345678", ...routeAuthenticated(),
    });
    receive({ type: "room-interactions-ready", serverTime: Date.now() });
    expect(signal.interactions.getSnapshot().ready).toBe(true);
    expect(sockets).toHaveLength(1);
    expect(sockets[0].close).not.toHaveBeenCalled();
    expect(onMessage).toHaveBeenCalledTimes(1); // Only authentication reaches media handling.
    sockets[0].bufferedAmount = 20 * 1024;
    expect(signal.interactions.send({ kind: "chat", text: "hello" })).toBe(false);
    expect(signal.interactions.getSnapshot().pending).toBeNull();
    signal.stop();
    expect(vi.getTimerCount()).toBe(0);
  });

  it("does not reconnect a session that another tab replaced", () => {
    expect(shouldReconnectSignaling(4001)).toBe(false);
    expect(shouldReconnectSignaling(4003)).toBe(false);
    expect(shouldReconnectSignaling(4004)).toBe(false);
    expect(shouldReconnectSignaling(1008)).toBe(false);
    expect(shouldReconnectSignaling(1012)).toBe(true);
    expect(shouldReconnectSignaling(4002)).toBe(true);
    expect(shouldReconnectSignaling(1006)).toBe(true);
  });

  it("uses the common reconnect state for a recoverable close", () => {
    vi.useFakeTimers();
    const sockets: FakeWebSocket[] = [];
    class FakeWebSocket extends EventTarget {
      static readonly OPEN = 1;
      static readonly CLOSING = 2;
      readyState = FakeWebSocket.OPEN;
      readonly send = vi.fn();
      readonly close = vi.fn();

      constructor(readonly url: string) {
        super();
        sockets.push(this);
      }
    }
    vi.stubGlobal("WebSocket", FakeWebSocket);
    vi.stubGlobal("window", {
      location: new URL("https://share.test/r/1234"),
      setTimeout,
      clearTimeout,
    });
    const statuses: string[] = [];
    const signal = new SignalingClient(
      {
        roomId: "1234",
        role: "viewer",
        clientId: "viewer-client",
      },
      {
        onMessage: () => undefined,
        onStatus: (status) => statuses.push(status),
        onTerminated: () => undefined,
        onAccessRequired: () => undefined,
      },
    );

    signal.start();
    sockets[0]!.dispatchEvent(new Event("open"));
    const closed = new Event("close");
    Object.defineProperty(closed, "code", { value: 1012 });
    sockets[0]!.dispatchEvent(closed);

    expect(statuses.at(-1)).toBe("reconnecting");
    signal.stop();
  });

  it("retains a terminal command until an authenticated socket accepts it", () => {
    vi.useFakeTimers();
    const sockets: FakeWebSocket[] = [];
    class FakeWebSocket extends EventTarget {
      static readonly OPEN = 1;
      static readonly CLOSING = 2;
      readyState = FakeWebSocket.OPEN;
      readonly send = vi.fn();
      readonly close = vi.fn(() => {
        this.readyState = FakeWebSocket.CLOSING;
      });

      constructor(readonly url: string) {
        super();
        sockets.push(this);
      }
    }
    vi.stubGlobal("WebSocket", FakeWebSocket);
    vi.stubGlobal("window", {
      location: new URL("https://share.test/r/1234"),
      setTimeout: globalThis.setTimeout,
      clearTimeout: globalThis.clearTimeout,
    });
    const signal = new SignalingClient(
      {
        roomId: "1234",
        role: "viewer",
        clientId: "viewer-client",
      },
      {
        onMessage: () => undefined,
        onStatus: () => undefined,
        onTerminated: () => undefined,
        onAccessRequired: () => undefined,
      },
    );
    const receiveAuthenticated = (socket: FakeWebSocket) => {
      const event = new Event("message");
      Object.defineProperty(event, "data", {
        value: JSON.stringify({
          type: "authenticated",
          protocol: SIGNALING_PROTOCOL,
          role: "viewer",
          peerId: "viewer_12345678",
          maxViewers: 8,
          endpointMediaCopyCapacity: 2,
          hostOnline: true,
          connectionId: null,
          ...routeAuthenticated(),
          iceConfig: { iceServers: [], natPredictionStunUrls: [] },
          codeEntryPolicy: "open",
          viewerAuthorizationGeneration: "viewer_generation_12345678",
        }),
      });
      socket.dispatchEvent(event);
    };

    signal.start();
    signal.sendThenStop({ type: "abandon-room" });
    sockets[0]!.dispatchEvent(new Event("open"));
    sockets[0]!.readyState = FakeWebSocket.CLOSING;
    receiveAuthenticated(sockets[0]!);
    expect(sockets[0]!.send).toHaveBeenCalledOnce();

    const closed = new Event("close");
    Object.defineProperties(closed, {
      code: { value: 1006 },
      reason: { value: "network interrupted" },
    });
    sockets[0]!.dispatchEvent(closed);
    vi.advanceTimersByTime(750);
    sockets[1]!.dispatchEvent(new Event("open"));
    receiveAuthenticated(sockets[1]!);

    expect(
      sockets[1]!.send.mock.calls.map(([value]) =>
        JSON.parse(String(value)),
      ),
    ).toEqual([
      expect.objectContaining({ type: "authenticate" }),
      { type: "abandon-room" },
    ]);
    expect(sockets[1]!.close).toHaveBeenCalledWith(1000, "client closed");
  });

  it("classifies a close-only authentication failure without claiming replacement", () => {
    const sockets: FakeWebSocket[] = [];
    class FakeWebSocket extends EventTarget {
      static readonly OPEN = 1;
      static readonly CLOSING = 2;
      readyState = FakeWebSocket.OPEN;
      readonly send = vi.fn();
      readonly close = vi.fn();

      constructor(readonly url: string) {
        super();
        sockets.push(this);
      }
    }
    vi.stubGlobal("WebSocket", FakeWebSocket);
    vi.stubGlobal("window", {
      location: new URL("https://share.test/r/1234"),
      setTimeout,
      clearTimeout,
    });
    const onTerminated = vi.fn();
    const signal = new SignalingClient(
      {
        roomId: "1234",
        role: "viewer",
        clientId: "viewer-client",
      },
      {
        onMessage: () => undefined,
        onStatus: () => undefined,
        onTerminated,
        onAccessRequired: () => undefined,
      },
    );

    signal.start();
    sockets[0]!.dispatchEvent(new Event("open"));
    const closed = new Event("close");
    Object.defineProperties(closed, {
      code: { value: 4003 },
      reason: { value: "Authentication required" },
    });
    sockets[0]!.dispatchEvent(closed);

    expect(onTerminated).toHaveBeenCalledOnce();
    expect(onTerminated).toHaveBeenCalledWith("SIGNAL_TERMINATED");
    expect(sockets).toHaveLength(1);
  });
});
