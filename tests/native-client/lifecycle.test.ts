import { afterEach, describe, expect, it, vi } from "vitest";
import { NativeClient } from "../../src/client/native/client";
import { DEFAULT_QUALITY_SETTINGS } from "../../src/shared/protocol";
import { restoreNativeMocks } from "./support.ts";

afterEach(restoreNativeMocks);
describe("native App private wire", () => {
  it("notifies the owner once on an unexpected close and stays silent on cleanup", async () => {
    const sockets: FakeWebSocket[] = [];
    const token = "a".repeat(43);
    let failureCode = "operation-failed";
    class FakeWebSocket extends EventTarget {
      static readonly OPEN = 1;
      static readonly CLOSING = 2;
      static readonly CLOSED = 3;
      readyState = FakeWebSocket.OPEN;
      protocol = `piik-client-v10.${token}`;
      readonly close = vi.fn(() => {
        this.readyState = FakeWebSocket.CLOSING;
      });

      constructor(readonly url: string, readonly protocols: string[]) {
        super();
        sockets.push(this);
        queueMicrotask(() => this.dispatchEvent(new Event("open")));
      }

      send(payload: string): void {
        const request = JSON.parse(payload) as { id: string; type: string };
        if (!["hello", "update-share", "ping"].includes(request.type)) return;
        queueMicrotask(() => {
          const event = new Event("message");
          Object.defineProperty(event, "data", {
            value: JSON.stringify({
              version: 10,
              id: request.id,
              type: request.type === "hello" ? "ready" : request.type === "ping" ? "pong" : "request-failed",
              ...(request.type === "update-share" ? { code: failureCode } : {}),
            }),
          });
          this.dispatchEvent(event);
        });
      }

      emitUnexpectedClose(): void {
        this.readyState = FakeWebSocket.CLOSED;
        this.dispatchEvent(new Event("close"));
      }
    }
    vi.stubGlobal("WebSocket", FakeWebSocket);
    vi.stubGlobal("window", {
      setTimeout: globalThis.setTimeout,
      clearTimeout: globalThis.clearTimeout,
      location: new URL("https://piik.example/"),
    });
    vi.stubGlobal("fetch", vi.fn(async () =>
      new Response(JSON.stringify({
        protocol: 10,
        service: "piik-client",
        port: 39_721,
        instanceToken: token,
        nativeMedia: {
          video: true,
          processAudio: false,
          systemAudio: false,
          hardwareH264: true,
          softwareVP8: true,
          receiverReuse: false, remoteCoPlay: false, shareVolume: false,
          sourceCancellation: false, captureFailureDetails: false, captureBorderControl: false,
        },
      }), { status: 200 }),
    ));

    const client = await NativeClient.connect();
    expect(client).not.toBeNull();
    const unexpected = vi.fn();
    client!.onClose(unexpected);
    await expect(client!.updateShare("share_123456", DEFAULT_QUALITY_SETTINGS))
      .rejects.toMatchObject({ name: "NativeRequestError", reason: "rejected", operation: "update-share" });
    failureCode = "source-canceled";
    await expect(client!.updateShare("share_123456", DEFAULT_QUALITY_SETTINGS))
      .rejects.toMatchObject({ name: "NotAllowedError" });
    await client!.ping();
    expect(sockets[0]!.close).not.toHaveBeenCalled();
    expect(unexpected).not.toHaveBeenCalled();
    vi.useFakeTimers();
    window.setTimeout = globalThis.setTimeout;
    window.clearTimeout = globalThis.clearTimeout;
    const timedOut = expect(client!.sources()).rejects.toMatchObject({ reason: "timeout", operation: "list-sources" });
    await vi.advanceTimersByTimeAsync(8_000);
    await timedOut;
    expect(sockets[0]!.close).not.toHaveBeenCalled();
    vi.useRealTimers();
    window.setTimeout = globalThis.setTimeout;
    window.clearTimeout = globalThis.clearTimeout;
    const pending = client!.sources();
    const closedRequest = expect(pending).rejects.toMatchObject({ name: "NativeRequestError", reason: "disconnected", operation: "list-sources" });
    sockets[0]!.emitUnexpectedClose();
    sockets[0]!.emitUnexpectedClose();
    expect(unexpected).toHaveBeenCalledOnce();
    await closedRequest;
    await expect(client!.sources()).rejects.toMatchObject({ reason: "unavailable", operation: "list-sources" });

    const cleanClient = await NativeClient.connect();
    expect(cleanClient).not.toBeNull();
    const intentional = vi.fn();
    cleanClient!.onClose(intentional);
    cleanClient!.close();
    sockets[1]!.emitUnexpectedClose();
    expect(intentional).not.toHaveBeenCalled();
  });
});
