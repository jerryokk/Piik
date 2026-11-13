//! Viewer session hook: owns the signaling connection, media route, relay,
//! native App control and server-message handling for one Viewer page mount.
//! The effect body and dependency array are the page's original session effect.

import {
  useEffect,
  useState,
  type Dispatch,
  type RefObject,
  type SetStateAction,
} from "react";
import {
  DEFAULT_QUALITY_SETTINGS,
  DEFAULT_ROUTE_POLICY,
  MAX_VIEWERS_PER_ROOM_LIMIT,
  type CoPlayParticipant,
  type IceConfig,
  type ParticipantPresenceEntry,
  type ParticipantRouteAssignment,
  type RoutePolicy,
  type ServerMessage,
  type ViewerRoutePreference,
} from "../../../shared/protocol";
import { getRuntimeCapabilities } from "../../lib/api";
import { createOpaqueId } from "../../lib/util/opaque-id";
import { readRoomPreferences } from "../../lib/room/preferences";
import { clearViewerGrant } from "../../lib/session";
import { SignalingClient } from "../../lib/signaling";
import { labelParticipantSnapshot } from "../../lib/room/viewer-presence";
import type { RoomInteractionSession } from "../../lib/room/interactions";
import { P2pQualityProbe } from "../../media/candidate-quality-probe";
import type { QualitySettings } from "../../media/quality";
import {
  relayCapacityMessageForBrowser,
  relayVolunteerLimit,
} from "../../media/relay-capability";
import { exactPeerSignalOwner } from "../../media/route-transition";
import {
  invalidateSenderQualityEvidence,
  senderQualityEvidenceFromSnapshot,
} from "../../media/sender-quality-evidence";
import { ViewerMessageAuthority } from "../../media/viewer-message-authority";
import type { ViewerPresentationState } from "../../media/viewer-presentation";
import {
  viewerFailureFromServerCode,
  type ViewerFailureCode,
} from "../../media/viewer-presentation";
import {
  presentViewerQualityEvidence,
  qualityEvidenceMatchesSnapshot,
  reconcileViewerQualityEvidencePresentation,
  type ViewerQualityEvidencePresentation,
  ViewerQualityEvidenceReporter,
} from "../../media/viewer-quality-evidence";
import { ViewerQualityEvidenceStore } from "../../media/viewer-quality-evidence-store";
import { ViewerSfuRoute } from "../../media/viewer-sfu-route";
import type { RemoteMediaBinding } from "../../media/viewer-playback";
import { NativeClient } from "../../native/client";
import { NativeClientConnection } from "../../native/client-connection";
import { NativeSenderPeer } from "../../native/native-sender-peer";
import { NativeCapableViewerPeer } from "../../native/native-viewer";
import { nativeRelayCapacity } from "../../native/wire";
import type { RoomVoiceSession } from "../../room-voice/session";
import type { InputRoute } from "../../remote-control/input-route";
import type { ConnectionMetrics, PeerSnapshot } from "../../types";
import type { CopyKey } from "../../ui/copy";
import {
  createOwnedViewerRestartSender,
  limitMediaAssignment,
  MAX_ENDPOINT_MEDIA_CHILDREN,
  viewerSignalMessage,
  type MediaAssignment,
} from "../../webrtc/media-assignment";
import {
  ViewerPeer,
  type ViewerMediaPeer,
  type ViewerPeerEvents,
  type ViewerPeerOptions,
} from "../../webrtc/viewer-peer";
import {
  ViewerRelay,
  type ViewerRelayPeerFactory,
} from "../../webrtc/viewer-relay";
import { connectionFact, type ViewerSessionHelpers } from "./session-helpers";
import type {
  PendingPeerRoute,
  SfuUpstreamState,
  ViewerAssignedRoute,
  ViewerQualityEvidence,
} from "./types";

type SessionCallback =
  | "acceptAssignedRoute"
  | "bindRemoteStream"
  | "clearHostPresence"
  | "clearParticipantPresence"
  | "clearRemoteMedia"
  | "dispatchPresentation"
  | "invalidatePresentedMedia"
  | "invalidateQualityPresentation"
  | "rearmCurrentFrameProof";

export interface ViewerSessionDeps
  extends Pick<ViewerSessionHelpers, SessionCallback> {
  roomId: string;
  viewerGrant: string | undefined;
  viewerPasswordAttempt: { password: string; sequence: number } | null;
  viewerClientId: string;
  clientActivationRef: RefObject<boolean>;
  displayNameRef: RefObject<string>;
  inputRoute: InputRoute;
  peerRef: RefObject<ViewerMediaPeer | null>;
  presentationStateRef: RefObject<ViewerPresentationState>;
  qualityEvidenceReporterRef: RefObject<ViewerQualityEvidenceReporter | null>;
  qualityFrameProofGenerationRef: RefObject<number | null>;
  refreshInput: Dispatch<SetStateAction<number>>;
  remoteMediaRef: RefObject<RemoteMediaBinding | null>;
  requestRelayVolunteerRef: RefObject<
    ((enabled: boolean, capacity?: number) => void) | null
  >;
  signalRef: RefObject<SignalingClient | null>;
  videoRef: RefObject<HTMLVideoElement | null>;
  viewerSfuRouteRef: RefObject<ViewerSfuRoute | null>;
  setAssignedRoute: Dispatch<SetStateAction<ViewerAssignedRoute | null>>;
  setAudioAvailable: Dispatch<SetStateAction<boolean | null>>;
  setCoPlayParticipants: Dispatch<SetStateAction<CoPlayParticipant[]>>;
  setInteractionSession: Dispatch<
    SetStateAction<RoomInteractionSession | null>
  >;
  setLastHostDisplayName: Dispatch<SetStateAction<string | null>>;
  setParticipantPresence: Dispatch<
    SetStateAction<ParticipantPresenceEntry[] | null>
  >;
  setPeerOnly: Dispatch<SetStateAction<boolean>>;
  setPeerSnapshot: Dispatch<SetStateAction<PeerSnapshot | null>>;
  setRelayVolunteerEnabled: Dispatch<SetStateAction<boolean>>;
  setRelayVolunteerLimits: Dispatch<
    SetStateAction<ReturnType<typeof relayVolunteerLimit>>
  >;
  setRelayVolunteerPending: Dispatch<SetStateAction<boolean>>;
  setRelayVolunteerSupported: Dispatch<SetStateAction<boolean>>;
  setRoomVoiceSession: Dispatch<SetStateAction<RoomVoiceSession | null>>;
  setRouteOptions: Dispatch<
    SetStateAction<{ sfu: boolean; sfuOnly: boolean } | null>
  >;
  setRoutePreference: Dispatch<SetStateAction<ViewerRoutePreference>>;
  setRoutePreferencePending: Dispatch<SetStateAction<boolean>>;
  setRoutePreferenceRejected: Dispatch<SetStateAction<boolean>>;
  setSelfPeerId: Dispatch<SetStateAction<string | null>>;
  setSfuUpstream: Dispatch<SetStateAction<SfuUpstreamState | null>>;
  setViewerPasswordDraft: Dispatch<SetStateAction<string>>;
  setViewerPasswordError: Dispatch<SetStateAction<CopyKey | null>>;
  setViewerPasswordExpanded: Dispatch<SetStateAction<boolean>>;
}

