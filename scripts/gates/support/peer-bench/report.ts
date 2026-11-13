// Report aggregation: topology, delivery, sender evidence, resources and the
// pass/fail checks derived from them.

import { DEFAULT_ENDPOINT_MEDIA_COPY_CAPACITY } from "../../../../src/shared/media-copy-accounting";
import { PROFILE_SETTINGS } from "./config";
import {
  activeVideoEdgeCount,
  hasAuthoritativeMediaUpstream,
  inspectSfuPublication,
  pageRuntimeRole,
  routeChildPeerIds,
  routeParentPeerId,
} from "./observation";
import type { PageRuntimeRole } from "./observation";
import type {
  BenchmarkRun,
  PageObservation,
  PageRole,
  ProfileId,
  RecoveryResult,
  RunCheck,
  TimedSample,
} from "./types";

export function summarizeSamples(samples: TimedSample[], viewerCount: number) {
  let maxHostActiveMediaEdges = 0;
  let maxHostAssignedChildren = 0;
  let maxRelayActiveMediaEdges = 0;
  let everyViewerDecoded = false;
  for (const sample of samples) {
    const host = sample.pages.find((page) => page.role === "host");
    if (host) {
      maxHostActiveMediaEdges = Math.max(
        maxHostActiveMediaEdges,
        activeVideoEdgeCount(host, "send"),
        host.maxActiveOutboundMediaEdges,
      );
      maxHostAssignedChildren = Math.max(
        maxHostAssignedChildren,
        routeChildPeerIds(host).length,
        host.maxAssignedChildren,
      );
    }
    for (const page of sample.pages) {
      if (page.role === "viewer") {
        maxRelayActiveMediaEdges = Math.max(
          maxRelayActiveMediaEdges,
          activeVideoEdgeCount(page, "send"),
          page.maxActiveOutboundMediaEdges,
        );
      }
    }
  }
  const finalPages = samples.at(-1)?.pages ?? [];
  const sfuPublication = inspectSfuPublication(finalPages);
  const viewerContinuity = finalPages
    .filter((page) => page.role === "viewer")
    .map((page) => {
      const activeReceive = page.connections.find(
        (connection) =>
          connection.hasInboundVideo &&
          connection.connectionState === "connected" &&
          connection.receiveTotals?.id,
      );
      const series = activeReceive
        ? samples.flatMap((sample) => {
            const sampledPage = sample.pages.find(
              (candidate) => candidate.label === page.label,
            );
            const sampledConnection = sampledPage?.connections.find(
              (connection) =>
                connection.receiveTotals?.id === activeReceive.receiveTotals?.id &&
                (activeReceive.connectionId
                  ? connection.connectionId === activeReceive.connectionId
                  : connection.index === activeReceive.index),
            );
            const frames = sampledConnection?.receiveTotals?.framesTotal;
            return typeof frames === "number" ? [frames] : [];
          })
        : [];
      const statsClean = samples.every((sample) => {
        const sampledPage = sample.pages.find(
          (candidate) => candidate.label === page.label,
        );
        return sampledPage?.connections.every(
          (connection) => connection.error === undefined,
        );
      });
      return {
        label: page.label,
        assigned: hasAuthoritativeMediaUpstream(page),
        routeRevision: page.routeRevision,
        upstreamKind: page.routeAssignment?.upstream.kind ?? null,
        activeUpstream: activeReceive !== undefined,
        statsClean,
        framesIncreased:
          series.length >= 2 && series.at(-1)! > series[0]!,
        firstFramesTotal: series[0] ?? null,
        finalFramesTotal: series.at(-1) ?? null,
      };
    });
  everyViewerDecoded =
    sfuPublication.coherent &&
    viewerContinuity.length === viewerCount &&
    viewerContinuity.every(
      (viewer) =>
        viewer.assigned &&
        viewer.activeUpstream &&
        viewer.statsClean &&
        viewer.framesIncreased,
    );
  const firstFrames = finalPages
    .filter((page) => page.role === "viewer")
    .map((page) => ({
      viewerIndex: page.viewerIndex,
      peerId: page.peerId,
      authenticateSentAtEpochMs: page.authenticateSentAtEpochMs,
      firstDecodedAtEpochMs: page.firstDecodedAtEpochMs,
      firstRenderedAtEpochMs: page.firstRenderedAtEpochMs,
      decodedAfterAuthenticateMs:
        page.firstDecodedAtEpochMs !== null &&
        page.authenticateSentAtEpochMs !== null
          ? page.firstDecodedAtEpochMs - page.authenticateSentAtEpochMs
          : null,
      renderedAfterAuthenticateMs:
        page.firstRenderedAtEpochMs !== null &&
        page.authenticateSentAtEpochMs !== null
          ? page.firstRenderedAtEpochMs - page.authenticateSentAtEpochMs
          : null,
    }));
  const decodedDelays = firstFrames.flatMap((frame) =>
    frame.decodedAfterAuthenticateMs === null
      ? []
      : [frame.decodedAfterAuthenticateMs],
  );
  const finalHost = finalPages.find((page) => page.role === "host");
  const shareToAuthenticateMs =
    finalHost?.shareRequestedAtEpochMs !== null &&
    finalHost?.shareRequestedAtEpochMs !== undefined &&
    finalHost.authenticateSentAtEpochMs !== null
      ? finalHost.authenticateSentAtEpochMs -
        finalHost.shareRequestedAtEpochMs
      : null;
  return {
    maxHostActiveMediaEdges,
    maxHostAssignedChildren,
    maxRelayActiveMediaEdges,
    everyViewerDecoded,
    sfuPublicationObserved: sfuPublication.observed,
    sfuRootCount: sfuPublication.rootCount,
    sfuPublicationCoherent: sfuPublication.coherent,
    viewerContinuity,
    firstFrames,
    maxFirstDecodedAfterAuthenticateMs:
      decodedDelays.length === viewerCount ? Math.max(...decodedDelays) : null,
    hostStartup: {
      shareToAuthenticateMs,
    },
    senderEvidence: {
      scope: "connected outbound video with stable page, connection, and RTP identities",
      host: summarizeSenderEvidence(samples, "host"),
      relay: summarizeSenderEvidence(samples, "viewer"),
    },
    browserProcessResources: summarizeBrowserProcessResources(samples),
    pageRuntimeResources: summarizePageRuntimeResources(samples),
    viewerExperienceByDepth: summarizeViewerExperienceByDepth(samples),
    finalTopology: finalPages.map((page) => ({
      label: page.label,
      role: page.role,
      viewerIndex: page.viewerIndex,
      peerId: page.peerId,
      qualitySettings: page.qualitySettings,
      parentPeerId: routeParentPeerId(page),
      childPeerIds: routeChildPeerIds(page),
      routeRevision: page.routeRevision,
      routeUpstream: page.routeAssignment?.upstream ?? null,
      sfuPublicationGeneration:
        page.routeAssignment?.sfuPublicationGeneration ?? null,
    })),
    finalEdges: finalPages.flatMap((page) =>
      page.connections
        .filter(
          (connection) =>
            connection.hasOutboundVideo || connection.hasInboundVideo,
        )
        .map((connection) => ({
          page: page.label,
          pagePeerId: page.peerId,
          connectionIndex: connection.index,
          connectionId: connection.connectionId,
          remotePeerId: connection.remotePeerId,
          connectionState: connection.connectionState,
          iceConnectionState: connection.iceConnectionState,
          send: connection.send,
          receive: connection.receive,
          sendTotals: connection.sendTotals,
          receiveTotals: connection.receiveTotals,
        })),
    ),
  };
}

