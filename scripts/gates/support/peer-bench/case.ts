// One measured case: join, settle, sample, optional quality smoke and relay
// recovery, then report checks and failure evidence.

import { qualitySettingsEqual } from "../../../../src/client/media/quality";
import type { QualitySettings } from "../../../../src/client/media/quality";
import { PROFILE_SETTINGS } from "./config";
import {
  closePage,
  createPage,
  delay,
  errorMessage,
  evaluate,
  progressSample,
  quickSnapshot,
  requestRouteTimingSummary,
  samplePages,
  startHost,
  waitForPage,
  waitForViewerMedia,
  type CdpConnection,
} from "./harness";
import { buildBenchmarkFailureEvidence } from "./observation";
import {
  activeVideoEdgeCount,
  routeChildPeerIds,
  routeParentPeerId,
} from "./observation";
import { buildRouteTimingCheck } from "./route-timing";
import { buildRecoveryCheck, buildRunChecks, summarizeSamples } from "./report";
import {
  MAX_VIEWERS,
  type BenchmarkConfig,
  type BenchmarkFailureEvidence,
  type PageHandle,
  type BenchmarkRouteTimingSummary,
  type BenchmarkRun,
  type PageObservation,
  type RecoveryHostPeaks,
  type RecoveryResult,
  type RecoveryViewerBaseline,
  type TimedSample,
} from "./types";

export async function joinViewerBurst<T>(
  viewerCount: number,
  createViewer: (viewerIndex: number) => Promise<T>,
  waitForMedia: (viewer: T, viewerIndex: number) => Promise<unknown>,
): Promise<T[]> {
  const creations = await Promise.allSettled(
    Array.from({ length: viewerCount }, (_, index) =>
      createViewer(index + 1),
    ),
  );
  const creationFailure = creations.find(
    (result): result is PromiseRejectedResult => result.status === "rejected",
  );
  if (creationFailure) {
    throw creationFailure.reason;
  }
  const viewers = creations.map(
    (result) => (result as PromiseFulfilledResult<T>).value,
  );
  const media = await Promise.allSettled(
    viewers.map((viewer, index) => waitForMedia(viewer, index + 1)),
  );
  const mediaFailure = media.find(
    (result): result is PromiseRejectedResult => result.status === "rejected",
  );
  if (mediaFailure) {
    throw mediaFailure.reason;
  }
  return viewers;
}

export async function captureFailureEvidence(
  cdp: CdpConnection,
  pages: readonly PageHandle[],
  expectedPageCount: number,
): Promise<BenchmarkFailureEvidence> {
  const observations = await Promise.allSettled(
    pages
      .slice(0, MAX_VIEWERS + 1)
      .map((page) => progressSample(cdp, page)),
  );
  return buildBenchmarkFailureEvidence(
    observations.flatMap((result) =>
      result.status === "fulfilled" ? [result.value] : [],
    ),
    expectedPageCount,
  );
}

export function peerConnectionFingerprint(pages: PageObservation[]): string {
  return pages
    .map((page) =>
      [
        page.label,
        ...page.connections.map(
          (connection) =>
            `${connection.index}:${connection.connectionId ?? "?"}`,
        ),
      ].join("|"),
    )
    .sort()
    .join("\n");
}

export async function applyQualityPreference(
  cdp: CdpConnection,
  hostPage: PageHandle,
  preference: QualitySettings["degradationPreference"],
  signal: AbortSignal,
): Promise<void> {
  const serializedPreference = JSON.stringify(preference);
  await evaluate(
    cdp,
    hostPage,
    `(() => {
      const toggle = document.querySelector('[aria-controls="host-advanced-door"]');
      if (!(toggle instanceof HTMLButtonElement)) return false;
      if (toggle.getAttribute('aria-expanded') !== 'true') toggle.click();
      return true;
    })()`,
  );
  const buttonPredicate =
    `Array.from(document.querySelectorAll('button[name="degradationPreference"]'))` +
    `.some((item) => item.value === ${serializedPreference})`;
  await waitForPage(
    cdp,
    hostPage,
    buttonPredicate,
    5_000,
    "advanced quality controls",
    signal,
  );
  await evaluate(
    cdp,
    hostPage,
    `(() => {
      const button = Array.from(
        document.querySelectorAll('button[name="degradationPreference"]'),
      ).find((item) => item.value === ${serializedPreference});
      if (!(button instanceof HTMLButtonElement)) return false;
      button.click();
      return true;
    })()`,
  );
  await waitForPage(
    cdp,
    hostPage,
    `Array.from(document.querySelectorAll('button[name="degradationPreference"]')).some((item) => item.value === ${serializedPreference} && item.getAttribute('aria-pressed') === 'true')`,
    5_000,
    `${preference} advanced quality selection`,
    signal,
  );
}

