// Process, port and CDP plumbing plus page sampling shared by the runner and
// each measured case.

import { spawn, spawnSync } from "node:child_process";
import { createServer } from "node:net";
import WebSocket from "ws";
import { cargoGateBuild } from "../../../release/app-package-targets.mjs";
import { buildBenchmarkInitScript } from "./init-script";
import { hasAuthoritativeMediaUpstream, pageRuntimeRole, parsePagePerformanceMetrics } from "./observation";
import { summarizeBenchmarkRouteTiming } from "./route-timing";
import { PROFILE_SETTINGS } from "./config";
import {
  REPO_ROOT,
  type BenchmarkCodecMode,
  type BenchmarkRouteTimingSamples,
  type BenchmarkRouteTimingSummary,
  type BrowserProcessSample,
  type CdpResponse,
  type ManagedProcess,
  type PageHandle,
  type PageObservation,
  type PagePerformanceSample,
  type ProfileId,
  type RuntimeEvaluation,
  type TimedSample,
} from "./types";

export class BoundedLog {
  private text = "";

  append(chunk: Buffer | string): void {
    this.text += chunk.toString();
    if (this.text.length > 32_000) {
      this.text = this.text.slice(-32_000);
    }
  }

  tail(): string {
    return this.text.trim();
  }
}

export type { PageHandle } from "./types";

export function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export class CdpConnection {
  private nextId = 1;
  private readonly pending = new Map<
    number,
    {
      resolve: (value: unknown) => void;
      reject: (error: Error) => void;
      timeout: NodeJS.Timeout;
    }
  >();
  private closed = false;

  private constructor(
    private readonly socket: WebSocket,
    private readonly commandTimeoutMs: number,
  ) {
    socket.on("message", (raw) => this.onMessage(raw.toString()));
    socket.on("close", () => this.onClose(new Error("CDP connection closed")));
    socket.on("error", (error) => this.onClose(error));
  }

  static async connect(
    url: string,
    commandTimeoutMs: number,
  ): Promise<CdpConnection> {
    const socket = new WebSocket(url, { maxPayload: 64 * 1024 * 1024 });
    await new Promise<void>((resolveOpen, rejectOpen) => {
      const timeout = setTimeout(() => {
        socket.terminate();
        rejectOpen(new Error("CDP connection timed out"));
      }, commandTimeoutMs);
      const finish = (callback: () => void) => {
        clearTimeout(timeout);
        callback();
      };
      socket.once("open", () => finish(resolveOpen));
      socket.once("error", (error) => finish(() => rejectOpen(error)));
    });
    return new CdpConnection(socket, commandTimeoutMs);
  }

  call<T = Record<string, unknown>>(
    method: string,
    params: Record<string, unknown> = {},
    sessionId?: string,
  ): Promise<T> {
    if (this.closed || this.socket.readyState !== WebSocket.OPEN) {
      return Promise.reject(new Error(`Cannot call ${method}: CDP is closed`));
    }
    const id = this.nextId++;
    return new Promise<T>((resolveCall, rejectCall) => {
      const timeout = setTimeout(() => {
        this.pending.delete(id);
        rejectCall(new Error(`CDP command timed out: ${method}`));
      }, this.commandTimeoutMs);
      this.pending.set(id, {
        resolve: (value) => {
          clearTimeout(timeout);
          resolveCall(value as T);
        },
        reject: (error) => {
          clearTimeout(timeout);
          rejectCall(error);
        },
        timeout,
      });
      try {
        this.socket.send(JSON.stringify({ id, method, params, sessionId }));
      } catch (error) {
        clearTimeout(timeout);
        this.pending.delete(id);
        rejectCall(error instanceof Error ? error : new Error("CDP send failed"));
      }
    });
  }

  async close(): Promise<void> {
    if (this.closed) {
      return;
    }
    await new Promise<void>((resolveClose) => {
      const timer = setTimeout(resolveClose, 1_000);
      this.socket.once("close", () => {
        clearTimeout(timer);
        resolveClose();
      });
      this.socket.close();
    });
  }

  private onMessage(raw: string): void {
    let message: CdpResponse;
    try {
      message = JSON.parse(raw) as CdpResponse;
    } catch {
      return;
    }
    if (message.id === undefined) {
      return;
    }
    const pending = this.pending.get(message.id);
    if (!pending) {
      return;
    }
    this.pending.delete(message.id);
    if (message.error) {
      pending.reject(
        new Error(`${message.error.message} (CDP ${message.error.code})`),
      );
      return;
    }
    pending.resolve(message.result ?? {});
  }

  private onClose(error: Error): void {
    if (this.closed) {
      return;
    }
    this.closed = true;
    for (const pending of this.pending.values()) {
      clearTimeout(pending.timeout);
      pending.reject(error);
    }
    this.pending.clear();
  }
}