/** Relay state the page renders outside the session effect. */
export interface ViewerSessionState {
  relaySnapshot: PeerSnapshot | null;
  relayChildEvidence: ReadonlyMap<string, ViewerQualityEvidencePresentation>;
}

export function useViewerSession(deps: ViewerSessionDeps): ViewerSessionState {
  const {
    roomId,
    viewerGrant,
    viewerPasswordAttempt,
    viewerClientId,
    clientActivationRef,
    displayNameRef,
    inputRoute,
    peerRef,
    presentationStateRef,
    qualityEvidenceReporterRef,
    qualityFrameProofGenerationRef,
    refreshInput,
    remoteMediaRef,
    requestRelayVolunteerRef,
    signalRef,
    videoRef,
    viewerSfuRouteRef,
    setAssignedRoute,
    setAudioAvailable,
    setCoPlayParticipants,
    setInteractionSession,
    setLastHostDisplayName,
    setParticipantPresence,
    setPeerOnly,
    setPeerSnapshot,
    setRelayVolunteerEnabled,
    setRelayVolunteerLimits,
    setRelayVolunteerPending,
    setRelayVolunteerSupported,
    setRoomVoiceSession,
    setRouteOptions,
    setRoutePreference,
    setRoutePreferencePending,
    setRoutePreferenceRejected,
    setSelfPeerId,
    setSfuUpstream,
    setViewerPasswordDraft,
    setViewerPasswordError,
    setViewerPasswordExpanded,
    acceptAssignedRoute,
    bindRemoteStream,
    clearHostPresence,
    clearParticipantPresence,
    clearRemoteMedia,
    dispatchPresentation,
    invalidatePresentedMedia,
    invalidateQualityPresentation,
    rearmCurrentFrameProof,
  } = deps;
  const [relaySnapshot, setRelaySnapshot] = useState<PeerSnapshot | null>(null);
  const [relayChildEvidence, setRelayChildEvidence] = useState<
    ReadonlyMap<string, ViewerQualityEvidencePresentation>
  >(() => new Map());

  useEffect(() => {
    let active = true;
    let currentIceConfig: IceConfig | null = null;
    let currentHostPaused = false;
    let currentPeerId: string | null = null;
    let currentRouteRevision = 0;
    let currentRouteAssignment: ParticipantRouteAssignment | null = null;
    let currentRouteConnectionId: string | null = null;
    let activePeerMetrics: ConnectionMetrics | null = null;
    let pendingRouteConnection: {
      revision: number;
      connectionId: string;
    } | null = null;
    let endpointMediaCopyCapacity = MAX_ENDPOINT_MEDIA_CHILDREN;
    let relayBackendCapacity: number | null = null;
    let ordinaryRelayCapacity = MAX_ENDPOINT_MEDIA_CHILDREN;
    let requestedRelayCapacity: number | null = null;
    let relayVolunteerAvailable = false;
    let viewerAuthorizationGeneration: string | null = null;
    let currentQualitySettings: QualitySettings = DEFAULT_QUALITY_SETTINGS;
    let currentRoutePolicy: RoutePolicy = DEFAULT_ROUTE_POLICY;
    let currentShareGeneration: string | null = null;
    let currentAssignment: MediaAssignment = {
      parentPeerId: null,
      childPeerIds: [],
    };
    let viewerRelay: ViewerRelay | null = null;
    let viewerRelaySourceKey: string | null = null;
    let viewerSfuRoute: ViewerSfuRoute | null = null;
    let preparedParentPeerId: string | null = null;
    let preparedParentSignals: Array<
      Extract<ServerMessage, { type: "signal" }>
    > = [];
    let pendingPeer: PendingPeerRoute | null = null;
    const messageAuthority = new ViewerMessageAuthority();
    const relayChildEvidenceStore = new ViewerQualityEvidenceStore((values) => {
      if (active) setRelayChildEvidence(values);
    });
    setRelayChildEvidence(relayChildEvidenceStore.getSnapshot());
    let sfuTransportConnected = false;
    const nativeConnection = new NativeClientConnection(signal =>
      NativeClient.connect({ waitForPermission: false, signal }).catch(() => null));
    let nativeViewerAvailable = true;
    const nativeViewerSessionId = createOpaqueId();

    const releaseNativeClient = (): void => {
      nativeConnection.release();
    };

    const acquireNativeClient = async (): Promise<NativeClient | null> => {
      if (!active || !clientActivationRef.current || !nativeViewerAvailable) return null;
      const client = await nativeConnection.acquire();
      return active && nativeViewerAvailable && nativeConnection.current === client ? client : null;
    };

    function createViewerMediaPeer(
      iceConfig: IceConfig,
      events: ViewerPeerEvents,
      options: ViewerPeerOptions,
    ): ViewerMediaPeer {
      if (!clientActivationRef.current) {
        return new ViewerPeer(iceConfig, events, options);
      }
      return new NativeCapableViewerPeer(
        iceConfig,
        events,
        options,
        acquireNativeClient,
        () => { nativeViewerAvailable = false; },
        nativeViewerSessionId,
        MAX_VIEWERS_PER_ROOM_LIMIT,
      );
    }

    let pageSuspended = document.visibilityState !== "visible";
    const suspendForPageLifecycle = (): void => {
      // Losing observation does not invalidate media already proved playable.
      invalidateQualityPresentation();
      rearmCurrentFrameProof();
      const newlySuspended = !pageSuspended;
      pageSuspended = true;
      viewerSfuRoute?.resetQualityProbe();
      pendingPeer?.qualityProbe?.reset();
      if (pendingPeer) pendingPeer.qualityResult = "pending";
      if (newlySuspended) {
        invalidateSenderQualityEvidence();
        if (currentRoutePolicy.topologyOptimization) {
          signal.send({ type: "reset-sender-quality" });
        }
      }
    };
    const recoverFromPageLifecycle = (): void => {
      invalidateQualityPresentation();
      rearmCurrentFrameProof();
      pageSuspended = document.visibilityState !== "visible";
    };
    const handleVisibilityChange = (): void => {
      if (document.visibilityState === "visible") {
        recoverFromPageLifecycle();
      } else {
        suspendForPageLifecycle();
      }
    };
    document.addEventListener("freeze", suspendForPageLifecycle);
    document.addEventListener("resume", recoverFromPageLifecycle);
    document.addEventListener("visibilitychange", handleVisibilityChange);
    window.addEventListener("pagehide", suspendForPageLifecycle);
    window.addEventListener("pageshow", recoverFromPageLifecycle);

    const signal = new SignalingClient(
      {
        roomId,
        role: "viewer",
        clientId: viewerClientId,
        ...(viewerGrant ? { viewerGrant } : {}),
        ...(!viewerGrant && viewerPasswordAttempt
          ? { viewerPassword: viewerPasswordAttempt.password }
          : {}),
        displayName: displayNameRef.current,
        routePreference: readRoomPreferences().routePreference,
      },
      {
        onStatus: (status) => {
          if (active) {
            if (status !== "connected") setCoPlayParticipants([]);
            dispatchPresentation({ type: "signal", signal: status });
          }
        },
        onTerminated: (reason) => {
          if (!active) {
            return;
          }
          denyRoomAccess(reason);
        },
        onAccessRequired: () => {
          if (active) {
            denyRoomAccess("INVALID_TOKEN");
          }
        },
        onMessage: (message) => {
          if (!active) {
            return;
          }
          void handleMessage(message, messageAuthority.tokenFor(message));
        },
      },
    );
    setInteractionSession(null);
    setRouteOptions(null);
    setRelayVolunteerSupported(false);
    setRelayVolunteerEnabled(false);
    setRelayVolunteerPending(false);
    function updateRelayVolunteer(enabled = signal.relayVolunteerRequest?.enabled ?? false, capacity?: number, send = true): void {
      if (capacity !== undefined) requestedRelayCapacity = capacity;
      // Keep consent in the signaling owner before asynchronous App discovery.
      // Until its backend is known, an App gets only the legacy physical bound.
      const backend = relayBackendCapacity ?? (clientActivationRef.current ? MAX_ENDPOINT_MEDIA_CHILDREN : MAX_VIEWERS_PER_ROOM_LIMIT);
      const limits = relayVolunteerLimit(requestedRelayCapacity, ordinaryRelayCapacity, backend);
      setRelayVolunteerLimits({ ...limits, available: relayBackendCapacity === null || limits.available });
      if (!send || !relayVolunteerAvailable) return;
      const effective = enabled ? limits.capacity : backend;
      const intent = signal.relayVolunteerRequest;
      if (intent?.enabled === enabled && intent.capacity === effective) return;
      setRelayVolunteerPending(true);
      if (!signal.setRelayVolunteer(enabled, effective)) setRelayVolunteerPending(false);
    }
    requestRelayVolunteerRef.current = (enabled, capacity) => {
      updateRelayVolunteer(enabled, capacity);
      void acquireNativeClient().then(client => {
        if (!active || relayBackendCapacity !== null) return;
        relayBackendCapacity = client
          ? nativeRelayCapacity(MAX_VIEWERS_PER_ROOM_LIMIT, client.health) : MAX_VIEWERS_PER_ROOM_LIMIT;
        updateRelayVolunteer();
      });
    };
    setAudioAvailable(null);
    setInteractionSession(signal.interactions);
    setRoomVoiceSession(signal.roomVoice);
    inputRoute.enable(identity => signal.send({ type: "subscribe-remote-input", ...identity }));
    void getRuntimeCapabilities().then(capabilities => {
      if (!active) return;
      relayVolunteerAvailable = !capabilities.sfuOnly;
      setRelayVolunteerSupported(relayVolunteerAvailable);
      updateRelayVolunteer(undefined, undefined, relayBackendCapacity !== null);
      setRouteOptions({ sfu: capabilities.sfu, sfuOnly: capabilities.sfuOnly === true });
    }).catch(() => { /* Route availability never blocks viewing. */ });
    signalRef.current = signal;
    const qualityEvidenceReporter = new ViewerQualityEvidenceReporter(
      (message) => active && signal.send(message),
    );
    qualityEvidenceReporterRef.current = qualityEvidenceReporter;
    function qualityPresentationEligible(
      connectionConnected: boolean,
    ): boolean {
      const state = presentationStateRef.current;
      const binding = remoteMediaRef.current;
      const video = videoRef.current;
      return Boolean(
        connectionConnected &&
          !pageSuspended &&
          document.visibilityState === "visible" &&
          !currentHostPaused &&
          currentRouteAssignment !== null &&
          currentRouteAssignment.upstream.kind !== "none" &&
          currentRouteConnectionId !== null &&
          binding &&
          qualityFrameProofGenerationRef.current === binding.generation &&
          state.routeStatus?.state !== "failed" &&
          video &&
          !video.paused &&
          !video.ended,
      );
    }
    function committedTransportConnected(): boolean {
      if (currentRouteAssignment?.upstream.kind === "peer") {
        return (
          peerRef.current?.isConnected() === true &&
          peerRef.current.isRecovering() === false
        );
      }
      return currentRouteAssignment?.upstream.kind === "sfu"
        ? sfuTransportConnected
        : false;
    }
    function offerPeerQualityEvidence(
      snapshot: PeerSnapshot,
      peer: ViewerMediaPeer,
    ): void {
      const connectionHealthy =
        snapshot.connectionState === "connected" && !peer.isRecovering();
      const qualityEligible =
        qualityPresentationEligible(connectionHealthy);
      if (!connectionHealthy) {
        invalidatePresentedMedia();
        return;
      }
      if (!qualityEligible) {
        invalidateQualityPresentation();
      }
      qualityEvidenceReporter.offer(
        snapshot,
        currentRouteRevision,
        qualityEligible,
      );
    }
    function activateRouteIdentity(
      revision: number,
      assignment: ParticipantRouteAssignment,
      connectionId: string | null,
    ): void {
      currentRouteRevision = revision;
      currentRouteAssignment = assignment;
      currentRouteConnectionId =
        assignment.upstream.kind === "none" ? null : connectionId;
      if (assignment.upstream.kind !== "peer") {
        activePeerMetrics = null;
      }
      pendingRouteConnection = null;
    }

    function acceptRelayChildEvidence(evidence: ViewerQualityEvidence): void {
      const relaySnapshot =
        viewerRelay?.getSnapshot(evidence.viewerPeerId) ?? null;
      if (
        evidence.upstream.kind !== "peer" ||
        evidence.upstream.peerId !== currentPeerId ||
        evidence.guard.routeRevision !== currentRouteRevision ||
        !qualityEvidenceMatchesSnapshot(evidence, relaySnapshot)
      ) {
        return;
      }
      relayChildEvidenceStore.set(
        evidence.viewerPeerId,
        presentViewerQualityEvidence(
          relayChildEvidenceStore.getSnapshot().get(evidence.viewerPeerId) ?? null,
          evidence,
        ),
      );
    }

    function ensureViewerRelay(
      upstreamKind = currentRouteAssignment?.upstream.kind,
    ): ViewerRelay | null {
      if (!currentIceConfig) {
        return null;
      }
      const source =
        upstreamKind === "peer" &&
        peerRef.current instanceof NativeCapableViewerPeer
          ? peerRef.current.nativeSource
          : null;
      // Wait until the upstream media identity is known. This lets a native
      // Viewer choose the encoded relay source before any child edge exists.
      if (!remoteMediaRef.current?.stream) {
        return null;
      }
      const capacity = source
        ? nativeRelayCapacity(MAX_VIEWERS_PER_ROOM_LIMIT, source.client.health) : MAX_VIEWERS_PER_ROOM_LIMIT;
      if (relayBackendCapacity !== capacity) {
        relayBackendCapacity = capacity;
        updateRelayVolunteer();
      }
      const sourceKey = source
        ? `native:${source.sessionId}:${source.connectionId}:${source.generation}`
        : "browser";
      if (viewerRelay && viewerRelaySourceKey === sourceKey) {
        return viewerRelay;
      }
      if (viewerRelay) {
        viewerRelay.dispose();
        viewerRelay = null;
        viewerRelaySourceKey = null;
      }
      const peerFactory: ViewerRelayPeerFactory | null = source
        ? {
            requiresStream: false,
            create: (childPeerId, connectionId, peerEvents, candidate) =>
              candidate?.qualityProbe
                ? null
                : new NativeSenderPeer(
                    childPeerId,
                    connectionId ?? createOpaqueId(),
                    source.sessionId,
                    currentIceConfig!,
                    currentRoutePolicy.natPrediction,
                    source.client,
                    peerEvents,
                    source.codec,
                    {
                      connectionId: source.connectionId,
                      getProfile: () => currentQualitySettings,
                      format: () => {
                        const settings = remoteMediaRef.current?.stream
                          .getVideoTracks()[0]
                          ?.getSettings();
                        return {
                          width:
                            typeof settings?.width === "number"
                              ? settings.width
                              : null,
                          height:
                            typeof settings?.height === "number"
                              ? settings.height
                              : null,
                        };
                      },
                    },
                    (candidate?.connectionAttempt?.current ?? 1) > 1,
                  ),
          }
        : null;
      viewerRelay = new ViewerRelay(
        currentIceConfig,
        currentQualitySettings,
        {
          onInputChannel: (channel, connection) => inputRoute.attachChild(channel, connection),
          sendSignal: (targetPeerId, payload) =>
            active
              ? signal.send({
                  type: "signal",
                  targetPeerId,
                  payload,
                })
              : false,
          onUpdate: (snapshot) => {
            if (active) {
              setRelaySnapshot(snapshot);
              for (const [peerId, presentation] of [
                ...relayChildEvidenceStore.getSnapshot(),
              ]) {
                const reconciled =
                  reconcileViewerQualityEvidencePresentation(
                    presentation,
                    viewerRelay?.getSnapshot(peerId) ?? null,
                  );
                if (reconciled !== presentation) {
                  relayChildEvidenceStore.set(peerId, reconciled);
                }
              }
            }
          },
          onSenderUpdate: (snapshot, preparedRevision) => {
            if (
              !active ||
              pageSuspended ||
              !currentRoutePolicy.topologyOptimization
            ) {
              return;
            }
            const evidence = senderQualityEvidenceFromSnapshot(
              snapshot,
              preparedRevision ?? currentRouteRevision,
            );
            if (evidence) {
              signal.send(evidence);
            }
          },
          onPreparedChildFailed: reportPreparedChildFailure,
          onPreparedChildConnected: (revision, connectionId) =>
            active && signal.send({ type: "route-transport-connected", revision, connectionId }),
        },
        endpointMediaCopyCapacity,
        currentRoutePolicy.natPrediction,
        peerFactory,
      );
      viewerRelaySourceKey = sourceKey;
      viewerRelay.setChildren(currentAssignment.childPeerIds);
      return viewerRelay;
    }

    function reportPreparedChildFailure(
      revision: number,
      connectionId: string,
    ): void {
      if (active) {
        signal.send({
          type: "route-failed",
          revision,
          phase: "prepare",
          connectionId,
        });
      }
    }

    function reconcileRelayChildren(
      previousChildPeerIds: readonly string[],
      activeRevision?: number,
    ): void {
      const nextChildPeerIds = currentAssignment.childPeerIds;
      const changed =
        previousChildPeerIds.length !== nextChildPeerIds.length ||
        previousChildPeerIds.some(
          (peerId, index) => peerId !== nextChildPeerIds[index],
        );
      if (changed) {
        relayChildEvidenceStore.clear();
      }
      const relay = ensureViewerRelay();
      if (activeRevision === undefined) {
        relay?.setChildren(nextChildPeerIds);
      } else {
        relay?.activateChildren(activeRevision, nextChildPeerIds);
      }
    }

    function discardPendingPeer(): void {
      const probe = pendingPeer;
      pendingPeer = null;
      prepareParent(null);
      probe?.peer?.dispose();
    }

    function reportActivePeerFailure(
      parentPeerId: string,
      connectionId: string,
      peer: ViewerMediaPeer,
    ): boolean {
      if (
        currentRouteAssignment?.upstream.kind !== "peer" ||
        currentRouteAssignment.upstream.peerId !== parentPeerId ||
        peerRef.current !== peer ||
        peer.getConnectionIdentity()?.parentPeerId !== parentPeerId ||
        !peer.hasConnectionId(connectionId)
      ) {
        return true;
      }
      return (
        viewerSfuRoute?.reportPeerFailure(
          parentPeerId,
          connectionId,
        ) ?? true
      );
    }

    function pendingPeerHasDecodedFrame(
      probe: PendingPeerRoute,
    ): probe is PendingPeerRoute & { peer: ViewerMediaPeer; snapshot: PeerSnapshot } {
      const { snapshot } = probe;
      return (
        snapshot !== null &&
        snapshot.peerId === probe.parentPeerId &&
        probe.peer?.hasConnectionId(snapshot.connectionId) === true &&
        probe.peer.isConnected() &&
        probe.decodedFrame
      );
    }

    function provePendingPeer(): boolean {
      const probe = pendingPeer;
      if (!probe || probe.readySent) {
        return true;
      }
      if (
        !probe.connectedSent &&
        probe.snapshot?.connectionState === "connected" &&
        probe.snapshot.connectionId === probe.candidateConnectionId
      ) {
        probe.connectedSent = signal.send({
          type: "route-transport-connected",
          revision: probe.revision,
          connectionId: probe.candidateConnectionId,
        });
      }
      if (!pendingPeerHasDecodedFrame(probe)) {
        return false;
      }
      if (probe.qualityProbe && probe.qualityResult !== "approved") {
        return false;
      }
      probe.readySent = signal.send({
        type: "route-ready",
        revision: probe.revision,
        phase: "prepare",
        ...(probe.qualityProbe ? { qualityApproved: true as const } : {}),
      });
      return probe.readySent;
    }

    function ensureViewerSfuRoute(): ViewerSfuRoute {
      if (viewerSfuRoute) {
        return viewerSfuRoute;
      }
      let route: ViewerSfuRoute;
      route = new ViewerSfuRoute(currentPeerId!, {
        onInputChanged: () => refreshInput(value => value + 1),
        activatePeer: async (assignment, revision) => {
          if (!active || viewerSfuRoute !== route) {
            return;
          }
          if (revision !== undefined) {
            const probe = pendingPeer;
            if (
              !probe ||
              !probe.readySent ||
              probe.revision !== revision ||
              assignment.upstream.kind !== "peer" ||
              assignment.upstream.peerId !== probe.parentPeerId ||
              !probe.peer ||
              !probe.stream ||
              !probe.snapshot
            ) {
              discardPendingPeer();
              return false;
            }
            if (!probe.peer.isConnected()) return "waiting";
            const previousPeer = peerRef.current;
            const activatedPeer = probe.peer;
            activatedPeer.stopDecodedFrameProof();
            pendingPeer = null;
            prepareParent(null);
            peerRef.current = activatedPeer;
            activatedPeer.activatePreparedRoute();
            previousPeer?.dispose();
            applyMediaAssignment(
              {
                parentPeerId: probe.parentPeerId,
                childPeerIds: currentAssignment.childPeerIds,
              },
              true,
            );
            bindRemoteStream(probe.stream, revision);
            ensureViewerRelay("peer")?.setStream(probe.stream);
            setPeerSnapshot(probe.snapshot);
            setSfuUpstream(null);
            return true;
          }
          applyMediaAssignment({
            parentPeerId:
              assignment.upstream.kind === "peer"
                ? assignment.upstream.peerId
                : null,
            childPeerIds: currentAssignment.childPeerIds,
          });
          if (assignment.upstream.kind === "peer") {
            await drainPreparedParentSignals(assignment.upstream.peerId);
          }
        },
        preparePeer: (assignment, revision, candidate) => {
          if (!active || viewerSfuRoute !== route) {
            return;
          }
          discardPendingPeer();
          if (
            revision !== undefined &&
            candidate &&
            candidate.childPeerId === currentPeerId &&
            candidate.transport !== "sfu" &&
            assignment?.upstream.kind === "peer"
          ) {
            pendingPeer = {
              revision,
              parentPeerId: assignment.upstream.peerId,
              peer: null,
              decodedFrame: false,
              connectedSent: false,
              readySent: false,
              candidateConnectionId: candidate.connectionId,
              stream: null,
              snapshot: null,
              qualityProbe: candidate.qualityProbe
                ? new P2pQualityProbe()
                : null,
              qualityResult: "pending",
            };
          }
          prepareParent(
            assignment?.upstream.kind === "peer"
              ? assignment.upstream.peerId
              : null,
          );
        },
        prepareChild: (candidate, childPeerIds, revision) => {
          if (!active || viewerSfuRoute !== route) {
            return;
          }
          if (candidate && childPeerIds && revision !== undefined) {
            if (!ensureViewerRelay()?.prepareChild(revision, candidate, childPeerIds)) {
              reportPreparedChildFailure(revision, candidate.connectionId);
            }
          } else {
            viewerRelay?.discardPreparedChild();
          }
        },
        resetMedia: () => {
          if (!active || viewerSfuRoute !== route) {
            return;
          }
          currentAssignment = { parentPeerId: null, childPeerIds: [] };
          sfuTransportConnected = false;
          setSfuUpstream(null);
          clearPeerState();
          viewerRelay?.setChildren([]);
        },
        activateChildren: (childPeerIds, revision) => {
          if (!active || viewerSfuRoute !== route) {
            return;
          }
          const previousChildPeerIds = currentAssignment.childPeerIds;
          currentAssignment = limitMediaAssignment(
            {
              parentPeerId: currentAssignment.parentPeerId,
              childPeerIds: [...childPeerIds],
            },
            endpointMediaCopyCapacity,
          );
          reconcileRelayChildren(previousChildPeerIds, revision);
        },
        onSfuUpdate: (metrics, revision) => {
          if (active && viewerSfuRoute === route) {
            if (metrics) {
              if (
                currentRouteAssignment?.upstream.kind === "sfu" &&
                currentRouteConnectionId &&
                revision === currentRouteRevision &&
                sfuTransportConnected
              ) {
                qualityEvidenceReporter.offerMetrics(
                  currentRouteConnectionId,
                  metrics,
                  revision,
                  qualityPresentationEligible(sfuTransportConnected),
                );
              }
            }
            setSfuUpstream((current) =>
              metrics && current ? { ...current, metrics } : null,
            );
          }
        },
        currentPeerMetrics: () => activePeerMetrics,
        qualityProbeEligible: () =>
          qualityPresentationEligible(committedTransportConnected()),
        onSfuFailure: (failure, revision) => {
          if (active && viewerSfuRoute === route) {
            dispatchPresentation({ type: "receive-failure", revision, failure });
          }
        },
        onSfuState: (state, revision) => {
          if (active && viewerSfuRoute === route) {
            const connected = state === "connected";
            if (sfuTransportConnected !== connected) {
              sfuTransportConnected = connected;
              if (!connected) {
                invalidatePresentedMedia();
              }
            }
            setSfuUpstream((current) =>
              current ? { ...current, connectionState: state } : null,
            );
            dispatchPresentation({
              type: "connection",
              revision,
              connection: state,
            });
          }
        },
        onSfuVideoAvailability: (available, revision) => {
          if (!active || viewerSfuRoute !== route) {
            return;
          }
          if (available) {
            setSfuUpstream((current) =>
              current ? { ...current, connectionState: "connected" } : current,
            );
            return;
          }
          invalidatePresentedMedia();
          setSfuUpstream((current) =>
            current ? { ...current, connectionState: "reconnecting" } : current,
          );
          dispatchPresentation({
            type: "connection",
            revision,
            connection: "reconnecting",
          });
        },
        onSfuStream: (
          nextStream,
          _assignment,
          initialVideoStream,
          revision,
        ) => {
          if (!active || viewerSfuRoute !== route) {
            return;
          }
          const previousChildPeerIds = currentAssignment.childPeerIds;
          currentAssignment = limitMediaAssignment(
            { parentPeerId: null, childPeerIds: currentAssignment.childPeerIds },
            endpointMediaCopyCapacity,
          );
          reconcileRelayChildren(previousChildPeerIds);
          bindRemoteStream(nextStream, revision);
          const relay = ensureViewerRelay("sfu");
          relay?.setStream(nextStream);
          if (initialVideoStream) {
            sfuTransportConnected = true;
          }
          setSfuUpstream(
            (current) =>
              current ?? { connectionState: "connected", metrics: null },
          );
          if (initialVideoStream) {
            if (!pendingPeer) {
              peerRef.current?.dispose();
              peerRef.current = null;
              setPeerSnapshot(null);
            }
          }
        },
        send: (message) =>
          active && viewerSfuRoute === route ? signal.send(message) : false,
      });
      viewerSfuRoute = route;
      viewerSfuRouteRef.current = route;
      return route;
    }

    function clearViewerSfuRoute(): void {
      const route = viewerSfuRoute;
      discardPendingPeer();
      viewerSfuRoute = null;
      sfuTransportConnected = false;
      invalidatePresentedMedia();
      if (viewerSfuRouteRef.current === route) {
        viewerSfuRouteRef.current = null;
      }
      setSfuUpstream(null);
      void route?.disconnect();
    }

    function prepareParent(parentPeerId: string | null): void {
      if (preparedParentPeerId === parentPeerId) {
        return;
      }
      preparedParentPeerId = parentPeerId;
      preparedParentSignals = [];
    }

    async function drainPreparedParentSignals(
      parentPeerId: string,
    ): Promise<void> {
      while (
        active &&
        preparedParentPeerId === parentPeerId
      ) {
        const message = preparedParentSignals.shift();
        if (!message) {
          preparedParentPeerId = null;
          return;
        }
        const peer = ensurePeer();
        if (!peer) {
          return;
        }
        await peer.acceptSignal(message.fromPeerId, message.payload);
      }
    }

    function clearUpstreamState(clearMedia = false): void {
      qualityEvidenceReporter.reset();
      const peer = peerRef.current;
      peer?.dispose();
      peerRef.current = null;
      activePeerMetrics = null;
      setPeerSnapshot(null);
      if (clearMedia) {
        clearRemoteMedia();
      } else {
        dispatchPresentation({
          type: "connection",
          revision: currentRouteRevision,
          connection: "reconnecting",
        });
      }
    }

    function clearPeerState(clearMedia = false): void {
      clearUpstreamState(clearMedia);
      relayChildEvidenceStore.clear();
      viewerRelay?.stop();
    }

    function denyRoomAccess(failure: ViewerFailureCode): void {
      messageAuthority.invalidate();
      setAssignedRoute(null);
      clearViewerSfuRoute();
      clearPeerState(true);
      clearParticipantPresence();
      // A denied page can remain mounted or enter the back/forward cache.
      releaseNativeClient();
      dispatchPresentation({ type: "access", access: "denied", failure });
    }

    function applyMediaAssignment(assignment: MediaAssignment, preserveUpstream = false): void {
      const nextAssignment = limitMediaAssignment(
        assignment,
        endpointMediaCopyCapacity,
      );
      const previousParentId = currentAssignment.parentPeerId;
      const previousChildPeerIds = currentAssignment.childPeerIds;
      currentAssignment = nextAssignment;

      if (!preserveUpstream && previousParentId !== nextAssignment.parentPeerId) {
        clearUpstreamState();
      }
      reconcileRelayChildren(previousChildPeerIds);
    }

    function ensurePendingPeerRoute(): ViewerMediaPeer | null {
      const probe = pendingPeer;
      if (!probe || !currentIceConfig) return null;
      if (probe.peer) return probe.peer;
      const peer = createViewerMediaPeer(
        currentIceConfig,
        {
          sendSignal: (targetPeerId, payload) =>
            signal.send(viewerSignalMessage(targetPeerId, payload)),
          sendRestartRequest: createOwnedViewerRestartSender(
            (targetPeerId, connectionId) => {
              const identity = peer.getConnectionIdentity();
              return (
                active &&
                pendingPeer !== probe &&
                peerRef.current === peer &&
                identity?.parentPeerId === targetPeerId &&
                identity.connectionId === connectionId
              );
            },
            (message) => signal.send(message),
          ),
          onStream: (stream) => {
            if (pendingPeer === probe) {
              probe.stream = stream;
              provePendingPeer();
            } else if (active && peerRef.current === peer) {
              bindRemoteStream(stream, currentRouteRevision);
              ensureViewerRelay()?.setStream(stream);
            }
          },
          onUpdate: (snapshot) => {
            if (pendingPeer === probe) {
              if (snapshot.connectionId !== probe.candidateConnectionId) return;
              probe.snapshot = snapshot;
              noteReceiveFailure(snapshot, probe.revision);
              if (probe.qualityProbe && !probe.readySent) {
                if (
                  pageSuspended ||
                  currentHostPaused ||
                  !qualityPresentationEligible(
                    peerRef.current?.isConnected() === true,
                  )
                ) {
                  probe.qualityProbe.reset();
                  probe.qualityResult = "pending";
                } else {
                  probe.qualityResult = probe.qualityProbe.observe(
                    activePeerMetrics,
                    snapshot.metrics,
                  );
                }
              }
              provePendingPeer();
              if (peer.isConnected()) viewerSfuRoute?.retryPeerActivation(snapshot.connectionId);
            } else if (active && peerRef.current === peer) {
              noteReceiveFailure(snapshot, currentRouteRevision);
              activePeerMetrics = snapshot.metrics;
              offerPeerQualityEvidence(snapshot, peer);
              setPeerSnapshot(snapshot);
              dispatchPresentation({
                type: "connection",
                revision: currentRouteRevision,
                connection: peer.isRecovering()
                  ? "reconnecting"
                  : connectionFact(snapshot.connectionState),
              });
            }
          },
          onFirstDecodedFrame: (connectionId): boolean => {
            if (
              pendingPeer !== probe ||
              !peer.hasConnectionId(connectionId) ||
              connectionId !== probe.candidateConnectionId
            ) {
              return true;
            }
            probe.decodedFrame = true;
            return provePendingPeer();
          },
          onRecoveryExhausted: (parentPeerId, connectionId): boolean => {
            if (pendingPeer === probe && peer.hasConnectionId(connectionId)) {
              probe.peer = null;
              probe.stream = null;
              probe.snapshot = null;
              probe.decodedFrame = false;
              probe.connectedSent = false;
              probe.readySent = false;
              peer.dispose();
              if (!viewerSfuRoute?.retryPeerActivation(connectionId)) {
                signal.send({
                  type: "route-failed",
                  revision: probe.revision,
                  phase: "prepare",
                  connectionId,
                });
              }
              return true;
            }
            return peerRef.current === peer
              ? reportActivePeerFailure(parentPeerId, connectionId, peer)
              : true;
          },
        },
        {
          natPredictionEnabled: currentRoutePolicy.natPrediction,
          recoveryOwner: "route",
        },
      );
      probe.peer = peer;
      return peer;
    }

    function ensurePeer(): ViewerMediaPeer | null {
      if (peerRef.current) {
        return peerRef.current;
      }
      if (!currentIceConfig) {
        return null;
      }
      const peer = createViewerMediaPeer(
        currentIceConfig,
        {
          sendSignal: (targetPeerId, payload) =>
              signal.send(viewerSignalMessage(targetPeerId, payload)),
          sendRestartRequest: createOwnedViewerRestartSender(
            (targetPeerId, connectionId) => {
              const identity = peer.getConnectionIdentity();
              return (
                active &&
                peerRef.current === peer &&
                identity?.parentPeerId === targetPeerId &&
                identity.connectionId === connectionId
              );
            },
            (message) => signal.send(message),
          ),
          onStream: (nextStream) => {
            if (active) {
              bindRemoteStream(nextStream, currentRouteRevision);
              // Audio and video can arrive as separate track events on the
              // same MediaStream, so refresh both downstream senders each time.
              ensureViewerRelay()?.setStream(nextStream);
            }
          },
          onUpdate: (snapshot) => {
            if (active) {
              noteReceiveFailure(snapshot, currentRouteRevision);
              activePeerMetrics = snapshot.metrics;
              offerPeerQualityEvidence(snapshot, peer);
              // Host signaling presence does not retire this edge's recovery
              // or the relay's downstream connections.
              setPeerSnapshot(snapshot);
              dispatchPresentation({
                type: "connection",
                revision: currentRouteRevision,
                connection: peer.isRecovering()
                  ? "reconnecting"
                  : connectionFact(snapshot.connectionState),
              });
            }
          },
          onRecoveryExhausted: (
            parentPeerId,
            connectionId,
          ): boolean => {
            if (viewerSfuRoute) {
              return reportActivePeerFailure(
                parentPeerId,
                connectionId,
                peer,
              );
            }
            invalidateQualityPresentation();
            dispatchPresentation({
              type: "route-status",
              revision: currentRouteRevision,
              state: "failed",
            });
            return true;
          },
        },
        { natPredictionEnabled: currentRoutePolicy.natPrediction },
      );
      peerRef.current = peer;
      return peer;
    }

    function noteReceiveFailure(snapshot: PeerSnapshot, revision: number): void {
      dispatchPresentation({ type: "receive-failure", revision,
        failure: snapshot.error?.key === "viewer.msg.videoRejected" ? "VIDEO_REJECTED" : null });
    }

    async function handleMessage(
      message: ServerMessage,
      authorityToken: number,
    ): Promise<void> {
      if (message.type === "relay-volunteer") {
        setRelayVolunteerEnabled(message.enabled);
        const intent = signal.relayVolunteerRequest;
        setRelayVolunteerPending(intent !== null && (intent.enabled !== message.enabled ||
          message.enabled && intent.capacity !== message.capacity));
        // Withdrawal is a controller handoff. Retain physical room for old
        // children until its assignments retire them, including on reauth.
        endpointMediaCopyCapacity = Math.max(endpointMediaCopyCapacity, message.capacity);
        viewerRelay?.updateCapacity(endpointMediaCopyCapacity);
        return;
      }
      if (message.type === "viewer-route-preference") {
        setRoutePreference(message.preference);
        setRoutePreferencePending(false);
        setRoutePreferenceRejected(!message.accepted);
        return;
      }
      if (message.type === "authenticated") {
        inputRoute.reauthenticate();
        dispatchPresentation({ type: "access", access: "ready" });
        setViewerPasswordDraft("");
        setViewerPasswordError(null);
        relayChildEvidenceStore.clear();
        ordinaryRelayCapacity = message.endpointMediaCopyCapacity;
        endpointMediaCopyCapacity = currentPeerId === null ? message.endpointMediaCopyCapacity
          : Math.max(endpointMediaCopyCapacity, message.endpointMediaCopyCapacity);
        currentPeerId = message.peerId;
        setSelfPeerId(message.peerId);
        setRelayVolunteerPending(signal.relayVolunteerRequest !== null);
        currentAssignment = limitMediaAssignment(
          currentAssignment,
          endpointMediaCopyCapacity,
        );
        viewerRelay?.updateCapacity(endpointMediaCopyCapacity);
        viewerAuthorizationGeneration =
          message.viewerAuthorizationGeneration;
        currentRoutePolicy = message.routePolicy;
        setPeerOnly(message.routePolicy.peerOnly);
        const nextRouteRevision = message.routeRevision;
        if (nextRouteRevision !== currentRouteRevision) {
          relayChildEvidenceStore.clear();
        }
        currentRouteRevision = nextRouteRevision;
        activateRouteIdentity(
          message.routeRevision,
          message.routeAssignment,
          message.connectionId,
        );
        acceptAssignedRoute(
          message.routeRevision,
          message.routeAssignment.upstream,
          "active",
          undefined,
          true,
        );
        signal.send(relayCapacityMessageForBrowser());
        // Normalize this visit's limit before signaling replays its intent.
        updateRelayVolunteer();
        currentIceConfig = message.iceConfig;
        const sharingPaused = message.hostPaused ?? false;
        if (currentHostPaused !== sharingPaused && sharingPaused) {
          invalidatePresentedMedia();
        }
        currentHostPaused = sharingPaused;
        dispatchPresentation({
          type: "host",
          host: sharingPaused
            ? "paused"
            : message.hostOnline
              ? "online"
              : "unknown",
        });
        currentQualitySettings = message.qualitySettings;
        currentShareGeneration = message.shareGeneration;
        void viewerRelay?.updateProfile(currentQualitySettings);
        const route = ensureViewerSfuRoute();
        route.setPaused(sharingPaused);
        viewerRelay?.resyncSignaling();
        await route.resyncAuthoritative(
          {
            revision: message.routeRevision,
            phase: "active",
            assignment: message.routeAssignment,
          },
          message.peerId,
        );
        if (!active || !messageAuthority.owns(authorityToken)) {
          return;
        }
        if (
          !message.hostOnline &&
          message.connectionId === null &&
          !peerRef.current?.isConnected()
        ) {
          clearPeerState();
        }
        const peer = peerRef.current;
        peer?.updateIceConfig(message.iceConfig);
        viewerRelay?.updateIceConfig(message.iceConfig);
        if (
          message.connectionId &&
          !peer?.hasConnectionId(message.connectionId)
        ) {
          const parentPeerId = currentAssignment.parentPeerId;
          if (parentPeerId) {
            signal.send({
              type: "restart-request",
              targetPeerId: parentPeerId,
              connectionId: message.connectionId,
              rebuild: true,
            });
          }
        } else if (peer?.hasConnection() && !peer.isConnected()) {
          peer.requestRecovery();
        }
        return;
      }
      if (message.type === "route-update") {
        const result = ensureViewerSfuRoute().accept(message);
        if (result !== "stale") {
          if (
            result === "accepted" &&
            message.phase === "prepare" &&
            message.candidate.childPeerId === currentPeerId
          ) {
            pendingRouteConnection = {
              revision: message.revision,
              connectionId: message.candidate.connectionId,
            };
            if (remoteMediaRef.current === null) {
              acceptAssignedRoute(
                message.revision,
                message.assignment.upstream,
                "prepare",
                message.candidate.connectionAttempt,
              );
            }
          }
          if (message.phase === "active") {
            const samePeerUpstream =
              currentRouteAssignment?.upstream.kind === "peer" &&
              message.assignment.upstream.kind === "peer" &&
              currentRouteAssignment.upstream.peerId ===
                message.assignment.upstream.peerId;
            const sameSfuUpstream =
              currentRouteAssignment?.upstream.kind === "sfu" &&
              message.assignment.upstream.kind === "sfu" &&
              currentRouteAssignment.sfuPublicationGeneration ===
                message.assignment.sfuPublicationGeneration;
            const connectionId =
              pendingRouteConnection?.revision === message.revision
                ? pendingRouteConnection.connectionId
                : samePeerUpstream || sameSfuUpstream
                  ? currentRouteConnectionId
                  : null;
            if (result === "accepted") {
              if (message.revision !== currentRouteRevision) {
                relayChildEvidenceStore.clear();
              }
              activateRouteIdentity(
                message.revision,
                message.assignment,
                connectionId,
              );
            }
            acceptAssignedRoute(
              message.revision,
              message.assignment.upstream,
              "active",
            );
          }
        }
        return;
      }
      if (message.type === "route-status") {
        if (message.state === "failed") {
          invalidateQualityPresentation();
        }
        dispatchPresentation({
          type: "route-status",
          revision: message.revision,
          state: message.state,
        });
        return;
      }
      if (message.type === "viewer-quality-evidence") {
        acceptRelayChildEvidence(message);
        return;
      }
      if (message.type === "sfu-config") {
        await ensureViewerSfuRoute().acceptConfig(message);
        return;
      }
      if (message.type === "sfu-signal") {
        await viewerSfuRoute?.acceptSignal(message);
        return;
      }
      if (message.type === "quality-settings") {
        currentQualitySettings = message.qualitySettings;
        void viewerRelay?.updateProfile(currentQualitySettings);
        return;
      }
      if (message.type === "route-policy") {
        if (
          currentShareGeneration === null ||
          currentShareGeneration === message.shareGeneration
        ) {
          currentShareGeneration = message.shareGeneration;
          currentRoutePolicy = message.routePolicy;
          setPeerOnly(message.routePolicy.peerOnly);
        }
        return;
      }
      if (message.type === "signal") {
        if (pendingPeer?.parentPeerId === message.fromPeerId) {
          if (currentAssignment.parentPeerId !== message.fromPeerId) {
            await ensurePendingPeerRoute()?.acceptSignal(
              message.fromPeerId,
              message.payload,
            );
            return;
          }
          const activeIdentity = peerRef.current?.getConnectionIdentity();
          const owner = exactPeerSignalOwner(
            message.payload.connectionId,
            pendingPeer.candidateConnectionId,
            activeIdentity?.parentPeerId === message.fromPeerId
              ? activeIdentity.connectionId
              : null,
          );
          if (owner === "pending") {
            await ensurePendingPeerRoute()?.acceptSignal(
              message.fromPeerId,
              message.payload,
            );
          } else if (owner === "active") {
            await peerRef.current?.acceptSignal(
              message.fromPeerId,
              message.payload,
            );
          }
          return;
        }
        if (preparedParentPeerId === message.fromPeerId) {
          preparedParentSignals.push(message);
          return;
        }
        if (
          viewerRelay &&
          (await viewerRelay.acceptSignal(
            message.fromPeerId,
            message.payload,
            viewerRelay.getSignalRouteRevision(
              message.fromPeerId,
              message.payload.connectionId,
              currentRouteRevision,
            ),
          ))
        ) {
          return;
        }
        if (currentAssignment.parentPeerId !== message.fromPeerId) {
          return;
        }
        const peer = ensurePeer();
        if (!peer) {
          return;
        }
        await peer.acceptSignal(message.fromPeerId, message.payload);
        return;
      }
      if (message.type === "restart-request") {
        await viewerRelay?.recover(
          message.fromPeerId,
          message.connectionId,
          message.rebuild,
        );
        return;
      }
      if (message.type === "audio-presence") {
        setAudioAvailable(message.available);
        return;
      }
      if (message.type === "co-play-presence") {
        setCoPlayParticipants(message.participants);
        return;
      }
      if (message.type === "host-status") {
        if (!message.online) setCoPlayParticipants([]);
        if (currentHostPaused !== message.paused && message.paused) {
          invalidatePresentedMedia();
        }
        currentHostPaused = message.paused;
        dispatchPresentation({
          type: "host",
          host: message.paused
            ? "paused"
            : message.online
              ? "online"
              : "offline",
        });
        viewerSfuRoute?.setPaused(message.paused);
        return;
      }
      if (message.type === "viewer-presence") {
        const hostLabel = labelParticipantSnapshot(message.viewers).host?.label;
        if (hostLabel) {
          setLastHostDisplayName(hostLabel);
        }
        setParticipantPresence(message.viewers);
        return;
      }
      if (message.type === "sharing-stopped") {
        setAudioAvailable(null);
        invalidatePresentedMedia();
        currentRouteAssignment = null;
        currentRouteConnectionId = null;
        pendingRouteConnection = null;
        currentShareGeneration = null;
        currentRoutePolicy = DEFAULT_ROUTE_POLICY;
        setAssignedRoute(null);
        currentHostPaused = false;
        clearViewerSfuRoute();
        clearPeerState(true);
        clearHostPresence();
        dispatchPresentation({ type: "sharing-stopped" });
        return;
      }
      if (message.type === "viewer-grant-revoked") {
        if (
          viewerAuthorizationGeneration !==
          message.viewerAuthorizationGeneration
        ) {
          return;
        }
        viewerAuthorizationGeneration = null;
        clearViewerGrant(roomId);
        denyRoomAccess("INVALID_TOKEN");
        signal.stop();
        return;
      }
      if (message.type === "room-closed") {
        denyRoomAccess("ROOM_CLOSED");
        signal.stop();
        return;
      }
      if (message.type === "error") {
        const failure = viewerFailureFromServerCode(message.code);
        if (failure !== null && failure !== "SERVER_ERROR") {
          denyRoomAccess(failure);
        }
        if (message.code === "INVALID_TOKEN") {
          clearViewerGrant(roomId);
          if (!viewerGrant && viewerPasswordAttempt) {
            setViewerPasswordError("join.passwordError");
          }
          return;
        }
        if (message.code === "ROOM_ACCESS_DENIED") {
          if (viewerPasswordAttempt) {
            setViewerPasswordError("join.passwordError");
            setViewerPasswordExpanded(true);
          }
          return;
        }
        if (failure === "SERVER_ERROR") {
          dispatchPresentation({ type: "server-error" });
        }
      }
    }

    signal.start();
    return () => {
      active = false;
      requestRelayVolunteerRef.current = null;
      document.removeEventListener("freeze", suspendForPageLifecycle);
      document.removeEventListener("resume", recoverFromPageLifecycle);
      document.removeEventListener("visibilitychange", handleVisibilityChange);
      window.removeEventListener("pagehide", suspendForPageLifecycle);
      window.removeEventListener("pageshow", recoverFromPageLifecycle);
      currentPeerId = null;
      qualityEvidenceReporter.reset();
      if (qualityEvidenceReporterRef.current === qualityEvidenceReporter) {
        qualityEvidenceReporterRef.current = null;
      }
      relayChildEvidenceStore.clear();
      signal.stop();
      inputRoute.dispose();
      clearParticipantPresence();
      if (signalRef.current === signal) {
        signalRef.current = null;
      }
      void viewerSfuRoute?.disconnect();
      if (viewerSfuRouteRef.current === viewerSfuRoute) {
        viewerSfuRouteRef.current = null;
      }
      viewerSfuRoute = null;
      preparedParentPeerId = null;
      preparedParentSignals = [];
      discardPendingPeer();
      peerRef.current?.dispose();
      peerRef.current = null;
      viewerRelay?.dispose();
      viewerRelay = null;
      viewerRelaySourceKey = null;
      releaseNativeClient();
    };
  }, [roomId, viewerGrant, viewerPasswordAttempt]);

  return { relaySnapshot, relayChildEvidence };
}
