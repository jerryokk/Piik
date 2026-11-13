// Peer-assisted topology loopback gate. This CLI entry keeps argument
// handling, orchestration and exit codes; report shaping, page instrumentation
// and CDP plumbing live under ./peer-bench.

import { access, mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { basename, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { runCase } from "./support/peer-bench/case";
import {
  PROFILE_SETTINGS,
  parseBenchmarkConfig,
  parseExternalServerTarget,
} from "./support/peer-bench/config";
import {
  CdpConnection,
  buildServer,
  errorMessage,
  reservePort,
  startProcess,
  stopProcess,
  waitForHttp,
} from "./support/peer-bench/harness";
import { authenticateExternalServer, gitCommit, writeReport } from "./support/peer-bench/output";
import type {
  BenchmarkConfig,
  BenchmarkReport,
  ExternalServerTarget,
  ManagedProcess,
} from "./support/peer-bench/types";

async function main(): Promise<number> {
  let config: BenchmarkConfig;
  let externalServer: ExternalServerTarget | null;
  try {
    config = parseBenchmarkConfig();
    externalServer = parseExternalServerTarget();
    await access(config.chromePath);
  } catch (error) {
    process.stderr.write(`${errorMessage(error)}\n`);
    return 2;
  }

  const profile = PROFILE_SETTINGS[config.profileId];
  const profileResolution = profile.resolution;
  const report: BenchmarkReport = {
    schemaVersion: 5,
    startedAt: new Date().toISOString(),
    completedAt: null,
    gitCommit: await gitCommit(),
    chromium: null,
    environment: {
      node: process.version,
      platform: process.platform,
      arch: process.arch,
      headless: config.headless,
    },
    configuration: {
      viewerCounts: config.viewerCounts,
      expectedEndpointCap: config.expectedEndpointCap,
      profileId: config.profileId,
      codecMode: config.codecMode,
      durationMs: config.durationMs,
      settleMs: config.settleMs,
      sampleIntervalMs: config.sampleIntervalMs,
      connectionTimeoutMs: config.connectionTimeoutMs,
      recoveryViewerCount: config.recoveryViewerCount,
      recoveryTimeoutMs: config.recoveryTimeoutMs,
      headless: config.headless,
      noSandbox: config.noSandbox,
      qualityControlSmoke: config.qualityControlSmoke,
      chromeExecutable: basename(config.chromePath),
      output: config.outputPath ?? "stdout",
    },
    capture: {
      kind: "deterministic-canvas",
      width: profileResolution.width,
      height: profileResolution.height,
      frameRate: profile.maxFramerate,
      audio: false,
    },
    limitations: [
      "Synthetic canvas motion exercises real Chromium WebRTC but is not a game-capture quality claim.",
      "Headless runs are topology and transport evidence, not representative GPU or power evidence.",
      "Timing values never determine this loopback gate's status; the exact 20-Viewer route snapshot and four complete current-child distributions are required without a duration threshold.",
      "CDP process CPU covers the isolated Chromium instance, not a specific Host or relay page; identity changes or counter resets make that interval unknown, and multicore utilization may exceed 100%.",
      "CDP SystemInfo exposes no resident-set field, so peakResidentSetBytes is null; GPU, NIC, glass-to-glass latency, generational visual quality, mobile browsers, and SFU require other measurement.",
      externalServer
        ? "The external server target is exercised as configured; the gate does not infer unavailable transports."
        : "The local runner does not enable embedded SFU; SFU consistency is reported only when an SFU route is actually observed.",
      "The harness emits raw gate fields and simple invariants; it does not implement a route score or runtime policy.",
      "BENCHMARK_QUALITY_SMOKE disables encoded-stream support to verify ordinary-sender parameter propagation. Browser pool producer/clock settings are checked by browser-local-pool-probe.ts.",
      "This gate measures initial topology and relay-loss recovery, not sender-quality convergence or synthetic Viewer-triggered handoffs.",
    ],
    runs: [],
  };

  let server: ManagedProcess | null = null;
  let chrome: ManagedProcess | null = null;
  let cdp: CdpConnection | null = null;
  let profileDirectory: string | null = null;
  const abortController = new AbortController();
  const onSignal = () => {
    abortController.abort();
  };
  process.on("SIGINT", onSignal);
  process.on("SIGTERM", onSignal);

  try {
    const appPort = externalServer ? null : await reservePort();
    const debugPort = await reservePort();
    const baseUrl = externalServer?.origin ?? `http://127.0.0.1:${appPort}`;
    profileDirectory = await mkdtemp(join(tmpdir(), "piik-peer-benchmark-"));
    if (appPort !== null) {
      // An explicit environment and a profile working directory: the measured
      // configuration is exactly these values, never the repository's .env or
      // the operator's shell (a stray LIVEKIT_* or SITE_ACCESS_PASSWORD would
      // silently change the topology this run reports).
      server = startProcess(buildServer(), [], {
        PATH: process.env.PATH,
        SystemRoot: process.env.SystemRoot,
        PIIK_ENV: "development",
        PORT: String(appPort),
        LISTEN_HOST: "127.0.0.1",
        PUBLIC_BASE_URL: baseUrl,
        ALLOWED_ORIGINS: baseUrl,
        ENDPOINT_MEDIA_COPY_CAPACITY: String(config.expectedEndpointCap),
        MAX_VIEWERS_PER_ROOM: String(Math.max(...config.viewerCounts)),
        // The loopback gate measures peer topology, never a relayed candidate.
        STUN_URLS: "",
      }, profileDirectory);
      await waitForHttp(
        `${baseUrl}/healthz`,
        server,
        20_000,
        abortController.signal,
      );
    }

    const chromeArgs = [
      `--remote-debugging-port=${debugPort}`,
      `--user-data-dir=${profileDirectory}`,
      "--no-first-run",
      "--no-default-browser-check",
      "--disable-extensions",
      "--disable-background-timer-throttling",
      "--disable-backgrounding-occluded-windows",
      "--disable-renderer-backgrounding",
      "--autoplay-policy=no-user-gesture-required",
      "--window-size=1920,1080",
      "about:blank",
    ];
    if (config.headless) {
      chromeArgs.unshift("--headless=new");
    }
    if (config.noSandbox) {
      chromeArgs.unshift("--no-sandbox");
    }
    chrome = startProcess(config.chromePath, chromeArgs, process.env);
    const versionUrl = `http://127.0.0.1:${debugPort}/json/version`;
    await waitForHttp(versionUrl, chrome, 20_000, abortController.signal);
    const version = (await (await fetch(versionUrl)).json()) as {
      webSocketDebuggerUrl: string;
      [key: string]: unknown;
    };
    cdp = await CdpConnection.connect(
      version.webSocketDebuggerUrl,
      config.connectionTimeoutMs,
    );
    report.chromium = await cdp.call("Browser.getVersion");
    if (externalServer) {
      await authenticateExternalServer(cdp, externalServer, abortController.signal);
    }

    for (const viewerCount of config.viewerCounts) {
      abortController.signal.throwIfAborted();
      console.error(`Running peer-topology loopback with ${viewerCount} viewer(s)`);
      report.runs.push(
        await runCase(
          cdp,
          baseUrl,
          config,
          viewerCount,
          abortController.signal,
        ),
      );
    }
  } catch (error) {
    report.fatalError = errorMessage(error);
  } finally {
    process.removeListener("SIGINT", onSignal);
    process.removeListener("SIGTERM", onSignal);
    report.completedAt = new Date().toISOString();
    const cleanupErrors: string[] = [];
    const cleanup = async (operation: Promise<unknown>): Promise<void> => {
      try {
        await operation;
      } catch (error) {
        cleanupErrors.push(errorMessage(error));
      }
    };
    await Promise.all([
      cleanup(
        (async () => {
          if (!cdp) return;
          try {
            await cdp.call("Browser.close");
          } finally {
            await cdp.close();
          }
        })(),
      ),
      cleanup(stopProcess(server)),
    ]);
    await cleanup(stopProcess(chrome));
    if (profileDirectory) {
      await cleanup(
        rm(profileDirectory, { recursive: true, force: true, maxRetries: 3 }),
      );
    }
    if (cleanupErrors.length > 0) {
      const cleanupError = `Cleanup failed: ${cleanupErrors.join("; ")}`;
      report.fatalError = report.fatalError
        ? `${report.fatalError}; ${cleanupError}`
        : cleanupError;
    }
  }

  await writeReport(report, config.outputPath);
  if (abortController.signal.aborted) {
    return 130;
  }
  return report.fatalError || report.runs.some((run) => run.status === "failed") ? 1 : 0;
}

const invokedPath = process.argv[1] ? pathToFileURL(resolve(process.argv[1])).href : null;
if (invokedPath === import.meta.url) {
  process.exitCode = await main();
}