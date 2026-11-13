//! Types and helpers HostPage uses before its component body: notice payloads,
//! room identity conversion and capture facts. Pure, no React state.

import {
  DEFAULT_ROUTE_POLICY,
  type CreateRoomResponse,
  type ServerMessage,
} from "../../../shared/protocol";
import type { ComicKind } from "../../ui/components/comic/Comic";
import type { ComicTone } from "../../ui/components/comic/comic-presentation";
import type { HintKind } from "../../ui/components/hints";
import { SignalingClient, type SignalingTerminationReason } from "../../lib/signaling";
import { getStableClientId } from "../../lib/session";
import { replaceViewerInvite, readViewerGrant, type HostRoomIdentity, type HostRoomState } from "../../lib/session";
import { createOpaqueId } from "../../lib/util/opaque-id";
import type { CopyKey } from "../../ui/copy";
import { ApiError } from "../../lib/api";
import {
  hostActionErrorKey,
  type HostAction,
} from "./notices";

export type NoticeValue = (
  | { kind: "text"; text: string }
  | { kind: "key"; key: CopyKey; vars?: Record<string, string> }
) & {
  target: "television" | "operation";
  comic: ComicKind | HintKind;
  tone: ComicTone;
  failureCode?: string;
};

export type StatusNoticeMessage = string | { key: CopyKey; vars?: Record<string, string>; failureCode?: string };
export type SourceSwitch = { replacingVideo?: MediaStreamTrack; audioOnly?: boolean; nativePending?: boolean };

export type ViewerQualityEvidence = Extract<
  ServerMessage,
  { type: "viewer-quality-evidence" }
>;
export type HostRouteAssignment = Extract<
  ServerMessage,
  { type: "route-update" }
>["assignment"];

export interface CaptureDetails {
  /** null = source reported no usable width/height; mapped to a localized
   *  "unknown" (or the vis dash) at render time so language switches and vis
   *  mode never see a baked-in string from capture time. */
  resolution: string | null;
  frameRate: number | null;
  hasSourceAudio: boolean;
}

export function captureDetails(stream: MediaStream, native = false, sourceAudio?: boolean): CaptureDetails {
  // A Native preview is a received track, not an observation of raw capture.
  const settings = native ? undefined : stream.getVideoTracks()[0]?.getSettings();
  return {
    resolution:
      settings?.width && settings.height
        ? `${settings.width}x${settings.height}`
        : null,
    frameRate: settings?.frameRate ?? null,
    hasSourceAudio: sourceAudio ?? stream.getAudioTracks().some(track => track.readyState === "live"),
  };
}

export function readableError(error: unknown, action: HostAction): StatusNoticeMessage {
  return error instanceof ApiError
    ? error.message
    : { key: hostActionErrorKey(error, action) };
}

export function closeAbandonedRoom(room: HostRoomIdentity): void {
  try {
    const signal = new SignalingClient(
      {
        roomId: room.roomId,
        role: "host",
        token: room.hostToken,
        clientId: getStableClientId("host", room.roomId),
        shareGeneration: createOpaqueId(),
        routePolicy: DEFAULT_ROUTE_POLICY,
      },
      {
        onMessage: () => undefined,
        onStatus: () => undefined,
        onTerminated: () => undefined,
        onAccessRequired: () => undefined,
      },
    );
    signal.start();
    signal.sendThenStop({ type: "abandon-room" });
  } catch {
    // Best effort only: cancellation must not revive the obsolete generation.
  }
}

export function hostRoomFromStored(room: HostRoomIdentity | null): HostRoomState | null {
  if (!room) {
    return null;
  }
  const viewerGrant = readViewerGrant(room.roomId);
  const inviteUrl = new URL(room.canonicalUrl);
  if (viewerGrant) {
    inviteUrl.hash = `v=${viewerGrant}`;
  }
  return {
    ...room,
    codeEntryPolicy: null,
    inviteUrl: viewerGrant ? inviteUrl.toString() : null,
  };
}

export function hostRoomFromCreated(room: CreateRoomResponse): HostRoomState {
  replaceViewerInvite(room.roomId, room.inviteUrl);
  const canonicalUrl = new URL(room.inviteUrl);
  canonicalUrl.hash = "";
  canonicalUrl.search = "";
  return {
    roomId: room.roomId,
    hostToken: room.hostToken,
    canonicalUrl: canonicalUrl.toString(),
    codeEntryPolicy: room.codeEntryPolicy,
    inviteUrl: room.inviteUrl,
  };
}

export function hostTerminationKey(reason: SignalingTerminationReason): CopyKey {
  switch (reason) {
    case "STALE_CLIENT":
      return "host.terminated.stale";
    case "SESSION_REPLACED":
      return "host.terminated.session";
    case "SIGNAL_TERMINATED":
      return "host.terminated.signal";
  }
}

