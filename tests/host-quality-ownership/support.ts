import { createContext, runInContext } from "node:vm";
import { vi } from "vitest";
import { qualitySettingsEqual, qualitySettingsLabel, qualityResolutionEqual, screenAudioBitrate, videoQualitySettingsEqual, supportsCaptureProfile, videoCaptureLimits, MAX_AUDIO_SEND_BITRATE, type QualitySettings } from "../../src/client/media/quality";
import { NativeSenderPeer } from "../../src/client/native/native-sender-peer";
import { NativeCompatibilityError, NativeRequestError, type NativeClient } from "../../src/client/native/client";
import { NativeClientConnection } from "../../src/client/native/client-connection";
import { nativeCaptureTargetKey } from "../../src/client/native/capture-selection";
import { nativeAppDetected } from "../../src/client/ui/components/share/CaptureSourcePicker";
import { NativeMediaBridgeError } from "../../src/client/native/media-bridge";
import { InputRoute } from "../../src/client/remote-control/input-route";
import { reconcileBoundedMediaChildren } from "../../src/client/webrtc/media-assignment";
import { debugError, debugEvent, debugOperation } from "../../src/client/lib/debug";
import { ApiError } from "../../src/client/lib/api";
import { hostActionErrorKey, hostServerErrorKey, hostFailureCode, isCapturePermissionFailure } from "../../src/client/pages/host/notices";
import { say } from "../../src/client/ui/copy";
import { executable, original, lower, ref, deferred, noticeTextSource, ingress } from "./harness.ts";

export { executable, original, lower, ref, deferred, noticeTextSource, ingress };

