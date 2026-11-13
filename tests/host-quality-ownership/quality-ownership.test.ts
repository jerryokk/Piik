import { describe, expect, it, vi } from "vitest";
import { QUALITY_PROFILES, type QualitySettings } from "../../src/client/media/quality";
import { NativeCompatibilityError, type NativeClient } from "../../src/client/native/client";
import { type NativeSourceList } from "../../src/client/ui/components/share/CaptureSourcePicker";
import { NativeMediaBridgeError } from "../../src/client/native/media-bridge";
import { ApiError } from "../../src/client/lib/api";
import { hostServerErrorKey } from "../../src/client/pages/host/notices";
import { setCopy } from "../../src/client/ui/copy";
import { fixture, useRealResourceDisposal, original, lower, ref, deferred, ingress } from "./support.ts";

describe("Host quality ownership", () => {
  it("rejects unsupported settings without changing capture, room or saved intent", async () => {
    const current = fixture();
    await current.change({ ...original, maxFramerate: 480, maxBitrate: 40_000_000 });
    expect(current.qualitySettingsRef.current).toEqual(original);
    expect(current.applyCaptureProfile).not.toHaveBeenCalled();
    expect(current.signalRef.current.setHostQualitySettings).not.toHaveBeenCalled();
    expect(current.context.savePreferredQuality).not.toHaveBeenCalled();
    expect(current.advancedQualityRef.current).toEqual(original);
  });

  it("ignores an unsupported App preference without erasing it", async () => {
    const current = fixture();
    current.activeGenerationRef.current = null;
    const preference = { read: vi.fn(async () => ({ ...original, maxFramerate: 480 })), save: vi.fn() };
    current.context.AppQualityPreference = { connect: vi.fn(async () => preference) };
    await current.context.loadAppQualityPreference(new AbortController().signal);
    expect(current.qualitySettingsRef.current).toEqual(original);
    expect(preference.save).not.toHaveBeenCalled();
    expect(current.NativeClient.connect).not.toHaveBeenCalled();
  });

  it("restores App preferences without acquiring media or changing a room", async () => {
    const current = fixture();
    current.activeGenerationRef.current = null;
    const preference = { read: vi.fn(async () => lower), save: vi.fn() };
    current.context.AppQualityPreference = { connect: vi.fn(async () => preference) };
    await current.context.loadAppQualityPreference(new AbortController().signal);
    expect(current.qualitySettingsRef.current).toEqual(lower);
    expect(current.NativeClient.connect).not.toHaveBeenCalled();
    expect(current.signalRef.current.setHostQualitySettings).not.toHaveBeenCalled();
    expect(preference.save).not.toHaveBeenCalled();
  });

  it.each(["explicit", "room", "sharing", "creating", "retired"])(
    "does not let a late App preference override %s ownership", async (owner) => {
      const current = fixture();
      current.activeGenerationRef.current = null;
      const pending = deferred<QualitySettings>();
      const preference = { read: vi.fn(() => pending.promise), save: vi.fn() };
      current.context.AppQualityPreference = { connect: vi.fn(async () => preference) };
      const controller = new AbortController();
      const loading = current.context.loadAppQualityPreference(controller.signal);
      await Promise.resolve();
      if (owner === "explicit") current.qualityPreferenceEditedRef.current = true;
      if (owner === "room") current.context.commitQuality({ ...original });
      if (owner === "sharing") current.activeGenerationRef.current = 2;
      if (owner === "creating") current.roomMutationRef.current = {};
      if (owner === "retired") controller.abort();
      pending.resolve(lower);
      await loading;
      expect(current.qualitySettingsRef.current).toEqual(original);
      expect(preference.save).not.toHaveBeenCalled();
    },
  );

  it("carries an applied choice across delayed App discovery without persisting room snapshots", async () => {
    const current = fixture();
    current.context.phase = "idle";
    current.activeGenerationRef.current = null;
    const preference = { read: vi.fn(async () => original), save: vi.fn() };
    current.context.AppQualityPreference = { connect: vi.fn(async () => preference) };
    await current.change(lower);
    await current.context.loadAppQualityPreference(new AbortController().signal);
    expect(preference.save).toHaveBeenCalledExactlyOnceWith(lower);
    expect(current.qualitySettingsRef.current).toEqual(lower);
    current.context.commitQuality(original);
    expect(preference.save).toHaveBeenCalledOnce();
    await current.change(lower);
    expect(preference.save).toHaveBeenCalledTimes(2);
  });

  it("remembers an explicit pre-share choice without changing room or capture state", async () => {
    const current = fixture();
    current.context.phase = "idle";
    await current.change(lower);
    expect(current.savePreferredQuality).toHaveBeenCalledWith(lower);
    expect(current.applyCaptureProfile).not.toHaveBeenCalled();
    expect(current.signalRef.current.setHostQualitySettings).not.toHaveBeenCalled();
  });

  it("does not replace a local preference with reauthenticated room settings", () => {
    const current = fixture();
    current.reauthenticate();
    expect(current.savePreferredQuality).not.toHaveBeenCalled();
  });

  it.each([true, false])("resumes the former raw audio input only after an accepted mixed-output resume: %s", (accepted) => {
    const current = fixture(false);
    const source = {} as MediaStream;
    current.hostAudioRef.current = { sourceStream: source };
    current.peersRef.current.clear();
    current.context.hostSfuRouteRef.current = null;
    const setMediaPaused = vi.fn(() => true);
    Object.assign(current.context, {
      sharingPausedRef: ref(true), setSharingPaused: vi.fn(), setMediaPaused,
      signalRef: ref({ setSharingPaused: () => accepted, confirmSharingPaused: vi.fn() }),
      setNoticeErrorKey: vi.fn(),
    });
    current.context.toggleSharingPause();
    expect(setMediaPaused).toHaveBeenCalledWith(current.streamRef.current, false);
    if (accepted) expect(setMediaPaused).toHaveBeenCalledWith(source, false);
    else {
      expect(setMediaPaused).not.toHaveBeenCalledWith(source, false);
      expect(setMediaPaused).toHaveBeenLastCalledWith(current.streamRef.current, true);
    }
  });

  it("refreshes source details from the capture input", async () => {
    const current = fixture(false);
    const source = {} as MediaStream;
    current.hostAudioRef.current = { sourceStream: source };
    const captureDetails = vi.fn(() => ({}));
    current.context.captureDetails = captureDetails;
    await current.change(lower);
    expect(captureDetails).toHaveBeenCalledWith(source);
  });

  it.each([false, true])("remembers the border preference and passes it to native capture: replacing %s", async (replacing) => {
    const current = fixture();
    current.context.phase = replacing ? "live" : "idle";
    current.nativeModeRef.current = replacing;
    Object.assign(current.client.health.nativeMedia, { captureBorderControl: true });
    await current.openPicker();
    current.context.nativeSources = current.setNativeSources.mock.calls.at(-1)![0];
    const target = { kind: "display", sourceId: "2", title: "Display 1" };
    current.context.startNativeShareFromPicker(target, false, true);
    expect(current.setShowCaptureBorder).toHaveBeenCalledWith(true);
    if (replacing) {
      await vi.waitFor(() => expect(current.client.replaceShareSource).toHaveBeenCalledWith(
        "share", target, false, { adapterIndex: 0, encoderIndex: 0 }, true, undefined,
      ));
    } else {
      const selection = current.startSharing.mock.calls[0]![0];
      expect(selection).toMatchObject({ kind: "native", target, audio: false, showCaptureBorder: true });
      current.client.startShare.mockRejectedValue(new Error("capture unavailable"));
      await expect(current.context.startNativeShare(1, "share", selection)).rejects.toThrow("capture unavailable");
      expect(current.client.startShare).toHaveBeenCalledWith(expect.objectContaining({ showCaptureBorder: true }));
    }
  });

  it("retains exclusion intent across audio off/on and a failed replacement until share retirement", async () => {
    const current = fixture();
    current.nativeModeRef.current = true;
    const target = { kind: "display", sourceId: "2", title: "Display" };
    const excluded = { kind: "window", sourceId: "3", pid: 123, creationTime: "456", title: "Voice fixture" };
    const path = { adapterIndex: 0, encoderIndex: 0 };
    for (const enabled of [true, false, true]) {
      await current.context.switchNativeSource(current.client, target, enabled, path, false, excluded);
      expect(current.nativeSourceSelectionRef.current).toMatchObject({ target, audio: enabled, excludeAudio: excluded });
      expect(current.client.replaceShareSource).toHaveBeenLastCalledWith("share", target, enabled, path, false, enabled ? excluded : undefined);
    }
    const next = { ...excluded, pid: 321, creationTime: "654" };
    current.client.replaceShareSource.mockRejectedValueOnce(new Error("audio unavailable"));
    await current.context.switchNativeSource(current.client, target, true, path, false, next);
    expect(current.nativeSourceSelectionRef.current).toMatchObject({ target, audio: true, excludeAudio: next });
    current.disposeNative();
    expect(current.nativeSourceSelectionRef.current).toBeNull();
  });

  it("changes native source audio without resetting sender quality or SFU publication", async () => {
    const current = fixture();
    current.nativeModeRef.current = true;
    current.routePolicyRef.current.topologyOptimization = true;
    const target = { kind: "display", sourceId: "2", title: "Display" };
    const path = { adapterIndex: 0, encoderIndex: 0 };
    current.nativeSourceSelectionRef.current = { target, path, client: current.client, showCaptureBorder: false, audio: true };
    const excluded = { kind: "window", sourceId: "3", pid: 123, creationTime: "456", title: "Voice" };
    await current.context.switchNativeSource(current.client, { ...target, title: "Renamed" }, true, path, false, excluded);
    expect(current.client.replaceShareSource).toHaveBeenCalledOnce();
    expect(current.invalidateSenderQualityEvidence).not.toHaveBeenCalled();
    expect(current.signalRef.current.send).not.toHaveBeenCalled();
    expect(current.hostSfuRouteRef.current.updateProfile).not.toHaveBeenCalled();
    expect(current.sourceSwitchRef.current).toBeNull();
  });

  it.each(["browser", "native"] as const)("keeps the %s selection until room work permits capture", async (kind) => {
    const current = fixture();
    current.context.phase = "idle";
    current.activeGenerationRef.current = null;
    current.nativeShareGenerationRef.current = null;
    const denied = new DOMException("cancelled", "NotAllowedError");
    const capture = vi.fn(async () => { throw denied; });
    current.context.captureBrowserSource = capture;
    current.client.startShare.mockRejectedValue(denied);
    await current.openPicker();
    current.context.nativeSources = current.setNativeSources.mock.calls.at(-1)![0];
    const request = current.nativeSourceRequestRef.current;
    const path = current.nativeSourcePathRef.current;
    const mutation = current.context.beginRoomMutation("access");
    let starting: Promise<void> | undefined;
    current.startSharing.mockImplementation(() => {
      starting = kind === "native" ? current.start() : current.startBrowser();
      return starting;
    });
    const confirm = () => kind === "native"
      ? current.context.startNativeShareFromPicker({}, false)
      : current.context.startBrowserShareFromPicker();
    confirm();
    await starting;
    expect(current.nativeSourceRequestRef.current).toBe(request);
    expect(current.nativeSourcePathRef.current).toBe(path);
    expect(current.client.startShare).not.toHaveBeenCalled();
    expect(capture).not.toHaveBeenCalled();

    current.context.finishRoomMutation(mutation);
    confirm();
    // Browser capture must still begin synchronously in the accepting gesture.
    if (kind === "browser") expect(capture).toHaveBeenCalledOnce();
    expect(current.nativeSourceRequestRef.current).toBeNull();
    await starting;
    if (kind === "native") expect(current.client.startShare).toHaveBeenCalledOnce();
  });

  it("retires an open switch picker and its control when sharing ends", async () => {
    const current = fixture();
    current.generationRef.current = current.activeGenerationRef.current!;
    current.nativeModeRef.current = true;
    await current.openPicker();
    expect(current.nativeSourceRequestRef.current).not.toBeNull();
    useRealResourceDisposal(current);
    Object.assign(current.signalRef, { current: null });
    current.context.endSharing({ key: "host.notice.stopped" });
    expect(current.nativeSourceRequestRef.current).toBeNull();
    expect(current.nativeSourcePathRef.current).toBeNull();
    expect(current.setNativeSources).toHaveBeenLastCalledWith(null);
    expect(current.client.close).toHaveBeenCalledOnce();
    expect(current.activeGenerationRef.current).toBeNull();
  });

  it.each(["browser", "app-browser", "native"])("keeps %s capture cancellation in the television status", async (entry) => {
    const current = fixture(entry !== "browser");
    const denied = new DOMException("private capture detail", "NotAllowedError");
    const setNoticeValue = vi.fn();
    Object.assign(current.context, {
      phase: "idle", setNoticeValue, setNoticeError: current.writeNoticeError,
      captureBrowserSource: vi.fn(async () => { throw denied; }),
    });
    current.activeGenerationRef.current = null;
    current.nativeShareGenerationRef.current = null;
    current.client.startShare.mockRejectedValue(denied);
    await (entry === "native" ? current.start() : current.startBrowser());
    expect(setNoticeValue).toHaveBeenLastCalledWith({
      kind: "key", key: "host.capture.cancelled",
      target: "television", comic: "hint-capture-browser", tone: "warn",
      failureCode: "browser/NotAllowedError",
    });
    expect(current.setPhase).toHaveBeenLastCalledWith("idle");
    expect(current.createRoom).not.toHaveBeenCalled();
    expect(current.roomMutationRef.current).toBeNull();
  });

  it.each(["camera", "browser"])("keeps a rejected %s source change out of healthy media status", (source) => {
    const current = fixture();
    current.context.setCaptureError(new DOMException("denied", "NotAllowedError"), source, "source");
    expect(current.setNoticeValue).toHaveBeenLastCalledWith(expect.objectContaining({ target: "operation", tone: "warn" }));
    expect(current.setPhase).not.toHaveBeenCalled();
    expect(current.track.stop).not.toHaveBeenCalled();
  });

  it("rejects unavailable Browser media before starting native capture or creating a room", async () => {
    const current = fixture();
    const failure = new NativeMediaBridgeError("Preview unavailable", {
      cause: new DOMException("Peer connection rejected", "NotAllowedError"),
    });
    const report = vi.fn();
    Object.assign(current.context, { phase: "idle", debugError: report });
    current.activeGenerationRef.current = null;
    current.nativeShareGenerationRef.current = null;
    current.NativeMediaBridge.mockImplementationOnce(function () { throw failure; });
    await current.start();

    expect(current.client.startShare).not.toHaveBeenCalled();
    expect(current.client.stopShare).not.toHaveBeenCalled();
    expect(current.client.close).toHaveBeenCalledOnce();
    expect(current.createRoom).not.toHaveBeenCalled();
    expect(current.roomMutationRef.current).toBeNull();
    expect(current.setPhase).toHaveBeenLastCalledWith("error");
    expect(current.setNoticeValue).toHaveBeenLastCalledWith(expect.objectContaining({
      target: "television", comic: "route-failed", tone: "bad",
    }));
    expect(report).toHaveBeenCalledWith("capture", "failed", failure, { action: "capture", source: undefined });
  });

  it.each(["reject", "cancel", "accept"])("owns the Browser reservation while native startup is pending: %s", async (outcome) => {
    const current = fixture();
    current.nativeShareGenerationRef.current = null;
    const started = deferred<{ audio: boolean; codec: "h264" }>();
    current.client.startShare.mockReturnValue(started.promise);
    const starting = current.startNative();
    const result = outcome === "accept" ? expect(starting).resolves.toBeDefined()
      : expect(starting).rejects.toThrow("capture unavailable");
    await vi.waitFor(() => expect(current.client.startShare).toHaveBeenCalledOnce());
    expect(current.nativeMediaBridgeRef.current).toBe(current.bridge);
    expect(current.nativeShareGenerationRef.current).toBeNull();
    expect(current.nativeModeRef.current).toBe(false);
    expect(current.bridge.start).not.toHaveBeenCalled();
    if (outcome === "accept") {
      // The App can supply its mixed audio track even when source audio was off.
      started.resolve({ audio: true, codec: "h264" });
    } else {
      if (outcome === "cancel") {
        current.activeGenerationRef.current = null;
        current.disposeNative();
        expect(current.bridge.dispose).toHaveBeenCalledOnce();
        expect(current.client.close).toHaveBeenCalledOnce();
      }
      started.reject(new Error("capture unavailable"));
    }
    await result;
    if (outcome === "accept") {
      expect(current.bridge.start).toHaveBeenCalledExactlyOnceWith(true);
      expect(current.nativeShareGenerationRef.current).toBe("share");
      expect(current.bridge.dispose).not.toHaveBeenCalled();
    } else {
      expect(current.bridge.dispose).toHaveBeenCalled();
      expect(current.nativeMediaBridgeRef.current).toBeNull();
      expect(current.client.stopShare).not.toHaveBeenCalled();
    }
  });

  it.each(["camera", "browser"])("retains the %s capture cause when presenting its notice", (source) => {
    const current = fixture();
    const report = vi.fn();
    current.context.debugError = report;
    const cause = new DOMException("Device unavailable", "NotReadableError");
    current.context.setCaptureError(cause, source, "capture");
    expect(report).toHaveBeenCalledWith("capture", "failed", cause, { action: "capture", source });
  });

  it("keeps a rejected source change as operation feedback while the current share stays live", async () => {
    const current = fixture();
    const denied = new DOMException("private capture detail", "NotAllowedError");
    const setNoticeValue = vi.fn();
    Object.assign(current.context, {
      setNoticeValue, setNoticeError: current.writeNoticeError,
    });
    current.client.replaceShareSource.mockRejectedValue(denied);
    await current.switchSource();
    expect(setNoticeValue).toHaveBeenLastCalledWith({
      kind: "key", key: "host.capture.cancelled",
      target: "operation", comic: "hint-capture-browser", tone: "warn",
      failureCode: "browser/NotAllowedError",
    });
    expect(current.setPhase).not.toHaveBeenCalled();
    expect(current.track.stop).not.toHaveBeenCalled();
  });

  it("keeps error keys until the actual page rendering uses the current language", () => {
    const current = fixture();
    const denied = new DOMException("private capture detail", "NotAllowedError");
    current.writeNoticeError(denied, "capture", "television");
    const notice = current.setNoticeValue.mock.lastCall![0];
    expect(notice).toMatchObject({ kind: "key", key: "host.capture.cancelled" });
    try {
      setCopy({ lang: "en" });
      expect(current.renderNotice(notice)).toBe("Screen selection cancelled or not permitted");
      setCopy({ lang: "zh" });
      expect(current.renderNotice(notice)).toBe("屏幕选择已取消或没有共享权限");
      current.context.setNoticeKey(hostServerErrorKey("ROOM_FULL"), "signal-failed", "bad");
      expect(current.renderNotice(current.setNoticeValue.mock.lastCall![0])).toBe("房间已满");
      current.writeNoticeError(new ApiError("HTTP failure with specific context", 403), "room");
      expect(current.renderNotice(current.setNoticeValue.mock.lastCall![0])).toBe("HTTP failure with specific context");
    } finally {
      setCopy({ lang: "zh" });
    }
  });

  it.each([
    { key: "host.stopNotice", comic: "share-ended", tone: "off" },
    { key: "host.shareEnded", comic: "source-failed", tone: "bad" },
  ])("keeps the share ending reason and tone together in the television: $tone", ({ key, comic, tone }) => {
    const current = fixture();
    const setNoticeValue = vi.fn();
    current.context.setNoticeValue = setNoticeValue;
    current.generationRef.current = 1;
    current.context.endSharing({ key }, false, comic, tone);
    expect(setNoticeValue).toHaveBeenCalledExactlyOnceWith({
      kind: "key", key, vars: undefined,
      target: "television", comic, tone,
    });
    expect(current.setPhase).toHaveBeenLastCalledWith("ended");
    expect(current.activeGenerationRef.current).toBeNull();
  });

  it.each([
    ["capture", "host.shareEnded", "app/capture/failed", "source-failed"],
    ["media", "native.fail.edge", "app/browser-media/connection", "route-failed"],
    ["control", "native.fail.controlDisconnected", "app/control/disconnected", "route-failed"],
  ])("retains the known %s failure when retiring native sharing", async (kind, key, failureCode, comic) => {
    const current = fixture();
    current.generationRef.current = 1;
    await current.startNative();
    if (kind === "control") current.client.onClose.mock.calls[0]![0]();
    else if (kind === "media") current.NativeMediaBridge.mock.calls[0]![2]();
    else current.client.onEvent.mock.calls[0]![0]({ type: "share-ended", shareId: "share", failed: true });
    expect(current.setNoticeValue).toHaveBeenLastCalledWith({
      kind: "key", key, vars: undefined, failureCode, target: "television", comic, tone: "bad",
    });
    // A failed publication retires; the persistent room session survives.
    expect(current.disposeResources).toHaveBeenCalledExactlyOnceWith(true, true);
    expect(current.activeGenerationRef.current).toBeNull();
  });

  it("retains connecting children during reconciliation and rebuilds them after reauthentication", async () => {
    const current = fixture(false);
    current.nativeConnection.release();
    current.peer.isConnected.mockReturnValue(false);
    const healthy = { ...current.peer, isConnected: vi.fn(() => true) };
    const replacement = { ...current.peer, start: vi.fn(async () => true) };
    current.peersRef.current.set("healthy", healthy);
    current.removePeer.mockImplementation((peerId: string) => current.peersRef.current.delete(peerId));
    const createPeer = vi.fn(function () { return replacement; });
    Object.assign(current.context, {
      startPeer: current.startChild, HostPeer: createPeer, browserVideoPool: () => null,
      iceConfigRef: ref({ iceServers: [] }),
      hostChildIsAssigned: (peerId: string) => current.activeHostChildPeerIdsRef.current.includes(peerId),
    });
    const reconcile = () => current.context.reconcileHostChildren(["viewer", "healthy"], 1);
    reconcile();
    expect(current.removePeer).not.toHaveBeenCalled();
    expect(current.peersRef.current.get("viewer")).toBe(current.peer);
    expect(createPeer).not.toHaveBeenCalled();

    current.route.resyncAuthoritative.mockImplementation(async () => { reconcile(); });
    current.reauthenticate();
    await vi.waitFor(() => expect(replacement.start).toHaveBeenCalledOnce());
    expect(current.removePeer).toHaveBeenCalledExactlyOnceWith("viewer");
    expect(current.peersRef.current.get("viewer")).toBe(replacement);
    expect(current.peersRef.current.get("healthy")).toBe(healthy);
    expect(createPeer).toHaveBeenCalledOnce();
  });

  it("opens the same source picker for Browser and App without probing localhost on the Browser path", async () => {
    const browser = fixture(false);
    browser.nativeConnection.release();
    browser.requestShare();
    expect(browser.startSharing).not.toHaveBeenCalled();
    expect(browser.openCaptureSourcePicker).toHaveBeenCalledOnce();
    await browser.openPicker();
    expect(browser.setNativeSources).toHaveBeenLastCalledWith({ kind: "browser" });
    expect(browser.NativeClient.connect).not.toHaveBeenCalled();

    const current = fixture();
    current.nativeConnection.release();
    current.routePolicyRef.current.topologyOptimization = true;
    const discovery = deferred<typeof current.client | null>();
    current.NativeClient.connect.mockReturnValue(discovery.promise);
    const attempt = current.discover();
    current.requestShare();
    expect(current.openCaptureSourcePicker).toHaveBeenCalledTimes(1);
    expect(current.discover()).toBe(attempt);
    await current.startOptionalIngress();
    discovery.resolve(null);
    await attempt;
    expect(current.NativeClient.connect).toHaveBeenCalledTimes(1);
    current.NativeClient.connect.mockResolvedValue(current.client);
    await expect(current.discover()).resolves.toBe(current.client);
    expect(current.NativeClient.connect).toHaveBeenCalledTimes(2);
    current.nativeConnection.adopt(current.client as unknown as NativeClient);
    current.requestShare();
    expect(current.openCaptureSourcePicker).toHaveBeenCalledTimes(2);
  });

  it("refreshes a waiting picker after App approval without changing a live Browser share", async () => {
    const current = fixture(false);
    current.context.phase = "idle";
    await current.openPicker();
    expect(current.setNativeSources).toHaveBeenLastCalledWith({ kind: "browser" });
    expect(current.client.sources).not.toHaveBeenCalled();

    current.isClientActivated.mockReturnValue(true);
    await current.openPicker();
    expect(current.setNativeSources).toHaveBeenLastCalledWith(expect.objectContaining({ kind: "ready" }));
    expect(current.client.sources).toHaveBeenCalledOnce();

    let sources: NativeSourceList | null = current.setNativeSources.mock.lastCall![0];
    current.setNativeSources.mockImplementation((next: NativeSourceList | null | ((previous: NativeSourceList | null) => NativeSourceList)) => {
      sources = typeof next === "function" ? next(sources) : next;
    });
    const enumeration = deferred<never[]>();
    current.client.sources.mockReturnValueOnce(enumeration.promise);
    const refresh = current.openPicker();
    await vi.waitFor(() => expect(current.client.sources).toHaveBeenCalledTimes(2));
    expect(sources).toEqual({ kind: "loading", appDetected: true, sources: [] });
    enumeration.resolve([]);
    await refresh;
    expect(sources).toMatchObject({ kind: "ready", sources: [] });

    current.context.phase = "live";
    await current.openPicker();
    expect(current.setNativeSources).toHaveBeenLastCalledWith(expect.objectContaining({ kind: "ready" }));
    expect(current.client.sources).toHaveBeenCalledTimes(3);
    expect(current.streamRef.current).not.toBeNull();
  });

  it("shows App incompatibility in the picker and clears it on a successful refresh", async () => {
    const current = fixture();
    current.context.phase = "idle";
    current.nativeConnection.release();
    current.nativeShareGenerationRef.current = null;
    current.NativeClient.connect.mockRejectedValueOnce(new NativeCompatibilityError(8));
    await current.openPicker();
    expect(current.setNativeSources).toHaveBeenLastCalledWith({ kind: "incompatible" });
    expect(current.nativeConnection.current).toBeNull();
    expect(current.client.sources).not.toHaveBeenCalled();
    current.NativeClient.connect.mockResolvedValue(current.client);
    await current.openPicker();
    expect(current.NativeClient.connect).toHaveBeenCalledTimes(2);
    expect(current.setNativeSources).toHaveBeenLastCalledWith(expect.objectContaining({ kind: "ready" }));
    current.closePicker();
  });

  it("distinguishes absent App, missing capture capability, incompatible codec and an empty source list", async () => {
    const absent = fixture();
    absent.context.phase = "idle";
    absent.nativeConnection.release();
    await absent.openPicker();
    expect(absent.setNativeSources).toHaveBeenLastCalledWith({ kind: "unavailable" });

    const unsupported = fixture();
    unsupported.context.phase = "idle";
    unsupported.client.health.nativeMedia.video = false;
    await unsupported.openPicker();
    expect(unsupported.setNativeSources).toHaveBeenLastCalledWith({ kind: "unsupported" });
    expect(unsupported.client.sources).not.toHaveBeenCalled();

    const codec = fixture();
    codec.context.phase = "idle";
    codec.context.defaultNativeCapturePath = () => null;
    await codec.openPicker();
    expect(codec.setNativeSources).toHaveBeenLastCalledWith({ kind: "unsupported" });

    const empty = fixture();
    empty.context.phase = "idle";
    await empty.openPicker();
    expect(empty.setNativeSources).toHaveBeenLastCalledWith(expect.objectContaining({ kind: "ready", sources: [] }));
  });

  it("reports listing failure separately and reconnects on refresh", async () => {
    const current = fixture();
    current.context.phase = "idle";
    current.nativeShareGenerationRef.current = null;
    current.client.sources.mockRejectedValueOnce(new Error("capture helper exited"));
    await current.openPicker();
    expect(current.setNativeSources).toHaveBeenLastCalledWith({ kind: "failed" });
    expect(current.client.close).toHaveBeenCalledOnce();
    const replacement = { ...current.client, close: vi.fn() };
    current.NativeClient.connect.mockResolvedValue(replacement);
    await current.openPicker();
    expect(current.setNativeSources).toHaveBeenLastCalledWith(expect.objectContaining({ kind: "ready" }));
    current.closePicker();
    expect(replacement.close).toHaveBeenCalledOnce();
  });

  it("keeps a live Native share when source enumeration fails", async () => {
    const current = fixture();
    current.nativeModeRef.current = true;
    current.client.sources.mockRejectedValueOnce(new Error("list failed"));
    await current.openPicker();
    expect(current.setNativeSources).toHaveBeenLastCalledWith({ kind: "failed" });
    expect(current.client.close).not.toHaveBeenCalled();
    expect(current.client.stopShare).not.toHaveBeenCalled();
    expect(current.nativeConnection.current).toBe(current.client);
  });

  it("ignores a cancelled discovery mismatch after another picker owns a compatible App", async () => {
    const current = fixture(true, false);
    current.context.phase = "idle";
    current.nativeShareGenerationRef.current = null;
    const discovery = deferred<void>();
    current.NativeClient.connect.mockReturnValueOnce(discovery.promise.then(() => {
      throw new NativeCompatibilityError(8);
    }));
    const obsolete = current.openPicker();
    await vi.waitFor(() => expect(current.NativeClient.connect).toHaveBeenCalledOnce());
    current.closePicker();
    current.NativeClient.connect.mockResolvedValue(current.client);
    await current.openPicker();
    current.setNativeSources.mockClear();
    discovery.resolve();
    await obsolete;
    expect(current.setNativeSources).not.toHaveBeenCalled();
    expect(current.client.close).not.toHaveBeenCalled();
    expect(current.nativeConnection.current).toBe(current.client);
    current.closePicker();
  });

  it("fails the full startup before room creation when App retires during cleanup", async () => {
    const current = fixture();
    current.context.phase = "idle";
    current.activeGenerationRef.current = null;
    current.nativeShareGenerationRef.current = null;
    current.routePolicyRef.current.topologyOptimization = true;
    const cleanup = deferred<void>();
    current.nativeShareCleanupRef.current = cleanup.promise;
    const starting = current.start();
    await Promise.resolve();
    expect(current.client.startShare).not.toHaveBeenCalled();
    current.nativeConnection.release();
    cleanup.resolve();
    await starting;
    expect(current.client.startShare).not.toHaveBeenCalled();
    expect(current.createRoom).not.toHaveBeenCalled();
    expect(current.setPhase).toHaveBeenLastCalledWith("error");
    expect(current.setNoticeError).toHaveBeenCalledOnce();
    expect(current.activeGenerationRef.current).toBeNull();
    expect(current.roomMutationRef.current).toBeNull();
  });

  it("retires discovery after picker cancellation and allows a later attempt", async () => {
    const current = fixture(true, false);
    current.context.phase = "idle";
    current.nativeShareGenerationRef.current = null;
    const discovery = deferred<typeof current.client>();
    current.NativeClient.connect.mockReturnValueOnce(discovery.promise);
    const opening = current.openPicker();
    await vi.waitFor(() => expect(current.NativeClient.connect).toHaveBeenCalledOnce());
    const signal = current.NativeClient.connect.mock.calls[0]![0]!.signal;
    expect(signal.aborted).toBe(false);
    current.closePicker();
    expect(signal.aborted).toBe(true);
    discovery.resolve(current.client);
    await opening;
    expect(current.client.close).toHaveBeenCalledOnce();
    expect(current.client.sources).not.toHaveBeenCalled();
    expect(current.nativeConnection.current).toBeNull();

    const replacement = { ...current.client, close: vi.fn() };
    current.NativeClient.connect.mockResolvedValue(replacement);
    await current.openPicker();
    expect(current.setNativeSources).toHaveBeenLastCalledWith(expect.objectContaining({ kind: "ready" }));
    current.closePicker();
    expect(replacement.close).toHaveBeenCalledOnce();
  });

  it.each(["probe", "ingress"])("does not commit Browser codec state after sharing ends during %s", async (stage) => {
    const current = fixture(false);
    current.activeGenerationRef.current = null;
    current.nativeShareGenerationRef.current = null;
    const probe = deferred<{ primary: string }>();
    const ingress = deferred<void>();
    Object.assign(current.track, { readyState: "live" });
    const captured = { getTracks: () => [current.track], getVideoTracks: () => [current.track] };
    const resolveStreamVideoCodec = vi.fn(() => probe.promise);
    const startBrowserNativeIngress = vi.fn(() => ingress.promise);
    const setResolvedVideoCodec = vi.fn();
    Object.assign(current.context, {
      phase: "idle", captureBrowserSource: async () => captured,
      HostAudio: class {
        audioAvailable = false;
        sourceStream = captured;
        start = async () => captured;
        dispose = vi.fn();
        setVolume = vi.fn();
      }, setStream: vi.fn(), watchCaptureEnd: vi.fn(),
      resolveStreamVideoCodec, startBrowserNativeIngress, setResolvedVideoCodec,
    });
    current.disposeResources.mockImplementation(() => {
      current.videoCodecRef.current = { primary: "vp8" };
      setResolvedVideoCodec(null);
    });
    const starting = current.startBrowser();
    await vi.waitFor(() => expect(resolveStreamVideoCodec).toHaveBeenCalledOnce());
    if (stage === "ingress") {
      probe.resolve({ primary: "h264" });
      await vi.waitFor(() => expect(startBrowserNativeIngress).toHaveBeenCalledOnce());
    }
    current.context.endSharing({ key: "host.notice.stopped" });
    setResolvedVideoCodec.mockClear();
    probe.resolve({ primary: "h264" });
    ingress.resolve();
    await starting;
    expect(current.videoCodecRef.current.primary).toBe("vp8");
    expect(setResolvedVideoCodec).not.toHaveBeenCalled();
    expect(startBrowserNativeIngress).toHaveBeenCalledTimes(stage === "ingress" ? 1 : 0);
    expect(current.track.stop).toHaveBeenCalled();
    expect(current.createRoom).not.toHaveBeenCalled();
    expect(current.roomMutationRef.current).toBeNull();
  });

  it("retires a cancelled start ACK before publishing audio, codec or connecting the bridge", async () => {
    const current = fixture();
    current.nativeShareGenerationRef.current = null;
    const started = deferred<{ audio: boolean; codec: "h264" }>();
    current.client.startShare.mockReturnValue(started.promise);
    const starting = current.startNative();
    await vi.waitFor(() => expect(current.client.startShare).toHaveBeenCalledOnce());
    current.activeGenerationRef.current = null;
    started.resolve({ audio: false, codec: "h264" });
    await expect(starting).resolves.toBeNull();
    expect(current.client.stopShare).toHaveBeenCalledWith("share");
    expect(current.nativeSourceAudioRef.current).toBeUndefined();
    expect(current.manualVideoCodecPreference).not.toHaveBeenCalled();
    expect(current.NativeMediaBridge).toHaveBeenCalledOnce();
    expect(current.bridge.start).not.toHaveBeenCalled();
    expect(current.bridge.dispose).toHaveBeenCalledOnce();
    expect(current.nativeMediaBridgeRef.current).toBeNull();
    expect(current.setNativeActive).not.toHaveBeenCalled();
  });

  it("ignores a stale listing failure after a newer picker owns the App", async () => {
    const current = fixture();
    current.context.phase = "idle";
    current.nativeShareGenerationRef.current = null;
    const listing = deferred<never>();
    current.client.sources.mockReturnValueOnce(listing.promise.then(() => { throw new Error("old listing failed"); }));
    const old = current.openPicker();
    await vi.waitFor(() => expect(current.client.sources).toHaveBeenCalledOnce());
    await current.openPicker();
    current.setNativeSources.mockClear();
    listing.resolve(undefined as never);
    await old;
    expect(current.client.close).not.toHaveBeenCalled();
    expect(current.nativeConnection.current).toBe(current.client);
    expect(current.setNativeSources).not.toHaveBeenCalled();
    current.closePicker();
  });

  it("retains control for Native media but releases unused Browser and ended-share control", () => {
    const current = fixture();
    current.releaseUnused();
    expect(current.client.close).not.toHaveBeenCalled();
    current.disposeNative("older-share");
    expect(current.client.stopShare).not.toHaveBeenCalled();
    current.disposeNative("share");
    expect(current.client.stopShare).toHaveBeenCalledWith("share");
    expect(current.client.close).toHaveBeenCalledOnce();

    const browser = fixture();
    browser.nativeShareGenerationRef.current = null;
    browser.roomMutationRef.current = {};
    browser.releaseUnused();
    expect(browser.client.close).not.toHaveBeenCalled();
    browser.context.finishRoomMutation(browser.roomMutationRef.current);
    expect(browser.client.close).toHaveBeenCalledOnce();
  });

  it.each([false, "reject"])("keeps applied Browser capture after optional ingress fails: %s", async (failure) => {
    const current = fixture();
    const optional = ingress();
    current.nativeMediaIngressRef.current = optional;
    optional.updateProfile.mockImplementation(async () => {
      expect(current.qualitySettingsRef.current).toEqual(lower);
      expect(current.signalRef.current.setHostQualitySettings).toHaveBeenCalledWith(lower);
      if (failure === "reject") throw new Error("ingress failed");
      return false;
    });
    await current.change(lower);
    expect(current.physical()).toEqual(lower);
    expect(current.qualitySettingsRef.current).toEqual(lower);
    expect(current.advancedQualityRef.current).toEqual(lower);
    expect(current.savePreferredQuality).toHaveBeenCalledWith(lower);
    expect(current.peer.updateCaptureProfile).toHaveBeenCalledWith(lower);
    expect(optional.dispose).toHaveBeenCalledOnce();
    expect(current.client.stopReceive).toHaveBeenCalledWith("share");
    expect(current.track.stop).not.toHaveBeenCalled();
    expect(current.setNoticeError).not.toHaveBeenCalled();
  });

  it("retains the committed settings when Browser capture constraints reject", async () => {
    const current = fixture();
    const optional = ingress();
    current.nativeMediaIngressRef.current = optional;
    current.applyCaptureProfile.mockRejectedValue(new Error("capture rejected"));
    await current.change(lower);
    expect(current.physical()).toEqual(original);
    expect(current.savePreferredQuality).not.toHaveBeenCalled();
    expect(current.qualitySettingsRef.current).toEqual(original);
    expect(current.advancedQualityRef.current).toEqual(original);
    expect(optional.updateProfile).not.toHaveBeenCalled();
    expect(current.signalRef.current.setHostQualitySettings).not.toHaveBeenCalled();
  });

  it.each(["peer", "sfu"])("shows partial %s settings application as a limitation and clears it after success", async (failure) => {
    const current = fixture();
    if (failure === "peer") current.peer.updateCaptureProfile.mockResolvedValueOnce(false);
    else current.route.updateProfile.mockResolvedValueOnce(false);
    await current.change(lower);
    expect(current.qualitySettingsRef.current).toEqual(lower);
    expect(current.track.stop).not.toHaveBeenCalled();
    expect(current.setNoticeValue).toHaveBeenLastCalledWith({
      kind: "text", text: "host.notice.partialApply", target: "operation", comic: "settings-failed", tone: "warn",
    });
    await current.change(original);
    expect(current.setNoticeValue).toHaveBeenLastCalledWith({
      kind: "text", text: "host.notice.qualitySet", target: "operation", comic: "hint-quality", tone: "live",
    });
  });

  it("keeps a committed Native source change with an SFU failure in recovery rather than success", async () => {
    const current = fixture();
    current.route.updateProfile.mockResolvedValueOnce(false);
    await current.switchSource();
    expect(current.client.replaceShareSource).toHaveBeenCalledOnce();
    expect(current.setPhase).not.toHaveBeenCalled();
    expect(current.track.stop).not.toHaveBeenCalled();
    expect(current.setNoticeValue).toHaveBeenLastCalledWith({
      kind: "text", text: "source-switch-result", target: "operation", comic: "connecting-sfu", tone: "warn",
    });
  });

  it("does not commit a capture completion after its share has ended", async () => {
    const current = fixture();
    const pending = deferred<void>();
    current.applyCaptureProfile.mockReturnValue(pending.promise);
    const changing = current.change(lower);
    current.activeGenerationRef.current = null;
    current.qualityChangeRef.current = null;
    pending.resolve();
    await changing;
    expect(current.qualitySettingsRef.current).toEqual(original);
    expect(current.savePreferredQuality).not.toHaveBeenCalled();
    expect(current.signalRef.current.setHostQualitySettings).not.toHaveBeenCalled();
  });

  it.each(["replacement", "ended"])("does not retire another ingress after an old update settles: %s", async (change) => {
    const current = fixture();
    const old = ingress(), replacement = ingress(), pending = deferred<boolean>();
    old.updateProfile.mockReturnValue(pending.promise);
    current.nativeMediaIngressRef.current = old;
    const changing = current.change(lower);
    await vi.waitFor(() => expect(old.updateProfile).toHaveBeenCalled());
    current.nativeMediaIngressRef.current = replacement;
    if (change === "ended") {
      current.activeGenerationRef.current = null;
      current.qualityChangeRef.current = null;
    }
    pending.resolve(false);
    await changing;
    expect(old.dispose).not.toHaveBeenCalled();
    expect(replacement.dispose).not.toHaveBeenCalled();
    expect(current.nativeMediaIngressRef.current).toBe(replacement);
    if (change === "ended") expect(current.peer.updateCaptureProfile).not.toHaveBeenCalled();
  });

  it.each([false, true])("preserves an active draft through reauthentication and commits its queued result: %s", async (queued) => {
    const current = fixture();
    const pending = deferred<void>();
    current.applyCaptureProfile.mockReturnValueOnce(pending.promise);
    const changing = current.change(lower);
    const latest = queued ? QUALITY_PROFILES["1080p60"] : lower;
    if (queued) await current.change(latest);
    current.reauthenticate();
    expect(current.qualitySettingsRef.current).toEqual(original);
    expect(current.advancedQualityRef.current).toEqual(latest);
    pending.resolve();
    await changing;
    await vi.waitFor(() => expect(current.qualitySettingsRef.current).toEqual(latest));
    expect(current.advancedQualityRef.current).toEqual(latest);
    expect(current.pendingQualityChangeRef.current).toBeNull();
  });

  it("does not overwrite a new notice after a stale Native source-switch SFU update", async () => {
    const current = fixture();
    const pending = deferred<boolean>();
    current.route.updateProfile.mockReturnValue(pending.promise);
    const switching = current.switchSource();
    await vi.waitFor(() => expect(current.route.updateProfile).toHaveBeenCalled());
    current.activeGenerationRef.current = 2;
    current.sourceSwitchRef.current = null;
    current.context.setNotice("new-share", "share-live", "live");
    pending.resolve(true);
    await switching;
    expect(current.setNoticeValue).toHaveBeenLastCalledWith({
      kind: "text", text: "new-share", target: "operation", comic: "share-live", tone: "live",
    });
    expect(current.setNoticeValue).not.toHaveBeenCalledWith(expect.objectContaining({ text: "source-switch-result" }));
  });

  it("uses the current committed profile when reauthentication SFU recovery finishes late", async () => {
    const current = fixture();
    const pending = deferred<void>();
    current.route.resyncAuthoritative.mockReturnValue(pending.promise);
    current.reauthenticate();
    await current.change(lower);
    current.route.updateProfile.mockClear();
    pending.resolve();
    await vi.waitFor(() => expect(current.route.updateProfile).toHaveBeenCalledOnce());
    expect(current.route.updateProfile).toHaveBeenCalledWith(lower);
  });
});