export async function reservePort(): Promise<number> {
  const server = createServer();
  await new Promise<void>((resolveListen, rejectListen) => {
    server.once("error", rejectListen);
    server.listen(0, "127.0.0.1", resolveListen);
  });
  const address = server.address();
  if (!address || typeof address === "string") {
    server.close();
    throw new Error("Could not reserve a local TCP port");
  }
  await new Promise<void>((resolveClose, rejectClose) => {
    server.close((error) => (error ? rejectClose(error) : resolveClose()));
  });
  return address.port;
}

export function buildStep(command: string, args: string[], failure: string, env: NodeJS.ProcessEnv = process.env): void {
  const result = spawnSync(command, args, {
    cwd: REPO_ROOT,
    env,
    encoding: "utf8",
    windowsHide: true,
  });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    throw new Error((result.stderr || result.stdout).trim() || failure);
  }
}

// buildServer compiles the Piik server this run starts. The Web assets are
// embedded, so the Vite build precedes the Cargo build and the Browser reaches the
// same bundle the binary serves.
export function buildServer(): string {
  const cargo = cargoGateBuild(REPO_ROOT, "piik-server");
  if (process.platform === "win32") {
    buildStep(process.env.ComSpec || "cmd.exe", ["/d", "/s", "/c", "npm run build:web"],
      "Web UI build failed");
  } else {
    buildStep("npm", ["run", "build:web"], "Web UI build failed");
  }
  buildStep(cargo.command, cargo.args, "Piik server build failed", cargo.env);
  return cargo.executable;
}

export function startProcess(
  command: string,
  args: string[],
  environment?: NodeJS.ProcessEnv,
  cwd: string = REPO_ROOT,
): ManagedProcess {
  const log = new BoundedLog();
  const child = spawn(command, args, {
    cwd,
    env: environment,
    stdio: ["ignore", "pipe", "pipe"],
    windowsHide: true,
  });
  child.stdout?.on("data", (chunk: Buffer) => log.append(chunk));
  child.stderr?.on("data", (chunk: Buffer) => log.append(chunk));
  return { child, log };
}

// Readiness is the endpoint, never the spawned process: Chrome's Windows
// launcher exits as soon as the browser process takes over the profile. The
// deadline bounds the wait; the exit code and captured log explain a failure.
export async function waitForHttp(
  url: string,
  process: ManagedProcess,
  timeoutMs: number,
  signal: AbortSignal,
): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const response = await fetch(url, { signal: AbortSignal.timeout(1_000) });
      if (response.ok) {
        return;
      }
    } catch {
      // Startup polling owns the retry.
    }
    await delay(100, signal);
  }
  throw new Error(
    `Timed out waiting for ${url} (exit ${process.child.exitCode ?? "running"})\n${process.log.tail()}`,
  );
}

export async function stopProcess(process: ManagedProcess | null): Promise<void> {
  if (!process || process.child.exitCode !== null) {
    return;
  }
  const exited = new Promise<void>((resolveExit) => process.child.once("exit", () => resolveExit()));
  process.child.kill("SIGTERM");
  if (await promiseSettledWithin(exited, 3_000)) {
    return;
  }
  process.child.kill("SIGKILL");
  await promiseSettledWithin(exited, 2_000);
}

export async function promiseSettledWithin(promise: Promise<void>, timeoutMs: number): Promise<boolean> {
  return Promise.race([
    promise.then(() => true),
    delay(timeoutMs).then(() => false),
  ]);
}

