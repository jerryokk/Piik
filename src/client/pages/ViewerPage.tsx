import type { VideoFit } from "../media/video-geometry";
import { useEffect, useMemo, useReducer, useRef, useState } from "react";
import { MAX_VIEWERS_PER_ROOM_LIMIT, viewerPasswordSchema, type ParticipantPresenceEntry, type ViewerRoutePreference, type CoPlayParticipant } from "../../shared/protocol";
import { viewerReconnectRoute, viewerRouteEvidence } from "./viewer/route-evidence";
import { AppHeader, LedStrip } from "../ui/components/shell/Header";
import { RoomInteractions } from "../ui/components/room/RoomInteractions";
import { RoomChatOverlay, RoomChatOverlaySettings, RoomChatToggle } from "../ui/components/room/RoomChatOverlay";
import type { RoomInteractionSession } from "../lib/room/interactions";
import type { RoomVoiceSession } from "../room-voice/session";
import { useRoomVoiceMetrics, useRoomVoiceSnapshot } from "../room-voice/use-room-voice-state";
import { describeVoiceConnection, voiceConnectionDetails } from "../room-voice/VoiceConnectionDetails";
import { RoomVoicePanel, RoomVoiceControls, RoomVoiceSettings } from "../room-voice/RoomVoicePanel";
import { ConnectionDetails } from "../ui/components/people/ConnectionDetails";
import { PawnDetail } from "../ui/components/people/PawnDetail";
import { Lcd } from "../ui/components/room/RoomChip";
import { RouteTree } from "../ui/components/people/RouteTree";
import { StageOverlay, StageTv } from "../ui/components/people/Stage";
import { StatusIndicator } from "../ui/components/people/StatusIndicator";
import { Tooltip } from "../ui/components/shell/Tooltip";
import { PlaybackControls } from "../ui/components/playback/PlaybackControls";
import { RemoteControl } from "../ui/components/control/RemoteControl";
import { InputRoute } from "../remote-control/input-route";
import { useTheaterMode } from "../ui/components/playback/use-theater-mode";
import { Btn, FieldCap, NameTag, Pill, Row } from "../ui/components/shell/primitives";
import { Glyph } from "../ui/icons";
import { useCopy, type CopyKey } from "../ui/copy";
import { defaultViewerDisplayName, readDisplayName, readStoredDisplayName, saveDisplayName } from "../lib/room/display-name";
import { useDocumentTitle } from "../ui/document-title";
import { deriveViewerStatus } from "../ui/media-status";
import { getStableClientId } from "../lib/session";
import { SignalingClient } from "../lib/signaling";
import { readRoomPreferences, saveRoomPreferences } from "../lib/room/preferences";
import { labelParticipantSnapshot } from "../lib/room/viewer-presence";
import { relayVolunteerLimit } from "../media/relay-capability";
import { metricsFromQualityEvidence, ViewerQualityEvidenceReporter } from "../media/viewer-quality-evidence";
import { INITIAL_VIEWER_PRESENTATION_STATE, deriveViewerPresentation, reduceViewerPresentation } from "../media/viewer-presentation";
import type { RemoteMediaBinding } from "../media/viewer-playback";
import { ViewerSfuRoute } from "../media/viewer-sfu-route";
import type { PeerSnapshot } from "../types";
import { MAX_ENDPOINT_MEDIA_CHILDREN } from "../webrtc/media-assignment";
import type { ViewerMediaPeer } from "../webrtc/viewer-peer";
import { ViewerAccessGate } from "./viewer/ViewerAccessGate";
import { viewerAttemptCopy, viewerDeckModel } from "./viewer/viewer-deck-model";
import {
  useViewerInputUpstream,
  useViewerPlayback,
} from "./viewer/use-viewer-playback";
import { createViewerSessionHelpers } from "./viewer/session-helpers";
import type {
  SfuUpstreamState,
  ViewerAssignedRoute,
} from "./viewer/types";
import { useViewerSession } from "./viewer/use-viewer-session";

interface ViewerPageProps {
  roomId: string;
  viewerGrant?: string;
  invalidGrant?: boolean;
  launchedByClient?: boolean;
}

