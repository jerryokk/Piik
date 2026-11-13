import type { CaptureEvidence, MediaEvidence } from "./app-media-gate-types";

export interface GateCleanup {
  browserExited: boolean;
  nativeExited: boolean;
  serverClosed: boolean;
  portsClosed: boolean;
  profileRemoved: boolean;
}

/** Both capture paths and both Browser receivers must show live media. */
export function mediaGatePassed(input: {
  error: string | null;
  capture: CaptureEvidence;
  media: MediaEvidence;
  cleanup: GateCleanup;
}): boolean {
  const { error, capture: evidence, media, cleanup } = input;
  return error === null && evidence.starting && evidence.active &&
    evidence.frames >= 30 && evidence.recoveryFrame && media.error === null &&
    (evidence.audioFrames === 0 || evidence.audioNonZeroSamples > 0) &&
    media.captureActive && media.connected && media.frames >= 30 &&
    media.connectedEdges === 2 && media.edgeFrames.length === 2 &&
    (!media.audioAvailable ||
      (media.audioPackets > 10 && media.audioEnergy > 0)) &&
    media.liveProfileUpdated && media.pausedProfileUpdated &&
    media.width > 0 && media.width <= 854 && media.height > 0 && media.height <= 480 &&
    media.nativeCandidateTypes.includes("srflx") &&
    cleanup.browserExited && cleanup.nativeExited && cleanup.serverClosed &&
    cleanup.portsClosed && cleanup.profileRemoved;
}