export function delay(milliseconds: number, signal?: AbortSignal): Promise<void> {
  return new Promise((resolveDelay, rejectDelay) => {
    if (signal?.aborted) {
      rejectDelay(new Error("Benchmark interrupted"));
      return;
    }
    const timer = setTimeout(() => {
      signal?.removeEventListener("abort", onAbort);
      resolveDelay();
    }, milliseconds);
    const onAbort = () => {
      clearTimeout(timer);
      signal?.removeEventListener("abort", onAbort);
      rejectDelay(new Error("Benchmark interrupted"));
    };
    signal?.addEventListener("abort", onAbort, { once: true });
  });
}

export async function evaluate<T>(
  cdp: CdpConnection,
  page: PageHandle,
  expression: string,
): Promise<T> {
  const evaluation = await cdp.call<RuntimeEvaluation<T>>(
    "Runtime.evaluate",
    {
      expression,
      awaitPromise: true,
      returnByValue: true,
      userGesture: true,
    },
    page.sessionId,
  );
  if (evaluation.exceptionDetails) {
    throw new Error(
      evaluation.exceptionDetails.exception?.description ??
        evaluation.exceptionDetails.text ??
        "Browser evaluation failed",
    );
  }
  return evaluation.result.value as T;
}

export async function createPage(
  cdp: CdpConnection,
  url: string,
  options: Parameters<typeof buildBenchmarkInitScript>[0],
  signal: AbortSignal,
): Promise<PageHandle> {
  const created = await cdp.call<{ targetId: string }>("Target.createTarget", {
    url: "about:blank",
    background: false,
  });
  try {
    const attached = await cdp.call<{ sessionId: string }>(
      "Target.attachToTarget",
      { targetId: created.targetId, flatten: true },
    );
    const page: PageHandle = {
      targetId: created.targetId,
      sessionId: attached.sessionId,
      label: options.label,
    };
    await Promise.all([
      cdp.call("Page.enable", {}, page.sessionId),
      cdp.call("Runtime.enable", {}, page.sessionId),
      cdp.call("Performance.enable", {}, page.sessionId),
    ]);
    await cdp.call(
      "Page.addScriptToEvaluateOnNewDocument",
      { source: buildBenchmarkInitScript(options) },
      page.sessionId,
    );
    const navigation = await cdp.call<{ errorText?: string }>(
      "Page.navigate",
      { url },
      page.sessionId,
    );
    if (navigation.errorText) {
      throw new Error(`Navigation failed: ${navigation.errorText}`);
    }
    await waitForPage(
      cdp,
      page,
      "document.readyState === 'complete' && Boolean(globalThis.__PIIK_BENCHMARK__)",
      15_000,
      "page initialization",
      signal,
    );
    return page;
  } catch (error) {
    await closeTarget(cdp, created.targetId);
    throw error;
  }
}

export async function closeTarget(cdp: CdpConnection, targetId: string): Promise<void> {
  try {
    await cdp.call("Target.closeTarget", { targetId });
  } catch {
    // Browser shutdown and a prior recovery close make target cleanup idempotent.
  }
}

export async function closePage(cdp: CdpConnection, page: PageHandle): Promise<void> {
  await closeTarget(cdp, page.targetId);
}

export async function waitForPage(
  cdp: CdpConnection,
  page: PageHandle,
  predicate: string,
  timeoutMs: number,
  description: string,
  signal: AbortSignal,
): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      if (await evaluate<boolean>(cdp, page, `Boolean(${predicate})`)) {
        return;
      }
    } catch {
      // Navigation can transiently invalidate an execution context.
    }
    await delay(100, signal);
  }
  throw new Error(`Timed out waiting for ${description} in ${page.label}`);
}

export async function quickSnapshot(
  cdp: CdpConnection,
  page: PageHandle,
): Promise<PageObservation> {
  return evaluate<PageObservation>(
    cdp,
    page,
    "globalThis.__PIIK_BENCHMARK__.snapshot()",
  );
}

export async function detailedSample(
  cdp: CdpConnection,
  page: PageHandle,
): Promise<PageObservation> {
  return evaluate<PageObservation>(
    cdp,
    page,
    "globalThis.__PIIK_BENCHMARK__.sample()",
  );
}

