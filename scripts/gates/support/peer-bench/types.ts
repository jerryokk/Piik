// Shared benchmark vocabulary: limits, observations, configuration and the
// report shape. Declarations only, so helpers never import the runner.

import type { ChildProcess } from "node:child_process";
import type { QualitySettings, QualityProfileId } from "../../../../src/client/media/quality";
import type {
  ParticipantRouteAssignment,
  RouteDiagnosticSnapshot,
} from "../../../../src/shared/protocol";
import { MAX_ENDPOINT_MEDIA_COPY_CAPACITY } from "../../../../src/shared/media-copy-accounting";
import { MAX_VIEWERS_PER_ROOM_LIMIT } from "../../../../src/shared/protocol";
import type { summarizeSamples } from "./report";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

export type ProfileId = QualityProfileId;
export type PageRole = "host" | "viewer";
export type BenchmarkCodecMode = "vp8" | "auto" | "h264";

export const ROUTE_TIMING_KEYS = [
  "queueWaitMs",
  "candidateStartMs",
  "firstDecodedFrameMs",
  "finalMs",
] as const satisfies readonly (keyof RouteDiagnosticSnapshot["children"][number])[];
export type RouteTimingKey = (typeof ROUTE_TIMING_KEYS)[number];
export interface RouteTimingDistribution {
  sampleCount: number;
  pendingCount: number;
  rawMs: number[];
  p50Ms: number | null;
  p95Ms: number | null;
  maxMs: number | null;
}
export type BenchmarkRouteTimingSamples = {
  [Key in RouteTimingKey]: Array<
    RouteDiagnosticSnapshot["children"][number][Key]
  >;
};
export type BenchmarkRouteTimingSummary = Record<
  RouteTimingKey,
  RouteTimingDistribution
>;

export interface BenchmarkConfig {
  chromePath: string;
  viewerCounts: number[];
  expectedEndpointCap: number;
  profileId: ProfileId;
  codecMode: BenchmarkCodecMode;
  durationMs: number;
  settleMs: number;
  sampleIntervalMs: number;
  connectionTimeoutMs: number;
  recoveryViewerCount: number | null;
  recoveryTimeoutMs: number;
  outputPath: string | null;
  headless: boolean;
  noSandbox: boolean;
  qualityControlSmoke: boolean;
}

export interface ExternalServerTarget {
  origin: string;
  accessPassword: string | null;
}

export interface MediaTotals {
  id: string;
  framesTotal: number | null;
  bytesTotal: number | null;
}

export interface ConnectionObservation {
  index: number;
  createdAtEpochMs: number;
  connectionId: string | null;
  remotePeerId: string | null;
  connectionState: string;
  iceConnectionState: string;
  hasOutboundVideo: boolean;
  hasInboundVideo: boolean;
  send: Record<string, unknown> | null;
  receive: Record<string, unknown> | null;
  sendTotals: MediaTotals | null;
  receiveTotals: MediaTotals | null;
  error?: string;
}

export interface PagePerformanceSample {
  timestampSeconds: number;
  taskDurationSeconds: number;
  scriptDurationSeconds: number;
  layoutDurationSeconds: number;
  recalcStyleDurationSeconds: number;
  jsHeapUsedBytes: number;
}

export interface PageObservation {
  label: string;
  role: PageRole;
  viewerIndex: number | null;
  roomId: string | null;
  peerId: string | null;
  authenticatedAtEpochMs: number | null;
  authenticateSentAtEpochMs: number | null;
  shareRequestedAtEpochMs?: number | null;
  signalingConnected: boolean;
  qualitySettings: QualitySettings | null;
  routeRevision: number | null;
  routeAssignment: ParticipantRouteAssignment | null;
  maxActiveOutboundMediaEdges: number;
  maxAssignedChildren: number;
  firstDecodedAtEpochMs: number | null;
  firstRenderedAtEpochMs: number | null;
  renderedFrames: number;
  connections: ConnectionObservation[];
  performance?: PagePerformanceSample | null;
}

