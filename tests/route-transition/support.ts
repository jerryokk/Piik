import { vi } from "vitest";
import { type ParticipantRouteAssignment, type PreparedRouteCandidate } from "../../src/shared/protocol.ts";
import { ViewerSfuRoute } from "../../src/client/media/viewer-sfu-route";
import { EMPTY_METRICS, type ConnectionMetrics } from "../../src/client/types.ts";

export const peerAssignment = (
  parentPeerId: string,
  childPeerIds: string[] = [],
): ParticipantRouteAssignment => ({
  upstream: { kind: "peer", peerId: parentPeerId },
  childPeerIds,
  sfuPublicationGeneration: null,
});

export const viewerSfuAssignment = (
  childPeerIds: string[] = [],
  publicationGeneration = "publication_generation_12345678",
): ParticipantRouteAssignment => ({
  upstream: { kind: "sfu" },
  childPeerIds,
  sfuPublicationGeneration: publicationGeneration,
});

export const emptyViewerAssignment = (): ParticipantRouteAssignment => ({
  upstream: { kind: "none" },
  childPeerIds: [],
  sfuPublicationGeneration: null,
});

export const hostAssignment = (
  publicationGeneration: string | null,
  childPeerIds: string[] = [],
): ParticipantRouteAssignment => ({
  upstream: { kind: "none" },
  childPeerIds,
  sfuPublicationGeneration: publicationGeneration,
});

export function sfuConfig(revision: number, publicationGeneration = "publication_generation_12345678") {
  return {
    type: "sfu-config" as const,
    revision,
    publicationGeneration,
    connectionId: `sfu_connection_${revision}_12345678`,
  };
}

export function createFakePublisher(log: string[], label: string) {
  return {
    updateConfig: vi.fn(),
    acceptSignal: vi.fn(async () => undefined),
    connect: vi.fn(async (_config: unknown) => {
      log.push(`${label}:connect`);
      return true;
    }),
    activate: vi.fn(async () => {
      log.push(`${label}:activate`);
      return true;
    }),
    deactivate: vi.fn(async () => {
      log.push(`${label}:deactivate`);
      return true;
    }),
    replaceStream: vi.fn(async () => true),
    updateProfile: vi.fn(async () => true),
    setPaused: vi.fn(),
    disconnect: vi.fn(async () => {
      log.push(`${label}:disconnect`);
    }),
  };
}

export type SubscriberEvents = Parameters<
  NonNullable<ConstructorParameters<typeof ViewerSfuRoute>[1]["createSubscriber"]>
>[0];

export const candidate = (
  revision: number,
  childPeerId = "viewer_12345678",
  transport: "direct" | "sfu" = "direct",
  connectionId = `candidate_${revision}_12345678`,
  qualityProbe = false,
): PreparedRouteCandidate => ({
  childPeerId,
  connectionId,
  transport,
  qualityProbe,
});

export function createFakeSubscriber(events: SubscriberEvents, log: string[], label: string) {
  return {
    isConnected: vi.fn(() => true),
    resync: vi.fn(),
    updateConfig: vi.fn(),
    acceptSignal: vi.fn(async () => undefined),
    reconnect: vi.fn(() => true),
    events,
    connect: vi.fn(async () => {
      log.push(`${label}:connect`);
      return true;
    }),
    activate: vi.fn(() => {
      log.push(`${label}:activate`);
      return true;
    }),
    deactivate: vi.fn(() => {
      log.push(`${label}:deactivate`);
      return true;
    }),
    armDecodedFrameProof: vi.fn((requireProgress = false) => {
      log.push(`${label}:proof:${requireProgress ? "progress" : "fresh"}`);
    }),
    stopDecodedFrameProof: vi.fn(() => {
      log.push(`${label}:proof:stop`);
    }),
    disconnect: vi.fn(async () => {
      log.push(`${label}:disconnect`);
    }),
  };
}

export async function prepareSfu(
  route: ViewerSfuRoute,
  revision: number,
  assignment = viewerSfuAssignment(),
  config = sfuConfig(revision, assignment.sfuPublicationGeneration ?? undefined),
) {
  route.accept({ revision, phase: "prepare", assignment,
    candidate: candidate(revision, "viewer_12345678", "sfu", config.connectionId) });
  await route.acceptConfig(config);
  route.accept({ revision, phase: "active", assignment });
}

export function receiveMetrics(
  timestampMs: number,
  overrides: Partial<ConnectionMetrics> = {},
): ConnectionMetrics {
  return {
    ...EMPTY_METRICS,
    sampleTimestampMs: timestampMs,
    sampleWindowMs: 2_000,
    frameWidth: 1920,
    frameHeight: 1080,
    framesPerSecond: 60,
    bitrateKbps: 5_000,
    intervalFramesDecoded: 120,
    intervalFreezeCount: 0,
    intervalFreezeDurationMs: 0,
    intervalPauseCount: 0,
    intervalPauseDurationMs: 0,
    ...overrides,
  };
}