export async function progressSample(
  cdp: CdpConnection,
  page: PageHandle,
): Promise<PageObservation> {
  return evaluate<PageObservation>(
    cdp,
    page,
    "globalThis.__PIIK_BENCHMARK__.progress()",
  );
}

export async function startHost(
  cdp: CdpConnection,
  page: PageHandle,
  profileId: ProfileId,
  codecMode: BenchmarkCodecMode,
  timeoutMs: number,
  signal: AbortSignal,
): Promise<PageObservation> {
  const profileIndex = Object.keys(PROFILE_SETTINGS).indexOf(profileId);
  await waitForPage(
    cdp,
    page,
    "document.querySelector('.lr-entry-actions button.lr-tv-big.is-action[aria-label]')",
    15_000,
    "host controls",
    signal,
  );
  await evaluate(cdp, page, `(() => {
    const settings = document.querySelector('button[aria-controls="host-advanced-door"]');
    if (!(settings instanceof HTMLButtonElement)) throw new Error('Sharing settings button missing');
    if (settings.getAttribute('aria-expanded') !== 'true') settings.click();
  })()`);
  await waitForPage(cdp, page, "document.querySelector('#host-advanced-door .lr-tiles button')",
    5_000, "sharing settings", signal);
  if (codecMode !== "auto") {
    await evaluate(cdp, page, `(() => {
      const picture = document.querySelector('#host-advanced-door .lr-sharing-picture-details');
      if (picture instanceof HTMLDetailsElement && !picture.open) picture.querySelector('summary')?.click();
    })()`);
    await waitForPage(
      cdp,
      page,
      `document.querySelector('#host-advanced-door button[name="videoCodec"][value="${codecMode}"]') instanceof HTMLButtonElement`,
      5_000,
      `${codecMode} codec control`,
      signal,
    );
    await evaluate(
      cdp,
      page,
      `(() => {
        const button = document.querySelector('#host-advanced-door button[name="videoCodec"][value="${codecMode}"]');
        if (!(button instanceof HTMLButtonElement)) throw new Error('Codec mode button missing');
        button.click();
        return true;
      })()`,
    );
  }
  await evaluate(
    cdp,
    page,
    `(() => {
      const profiles = document.querySelectorAll('.lr-tiles button.lr-tile[aria-pressed]');
      const profile = profiles[${profileIndex}];
      if (!(profile instanceof HTMLButtonElement)) throw new Error('Quality profile button missing');
      profile.click();
      const start = document.querySelector('.lr-entry-actions button.lr-tv-big.is-action[aria-label]');
      if (!(start instanceof HTMLButtonElement)) throw new Error('Start button missing');
      globalThis.__PIIK_BENCHMARK__.markShareRequested();
      start.click();
      return true;
    })()`,
  );
  await waitForPage(cdp, page, `(() => {
    const tab = document.querySelector('button[data-source-tab="browser"]');
    return tab instanceof HTMLButtonElement && !tab.disabled && tab.getClientRects().length > 0;
  })()`, 5_000, "Browser capture tab", signal);
  await evaluate(cdp, page, `document.querySelector('button[data-source-tab="browser"]').click()`);
  await waitForPage(cdp, page, `(() => {
    const choice = document.querySelector('[role="tabpanel"] button.lr-source-option');
    return choice instanceof HTMLButtonElement && !choice.disabled && choice.getClientRects().length > 0;
  })()`, 5_000, "Browser capture choice", signal);
  await evaluate(cdp, page, `document.querySelector('[role="tabpanel"] button.lr-source-option').click()`);
  await waitForPage(
    cdp,
    page,
    "globalThis.__PIIK_BENCHMARK__.snapshot().peerId !== null",
    timeoutMs,
    "host signaling authentication",
    signal,
  );
  const snapshot = await quickSnapshot(cdp, page);
  if (!snapshot.roomId) {
    throw new Error("Host authenticated without a recorded room ID");
  }
  return snapshot;
}

export async function waitForViewerMedia(
  cdp: CdpConnection,
  page: PageHandle,
  timeoutMs: number,
  signal: AbortSignal,
): Promise<PageObservation> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const snapshot = await progressSample(cdp, page);
    if (
      snapshot.peerId &&
      hasAuthoritativeMediaUpstream(snapshot) &&
      snapshot.connections.some(
        (connection) =>
          connection.connectionState === "connected" &&
          connection.receiveTotals !== null &&
          (connection.receiveTotals.framesTotal ?? 0) > 0,
      )
    ) {
      return snapshot;
    }
    await delay(100, signal);
  }
  throw new Error(`Timed out waiting for decoded media in ${page.label}`);
}

