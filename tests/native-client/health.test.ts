import { afterEach, describe, expect, it, vi } from "vitest";
import { NativeClient } from "../../src/client/native/client";
import { DEFAULT_QUALITY_SETTINGS } from "../../src/shared/protocol";
import { NATIVE_CLIENT_PORT_END } from "../../src/client/native/discovery-ports";
import { nativeHealthSchema, NATIVE_CLIENT_PROTOCOL } from "../../src/client/native/wire";
import { restoreNativeMocks, health, nativeMedia } from "./support.ts";

afterEach(restoreNativeMocks);
describe("native App private wire", () => {
  it("accepts descriptive health extensions and treats absent probed media facts as unavailable", () => {
    expect(
      nativeHealthSchema.parse({
        protocol: 10,
        service: "piik-client",
        port: 39_721,
        instanceToken: "a".repeat(43),
        nativeMedia: {
          ...nativeMedia,
          video: true,
          processAudio: false,
          systemAudio: true,
          hardwareH264: true,
          softwareVP8: true,
        },
      }),
    ).toMatchObject({ nativeMedia: { processAudio: false } });
    expect(
      nativeHealthSchema.safeParse({
        protocol: NATIVE_CLIENT_PROTOCOL,
        service: "piik-client",
        port: 39_721,
        instanceToken: "a".repeat(43),
        nativeMedia: {
          ...nativeMedia,
          video: true,
          processAudio: true,
          systemAudio: true,
          hardwareH264: true,
          softwareVP8: false,
        },
        adapters: ["private"],
      }).success,
    ).toBe(true);
    expect(nativeHealthSchema.parse({
      ...health, futureDescription: "ignored", nativeMedia: { ...nativeMedia, video: true, futureFeature: true },
    })).toEqual({
      ...health, nativeMedia: {
        ...nativeMedia,
        video: true, processAudio: false, processAudioExclusion: false, systemAudio: false, microphone: false, hardwareH264: false, softwareVP8: false,
      },
    });
    expect(nativeHealthSchema.safeParse({ ...health, nativeMedia: undefined }).success).toBe(false);
    for (const invalid of [
      { protocol: 0 }, { protocol: 9.5 }, { protocol: Number.MAX_SAFE_INTEGER + 1 },
      { service: "other" }, { port: NATIVE_CLIENT_PORT_END + 1 }, { instanceToken: "short" },
      ...["receiverReuse", "remoteCoPlay", "shareVolume", "sourceCancellation", "captureFailureDetails", "captureBorderControl"]
        .map(field => ({ nativeMedia: { ...health.nativeMedia, [field]: undefined } })),
      { nativeMedia: { ...nativeMedia, softwareVP8: "true" } }, { nativeMedia: { ...nativeMedia, captureBorderControl: "true" } }, { nativeMedia: null },
    ]) expect(nativeHealthSchema.safeParse({ ...health, ...invalid }).success).toBe(false);
  });

  it.each([undefined, false, true])("gates capture border control on the probed helper: %s", async (supported) => {
    const requests: Record<string, unknown>[] = [];
    class CaptureSocket extends EventTarget {
      static current: CaptureSocket;
      static readonly OPEN = 1;
      static readonly CLOSING = 2;
      readyState = CaptureSocket.OPEN;
      protocol = `piik-client-v10.${health.instanceToken}`;
      constructor() {
        super();
        CaptureSocket.current = this;
        queueMicrotask(() => this.dispatchEvent(new Event("open")));
      }
      close() { this.readyState = CaptureSocket.CLOSING; }
      send(payload: string) {
        const request = JSON.parse(payload) as Record<string, unknown>;
        requests.push(request);
        queueMicrotask(() => this.dispatchEvent(new MessageEvent("message", {
          data: JSON.stringify({
            version: NATIVE_CLIENT_PROTOCOL, id: request.id,
            ...(request.type === "hello" ? { type: "ready" } : {
              type: request.type === "start-share" ? "share-started" : "share-source-replaced",
              shareId: request.shareId,
              ...(request.type === "start-share" ? { audio: true, codec: "h264" } : {}),
            }),
          }),
        })));
      }
    }
    vi.stubGlobal("WebSocket", CaptureSocket);
    vi.stubGlobal("window", { setTimeout, clearTimeout, location: new URL("https://piik.example/") });
    vi.stubGlobal("fetch", vi.fn(async () => new Response(JSON.stringify({
      ...health, nativeMedia: { ...health.nativeMedia, captureBorderControl: supported ?? false, remoteInput: supported, sourceCancellation: true, captureFailureDetails: true },
    }))));
    const client = await NativeClient.connect();
    expect(client).not.toBeNull();
    expect(client!.health.nativeMedia.captureBorderControl).toBe(supported ?? false);
    const input = {
      shareId: "share_123456", source: { kind: "display" as const, sourceId: "2", title: "Display 1" },
      audio: true, adapterIndex: 0, encoderIndex: 0, edgeCapacity: 2,
      profile: DEFAULT_QUALITY_SETTINGS, codec: "auto" as const,
    };
    try {
      for (const showCaptureBorder of [undefined, false, true]) {
        const onRequested = vi.fn(() => expect(requests.at(-1)?.type).toBe("start-share"));
        let acknowledged = false;
        const starting = client!.startShare({ ...input, showCaptureBorder }, onRequested);
        void starting.then(() => { acknowledged = true; });
        expect(onRequested).toHaveBeenCalledOnce();
        expect(acknowledged).toBe(false);
        await starting;
        await client!.replaceShareSource(input.shareId, input.source, input.audio, input, showCaptureBorder);
        const extension = { microphoneMixing: true };
        expect(requests.at(-2)).toEqual({
          version: NATIVE_CLIENT_PROTOCOL, id: expect.any(String), type: "start-share", ...input, ...extension,
          reportCaptureFailure: true,
          ...(supported ? { remoteInput: true } : {}),
          ...(supported ? { showCaptureBorder: showCaptureBorder ?? false } : {}),
          reportSourceCancellation: true,
        });
        expect(requests.at(-1)).toEqual({
          version: NATIVE_CLIENT_PROTOCOL, id: expect.any(String), type: "replace-share-source",
          shareId: input.shareId, source: input.source, audio: true, adapterIndex: 0, encoderIndex: 0,
          reportSourceCancellation: true,
          ...(supported ? { showCaptureBorder: showCaptureBorder ?? false } : {}),
        });
      }
      const onEvent = vi.fn(), onClose = vi.fn();
      client!.onEvent(onEvent);
      client!.onClose(onClose);
      const ended = { version: NATIVE_CLIENT_PROTOCOL, type: "share-ended", shareId: input.shareId, failed: true,
        failure: { stage: "windows-runtime", hresult: "0x887a0005", exitCode: 2 } };
      CaptureSocket.current.dispatchEvent(new MessageEvent("message", { data: JSON.stringify(ended) }));
      expect(onEvent).toHaveBeenCalledWith(ended);
      expect(onClose).not.toHaveBeenCalled();
      const unsent = vi.fn();
      vi.spyOn(CaptureSocket.prototype, "send").mockImplementationOnce(() => { throw new Error("send failed"); });
      await expect(client!.startShare(input, unsent)).rejects.toMatchObject({ reason: "send-failed" });
      expect(unsent).not.toHaveBeenCalled();
      client!.close();
      await expect(client!.startShare(input, unsent)).rejects.toMatchObject({ reason: "unavailable" });
      expect(unsent).not.toHaveBeenCalled();
    } finally { client!.close(); }
  });

  it.each([false, true])("never silently drops requested audio exclusion (%s)", async (supported) => {
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
        queueMicrotask(() => this.dispatchEvent(new MessageEvent("message", { data: JSON.stringify({
          version: NATIVE_CLIENT_PROTOCOL, id: request.id,
          ...(request.type === "hello" ? { type: "ready" } : {
            type: request.type === "start-share" ? "share-started" : "share-source-replaced", shareId: request.shareId,
            ...(request.type === "start-share" ? { audio: true, codec: "vp8", sourceAudio: true } : {}),
          }),
        }) })));
      }
    }
    vi.stubGlobal("WebSocket", Socket);
    vi.stubGlobal("window", { setTimeout, clearTimeout, location: new URL("https://piik.example/") });
    vi.stubGlobal("fetch", vi.fn(async () => new Response(JSON.stringify({ ...health,
      nativeMedia: { ...health.nativeMedia, microphone: true, ...(supported ? { processAudioExclusion: true } : {}) },
    }))));
    const client = (await NativeClient.connect())!;
    const input = { shareId: "share_123456", source: { kind: "display" as const, sourceId: "1", title: "Display" },
      audio: true, adapterIndex: 0, encoderIndex: 0, edgeCapacity: 1, profile: DEFAULT_QUALITY_SETTINGS, codec: "vp8" as const };
    const excludeAudio = { kind: "window" as const, sourceId: "2", pid: 123, creationTime: "456", title: "Voice fixture" };
    try {
      expect(client.health.nativeMedia.processAudioExclusion).toBe(supported);
      await client.startShare(input);
      expect(requests.at(-1)).not.toHaveProperty("excludeAudio");
      if (supported) {
        await client.startShare({ ...input, excludeAudio });
        await client.replaceShareSource(input.shareId, input.source, true, input, false, excludeAudio);
        expect(requests.at(-2)?.excludeAudio).toEqual(excludeAudio);
        expect(requests.at(-1)?.excludeAudio).toEqual(excludeAudio);
      } else {
        const count = requests.length;
        await expect(client.startShare({ ...input, excludeAudio })).rejects.toThrow("audio exclusion");
        await expect(client.replaceShareSource(input.shareId, input.source, true, input, false, excludeAudio)).rejects.toThrow("audio exclusion");
        expect(requests).toHaveLength(count);
      }
    } finally { client.close(); }
  });

  it.each([
    { microphone: false, shareVolume: false },
    { microphone: true, shareVolume: true },
    { microphone: false, shareVolume: true },
  ])("gates audio mixing and coalesces volume without delaying Stop (%j)", async ({ microphone, shareVolume }) => {
    const supported = microphone || shareVolume;
    const requests: Record<string, unknown>[] = [];
    let socket!: Socket;
    class Socket extends EventTarget {
      static readonly OPEN = 1;
      static readonly CLOSING = 2;
      readyState = Socket.OPEN;
      protocol = `piik-client-v10.${health.instanceToken}`;
      constructor() { super(); socket = this; queueMicrotask(() => this.dispatchEvent(new Event("open"))); }
      close() { this.readyState = Socket.CLOSING; }
      ack(request: Record<string, unknown>, type: string, fields = {}) {
        this.dispatchEvent(new MessageEvent("message", { data: JSON.stringify({ version: 10, id: request.id, type, ...fields }) }));
      }
      send(payload: string) {
        const request = JSON.parse(payload) as Record<string, unknown>;
        requests.push(request);
        if (request.type === "set-microphone" || request.type === "set-share-volume") return;
        if (request.type === "list-microphones") {
          queueMicrotask(() => this.ack(request, "microphone-list", { devices: [{ id: "headset", label: "USB Headset" }] }));
          return;
        }
        queueMicrotask(() => this.ack(request,
          request.type === "hello" ? "ready" : request.type === "start-share" ? "share-started" : "share-stopped",
          request.type === "start-share" ? { shareId: request.shareId, audio: supported, codec: "vp8", ...(supported ? { sourceAudio: false } : {}) } : {}));
      }
    }
    vi.stubGlobal("WebSocket", Socket);
    vi.stubGlobal("window", { setTimeout, clearTimeout, location: new URL("https://piik.example/") });
    vi.stubGlobal("fetch", vi.fn(async () => new Response(JSON.stringify({
      ...health, nativeMedia: { ...health.nativeMedia, microphone, shareVolume },
    }))));
    const client = (await NativeClient.connect())!;
    try {
      const started = await client.startShare({ shareId: "share_123456", audio: false,
        source: { kind: "display", sourceId: "1", title: "Screen" }, adapterIndex: 0, encoderIndex: 0,
        edgeCapacity: 1, profile: DEFAULT_QUALITY_SETTINGS, codec: "vp8" });
      expect(requests.at(-1)?.microphoneMixing).toBe(true);
      expect(started.audio).toBe(supported);
      if (!supported) {
        await expect(client.setMicrophone("share_123456", true, 1)).rejects.toThrow();
        await expect(client.setShareVolume("share_123456", 2)).rejects.toThrow();
        await expect(client.microphones()).rejects.toThrow();
        expect(requests.some(request => request.type === "set-microphone")).toBe(false);
        expect(requests.some(request => request.type === "set-share-volume")).toBe(false);
        return;
      }
      if (microphone) expect(await client.microphones()).toEqual([{ id: "headset", label: "USB Headset" }]);
      else await expect(client.microphones()).rejects.toThrow();
      const volume = client.setShareVolume("share_123456", .5);
      expect(client.setShareVolume("share_123456", 2)).toBe(volume);
      const shareVolumeRequest = requests.at(-1)!;
      const microphoneVolume = microphone ? client.setMicrophoneVolume("share_123456", .4) : null;
      if (microphone) expect(client.setMicrophoneVolume("share_123456", .9)).toBe(microphoneVolume);
      const microphoneVolumeRequest = requests.at(-1)!;
      socket.ack(shareVolumeRequest, "share-volume-set");
      await vi.waitFor(() => expect(requests.at(-1)).toMatchObject({ type: "set-share-volume", volume: 2 }));
      socket.ack(requests.at(-1)!, "share-volume-set");
      await volume;
      if (!microphone) {
        expect(requests.some(request => request.type === "set-microphone")).toBe(false);
        return;
      }
      socket.ack(microphoneVolumeRequest, "microphone-set");
      await vi.waitFor(() => expect(requests.at(-1)).toMatchObject({ type: "set-microphone", volume: .9 }));
      socket.ack(requests.at(-1)!, "microphone-set");
      await microphoneVolume;
      const choosing = client.setMicrophone("share_123456", true, 1, "headset");
      expect(requests.at(-1)).toMatchObject({ type: "set-microphone", deviceId: "headset", enabled: true });
      socket.ack(requests.at(-1)!, "microphone-set");
      await choosing;
      requests.length = 0;
      const pending = client.setMicrophoneVolume("share_123456", 0.1);
      for (let value = 2; value <= 20; value++) expect(client.setMicrophoneVolume("share_123456", value / 10)).toBe(pending);
      expect(requests.filter(request => request.type === "set-microphone")).toHaveLength(1);
      socket.ack(requests.at(-1)!, "microphone-set");
      await vi.waitFor(() => expect(requests.at(-1)?.volume).toBe(2));
      const last = requests.at(-1)!;
      expect(last).not.toHaveProperty("enabled");
      await client.stopShare("share_123456");
      socket.ack(last, "microphone-set");
      await pending;
      expect(requests.at(-1)?.type).toBe("stop-share");
    } finally { client.close(); }
  });

  it.each([false, true])("requests the App's reusable receiver (%s)", async (reused) => {
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
        queueMicrotask(() => this.dispatchEvent(new MessageEvent("message", { data: JSON.stringify({
          version: 10, id: request.id, ...(request.type === "hello" ? { type: "ready" } : {
            type: "receive-answer", shareId: request.shareId, connectionId: request.connectionId,
            sdp: "answer", audio: false, codec: "vp8", ...(reused ? { reused: true } : {}),
          }),
        }) })));
      }
    }
    vi.stubGlobal("WebSocket", Socket);
    vi.stubGlobal("window", { setTimeout, clearTimeout, location: new URL("https://piik.example/") });
    vi.stubGlobal("fetch", vi.fn(async () => new Response(JSON.stringify({
      ...health, nativeMedia: { ...health.nativeMedia, receiverReuse: true },
    }))));
    const client = (await NativeClient.connect())!;
    try {
      const result = await client.receiveOffer("share_123456", "connection_1234", { type: "offer", sdp: "offer" }, { iceServers: [] }, 2);
      expect(requests.at(-1)).toEqual({ version: 10, id: expect.any(String), type: "receive-offer",
        shareId: "share_123456", connectionId: "connection_1234", sdp: "offer", iceServers: [], edgeCapacity: 2,
        reuseReceiver: true,
      });
      expect(result.reused).toBe(reused);
    } finally { client.close(); }
  });
});
