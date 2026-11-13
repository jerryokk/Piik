import { afterEach, describe, expect, it, vi } from "vitest";
import { discoverNativeHealth, NativeClient, notifyNativePresentation } from "../../src/client/native/client";
import { NATIVE_CLIENT_PORT_START, NATIVE_CLIENT_PORT_END } from "../../src/client/native/discovery-ports";
import { NATIVE_CLIENT_PROTOCOL } from "../../src/client/native/wire";
import { restoreNativeMocks, health } from "./support.ts";

afterEach(restoreNativeMocks);
describe("native App private wire", () => {
  it("keeps discovery alive while local access takes longer than 400 ms", async () => {
    vi.useFakeTimers();
    vi.stubGlobal("window", { setTimeout, clearTimeout, location: new URL("https://piik.example/") });
    vi.stubGlobal("fetch", vi.fn(async (url: string, options: RequestInit) => {
      if (new URL(url).port !== String(health.port)) throw new TypeError("No listener");
      await new Promise<void>((resolve, reject) => {
        const timer = setTimeout(resolve, 1_500);
        options.signal!.addEventListener("abort", () => {
          clearTimeout(timer);
          reject(new DOMException("Aborted", "AbortError"));
        }, { once: true });
      });
      return new Response(JSON.stringify(health));
    }));
    const discovery = discoverNativeHealth();
    await vi.advanceTimersByTimeAsync(1_500);
    await expect(discovery).resolves.toMatchObject({ port: health.port });
    expect(vi.getTimerCount()).toBe(0);
  });

  it("does not spend the discovery deadline while the user decides local permission", async () => {
    vi.useFakeTimers();
    vi.stubGlobal("window", { setTimeout, clearTimeout, location: new URL("https://piik.example/") });
    const permission = Object.assign(new EventTarget(), { state: "prompt" as PermissionState });
    const query = vi.fn().mockResolvedValue(permission);
    const remove = vi.spyOn(permission, "removeEventListener");
    vi.stubGlobal("navigator", { permissions: { query } });
    let respond!: (response: Response) => void;
    vi.stubGlobal("fetch", vi.fn((url: string, options: RequestInit) => {
      if (Number(new URL(url).port) !== health.port) return Promise.reject(new TypeError("No listener"));
      return new Promise<Response>((resolve, reject) => {
        respond = resolve;
        options.signal!.addEventListener("abort", () => reject(options.signal!.reason), { once: true });
      });
    }));
    const settled = vi.fn();
    const discovery = discoverNativeHealth().then(settled);
    await vi.advanceTimersByTimeAsync(12_000);
    expect(settled).not.toHaveBeenCalled();
    permission.state = "granted";
    permission.dispatchEvent(new Event("change"));
    respond(Response.json(health));
    await discovery;
    expect(settled).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({ port: health.port }));
    expect(query).toHaveBeenCalledOnce();
    expect(remove).toHaveBeenCalledWith("change", expect.any(Function));
    expect(vi.getTimerCount()).toBe(0);
  });

  it("finds a later App without waiting for a silent port and aborts the unused probe", async () => {
    vi.stubGlobal("window", { setTimeout, clearTimeout, location: new URL("https://piik.example/") });
    let silentSignal: AbortSignal | undefined;
    vi.stubGlobal("fetch", vi.fn(async (url: string, options: RequestInit) => {
      const port = Number(new URL(url).port);
      if (port === health.port) {
        silentSignal = options.signal!;
        return new Promise<Response>((_resolve, reject) => {
          silentSignal!.addEventListener("abort", () => reject(new DOMException("Aborted", "AbortError")), { once: true });
        });
      }
      if (port !== health.port + 1) throw new TypeError("No listener");
      return new Response(JSON.stringify({ ...health, port }));
    }));
    await expect(discoverNativeHealth()).resolves.toMatchObject({ port: health.port + 1 });
    expect(silentSignal?.aborted).toBe(true);
  });

  it.each(["unsupported", "grant", "response"])("bounds silent discovery ports with one network deadline after %s", async mode => {
    vi.useFakeTimers();
    vi.spyOn(AbortSignal, "any").mockImplementation(() => { throw new TypeError("AbortSignal.any is not a function"); });
    vi.stubGlobal("window", { setTimeout, clearTimeout, location: new URL("https://piik.example/") });
    const permission = Object.assign(new EventTarget(), { state: "prompt" as PermissionState });
    if (mode !== "unsupported") vi.stubGlobal("navigator", { permissions: { query: vi.fn().mockResolvedValue(permission) } });
    const signals: AbortSignal[] = [];
    vi.stubGlobal("fetch", vi.fn(async (url: string, options: RequestInit) => {
      signals.push(options.signal!);
      if (mode === "response" && Number(new URL(url).port) === health.port) return new Response(null, { status: 403 });
      return new Promise<Response>((_resolve, reject) => {
        options.signal!.addEventListener("abort", () => reject(new DOMException("Aborted", "AbortError")), { once: true });
      });
    }));
    const discovery = discoverNativeHealth({ signal: new AbortController().signal });
    if (mode === "grant") {
      await vi.advanceTimersByTimeAsync(12_000);
      expect(signals.every(signal => !signal.aborted)).toBe(true);
      permission.state = "granted";
      permission.dispatchEvent(new Event("change"));
    }
    await vi.advanceTimersByTimeAsync(8_000);
    await expect(discovery).resolves.toBeNull();
    expect(signals).toHaveLength(NATIVE_CLIENT_PORT_END - NATIVE_CLIENT_PORT_START + 1);
    expect(signals.every((signal) => signal.aborted)).toBe(true);
    expect(vi.getTimerCount()).toBe(0);
  });

  it("does not wait for a permission decision when every discovery port rejects", async () => {
    vi.useFakeTimers();
    vi.stubGlobal("window", { setTimeout, clearTimeout, location: new URL("https://piik.example/") });
    const permission = Object.assign(new EventTarget(), { state: "prompt" });
    const remove = vi.spyOn(permission, "removeEventListener");
    vi.stubGlobal("navigator", { permissions: { query: vi.fn().mockResolvedValue(permission) } });
    const fetcher = vi.fn().mockRejectedValue(new TypeError("No listener"));
    vi.stubGlobal("fetch", fetcher);
    await expect(discoverNativeHealth()).resolves.toBeNull();
    expect(fetcher).toHaveBeenCalledTimes(NATIVE_CLIENT_PORT_END - NATIVE_CLIENT_PORT_START + 1);
    expect(remove).toHaveBeenCalledWith("change", expect.any(Function));
    expect(vi.getTimerCount()).toBe(0);
  });

  it("identifies an incompatible App even when that protocol uses different token or media metadata", async () => {
    vi.stubGlobal("window", { setTimeout, clearTimeout, location: new URL("https://piik.example/") });
    vi.stubGlobal("fetch", vi.fn(async () => new Response(JSON.stringify({
      ...health, protocol: NATIVE_CLIENT_PROTOCOL + 1,
      instanceToken: { format: "different-protocol" }, nativeMedia: "different-protocol",
    }))));
    await expect(NativeClient.connect()).rejects.toMatchObject({
      name: "NativeCompatibilityError", actualProtocol: NATIVE_CLIENT_PROTOCOL + 1,
    });
  });

  it("treats denied, absent and malformed endpoints as ordinary Browser-only operation", async () => {
    vi.stubGlobal("window", { setTimeout, clearTimeout, location: new URL("https://piik.example/") });
    const malformed = [
      { ...health, service: "another-service" },
      { ...health, protocol: "8" },
      { ...health, instanceToken: "invalid" },
      { ...health, nativeMedia: { video: "yes" } },
      { ...health, protocol: 8, port: NATIVE_CLIENT_PORT_END },
    ];
    let responseIndex = 0;
    vi.stubGlobal("fetch", vi.fn(async () => {
      const body = malformed[responseIndex++];
      if (body) return new Response(JSON.stringify(body));
      if (responseIndex % 2 === 0) return new Response(null, { status: 403 });
      throw new TypeError("Network unavailable");
    }));
    await expect(NativeClient.connect()).resolves.toBeNull();
  });

  it("notifies only the current presentation request after App discovery", async () => {
    vi.stubGlobal("window", {
      setTimeout: globalThis.setTimeout,
      clearTimeout: globalThis.clearTimeout,
      location: new URL("http://127.0.0.1/"),
    });
    const fetcher = vi.fn(async (url: string) => url.endsWith("/health")
      ? new Response(JSON.stringify({
          protocol: 10, service: "piik-client", port: 39_721,
          instanceToken: "a".repeat(43),
          nativeMedia: {
            video: true, processAudio: false, systemAudio: true, hardwareH264: true, softwareVP8: true,
            receiverReuse: false, remoteCoPlay: false, shareVolume: false,
            sourceCancellation: false, captureFailureDetails: false, captureBorderControl: false,
          },
        }), {status: 200})
      : new Response(null, {status: 204}));
    vi.stubGlobal("fetch", fetcher);
    const stale = new AbortController();
    const staleNotification = notifyNativePresentation("zh", stale.signal);
    stale.abort();
    await staleNotification;
    expect(fetcher).not.toHaveBeenCalled();
    const current = new AbortController();
    await notifyNativePresentation("vis", current.signal);
    expect(fetcher).toHaveBeenCalledWith("http://127.0.0.1:39721/health", expect.anything());
    expect(fetcher).toHaveBeenLastCalledWith("http://127.0.0.1:39721/presentation", expect.objectContaining({
      method: "POST", body: JSON.stringify({language: "vis"}), signal: current.signal,
    }));
  });
});
