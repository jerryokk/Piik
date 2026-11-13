//! Component-level Viewer session helpers: presentation dispatch, media
//! binding, frame-proof invalidation, route acceptance and presence clearing.
//! They close only over refs and setters supplied per render by the page.

import type { Dispatch, RefObject, SetStateAction } from "react";
import type {
  ParticipantPresenceEntry,
  ParticipantRouteAssignment,
  PreparedRouteCandidate,
} from "../../../shared/protocol";
import {
  reduceViewerPresentation,
  type ViewerPresentationAction,
  type ViewerPresentationState,
  type ViewerRouteKind,
} from "../../media/viewer-presentation";
import {
  nextViewerMediaBinding,
  playbackFailure,
  type RemoteMediaBinding,
} from "../../media/viewer-playback";
import type { ViewerQualityEvidenceReporter } from "../../media/viewer-quality-evidence";

interface ViewerSessionHelpersDeps {
  presentationStateRef: RefObject<ViewerPresentationState>;
  dispatchPresentationState: Dispatch<ViewerPresentationAction>;
  remoteMediaRef: RefObject<RemoteMediaBinding | null>;
  mediaGenerationRef: RefObject<number>;
  setRemoteMedia: Dispatch<SetStateAction<RemoteMediaBinding | null>>;
  qualityEvidenceReporterRef: RefObject<ViewerQualityEvidenceReporter | null>;
  qualityFrameProofGenerationRef: RefObject<number | null>;
  setAssignedRoute: Dispatch<
    SetStateAction<{
      revision: number;
      phase: "prepare" | "active";
      upstream: ParticipantRouteAssignment["upstream"];
      connectionAttempt?: PreparedRouteCandidate["connectionAttempt"];
    } | null>
  >;
  setParticipantPresence: Dispatch<
    SetStateAction<ParticipantPresenceEntry[] | null>
  >;
  setLastHostDisplayName: Dispatch<SetStateAction<string | null>>;
}

export interface ViewerSessionHelpers {
  dispatchPresentation(action: ViewerPresentationAction): void;
  bindRemoteStream(stream: MediaStream, revision: number): void;
  clearRemoteMedia(): void;
  resetCurrentFrameProof(): void;
  rearmCurrentFrameProof(): void;
  invalidateQualityPresentation(): void;
  invalidatePresentedMedia(): void;
  mediaBindingIsCurrent(binding: RemoteMediaBinding): boolean;
  attemptPlayback(video: HTMLVideoElement, binding: RemoteMediaBinding): void;
  acceptAssignedRoute(
    revision: number,
    upstream: ParticipantRouteAssignment["upstream"],
    phase?: "prepare" | "active",
    connectionAttempt?: PreparedRouteCandidate["connectionAttempt"],
    authoritative?: boolean,
  ): void;
  clearParticipantPresence(): void;
  clearHostPresence(forgetDisplayName?: boolean): void;
}

function routeKindFromAssignment(
  upstream: ParticipantRouteAssignment["upstream"],
): ViewerRouteKind {
  return upstream.kind === "peer"
    ? "p2p"
    : upstream.kind === "sfu"
      ? "sfu"
      : "none";
}

export function connectionFact(
  state: RTCPeerConnectionState,
): "idle" | "connecting" | "connected" | "reconnecting" | "failed" {
  switch (state) {
    case "connected":
      return "connected";
    case "new":
    case "connecting":
      return "connecting";
    case "disconnected":
      return "reconnecting";
    case "failed":
    case "closed":
      return "failed";
  }
}