export function fixture(launchedByClient = true, nativeConnected = true) {
  let physical: QualitySettings = original;
  const track = { stop: vi.fn() };
  const stream = { getTracks: () => [track] };
  const bridge = { start: vi.fn(async (_audio: boolean) => stream), dispose: vi.fn(), getRemoteInput: () => null };
  const peer = { updateCaptureProfile: vi.fn(async () => true), updateProfile: vi.fn(async () => true),
    isConnected: vi.fn(() => true) };
  const client = { updateShare: vi.fn(async () => undefined), replaceShareSource: vi.fn(async () => undefined),
    startShare: vi.fn(async (): Promise<{ audio: boolean; codec: "h264" }> => ({ audio: false, codec: "h264" })),
    onEvent: vi.fn((_listener: (event: unknown) => void) => () => undefined),
    close: vi.fn(), onClose: vi.fn((_listener: () => void) => () => undefined),
    health: { nativeMedia: { video: true, hardwareH264: true, softwareVP8: true } },
    captureOptions: vi.fn(async () => []), sources: vi.fn(async () => []),
    stopReceive: vi.fn(async () => undefined), stopShare: vi.fn(async () => undefined) };
  const route = { updateProfile: vi.fn(async () => true), resyncAuthoritative: vi.fn(async (): Promise<void> => undefined) };
  let context: ReturnType<typeof createContext>;
  const state = {
    debugError, debugEvent, debugOperation, NativeCompatibilityError, NativeRequestError, NativeMediaBridgeError, isCapturePermissionFailure, hostFailureCode, hostActionErrorKey, hostServerErrorKey, DOMException, AbortController,
    launchedByClient, isClientActivated: vi.fn(() => launchedByClient),
    NativeClient: { connect: vi.fn(async (_options?: { signal: AbortSignal }): Promise<typeof client | null> => null) },
    setJoiningRoom: vi.fn(), startSharing: vi.fn(), openCaptureSourcePicker: vi.fn(),
    videoCodecRef: ref({ primary: "h264" }),
    videoCodecModeRef: ref("h264"), MAX_ENDPOINT_MEDIA_CHILDREN: 2,
    sourcePickerReturnRef: ref<{ id: string; restore: boolean } | null>(null),
    nativeSourceRequestRef: ref<object | null>(null), nativeSourcePathRef: ref<unknown>(null),
    setNativeSources: vi.fn(), setShowCaptureBorder: vi.fn(), defaultNativeCapturePath: () => ({ adapterIndex: 0, encoderIndex: 0 }),
    roomMutationRef: ref<object | null>(null), setRoomMutation: vi.fn(),
    generationRef: ref(0), shareGenerationRef: ref<string | null>("share"),
    serverShareGenerationRef: ref<string | null>("share"), createOpaqueId: () => "share",
    setCopiedRoomLink: vi.fn(), setPhase: vi.fn(), roomInitializationRef: ref(Promise.resolve()), roomRef: ref(null),

    createRoom: vi.fn(async () => { throw new Error("must not create an empty room"); }),
    readPreferredRoomId: () => null, disposeResources: vi.fn(), ApiError,
    NativeMediaBridge: vi.fn(function (_shareId: string, _client: unknown, _onFailed: () => void) { return bridge; }), manualVideoCodecPreference: vi.fn(),
    phase: "live", qualitySettingsRef: ref<QualitySettings>(original), advancedQualityRef: ref<QualitySettings>(original),
    nativeActive: false, switchingSource: false, sourceSwitchState: null, shareVolume: { level: 1, muted: false },
    window: { location: { assign: vi.fn() } },
    qualityChangeRef: ref<object | null>(null), pendingQualityChangeRef: ref<QualitySettings | null>(null),
    appQualityPreferenceRef: ref<{ save: (quality: QualitySettings) => void } | null>(null),
    qualityPreferenceEditedRef: ref(false), pendingAppQualityPreferenceRef: ref<QualitySettings | null>(null),
    activeGenerationRef: ref<number | null>(1), streamRef: ref<typeof stream | null>(stream), sourceSwitchRef: ref<object | null>(null),
    nativeSourceAudioRef: ref<boolean | undefined>(undefined), audioAvailableRef: ref(false), setMicrophoneEnabled: vi.fn(),
    nativeSourceSelectionRef: ref<Record<string, unknown> | null>(null), nativeCaptureTargetKey, nativeAppDetected,
    nativeModeRef: ref(false), nativeConnection: new NativeClientConnection(signal => context.connectNativeClient(signal),
      () => context.handleNativeClientClosed()), nativeShareGenerationRef: ref<string | null>("share"),
    hostAudioRef: ref<{ sourceStream: MediaStream } | null>(null),
    shareVolumeChangeRef: ref<object | null>(null), setShareVolume: vi.fn(),
    sharingPauseOperationRef: ref<object | null>(null),
    nativeMediaIngressRef: ref<ReturnType<typeof ingress> | null>(null), nativeMediaBridgeRef: ref(null),
    nativeInputRoute: new InputRoute(() => {}),
    nativeEventCleanupRef: ref(null), nativeShareCleanupRef: ref(Promise.resolve()),
    peersRef: ref(new Map([["viewer", peer]])), hostProvisionalChildRef: ref(null), hostSfuRouteRef: ref(route),
    signalRef: ref({ setHostQualitySettings: vi.fn(), setAudioAvailable: vi.fn(), setCoPlayGrants: vi.fn(), send: vi.fn() }),
    setQualitySettings: vi.fn(), setAdvancedQuality: vi.fn(), setChangingQuality: vi.fn(), savePreferredQuality: vi.fn(),
    setNoticeValue: vi.fn(), setNoticeError: vi.fn(),
    setDetails: vi.fn(), setNativeActive: vi.fn(), setRemoteControlState: vi.fn(),
    remoteInputPeersRef: ref([]),
    applyCaptureProfile: vi.fn(async (_stream: unknown, profile: QualitySettings) => { physical = profile; }),
    captureDetails: () => ({}),
    qualitySettingsEqual, qualityResolutionEqual, videoQualitySettingsEqual, screenAudioBitrate, qualitySettingsLabel,
    supportsCaptureProfile, videoLimits: videoCaptureLimits(), audioBitrateCeiling: MAX_AUDIO_SEND_BITRATE,
    say: (key: string) => key, syncHostSfuQualityWarning: () => null,
    NativeSenderPeer, discardPreparedHostChild: vi.fn(), removePeer: vi.fn(), startPeer: vi.fn(async (_peerId: string, _generation: number): Promise<void> => {}),
    nativePreviewTailRef: ref(Promise.resolve()), setSourceSwitch: vi.fn(),
    invalidateSenderQualityEvidence: vi.fn(), routePolicyRef: ref({ topologyOptimization: false }),
    sourceSwitchNotice: () => "source-switch-result", hostPeerIdRef: ref(null), endpointMediaCopyCapacityRef: ref(2),
    creationProfileRef: ref({ roomPassword: null }), saveCreationProfile: vi.fn(), setCreationProfile: vi.fn(),
    setViewerPasswordEnabled: vi.fn(), setViewerPasswordDraft: vi.fn(), setViewerPasswordVisible: vi.fn(),
    setRoutePolicy: vi.fn(), setRoom: vi.fn(), activeRouteRevisionRef: ref(1),
    ensureHostSfuRoute: () => route,
    reconcileBoundedMediaChildren, activeHostChildPeerIdsRef: ref<string[]>([]),
  };
  context = createContext({ ...state,
    isCurrentGeneration: (generation: number) => state.activeGenerationRef.current === generation,
    isCurrentShare: (generation: number, share: string) =>
      state.activeGenerationRef.current === generation && state.shareGenerationRef.current === share,
  });
  runInContext(executable, context);
  state.setNativeActive.mockImplementation((value: boolean) => { context.nativeActive = value; });
  state.setSourceSwitch.mockImplementation((value: unknown) => { context.sourceSwitchState = value; context.switchingSource = value !== null; });
  if (nativeConnected) state.nativeConnection.adopt(client as unknown as NativeClient);
  const start = context.startSharing;
  const startChild = context.startPeer;
  const openPicker = context.openCaptureSourcePicker;
  const dispose = context.disposeResources as (notifyServer: boolean, keepRoomSession?: boolean) => void;
  const writeNoticeError = context.setNoticeError;
  state.setNoticeError.mockImplementation(writeNoticeError);
  context.setNoticeError = state.setNoticeError;
  context.startSharing = state.startSharing;
  context.startPeer = state.startPeer;
  context.openCaptureSourcePicker = state.openCaptureSourcePicker;
  context.disposeResources = state.disposeResources;
  return { ...state, peer, client, route, track, bridge, physical: () => physical,
    context, startChild, dispose, openPicker: openPicker as () => Promise<void>,
    closePicker: context.closeCaptureSourcePicker as () => void,
    releaseUnused: context.releaseUnusedNativeClient as () => void,
    disposeNative: context.disposeNativeShare as (expectedShare?: string) => void,
    start: () => start( { kind: "native", client, target: {}, audio: false, path: {} }) as Promise<void>,
    startBrowser: () => start({ kind: "browser" }) as Promise<void>,
    writeNoticeError,
    renderNotice: (noticeValue: unknown) => runInContext(noticeTextSource(), createContext({ noticeValue, t: say })),
    discover: context.acquireNativeClient as () => Promise<typeof client | null>,
    requestShare: context.requestSharing as () => void,
    startNative: () => context.startNativeShare(1, "share", { client, target: {}, audio: false, path: {} }) as Promise<unknown>,
    startOptionalIngress: () => context.startBrowserNativeIngress(1, "share", stream) as Promise<void>,
    change: context.changeQuality as (profile: QualitySettings) => Promise<void>,
    reauthenticate: () => context.handleSignalMessage({ type: "authenticated", role: "host", qualitySettings: original,
      routePolicy: {}, routeRevision: 1, endpointMediaCopyCapacity: 2 }, 1, true, null),
    switchSource: () => context.switchNativeSource(client, {}, false, {}),
  };
}