export function ViewerPage({
  roomId,
  viewerGrant,
  invalidGrant = false,
  launchedByClient = false,
}: ViewerPageProps) {
  const { lang, t, vis, titleFrames } = useCopy();
  const clientActivationRef = useRef(launchedByClient);
  clientActivationRef.current = launchedByClient;
  const viewerClientId = useMemo(
    () => getStableClientId("viewer", roomId),
    [roomId],
  );
  const [presentationState, dispatchPresentationState] = useReducer(
    reduceViewerPresentation,
    INITIAL_VIEWER_PRESENTATION_STATE,
  );
  const presentation = deriveViewerPresentation(presentationState);
  const accessState = presentationState.access;
  const signalStatus = presentationState.signal;
  const [remoteMedia, setRemoteMedia] = useState<RemoteMediaBinding | null>(
    null,
  );
  const [peerSnapshot, setPeerSnapshot] = useState<PeerSnapshot | null>(null);
  const [, refreshInput] = useState(0);
  const inputRoute = useMemo(() => new InputRoute(() => refreshInput(value => value + 1)), []);
  const [sfuUpstream, setSfuUpstream] = useState<SfuUpstreamState | null>(null);
  const [assignedRoute, setAssignedRoute] = useState<ViewerAssignedRoute | null>(
    null,
  );
  const [showConnectionDetails, setShowConnectionDetails] = useState(false);
  const [showTopology, setShowTopology] = useState(false);
  const [theaterMode, setTheaterMode] = useTheaterMode();
  const [audioAvailable, setAudioAvailable] = useState<boolean | null>(null);
  const [hasCustomDisplayName, setHasCustomDisplayName] = useState(
    () => readStoredDisplayName() !== null,
  );
  const [displayName, setDisplayName] = useState(() =>
    readDisplayName(defaultViewerDisplayName(vis)),
  );
  const [displayNameDraft, setDisplayNameDraft] = useState(displayName);
  const [displayNameError, setDisplayNameError] = useState(false);
  const [editingDisplayName, setEditingDisplayName] = useState(false);
  const [participantPresence, setParticipantPresence] = useState<
    ParticipantPresenceEntry[] | null
  >(null);
  const [lastHostDisplayName, setLastHostDisplayName] = useState<string | null>(
    null,
  );
  const [viewerPasswordDraft, setViewerPasswordDraft] = useState("");
  const [viewerPasswordError, setViewerPasswordError] = useState<CopyKey | null>(
    null,
  );
  const [viewerPasswordAttempt, setViewerPasswordAttempt] = useState<{
    password: string;
    sequence: number;
  } | null>(null);
  const [viewerPasswordExpanded, setViewerPasswordExpanded] = useState(false);
  // Presentation-only: which couch pawn is drilled into, and this viewer's
  // own peer id (mirrors the authenticated message for couch/route-tree).
  const [selectedPawn, setSelectedPawn] = useState<string | null>(null);
  const [selfPeerId, setSelfPeerId] = useState<string | null>(null);
  const [coPlayParticipants, setCoPlayParticipants] = useState<CoPlayParticipant[]>([]);

  const { host: labeledHostPresence, viewers } = useMemo(
    () => labelParticipantSnapshot(participantPresence ?? []),
    [participantPresence],
  );
  const currentHostDisplayName = labeledHostPresence?.label ?? null;
  const hostDisplayName = currentHostDisplayName ?? lastHostDisplayName;

  const videoRef = useRef<HTMLVideoElement | null>(null);
  const peerRef = useRef<ViewerMediaPeer | null>(null);
  const viewerSfuRouteRef = useRef<ViewerSfuRoute | null>(null);
  const signalRef = useRef<SignalingClient | null>(null);
  const [interactionSession, setInteractionSession] = useState<RoomInteractionSession | null>(null);
  const [roomVoiceSession, setRoomVoiceSession] = useState<RoomVoiceSession | null>(null);
  const roomVoiceSnapshot = useRoomVoiceSnapshot(roomVoiceSession);
  const roomVoice = roomVoiceSnapshot?.connected ? roomVoiceSnapshot.room : null;
  const ownVoiceConnection = describeVoiceConnection(roomVoiceSnapshot);
  const voiceMetrics = useRoomVoiceMetrics(roomVoiceSession, showConnectionDetails || selectedPawn !== null, roomVoice);
  const [pictureFit, setPictureFit] = useState<VideoFit>(() => readRoomPreferences().pictureFit);
  const [pictureRotation, setPictureRotation] = useState(() => readRoomPreferences().pictureRotation);
  const [remoteTouchMode, setRemoteTouchMode] = useState(() => readRoomPreferences().remoteTouchMode);
  const [routeOptions, setRouteOptions] = useState<{ sfu: boolean; sfuOnly: boolean } | null>(null);
  const [routePreference, setRoutePreference] = useState<ViewerRoutePreference>("auto");
  const [routePreferencePending, setRoutePreferencePending] = useState(false);
  const [routePreferenceRejected, setRoutePreferenceRejected] = useState(false);
  const [relayVolunteerSupported, setRelayVolunteerSupported] = useState(false);
  const [relayVolunteerEnabled, setRelayVolunteerEnabled] = useState(false);
  const [relayVolunteerPending, setRelayVolunteerPending] = useState(false);
  const [relayVolunteerLimits, setRelayVolunteerLimits] = useState(() => relayVolunteerLimit(null, MAX_ENDPOINT_MEDIA_CHILDREN, MAX_VIEWERS_PER_ROOM_LIMIT));
  const requestRelayVolunteerRef = useRef<((enabled: boolean, capacity?: number) => void) | null>(null);
  const [peerOnly, setPeerOnly] = useState(false);
  useEffect(() => {
    if (signalStatus !== "connected") {
      setRoutePreferencePending(false);
      setRelayVolunteerPending(false);
    }
  }, [signalStatus]);
  const presentationStateRef = useRef(presentationState);
  presentationStateRef.current = presentationState;
  const qualityEvidenceReporterRef =
    useRef<ViewerQualityEvidenceReporter | null>(null);
  const qualityFrameProofGenerationRef = useRef<number | null>(null);
  const displayNameRef = useRef(displayName);
  useEffect(() => {
    if (hasCustomDisplayName) return;
    const fallback = defaultViewerDisplayName(vis);
    if (displayNameRef.current === fallback) return;
    displayNameRef.current = fallback;
    setDisplayName(fallback);
    if (!editingDisplayName) setDisplayNameDraft(fallback);
    signalRef.current?.setDisplayName(fallback);
  }, [hasCustomDisplayName, lang, vis, viewerClientId]);
  const remoteMediaRef = useRef<RemoteMediaBinding | null>(null);
  const mediaGenerationRef = useRef(0);
  const hostPlaybackPauseRef = useRef({ active: false, resume: false });
  const {
    dispatchPresentation,
    bindRemoteStream,
    clearRemoteMedia,
    rearmCurrentFrameProof,
    invalidateQualityPresentation,
    invalidatePresentedMedia,
    mediaBindingIsCurrent,
    attemptPlayback,
    acceptAssignedRoute,
    clearParticipantPresence,
    clearHostPresence,
  } = useMemo(
    () =>
      createViewerSessionHelpers({
        presentationStateRef,
        dispatchPresentationState,
        remoteMediaRef,
        mediaGenerationRef,
        setRemoteMedia,
        qualityEvidenceReporterRef,
        qualityFrameProofGenerationRef,
        setAssignedRoute,
        setParticipantPresence,
        setLastHostDisplayName,
      }),
    [],
  );
  const routePresentation = viewerRouteEvidence(
    assignedRoute?.upstream ?? null,
    peerSnapshot,
    sfuUpstream,
  );
  const assignedRouteKind =
    assignedRoute?.upstream.kind === "sfu"
      ? "sfu"
      : assignedRoute?.upstream.kind === "peer"
        ? "p2p"
        : routePresentation.route;
  const viewerStatus = deriveViewerStatus(
    presentation,
    signalStatus,
    assignedRouteKind,
  );
  const titleContent = titleFrames(viewerStatus.titleFrameKey);
  useDocumentTitle(
    [
      accessState === "ready" ? roomId : null,
      titleContent.label,
      viewerStatus.titleMarker,
    ],
    titleContent.variations,
    `${lang}:${vis}:${viewerStatus.titleFrameKey}:${roomId}`,
  );
  const peerConnectionIdentity = peerRef.current?.getConnectionIdentity() ?? null;
  const reconnectRoute = viewerReconnectRoute(
    assignedRoute?.upstream ?? null,
    peerSnapshot,
    sfuUpstream,
    peerConnectionIdentity,
  );
  const reconnectAvailable =
    signalStatus === "connected" &&
    reconnectRoute !== null &&
    presentationState.connection !== "reconnecting";
  const routeMetrics = routePresentation.evidence?.metrics ?? null;

  const { relaySnapshot, relayChildEvidence } = useViewerSession({
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
  });

  useViewerInputUpstream(inputRoute, peerRef, viewerSfuRouteRef);
  useViewerPlayback({
    videoRef,
    remoteMedia,
    presentationState,
    presentationStateRef,
    hostPlaybackPauseRef,
    qualityFrameProofGenerationRef,
    mediaBindingIsCurrent,
    attemptPlayback,
    dispatchPresentation,
  });

  function retryConnection(): void {
    if (reconnectRoute === "sfu") {
      viewerSfuRouteRef.current?.reconnectActive();
    } else if (reconnectRoute === "p2p") {
      if (peerRef.current?.requestRecovery(true)) {
        dispatchPresentation({
          type: "connection",
          revision: presentationState.revision ?? assignedRoute?.revision ?? 0,
          connection: "reconnecting",
        });
      }
    }
  }

  function commitDisplayName(): void {
    const fallback = defaultViewerDisplayName(vis);
    const saved = saveDisplayName(displayNameDraft, fallback);
    if (!saved) {
      setDisplayNameError(true);
      return;
    }
    displayNameRef.current = saved;
    setDisplayName(saved);
    setDisplayNameDraft(saved);
    setDisplayNameError(false);
    setEditingDisplayName(false);
    setHasCustomDisplayName(readStoredDisplayName() !== null);
    signalRef.current?.setDisplayName(saved);
  }

  function submitViewerPassword(event: React.FormEvent<HTMLFormElement>): void {
    event.preventDefault();
    if (!viewerPasswordSchema.safeParse(viewerPasswordDraft).success) {
      setViewerPasswordError("join.passwordRule");
      return;
    }
    setViewerPasswordError(null);
    dispatchPresentation({
      type: "access",
      access: "checking",
      failure: null,
    });
    setViewerPasswordAttempt((current) => ({
      password: viewerPasswordDraft,
      sequence: (current?.sequence ?? 0) + 1,
    }));
  }

  if (accessState !== "ready") {
    return (
      <ViewerAccessGate
        accessState={accessState}
        failureCode={presentation.failureCode}
        messageKey={presentation.messageKey}
        activity={viewerStatus.activity}
        vis={vis}
        t={t}
        viewerGrant={viewerGrant}
        invalidGrant={invalidGrant}
        viewerPasswordDraft={viewerPasswordDraft}
        viewerPasswordError={viewerPasswordError}
        viewerPasswordExpanded={viewerPasswordExpanded}
        onViewerPasswordDraftChange={setViewerPasswordDraft}
        onViewerPasswordErrorCleared={() => setViewerPasswordError(null)}
        onViewerPasswordExpandedChange={setViewerPasswordExpanded}
        onViewerPasswordSubmit={submitViewerPassword}
      />
    );
  }

  const {
    retryAttempt,
    stageMessage,
    connectionProgress,
  } = viewerAttemptCopy(
    presentation,
    presentationState.revision,
    assignedRoute,
    t,
  );
  const {
    selectedChildEvidence,
    freshRelayChildEvidence,
    pictureActive,
    couchEntries,
    selectableRelayChildren,
    selectedRelayChild,
  } = viewerDeckModel({
    viewers,
    relayChildEvidence,
    selectedPawn,
    selfPeerId,
    coPlayParticipants,
    host: presentationState.host,
    roomVoice,
  });

  return (
    <div className="lr-app">
      <AppHeader
        showApp={false}
        led={
          <LedStrip
            state={viewerStatus.connection.tone}
            label={t(viewerStatus.connection.labelKey)}
            comic={viewerStatus.connection.comic}
          />
        }
      />
      <main className={`lr-room${theaterMode ? " is-theater" : ""}`}>
        <h1 className="visually-hidden">
          {hostDisplayName
            ? t("viewer.title", { name: hostDisplayName })
            : t("viewer.titleFallback")}
        </h1>
        <div className="lr-scene" id="viewer-stage">
          <StageTv
            live={presentation.hasCurrentFrame || presentation.hasRetainedFrame}
            label={t("viewer.stageAria")}
            indicator={<StatusIndicator status={viewerStatus.television}
              label={retryAttempt ? stageMessage : undefined} />}
          >
            <video
              ref={videoRef}
              style={{ objectFit: pictureFit }}
              autoPlay
              tabIndex={0}
              inert={!remoteMedia}
              aria-label={t("viewer.stageAria")}
              playsInline
              onPlay={() => {
                invalidateQualityPresentation();
                rearmCurrentFrameProof();
                const binding = remoteMediaRef.current;
                if (binding) {
                  dispatchPresentation({
                    type: "autoplay-cleared",
                    generation: binding.generation,
                  });
                }
              }}
              onPause={invalidateQualityPresentation}
              onEnded={invalidateQualityPresentation}
            />
            <PlaybackControls
              overlayAction={<RoomChatToggle session={interactionSession} />}
              overlaySettings={interactionSession && <RoomChatOverlaySettings session={interactionSession} />}
              remoteActions={<RemoteControl channel={inputRoute.current} videoRef={videoRef}
                  pictureFit={pictureFit} pictureRotation={pictureRotation} touchMode={remoteTouchMode}
                  available={presentation.overlay === "none" && presentation.hasCurrentFrame} />}
              settings={{ pictureFit, pictureRotation, setPictureRotation: rotation => {
                setPictureRotation(rotation); saveRoomPreferences({ pictureRotation: rotation });
              }, setPictureFit: fit => {
                setPictureFit(fit); saveRoomPreferences({ pictureFit: fit });
              },
                remoteTouch: inputRoute.current ? {
                  mode: remoteTouchMode,
                  onChange: mode => { setRemoteTouchMode(mode); saveRoomPreferences({ remoteTouchMode: mode }); },
                } : undefined,
                route: routeOptions ? {
                  preference: routePreference, sfuAvailable: routeOptions.sfu && !peerOnly,
                  sfuOnly: routeOptions.sfuOnly, peerOnly,
                  disabled: signalStatus !== "connected" || routePreferencePending,
                  rejected: routePreferenceRejected,
                  onChange: preference => {
                    if (signalRef.current?.setViewerRoutePreference(preference)) {
                      saveRoomPreferences({ routePreference: preference });
                      setRoutePreferencePending(true); setRoutePreferenceRejected(false);
                    }
                  },
                } : undefined,
                relayVolunteer: relayVolunteerSupported ? {
                  enabled: relayVolunteerEnabled, pending: relayVolunteerPending,
                  ...relayVolunteerLimits,
                  disabled: signalStatus !== "connected" || relayVolunteerPending || (!relayVolunteerLimits.available && !relayVolunteerEnabled),
                  onChange: enabled => requestRelayVolunteerRef.current?.(enabled),
                  onCapacityChange: capacity => requestRelayVolunteerRef.current?.(true, capacity),
                } : undefined,
              }}
              videoRef={videoRef}
              stream={remoteMedia?.stream ?? null}
              audioTrackKey={remoteMedia?.audioTrackKey}
              audioAvailable={audioAvailable}
              theaterMode={theaterMode}
              onToggleTheater={() => setTheaterMode((current) => !current)}
              onReconnect={retryConnection}
              reconnectAvailable={reconnectAvailable}
              canPlay={presentation.overlay === "none" ||
                presentation.stage === "needs-play" ||
                (presentation.stage === "receiving" && presentation.hasRetainedFrame)}
              onPlay={() => {
                const video = videoRef.current;
                const binding = remoteMediaRef.current;
                if (video && binding) attemptPlayback(video, binding);
              }}
            />
            <RoomChatOverlay session={interactionSession} visible={presentation.overlay === "none" && presentation.hasCurrentFrame} />
            {presentation.overlay === "blocking" && (
              <StageOverlay
                dim
                icon={viewerStatus.activity.icon}
                waiting={viewerStatus.overlay?.waiting}
                comic={viewerStatus.activity.comic}
                tone={viewerStatus.activity.tone}
                message={stageMessage}
                progress={connectionProgress}
              />
            )}
            {presentation.overlay === "status" &&
              presentation.stage === "needs-play" && (
                <StageOverlay
                  dim
                  icon={viewerStatus.activity.icon}
                  comic={viewerStatus.activity.comic}
                  tone={viewerStatus.activity.tone}
                  message={t(presentation.messageKey)}
                  onActivate={() => {
                    const video = videoRef.current;
                    const binding = remoteMediaRef.current;
                    if (video && binding) attemptPlayback(video, binding);
                  }}
                />
              )}
            {presentation.overlay === "status" &&
              presentation.stage !== "needs-play" && (
                <StageOverlay
                  dim
                  icon={viewerStatus.activity.icon}
                  comic={viewerStatus.activity.comic}
                  tone={viewerStatus.activity.tone}
                  message={stageMessage}
                  progress={connectionProgress}
                  waiting={viewerStatus.overlay?.waiting}
                />
              )}
          </StageTv>
          <div className="lr-stage-notices" role="status" aria-live="polite">
            {viewerStatus.notice && (
              <Pill
                icon={viewerStatus.notice.icon}
                label={t(viewerStatus.notice.labelKey)}
                comic={(viewerStatus.notice.comic ?? viewerStatus.notice.tooltip)!}
                tone={viewerStatus.notice.tone}
                motion={viewerStatus.notice.pulse ? "progress" : "still"}
              />
            )}
          </div>
          <RoomInteractions session={interactionSession}
            voiceState={roomVoice}
            voice={roomVoiceSession ? {
              content: onSelect => <RoomVoicePanel session={roomVoiceSession} onSelectParticipant={onSelect}
                names={new Map((participantPresence ?? []).map(person => [person.peerId, person.displayName]))} />,
              settings: <RoomVoiceSettings session={roomVoiceSession} />,
              controls: <RoomVoiceControls session={roomVoiceSession} />,
            } : undefined}
            view="viewer"
            host={
              labeledHostPresence
                ? {
                    key: labeledHostPresence.peerId,
                    name: labeledHostPresence.label,
                  }
                : null
            }
            entries={couchEntries}
            selectedKey={selectedPawn}
            onSelect={(key) =>
              setSelectedPawn((current) => (current === key ? null : key))
            }
          />
        </div>
        <div className="lr-deck">
          <div className="lr-row lr-viewer-summary-row">
            <div className="lr-row-group lr-viewer-room-slot">
              <FieldCap k="common.roomCode" />
              <Lcd code={roomId} />
            </div>
            <div
              className="lr-row-group lr-viewer-host-slot"
            >
              <Glyph name="tv" size={17} />
              <Tooltip overflow={{ text: hostDisplayName ?? t("common.host"), selector: "b" }} className="lr-name-hint">
                <b>{hostDisplayName ?? t("common.host")}</b>
              </Tooltip>
            </div>
            <div className="lr-viewer-personal-controls">
              <form
                className="lr-row-group lr-group-name lr-viewer-self-slot"
                onSubmit={(event) => {
                  event.preventDefault();
                  commitDisplayName();
                }}
              >
                {editingDisplayName ? (
                  <>
                    <span className="lr-input lr-name-editor">
                      <input
                        id="viewer-display-name"
                        type="text"
                        value={displayNameDraft}
                        maxLength={96}
                        autoComplete="nickname"
                        autoFocus
                        aria-label={t("host.name")}
                        aria-invalid={displayNameError ? "true" : undefined}
                        onChange={(event) => {
                          setDisplayNameDraft(event.target.value);
                          setDisplayNameError(false);
                        }}
                      />
                    </span>
                    <Btn
                      key="save-name"
                      icon="check"
                      title="host.nameSave"
                      hint="hint-rename"
                      type="submit"
                      disabled={displayNameDraft === displayName}
                    />
                    <Btn
                      icon="x"
                      title="host.nameCancel"
                      hint="hint-close"
                      onClick={() => {
                        setDisplayNameDraft(displayName);
                        setDisplayNameError(false);
                        setEditingDisplayName(false);
                      }}
                    />
                  </>
                ) : (
                  <>
                    <NameTag
                      name={displayName}
                      identity={selfPeerId ?? viewerClientId}
                    />
                    <Btn
                      key="edit-name"
                      icon="pencil"
                      cap="common.edit"
                      title="host.nameEdit"
                      hint="hint-rename"
                      onClick={() => {
                        setDisplayNameDraft(displayName);
                        setDisplayNameError(false);
                        setEditingDisplayName(true);
                      }}
                    />
                  </>
                )}
                {displayNameError && (
                  <Pill
                    icon="alert"
                    tone="bad"
                    label={t("host.nameError")}
                    alert
                    comic="name-invalid"
                  />
                )}
              </form>
              <div className="lr-row-group lr-group-actions lr-viewer-actions-slot">
                <span
                  className="lr-viewer-action-separator"
                  aria-hidden="true"
                />
                <span className="lr-viewer-action-cluster">
                  <Btn
                    icon="gauge"
                    cap="host.details"
                    title={
                      showConnectionDetails ? "host.details.hide" : "host.details.show"
                    }
                    hint={showConnectionDetails ? "hint-collapse" : "hint-details"}
                    tone={showConnectionDetails ? "on" : undefined}
                    expanded={showConnectionDetails}
                    controls="viewer-details-panel"
                    disabled={
                      !showConnectionDetails &&
                      !routePresentation.route &&
                      !ownVoiceConnection &&
                      !relaySnapshot &&
                      freshRelayChildEvidence.length === 0
                    }
                    onClick={() =>
                      setShowConnectionDetails((current) => !current)
                    }
                  />
                  <Btn
                    icon="network"
                    cap="host.topology"
                    title={
                      labeledHostPresence && showTopology
                        ? "host.topology.hide"
                        : "host.topology.show"
                    }
                    hint={labeledHostPresence && showTopology ? "hint-collapse" : "hint-topology"}
                    tone={
                      labeledHostPresence && showTopology ? "on" : undefined
                    }
                    expanded={Boolean(labeledHostPresence && showTopology)}
                    controls="room-topology"
                    disabled={!labeledHostPresence}
                    onClick={() =>
                      setShowTopology((current) => !current)
                    }
                  />
                </span>
              </div>
            </div>
          </div>
          {showConnectionDetails && (routePresentation.route || ownVoiceConnection || relaySnapshot || freshRelayChildEvidence.length > 0) ? (
            <Row sub>
              <ConnectionDetails id="viewer-details-panel" rows={[
                ...(routePresentation.route ? [{ key: "picture", media: "video" as const, label: t("stats.sharedMedia"),
                  icon: "arrowDown" as const, route: routePresentation.route, metrics: routeMetrics, direction: "receive" as const,
                }] : []),
                ...(relaySnapshot ? [{ key: "relay", media: "video" as const, label: t("stats.relay"),
                  icon: "arrowUp" as const, metrics: relaySnapshot.metrics, direction: "send" as const,
                }] : []),
                ...freshRelayChildEvidence.map(({ viewer, evidence }) => ({ key: `downstream/${viewer.peerId}`, media: "video" as const,
                  label: `${t("stats.downstream")} · ${viewer.label}`, icon: "arrowUp" as const,
                  metrics: metricsFromQualityEvidence(evidence), direction: "receive" as const, detailOnly: true,
                })),
                ...voiceConnectionDetails({ voice: roomVoiceSnapshot, metrics: voiceMetrics, t,
                  peerName: id => id === labeledHostPresence?.peerId ? labeledHostPresence.label : viewers.find(viewer => viewer.peerId === id)?.label ?? t("common.name.viewerDefault") }),
              ]} />
            </Row>
          ) : null}
          {selectedRelayChild ? (
            <PawnDetail
              pawnKey={selectedRelayChild.peerId}
              name={selectedRelayChild.label}
              route={null}
              metrics={selectedChildEvidence ? metricsFromQualityEvidence(selectedChildEvidence) : null}
              direction="receive"
              tag={selectedChildEvidence ? { icon: "arrowUp", label: t("stats.downstream") } : undefined}
              onClose={() => setSelectedPawn(null)}
              voiceDetails={voiceConnectionDetails({ voice: roomVoiceSnapshot, peerId: selectedRelayChild.peerId, metrics: voiceMetrics, t,
                peerName: id => id === labeledHostPresence?.peerId ? labeledHostPresence.label : viewers.find(viewer => viewer.peerId === id)?.label ?? t("common.name.viewerDefault") })}
            />
          ) : null}
          {showTopology && labeledHostPresence ? (
            <Row sub>
              <RouteTree
                hostPeerId={labeledHostPresence.peerId}
                pictureActive={pictureActive}
                voice={roomVoice}
                hostLabel={labeledHostPresence.label}
                viewers={viewers}
                selfPeerId={selfPeerId}
                selectedPeerId={selectedPawn}
                selectablePeerIds={selectableRelayChildren}
                onSelectPeer={(peerId) =>
                  setSelectedPawn((current) =>
                    current === peerId ? null : peerId,
                  )
                }
              />
            </Row>
          ) : null}
        </div>
      </main>
    </div>
  );
}
