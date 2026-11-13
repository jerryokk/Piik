//! Playback hooks for the Viewer page: they attach the remote stream to the
//! video element, mirror Host pause/resume, and prove a composited frame.

import { useEffect, type RefObject } from "react";
import type { RemoteMediaBinding } from "../../media/viewer-playback";
import { prepareViewerPlayback } from "../../media/viewer-playback";
import type { ViewerPresentationState } from "../../media/viewer-presentation";
import type { ViewerSfuRoute } from "../../media/viewer-sfu-route";
import { observeCompositedVideoFrame } from "../../media/video-frame-proof";
import type { InputRoute } from "../../remote-control/input-route";
import type { ViewerMediaPeer } from "../../webrtc/viewer-peer";
import type { ViewerSessionHelpers } from "./session-helpers";

interface ViewerPlaybackDeps {
  videoRef: RefObject<HTMLVideoElement | null>;
  remoteMedia: RemoteMediaBinding | null;
  presentationState: ViewerPresentationState;
  presentationStateRef: RefObject<ViewerPresentationState>;
  hostPlaybackPauseRef: RefObject<{ active: boolean; resume: boolean }>;
  qualityFrameProofGenerationRef: RefObject<number | null>;
  mediaBindingIsCurrent: ViewerSessionHelpers["mediaBindingIsCurrent"];
  attemptPlayback: ViewerSessionHelpers["attemptPlayback"];
  dispatchPresentation: ViewerSessionHelpers["dispatchPresentation"];
}

/**
 * Attaches the current binding, pauses with the Host and proves a composited
 * frame. Effect order matches the page's original inline effects.
 */
export function useViewerPlayback({
  videoRef,
  remoteMedia,
  presentationState,
  presentationStateRef,
  hostPlaybackPauseRef,
  qualityFrameProofGenerationRef,
  mediaBindingIsCurrent,
  attemptPlayback,
  dispatchPresentation,
}: ViewerPlaybackDeps): void {
  useEffect(() => {
    const video = videoRef.current;
    if (!video || !remoteMedia) {
      if (video) {
        video.srcObject = null;
      }
      return;
    }
    if (
      prepareViewerPlayback(
        video,
        remoteMedia.stream,
        presentationState.host === "paused",
        hostPlaybackPauseRef.current,
      )
    ) {
      attemptPlayback(video, remoteMedia);
    }
  }, [remoteMedia?.generation]);

  useEffect(() => {
    const paused = presentationState.host === "paused";
    const pauseState = hostPlaybackPauseRef.current;
    const video = videoRef.current;
    if (paused) {
      if (!pauseState.active) {
        pauseState.active = true;
        pauseState.resume = video && remoteMedia ? !video.paused : true;
      }
      video?.pause();
      return;
    }
    if (!pauseState.active) {
      return;
    }
    pauseState.active = false;
    const shouldResume = pauseState.resume;
    pauseState.resume = false;
    if (shouldResume && video && remoteMedia) {
      attemptPlayback(video, remoteMedia);
    }
  }, [presentationState.host, remoteMedia?.generation]);

  useEffect(() => {
    const video = videoRef.current;
    const mediaFact = presentationState.media;
    if (
      !video ||
      !remoteMedia ||
      !mediaFact ||
      mediaFact.generation !== remoteMedia.generation ||
      presentationState.host === "paused" ||
      document.visibilityState !== "visible"
    ) {
      return;
    }
    const binding = remoteMedia;
    const proofEpoch = mediaFact.proofEpoch;
    return observeCompositedVideoFrame(video, binding.stream, () => {
      const currentMedia = presentationStateRef.current.media;
      if (
        !mediaBindingIsCurrent(binding) ||
        currentMedia?.generation !== binding.generation ||
        currentMedia.proofEpoch !== proofEpoch
      ) {
        return;
      }
      qualityFrameProofGenerationRef.current = binding.generation;
      dispatchPresentation({
        type: "frame-presented",
        generation: binding.generation,
        proofEpoch,
        revision: binding.boundAtRevision,
      });
    });
  }, [
    presentationState.media?.proofEpoch,
    presentationState.media?.generation,
    presentationState.host,
    remoteMedia?.generation,
  ]);
}

/** Keeps the remote-input route attached to the current media upstream. */
export function useViewerInputUpstream(
  inputRoute: InputRoute,
  peerRef: RefObject<ViewerMediaPeer | null>,
  viewerSfuRouteRef: RefObject<ViewerSfuRoute | null>,
): void {
  useEffect(() => {
    inputRoute.setUpstream(peerRef.current?.getRemoteInput?.() ?? viewerSfuRouteRef.current?.getRemoteInput() ?? null);
  });
}
