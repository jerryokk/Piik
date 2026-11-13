import { vi } from "vitest";
import { DEFAULT_QUALITY_SETTINGS, DEFAULT_ROUTE_POLICY, type QualitySettings } from "../../src/shared/protocol.ts";
import { setCopy } from "../../src/client/ui/copy.ts";
import { releaseHostRoom } from "../../src/client/lib/session";

export const restoreClientState = () => {
  releaseHostRoom();
  vi.useRealTimers();
  vi.unstubAllGlobals();
  setCopy({ lang: "zh", vis: false });
};

export function routeAuthenticated(
  shareGeneration: string | null = null,
  qualitySettings: QualitySettings = DEFAULT_QUALITY_SETTINGS,
) {
  return {
    mediaMode: "peer-assisted" as const,
    shareGeneration,
    routeRevision: 0,
    routeAssignment: {
      upstream: { kind: "none" as const },
      childPeerIds: [],
      sfuPublicationGeneration: null,
    },
    qualitySettings,
    routePolicy: DEFAULT_ROUTE_POLICY,
  };
}

export function hostRoomStorage(lockState: "available" | "held" | "absent" = "available") {
  const local = new Map<string, string>();
  const session = new Map<string, string>();
  const held = new Set<string>();
  const storage = (values: Map<string, string>) => ({
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => values.set(key, value),
    removeItem: (key: string) => values.delete(key),
  });
  const request = vi.fn(async (name: string, _options: LockOptions, callback: (lock: Lock | null) => Promise<void>) => {
    if (lockState === "held" || held.has(name)) return callback(null);
    held.add(name);
    try { await callback({ name, mode: "exclusive" } as Lock); }
    finally { held.delete(name); }
  });
  vi.stubGlobal("window", {
    localStorage: storage(local), sessionStorage: storage(session),
    navigator: { locks: lockState === "absent" ? undefined : { request } },
  });
  return { local, session, held, request };
}

export const storedHost = (roomId = "1234", hostToken = "a".repeat(32)) => ({
  roomId, hostToken, inviteUrl: `https://share.test/r/${roomId}`,
});