export interface FailurePageEvidence {
  label: string;
  role: PageRole;
  authenticated: boolean;
  signalingConnected: boolean;
  routeRevision: number | null;
  upstreamKind: ParticipantRouteAssignment["upstream"]["kind"] | null;
  assignedChildCount: number;
  firstDecoded: boolean;
  firstRendered: boolean;
  connectionCount: number;
  connections: Array<{
    connectionState: string;
    iceConnectionState: string;
    identity: "peer" | "sfu" | "unidentified";
    hasOutboundVideo: boolean;
    hasInboundVideo: boolean;
    decoded: boolean;
  }>;
}

export interface BenchmarkFailureEvidence {
  expectedPageCount: number;
  observedPageCount: number;
  pages: FailurePageEvidence[];
}

export interface TimedSample {
  atEpochMs: number;
  elapsedMs: number;
  pages: PageObservation[];
  browserProcesses?: BrowserProcessSample | null;
}

export interface BrowserProcessSample { processes: Array<{ type: string; id: number; cpuTimeSeconds: number }> }

export interface RunCheck {
  name: string;
  passed: boolean;
  actual: number | boolean;
  expected: string;
}

export interface RecoveryResult {
  triggered: boolean;
  relayPeerId?: string;
  affectedPeerIds?: string[];
  failureAtEpochMs?: number;
  reassignedAtEpochMs?: number;
  recoveredAtEpochMs?: number;
  recoveryMs?: number;
  maxHostActiveMediaEdges?: number;
  maxHostAssignedChildren?: number;
  error?: string;
}

export interface RecoveryHostPeaks {
  maxHostActiveMediaEdges?: number;
  maxHostAssignedChildren?: number;
}

export interface RecoveryViewerBaseline {
  label: string;
  routeRevision: number;
  upstreamKind: "peer" | "sfu";
  parentPeerId: string | null;
  connectionId: string | null;
  connectionIndex: number;
  rtpId: string;
  framesTotal: number;
}

export interface BenchmarkRun {
  viewerCount: number;
  startedAt: string;
  completedAt: string;
  status: "passed" | "failed";
  checks: RunCheck[];
  summary: ReturnType<typeof summarizeSamples> | null;
  routeTimingSummary: BenchmarkRouteTimingSummary | null;
  routeTimingStatus: "not-requested" | "captured" | "unavailable";
  samples: TimedSample[];
  recovery: RecoveryResult;
  failureEvidence?: BenchmarkFailureEvidence;
  error?: string;
}

export interface BenchmarkReport {
  schemaVersion: 5;
  startedAt: string;
  completedAt: string | null;
  gitCommit: string | null;
  chromium: Record<string, unknown> | null;
  environment: {
    node: string;
    platform: NodeJS.Platform;
    arch: string;
    headless: boolean;
  };
  configuration: Omit<BenchmarkConfig, "chromePath" | "outputPath"> & {
    chromeExecutable: string;
    output: string;
  };
  capture: {
    kind: "deterministic-canvas";
    width: number;
    height: number;
    frameRate: number;
    audio: false;
  };
  limitations: string[];
  runs: BenchmarkRun[];
  fatalError?: string;
}

export interface PageHandle {
  targetId: string;
  sessionId: string;
  label: string;
}

export interface RuntimeEvaluation<T> {
  result: { value?: T };
  exceptionDetails?: { text?: string; exception?: { description?: string } };
}

export interface CdpResponse {
  id?: number;
  result?: unknown;
  error?: { code: number; message: string };
}

export interface ManagedProcess {
  child: ChildProcess;
  log: { append(chunk: Buffer | string): void; tail(): string };
}

export const MAX_VIEWERS = MAX_VIEWERS_PER_ROOM_LIMIT;
export const MAX_FAILURE_CONNECTIONS_PER_PAGE =
  MAX_ENDPOINT_MEDIA_COPY_CAPACITY * 2;
export const DEFAULT_VIEWER_COUNTS = [MAX_VIEWERS];
export const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