export function numericSummary(values: number[]) {
  return {
    sampleCount: values.length,
    min: values.length > 0 ? Math.min(...values) : null,
    max: values.length > 0 ? Math.max(...values) : null,
    mean: values.length > 0
      ? values.reduce((total, value) => total + value, 0) / values.length
      : null,
  };
}

export type ViewerExperienceRoute = "peer" | "sfu";

export interface ViewerExperienceAccumulator {
  route: ViewerExperienceRoute;
  depth: number | null;
  viewers: Set<string>;
  routeSamples: number;
  metricSamples: number;
  framesPerSecond: number[];
  bitrateKbps: number[];
  pixelArea: number[];
  rttMs: number[];
  packetLossPercent: number[];
  jitterMs: number[];
  decodeMs: number[];
  jitterBufferDelayMs: number[];
  resolutions: Set<string>;
  codecs: Set<string>;
  freezeWindows: number;
  freezeCount: number;
  freezeDurationMs: number;
}

export function peerDepths(pages: readonly PageObservation[]): Map<string, number> {
  const byPeer = new Map(
    pages.flatMap((page) => (page.peerId ? [[page.peerId, page] as const] : [])),
  );
  const depths = new Map<string, number>();
  const resolveDepth = (peerId: string, visiting: Set<string>): number | null => {
    const known = depths.get(peerId);
    if (known !== undefined) return known;
    const page = byPeer.get(peerId);
    if (!page || visiting.has(peerId)) return null;
    if (page.role === "host") {
      depths.set(peerId, 0);
      return 0;
    }
    const upstream = page.routeAssignment?.upstream;
    if (upstream?.kind !== "peer") return null;
    const nextVisiting = new Set(visiting).add(peerId);
    const parentDepth = resolveDepth(upstream.peerId, nextVisiting);
    if (parentDepth === null) return null;
    const depth = parentDepth + 1;
    depths.set(peerId, depth);
    return depth;
  };
  for (const peerId of byPeer.keys()) resolveDepth(peerId, new Set());
  return depths;
}

