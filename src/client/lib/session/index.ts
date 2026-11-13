// Browser page state: launch activation, routes, Host room ownership, Viewer
// invitation grants and the per-tab client id. Each concern has its own module;
// this index is the single import path for callers.
export {
  activateClient,
  clientLaunchURL,
  clientRuntimePort,
  isClientActivated,
  takeClientLaunchBootstrap,
  watchClientActivation,
  type ClientLaunchPresentation,
} from "./client-activation";
export { isValidRoomId, parseAppRoute, roomRouteForExplicitEntry, roomRouteFromInput } from "./routes";
export {
  clearHostRoom,
  readHostRoom,
  readPreferredRoomId,
  releaseHostRoom,
  writeHostRoom,
  writePreferredRoom,
  type HostRoomIdentity,
  type HostRoomState,
} from "./host-room";
export {
  clearViewerGrant,
  mergeAuthenticatedHostRoom,
  readViewerGrant,
  readViewerRoute,
  replaceViewerInvite,
  watchViewerInvites,
} from "./viewer-invite";
export { getStableClientId } from "./client-id";
export type { AppRoute, ViewerRoute } from "./types";
