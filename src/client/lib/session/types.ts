// Route shapes shared by route parsing and invitation handling.

export interface ViewerRoute {
  roomId: string;
  viewerGrant?: string;
  /** An invite fragment was present but its grant was unusable. */
  invalidGrant?: true;
}

export type AppRoute =
  | { kind: "client" }
  | { kind: "host" }
  | { kind: "join" }
  | { kind: "viewer"; roomId: string }
  | { kind: "malformed-room" }
  | { kind: "unknown" };
