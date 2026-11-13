// Viewer invitation grants. A `#v=` fragment is the only invite credential;
// any other fragment belongs to the mail client or a scroll target and must not
// revoke the grant this tab already holds.
import { viewerGrantSchema, type CodeEntryPolicy } from "../../../shared/protocol";
import { withBrowserDebug } from "../debug";
import { parseAppRoute } from "./routes";
import { readSessionValue, writeSessionValue } from "./storage";
import type { HostRoomState } from "./host-room";
import type { ViewerRoute } from "./types";

export function readViewerRoute(): ViewerRoute | null {
  const route = parseAppRoute(window.location.pathname);
  if (route.kind !== "viewer") {
    return null;
  }

  const roomId = route.roomId;
  const fragmentMatch = window.location.hash.match(/^#v=(.*)$/);
  if (fragmentMatch) {
    const viewerGrant = fragmentMatch[1];
    const validGrant = isValidViewerGrant(viewerGrant) ? viewerGrant : null;
    if (validGrant) {
      writeSessionValue(viewerGrantStorageKey(roomId), validGrant);
    } else {
      clearViewerGrant(roomId);
    }
    window.history.replaceState(window.history.state, "", withBrowserDebug(`/r/${roomId}`));
    return validGrant
      ? { roomId, viewerGrant: validGrant }
      : { roomId, invalidGrant: true };
  }

  const storedGrant = readSessionValue(viewerGrantStorageKey(roomId));
  if (storedGrant && isValidViewerGrant(storedGrant)) {
    return { roomId, viewerGrant: storedGrant };
  }
  if (storedGrant) {
    clearViewerGrant(roomId);
  }
  return { roomId };
}

// Following another invitation to this page is a fragment navigation, not a
// document load. Consume it through the same authority owner as initial entry.
export function watchViewerInvites(changed: (route: ViewerRoute) => void): () => void {
  const onHashChange = () => {
    if (!window.location.hash.startsWith("#v=")) return;
    const route = readViewerRoute();
    if (route) changed(route);
  };
  window.addEventListener("hashchange", onHashChange);
  return () => window.removeEventListener("hashchange", onHashChange);
}

export function replaceViewerInvite(
  roomId: string,
  inviteUrl: string | null,
): void {
  if (inviteUrl === null) {
    clearViewerGrant(roomId);
    return;
  }
  let parsed: URL;
  try {
    parsed = new URL(inviteUrl);
  } catch {
    clearViewerGrant(roomId);
    return;
  }
  const inviteRoute = parseAppRoute(parsed.pathname);
  const match = parsed.hash.match(/^#v=(.+)$/);
  const viewerGrant = match?.[1];
  if (
    inviteRoute.kind === "viewer" &&
    inviteRoute.roomId === roomId &&
    viewerGrant &&
    isValidViewerGrant(viewerGrant)
  ) {
    writeSessionValue(viewerGrantStorageKey(roomId), viewerGrant);
    return;
  }
  clearViewerGrant(roomId);
}

export function readViewerGrant(roomId: string): string | null {
  const value = readSessionValue(viewerGrantStorageKey(roomId));
  if (value && isValidViewerGrant(value)) {
    return value;
  }
  if (value) {
    clearViewerGrant(roomId);
  }
  return null;
}

export function mergeAuthenticatedHostRoom(
  current: HostRoomState | null,
  activeRoomId: string,
  codeEntryPolicy: CodeEntryPolicy,
): HostRoomState | null {
  if (!current || current.roomId !== activeRoomId) {
    return current;
  }

  let inviteUrl = current.inviteUrl;
  if (inviteUrl === null) {
    const viewerGrant = readViewerGrant(current.roomId);
    if (viewerGrant) {
      const restoredInvite = new URL(current.canonicalUrl);
      restoredInvite.hash = `v=${viewerGrant}`;
      inviteUrl = restoredInvite.toString();
    }
  }

  return {
    ...current,
    codeEntryPolicy,
    inviteUrl,
  };
}

export function clearViewerGrant(roomId: string): void {
  try {
    window.sessionStorage.removeItem(viewerGrantStorageKey(roomId));
  } catch {
    // Restricted storage does not change the current in-memory authorization.
  }
}

function viewerGrantStorageKey(roomId: string): string {
  return `piik:viewer-grant:${roomId}`;
}

function isValidViewerGrant(value: string): boolean {
  return viewerGrantSchema.safeParse(value).success;
}
