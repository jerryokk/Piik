// Local-mode media journey for the native Host gate: quality presets, live
// parameter changes, pause/resume, source replacement and quality evidence.
// Orchestration, cleanup and pass/fail aggregation stay in the gate entry.

import { readFile, stat } from "node:fs/promises";
import { join } from "node:path";
import {
  type CdpConnection,
  type PageHandle,
  cleanupRun,
  evaluate,
} from "./browser-gate-harness";
import { fittedCaptureSize, matchesCaptureSize } from "./media-gate-geometry";
import { SOURCE_TITLE } from "./app-native-host-source";
import {
  assertVideoCodec,
  clickHostControl,
  selectNativeSource,
  waitForValue,
  type GateMode,
  type VideoCodec,
} from "./app-native-host-driver";

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

export interface NativeViewerState {
  frames: number;
  width: number;
  height: number;
  connected: boolean;
}

export interface LocalModeStageOptions {
  cdp: CdpConnection;
  host: PageHandle;
  viewer: PageHandle;
  viewerState: NativeViewerState;
  result: GateResult;
  actualCodec: "h264" | "vp8";
  sourceKind: "window" | "display";
  sourcePort: number;
  diagnosticDirectory: string;
  state: { stage: string };
  sourceCdp: () => CdpConnection | null;
}

