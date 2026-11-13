//! Derived view model for the Viewer room stage and deck: retry copy, fresh
//! relay-child evidence, couch entries and selectable relay children.

import type { RoomVoiceState } from "../../../shared/protocol";
import type { CouchEntry } from "../../ui/components/people/Couch";
import { participantConnection } from "../../lib/room/participant-connection";
import {
  freshViewerQualityEvidence,
  type ViewerQualityEvidencePresentation,
} from "../../media/viewer-quality-evidence";
import type {
  ViewerMessageKey,
  ViewerPresentationState,
} from "../../media/viewer-presentation";
import { labelParticipantSnapshot } from "../../lib/room/viewer-presence";
import { deriveParticipantStatus } from "../../ui/media-status";
import type { CopyKey } from "../../ui/copy";
import type { ViewerAssignedRoute, ViewerQualityEvidence } from "./types";

interface ViewerAttemptCopy {
  retryAttempt?: { current: number; total: number };
  stageMessage: string;
  connectionProgress: string | undefined;
}

export function viewerAttemptCopy(
  presentation: {
    stage: string;
    messageKey: ViewerMessageKey;
  },
  revision: number | null,
  assignedRoute: ViewerAssignedRoute | null,
  t: (key: CopyKey, vars?: Record<string, string>) => string,
): ViewerAttemptCopy {
  const connectionAttempt = presentation.stage === "preparing-p2p" &&
    assignedRoute?.phase === "prepare" &&
    assignedRoute.revision === revision
      ? assignedRoute.connectionAttempt
      : undefined;
  // Keep the first candidate quiet; the counter describes a retry, not normal setup.
  const retryAttempt =
    connectionAttempt && connectionAttempt.current > 1
      ? {
        ...connectionAttempt,
        current: connectionAttempt.current - 1,
        total: Math.max(1, connectionAttempt.total - 1),
        }
      : undefined;
  const stageMessage = retryAttempt
    ? t("viewer.msg.connectionRetry", {
        current: String(retryAttempt.current),
        total: String(retryAttempt.total),
      })
    : t(presentation.messageKey);
  const connectionProgress = retryAttempt
    ? `${retryAttempt.current}/${retryAttempt.total}`
    : undefined;
  return { retryAttempt, stageMessage, connectionProgress };
}

interface FreshRelayChildEntry {
  viewer: ReturnType<typeof labelParticipantSnapshot>["viewers"][number];
  evidence: ViewerQualityEvidence;
}

interface ViewerDeckModel {
  selectedChildEvidence: ViewerQualityEvidence | null;
  freshRelayChildEvidence: FreshRelayChildEntry[];
  pictureActive: boolean;
  couchEntries: CouchEntry[];
  selectableRelayChildren: string[];
  selectedRelayChild: FreshRelayChildEntry["viewer"] | undefined;
}

export function viewerDeckModel({
  viewers,
  relayChildEvidence,
  selectedPawn,
  selfPeerId,
  coPlayParticipants,
  host,
  roomVoice,
}: {
  viewers: ReturnType<typeof labelParticipantSnapshot>["viewers"];
  relayChildEvidence: ReadonlyMap<string, ViewerQualityEvidencePresentation>;
  selectedPawn: string | null;
  selfPeerId: string | null;
  coPlayParticipants: readonly { peerId: string }[];
  host: ViewerPresentationState["host"];
  roomVoice: RoomVoiceState | null;
}): ViewerDeckModel {
  const selectedChildEvidence =
    selectedPawn !== null && selectedPawn !== selfPeerId
      ? freshViewerQualityEvidence(relayChildEvidence.get(selectedPawn))
      : null;
  // Downstream rows describe one named child each; expired evidence stops
  // rendering instead of freezing the last numbers on screen.
  const freshRelayChildEvidence = viewers
    .map((viewer) => ({
      viewer,
      evidence: freshViewerQualityEvidence(
        relayChildEvidence.get(viewer.peerId),
      ),
    }))
    .filter(
      (entry): entry is FreshRelayChildEntry => entry.evidence !== null,
    );
  const pictureActive = host === "online" || host === "paused";
  const couchEntries: CouchEntry[] = viewers.map((viewer) => {
    const isSelf = selfPeerId !== null && viewer.peerId === selfPeerId;
    const upstream = participantConnection(viewer, pictureActive, roomVoice).upstream;
    const isChild =
      !isSelf &&
      selfPeerId !== null &&
      upstream.kind === "peer" &&
      upstream.peerId === selfPeerId;
    return {
      key: viewer.peerId,
      name: viewer.label,
      status: deriveParticipantStatus(viewer, pictureActive, undefined, roomVoice),
      collaboration: coPlayParticipants.some(participant => participant.peerId === viewer.peerId),
      you: isSelf,
      selectable: isChild ? undefined : false,
    };
  });
  const selectableRelayChildren = viewers
    .filter(
      (viewer) => {
        const upstream = participantConnection(viewer, pictureActive, roomVoice).upstream;
        return selfPeerId !== null && upstream.kind === "peer" && upstream.peerId === selfPeerId;
      },
    )
    .map((viewer) => viewer.peerId);
  const selectedRelayChild = viewers.find(viewer => viewer.peerId === selectedPawn && selectableRelayChildren.includes(viewer.peerId));
  return {
    selectedChildEvidence,
    freshRelayChildEvidence,
    pictureActive,
    couchEntries,
    selectableRelayChildren,
    selectedRelayChild,
  };
}
