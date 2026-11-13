import { afterEach, describe, expect, it, vi } from "vitest";
import { z } from "zod";
import { discoverNativeConnect, discoverNativeHealth, NativeClient, NativeCompatibilityError, notifyNativePresentation } from "../../src/client/native/client";
import { DEFAULT_QUALITY_SETTINGS } from "../../src/shared/protocol";
import { AppQualityPreference } from "../../src/client/native/app-quality-preference";
import { NATIVE_CLIENT_PORT_START, NATIVE_CLIENT_PORT_END } from "../../src/client/native/discovery-ports";
import { nativeEventSchema, nativeHealthSchema, nativeRelayCapacity, NATIVE_CLIENT_PROTOCOL, requestFailedResponseSchema } from "../../src/client/native/wire";
import { restoreNativeMocks } from "./support.ts";

afterEach(restoreNativeMocks);
describe("native App private wire", () => {
  it("preserves the published strict failure reader for requests without cancellation opt-in", () => {
    const published = z.object({ version: z.literal(10), id: z.string(), type: z.literal("request-failed"), code: z.literal("operation-failed") }).strict();
    const legacy = { version: 10, id: "request_start", type: "request-failed", code: "operation-failed" };
    expect(published.safeParse(legacy).success).toBe(true);
    expect(requestFailedResponseSchema.safeParse(legacy).success).toBe(true);
    const optedIn = { ...legacy, code: "source-canceled" };
    expect(published.safeParse(optedIn).success).toBe(false);
    expect(requestFailedResponseSchema.safeParse(optedIn).success).toBe(true);
  });

  const health = {
    protocol: NATIVE_CLIENT_PROTOCOL,
    service: "piik-client",
    port: NATIVE_CLIENT_PORT_START,
    instanceToken: "a".repeat(43),
    nativeMedia: {
      video: true, hardwareH264: true,
      receiverReuse: false, remoteCoPlay: false, shareVolume: false,
      sourceCancellation: false, captureFailureDetails: false, captureBorderControl: false,
    },
  };

  it("discovers with caller cancellation when AbortSignal.any is unavailable", async () => {
    vi.spyOn(AbortSignal, "any").mockImplementation(() => { throw new TypeError("AbortSignal.any is not a function"); });
    vi.stubGlobal("window", { setTimeout, clearTimeout, location: new URL("http://127.0.0.1:8787/") });
    const fetcher = vi.fn(async (url: string) => {
      if (new URL(url).port !== String(health.port)) throw new TypeError("No listener");
      return new Response(JSON.stringify(health));
    });
    vi.stubGlobal("fetch", fetcher);
    await expect(discoverNativeHealth({ signal: new AbortController().signal })).resolves.toMatchObject({ port: health.port });
    expect(fetcher).toHaveBeenCalled();
  });

  it.each(["localhost", "127.0.0.1", "[::1]"])("discovers and updates presentation from %s without an address-space hint", async hostname => {
    vi.stubGlobal("window", { setTimeout, clearTimeout, location: new URL(`http://${hostname}:8787/`) });
    const query = vi.fn().mockResolvedValue({ state: "prompt" });
    vi.stubGlobal("navigator", { permissions: { query } });
    const fetcher = vi.fn((url: string, init: RequestInit) => {
      if ("targetAddressSpace" in init) throw new TypeError("loopback is not a valid IPAddressSpace enum value");
      return Promise.resolve(new Response(JSON.stringify(url.endsWith("quality-preference") ? DEFAULT_QUALITY_SETTINGS
        : url.endsWith("connect-discovery") ? { ...health, connectURL: "http://127.0.0.1:54321/client" }
          : health)));
    });
    vi.stubGlobal("fetch", fetcher);
    await expect(discoverNativeHealth({ waitForPermission: false })).resolves.toMatchObject({ port: health.port });
    await expect(discoverNativeConnect()).resolves.toContain("http://127.0.0.1:54321/client?");
    await notifyNativePresentation("en", new AbortController().signal);
    const preference = await AppQualityPreference.connect(new AbortController().signal);
    await expect(preference!.read()).resolves.toEqual(DEFAULT_QUALITY_SETTINGS);
    expect(query).not.toHaveBeenCalled();
    expect(fetcher).toHaveBeenCalledWith(`http://127.0.0.1:${health.port}/presentation`, expect.objectContaining({ method: "POST" }));
    expect(fetcher.mock.calls.every(([, init]) => !("targetAddressSpace" in init))).toBe(true);
  });

  it("retains the address-space hint for hosted discovery and presentation without a no-hint fallback", async () => {
    vi.stubGlobal("window", { setTimeout, clearTimeout, location: new URL("https://piik.example/") });
    vi.stubGlobal("navigator", { permissions: { query: vi.fn().mockResolvedValue({ state: "granted" }) } });
    const fetcher = vi.fn(async (url: string, _init: RequestInit) => new Response(JSON.stringify(
      url.endsWith("quality-preference") ? DEFAULT_QUALITY_SETTINGS : health)));
    vi.stubGlobal("fetch", fetcher);
    await notifyNativePresentation("en", new AbortController().signal);
    const preference = await AppQualityPreference.connect(new AbortController().signal);
    await expect(preference!.read()).resolves.toEqual(DEFAULT_QUALITY_SETTINGS);
    expect(fetcher.mock.calls.every(call => (call[1] as RequestInit & { targetAddressSpace: string }).targetAddressSpace === "loopback")).toBe(true);
    expect(fetcher).toHaveBeenCalledWith(`http://127.0.0.1:${health.port}/presentation`, expect.objectContaining({ targetAddressSpace: "loopback" }));
    fetcher.mockImplementation(() => { throw new TypeError("Unsupported IPAddressSpace"); });
    fetcher.mockClear();
    await expect(discoverNativeHealth()).resolves.toBeNull();
    expect(fetcher).toHaveBeenCalledTimes(NATIVE_CLIENT_PORT_END - NATIVE_CLIENT_PORT_START + 1);
    expect(fetcher.mock.calls.every(call => (call[1] as RequestInit & { targetAddressSpace: string }).targetAddressSpace === "loopback")).toBe(true);
  });

  it("requires the controlled App platform in remote-control state", () => {
    const event = { version: NATIVE_CLIENT_PROTOCOL, type: "remote-control-state", shareId: "share_123456", mode: "off", requests: [] };
    expect(nativeEventSchema.safeParse(event).success).toBe(false);
    expect(nativeEventSchema.parse({ ...event, platform: "windows" })).toMatchObject({ platform: "windows" });
    for (const platform of ["darwin", "linux", "unknown"]) {
      expect(nativeEventSchema.safeParse({ ...event, platform }).success).toBe(false);
    }
  });

  it.each(["prompt", "denied"])("background App preferences do not request local access when permission is %s", async (state) => {
    vi.stubGlobal("window", { setTimeout, clearTimeout, location: new URL("https://piik.example/") });
    vi.stubGlobal("navigator", { permissions: { query: vi.fn().mockResolvedValue({ state }) } });
    const fetcher = vi.fn(async () => new Response(JSON.stringify(health)));
    vi.stubGlobal("fetch", fetcher);
    const controller = new AbortController();
    await notifyNativePresentation("en", controller.signal);
    expect(await AppQualityPreference.connect(controller.signal)).toBeNull();
    expect(fetcher).not.toHaveBeenCalled();
  });

  it("discovers only a local approval page without opening native control", async () => {
    vi.stubGlobal("window", { setTimeout, clearTimeout, location: new URL("https://piik.example/r/9527") });
    const socket = vi.fn();
    vi.stubGlobal("WebSocket", socket);
    const fetcher = vi.fn(async (url: string) => url === `http://127.0.0.1:${health.port}/connect-discovery`
      ? new Response(JSON.stringify({ ...health, connectURL: "http://127.0.0.1:54321/client" }))
      : new Response(null, { status: 404 }));
    vi.stubGlobal("fetch", fetcher);
    await expect(discoverNativeConnect()).resolves.toBe("http://127.0.0.1:54321/client?site=https%3A%2F%2Fpiik.example");
    expect(fetcher.mock.calls.every(([url]) => url.endsWith("/connect-discovery"))).toBe(true);
    expect(socket).not.toHaveBeenCalled();
  });

  it.each([
    "https://untrusted.example/client", "http://localhost:54321/client", "http://127.0.0.1:54321/other",
    "http://user@127.0.0.1:54321/client", "http://127.0.0.1:54321/client?site=https://other.example",
  ])("rejects an unsafe App approval address: %s", async (connectURL) => {
    vi.stubGlobal("window", { setTimeout, clearTimeout, location: new URL("https://piik.example/") });
    vi.stubGlobal("fetch", vi.fn(async (url: string) => url === `http://127.0.0.1:${health.port}/connect-discovery`
      ? new Response(JSON.stringify({ ...health, connectURL })) : new Response(null, { status: 404 })));
    await expect(discoverNativeConnect()).resolves.toBeNull();
  });

  it.each([false, true])("cancels the whole approval scan with its caller (permission prompt: %s)", async prompting => {
    vi.spyOn(AbortSignal, "any").mockImplementation(() => { throw new TypeError("AbortSignal.any is not a function"); });
    vi.stubGlobal("window", { setTimeout, clearTimeout, location: new URL("https://piik.example/") });
    if (prompting) vi.stubGlobal("navigator", { permissions: {
      query: vi.fn().mockResolvedValue(Object.assign(new EventTarget(), { state: "prompt" })),
    } });
    const signals: AbortSignal[] = [];
    vi.stubGlobal("fetch", vi.fn((_url: string, init: RequestInit) => new Promise((_resolve, reject) => {
      const signal = init.signal!;
      signals.push(signal);
      signal.addEventListener("abort", () => reject(signal.reason), { once: true });
    })));
    const controller = new AbortController();
    const result = discoverNativeConnect(controller.signal);
    await vi.waitFor(() => expect(signals).toHaveLength(NATIVE_CLIENT_PORT_END - NATIVE_CLIENT_PORT_START + 1));
    controller.abort();
    await expect(result).resolves.toBeNull();
    expect(signals).toHaveLength(NATIVE_CLIENT_PORT_END - NATIVE_CLIENT_PORT_START + 1);
    expect(signals.every(signal => signal.aborted)).toBe(true);
  });

  it("reads numeric video capability descriptions and rejects malformed limits", () => {
    const nativeMedia = { ...health.nativeMedia, maxVideoResolution: { width: 3840, height: 3840 },
      maxVideoPixels: 3840 * 2160, maxVideoMacroblocksPerSecond: 983_040 };
    expect(nativeHealthSchema.parse({ ...health, nativeMedia }).nativeMedia).toMatchObject(nativeMedia);
    const stronger = { ...nativeMedia, maxVideoResolution: { width: 32768, height: 32768 },
      maxVideoPixels: 32768 ** 2, maxVideoFramerate: 120, maxVideoBitrate: 100_000_000,
      maxAudioBitrate: 960_000, maxRelayChildren: 32 };
    const strongerHealth = nativeHealthSchema.parse({ ...health, nativeMedia: stronger });
    expect(strongerHealth.nativeMedia).toMatchObject(stronger);
    expect(nativeRelayCapacity(20, strongerHealth)).toBe(20);
    expect(nativeHealthSchema.safeParse({ ...health, nativeMedia: { ...nativeMedia, maxVideoPixels: 0 } }).success).toBe(false);
    expect(nativeHealthSchema.safeParse({ ...health, nativeMedia: { ...nativeMedia, maxVideoMacroblocksPerSecond: -1 } }).success).toBe(false);
    expect(nativeHealthSchema.parse({ ...health, nativeMedia: { ...nativeMedia, maxRelayChildren: 20 } })
      .nativeMedia.maxRelayChildren).toBe(20);
    for (const maxRelayChildren of [0, 0x1_0000_0000, 1.5]) {
      expect(nativeHealthSchema.safeParse({ ...health, nativeMedia: { ...nativeMedia, maxRelayChildren } }).success).toBe(false);
    }
  });

  it.each(["opening", "hello", "close"])("retires App acquisition during %s without waiting for its timeout", async stage => {
    vi.useFakeTimers();
    vi.stubGlobal("window", { setTimeout, clearTimeout, location: new URL("https://piik.example/") });
    vi.stubGlobal("fetch", vi.fn(async () => new Response(JSON.stringify(health))));
    const sockets: Socket[] = [];
    class Socket extends EventTarget {
      static readonly OPEN = 1;
      static readonly CLOSING = 2;
      readyState = 0;
      protocol = `piik-client-v10.${health.instanceToken}`;
      close = vi.fn(() => { this.readyState = Socket.CLOSING; });
      send = vi.fn();
      constructor() { super(); sockets.push(this); }
    }
    vi.stubGlobal("WebSocket", Socket);
    const controller = new AbortController();
    const settled = vi.fn();
    const connecting = NativeClient.connect({ signal: controller.signal }).then(settled);
    await vi.advanceTimersByTimeAsync(0);
    const socket = sockets[0]!;
    expect(socket).toBeDefined();
    if (stage === "hello") {
      socket.readyState = Socket.OPEN;
      socket.dispatchEvent(new Event("open"));
      await vi.advanceTimersByTimeAsync(0);
      expect(socket.send).toHaveBeenCalledOnce();
    }
    if (stage === "close") {
      socket.readyState = 3;
      socket.dispatchEvent(new Event("close"));
    } else controller.abort();
    await vi.advanceTimersByTimeAsync(0);
    expect(settled).toHaveBeenCalledExactlyOnceWith(null);
    expect(vi.getTimerCount()).toBe(0);
    if (stage !== "close") expect(socket.close).toHaveBeenCalledOnce();
    // An obsolete opening event cannot start a hello or revive acquisition.
    socket.dispatchEvent(new Event("open"));
    await vi.advanceTimersByTimeAsync(0);
    expect(socket.send).toHaveBeenCalledTimes(stage === "hello" ? 1 : 0);
    await connecting;
  });

  it("binds a local answer to its prepare request across an ICE restart", async () => {
    const requests: Record<string, unknown>[] = [];
    class Socket extends EventTarget {
      static readonly OPEN = 1;
      static readonly CLOSING = 2;
      readyState = Socket.OPEN;
      protocol = `piik-client-v10.${health.instanceToken}`;
      constructor() { super(); queueMicrotask(() => this.dispatchEvent(new Event("open"))); }
      close() { this.readyState = Socket.CLOSING; }
      send(payload: string) {
        const request = JSON.parse(payload) as Record<string, unknown>;
        requests.push(request);
        const fields = request.type === "hello" ? { type: "ready" }
          : request.type === "prepare-local-edge" ? {
            type: "edge-offer", shareId: request.shareId, connectionId: request.connectionId, sdp: "v=0\r\n",
          } : { type: "edge-answer-accepted" };
        queueMicrotask(() => this.dispatchEvent(new MessageEvent("message", {
          data: JSON.stringify({ version: 10, id: request.id, ...fields }),
        })));
      }
    }
    vi.stubGlobal("WebSocket", Socket);
    vi.stubGlobal("window", { setTimeout, clearTimeout, location: new URL("https://piik.example/") });
    vi.stubGlobal("fetch", vi.fn(async () => new Response(JSON.stringify(health))));
    const controller = new AbortController();
    const client = (await NativeClient.connect({ signal: controller.signal }))!;
    controller.abort(); // Acquisition cancellation no longer owns this accepted client.
    try {
      let lastOffer = "";
      for (const restart of [false, true]) {
        const offer = await client.prepareLocalEdge("share_123456", "local_123456", "source_123456", restart);
        expect(offer.offerId).toBe(requests.at(-1)?.id);
        expect(offer.offerId).not.toBe(lastOffer);
        expect(requests.at(-1)).toMatchObject({ type: "prepare-local-edge", restart, sourceConnectionId: "source_123456" });
        await client.acceptSignal("share_123456", "local_123456", {
          kind: "description", connectionId: "local_123456", description: { type: "answer", sdp: "v=0\r\n" },
        }, offer.offerId);
        expect(requests.at(-1)).toMatchObject({ type: "edge-answer", offerId: offer.offerId });
        lastOffer = offer.offerId;
      }
    } finally { client.close(); }
  });

  it.each(["prompt", "denied"] as const)("does not hold Viewer reception for %s permission and observes a later grant", async (state) => {
    vi.stubGlobal("window", { setTimeout, clearTimeout, location: { hostname: "piik.example" } });
    const query = vi.fn().mockResolvedValue({ state });
    vi.stubGlobal("navigator", { permissions: { query } });
    const fetcher = vi.fn(async () => new Response(JSON.stringify(health)));
    vi.stubGlobal("fetch", fetcher);
    const socket = vi.fn();
    vi.stubGlobal("WebSocket", socket);

    await expect(NativeClient.connect({ waitForPermission: false })).resolves.toBeNull();
    expect(fetcher).not.toHaveBeenCalled();
    expect(socket).not.toHaveBeenCalled();
    query.mockResolvedValue({ state: "granted" });
    await expect(discoverNativeHealth({ waitForPermission: false })).resolves.toMatchObject({ port: health.port });
  });

  it("still lets explicit source discovery ask for local permission", async () => {
    vi.stubGlobal("window", { setTimeout, clearTimeout, location: { hostname: "piik.example" } });
    const query = vi.fn().mockResolvedValue(Object.assign(new EventTarget(), { state: "prompt" }));
    vi.stubGlobal("navigator", { permissions: { query } });
    vi.stubGlobal("fetch", vi.fn(async () => new Response(JSON.stringify(health))));
    await expect(discoverNativeHealth()).resolves.toMatchObject({ port: health.port });
    expect(query).toHaveBeenCalledOnce();
  });

  it.each(["localhost", "127.0.0.1", "[::1]"])("keeps App Local reception on %s available without cross-address-space consent", async (hostname) => {
    vi.stubGlobal("window", { setTimeout, clearTimeout, location: { hostname } });
    const query = vi.fn().mockResolvedValue({ state: "prompt" });
    vi.stubGlobal("navigator", { permissions: { query } });
    vi.stubGlobal("fetch", vi.fn(async () => new Response(JSON.stringify(health))));
    await expect(discoverNativeHealth({ waitForPermission: false })).resolves.toMatchObject({ port: health.port });
    expect(query).not.toHaveBeenCalled();
  });

  it("uses the older combined permission when split permission is unsupported", async () => {
    vi.stubGlobal("window", { setTimeout, clearTimeout, location: { hostname: "piik.example" } });
    const query = vi.fn().mockRejectedValueOnce(new TypeError("Unsupported permission"))
      .mockResolvedValue({ state: "prompt" });
    vi.stubGlobal("navigator", { permissions: { query } });
    const fetcher = vi.fn();
    vi.stubGlobal("fetch", fetcher);
    await expect(discoverNativeHealth({ waitForPermission: false })).resolves.toBeNull();
    expect(query).toHaveBeenLastCalledWith({ name: "local-network-access" });
    expect(fetcher).not.toHaveBeenCalled();
  });

  it.each([undefined, { query: vi.fn().mockRejectedValue(new TypeError("Unsupported permission")) }])(
    "retains ordinary discovery when local permissions cannot be queried",
    async (permissions) => {
      vi.stubGlobal("window", { setTimeout, clearTimeout, location: { hostname: "piik.example" } });
      vi.stubGlobal("navigator", { permissions });
      vi.stubGlobal("fetch", vi.fn(async () => new Response(JSON.stringify(health))));
      await expect(discoverNativeHealth({ waitForPermission: false })).resolves.toMatchObject({ port: health.port });
    },
  );

  it.each([NATIVE_CLIENT_PROTOCOL - 1, NATIVE_CLIENT_PROTOCOL + 1])(
    "reports observed Piik App protocol %s without opening control or presentation",
    async (protocol) => {
      vi.stubGlobal("window", { setTimeout, clearTimeout, location: new URL("http://127.0.0.1/") });
      const socket = vi.fn();
      vi.stubGlobal("WebSocket", socket);
      const fetcher = vi.fn(async (url: string) => url === `http://127.0.0.1:${health.port}/health`
        ? new Response(JSON.stringify({ ...health, protocol }))
        : new Response(null, { status: 403 }));
      vi.stubGlobal("fetch", fetcher);
      await expect(NativeClient.connect()).rejects.toMatchObject({
        name: "NativeCompatibilityError", actualProtocol: protocol,
      });
      await expect(notifyNativePresentation("en", new AbortController().signal))
        .rejects.toBeInstanceOf(NativeCompatibilityError);
      expect(socket).not.toHaveBeenCalled();
      expect(fetcher.mock.calls.every(([url]) => url.endsWith("/health"))).toBe(true);
      expect(fetcher).toHaveBeenCalledTimes(2 * (NATIVE_CLIENT_PORT_END - NATIVE_CLIENT_PORT_START + 1));
    },
  );

  it("prefers a compatible App on a later port after observing an incompatible App", async () => {
    vi.stubGlobal("window", { setTimeout, clearTimeout, location: new URL("https://piik.example/") });
    const fetcher = vi.fn(async (url: string) => {
      const port = Number(new URL(url).port);
      return new Response(JSON.stringify({ ...health, port,
        protocol: port === health.port ? NATIVE_CLIENT_PROTOCOL + 1 : NATIVE_CLIENT_PROTOCOL,
      }));
    });
    vi.stubGlobal("fetch", fetcher);
    await expect(discoverNativeHealth()).resolves.toMatchObject({
      protocol: NATIVE_CLIENT_PROTOCOL, port: NATIVE_CLIENT_PORT_START + 1,
    });
    expect(fetcher).toHaveBeenCalledTimes(NATIVE_CLIENT_PORT_END - NATIVE_CLIENT_PORT_START + 1);
  });
});
