// Pure projections of one page observation: media edges, SFU publication
// coherence, runtime role, performance metrics and failure evidence.

import {
  MAX_FAILURE_CONNECTIONS_PER_PAGE,
  type BenchmarkFailureEvidence,
  type FailurePageEvidence,
  type PageObservation,
  type PagePerformanceSample,
} from "./types";

export type PageRuntimeRole = "host" | "viewer" | "relay";

export function activeVideoEdgeCount(
  page: PageObservation,
  direction: "send" | "receive",
): number {
  return page.connections.filter((connection) => {
    const hasVideo =
      direction === "send"
        ? connection.hasOutboundVideo
        : connection.hasInboundVideo;
    return (
      hasVideo &&
      connection.connectionState !== "closed" &&
      connection.connectionState !== "failed"
    );
  }).length;
}

export function sanitizeFailurePageEvidence(
  page: PageObservation,
): FailurePageEvidence {
  const connectionStates = new Set([
    "new",
    "connecting",
    "connected",
    "disconnected",
    "failed",
    "closed",
  ]);
  const iceConnectionStates = new Set([
    "new",
    "checking",
    "connected",
    "completed",
    "disconnected",
    "failed",
    "closed",
  ]);
  return {
    label: page.label,
    role: page.role,
    authenticated: page.peerId !== null,
    signalingConnected: page.signalingConnected,
    routeRevision: page.routeRevision,
    upstreamKind: page.routeAssignment?.upstream.kind ?? null,
    assignedChildCount: page.routeAssignment?.childPeerIds.length ?? 0,
    firstDecoded: page.firstDecodedAtEpochMs !== null,
    firstRendered: page.firstRenderedAtEpochMs !== null,
    connectionCount: page.connections.length,
    connections: page.connections
      .slice(-MAX_FAILURE_CONNECTIONS_PER_PAGE)
      .map((connection) => ({
        connectionState: connectionStates.has(connection.connectionState)
          ? connection.connectionState
          : "unknown",
        iceConnectionState: iceConnectionStates.has(
          connection.iceConnectionState,
        )
          ? connection.iceConnectionState
          : "unknown",
        identity:
          connection.remotePeerId !== null
            ? "peer"
            : connection.connectionId !== null ? "sfu" : "unidentified",
        hasOutboundVideo: connection.hasOutboundVideo,
        hasInboundVideo: connection.hasInboundVideo,
        decoded: (connection.receiveTotals?.framesTotal ?? 0) > 0,
      })),
  };
}

export function hasAuthoritativeMediaUpstream(page: PageObservation): boolean {
  return (
    page.routeRevision !== null &&
    (page.routeAssignment?.upstream.kind === "peer" ||
      page.routeAssignment?.upstream.kind === "sfu")
  );
}

export function routeParentPeerId(page: PageObservation): string | null {
  return page.routeAssignment?.upstream.kind === "peer"
    ? page.routeAssignment.upstream.peerId
    : null;
}

export function routeChildPeerIds(page: PageObservation): readonly string[] {
  return page.routeAssignment?.childPeerIds ?? [];
}

export function inspectSfuPublication(pages: readonly PageObservation[]) {
  const sfuViewers = pages.filter(
    (page) =>
      page.role === "viewer" && page.routeAssignment?.upstream.kind === "sfu",
  );
  const publicationOwners = pages.filter(
    (page) =>
      page.role === "host" &&
      typeof page.routeAssignment?.sfuPublicationGeneration === "string",
  );
  if (sfuViewers.length === 0) {
    return {
      observed: false,
      rootCount: 0,
      coherent: publicationOwners.length === 0,
    };
  }

  if (publicationOwners.length !== 1) {
    return { observed: true, rootCount: sfuViewers.length, coherent: false };
  }
  const host = publicationOwners[0]!;
  const hostRevision = host.routeRevision;
  const hostGeneration = host.routeAssignment?.sfuPublicationGeneration ?? null;
  const hasExactSfuMedia = (
    page: PageObservation,
    direction: "send" | "receive",
  ): boolean =>
    page.connections.some((connection) => {
      const totals =
        direction === "send"
          ? connection.sendTotals
          : connection.receiveTotals;
      return (
        connection.connectionId !== null &&
        connection.remotePeerId === null &&
        connection.connectionState === "connected" &&
        (direction === "send"
          ? connection.hasOutboundVideo
          : connection.hasInboundVideo) &&
        totals !== null &&
        typeof totals.framesTotal === "number" &&
        totals.framesTotal > 0
      );
    });
  const coherent =
    hostRevision !== null &&
    hostGeneration !== null &&
    pages.every((page) => page.routeRevision === hostRevision) &&
    hasExactSfuMedia(host, "send") &&
    sfuViewers.every(
      (viewer) =>
        viewer.routeAssignment?.sfuPublicationGeneration === hostGeneration &&
        hasExactSfuMedia(viewer, "receive"),
    );
  return { observed: true, rootCount: sfuViewers.length, coherent };
}

export const PAGE_PERFORMANCE_METRICS = {
  Timestamp: "timestampSeconds",
  TaskDuration: "taskDurationSeconds",
  ScriptDuration: "scriptDurationSeconds",
  LayoutDuration: "layoutDurationSeconds",
  RecalcStyleDuration: "recalcStyleDurationSeconds",
  JSHeapUsedSize: "jsHeapUsedBytes",
} as const satisfies Record<string, keyof PagePerformanceSample>;

export function parsePagePerformanceMetrics(
  rawMetrics: unknown,
): PagePerformanceSample | null {
  if (!Array.isArray(rawMetrics)) return null;
  const values = new Map<keyof PagePerformanceSample, number>();
  for (const raw of rawMetrics) {
    if (!raw || typeof raw !== "object") continue;
    const { name, value } = raw as Record<string, unknown>;
    if (
      typeof name !== "string" ||
      !Object.hasOwn(PAGE_PERFORMANCE_METRICS, name)
    ) {
      continue;
    }
    if (typeof value !== "number" || !Number.isFinite(value) || value < 0) {
      return null;
    }
    const key = PAGE_PERFORMANCE_METRICS[
      name as keyof typeof PAGE_PERFORMANCE_METRICS
    ];
    if (values.has(key)) return null;
    values.set(key, value);
  }
  if (values.size !== Object.keys(PAGE_PERFORMANCE_METRICS).length) {
    return null;
  }
  return {
    timestampSeconds: values.get("timestampSeconds")!,
    taskDurationSeconds: values.get("taskDurationSeconds")!,
    scriptDurationSeconds: values.get("scriptDurationSeconds")!,
    layoutDurationSeconds: values.get("layoutDurationSeconds")!,
    recalcStyleDurationSeconds: values.get("recalcStyleDurationSeconds")!,
    jsHeapUsedBytes: values.get("jsHeapUsedBytes")!,
  };
}

export function pageRuntimeRole(page: PageObservation): PageRuntimeRole {
  if (page.role === "host") return "host";
  return activeVideoEdgeCount(page, "send") > 0 ? "relay" : "viewer";
}

export function buildBenchmarkFailureEvidence(
  pages: readonly PageObservation[],
  expectedPageCount: number,
): BenchmarkFailureEvidence {
  return {
    expectedPageCount,
    observedPageCount: pages.length,
    pages: pages.map(sanitizeFailurePageEvidence),
  };
}
