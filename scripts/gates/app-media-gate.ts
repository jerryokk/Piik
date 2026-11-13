import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { copyFile, mkdir, mkdtemp } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { fittedCaptureSize, matchesCaptureSize } from "./support/media-gate-geometry";
import {
  CdpConnection,
  cleanupRun,
  createPage,
  evaluate,
  launchChrome,
  reservePort,
  waitForSample,
  waitForVersion,
} from "./support/browser-gate-harness";
import { readAppEndpoint } from "./support/app-gate-endpoint";
import {
  NATIVE_CLIENT_PROTOCOL,
  NATIVE_CLIENT_SUBPROTOCOL,
} from "../../src/client/native/wire";
import { cargoGateBuild } from "../release/app-package-targets.mjs";
import { gateDrivers } from "./support/app-media-gate-drivers";
import { browserMediaGate } from "./support/app-media-gate-browser";
import { mediaGatePassed } from "./support/app-media-gate-assertions";
import type {
  CaptureEvidence,
  MediaEvidence,
  Probe,
  WindowTarget,
} from "./support/app-media-gate-types";

const ROOT = resolve(import.meta.dirname, "..", "..");
const BUILD_ROOT = join(ROOT, "build", "rust-check");
const SOURCE_TITLE = "Piik Native Gate Source";
const GATE_STUN_URLS = process.env.PIIK_CLIENT_GATE_STUN_URLS?.trim();

const {
  run,
  powershell,
  startPageServer,
  observeCapture,
  observeAudioCapture,
  stopCapture,
} = gateDrivers(ROOT, SOURCE_TITLE);