export async function runQualityControlSmoke(
  cdp: CdpConnection,
  pages: PageHandle[],
  initialSettings: QualitySettings,
  signal: AbortSignal,
): Promise<{
  settingsPropagated: boolean;
  peerConnectionsStable: boolean;
  senderReadbacksMatched: boolean;
  viewersAdvanced: boolean;
}> {
  const hostPage = pages[0];
  if (!hostPage) {
    throw new Error("Quality control smoke requires a host page");
  }
  const baseline = await Promise.all(pages.map((page) => quickSnapshot(cdp, page)));
  const baselineFingerprint = peerConnectionFingerprint(baseline);
  const balanced = {
    ...initialSettings,
    degradationPreference: "balanced",
  } as const satisfies QualitySettings;
  const clarity = {
    ...initialSettings,
    degradationPreference: "maintain-resolution",
  } as const satisfies QualitySettings;

  const waitForSettings = async (settings: QualitySettings): Promise<boolean> => {
    const serializedSettings = JSON.stringify(JSON.stringify(settings));
    await Promise.all(
      pages.map((page) =>
        waitForPage(
          cdp,
          page,
          `JSON.stringify(globalThis.__PIIK_BENCHMARK__.snapshot().qualitySettings) === ${serializedSettings}`,
          10_000,
          `${settings.degradationPreference} quality propagation`,
          signal,
        ),
      ),
    );
    const snapshots = await Promise.all(
      pages.map((page) => quickSnapshot(cdp, page)),
    );
    return snapshots.every(
      (page) =>
        page.qualitySettings !== null &&
        qualitySettingsEqual(page.qualitySettings, settings),
    );
  };
  const activeSenderCounts = new Map(
    baseline.map((page) => [page.label, activeVideoEdgeCount(page, "send")]),
  );
  const sendingPages = pages.filter(
    (page) => (activeSenderCounts.get(page.label) ?? 0) > 0,
  );
  const waitForReadback = async (
    preference: QualitySettings["degradationPreference"],
  ): Promise<boolean> => {
    const matched = await Promise.all(
      sendingPages.map(async (page) => {
        const expectedCount = activeSenderCounts.get(page.label) ?? 0;
        const predicate = `(() => {
          const values = globalThis.__PIIK_BENCHMARK__.senderDegradationPreferences();
          return values.length === ${expectedCount} &&
            values.every((value) => value === ${JSON.stringify(preference)});
        })()`;
        await waitForPage(
          cdp,
          page,
          predicate,
          10_000,
          `${preference} sender parameter readback`,
          signal,
        ).catch(async error => {
          const values = await evaluate(cdp, page, "globalThis.__PIIK_BENCHMARK__.senderDegradationPreferences()");
          throw new Error(`${errorMessage(error)}; expected ${expectedCount} sender(s), received ${JSON.stringify(values)}`);
        });
        return evaluate<boolean>(cdp, page, `Boolean(${predicate})`);
      }),
    );
    return (
      sendingPages.length > 0 &&
      matched.length === sendingPages.length &&
      matched.every(Boolean)
    );
  };
  const viewerPages = pages.slice(1);
  const waitForViewerProgress = async (): Promise<boolean> => {
    for (const page of viewerPages) {
      await cdp.call("Page.bringToFront", {}, page.sessionId);
      const before = [await progressSample(cdp, page)];
      const deadline = Date.now() + 10_000;
      let advanced = false;
      while (Date.now() < deadline) {
        const after = [await progressSample(cdp, page)];
        if (everyViewerAdvanced(before, after)) {
          advanced = true;
          break;
        }
        await delay(100, signal);
      }
      if (!advanced) {
        return false;
      }
    }
    return viewerPages.length > 0;
  };

  const runStep = async (
    preference: QualitySettings["degradationPreference"],
    settings: QualitySettings,
  ): Promise<{
    settingsPropagated: boolean;
    senderReadbacksMatched: boolean;
    viewersAdvanced: boolean;
  }> => {
    await cdp.call("Page.bringToFront", {}, hostPage.sessionId);
    await applyQualityPreference(cdp, hostPage, preference, signal);
    const settingsPropagated = await waitForSettings(settings);
    const senderReadbacksMatched = await waitForReadback(preference);
    const viewersAdvanced = await waitForViewerProgress();
    return {
      settingsPropagated,
      senderReadbacksMatched,
      viewersAdvanced,
    };
  };

  const clarityResult = await runStep("maintain-resolution", clarity);
  const balancedResult = await runStep("balanced", balanced);

  const final = await Promise.all(pages.map((page) => quickSnapshot(cdp, page)));

  return {
    settingsPropagated:
      balancedResult.settingsPropagated && clarityResult.settingsPropagated,
    peerConnectionsStable:
      peerConnectionFingerprint(final) === baselineFingerprint,
    senderReadbacksMatched:
      balancedResult.senderReadbacksMatched &&
      clarityResult.senderReadbacksMatched,
    viewersAdvanced:
      balancedResult.viewersAdvanced && clarityResult.viewersAdvanced,
  };
}

