import { describe, expect, it, vi } from "vitest";
import { HostAudio } from "../../src/client/media/host-audio";
import { NativeSenderPeer } from "../../src/client/native/native-sender-peer";
import { HostPeer } from "../../src/client/webrtc/host-peer";
import { NativeRequestError, type NativeClient } from "../../src/client/native/client";
import { NativeMediaBridgeError } from "../../src/client/native/media-bridge";
import { fixture, original, ref, deferred } from "./support.ts";

describe("Camera replacement ownership", () => {
  function sourceFixture(native: boolean) {
    const current = fixture();
    class Track extends EventTarget {
      readyState = "live";
      enabled = true;
      stop = vi.fn(() => { this.readyState = "ended"; });
      getSettings = () => ({ width: 1280, height: 720 });
    }
    const picture = (sound = false) => {
      const video = new Track();
      const audio = sound ? new Track() : null;
      return { video, getTracks: () => audio ? [video, audio] : [video], getVideoTracks: () => [video], getAudioTracks: () => audio ? [audio] : [] };
    };
    const previous = picture(), camera = picture(), nativeStream = picture(true);
    const oldBridge = { stream: previous, dispose: vi.fn(() => previous.video.stop()) };
    const peer = Object.assign(Object.create(HostPeer.prototype), { replaceStream: vi.fn(async () => true), dispose: vi.fn() });
    const nativePeer = Object.assign(Object.create(NativeSenderPeer.prototype), { dispose: vi.fn() });
    const startedPeerStreams: unknown[] = [];
    const replacePublication = vi.fn(async () => true);
    current.peersRef.current.clear();
    current.peersRef.current.set("viewer", native ? nativePeer : peer as never);
    current.removePeer.mockImplementation(id => {
      const retired = current.peersRef.current.get(id) as unknown as { dispose?: () => void };
      current.peersRef.current.delete(id);
      retired?.dispose?.();
    });
    current.startPeer.mockImplementation(async id => {
      startedPeerStreams.push(current.context.streamRef.current);
      current.peersRef.current.set(id, peer as never);
    });
    Object.assign(current.context, {
      HostAudio, streamRef: ref(previous), retiringStreamRef: ref(null), sharingPausedRef: ref(false),
      hostAudioRef: ref(native ? null : new HostAudio(previous as unknown as MediaStream, "camera")),
      nativeMediaBridgeRef: ref(native ? oldBridge : null), setCommentary: vi.fn(), setCameraDevice: vi.fn(),
      setStream: vi.fn(), setResolvedVideoCodec: vi.fn(), setMediaPaused: vi.fn(),
      captureBrowserSource: vi.fn(async () => camera), endSharing: vi.fn(),
      createOpaqueId: () => "native-source-2", nativeSources: { kind: "ready" },
      hostSfuRouteRef: ref({ replaceStream: replacePublication }),
      iceConfigRef: ref({ iceServers: [] }), hostChildIsAssigned: () => true,
      acceptHostPeerUpdate: vi.fn(), reportHostChildFailure: vi.fn(),
    });
    const nativeControl = {
      prepareEdge: vi.fn(async () => ({ type: "offer", sdp: "native encoded offer" })),
      closeEdge: vi.fn(async () => {}),
      setRemoteInputPeers: vi.fn(async () => {
        current.client.onEvent.mock.calls[0]![0](remoteSnapshot);
      }),
    };
    const remoteSnapshot = { version: 10, type: "remote-control-state", shareId: "native-source-2", mode: "ask", platform: "windows", requests: [], coPlay: false };
    Object.assign(current.client, nativeControl);
    current.signalRef.current.send.mockReturnValue(true);
    current.nativeModeRef.current = native;
    current.context.nativeActive = native;
    current.nativeShareGenerationRef.current = native ? "native-source-1" : null;
    current.nativeSourcePathRef.current = { adapterIndex: 0, encoderIndex: 0 };
    current.manualVideoCodecPreference.mockImplementation(codec => ({ primary: codec }));
    current.bridge.start.mockResolvedValue(nativeStream);
    Object.assign(current.bridge, { stream: nativeStream });
    return { ...current, nativeControl, remoteSnapshot, picture, previous, camera, nativeStream, oldBridge, peer, nativePeer, startedPeerStreams, replacePublication };
  }

  it.each(["initial", "replacement"])("rejects Browser capture that ends during %s audio preparation", async entry => {
    const current = sourceFixture(entry === "replacement");
    const pending = deferred<void>();
    const outputAudio = { readyState: "live", stop: vi.fn() };
    const resume = vi.fn(() => pending.promise), close = vi.fn(async () => {});
    const resolveStreamVideoCodec = vi.fn(async () => ({ primary: "h264" }));
    Object.assign(current.context, { resolveStreamVideoCodec, startBrowserNativeIngress: vi.fn(async () => {}) });
    if (entry === "initial") {
      current.context.phase = "idle";
      current.activeGenerationRef.current = null;
      current.nativeShareGenerationRef.current = null;
      current.context.streamRef.current = null;
      current.context.hostAudioRef.current = null;
    }
    vi.stubGlobal("AudioContext", class {
      resume = resume;
      close = close;
      createMediaStreamDestination() { return { stream: { getAudioTracks: () => [outputAudio], getTracks: () => [outputAudio] } }; }
      createGain() { return { gain: { value: 1 }, connect() {}, disconnect() {} }; }
    });
    vi.stubGlobal("MediaStream", class {
      constructor(private tracks: unknown[]) {}
      getTracks() { return this.tracks; }
      getVideoTracks() { return this.tracks.filter(track => track === current.camera.video); }
      getAudioTracks() { return this.tracks.filter(track => track === outputAudio); }
    });
    try {
      const starting = entry === "initial" ? current.startBrowser() : current.context.switchSource("browser");
      await vi.waitFor(() => expect(resume).toHaveBeenCalledOnce());
      expect(current.oldBridge.dispose).not.toHaveBeenCalled();
      current.camera.video.readyState = "ended";
      current.camera.video.dispatchEvent(new Event("ended"));
      pending.resolve();
      await starting;
      expect(current.context.streamRef.current).toBe(entry === "initial" ? null : current.previous);
      expect(current.oldBridge.dispose).not.toHaveBeenCalled();
      expect(current.client.stopShare).not.toHaveBeenCalled();
      expect(current.previous.video.stop).not.toHaveBeenCalled();
      expect(current.camera.video.stop).toHaveBeenCalled();
      expect(outputAudio.stop).toHaveBeenCalledOnce();
      expect(close).toHaveBeenCalledOnce();
      expect(resolveStreamVideoCodec).not.toHaveBeenCalled();
      expect(current.createRoom).not.toHaveBeenCalled();
      expect(current.replacePublication).not.toHaveBeenCalled();
      expect(current.sourceSwitchRef.current).toBeNull();
      expect(current.setNoticeValue).toHaveBeenLastCalledWith(expect.objectContaining({
        target: entry === "initial" ? "television" : "operation", tone: "bad", key: "host.capture.readFailed",
      }));
    } finally { vi.unstubAllGlobals(); }
  });

  it.each(["NotAllowedError", "NotReadableError"])("keeps native capture and its room when Camera fails with %s", async name => {
    const current = sourceFixture(true);
    const permission = deferred<never>();
    current.context.captureBrowserSource.mockReturnValue(permission.promise);
    current.context.startBrowserShareFromPicker("camera", "front");
    expect(current.context.captureBrowserSource).toHaveBeenCalledExactlyOnceWith(original, "camera", "front");
    expect(current.client.stopShare).not.toHaveBeenCalled();
    permission.reject(new DOMException("camera rejected", name));
    await vi.waitFor(() => expect(current.sourceSwitchRef.current).toBeNull());
    expect(current.context.streamRef.current).toBe(current.previous);
    expect(current.nativeModeRef.current).toBe(true);
    expect(current.oldBridge.dispose).not.toHaveBeenCalled();
    expect(current.createRoom).not.toHaveBeenCalled();
    expect(current.context.endSharing).not.toHaveBeenCalled();
  });

  it("switches native → Camera → native through the picker handlers in one live room", async () => {
    const current = sourceFixture(true);
    const roomSignal = current.signalRef.current;
    current.context.startBrowserShareFromPicker("camera", "front");
    await vi.waitFor(() => expect(current.sourceSwitchRef.current).toBeNull());
    expect(current.context.streamRef.current).toBe(current.camera);
    expect(current.client.stopShare).toHaveBeenCalledExactlyOnceWith("native-source-1");
    expect(current.previous.video.stop).toHaveBeenCalled();
    expect(current.nativeModeRef.current).toBe(false);
    expect(current.nativePeer.dispose).toHaveBeenCalledOnce();
    expect(current.startedPeerStreams).toEqual([current.camera]);
    expect(current.replacePublication).toHaveBeenCalledWith(current.camera, true);
    expect(current.context.hostAudioRef.current).toBeInstanceOf(HostAudio);
    expect(current.context.endSharing).not.toHaveBeenCalled();

    // Revisiting the picker reacquires its control owner; the Browser camera
    // remains the publication until App ACK and the required bridge both finish.
    current.nativeConnection.adopt(current.client as unknown as NativeClient);
    current.context.startPeer = current.startChild;
    Object.assign(current.client.health.nativeMedia, { remoteInput: true });
    current.setRemoteControlState.mockClear();
    current.nativeSourcePathRef.current = { adapterIndex: 0, encoderIndex: 0 };
    const ack = deferred<{ audio: boolean; codec: "h264" }>();
    const bridge = deferred<typeof current.nativeStream>();
    current.client.startShare.mockReturnValue(ack.promise);
    current.bridge.start.mockReturnValue(bridge.promise);
    current.context.startNativeShareFromPicker({ kind: "picker", sourceId: "1", title: "System" }, true, false);
    await vi.waitFor(() => expect(current.client.startShare).toHaveBeenCalledOnce());
    expect(current.context.streamRef.current).toBe(current.camera);
    ack.resolve({ audio: true, codec: "h264" });
    await vi.waitFor(() => expect(current.bridge.start).toHaveBeenCalledOnce());
    expect(current.context.streamRef.current).toBe(current.camera);
    expect(current.nativeModeRef.current).toBe(false);
    expect(current.peersRef.current.get("viewer")).toBe(current.peer);
    expect(current.peer.dispose).not.toHaveBeenCalled();
    expect(current.setRemoteControlState).not.toHaveBeenCalledWith(current.remoteSnapshot);
    expect(current.camera.video.stop).not.toHaveBeenCalled();
    bridge.resolve(current.nativeStream);
    await vi.waitFor(() => expect(current.sourceSwitchRef.current).toBeNull());
    expect(current.context.streamRef.current).toBe(current.nativeStream);
    expect(current.nativeModeRef.current).toBe(true);
    expect(current.setRemoteControlState).toHaveBeenCalledWith(current.remoteSnapshot);
    expect(current.peersRef.current.get("viewer")).toBeInstanceOf(NativeSenderPeer);
    expect(current.peer.dispose).toHaveBeenCalledOnce();
    expect(current.peer.replaceStream).not.toHaveBeenCalledWith(current.nativeStream);
    expect(current.nativeControl.prepareEdge).toHaveBeenCalledWith(
      "native-source-2", expect.any(String), { iceServers: [] }, undefined, false,
    );
    expect(current.replacePublication).toHaveBeenLastCalledWith(current.nativeStream, true);
    expect(current.audioAvailableRef.current).toBe(true);
    expect(current.nativeShareGenerationRef.current).toBe("native-source-2");
    expect(current.shareGenerationRef.current).toBe("share");
    expect(current.activeGenerationRef.current).toBe(1);
    expect(current.signalRef.current).toBe(roomSignal);
    expect(current.camera.video.stop).toHaveBeenCalled();
    expect(current.createRoom).not.toHaveBeenCalled();
    expect(current.disposeResources).not.toHaveBeenCalled();
    expect(current.context.endSharing).not.toHaveBeenCalled();
    const nativeEvents = current.client.onEvent.mock.calls[0]![0];
    nativeEvents({ type: "share-ended", shareId: "native-source-1", failed: true });
    expect(current.context.endSharing).not.toHaveBeenCalled();
    nativeEvents({ type: "share-ended", shareId: "native-source-2", failed: true });
    expect(current.context.endSharing).toHaveBeenCalledOnce();
  });

  it.each(["permission", "bridge"])("retains Camera picture, audio and codec after native %s fails", async stage => {
    const current = sourceFixture(false);
    const previousCodec = current.videoCodecRef.current;
    current.audioAvailableRef.current = true;
    Object.assign(current.client.health.nativeMedia, { remoteInput: true });
    const failure = new NativeRequestError("rejected", "start-share");
    if (stage === "permission") current.client.startShare.mockRejectedValue(failure);
    else current.bridge.start.mockImplementation(async () => {
      // Exercise the real bridge-failure callback as well as its rejected wait.
      current.NativeMediaBridge.mock.calls.at(-1)![2]();
      throw new NativeMediaBridgeError("local preview failed");
    });
    await current.context.switchToNativeSource({ kind: "native", client: current.client,
      target: { kind: "picker", sourceId: "1", title: "System" }, audio: false, path: {} });
    expect(current.context.streamRef.current).toBe(current.previous);
    expect(current.previous.video.stop).not.toHaveBeenCalled();
    expect(current.audioAvailableRef.current).toBe(true);
    expect(current.videoCodecRef.current).toBe(previousCodec);
    expect(current.nativeModeRef.current).toBe(false);
    expect(current.context.hostAudioRef.current).toBeInstanceOf(HostAudio);
    expect(current.context.endSharing).not.toHaveBeenCalled();
    expect(current.createRoom).not.toHaveBeenCalled();
    expect(current.peer.replaceStream).not.toHaveBeenCalled();
    expect(current.peersRef.current.get("viewer")).toBe(current.peer);
    expect(current.peer.dispose).not.toHaveBeenCalled();
    expect(current.setRemoteControlState).not.toHaveBeenCalledWith(current.remoteSnapshot);
    expect(current.sourceSwitchRef.current).toBeNull();
    expect(current.setNoticeValue).toHaveBeenLastCalledWith(expect.objectContaining({ target: "operation" }));
  });

  it.each([true, false])("does not apply a retired ingress replacement to a new share: %s", async result => {
    const current = fixture();
    const pending = deferred<boolean>();
    const oldTrack = { stop: vi.fn() }, capturedTrack = { stop: vi.fn() };
    const previous = { getTracks: () => [oldTrack], getVideoTracks: () => [oldTrack] };
    const captured = { getTracks: () => [capturedTrack], getVideoTracks: () => [capturedTrack], getAudioTracks: () => [] };
    const optional = { hasAudio: false, replaceStream: vi.fn(() => pending.promise) };
    const token = {};
    Object.assign(current.context, {
      streamRef: ref(previous), nativeMediaIngressRef: ref(optional), sourceSwitchRef: ref(token),
      retiringStreamRef: ref(null), sharingPausedRef: ref(false),
      setMediaPaused: vi.fn(), setStream: vi.fn(), watchCaptureEnd: vi.fn(),
    });
    const replacing = current.context.replaceBrowserStream(captured, 1, token);
    expect(optional.replaceStream).toHaveBeenCalledWith(captured);
    const replacementPeer = { replaceStream: vi.fn(async () => true) };
    const replacementRoute = { replaceStream: vi.fn(async () => true) };
    current.activeGenerationRef.current = 2;
    current.context.sourceSwitchRef.current = {};
    current.context.nativeMediaIngressRef.current = null;
    current.peersRef.current.clear();
    current.peersRef.current.set("replacement", replacementPeer as never);
    current.context.hostSfuRouteRef.current = replacementRoute;
    pending.resolve(result);
    await replacing;
    expect(replacementPeer.replaceStream).not.toHaveBeenCalled();
    expect(replacementRoute.replaceStream).not.toHaveBeenCalled();
    expect(capturedTrack.stop).toHaveBeenCalledOnce();
    expect(oldTrack.stop).toHaveBeenCalledOnce();
  });

  it("keeps a screen Stop authoritative while a camera picker is pending", async () => {
    const current = fixture();
    let ended!: () => void;
    const track = { readyState: "live", addEventListener: (_event: string, callback: () => void) => { ended = callback; } };
    const previous = { getVideoTracks: () => [track] };
    const pending = deferred<unknown>();
    const endSharing = vi.fn(() => { current.context.sourceSwitchRef.current = null; });
    Object.assign(current.context, {
      streamRef: ref(previous), captureBrowserSource: () => pending.promise,
      closeCaptureSourcePicker: vi.fn(), hostAudioRef: ref({ sourceKind: "browser" }), endSharing,
    });
    current.context.watchCaptureEnd(previous, 1);
    const switching = current.context.switchSource("camera", "second");
    track.readyState = "ended";
    ended();
    expect(endSharing).toHaveBeenCalledOnce();
    const stop = vi.fn();
    pending.resolve({ getTracks: () => [{ stop }] });
    await switching;
    expect(stop).toHaveBeenCalledOnce();
  });

  it.each(["success", "failed-live", "failed-ended"])("owns old camera retirement while switching: %s", async outcome => {
    const current = fixture();
    let ended!: () => void;
    const track = { readyState: "live", addEventListener: (_event: string, callback: () => void) => { ended = callback; } };
    const previous = { getVideoTracks: () => [track] };
    const replacement = { getTracks: () => [], getVideoTracks: () => [{ readyState: "live" }] };
    const pending = deferred<unknown>();
    const endSharing = vi.fn(), replaceBrowserStream = vi.fn(async () => {}), setCaptureError = vi.fn();
    Object.assign(current.context, {
      streamRef: ref(previous), captureBrowserSource: () => pending.promise, closeCaptureSourcePicker: vi.fn(),
      hostAudioRef: ref({ sourceKind: "camera", attach: (stream: unknown) => stream }),
      endSharing, replaceBrowserStream, setCaptureError, setCameraDevice: vi.fn(),
    });
    current.context.watchCaptureEnd(previous, 1);
    const switching = current.context.switchSource("camera", "second");
    if (outcome !== "failed-live") { track.readyState = "ended"; ended(); }
    expect(endSharing).not.toHaveBeenCalled();
    if (outcome === "success") pending.resolve(replacement);
    else pending.reject(new Error("camera unavailable"));
    await switching;
    expect(replaceBrowserStream).toHaveBeenCalledTimes(outcome === "success" ? 1 : 0);
    expect(endSharing).toHaveBeenCalledTimes(outcome === "failed-ended" ? 1 : 0);
    expect(setCaptureError).toHaveBeenCalledTimes(outcome === "failed-live" ? 1 : 0);
  });
});