export async function requestRouteTimingSummary(
  cdp: CdpConnection,
  hostPage: PageHandle,
  timeoutMs: number,
  signal: AbortSignal,
): Promise<BenchmarkRouteTimingSummary> {
  const requested = await evaluate<boolean>(
    cdp,
    hostPage,
    "globalThis.__PIIK_BENCHMARK__.requestRouteDiagnosticSnapshot()",
  );
  if (!requested) {
    throw new Error("Host route diagnostic request was unavailable");
  }
  await waitForPage(
    cdp,
    hostPage,
    "globalThis.__PIIK_BENCHMARK__.routeDiagnosticTimingSamples() !== null",
    timeoutMs,
    "Host route diagnostic snapshot",
    signal,
  );
  const samples = await evaluate<BenchmarkRouteTimingSamples>(
    cdp,
    hostPage,
    "globalThis.__PIIK_BENCHMARK__.routeDiagnosticTimingSamples()",
  );
  return summarizeBenchmarkRouteTiming(samples);
}

export async function samplePages(
  cdp: CdpConnection,
  pages: PageHandle[],
  startedAtMs: number,
): Promise<TimedSample> {
  const atEpochMs = Date.now();
  const browserProcessesPromise = sampleBrowserProcesses(cdp);
  const observations: PageObservation[] = [];
  let sampledViewerRuntime = false;
  let sampledRelayRuntime = false;
  for (const page of pages) {
    let observation: PageObservation;
    try {
      observation = await detailedSample(cdp, page);
    } catch (error) {
      throw new Error(`${page.label} sample failed: ${errorMessage(error)}`);
    }
    const runtimeRole = pageRuntimeRole(observation);
    const sampleRuntime: boolean =
      runtimeRole === "host" ||
      (runtimeRole === "viewer" && !sampledViewerRuntime) ||
      (runtimeRole === "relay" && !sampledRelayRuntime);
    const performance = sampleRuntime
      ? await samplePagePerformance(cdp, page)
      : null;
    sampledViewerRuntime ||= runtimeRole === "viewer" && sampleRuntime;
    sampledRelayRuntime ||= runtimeRole === "relay" && sampleRuntime;
    observations.push({ ...observation, performance });
  }
  const browserProcesses = await browserProcessesPromise;
  return {
    atEpochMs,
    elapsedMs: atEpochMs - startedAtMs,
    pages: observations,
    browserProcesses,
  };
}

export async function samplePagePerformance(
  cdp: CdpConnection,
  page: PageHandle,
): Promise<PagePerformanceSample | null> {
  try {
    const result = await cdp.call<{ metrics: unknown }>(
      "Performance.getMetrics",
      {},
      page.sessionId,
    );
    return parsePagePerformanceMetrics(result.metrics);
  } catch {
    return null;
  }
}

export async function sampleBrowserProcesses(cdp: CdpConnection): Promise<BrowserProcessSample | null> {
  try {
    const result = await cdp.call<{ processInfo: unknown[] }>("SystemInfo.getProcessInfo");
    if (!Array.isArray(result.processInfo)) return null;
    const processes: BrowserProcessSample["processes"] = [];
    const identities = new Set<string>();
    for (const raw of result.processInfo) {
      if (!raw || typeof raw !== "object") return null;
      const { type, id, cpuTime } = raw as Record<string, unknown>;
      if (typeof type !== "string" || type.length === 0 || !Number.isSafeInteger(id) || Number(id) < 0 || typeof cpuTime !== "number" || !Number.isFinite(cpuTime) || cpuTime < 0) return null;
      const identity = `${type}:${id}`;
      if (identities.has(identity)) return null;
      identities.add(identity);
      processes.push({ type, id: Number(id), cpuTimeSeconds: cpuTime });
    }
    return { processes: processes.sort((left, right) => left.id - right.id || left.type.localeCompare(right.type)) };
  } catch {
    return null;
  }
}
