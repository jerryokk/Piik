import {
  spawn,
  type ChildProcessWithoutNullStreams,
} from "node:child_process";
import { mkdir, mkdtemp } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { appShellInitScript } from "../../src/client/lib/app-shell";
import { fittedCaptureSize, matchesCaptureSize } from "./support/media-gate-geometry";
import {
  CdpConnection,
  cleanupRun,
  createPage,
  evaluate,
  launchChrome,
  reservePort,
  waitForVersion,
} from "./support/browser-gate-harness";
import { cargoGateBuild } from "../release/app-package-targets.mjs";
import { runLocalModeStages } from "./support/app-native-host-stages";
import {
  SOURCE_TITLE,
  closeSourceBrowser,
  powershell,
  sourceServer,
  startSourceBrowser,
  waitForCaptureWindow,
} from "./support/app-native-host-source";
import {
  assertVideoCodec,
  clickHostControl,
  readAppEndpoint,
  readVideoCodec,
  run,
  selectNativeSource,
  stopChild,
  waitForValue,
  type GateMode,
  type VideoCodec,
} from "./support/app-native-host-driver";

// Stable re-export so the Embedded SFU gate keeps one fixture import path.
export {
  SOURCE_TITLE,
  closeSourceBrowser,
  powershell,
  sourceServer,
  startSourceBrowser,
  waitForCaptureWindow,
} from "./support/app-native-host-source";

const ROOT = resolve(import.meta.dirname, "..", "..");
const BUILD_ROOT = join(ROOT, "build", "rust-check");

const CODEC_PROBE = `(() => {
  const fitSize = ${fittedCaptureSize.toString()}, matchesSize = ${matchesCaptureSize.toString()};
  window.__piikGateFitsProfile = (width, height) => {
    const video = document.querySelector('video'), source = window.__piikGateSourceSize;
    return Boolean(video && source && matchesSize(video.videoWidth, video.videoHeight, fitSize(source, width, height)));
  };
  const peers = [], BrowserPeer = RTCPeerConnection;
  const sockets = [], BrowserSocket = WebSocket;
  window.WebSocket = class extends BrowserSocket {
    constructor(...args) { super(...args); sockets.push(this); }
  };
  window.__piikGatePublicSignal = () => sockets.some((socket) => {
    const url = new URL(socket.url);
    return socket.readyState === WebSocket.OPEN && url.protocol === 'wss:' &&
      url.host === location.host && url.pathname === '/signal';
  });
  window.RTCPeerConnection = class extends BrowserPeer {
    constructor(...args) { super(...args); peers.push(this); }
  };
  window.__piikGateVideoCodec = async () => {
    const trackId = document.querySelector('video')?.srcObject?.getVideoTracks()[0]?.id;
    if (!trackId) return null;
    for (const peer of peers) {
      if (peer.connectionState !== 'connected') continue;
      const report = await peer.getStats();
      for (const row of report.values()) {
        if (row.type !== 'inbound-rtp' || row.kind !== 'video' ||
            row.trackIdentifier !== trackId || !(row.framesDecoded > 0)) continue;
        const mime = report.get(row.codecId)?.mimeType?.toLowerCase();
        if (mime === 'video/h264' || mime === 'video/vp8') return mime.slice(6);
      }
    }
    return null;
  };
})()`;

interface GateResult {
  passed: boolean;
  requestedCodec: VideoCodec | "auto";
  actualCodec: VideoCodec | null;
  codecPreserved: boolean;
  hostNativeActive: boolean;
  hostInvite: boolean;
  preSharePreferenceEnabled: boolean;
  qualityControlsEnabled: boolean;
  livePresetChanges: number;
  liveQualityChanged: boolean;
  fourKQualityChanged: boolean;
  customQualityChanged: boolean;
  rateQualityChanges: number;
  pausedQualityChanged: boolean;
  backgroundProfileRecovery: boolean | null;
  sourceWindowRecovery: boolean | null;
  mediaObjectPreserved: boolean;
  nativeSourceChanged: boolean;
  nativeQualityEvidence: boolean;
  viewerConnected: boolean;
  viewerFrames: number;
  viewerWidth: number;
  viewerHeight: number;
  sourceFailureEndedShare: boolean | null;
  replacementViewerConnected: boolean | null;
  replacementViewerFrames: number | null;
  appCrashEndedShare: boolean | null;
  mode: GateMode;
  viewerLocation: "local-browser" | "public-url-browser";
  publicViewerPage: boolean;
  publicViewerSignal: boolean;
  cleanup: Awaited<ReturnType<typeof cleanupRun>>;
  error: string | null;
  stage?: string;
}