export function totalDecodedFrames(page: PageObservation): number {
  return page.connections.reduce(
    (total, connection) => total + (connection.receiveTotals?.framesTotal ?? 0),
    0,
  );
}

export function everyViewerAdvanced(
  before: PageObservation[],
  after: PageObservation[],
): boolean {
  const baselineViewers = before.filter((page) => page.role === "viewer");
  const currentByLabel = new Map(after.map((page) => [page.label, page]));
  return (
    baselineViewers.length > 0 &&
    baselineViewers.every((baseline) => {
      const current = currentByLabel.get(baseline.label);
      return (
        current !== undefined &&
        totalDecodedFrames(current) > totalDecodedFrames(baseline) &&
        current.renderedFrames > baseline.renderedFrames
      );
    })
  );
}

export function recoveryViewerBaseline(
  page: PageObservation,
): RecoveryViewerBaseline | null {
  const upstream = page.routeAssignment?.upstream;
  if (page.routeRevision === null || !upstream || upstream.kind === "none") {
    return null;
  }
  const parentPeerId = upstream.kind === "peer" ? upstream.peerId : null;
  const candidates = page.connections.filter((connection) => {
    const framesTotal = connection.receiveTotals?.framesTotal;
    return (
      connection.hasInboundVideo &&
      connection.connectionState === "connected" &&
      typeof connection.receiveTotals?.id === "string" &&
      connection.receiveTotals.id.length > 0 &&
      typeof framesTotal === "number" &&
      Number.isFinite(framesTotal) &&
      framesTotal >= 0 &&
      (parentPeerId === null || connection.remotePeerId === parentPeerId)
    );
  });
  if (candidates.length !== 1) {
    return null;
  }
  const connection = candidates[0]!;
  return {
    label: page.label,
    routeRevision: page.routeRevision,
    upstreamKind: upstream.kind,
    parentPeerId,
    connectionId: connection.connectionId,
    connectionIndex: connection.index,
    rtpId: connection.receiveTotals!.id,
    framesTotal: connection.receiveTotals!.framesTotal!,
  };
}

export function captureRecoveryViewerBaselines(
  pages: PageObservation[],
): RecoveryViewerBaseline[] | null {
  const viewers = pages.filter((page) => page.role === "viewer");
  const baselines = viewers.map(recoveryViewerBaseline);
  return viewers.length > 0 && baselines.every((baseline) => baseline !== null)
    ? baselines
    : null;
}