export function useRealResourceDisposal(current: ReturnType<typeof fixture>): void {
  current.peersRef.current.clear();
  Object.assign(current.hostSfuRouteRef, { current: null });
  Object.assign(current.context, {
    codecProbeAbortRef: ref(null), VP8_ONLY_VIDEO_CODEC: { primary: "vp8" },
    setResolvedVideoCodec: vi.fn(), setHostSfuQualityWarning: vi.fn(),
    setInteractionSession: vi.fn(), browserVideoPoolRef: ref(null),
    setMicrophonePending: vi.fn(), retiringStreamRef: ref(null), iceConfigRef: ref(null),
    commentaryOperationRef: ref(null), setCommentary: vi.fn(), setCommentaryBusy: vi.fn(), setCommentaryInput: vi.fn(),
    setStream: vi.fn(), setPeerSnapshots: vi.fn(), setParticipantPresence: vi.fn(),
    viewerQualityEvidenceStore: { clear: vi.fn(), getSnapshot: () => new Map() },
    cancelViewerQualityEvidenceRender: vi.fn(), setViewerQualityEvidence: vi.fn(),
    setSignalStatus: vi.fn(), sharingPausedRef: ref(false), setSharingPaused: vi.fn(),
  });
  current.disposeResources.mockImplementation(current.dispose);
}