async function main(): Promise<void> {
  if (process.platform !== "win32") {
    throw new Error("The native Host gate currently requires Windows");
  }
  if (process.env.PIIK_CLIENT_NATIVE_HOST_GATE !== "true") {
    throw new Error("PIIK_CLIENT_NATIVE_HOST_GATE=true is required");
  }
  const linkMedia = process.env.PIIK_CLIENT_LINK_MEDIA_GATE === "true";
  const mode: GateMode = linkMedia ? "one-link" : "local";
  const crashGate =
    process.env.PIIK_CLIENT_NATIVE_HOST_CRASH_GATE === "true";
  if (crashGate && mode !== "local") {
    throw new Error("App crash gate requires local mode");
  }
  const sourceKind: "window" | "display" =
    process.env.PIIK_CLIENT_NATIVE_HOST_SOURCE === "display"
      ? "display"
      : "window";
  const requestedCodec = process.env.PIIK_CLIENT_NATIVE_HOST_CODEC?.trim() || "auto";
  if (requestedCodec !== "auto" && requestedCodec !== "h264" && requestedCodec !== "vp8") {
    throw new Error("PIIK_CLIENT_NATIVE_HOST_CODEC must be auto, h264, or vp8");
  }
  const chromePath = process.env.CHROME_PATH?.trim();
  if (!chromePath) throw new Error("CHROME_PATH is required");
  const lanAddress = process.env.PIIK_CLIENT_GATE_LAN_ADDRESS?.trim();
  const appBuild = cargoGateBuild(ROOT, "piik-app");
  const tunnel = process.env.PIIK_CLOUDFLARED?.trim() || join(
    BUILD_ROOT,
    "cloudflared.exe",
  );
  const profile = await mkdtemp(join(tmpdir(), "piik-client-media-"));
  const sourceProfile = await mkdtemp(join(tmpdir(), "piik-client-media-"));
  await mkdir(BUILD_ROOT, { recursive: true });
  const diagnosticDirectory = await mkdtemp(join(BUILD_ROOT, "native-host-gate-"));
  const sourcePort = await reservePort();
  const appPort = await reservePort();
  const debugPort = await reservePort();
  const sourceDebugPort = await reservePort();
  const appConfig = join(profile, "client.json");
  const captureBuild = BUILD_ROOT;
  const appBinary = appBuild.executable;
  const captureBinary = join(captureBuild, "piik-capture.exe");
  let source: { close(): Promise<void> } | null = null;
  let app: ChildProcessWithoutNullStreams | null = null;
  let sourceChrome: ChildProcessWithoutNullStreams | null = null;
  let sourceCdp: CdpConnection | null = null;
  let chrome: ChildProcessWithoutNullStreams | null = null;
  let cdp: CdpConnection | null = null;
  let nativePort = 0;
  const state = { stage: "setup" };
  const result: GateResult = {
    passed: false,
    requestedCodec,
    actualCodec: null,
    codecPreserved: false,
    hostNativeActive: false,
    hostInvite: false,
    preSharePreferenceEnabled: false,
    qualityControlsEnabled: false,
    livePresetChanges: 0,
    liveQualityChanged: false,
    fourKQualityChanged: false,
    customQualityChanged: false,
    rateQualityChanges: 0,
    pausedQualityChanged: false,
    backgroundProfileRecovery: mode === "local" && sourceKind === "window" ? false : null,
    sourceWindowRecovery: mode === "local" && sourceKind === "window" ? false : null,
    mediaObjectPreserved: false,
    nativeSourceChanged: false,
    nativeQualityEvidence: false,
    viewerConnected: false,
    viewerFrames: 0,
    viewerWidth: 0,
    viewerHeight: 0,
    sourceFailureEndedShare: mode === "local" ? false : null,
    replacementViewerConnected: mode === "local" ? false : null,
    replacementViewerFrames: mode === "local" ? 0 : null,
    appCrashEndedShare: crashGate ? false : null,
    mode,
    viewerLocation: mode === "one-link" ? "public-url-browser" : "local-browser",
    publicViewerPage: false,
    publicViewerSignal: false,
    cleanup: {
      browserExited: false,
      nativeExited: false,
      serverClosed: false,
      portsClosed: false,
      profileRemoved: false,
    },
    error: null,
  };
  try {
    state.stage = "web-build";
    run(process.env.ComSpec || "cmd.exe", [
      "/d", "/s", "/c", "npm run build:web",
    ]);
    state.stage = "source-server";
    source = await sourceServer(sourcePort);
    state.stage = "capture-build";
    process.stderr.write(`${JSON.stringify({ stage: state.stage, status: "started", at: new Date().toISOString() })}\n`);
    run(powershell(), [
      "-NoProfile", "-ExecutionPolicy", "Bypass", "-File",
      join(ROOT, "native", "capture", "windows", "build.ps1"),
      "-OutputDirectory", captureBuild,
    ]);
    process.stderr.write(`${JSON.stringify({ stage: state.stage, status: "finished", at: new Date().toISOString() })}\n`);
    state.stage = "app-build";
    run(appBuild.command, appBuild.args, ROOT, appBuild.env);
    state.stage = "source-browser";
    ({ child: sourceChrome, cdp: sourceCdp } = await startSourceBrowser(
      chromePath,
      sourceProfile,
      sourceDebugPort,
      sourcePort,
    ));
    await waitForCaptureWindow(captureBinary);
    state.stage = "app-start";
    app = spawn(appBinary, [
      mode === "one-link" ? "--link" : "--local",
      ...(mode === "one-link"
        ? ["--tunnel-process", tunnel]
        : lanAddress ? ["--lan-address", lanAddress] : []),
      "--capture-process", captureBinary,
      "--config", appConfig,
      "--port", String(appPort),
      "--debug", "--log-dir", diagnosticDirectory,
    ], {
      stdio: "pipe",
      windowsHide: true,
      env: {
        ...process.env,
        PIIK_DEBUG: "",
        PIIK_CLIENT_GATE_NO_BROWSER: "true",
      },
    });
    app.stderr.resume();
    state.stage = "app-ready";
    const appInfo = await readAppEndpoint(app, mode === "one-link");
    nativePort = appInfo.endpoint.port;
    state.stage = "local-server-ready";
    await waitForValue(
      async () => {
        const response = await fetch("http://127.0.0.1:" + appPort + "/healthz");
        return response.ok;
      },
      Boolean,
      15_000,
    );
    state.stage = "host-browser";
    chrome = await launchChrome(chromePath, debugPort, profile, [
      "--no-first-run", "--no-default-browser-check",
      "--disable-extensions", "--disable-logging",
      "--disable-background-timer-throttling",
      "--disable-backgrounding-occluded-windows",
      "--disable-renderer-backgrounding",
      "--window-size=1280,900",
    ]);
    chrome.stdout.resume();
    chrome.stderr.resume();
    state.stage = "host-cdp";
    const version = await waitForVersion(debugPort, chrome);
    cdp = await CdpConnection.connect(version.webSocketDebuggerUrl, Date.now() + 10_000);
    const hostBootstrap = new URLSearchParams({
      ...(appInfo.password ? { "client-access": appInfo.password } : {}),
      "piik-client": "1",
    }).toString();
    state.stage = "host-page";
    const host = await createPage(
      cdp,
      "http://localhost:" + appPort + "/?debug=1#" + hostBootstrap,
      appShellInitScript("gate") + CODEC_PROBE,
      true,
    );
    await waitForValue(
      (deadline) => evaluate<boolean>(
        cdp!,
        host,
        "Boolean(document.querySelector('button.lr-tv-big.is-action'))",
        deadline,
      ),
      Boolean,
      15_000,
    );
    state.stage = "pre-share-quality-controls";
    await evaluate<void>(
      cdp,
      host,
      `document.querySelector('button[aria-controls="host-advanced-door"]')?.click()`,
      Date.now() + 5_000,
    );
    await clickHostControl(cdp, host,
      `document.querySelector('#host-advanced-door .lr-sharing-picture-details > summary')`,
    );
    result.preSharePreferenceEnabled = await waitForValue(
      (deadline) => evaluate<boolean>(
        cdp!,
        host,
        `(() => {
          const preference = document.querySelector('button[name="degradationPreference"][value="maintain-resolution"]');
          return Boolean(preference && !preference.disabled);
        })()`,
        deadline,
      ),
      Boolean,
      5_000,
    );
    state.stage = "pre-share-codec";
    // Observe the source at the largest current ceiling before testing smaller
    // ceilings. Native capture no longer pads/upscales every source to 16:9.
    await clickHostControl(cdp, host, `document.querySelector('button.lr-chip[aria-label="1440p"]')`);
    await clickHostControl(cdp, host, `document.querySelector('button[name="degradationPreference"][value="maintain-resolution"]')`);
    await clickHostControl(cdp, host,
      `document.querySelector('button[name="videoCodec"][value="${requestedCodec}"]')`,
    );
    await evaluate<void>(
      cdp,
      host,
      `document.querySelector('button[aria-controls="host-advanced-door"]')?.click()`,
      Date.now() + 5_000,
    );
    state.stage = "host-click";
    await evaluate<void>(
      cdp,
      host,
      "document.querySelector('button.lr-tv-big.is-action')?.click()",
      Date.now() + 5_000,
    );
    await selectNativeSource(
      cdp,
      host,
      sourceKind,
      sourceKind === "window" ? SOURCE_TITLE : undefined,
    );
    state.stage = "host-native-share";
    let hostState: { invite: string | null; native: boolean };
    try {
      hostState = await waitForValue(
        (deadline) => evaluate<{
          invite: string | null;
          native: boolean;
        }>(
          cdp!,
          host,
          "(async () => ({invite: document.querySelector('.lr-invite-url')?.value || null, " +
            "native: (await window.__piikGateVideoCodec()) !== null, " +
            "body: document.body.innerText.slice(0, 500)}))()",
          deadline,
        ),
        (state) => state.invite !== null && state.native,
        40_000,
      );
    } catch (error) {
      const diagnostic = await evaluate<unknown>(
        cdp!,
        host,
        "({buttons: [...document.querySelectorAll('button')].map((button) => ({label: button.getAttribute('aria-label'), text: button.textContent?.slice(0, 120), disabled: button.disabled})), inputs: [...document.querySelectorAll('input')].map((input) => ({className: input.className, type: input.type, disabled: input.disabled}))})",
        Date.now() + 2_000,
      ).catch(() => null);
      throw new Error("native host stage failed: " +
        (error instanceof Error ? error.message : String(error)) +
        " diagnostic=" + JSON.stringify(diagnostic));
    }
    result.hostNativeActive = hostState.native;
    result.hostInvite = hostState.invite !== null;
    state.stage = "host-codec";
    const actualCodec = await readVideoCodec(cdp, host);
    result.actualCodec = actualCodec;
    if (requestedCodec !== "auto" && actualCodec !== requestedCodec) {
      throw new Error(`Native ${requestedCodec} selection produced ${actualCodec}`);
    }
    if (!hostState.invite) throw new Error("Host did not publish an invitation");
    const inviteURL = new URL(hostState.invite);
    const roomMatch = /^\/r\/([1-9][0-9]{3})$/.exec(inviteURL.pathname);
    const viewerGrant = new URLSearchParams(inviteURL.hash.slice(1)).get("v");
    if (!roomMatch || !viewerGrant) {
      throw new Error("Host invitation is not a room grant");
    }
    const viewerURL = new URL(hostState.invite);
    if (mode === "local") {
      // Local invitations use LAN HTTPS; this gate joins through App's HTTP loopback entry.
      viewerURL.protocol = "http:";
      viewerURL.hostname = "localhost";
    }
    else {
      if (viewerURL.protocol !== "https:" || viewerURL.origin !== appInfo.publicOrigin) {
        throw new Error("One-link invitation does not use the actual public origin");
      }
      state.stage = "public-page-ready";
      await waitForValue(async (deadline) => {
        const response = await fetch(viewerURL, { signal: AbortSignal.timeout(Math.max(1, deadline - Date.now())) });
        return response.ok;
      }, Boolean, 20_000);
    }
    state.stage = "viewer-page";
    const viewer = await createPage(cdp, viewerURL.toString(), CODEC_PROBE, true);
    state.stage = "viewer-media";
    const viewerState = await waitForValue(
      (deadline) => evaluate<{
        frames: number;
        width: number;
        height: number;
        connected: boolean;
      }>(
        cdp!,
        viewer,
        "(() => { const video = document.querySelector('video'); " +
          "if (!video) return {frames:0,width:0,height:0,connected:false}; " +
          "const q=video.getVideoPlaybackQuality(); return {frames:q.totalVideoFrames," +
          "width:video.videoWidth,height:video.videoHeight,connected:video.videoWidth>0&&video.videoHeight>0}; })()",
        deadline,
      ),
      (state) => state.connected && state.frames >= 30 &&
        state.width >= 320 && state.width <= 2560 && state.height >= 240 && state.height <= 1440,
      40_000,
    );
    result.viewerConnected = viewerState.connected;
    result.viewerFrames = viewerState.frames;
    result.viewerWidth = viewerState.width;
    result.viewerHeight = viewerState.height;
    await evaluate<void>(cdp, viewer,
      `window.__piikGateSourceSize = ${JSON.stringify({ width: viewerState.width, height: viewerState.height })}`,
      Date.now() + 5_000);
    await assertVideoCodec(cdp, viewer, actualCodec);
    if (mode === "one-link") {
      result.codecPreserved = true;
      result.publicViewerPage = await evaluate<boolean>(cdp, viewer,
        `location.origin === ${JSON.stringify(appInfo.publicOrigin)} && location.pathname === ${JSON.stringify(viewerURL.pathname)}`,
        Date.now() + 5_000);
      result.publicViewerSignal = await evaluate<boolean>(cdp, viewer,
        "window.__piikGatePublicSignal()", Date.now() + 5_000);
    } else {
    await runLocalModeStages({
      cdp,
      host,
      viewer,
      viewerState,
      result,
      actualCodec,
      sourceKind,
      sourcePort,
      diagnosticDirectory,
      state,
      sourceCdp: () => sourceCdp,
    });


    if (crashGate) {
      state.stage = "app-crash";
      if (!app || !(await stopChild(app))) {
        throw new Error("App did not terminate for crash gate");
      }
      app = null;
      result.appCrashEndedShare = await waitForValue(
        (deadline) => evaluate<boolean>(
          cdp!,
          host,
          "Boolean(document.querySelector('button.lr-tv-big.is-action'))",
          deadline,
        ),
        Boolean,
        5_000,
      );
    } else if (sourceKind === "window") {
    state.stage = "source-failure";
    if (!sourceChrome || !sourceCdp ||
        !(await closeSourceBrowser(sourceChrome, sourceCdp))) {
      throw new Error("captured source did not close");
    }
    sourceChrome = null;
    sourceCdp = null;
    state.stage = "host-source-failure";
    result.sourceFailureEndedShare = await waitForValue(
      (deadline) => evaluate<boolean>(
        cdp!,
        host,
        "Boolean(document.querySelector('button.lr-tv-big.is-action'))",
        deadline,
      ),
      Boolean,
      20_000,
    );
    state.stage = "source-restart";
    ({ child: sourceChrome, cdp: sourceCdp } = await startSourceBrowser(
      chromePath,
      sourceProfile,
      sourceDebugPort,
      sourcePort,
    ));
    await waitForCaptureWindow(captureBinary);
    state.stage = "host-restart";
    await evaluate<void>(
      cdp,
      host,
      "document.querySelector('button.lr-tv-big.is-action')?.click()",
      Date.now() + 5_000,
    );
    await selectNativeSource(
      cdp,
      host,
      sourceKind,
      sourceKind === "window" ? SOURCE_TITLE : undefined,
    );
    await waitForValue(
      (deadline) => evaluate<boolean>(
        cdp!,
        host,
        "Boolean(document.querySelector('.lr-tv-overlay[role=\"status\"]'))",
        deadline,
      ),
      Boolean,
      40_000,
    );
    state.stage = "viewer-replacement-media";
    result.replacementViewerConnected = await waitForValue(
      (deadline) => evaluate<boolean>(
        cdp!,
        viewer,
        `(() => {
          const video = document.querySelector('video');
          return Boolean(video && video.srcObject &&
            video.srcObject !== window.__piikGateMedia &&
            video.videoWidth > 0 && video.videoWidth <= 854 && video.videoHeight > 0 && video.videoHeight <= 480);
        })()`,
        deadline,
      ),
      Boolean,
      40_000,
    );
    result.replacementViewerFrames = await evaluate<number>(
      cdp,
      viewer,
      `new Promise((resolve) => {
        const video = document.querySelector('video');
        let frames = 0;
        const next = () => video.requestVideoFrameCallback(() => {
          frames += 1;
          if (frames >= 30) resolve(frames); else next();
        });
        next();
      })`,
      Date.now() + 10_000,
    );
    } else {
      // A display remains available while the test Browser is open, so this
      // arm does not claim source-end or same-room reselection evidence.
      result.sourceFailureEndedShare = null;
      result.replacementViewerConnected = null;
      result.replacementViewerFrames = null;
    }
    }
  } catch (error) {
    // App stderr can contain implementation diagnostics or URLs; keep gate
    // output independent of credentials and media-path identifiers.
    result.error = error instanceof Error ? error.message : String(error);
    result.stage = state.stage;
  } finally {
    if (sourceChrome && sourceCdp) {
      await closeSourceBrowser(sourceChrome, sourceCdp);
      sourceChrome = null;
      sourceCdp = null;
    } else if (sourceChrome && sourceChrome.exitCode === null) {
      sourceChrome.kill();
    }
    result.cleanup = await cleanupRun({
      cdp,
      native: app,
      chrome,
      server: source,
      profile,
      ports: [sourcePort, appPort, debugPort, ...(nativePort ? [nativePort] : [])],
    });
    if (sourceProfile) {
      const sourceCleanup = await cleanupRun({
        cdp: null,
        native: sourceChrome,
        chrome: null,
        server: null,
        profile: sourceProfile,
        ports: [sourceDebugPort],
      });
      result.cleanup.profileRemoved =
        result.cleanup.profileRemoved && sourceCleanup.profileRemoved;
      result.cleanup.portsClosed =
        result.cleanup.portsClosed && sourceCleanup.portsClosed;
    }
  }
  result.passed = result.error === null && result.actualCodec !== null && result.preSharePreferenceEnabled &&
    result.hostNativeActive &&
    result.hostInvite &&
    (mode === "one-link"
        ? result.publicViewerPage && result.publicViewerSignal && result.viewerConnected &&
          result.viewerFrames >= 30 && result.viewerWidth > 0 && result.viewerWidth <= 2560 &&
          result.viewerHeight > 0 && result.viewerHeight <= 1440 && result.codecPreserved
        : result.viewerConnected && result.viewerFrames >= 30 &&
        result.qualityControlsEnabled && result.liveQualityChanged && result.fourKQualityChanged && result.customQualityChanged &&
        result.rateQualityChanges === 2 &&
        result.backgroundProfileRecovery !== false &&
        result.sourceWindowRecovery !== false &&
        result.livePresetChanges === 2 &&
        result.codecPreserved &&
        result.pausedQualityChanged && result.mediaObjectPreserved &&
        result.nativeSourceChanged &&
        result.viewerWidth > 0 && result.viewerWidth <= 854 && result.viewerHeight > 0 && result.viewerHeight <= 480 &&
        result.nativeQualityEvidence &&
        (crashGate
          ? result.appCrashEndedShare === true
          : sourceKind === "display" ||
            (result.sourceFailureEndedShare === true &&
              result.replacementViewerConnected === true &&
              (result.replacementViewerFrames ?? 0) >= 30))) &&
    result.cleanup.browserExited && result.cleanup.nativeExited &&
    result.cleanup.serverClosed && result.cleanup.portsClosed &&
    result.cleanup.profileRemoved;
  process.stdout.write(JSON.stringify(result) + "\n");
  if (!result.passed) process.exitCode = 1;
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(resolve(process.argv[1])).href
) {
  await main();
}