export function createViewerSessionHelpers(
  deps: ViewerSessionHelpersDeps,
): ViewerSessionHelpers {
  const {
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
  } = deps;

  function dispatchPresentation(action: ViewerPresentationAction): void {
    presentationStateRef.current = reduceViewerPresentation(
      presentationStateRef.current,
      action,
    );
    dispatchPresentationState(action);
  }

  function clearParticipantPresence(): void {
    setParticipantPresence(null);
    setLastHostDisplayName(null);
  }

  function clearHostPresence(forgetDisplayName = true): void {
    setParticipantPresence((current) =>
      current?.filter((participant) => participant.role !== "host") ?? null,
    );
    if (forgetDisplayName) {
      setLastHostDisplayName(null);
    }
  }

  function invalidateQualityPresentation(): void {
    qualityEvidenceReporterRef.current?.invalidatePresentation();
    qualityFrameProofGenerationRef.current = null;
  }

  function resetCurrentFrameProof(): void {
    const binding = remoteMediaRef.current;
    if (!binding) {
      return;
    }
    dispatchPresentation({
      type: "frame-proof-reset",
      generation: binding.generation,
      revision: binding.boundAtRevision,
    });
  }

  function rearmCurrentFrameProof(): void {
    const binding = remoteMediaRef.current;
    if (!binding) {
      return;
    }
    dispatchPresentation({
      type: "frame-proof-rearm",
      generation: binding.generation,
    });
  }

  function invalidatePresentedMedia(): void {
    invalidateQualityPresentation();
    resetCurrentFrameProof();
  }

  function mediaBindingIsCurrent(binding: RemoteMediaBinding): boolean {
    const current = remoteMediaRef.current;
    return (
      current?.generation === binding.generation &&
      current.stream === binding.stream
    );
  }

  function bindRemoteStream(stream: MediaStream, revision: number): void {
    const current = remoteMediaRef.current;
    const next = nextViewerMediaBinding(current, stream, revision, mediaGenerationRef.current + 1);
    if (next === current) return;
    const pictureChanged = next.generation !== current?.generation;
    mediaGenerationRef.current = next.generation;
    remoteMediaRef.current = next;
    setRemoteMedia(next);
    if (!pictureChanged) return;
    invalidateQualityPresentation();
    dispatchPresentation({
      type: "media-bound",
      generation: next.generation,
      revision: next.boundAtRevision,
    });
  }

  function clearRemoteMedia(): void {
    invalidateQualityPresentation();
    remoteMediaRef.current = null;
    setRemoteMedia(null);
    dispatchPresentation({ type: "media-cleared" });
  }

  function attemptPlayback(
    video: HTMLVideoElement,
    binding: RemoteMediaBinding,
  ): void {
    void video.play().then(
      () => {
        if (!mediaBindingIsCurrent(binding)) {
          return;
        }
        dispatchPresentation({
          type: "autoplay-cleared",
          generation: binding.generation,
        });
      },
      (error: unknown) => {
        const failure = playbackFailure(error);
        if (!mediaBindingIsCurrent(binding) || !failure) {
          return;
        }
        invalidatePresentedMedia();
        dispatchPresentation({
          type: failure,
          generation: binding.generation,
          revision: binding.boundAtRevision,
        });
      },
    );
  }

  function acceptAssignedRoute(
    revision: number,
    upstream: ParticipantRouteAssignment["upstream"],
    phase: "prepare" | "active" = "active",
    connectionAttempt?: PreparedRouteCandidate["connectionAttempt"],
    authoritative = false,
  ): void {
    setAssignedRoute((current) =>
      !authoritative && current && revision < current.revision
        ? current
        : { revision, phase, upstream, connectionAttempt },
    );
    dispatchPresentation({
      type: "route",
      revision,
      phase,
      kind: routeKindFromAssignment(upstream),
      authoritative,
    });
  }

  return {
    dispatchPresentation,
    bindRemoteStream,
    clearRemoteMedia,
    resetCurrentFrameProof,
    rearmCurrentFrameProof,
    invalidateQualityPresentation,
    invalidatePresentedMedia,
    mediaBindingIsCurrent,
    attemptPlayback,
    acceptAssignedRoute,
    clearParticipantPresence,
    clearHostPresence,
  };
}
