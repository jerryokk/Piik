import { afterEach, describe, expect, it, vi } from "vitest";
import { DEFAULT_ROUTE_POLICY, SIGNALING_PROTOCOL, type QualitySettings } from "../../src/shared/protocol.ts";
import { SignalingClient } from "../../src/client/lib/signaling";
import { restoreClientState, routeAuthenticated } from "../support/client-state.ts";

afterEach(restoreClientState);
describe("client signaling recovery policy", () => {
  it("keeps a Host paused across reconnect until an explicit Resume is sent", () => {
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
        role: "host",
        token: "h".repeat(43),
        clientId: "host-client",
        routePolicy: DEFAULT_ROUTE_POLICY,
        shareGeneration: "share_generation_12345678",
        sharingPaused: true,
      },
      {
        onMessage: () => undefined,
        onStatus: () => undefined,
        onTerminated: () => undefined,
        onAccessRequired: () => undefined,
      },
    );
    const receive = (socket: FakeWebSocket, value: object) => {
      const event = new Event("message");
      Object.defineProperty(event, "data", { value: JSON.stringify(value) });
      socket.dispatchEvent(event);
    };
    const authenticate = (socket: FakeWebSocket) => {
      socket.dispatchEvent(new Event("open"));
      receive(socket, {
        type: "authenticated",
        protocol: SIGNALING_PROTOCOL,
        role: "host",
        peerId: "host_12345678",
        maxViewers: 8,
        endpointMediaCopyCapacity: 2,
        hostOnline: true,
        hostPaused: true,
        connectionId: null,
        ...routeAuthenticated("share_generation_12345678"),
        iceConfig: { iceServers: [], natPredictionStunUrls: [] },
        codeEntryPolicy: "open",
        viewerPasswordEnabled: false,
        viewerAuthorizationGeneration: "viewer_generation_12345678",
      });
    };
    const reconnect = (socket: FakeWebSocket) => {
      const close = new Event("close");
      Object.defineProperties(close, {
        code: { value: 1006 },
        reason: { value: "network interrupted" },
      });
      socket.dispatchEvent(close);
      vi.advanceTimersByTime(750);
    };

    signal.start();
    authenticate(sockets[0]!);
    reconnect(sockets[0]!);
    sockets[1]!.dispatchEvent(new Event("open"));
    expect(JSON.parse(String(sockets[1]!.send.mock.calls[0]![0]))).toMatchObject({
      type: "authenticate",
      sharingPaused: true,
    });
    receive(sockets[1]!, {
      type: "authenticated",
      protocol: SIGNALING_PROTOCOL,
      role: "host",
      peerId: "host_12345678",
      maxViewers: 8,
      endpointMediaCopyCapacity: 2,
      hostOnline: true,
      hostPaused: true,
      connectionId: null,
      ...routeAuthenticated("share_generation_12345678"),
      iceConfig: { iceServers: [], natPredictionStunUrls: [] },
      codeEntryPolicy: "open",
      viewerPasswordEnabled: false,
      viewerAuthorizationGeneration: "viewer_generation_12345678",
    });
    expect(signal.setSharingPaused(false)).toBe(true);
    expect(
      JSON.parse(String(sockets[1]!.send.mock.calls.at(-1)![0])),
    ).toMatchObject({
      type: "set-sharing-paused",
      shareGeneration: "share_generation_12345678",
      paused: false,
    });

    reconnect(sockets[1]!);
    sockets[2]!.dispatchEvent(new Event("open"));
    expect(JSON.parse(String(sockets[2]!.send.mock.calls[0]![0]))).toMatchObject({
      type: "authenticate",
      sharingPaused: false,
    });
    signal.stop();
  });

  it("replays the latest exact Host quality intent after reauthentication", () => {
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
      location: new URL("https://share.test/"),
      setTimeout: globalThis.setTimeout,
      clearTimeout: globalThis.clearTimeout,
    });
    const shareGeneration = "share_generation_12345678";
    const initialQuality = {
      resolution: { width: 1920, height: 1080 },
      maxFramerate: 30,
      maxBitrate: 5_000_000,
      degradationPreference: "balanced",
      maxAudioBitrate: 128000,
    } as const;
    const firstPending = {
      ...initialQuality,
      resolution: { width: 1280, height: 720 },
      maxBitrate: 3_000_000,
    } as const;
    const latestPending = {
      ...firstPending,
      maxFramerate: 60,
      maxBitrate: 8_000_000,
    } as const;
    let authenticatedCount = 0;
    let pendingDuringReauthentication: typeof latestPending | null = null;
    let signal!: SignalingClient;
    signal = new SignalingClient(
      {
        roomId: "1234",
        role: "host",
        token: "h".repeat(43),
        clientId: "host-client",
        routePolicy: DEFAULT_ROUTE_POLICY,
        shareGeneration,
        qualitySettings: initialQuality,
      },
      {
        onMessage: (message) => {
          if (message.type !== "authenticated") {
            return;
          }
          authenticatedCount += 1;
          if (authenticatedCount === 2) {
            pendingDuringReauthentication =
              signal.pendingHostQualitySettings(shareGeneration) as
                | typeof latestPending
                | null;
          }
        },
        onStatus: () => undefined,
        onTerminated: () => undefined,
        onAccessRequired: () => undefined,
      },
    );
    const receiveAuthenticated = (
      socket: FakeWebSocket,
      qualitySettings: QualitySettings = initialQuality,
    ) => {
      const event = new Event("message");
      Object.defineProperty(event, "data", {
        value: JSON.stringify({
          type: "authenticated",
          protocol: SIGNALING_PROTOCOL,
          role: "host",
          peerId: "host_12345678",
          maxViewers: 8,
          endpointMediaCopyCapacity: 2,
          hostOnline: true,
          hostPaused: false,
          connectionId: null,
          iceConfig: { iceServers: [], natPredictionStunUrls: [] },
          codeEntryPolicy: "open",
          viewerPasswordEnabled: false,
          viewerAuthorizationGeneration: "viewer_generation_12345678",
          mediaMode: "peer-assisted",
          shareGeneration,
          routeRevision: 1,
          routeAssignment: {
            upstream: { kind: "none" },
            childPeerIds: [],
            sfuPublicationGeneration: null,
          },
          qualitySettings,
          routePolicy: DEFAULT_ROUTE_POLICY,
        }),
      });
      socket.dispatchEvent(event);
    };

    signal.start();
    sockets[0]!.dispatchEvent(new Event("open"));
    receiveAuthenticated(sockets[0]!);
    const close = new Event("close");
    Object.defineProperties(close, {
      code: { value: 1006 },
      reason: { value: "network interrupted" },
    });
    sockets[0]!.dispatchEvent(close);

    expect(signal.setHostQualitySettings(firstPending)).toBe(false);
    expect(signal.setHostQualitySettings(latestPending)).toBe(false);
    expect(
      signal.pendingHostQualitySettings("other_share_generation_12345678"),
    ).toBeNull();
    expect(signal.pendingHostQualitySettings(shareGeneration)).toEqual(
      latestPending,
    );

    vi.advanceTimersByTime(750);
    sockets[1]!.dispatchEvent(new Event("open"));
    expect(JSON.parse(String(sockets[1]!.send.mock.calls[0]![0]))).toMatchObject({
      type: "authenticate",
      shareGeneration,
      qualitySettings: latestPending,
    });
    receiveAuthenticated(sockets[1]!);

    expect(pendingDuringReauthentication).toEqual(latestPending);
    // Reauthentication first publishes the share baseline, then the pending intent.
    expect(sockets[1]!.send).toHaveBeenCalledTimes(3);
    expect(
      JSON.parse(String(sockets[1]!.send.mock.calls[2]![0])),
    ).toEqual({
      type: "set-quality-settings",
      qualitySettings: latestPending,
    });
    expect(signal.pendingHostQualitySettings(shareGeneration)).toEqual(
      latestPending,
    );

    receiveAuthenticated(sockets[1]!, latestPending);
    // The acknowledged intent is not re-sent; the share baseline is.
    expect(sockets[1]!.send.mock.calls
      .map(([value]) => (JSON.parse(String(value)) as { type: string }).type))
      .toEqual(["authenticate", "set-co-play-presence", "set-quality-settings", "set-co-play-presence"]);
    expect(signal.pendingHostQualitySettings(shareGeneration)).toBeNull();
    signal.stop();
  });

  it("confirms an exact silent partition before replacing the socket immediately", () => {
    vi.useFakeTimers();
    let now = 0;
    const sockets: FakeWebSocket[] = [];
    class FakeWebSocket extends EventTarget {
      static readonly OPEN = 1;
      static readonly CLOSING = 2;
      readyState = FakeWebSocket.OPEN;
      readonly send = vi.fn();
      readonly close = vi.fn((code?: number, reason?: string) => {
        void code;
        void reason;
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
    const statuses: string[] = [];
    const onMessage = vi.fn();
    const onTerminated = vi.fn();
    const signal = new SignalingClient(
      {
        roomId: "1234",
        role: "host",
        token: "h".repeat(43),
        clientId: "host-client",
        routePolicy: DEFAULT_ROUTE_POLICY,
      },
      {
        onMessage,
        onStatus: (status) => statuses.push(status),
        onTerminated,
        onAccessRequired: () => undefined,
      },
      () => now,
    );
    const receive = (socket: FakeWebSocket, value: object) => {
      const event = new Event("message");
      Object.defineProperty(event, "data", { value: JSON.stringify(value) });
      socket.dispatchEvent(event);
    };
    const authenticate = (socket: FakeWebSocket) => {
      socket.dispatchEvent(new Event("open"));
      receive(socket, {
        type: "authenticated",
        protocol: SIGNALING_PROTOCOL,
        role: "host",
        peerId: "host_12345678",
        maxViewers: 8,
        endpointMediaCopyCapacity: 2,
        hostOnline: true,
        connectionId: null,
        ...routeAuthenticated(),
        iceConfig: { iceServers: [], natPredictionStunUrls: [] },
        codeEntryPolicy: "open",
        viewerPasswordEnabled: false,
        viewerAuthorizationGeneration: "viewer_generation_12345678",
      });
    };
    const advance = (milliseconds: number) => {
      now += milliseconds;
      vi.advanceTimersByTime(milliseconds);
    };

    signal.start();
    authenticate(sockets[0]!);
    advance(5_000);
    const first = JSON.parse(String(sockets[0]!.send.mock.calls.at(-1)![0]));
    expect(first).toEqual({ type: "signaling-challenge", sequence: 1 });

    advance(2_000);
    const confirm = JSON.parse(String(sockets[0]!.send.mock.calls.at(-1)![0]));
    expect(confirm).toEqual({ type: "signaling-challenge", sequence: 2 });
    receive(sockets[0]!, {
      type: "signaling-challenge-response",
      sequence: first.sequence,
    });
    advance(2_000);

    expect(sockets).toHaveLength(1);
    expect(sockets[0]!.close).not.toHaveBeenCalled();
    expect(statuses.at(-1)).toBe("connected");
    expect(onTerminated).not.toHaveBeenCalled();
    expect(
      sockets[0]!.send.mock.calls
        .map(([value]) => JSON.parse(String(value)).type)
        .includes("route-failed"),
    ).toBe(false);

    advance(3_000);
    const next = JSON.parse(String(sockets[0]!.send.mock.calls.at(-1)![0]));
    expect(next).toEqual({ type: "signaling-challenge", sequence: 3 });
    receive(sockets[0]!, {
      type: "signaling-challenge-response",
      sequence: next.sequence,
    });
    advance(2_000);

    expect(sockets).toHaveLength(1);
    expect(confirm.sequence).toBe(2);
    expect(onMessage).toHaveBeenCalledTimes(1);
    signal.stop();
  });

  it("rebases inactive, hidden, and obviously delayed challenge windows", () => {
    vi.useFakeTimers();
    let now = 0;
    let visibilityState: DocumentVisibilityState = "visible";
    const documentTarget = new EventTarget();
    Object.defineProperty(documentTarget, "visibilityState", {
      get: () => visibilityState,
    });
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
    vi.stubGlobal("document", documentTarget);
    vi.stubGlobal("window", {
      location: new URL("https://share.test/r/1234"),
      setTimeout: globalThis.setTimeout,
      clearTimeout: globalThis.clearTimeout,
    });
    const onMessage = vi.fn();
    const signal = new SignalingClient(
      {
        roomId: "1234",
        role: "viewer",
        clientId: "viewer-client",
      },
      {
        onMessage,
        onStatus: () => undefined,
        onTerminated: () => undefined,
        onAccessRequired: () => undefined,
      },
      () => now,
    );
    const receive = (value: object) => {
      const event = new Event("message");
      Object.defineProperty(event, "data", { value: JSON.stringify(value) });
      sockets[0]!.dispatchEvent(event);
    };
    const advance = (milliseconds: number) => {
      now += milliseconds;
      vi.advanceTimersByTime(milliseconds);
    };

    signal.start();
    sockets[0]!.dispatchEvent(new Event("open"));
    receive({
      type: "authenticated",
      protocol: SIGNALING_PROTOCOL,
      role: "viewer",
      peerId: "viewer_12345678",
      maxViewers: 8,
      endpointMediaCopyCapacity: 2,
      hostOnline: true,
      connectionId: "connection_12345678",
      iceConfig: { iceServers: [], natPredictionStunUrls: [] },
      codeEntryPolicy: "open",
      viewerAuthorizationGeneration: "viewer_generation_12345678",
      mediaMode: "peer-assisted",
      shareGeneration: "share_generation_12345678",
      routeRevision: 1,
      routeAssignment: {
        upstream: { kind: "none" },
        childPeerIds: [],
        sfuPublicationGeneration: null,
      },
      qualitySettings: {
        resolution: { width: 1920, height: 1080 },
        maxFramerate: 30,
        maxBitrate: 5_000_000,
        degradationPreference: "balanced",
        maxAudioBitrate: 128000,
      },
      routePolicy: DEFAULT_ROUTE_POLICY,
    });
    advance(5_000);
    expect(sockets[0]!.send).toHaveBeenCalledTimes(2);
    const waiting = JSON.parse(String(sockets[0]!.send.mock.calls.at(-1)![0]));
    expect(waiting.type).toBe("signaling-challenge");
    receive({
      type: "signaling-challenge-response",
      sequence: waiting.sequence,
    });

    receive({
      type: "route-update",
      revision: 2,
      phase: "active",
      assignment: {
        upstream: { kind: "peer", peerId: "host_12345678" },
        childPeerIds: [],
        sfuPublicationGeneration: null,
      },
    });
    advance(5_000);
    const first = JSON.parse(String(sockets[0]!.send.mock.calls.at(-1)![0]));
    expect(first.type).toBe("signaling-challenge");

    visibilityState = "hidden";
    documentTarget.dispatchEvent(new Event("visibilitychange"));
    advance(20_000);
    expect(sockets).toHaveLength(1);
    expect(sockets[0]!.send).toHaveBeenCalledTimes(3);

    visibilityState = "visible";
    documentTarget.dispatchEvent(new Event("visibilitychange"));
    advance(5_000);
    const visible = JSON.parse(String(sockets[0]!.send.mock.calls.at(-1)![0]));
    receive({
      type: "signaling-challenge-response",
      sequence: visible.sequence,
    });
    now += 7_000;
    vi.advanceTimersByTime(5_000);
    expect(sockets[0]!.send).toHaveBeenCalledTimes(4);
    advance(5_000);
    expect(sockets[0]!.send).toHaveBeenCalledTimes(5);
    expect(onMessage).toHaveBeenCalledTimes(2);
    signal.stop();
  });

  it.each([
    JSON.stringify({
      type: "error",
      code: "INVALID_MESSAGE",
      message: "Message is invalid",
    }),
    JSON.stringify({
      type: "authenticated",
      role: "viewer",
      peerId: "viewer_12345678",
      maxViewers: 8,
      hostOnline: true,
      connectionId: null,
      ...routeAuthenticated(),
      iceConfig: { iceServers: [], natPredictionStunUrls: [] },
    }),
  ])(
    "terminates once when a server payload is incompatible with the current wire",
    (payload) => {
      const sockets: FakeWebSocket[] = [];
      class FakeWebSocket extends EventTarget {
        static readonly CLOSING = 2;
        readyState = 1;
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
      expect(
        JSON.parse(String(sockets[0]!.send.mock.calls[0]![0])),
      ).toMatchObject({
        type: "authenticate",
        protocol: SIGNALING_PROTOCOL,
      });
      const message = new Event("message");
      Object.defineProperty(message, "data", { value: payload });
      sockets[0]!.dispatchEvent(message);
      const close = new Event("close");
      Object.defineProperties(close, {
        code: { value: 1008 },
        reason: { value: "Invalid message" },
      });
      sockets[0]!.dispatchEvent(close);

      expect(onTerminated).toHaveBeenCalledOnce();
      expect(onTerminated).toHaveBeenCalledWith("STALE_CLIENT");
      expect(sockets[0]!.close).toHaveBeenCalledOnce();
      expect(sockets).toHaveLength(1);
    },
  );

  it("returns a Host to the admission gate after confirmed site-access denial", async () => {
    vi.useFakeTimers();
    vi.stubGlobal("fetch", vi.fn(async () => Response.json({ required: true, authenticated: false })));
    const sockets: FakeWebSocket[] = [];
    class FakeWebSocket extends EventTarget {
      static readonly CLOSING = 2;
      readyState = 1;
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
    const onAccessRequired = vi.fn();
    const statuses: string[] = [];
    const onMessage = vi.fn();
    const signal = new SignalingClient(
      {
        roomId: "1234",
        role: "host",
        token: "h".repeat(43),
        clientId: "host-client",
        routePolicy: DEFAULT_ROUTE_POLICY,
      },
      {
        onMessage,
        onStatus: (status) => statuses.push(status),
        onTerminated: () => undefined,
        onAccessRequired,
      },
    );

    signal.start();
    sockets[0]!.dispatchEvent(new Event("open"));
    const message = new Event("message");
    Object.defineProperties(message, {
      data: {
        value: JSON.stringify({
          type: "error",
          code: "AUTH_REQUIRED",
          message: "Site access is required",
        }),
      },
    });
    sockets[0]!.dispatchEvent(message);
    await vi.advanceTimersByTimeAsync(0);

    expect(onAccessRequired).toHaveBeenCalledOnce();
    expect(onMessage).not.toHaveBeenCalled();
    expect(statuses.at(-1)).toBe("offline");
    expect(sockets).toHaveLength(1);
  });
});