export function everyViewerRecoveredMedia(
  baselines: RecoveryViewerBaseline[] | null,
  after: PageObservation[],
): boolean {
  if (!baselines || baselines.length === 0) {
    return false;
  }
  const currentViewers = after.filter((page) => page.role === "viewer");
  const currentByLabel = new Map(currentViewers.map((page) => [page.label, page]));
  return (
    currentViewers.length === baselines.length &&
    currentByLabel.size === baselines.length &&
    baselines.every((baseline) => {
      const currentPage = currentByLabel.get(baseline.label);
      const current = currentPage ? recoveryViewerBaseline(currentPage) : null;
      return (
        current !== null &&
        current.routeRevision === baseline.routeRevision &&
        current.upstreamKind === baseline.upstreamKind &&
        current.parentPeerId === baseline.parentPeerId &&
        current.connectionId === baseline.connectionId &&
        current.connectionIndex === baseline.connectionIndex &&
        current.rtpId === baseline.rtpId &&
        current.framesTotal > baseline.framesTotal
      );
    })
  );
}

export function mergeRecoveryHostPeaks(
  peaks: RecoveryHostPeaks,
  host: PageObservation,
): RecoveryHostPeaks {
  if (
    !Number.isFinite(host.maxActiveOutboundMediaEdges) ||
    !Number.isFinite(host.maxAssignedChildren)
  ) {
    return peaks;
  }
  return {
    maxHostActiveMediaEdges: Math.max(
      peaks.maxHostActiveMediaEdges ?? 0,
      activeVideoEdgeCount(host, "send"),
      host.maxActiveOutboundMediaEdges,
    ),
    maxHostAssignedChildren: Math.max(
      peaks.maxHostAssignedChildren ?? 0,
      routeChildPeerIds(host).length,
      host.maxAssignedChildren,
    ),
  };
}

export function descendantsOf(
  rootPeerId: string,
  pages: PageObservation[],
): string[] {
  const descendants: string[] = [];
  const queue = [rootPeerId];
  while (queue.length > 0) {
    const parent = queue.shift()!;
    for (const page of pages) {
      if (
        page.peerId &&
        routeParentPeerId(page) === parent &&
        !descendants.includes(page.peerId)
      ) {
        descendants.push(page.peerId);
        queue.push(page.peerId);
      }
    }
  }
  return descendants;
}

export async function runRecovery(
  cdp: CdpConnection,
  pages: PageHandle[],
  finalPages: PageObservation[],
  timeoutMs: number,
  signal: AbortSignal,
): Promise<RecoveryResult> {
  const host = finalPages.find((page) => page.role === "host");
  if (!host?.peerId) {
    return { triggered: true, error: "Host topology was unavailable" };
  }
  const relay = finalPages.find(
    (page) =>
      page.role === "viewer" &&
      routeParentPeerId(page) === host.peerId &&
      routeChildPeerIds(page).length > 0,
  );
  if (!relay?.peerId) {
    return { triggered: true, error: "No first-level relay was available" };
  }
  const affectedPeerIds = descendantsOf(relay.peerId, finalPages);
  const relayHandle = pages.find((page) => page.label === relay.label);
  if (!relayHandle || affectedPeerIds.length === 0) {
    return { triggered: true, error: "Relay had no measurable descendant branch" };
  }
  const failureAtEpochMs = Date.now();
  await closePage(cdp, relayHandle);
  const remainingPages = pages.filter((page) => page !== relayHandle);
  const deadline = failureAtEpochMs + timeoutMs;
  let reassignedAtEpochMs: number | undefined;
  let remainingViewerBaselines: RecoveryViewerBaseline[] | null = null;
  let hostPeaks: RecoveryHostPeaks = {};

  while (Date.now() < deadline) {
    const observations = await Promise.all(
      remainingPages.map((page) => progressSample(cdp, page)),
    );
    const currentHost = observations.find((page) => page.role === "host");
    if (currentHost) {
      hostPeaks = mergeRecoveryHostPeaks(hostPeaks, currentHost);
    }
    const affected = observations.filter((page) =>
      page.peerId ? affectedPeerIds.includes(page.peerId) : false,
    );
    const branchReassigned =
      affected.length === affectedPeerIds.length &&
      affected.every(
        (page) =>
          routeParentPeerId(page) !== null &&
          routeParentPeerId(page) !== relay.peerId,
      );
    if (!reassignedAtEpochMs && branchReassigned) {
      reassignedAtEpochMs = Date.now();
    }
    if (
      reassignedAtEpochMs &&
      branchReassigned &&
      remainingViewerBaselines === null
    ) {
      const baselines = captureRecoveryViewerBaselines(observations);
      const remainingViewerCount = remainingPages.length - 1;
      if (baselines?.length === remainingViewerCount) {
        remainingViewerBaselines = baselines;
      }
    } else if (
      reassignedAtEpochMs &&
      everyViewerRecoveredMedia(remainingViewerBaselines, observations)
    ) {
      const recoveredAtEpochMs = Date.now();
      return {
        triggered: true,
        relayPeerId: relay.peerId,
        affectedPeerIds,
        failureAtEpochMs,
        reassignedAtEpochMs,
        recoveredAtEpochMs,
        recoveryMs: recoveredAtEpochMs - failureAtEpochMs,
        ...hostPeaks,
      };
    }
    await delay(100, signal);
  }
  return {
    triggered: true,
    relayPeerId: relay.peerId,
    affectedPeerIds,
    failureAtEpochMs,
    reassignedAtEpochMs,
    ...hostPeaks,
    error: "Recovery did not resume decoded frames before the timeout",
  };
}

