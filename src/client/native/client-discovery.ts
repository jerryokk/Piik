import { debugEvent } from "../lib/debug";
import { clientRuntimePort } from "../lib/session";
import { NATIVE_CLIENT_PORT_START, NATIVE_CLIENT_PORT_END } from "./discovery-ports";
import {
  nativeDiscoveryIdentitySchema,
  NATIVE_CLIENT_PROTOCOL,
  nativeConnectSchema,
  nativeHealthSchema,
  type NativeHealth,
} from "./wire";

const REQUEST_TIMEOUT_MS = 8_000;

export interface NativeDiscoveryOptions {
  waitForPermission?: boolean;
  signal?: AbortSignal;
}

export function isLoopbackPage(): boolean {
  return ["localhost", "127.0.0.1", "[::1]"].includes(window.location.hostname);
}

async function localPermission(): Promise<PermissionStatus | null> {
  // App Local already runs on loopback. Its permission can say "prompt" even
  // though loopback-to-loopback requests do not need consent.
  if (isLoopbackPage()) return null;
  for (const name of ["loopback-network", "local-network-access"]) {
    try {
      return await navigator.permissions.query({ name: name as PermissionName });
    } catch {
      // Older Chromium uses the combined name; other browsers may expose neither.
    }
  }
  return null;
}

export class NativeCompatibilityError extends Error {
  constructor(readonly actualProtocol: number) {
    super(`Piik App control protocol ${actualProtocol} is incompatible with ${NATIVE_CLIENT_PROTOCOL}`);
    this.name = "NativeCompatibilityError";
  }
}

export async function discoverNativeHealth(
  options: NativeDiscoveryOptions = {},
): Promise<NativeHealth | null> {
  const health = await discoverNative("health", body => nativeHealthSchema.parse(body), options);
  if (health) debugEvent("native", "discovered", { capabilities: health.nativeMedia });
  return health;
}

/** A local approval address is not permission to use native media. */
export async function discoverNativeConnect(signal?: AbortSignal): Promise<string | null> {
  return discoverNative("connect-discovery", body => {
    const { connectURL } = nativeConnectSchema.parse(body);
    const url = new URL(connectURL);
    if (url.protocol !== "http:" || url.hostname !== "127.0.0.1" || !url.port ||
        url.username || url.password || url.pathname !== "/client" || url.search || url.hash) {
      throw new Error("Invalid local App approval address");
    }
    url.searchParams.set("site", window.location.origin);
    return url.toString();
  }, { signal });
}

async function discoverNative<T>(
  path: "health" | "connect-discovery", read: (body: unknown) => T,
  { waitForPermission = true, signal }: NativeDiscoveryOptions,
): Promise<T | null> {
  if (signal?.aborted) return null;
  const permission = await localPermission();
  if (!waitForPermission && permission && permission.state !== "granted") {
    debugEvent("native", "unavailable", { stage: "permission", permission: permission.state });
    return null;
  }
  if (signal?.aborted) return null;
  const controller = new AbortController();
  const abort = () => controller.abort(signal?.reason);
  signal?.addEventListener("abort", abort, { once: true });
  // Permission waiting is user-owned; the parallel scan has one network deadline.
  let timer: number | undefined;
  const startDeadline = () => { timer ??= window.setTimeout(() => controller.abort(), REQUEST_TIMEOUT_MS); };
  const permissionChanged = () => { if (permission?.state !== "prompt") startDeadline(); };
  const awaitingPermission = permission?.state === "prompt";
  try {
    if (awaitingPermission) permission.addEventListener("change", permissionChanged);
    permissionChanged();
    // An explicit launch stays with its App even when another approved App is faster.
    const selectedPort = clientRuntimePort();
    const ports = selectedPort === null ? Array.from({
      length: NATIVE_CLIENT_PORT_END - NATIVE_CLIENT_PORT_START + 1,
    }, (_, index) => NATIVE_CLIENT_PORT_START + index) : [selectedPort];
    return await Promise.any(ports.map(async port => {
      const response = await fetch(`http://127.0.0.1:${port}/${path}`, {
        cache: "no-store",
        signal: controller.signal,
        ...(isLoopbackPage() ? {} : { targetAddressSpace: "loopback" }),
      } as RequestInit);
      startDeadline();
      if (!response.ok) throw new Error("Piik App discovery was not accepted");
      const body: unknown = await response.json();
      const identity = nativeDiscoveryIdentitySchema.safeParse(body);
      if (!identity.success || identity.data.port !== port) {
        throw new Error("Not a Piik App discovery response");
      }
      if (identity.data.protocol !== NATIVE_CLIENT_PROTOCOL) {
        throw new NativeCompatibilityError(identity.data.protocol);
      }
      return read(body);
    }));
  } catch (error) {
    const incompatible = error instanceof AggregateError
      ? error.errors.find((failure) => failure instanceof NativeCompatibilityError)
      : undefined;
    if (incompatible) {
      debugEvent("native", "incompatible", {
        expectedProtocol: NATIVE_CLIENT_PROTOCOL,
        actualProtocol: incompatible.actualProtocol,
      });
      throw incompatible;
    }
    // A failed fetch cannot distinguish an absent App from blocked local access.
    debugEvent("native", "unavailable", { stage: "discovery", timedOut: controller.signal.aborted });
    return null;
  } finally {
    if (awaitingPermission) permission.removeEventListener("change", permissionChanged);
    signal?.removeEventListener("abort", abort);
    window.clearTimeout(timer);
    controller.abort();
  }
}

export async function notifyNativePresentation(
  language: "zh" | "en" | "vis",
  signal: AbortSignal,
): Promise<void> {
  if (signal.aborted) return;
  const health = await discoverNativeHealth({ signal, waitForPermission: false });
  if (!health || signal.aborted) return;
  await fetch(`http://127.0.0.1:${health.port}/presentation`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ language }),
    signal,
    ...(isLoopbackPage() ? {} : { targetAddressSpace: "loopback" }),
  } as RequestInit);
}