export function committedReceiveMetrics(
  page: PageObservation,
): Record<string, unknown> | null {
  const upstream = page.routeAssignment?.upstream;
  if (!upstream || upstream.kind === "none") return null;
  const matching = page.connections.filter(
    (connection) =>
      connection.hasInboundVideo &&
      connection.connectionState === "connected" &&
      connection.receive !== null &&
      (upstream.kind === "peer"
        ? connection.remotePeerId === upstream.peerId
        : connection.connectionId !== null && connection.remotePeerId === null),
  );
  return matching.length === 1 ? matching[0]!.receive : null;
}

export function emptyViewerExperienceAccumulator(
  route: ViewerExperienceRoute,
  depth: number | null,
): ViewerExperienceAccumulator {
  return {
    route,
    depth,
    viewers: new Set(),
    routeSamples: 0,
    metricSamples: 0,
    framesPerSecond: [],
    bitrateKbps: [],
    pixelArea: [],
    rttMs: [],
    packetLossPercent: [],
    jitterMs: [],
    decodeMs: [],
    jitterBufferDelayMs: [],
    resolutions: new Set(),
    codecs: new Set(),
    freezeWindows: 0,
    freezeCount: 0,
    freezeDurationMs: 0,
  };
}

export function finiteMetric(metrics: Record<string, unknown>, key: string): number | null {
  const value = metrics[key];
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

export function summarizeViewerExperienceByDepth(samples: TimedSample[]) {
  const cohorts = new Map<string, ViewerExperienceAccumulator>();
  let unresolvedRouteSamples = 0;
  const cohort = (route: ViewerExperienceRoute, depth: number | null) => {
    const key = route === "peer" ? `peer:${depth}` : "sfu";
    let current = cohorts.get(key);
    if (!current) {
      current = emptyViewerExperienceAccumulator(route, depth);
      cohorts.set(key, current);
    }
    return current;
  };

  for (const sample of samples) {
    const depths = peerDepths(sample.pages);
    for (const page of sample.pages) {
      if (page.role !== "viewer") continue;
      const upstream = page.routeAssignment?.upstream;
      let current: ViewerExperienceAccumulator | null = null;
      if (upstream?.kind === "peer" && page.peerId) {
        const depth = depths.get(page.peerId) ?? null;
        if (depth !== null && depth > 0) current = cohort("peer", depth);
      } else if (upstream?.kind === "sfu") {
        current = cohort("sfu", null);
      }
      if (!current) {
        unresolvedRouteSamples += 1;
        continue;
      }
      current.routeSamples += 1;
      current.viewers.add(page.label);
      const metrics = committedReceiveMetrics(page);
      if (!metrics) continue;
      current.metricSamples += 1;

      const numericFields = [
        ["framesPerSecond", current.framesPerSecond],
        ["bitrateKbps", current.bitrateKbps],
        ["rttMs", current.rttMs],
        ["packetLossPercent", current.packetLossPercent],
        ["jitterMs", current.jitterMs],
        ["intervalDecodeMs", current.decodeMs],
        ["videoJitterBufferDelayMs", current.jitterBufferDelayMs],
      ] as const;
      for (const [key, target] of numericFields) {
        const value = finiteMetric(metrics, key);
        if (value !== null) target.push(value);
      }
      const width = finiteMetric(metrics, "frameWidth");
      const height = finiteMetric(metrics, "frameHeight");
      if (width !== null && height !== null && width > 0 && height > 0) {
        current.pixelArea.push(width * height);
        current.resolutions.add(`${width}x${height}`);
      }
      const codec = metrics.codec;
      if (typeof codec === "string" && codec.length > 0) {
        current.codecs.add(codec);
      }
      const freezeCount = finiteMetric(metrics, "intervalFreezeCount");
      if (freezeCount !== null && freezeCount > 0) {
        current.freezeWindows += 1;
        current.freezeCount += freezeCount;
      }
      const freezeDuration = finiteMetric(metrics, "intervalFreezeDurationMs");
      if (freezeDuration !== null && freezeDuration > 0) {
        current.freezeDurationMs += freezeDuration;
      }
    }
  }

  return {
    scope:
      "exact committed Viewer receive metrics grouped by Peer depth; SFU is a separate route cohort",
    unresolvedRouteSamples,
    cohorts: [...cohorts.values()]
      .sort((left, right) => {
        if (left.route !== right.route) return left.route === "peer" ? -1 : 1;
        return (left.depth ?? Number.MAX_SAFE_INTEGER) -
          (right.depth ?? Number.MAX_SAFE_INTEGER);
      })
      .map((current) => ({
        route: current.route,
        depth: current.depth,
        viewerCount: current.viewers.size,
        routeSampleCount: current.routeSamples,
        metricSampleCount: current.metricSamples,
        missingMetricSamples: current.routeSamples - current.metricSamples,
        framesPerSecond: numericSummary(current.framesPerSecond),
        bitrateKbps: numericSummary(current.bitrateKbps),
        pixelArea: numericSummary(current.pixelArea),
        rttMs: numericSummary(current.rttMs),
        packetLossPercent: numericSummary(current.packetLossPercent),
        jitterMs: numericSummary(current.jitterMs),
        decodeMs: numericSummary(current.decodeMs),
        jitterBufferDelayMs: numericSummary(current.jitterBufferDelayMs),
        resolutions:
          current.resolutions.size > 0 ? [...current.resolutions].sort() : null,
        codecs: current.codecs.size > 0 ? [...current.codecs].sort() : null,
        freezeWindows: current.freezeWindows,
        freezeCount: current.freezeCount,
        freezeDurationMs: current.freezeDurationMs,
      })),
  };
}

export function summarizeSenderEvidence(samples: TimedSample[], role: PageRole) {
  const identities = new Set<string>();
  const bitrateKbps: number[] = [], framesPerSecond: number[] = [];
  const captureFramesPerSecond: number[] = [];
  const mediaSourceFramesPerSecond: number[] = [];
  const availableOutgoingKbps: number[] = [];
  const resolutions = new Set<string>();
  const codecs = new Set<string>();
  const encoderImplementations = new Set<string>();
  const scalabilityModes = new Set<string>();
  const qualityLimitationReasonSamples: Record<string, number> = {};
  let unknownIdentitySamples = 0;
  let unknownQualityLimitationSamples = 0;
  let powerEfficientEncoderTrueSamples = 0;
  let powerEfficientEncoderFalseSamples = 0;
  let powerEfficientEncoderUnknownSamples = 0;
  let encodeIntervalCount = 0, framesEncoded = 0, encodeTimeMs = 0;
  const finite = (value: unknown): value is number =>
    typeof value === "number" && Number.isFinite(value);

  for (const sample of samples) {
    for (const page of sample.pages.filter((candidate) => candidate.role === role)) {
      for (const connection of page.connections) {
        if (!connection.hasOutboundVideo || connection.connectionState !== "connected" || !connection.send) continue;
        const rtpStatsId = connection.send.rtpStatsId;
        if (typeof rtpStatsId !== "string" || rtpStatsId.length === 0) {
          unknownIdentitySamples += 1;
          continue;
        }
        identities.add(`${page.label}:${connection.createdAtEpochMs}:${connection.connectionId ?? connection.index}:${rtpStatsId}`);
        if (finite(connection.send.bitrateKbps)) bitrateKbps.push(connection.send.bitrateKbps);
        if (finite(connection.send.framesPerSecond)) framesPerSecond.push(connection.send.framesPerSecond);
        if (finite(connection.send.captureFramesPerSecond)) captureFramesPerSecond.push(connection.send.captureFramesPerSecond);
        if (finite(connection.send.mediaSourceFramesPerSecond)) mediaSourceFramesPerSecond.push(connection.send.mediaSourceFramesPerSecond);
        if (finite(connection.send.availableOutgoingKbps)) availableOutgoingKbps.push(connection.send.availableOutgoingKbps);
        if (typeof connection.send.resolution === "string" && /^\d+x\d+$/.test(connection.send.resolution)) resolutions.add(connection.send.resolution);
        if (typeof connection.send.codec === "string" && connection.send.codec.length > 0) codecs.add(connection.send.codec);
        if (typeof connection.send.encoderImplementation === "string" && connection.send.encoderImplementation.length > 0) encoderImplementations.add(connection.send.encoderImplementation);
        if (typeof connection.send.scalabilityMode === "string" && connection.send.scalabilityMode.length > 0) scalabilityModes.add(connection.send.scalabilityMode);
        if (connection.send.powerEfficientEncoder === true) {
          powerEfficientEncoderTrueSamples += 1;
        } else if (connection.send.powerEfficientEncoder === false) {
          powerEfficientEncoderFalseSamples += 1;
        } else {
          powerEfficientEncoderUnknownSamples += 1;
        }
        const reason = connection.send.qualityLimitationReason;
        if (typeof reason === "string" && reason.length > 0) {
          qualityLimitationReasonSamples[reason] = (qualityLimitationReasonSamples[reason] ?? 0) + 1;
        } else {
          unknownQualityLimitationSamples += 1;
        }
        const intervalFrames = connection.send.intervalFramesEncoded;
        const intervalTime = connection.send.intervalEncodeTimeMs;
        if (finite(intervalFrames) && intervalFrames >= 0 && finite(intervalTime) && intervalTime >= 0) {
          encodeIntervalCount += 1;
          framesEncoded += intervalFrames;
          encodeTimeMs += intervalTime;
        }
      }
    }
  }
  return {
    uniqueSenderCount: identities.size,
    unknownIdentitySamples,
    bitrateKbps: numericSummary(bitrateKbps),
    framesPerSecond: numericSummary(framesPerSecond),
    captureFramesPerSecond: numericSummary(captureFramesPerSecond),
    mediaSourceFramesPerSecond: numericSummary(mediaSourceFramesPerSecond),
    resolutions: resolutions.size > 0 ? [...resolutions].sort() : null,
    codecs: codecs.size > 0 ? [...codecs].sort() : null,
    encoderImplementations:
      encoderImplementations.size > 0
        ? [...encoderImplementations].sort()
        : null,
    scalabilityModes:
      scalabilityModes.size > 0 ? [...scalabilityModes].sort() : null,
    powerEfficientEncoderSamples: {
      true: powerEfficientEncoderTrueSamples,
      false: powerEfficientEncoderFalseSamples,
      unknown: powerEfficientEncoderUnknownSamples,
    },
    availableOutgoingKbps: numericSummary(availableOutgoingKbps),
    encodeIntervals: {
      sampleCount: encodeIntervalCount,
      framesEncoded: encodeIntervalCount > 0 ? framesEncoded : null,
      encodeTimeMs: encodeIntervalCount > 0 ? encodeTimeMs : null,
      meanEncodeMsPerFrame: framesEncoded > 0 ? encodeTimeMs / framesEncoded : null,
    },
    qualityLimitationReasonSamples: Object.keys(qualityLimitationReasonSamples).length > 0
      ? qualityLimitationReasonSamples : null,
    unknownQualityLimitationSamples,
  };
}

export function summarizeBrowserProcessResources(samples: TimedSample[]) {
  let validCpuIntervals = 0, invalidCpuIntervals = 0;
  let measuredCpuTimeSeconds = 0, measuredWallTimeSeconds = 0;
  let peakIntervalCpuUtilizationPercent: number | null = null;
  for (let index = 1; index < samples.length; index += 1) {
    const previous = samples[index - 1]!;
    const current = samples[index]!;
    const wallSeconds = (current.atEpochMs - previous.atEpochMs) / 1_000;
    const previousProcesses = previous.browserProcesses?.processes;
    const currentProcesses = current.browserProcesses?.processes;
    const previousByIdentity = new Map(previousProcesses?.map((process) => [`${process.type}:${process.id}`, process]));
    let cpuSeconds = 0;
    const valid = wallSeconds > 0 && previousProcesses && currentProcesses &&
      previousProcesses.length === currentProcesses.length && previousByIdentity.size === previousProcesses.length &&
      currentProcesses.every((process) => {
        const baseline = previousByIdentity.get(`${process.type}:${process.id}`);
        if (!baseline || process.cpuTimeSeconds < baseline.cpuTimeSeconds) return false;
        cpuSeconds += process.cpuTimeSeconds - baseline.cpuTimeSeconds;
        return true;
      });
    if (!valid) {
      invalidCpuIntervals += 1;
      continue;
    }
    validCpuIntervals += 1;
    measuredCpuTimeSeconds += cpuSeconds;
    measuredWallTimeSeconds += wallSeconds;
    const utilization = (cpuSeconds / wallSeconds) * 100;
    peakIntervalCpuUtilizationPercent = Math.max(peakIntervalCpuUtilizationPercent ?? 0, utilization);
  }
  return {
    source: "CDP SystemInfo.getProcessInfo",
    scope: "all CDP-reported processes in the isolated benchmark Chromium instance",
    validCpuIntervals,
    invalidCpuIntervals,
    measuredCpuTimeSeconds: validCpuIntervals ? measuredCpuTimeSeconds : null,
    measuredWallTimeSeconds: validCpuIntervals ? measuredWallTimeSeconds : null,
    averageCpuUtilizationPercent: measuredWallTimeSeconds > 0
      ? (measuredCpuTimeSeconds / measuredWallTimeSeconds) * 100 : null,
    peakIntervalCpuUtilizationPercent,
    peakResidentSetBytes: null,
  };
}

interface PageRuntimeAccumulator {
  validIntervals: number;
  invalidIntervals: number;
  wallSeconds: number;
  taskSeconds: number;
  scriptSeconds: number;
  layoutSeconds: number;
  recalcStyleSeconds: number;
  peakTaskUtilizationPercent: number | null;
  peakJsHeapUsedBytes: number | null;
}

function emptyPageRuntimeAccumulator(): PageRuntimeAccumulator {
  return {
    validIntervals: 0,
    invalidIntervals: 0,
    wallSeconds: 0,
    taskSeconds: 0,
    scriptSeconds: 0,
    layoutSeconds: 0,
    recalcStyleSeconds: 0,
    peakTaskUtilizationPercent: null,
    peakJsHeapUsedBytes: null,
  };
}

function finishPageRuntime(accumulator: PageRuntimeAccumulator) {
  return {
    validIntervals: accumulator.validIntervals,
    invalidIntervals: accumulator.invalidIntervals,
    measuredWallTimeSeconds:
      accumulator.validIntervals > 0 ? accumulator.wallSeconds : null,
    taskTimeSeconds:
      accumulator.validIntervals > 0 ? accumulator.taskSeconds : null,
    averageTaskUtilizationPercent:
      accumulator.wallSeconds > 0
        ? (accumulator.taskSeconds / accumulator.wallSeconds) * 100
        : null,
    peakTaskUtilizationPercent: accumulator.peakTaskUtilizationPercent,
    scriptTimeSeconds:
      accumulator.validIntervals > 0 ? accumulator.scriptSeconds : null,
    layoutTimeSeconds:
      accumulator.validIntervals > 0 ? accumulator.layoutSeconds : null,
    recalcStyleTimeSeconds:
      accumulator.validIntervals > 0
        ? accumulator.recalcStyleSeconds
        : null,
    peakJsHeapUsedBytes: accumulator.peakJsHeapUsedBytes,
  };
}

export function summarizePageRuntimeResources(samples: TimedSample[]) {
  const accumulators: Record<PageRuntimeRole, PageRuntimeAccumulator> = {
    host: emptyPageRuntimeAccumulator(),
    viewer: emptyPageRuntimeAccumulator(),
    relay: emptyPageRuntimeAccumulator(),
  };
  for (const sample of samples) {
    for (const page of sample.pages) {
      const heap = page.performance?.jsHeapUsedBytes;
      if (heap === undefined) continue;
      const accumulator = accumulators[pageRuntimeRole(page)];
      accumulator.peakJsHeapUsedBytes = Math.max(
        accumulator.peakJsHeapUsedBytes ?? 0,
        heap,
      );
    }
  }
  for (let index = 1; index < samples.length; index += 1) {
    const previous = samples[index - 1]!;
    const current = samples[index]!;
    const previousPages = new Map(
      previous.pages.map((page) => [page.label, page]),
    );
    for (const page of current.pages) {
      const role = pageRuntimeRole(page);
      const accumulator = accumulators[role];
      const before = previousPages.get(page.label)?.performance;
      const after = page.performance;
      if (!before && !after) continue;
      const wallSeconds =
        before && after
          ? after.timestampSeconds - before.timestampSeconds
          : null;
      const deltas = before && after
        ? {
            task: after.taskDurationSeconds - before.taskDurationSeconds,
            script: after.scriptDurationSeconds - before.scriptDurationSeconds,
            layout: after.layoutDurationSeconds - before.layoutDurationSeconds,
            recalc:
              after.recalcStyleDurationSeconds -
              before.recalcStyleDurationSeconds,
          }
        : null;
      if (
        wallSeconds === null ||
        !Number.isFinite(wallSeconds) ||
        wallSeconds <= 0 ||
        !deltas ||
        Object.values(deltas).some(
          (value) => !Number.isFinite(value) || value < 0,
        )
      ) {
        accumulator.invalidIntervals += 1;
        continue;
      }
      accumulator.validIntervals += 1;
      accumulator.wallSeconds += wallSeconds;
      accumulator.taskSeconds += deltas.task;
      accumulator.scriptSeconds += deltas.script;
      accumulator.layoutSeconds += deltas.layout;
      accumulator.recalcStyleSeconds += deltas.recalc;
      accumulator.peakTaskUtilizationPercent = Math.max(
        accumulator.peakTaskUtilizationPercent ?? 0,
        (deltas.task / wallSeconds) * 100,
      );
    }
  }
  return {
    source: "CDP Performance.getMetrics",
    scope:
      "Host plus one representative plain Viewer and relay per sample; relay means a Viewer page with active outbound video",
    host: finishPageRuntime(accumulators.host),
    viewer: finishPageRuntime(accumulators.viewer),
    relay: finishPageRuntime(accumulators.relay),
  };
}

export function buildRunChecks(
  summary: NonNullable<BenchmarkRun["summary"]>,
  viewerCount: number,
  profileId: ProfileId,
  expectedEndpointCap = DEFAULT_ENDPOINT_MEDIA_COPY_CAPACITY,
): RunCheck[] {
  const sfuConsistencyCheck: RunCheck = summary.sfuPublicationObserved
    ? {
        name: "sfu-route-consistency",
        passed: summary.sfuPublicationCoherent,
        actual: summary.sfuRootCount,
        expected:
          "admitted SFU subscriptions, one Host publication, and one active revision across all participants",
      }
    : {
        name: "no-orphan-sfu-publication",
        passed: summary.sfuPublicationCoherent,
        actual: summary.sfuPublicationCoherent,
        expected: "no SFU publication generation when no SFU root is active",
      };
  return [
    {
      name: "host-active-media-edges",
      passed: summary.maxHostActiveMediaEdges <= expectedEndpointCap,
      actual: summary.maxHostActiveMediaEdges,
      expected: `<= ${expectedEndpointCap}`,
    },
    {
      name: "host-assigned-children",
      passed: summary.maxHostAssignedChildren <= expectedEndpointCap,
      actual: summary.maxHostAssignedChildren,
      expected: `<= ${expectedEndpointCap}`,
    },
    {
      name: "relay-active-media-edges",
      passed: summary.maxRelayActiveMediaEdges <= expectedEndpointCap,
      actual: summary.maxRelayActiveMediaEdges,
      expected: `<= ${expectedEndpointCap}`,
    },
    {
      name: "all-viewers-decoded",
      passed: summary.everyViewerDecoded,
      actual: summary.everyViewerDecoded,
      expected: `${viewerCount} viewers with a live upstream, clean stats, and increasing decoded frames`,
    },
    sfuConsistencyCheck,
    {
      name: "quality-settings-propagated",
      passed: summary.finalTopology.every(
        (participant) =>
          JSON.stringify(participant.qualitySettings) ===
          JSON.stringify(PROFILE_SETTINGS[profileId]),
      ),
      actual: summary.finalTopology.every(
        (participant) =>
          JSON.stringify(participant.qualitySettings) ===
          JSON.stringify(PROFILE_SETTINGS[profileId]),
      ),
      expected: `${profileId} quality settings on every participant`,
    },
  ];
}

export function buildRecoveryCheck(
  recovery: RecoveryResult,
  expectedEndpointCap = DEFAULT_ENDPOINT_MEDIA_COPY_CAPACITY,
): RunCheck {
  const passed =
    recovery.triggered &&
    recovery.error === undefined &&
    recovery.recoveredAtEpochMs !== undefined &&
    recovery.maxHostActiveMediaEdges !== undefined &&
    recovery.maxHostActiveMediaEdges <= expectedEndpointCap &&
    recovery.maxHostAssignedChildren !== undefined &&
    recovery.maxHostAssignedChildren <= expectedEndpointCap;
  return {
    name: "relay-recovery-correctness",
    passed,
    actual: passed,
    expected: `decoded frames resume with host media edges and assigned children <= ${expectedEndpointCap}`,
  };
}