export async function runCase(
  cdp: CdpConnection,
  baseUrl: string,
  config: BenchmarkConfig,
  viewerCount: number,
  signal: AbortSignal,
): Promise<BenchmarkRun> {
  const startedAtMs = Date.now();
  const pages: PageHandle[] = [];
  const samples: TimedSample[] = [];
  let recovery: RecoveryResult = { triggered: false };
  let routeTimingSummary: BenchmarkRouteTimingSummary | null = null;
  let routeTimingStatus: BenchmarkRun["routeTimingStatus"] = "not-requested";
  try {
    const capture = PROFILE_SETTINGS[config.profileId];
    const captureResolution = capture.resolution;
    const commonInit = {
      width: captureResolution.width,
      height: captureResolution.height,
      frameRate: capture.maxFramerate,
      expectedEndpointCap: config.expectedEndpointCap,
      qualityControlSmoke: config.qualityControlSmoke,
    };
    const hostPage = await createPage(cdp, baseUrl, {
      ...commonInit,
      label: `host-${viewerCount}`,
      role: "host",
      viewerIndex: null,
      clearHostRoom: true,
    }, signal);
    pages.push(hostPage);
    const host = await startHost(
      cdp,
      hostPage,
      config.profileId,
      config.codecMode,
      config.connectionTimeoutMs,
      signal,
    );
    const roomId = host.roomId!;
    const viewerUrl = await evaluate<string>(
      cdp,
      hostPage,
      `(() => {
        const grant = sessionStorage.getItem('piik:viewer-grant:${roomId}');
        return grant ? location.origin + '/r/${roomId}#v=' + grant : '';
      })()`,
    );
    if (!viewerUrl.startsWith(`${baseUrl}/r/${roomId}#v=`)) {
      throw new Error("Host private Viewer invite was unavailable");
    }
    const viewerPages = await joinViewerBurst(
      viewerCount,
      async (viewerIndex) => {
        const viewerPage = await createPage(cdp, viewerUrl, {
          ...commonInit,
          label: `viewer-${viewerIndex}`,
          role: "viewer",
          viewerIndex,
          clearHostRoom: false,
        }, signal);
        pages.push(viewerPage);
        return viewerPage;
      },
      (viewerPage) =>
        waitForViewerMedia(
          cdp,
          viewerPage,
          config.connectionTimeoutMs,
          signal,
        ),
    );
    pages.splice(1, pages.length - 1, ...viewerPages);
    if (viewerCount === MAX_VIEWERS) {
      routeTimingStatus = "unavailable";
      try {
        routeTimingSummary = await requestRouteTimingSummary(
          cdp,
          hostPage,
          config.connectionTimeoutMs,
          signal,
        );
        routeTimingStatus = "captured";
      } catch {
        routeTimingSummary = null;
      }
    }
    if (config.settleMs > 0) {
      await delay(config.settleMs, signal);
    }
    const measurementStartedAt = Date.now();
    const deadline = measurementStartedAt + config.durationMs;
    do {
      samples.push(await samplePages(cdp, pages, measurementStartedAt));
      if (Date.now() < deadline) {
        await delay(
          Math.min(config.sampleIntervalMs, deadline - Date.now()),
          signal,
        );
      }
    } while (Date.now() < deadline);
    samples.push(await samplePages(cdp, pages, measurementStartedAt));

    const summary = summarizeSamples(samples, viewerCount);
    const checks = buildRunChecks(
      summary,
      viewerCount,
      config.profileId,
      config.expectedEndpointCap,
    );
    if (viewerCount === MAX_VIEWERS) {
      checks.push(
        buildRouteTimingCheck(routeTimingStatus, routeTimingSummary, viewerCount),
      );
    }
    if (config.qualityControlSmoke && viewerCount >= 3) {
      const qualityControl = await runQualityControlSmoke(
        cdp,
        pages,
        capture,
        signal,
      );
      checks.push({
        name: "quality-control-propagation",
        passed: qualityControl.settingsPropagated,
        actual: qualityControl.settingsPropagated,
        expected: "balanced and clarity settings reach every participant",
      });
      checks.push({
        name: "quality-control-no-peer-rebuild",
        passed: qualityControl.peerConnectionsStable,
        actual: qualityControl.peerConnectionsStable,
        expected: "peer connection identities stay unchanged",
      });
      checks.push({
        name: "quality-control-sender-readback",
        passed: qualityControl.senderReadbacksMatched,
        actual: qualityControl.senderReadbacksMatched,
        expected: "every baseline active video sender displays matching preference readback",
      });
      checks.push({
        name: "quality-control-viewer-progress",
        passed: qualityControl.viewersAdvanced,
        actual: qualityControl.viewersAdvanced,
        expected: "every viewer decodes and renders frames after both setting changes",
      });
    }
    if (config.recoveryViewerCount === viewerCount) {
      recovery = await runRecovery(
        cdp,
        pages,
        samples.at(-1)?.pages ?? [],
        config.recoveryTimeoutMs,
        signal,
      );
      checks.push(buildRecoveryCheck(recovery, config.expectedEndpointCap));
    }
    return {
      viewerCount,
      startedAt: new Date(startedAtMs).toISOString(),
      completedAt: new Date().toISOString(),
      status: checks.every((check) => check.passed) ? "passed" : "failed",
      checks,
      summary,
      routeTimingSummary,
      routeTimingStatus,
      samples,
      recovery,
    };
  } catch (error) {
    if (
      viewerCount === MAX_VIEWERS &&
      routeTimingStatus !== "captured" &&
      pages[0]
    ) {
      routeTimingStatus = "unavailable";
      try {
        routeTimingSummary = await requestRouteTimingSummary(
          cdp,
          pages[0],
          config.sampleIntervalMs,
          signal,
        );
        routeTimingStatus = "captured";
      } catch {
        routeTimingSummary = null;
      }
    }
    const failureEvidence = await captureFailureEvidence(
      cdp,
      pages,
      viewerCount + 1,
    ).catch(() => ({
      expectedPageCount: viewerCount + 1,
      observedPageCount: 0,
      pages: [],
    }));
    return {
      viewerCount,
      startedAt: new Date(startedAtMs).toISOString(),
      completedAt: new Date().toISOString(),
      status: "failed",
      checks:
        viewerCount === MAX_VIEWERS
          ? [
              buildRouteTimingCheck(
                routeTimingStatus,
                routeTimingSummary,
                viewerCount,
              ),
            ]
          : [],
      summary: samples.length > 0 ? summarizeSamples(samples, viewerCount) : null,
      routeTimingSummary,
      routeTimingStatus,
      samples,
      recovery,
      failureEvidence,
      error: errorMessage(error),
    };
  } finally {
    await Promise.allSettled(pages.map((page) => closePage(cdp, page)));
    await delay(250);
  }
}
