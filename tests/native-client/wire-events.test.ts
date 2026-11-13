import { afterEach, describe, expect, it, vi } from "vitest";
import { NativeClient } from "../../src/client/native/client";
import { nativeEventSchema, nativeCaptureTargetSchema, shareSourceReplacedResponseSchema, shareStartedResponseSchema, shareUpdatedResponseSchema, sourcePreviewResponseSchema, receiveAnswerResponseSchema } from "../../src/client/native/wire";
import { restoreNativeMocks, health } from "./support.ts";

afterEach(restoreNativeMocks);
describe("native App private wire", () => {
  it("keeps 64-bit Windows identities as exact decimal strings", () => {
    expect(
      nativeCaptureTargetSchema.safeParse({
        kind: "window",
        sourceId: "12345678901234567890",
        pid: 1234,
        creationTime: "134327999999999999",
        title: "Game",
      }).success,
    ).toBe(true);
    expect(
      nativeCaptureTargetSchema.safeParse({
        kind: "window",
        sourceId: 12345678901234567890,
        pid: 1234,
        creationTime: 134327999999999999,
        title: "Game",
      }).success,
    ).toBe(false);
    expect(
      nativeCaptureTargetSchema.safeParse({
        kind: "display",
        sourceId: "65537",
        title: "Display 1",
      }).success,
    ).toBe(true);
    expect(
      nativeCaptureTargetSchema.safeParse({
        kind: "picker",
        sourceId: "1",
        title: "System picker",
      }).success,
    ).toBe(true);
  });

  it("fences native events by share and connection identity", () => {
    const event = {
      version: 10,
      type: "edge-state",
      shareId: "share_123456",
      connectionId: "edge_1234567",
      state: "connected",
    };
    expect(nativeEventSchema.safeParse(event).success).toBe(true);
    expect(
      nativeEventSchema.safeParse({ ...event, connectionId: "short" }).success,
    ).toBe(false);
    expect(
      nativeEventSchema.safeParse({ ...event, routeRevision: 1 }).success,
    ).toBe(false);
    const path = {
      version: 10,
      type: "edge-path",
      shareId: "share_123456",
      connectionId: "edge_1234567",
      localType: "host",
      remoteType: "srflx",
      natTraversalPath: "predicted",
    };
    expect(nativeEventSchema.safeParse(path).success).toBe(true);
    const { natTraversalPath: _missing, ...incompletePath } = path;
    expect(nativeEventSchema.safeParse(incompletePath).success).toBe(false);
  });

  it("keeps an unavailable preview advisory instead of treating it as media failure", () => {
    const preview = {
      version: 10,
      id: "request_preview",
      type: "source-preview",
      sourceKey: "display:65537",
      mime: "image/bmp",
      data: "",
    };
    expect(sourcePreviewResponseSchema.safeParse(preview).success).toBe(true);
    expect(sourcePreviewResponseSchema.safeParse({
      ...preview,
      data: Buffer.alloc(54 + 320 * 180 * 3).toString("base64"),
    }).success).toBe(true);
    expect(sourcePreviewResponseSchema.safeParse({
      ...preview,
      data: "A".repeat(256 * 1024 + 1),
    }).success).toBe(false);
  });

  it("keeps native share lifecycle responses as strict acknowledgements", () => {
    const profile = {
      resolution: { width: 2560, height: 1440 },
      maxFramerate: 60,
      maxBitrate: 12_000_000,
      degradationPreference: "maintain-framerate",
      maxAudioBitrate: 192000,
    };
    expect(shareStartedResponseSchema.safeParse({
      version: 10,
      id: "request_start",
      type: "share-started",
      shareId: "share_123456",
      audio: true,
      codec: "h264",
    }).success).toBe(true);
    expect(shareUpdatedResponseSchema.safeParse({
      version: 10,
      id: "request_update",
      type: "share-updated",
      shareId: "share_123456",
    }).success).toBe(true);
    expect(shareSourceReplacedResponseSchema.safeParse({
      version: 10,
      id: "request_source",
      type: "share-source-replaced",
      shareId: "share_123456",
    }).success).toBe(true);
    expect(shareUpdatedResponseSchema.safeParse({
      version: 10,
      id: "request_update",
      type: "share-updated",
      shareId: "share_123456",
      profile,
    }).success).toBe(false);
  });

  it("requires the actual Native codec instead of reporting Auto as media", () => {
    const response = {
      version: 10,
      id: "request_receive",
      type: "receive-answer",
      shareId: "share_123456",
      connectionId: "edge_1234567",
      sdp: "v=0\r\n",
      audio: false,
    };
    for (const codec of ["h264", "vp8"]) {
      expect(receiveAnswerResponseSchema.safeParse({ ...response, codec }).success).toBe(true);
      expect(shareStartedResponseSchema.safeParse({
        version: 10, id: "request_start", type: "share-started",
        shareId: response.shareId, audio: false, codec,
      }).success).toBe(true);
    }
    expect(receiveAnswerResponseSchema.safeParse(response).success).toBe(false);
    expect(receiveAnswerResponseSchema.safeParse({ ...response, codec: "auto" }).success).toBe(false);
  });

  it("accepts only internally consistent native quality evidence", () => {
    const event = {
      version: 10,
      type: "edge-quality",
      shareId: "share_123456",
      connectionId: "edge_1234567",
      sampleTimestampMs: 10_000,
      sampleWindowMs: 2_000,
      rtpStatsId: "pc_12345678",
      trackIdentifier: "screen",
      state: "degraded",
      reason: "bandwidth",
      intervalFramesEncoded: 60,
      framesPerSecond: 30,
      bitrateKbps: 3_000,
      availableOutgoingKbps: 1_000,
      width: 1280,
      height: 720,
    };
    expect(nativeEventSchema.safeParse(event).success).toBe(true);
    expect(
      nativeEventSchema.safeParse({ ...event, reason: "none" }).success,
    ).toBe(false);
    expect(
      nativeEventSchema.safeParse({
        ...event,
        state: "unknown",
        reason: null,
      }).success,
    ).toBe(true);
  });

  it.each(["edge-quality", "publication-quality"] as const)("isolates %s observations from control lifetime", async (type) => {
    class Socket extends EventTarget {
      static readonly OPEN = 1;
      static readonly CLOSING = 2;
      static latest: Socket;
      readyState = Socket.OPEN;
      protocol = `piik-client-v10.${health.instanceToken}`;
      close = vi.fn(() => { this.readyState = Socket.CLOSING; this.dispatchEvent(new Event("close")); });
      constructor() { super(); Socket.latest = this; queueMicrotask(() => this.dispatchEvent(new Event("open"))); }
      emit(value: unknown) { this.dispatchEvent(new MessageEvent("message", { data: JSON.stringify(value) })); }
      send(payload: string) {
        const request = JSON.parse(payload);
        this.emit({ version: 10, id: request.id, type: request.type === "hello" ? "ready" : "pong" });
      }
    }
    vi.stubGlobal("WebSocket", Socket);
    vi.stubGlobal("window", { setTimeout, clearTimeout, location: new URL("https://piik.example/") });
    vi.stubGlobal("fetch", vi.fn(async () => Response.json(health)));
    const client = (await NativeClient.connect())!;
    const received = vi.fn();
    client.onEvent(received);
    const event = { version: 10, type, shareId: "share_123456", connectionId: "connection_123456",
      sampleTimestampMs: 10_000, sampleWindowMs: 2_000, rtpStatsId: "rtp-1", trackIdentifier: "screen",
      state: "healthy", reason: "none", framesPerSecond: 480, bitrateKbps: 120_000,
      availableOutgoingKbps: 1_000_000, width: 1920, height: 1080,
      ...(type === "edge-quality" ? { intervalFramesEncoded: 60 } : {
        publicationGeneration: "publication_123456", codec: "vp8", videoEncodingCount: 2,
        activeVideoEncodingCount: 2, rid: "h", intervalFramesSent: 60, audioBitrateKbps: 192,
      }),
    };
    Socket.latest.emit(event);
    expect(received).toHaveBeenLastCalledWith(event);
    Socket.latest.emit({ ...event, framesPerSecond: -1, availableOutgoingKbps: "invalid" });
    expect(received).toHaveBeenLastCalledWith({ ...event, framesPerSecond: null, availableOutgoingKbps: null });
    // Invalid identity/state never becomes evidence, but cannot retire media.
    Socket.latest.emit({ ...event, shareId: "bad", state: "degraded", reason: "none" });
    expect(received).toHaveBeenCalledTimes(2);
    await expect(client.ping()).resolves.toBeUndefined();
    expect(Socket.latest.close).not.toHaveBeenCalled();
    Socket.latest.emit({ version: 10, type: "edge-state", shareId: "bad" });
    expect(Socket.latest.close).toHaveBeenCalledWith(1008, "invalid Client message");
  });
});