export async function runLocalModeStages(options: LocalModeStageOptions): Promise<void> {
  const {
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
    sourceCdp,
  } = options;
    state.stage = "native-quality-presets";
    await clickHostControl(cdp, host,
      `document.querySelector('button[aria-controls="host-advanced-door"]')`,
    );
    for (const [index, width, height] of [[0, 1280, 720], [1, 1920, 1080]] as const) {
      await clickHostControl(cdp, host, `document.querySelectorAll('button.lr-tile')[${index}]`);
      await waitForValue(
        (deadline) => evaluate<boolean>(
          cdp!,
          viewer,
          `window.__piikGateFitsProfile(${width}, ${height})`,
          deadline,
        ),
        Boolean,
        20_000,
      );
      await assertVideoCodec(cdp, viewer, actualCodec);
      result.livePresetChanges += 1;
    }
    state.stage = "native-quality-controls";
    await clickHostControl(cdp, host,
      `document.querySelector('#host-advanced-door .lr-sharing-picture-details > summary')`,
    );
    await waitForValue(
      (deadline) => evaluate<boolean>(
        cdp!,
        host,
        `Boolean(document.querySelector('button.lr-chip[aria-label="1440p"]'))`,
        deadline,
      ),
      Boolean,
      5_000,
    );
    result.qualityControlsEnabled = await evaluate<boolean>(
      cdp,
      host,
      `(() => {
        const resolution = document.querySelector('button.lr-chip[aria-label="1440p"]');
        const preference = document.querySelector('button[name="degradationPreference"][value="maintain-framerate"]');
        const sliders = [...document.querySelectorAll('.lr-quality-rate input[type="range"]')];
        return Boolean(
          resolution && preference && !resolution.disabled && !preference.disabled &&
          sliders.length === 2 && sliders.every((slider) => !slider.disabled)
        );
      })()`,
      Date.now() + 5_000,
    );
    if (!result.qualityControlsEnabled) {
      throw new Error("Native quality controls remained disabled");
    }
    await evaluate<void>(
      cdp,
      host,
      `document.querySelector('button.lr-chip[aria-label="1440p"]')?.click()`,
      Date.now() + 5_000,
    );
    result.liveQualityChanged = await waitForValue(
      (deadline) => evaluate<boolean>(
        cdp!,
        viewer,
        `(() => { const video = document.querySelector('video'); return Boolean(
          video && window.__piikGateFitsProfile(2560, 1440) &&
          video.getVideoPlaybackQuality().totalVideoFrames >= ${viewerState.frames + 10}
        ); })()`,
        deadline,
      ),
      Boolean,
      20_000,
    );
    if (!result.liveQualityChanged) {
      throw new Error("Native quality change did not reach the Viewer");
    }
    result.viewerWidth = viewerState.width;
    result.viewerHeight = viewerState.height;
    await assertVideoCodec(cdp, viewer, actualCodec);
    state.stage = "native-4k-preset";
    await clickHostControl(cdp, host, `document.querySelectorAll('button.lr-tile')[2]`);
    await waitForValue(deadline => evaluate<boolean>(cdp!, host,
      `document.querySelector('input[name="maxFramerate"]')?.value === '60' &&
        document.querySelector('.lr-sharing-panel')?.getAttribute('aria-busy') === 'false'`,
      deadline), Boolean, 20_000);
    state.stage = "native-4k-controls";
    const framesBefore4K = await evaluate<number>(cdp, viewer,
      `document.querySelector('video')?.getVideoPlaybackQuality().totalVideoFrames ?? 0`, Date.now() + 5_000);
    await evaluate<void>(cdp, host,
      `document.querySelector('button.lr-chip[aria-label="2160p"]')?.click()`, Date.now() + 5_000);
    await waitForValue(deadline => evaluate<boolean>(cdp!, host, `(() => {
      const fps = document.querySelector('input[name="maxFramerate"]');
      const width = document.querySelector('.lr-video-size input[name="width"]');
      const height = document.querySelector('.lr-video-size input[name="height"]');
      return fps && Number(fps.max) >= 60 && fps.validity.valid && fps.value === '60' &&
        width?.value === '3840' && height?.value === '2160';
    })()`, deadline), Boolean, 10_000);
    state.stage = "native-4k-decode";
    result.fourKQualityChanged = await waitForValue(deadline => evaluate<boolean>(cdp!, viewer, `(() => {
      const video = document.querySelector('video');
      return Boolean(video && window.__piikGateFitsProfile(3840, 2160) &&
        video.srcObject === window.__piikGateMedia &&
        video.getVideoPlaybackQuality().totalVideoFrames >= ${framesBefore4K + 10});
    })()`, deadline), Boolean, 20_000);
    await assertVideoCodec(cdp, viewer, actualCodec);
    state.stage = "native-custom-quality";
    const framesBeforeCustom = await evaluate<number>(cdp, viewer,
      `document.querySelector('video')?.getVideoPlaybackQuality().totalVideoFrames ?? 0`, Date.now() + 5_000);
    await evaluate<void>(cdp, host, `(() => {
      const width = document.querySelector('.lr-video-size input[name="width"]');
      const height = document.querySelector('.lr-video-size input[name="height"]');
      width.focus(); width.value = '960';
      height.focus(); height.value = '600'; height.blur();
    })()`, Date.now() + 5_000);
    result.customQualityChanged = await waitForValue(
      deadline => evaluate<boolean>(cdp!, viewer, `(() => {
        const video = document.querySelector('video');
        return Boolean(video && window.__piikGateFitsProfile(960, 600) &&
          video.srcObject === window.__piikGateMedia &&
          video.getVideoPlaybackQuality().totalVideoFrames >= ${framesBeforeCustom + 10});
      })()`, deadline), Boolean, 20_000);
    await assertVideoCodec(cdp, viewer, actualCodec);
    state.stage = "native-rate-range";
    for (const [fps, mbps] of [[1, 0.1], [60, 50]]) {
      for (const [name, value] of [["maxFramerate", fps], ["maxBitrate", mbps]] as const) {
        const selector = JSON.stringify(`input[name="${name}"]`);
        await clickHostControl(cdp, host, `document.querySelector(${selector})`);
        await evaluate<void>(cdp, host, `(() => {
          const input = document.querySelector(${selector});
          window.__piikGateEditingField = input;
          input.select();
        })()`, Date.now() + 5_000);
        await cdp.call("Input.insertText", { text: String(value) }, host.sessionId, Date.now() + 5_000);
        for (const type of ["keyDown", "keyUp"]) {
          await cdp.call("Input.dispatchKeyEvent", {
            type, key: "Enter", code: "Enter", windowsVirtualKeyCode: 13,
          }, host.sessionId, Date.now() + 5_000);
        }
        // NumberControl remounts its input when the committed value changes.
        // Reading the edited DOM value alone can race the blur handler and
        // falsely pass before the new profile has reached native capture.
        await waitForValue(deadline => evaluate<boolean>(cdp!, host,
          `!window.__piikGateEditingField?.isConnected &&
            document.querySelector(${selector})?.value === '${value}' &&
            document.querySelector('.lr-sharing-panel')?.getAttribute('aria-busy') === 'false'`,
          deadline), Boolean, 20_000);
      }
      const before = await evaluate<number>(cdp, viewer,
        `document.querySelector('video')?.getVideoPlaybackQuality().totalVideoFrames ?? 0`, Date.now() + 5_000);
      await waitForValue(deadline => evaluate<boolean>(cdp!, viewer, `(() => {
        const video = document.querySelector('video');
        return Boolean(video && video.srcObject === window.__piikGateMedia &&
          video.getVideoPlaybackQuality().totalVideoFrames >= ${before + 3});
      })()`, deadline), Boolean, 20_000);
      await assertVideoCodec(cdp, viewer, actualCodec);
      result.rateQualityChanges += 1;
    }
    const framesBeforePreference = await evaluate<number>(
      cdp,
      viewer,
      `document.querySelector('video')?.getVideoPlaybackQuality().totalVideoFrames ?? 0`,
      Date.now() + 5_000,
    );
    await evaluate<void>(
      cdp,
      host,
      `document.querySelector('button[name="degradationPreference"][value="maintain-framerate"]')?.click()`,
      Date.now() + 5_000,
    );
    await waitForValue(
      (deadline) => evaluate<number>(
        cdp!,
        viewer,
        `document.querySelector('video')?.getVideoPlaybackQuality().totalVideoFrames ?? 0`,
        deadline,
      ),
      (frames) => frames >= framesBeforePreference + 10,
      15_000,
    );
    state.stage = "native-pause-ready";
    await waitForValue(
      (deadline) => evaluate<boolean>(
        cdp!,
        host,
        `Boolean([...document.querySelectorAll('button')].find((candidate) =>
          /^(Pause sharing|暂停分享)$/.test(candidate.getAttribute('aria-label') || '') &&
          !candidate.disabled && candidate.getAttribute('aria-disabled') !== 'true'
        ))`,
        deadline,
      ),
      Boolean,
      10_000,
    );
    await evaluate<void>(
      cdp,
      host,
      `(() => {
        const button = [...document.querySelectorAll('button')].find((candidate) =>
          /^(Pause sharing|暂停分享)$/.test(candidate.getAttribute('aria-label') || '')
        );
        button?.click();
      })()`,
      Date.now() + 5_000,
    );
    state.stage = "native-pause-applied";
    await waitForValue(
      (deadline) => evaluate<boolean>(
        cdp!,
        host,
        `Boolean([...document.querySelectorAll('button')].find((candidate) =>
          /^(Resume sharing|恢复分享)$/.test(candidate.getAttribute('aria-label') || '')
        ))`,
        deadline,
      ),
      Boolean,
      5_000,
    );
    await evaluate<void>(
      cdp,
      host,
      `document.querySelector('button.lr-chip[aria-label="480p"]')?.click()`,
      Date.now() + 5_000,
    );
    state.stage = "native-paused-profile";
    await waitForValue(
      (deadline) => evaluate<boolean>(
        cdp!,
        host,
        `(() => {
          const resolution = document.querySelector('button.lr-chip[aria-label="480p"]');
          const resume = [...document.querySelectorAll('button')].find((candidate) =>
            /^(Resume sharing|恢复分享)$/.test(candidate.getAttribute('aria-label') || '')
          );
          return Boolean(
            resolution?.getAttribute('aria-pressed') === 'true' && resume &&
            !resume.disabled && resume.getAttribute('aria-disabled') !== 'true'
          );
        })()`,
        deadline,
      ),
      Boolean,
      10_000,
    );
    const framesBeforeResume = await evaluate<number>(
      cdp,
      viewer,
      `document.querySelector('video')?.getVideoPlaybackQuality().totalVideoFrames ?? 0`,
      Date.now() + 5_000,
    );
    await evaluate<void>(
      cdp,
      host,
      `(() => {
        const button = [...document.querySelectorAll('button')].find((candidate) =>
          /^(Resume sharing|恢复分享)$/.test(candidate.getAttribute('aria-label') || '')
        );
        button?.click();
      })()`,
      Date.now() + 5_000,
    );
    state.stage = "native-resumed-profile";
    const resumed = await waitForValue(
      (deadline) => evaluate<{
        frames: number;
        width: number;
        height: number;
        sameMedia: boolean;
      }>(
        cdp!,
        viewer,
        `(() => {
          const video = document.querySelector('video');
          return {
            frames: video?.getVideoPlaybackQuality().totalVideoFrames ?? 0,
            width: video?.videoWidth ?? 0,
            height: video?.videoHeight ?? 0,
            sameMedia: Boolean(video && video.srcObject === window.__piikGateMedia),
          };
        })()`,
        deadline,
      ),
      (value) => value.frames >= framesBeforeResume + 10 &&
        matchesCaptureSize(value.width, value.height, fittedCaptureSize(viewerState, 854, 480)),
      20_000,
    );
    result.pausedQualityChanged = matchesCaptureSize(resumed.width, resumed.height, fittedCaptureSize(viewerState, 854, 480));
    result.mediaObjectPreserved = resumed.sameMedia;
    result.viewerWidth = resumed.width;
    result.viewerHeight = resumed.height;
    await assertVideoCodec(cdp, viewer, actualCodec);
    if (sourceKind === "window") {
      state.stage = "native-background-profile";
      const { targetInfos } = await sourceCdp()!.call<{
        targetInfos: Array<{ targetId: string; type: string; url: string }>;
      }>("Target.getTargets", {}, undefined, Date.now() + 5_000);
      const target = targetInfos.find((value) =>
        value.type === "page" && value.url === `http://127.0.0.1:${sourcePort}/`,
      );
      if (!target) throw new Error("Capture source page is missing");
      const { windowId } = await sourceCdp()!.call<{ windowId: number }>(
        "Browser.getWindowForTarget", { targetId: target.targetId },
        undefined, Date.now() + 5_000,
      );
      await sourceCdp()!.call("Browser.setWindowBounds", {
        windowId, bounds: { windowState: "minimized" },
      }, undefined, Date.now() + 5_000);
      try {
        await evaluate<void>(cdp, host,
          `new Promise((resolve) => {
            document.querySelector('button.lr-chip[aria-label="720p"]')?.click();
            requestAnimationFrame(() => requestAnimationFrame(resolve));
          })`,
          Date.now() + 5_000,
        );
        await waitForValue((deadline) => evaluate<boolean>(cdp!, host,
          `Boolean(document.querySelector('#host-advanced-door .lr-sharing-panel[aria-busy="true"]'))`,
          deadline,
        ), Boolean, 5_000);
      } finally {
        await sourceCdp()!.call("Browser.setWindowBounds", {
          windowId, bounds: { windowState: "normal" },
        }, undefined, Date.now() + 5_000);
      }
      state.stage = "native-background-profile-recovery";
      // A quiet source retains its pending replacement until restoration.
      // Prove its actual delivery before applying the following profile.
      for (const [label, width, height] of [["720p", 1280, 720], ["480p", 854, 480]] as const) {
        const before = await evaluate<number>(cdp, viewer,
          `document.querySelector('video')?.getVideoPlaybackQuality().totalVideoFrames ?? 0`,
          Date.now() + 5_000,
        );
        await evaluate<void>(cdp, host,
          `document.querySelector('button.lr-chip[aria-label="${label}"]')?.click()`,
          Date.now() + 5_000,
        );
        await waitForValue((deadline) => evaluate<boolean>(cdp!, viewer,
          `(() => {
            const video = document.querySelector('video');
            return Boolean(video && video.srcObject === window.__piikGateMedia &&
              window.__piikGateFitsProfile(${width}, ${height}) &&
              video.getVideoPlaybackQuality().totalVideoFrames >= ${before + 10});
          })()`, deadline,
        ), Boolean, 20_000);
        await waitForValue((deadline) => evaluate<boolean>(cdp!, host,
          `Boolean(document.querySelector('#host-advanced-door .lr-sharing-panel[aria-busy="false"]'))`,
          deadline,
        ), Boolean, 5_000);
      }
      result.backgroundProfileRecovery = true;
      // WGC must keep the share through size changes and a temporarily quiet
      // minimized window. This is a window-transition check, not a game test.
      for (const bounds of [
        { width: 960, height: 650 },
        { windowState: "fullscreen" },
        { windowState: "normal" },
        { windowState: "minimized" },
      ]) {
        state.stage = "native-window-" + (bounds.windowState ?? "resize");
        await sourceCdp()!.call("Browser.setWindowBounds", {
          windowId, bounds,
        }, undefined, Date.now() + 5_000);
        if (bounds.windowState === "minimized") {
          await new Promise((resolveWait) => setTimeout(resolveWait, 5_000));
          await sourceCdp()!.call("Browser.setWindowBounds", {
            windowId, bounds: { windowState: "normal" },
          }, undefined, Date.now() + 5_000);
        }
        const before = await evaluate<number>(cdp, viewer,
          "document.querySelector('video')?.getVideoPlaybackQuality().totalVideoFrames ?? 0", Date.now() + 5_000);
        await waitForValue((deadline) => evaluate<boolean>(cdp!, viewer,
          `(() => {
            const video = document.querySelector('video');
            return Boolean(video && video.srcObject === window.__piikGateMedia &&
              video.getVideoPlaybackQuality().totalVideoFrames >= ${before + 30});
          })()`, deadline,
        ), Boolean, 20_000);
        await assertVideoCodec(cdp, viewer, actualCodec);
      }
      result.sourceWindowRecovery = true;
    }
    state.stage = "native-source-picker";
    await waitForValue(
      (deadline) => evaluate<boolean>(
        cdp!,
        host,
        `Boolean([...document.querySelectorAll('button')].find((candidate) =>
          /^(Switch source|切换来源)$/.test(candidate.getAttribute('aria-label') || '') &&
          !candidate.disabled && candidate.getAttribute('aria-disabled') !== 'true'
        ))`,
        deadline,
      ),
      Boolean,
      10_000,
    );
    await evaluate<void>(
      cdp,
      host,
      `(() => {
        const button = [...document.querySelectorAll('button')].find((candidate) =>
          /^(Switch source|切换来源)$/.test(candidate.getAttribute('aria-label') || '')
        );
        button?.click();
      })()`,
      Date.now() + 5_000,
    );
    await waitForValue(
      (deadline) => evaluate<boolean>(
        cdp!,
        host,
        `Boolean(document.querySelector('[role="dialog"] button[data-native-source]'))`,
        deadline,
      ),
      Boolean,
      10_000,
    );
    const framesBeforeSourceChange = resumed.frames;
    await selectNativeSource(cdp, host, sourceKind,
      sourceKind === "window" ? SOURCE_TITLE : undefined,
    );
    state.stage = "native-source-replaced";
    result.nativeSourceChanged = await waitForValue(
      (deadline) => evaluate<boolean>(
        cdp!,
        viewer,
        `(() => {
          const video = document.querySelector('video');
          return Boolean(
            video && video.srcObject === window.__piikGateMedia &&
            video.videoWidth > 0 && video.videoWidth <= 854 && video.videoHeight > 0 && video.videoHeight <= 480 &&
            video.getVideoPlaybackQuality().totalVideoFrames >= ${framesBeforeSourceChange + 10}
          );
        })()`,
        deadline,
      ),
      Boolean,
      20_000,
    );
    state.stage = "native-quality-evidence";
    await assertVideoCodec(cdp, viewer, actualCodec);
    result.codecPreserved = true;
    result.nativeQualityEvidence = await waitForValue(
      async () => {
        const logPath = join(diagnosticDirectory, "client.log");
        if ((await stat(logPath)).size > 8 * 1024 * 1024) {
          throw new Error("Native Host gate diagnostic log exceeded its bound");
        }
        const diagnostics = await readFile(logPath, "utf8");
        return diagnostics.split(/\r?\n/).some((line) => {
          try {
            const record = JSON.parse(line);
            return record.msg === "piik-route" && record.event === "sender-quality-evidence" &&
              (record.details?.state === "healthy" || record.details?.state === "degraded");
          } catch {
            return false;
          }
        });
      },
      Boolean,
      12_000,
    );
}
