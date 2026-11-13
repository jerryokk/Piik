// Page routes. A room code is the only variable segment; `/r/<code>` selects a
// Viewer page, and anything else under `/r` is a malformed invitation.
import { roomCodeSchema } from "../../../shared/protocol";
import { withBrowserDebug } from "../debug";
import { clearViewerGrant } from "./viewer-invite";
import type { AppRoute } from "./types";

export function isValidRoomId(value: string): boolean {
  return roomCodeSchema.safeParse(value).success;
}

export function roomRouteFromInput(value: string): string | null {
  return isValidRoomId(value) ? `/r/${value}` : null;
}

export function roomRouteForExplicitEntry(value: string): string | null {
  const route = roomRouteFromInput(value);
  if (route) {
    clearViewerGrant(value);
  }
  return route ? withBrowserDebug(route) : null;
}

export function parseAppRoute(pathname: string): AppRoute {
  if (/^\/client\/?$/.test(pathname)) {
    return { kind: "client" };
  }
  if (pathname === "/") {
    return { kind: "host" };
  }
  if (/^\/join\/?$/.test(pathname)) {
    return { kind: "join" };
  }
  const viewerMatch = pathname.match(/^\/r\/([1-9]\d{3})\/?$/);
  if (viewerMatch) {
    return { kind: "viewer", roomId: viewerMatch[1]! };
  }
  if (pathname === "/r" || pathname.startsWith("/r/")) {
    return { kind: "malformed-room" };
  }
  return { kind: "unknown" };
}