async function main(): Promise<void> {
  if (process.platform !== "win32") {
    throw new Error("The native capture gate currently requires Windows");
  }
  if (process.env.PIIK_CLIENT_MEDIA_GATE !== "true") {
    throw new Error("PIIK_CLIENT_MEDIA_GATE=true is required");
  }
  const chromePath = process.env.CHROME_PATH?.trim();
  if (!chromePath) throw new Error("CHROME_PATH is required");
  if (!GATE_STUN_URLS) {
    throw new Error("PIIK_CLIENT_GATE_STUN_URLS is required");
  }

  const profile = await mkdtemp(join(tmpdir(), "piik-client-media-"));
  // Keep network-capable binaries at a stable repository path. Windows
  // associates its firewall decision with the full executable path; the
  // disposable Browser profile remains in the system temp directory.
  const buildRoot = BUILD_ROOT;
  await mkdir(buildRoot, { recursive: true });
  const pagePort = await reservePort();
  const debugPort = await reservePort();
  let server: Awaited<ReturnType<typeof startPageServer>> | null = null;
  let chrome: ChildProcessWithoutNullStreams | null = null;
  let capture: ChildProcessWithoutNullStreams | null = null;
  let audioCapture: ChildProcessWithoutNullStreams | null = null;
  let app: ChildProcessWithoutNullStreams | null = null;
  let cdp: CdpConnection | null = null;
  let nativePort = 0;
  let browser: string | null = null;
  const evidence: CaptureEvidence = {
    starting: false,
    active: false,
    frames: 0,
    keyFrameRequested: false,
    recoveryFrame: false,
    audioReady: false,
    audioFrames: 0,
    audioNonZeroSamples: 0,
  };
  let media: MediaEvidence = {
    captureActive: false,
    connected: false,
    connectedEdges: 0,
    frames: 0,
    edgeFrames: [],
    width: 0,
    height: 0,
    localType: null,
    remoteType: null,
    nativeCandidateTypes: [],
    liveProfileUpdated: false,
    pausedProfileUpdated: false,
    error: "not run",
    audioPackets: 0,
    edgeAudioPackets: [],
    audioEnergy: 0,
    edgeAudioEnergy: [],
    audioSamples: 0,
    edgeAudioSamples: [],
    audioBytes: 0,
    edgeAudioBytes: [],
    audioAvailable: false,
  };
  let error: string | null = null;
  let cleanup = {
    browserExited: false,
    nativeExited: false,
    serverClosed: false,
    portsClosed: false,
    profileRemoved: false,
  };
  try {
    run(powershell(), [
      "-NoProfile",
      "-ExecutionPolicy",
      "Bypass",
      "-File",
      join(ROOT, "native", "capture", "windows", "build.ps1"),
      "-OutputDirectory",
      buildRoot,
    ]);
    const executable = join(buildRoot, "piik-capture.exe");
    const probe = JSON.parse(run(executable, ["--probe"])) as Probe;
    const adapter = probe.adapters.find((candidate) => candidate.hardwareH264.length > 0);
    const encoder = adapter?.hardwareH264[0];
    if (probe.protocol !== 7 || !adapter || !encoder) {
      throw new Error("No hardware H264 capture path is available");
    }

    server = await startPageServer(pagePort);
    chrome = await launchChrome(chromePath, debugPort, profile, [
      "--no-first-run",
      "--no-default-browser-check",
      "--disable-extensions",
      "--disable-logging",
      "--disable-background-timer-throttling",
      "--disable-backgrounding-occluded-windows",
      "--disable-renderer-backgrounding",
      "--window-size=1280,900",
    ]);
    chrome.stdout.resume();
    chrome.stderr.resume();
    const version = await waitForVersion(debugPort, chrome);
    browser = version.Browser;
    cdp = await CdpConnection.connect(version.webSocketDebuggerUrl, Date.now() + 10_000);
    const sourcePage = await createPage(
      cdp,
      "http://127.0.0.1:" + pagePort + "/source",
      undefined,
      true,
    );
    const audioState = await evaluate<string>(
      cdp,
      sourcePage,
      `(() => {
        const context = new AudioContext();
        const gain = context.createGain();
        gain.gain.value = 0.05;
        const oscillator = context.createOscillator();
        oscillator.frequency.value = 440;
        oscillator.connect(gain).connect(context.destination);
        oscillator.start();
        globalThis.__piikAudioGate = { context, oscillator };
        return context.resume().then(() => context.state);
      })()`,
      Date.now() + 5_000,
    );
    if (audioState !== "running") {
      throw new Error("Browser could not start the selected-window audio source");
    }
    const targets = JSON.parse(run(executable, ["--list"])) as WindowTarget[];
    const sourceKind: "window" | "display" =
      process.env.PIIK_CLIENT_MEDIA_SOURCE === "display"
        ? "display"
        : "window";
    const target = targets.find(
      (candidate) => candidate.kind === sourceKind &&
        (sourceKind === "display" || candidate.title.includes(SOURCE_TITLE)),
    );
    if (!target) throw new Error("The animated capture target was not enumerated");

    capture = spawnCapture(executable, [
      "--capture-video",
      target.kind,
      target.sourceId,
      String(target.pid ?? 0),
      target.creationTime ?? "0",
      "--adapter-index",
      String(adapter.index),
      "--mft-index",
      String(encoder.index),
      "--width",
      "1280",
      "--height",
      "720",
      "--fps",
      "30",
      "--bitrate",
      "3000000",
      "--preference",
      "balanced",
      "--codec",
      "h264",
      "--protocol-v7",
      "--output", "1280", "720", "30", "3000000",
    ]);
    capture.stderr.resume();
    observeCapture(capture, evidence);
    await waitForSample(
      async () => ({ ...evidence }),
      (sample) =>
        sample.starting && sample.active && sample.frames >= 30 &&
        sample.keyFrameRequested && sample.recoveryFrame,
      15_000,
    );
    await stopCapture(capture);
    capture = null;

    const audioAvailable = sourceKind === "window"
      ? probe.processAudio
      : probe.systemAudio;
    if (audioAvailable) {
      audioCapture = spawnCapture(executable, [
        "--capture-audio",
        target.kind,
        String(target.pid ?? 0),
        target.creationTime ?? "0",
      ]);
      audioCapture.stderr.resume();
      observeAudioCapture(audioCapture, evidence);
      await waitForSample(
        async () => ({ ...evidence }),
        (sample) =>
          sample.audioReady && sample.audioFrames >= 10 &&
          sample.audioNonZeroSamples > 0,
        10_000,
      );
      await stopCapture(audioCapture);
      audioCapture = null;
    }

    const packageRoot = join(buildRoot, "media-package");
    const nativeRoot = join(packageRoot, "runtime", "native");
    await mkdir(nativeRoot, { recursive: true });
    await copyFile(
      executable,
      join(nativeRoot, "piik-capture.exe"),
    );
    const appExecutable = join(packageRoot, "piik-app.exe");
    // The App embeds the Vite output, so the Web build precedes the Cargo build.
    run(process.env.ComSpec || "cmd.exe", [
      "/d", "/s", "/c", "npm run build:web",
    ], ROOT);
    const appBuild = cargoGateBuild(ROOT, "piik-app");
    run(appBuild.command, appBuild.args, ROOT, appBuild.env);
    // The packaged layout keeps runtime/native beside the App executable.
    await copyFile(appBuild.executable, appExecutable);
    app = spawnCapture(appExecutable, [
      "--site",
      "http://127.0.0.1:" + pagePort,
      "--config",
      join(profile, "client.json"),
    ], { PIIK_CLIENT_GATE_NO_BROWSER: "true" });
    app.stderr.resume();
    const endpoint = await readAppEndpoint(app);
    nativePort = endpoint.port;
    const controlPage = await createPage(
      cdp,
      "http://127.0.0.1:" + pagePort + "/control",
      undefined,
      true,
    );
    media = await evaluate<MediaEvidence>(
      cdp,
      controlPage,
      "((__name) => (" + browserMediaGate.toString() + ")(" + JSON.stringify({
        endpoint,
        protocol: NATIVE_CLIENT_PROTOCOL,
        subprotocol: NATIVE_CLIENT_SUBPROTOCOL,
        sourceTitle: SOURCE_TITLE,
        sourceKind,
        stunUrls: GATE_STUN_URLS,
      }) + `, { fit: ${fittedCaptureSize.toString()}, matches: ${matchesCaptureSize.toString()} }))((target) => target)`,
      Date.now() + 70_000,
    );
    if (media.error) throw new Error(media.error);
  } catch (caught) {
    error = caught instanceof Error ? caught.message : String(caught);
  } finally {
    cleanup = await cleanupRun({
      cdp,
      native: app ?? capture ?? audioCapture,
      chrome,
      server,
      profile,
      ports: [pagePort, debugPort, ...(nativePort ? [nativePort] : [])],
    });
  }
  const passed = mediaGatePassed({ error, capture: evidence, media, cleanup });
  process.stdout.write(JSON.stringify({ passed, browser, capture: evidence, media, cleanup, error }) + "\n");
  if (!passed) process.exitCode = 1;
}

function spawnCapture(
  executable: string,
  args: string[],
  extraEnv: NodeJS.ProcessEnv = {},
): ChildProcessWithoutNullStreams {
  return spawn(executable, args, {
    stdio: "pipe",
    windowsHide: true,
    env: { ...process.env, ...extraEnv },
  });
}

await main();
