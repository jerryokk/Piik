// Host room ownership. One tab at a time holds a room via the Web Locks API;
// the stored identity is a convenience for reload and resumption, never proof
// of authority.
import { createRoomResponseSchema, roomCodeSchema, type CodeEntryPolicy, type CreateRoomResponse } from "../../../shared/protocol";
import { z } from "zod";
import { inAppShell } from "../app-shell";

const HOST_ROOM_STORAGE_KEY = "piik:host-room:v1";
const HOST_ROOM_PREFERENCE_STORAGE_KEY = "piik:host-room-preference:v1";
const hostRoomStorageSchema = createRoomResponseSchema.pick({
  roomId: true,
  hostToken: true,
  inviteUrl: true,
}).strip();
const hostRoomPreferenceSchema = z
  .object({
    roomId: roomCodeSchema,
  })
  .strict();

export type HostRoomIdentity = Pick<
  CreateRoomResponse,
  "roomId" | "hostToken"
> & { canonicalUrl: string };

export interface HostRoomState extends HostRoomIdentity {
  codeEntryPolicy: CodeEntryPolicy | null;
  inviteUrl: string | null;
}

interface HostRoomClaim {
  room: HostRoomIdentity;
  ready: Promise<boolean>;
  done: Promise<void>;
  release: () => void;
}

let hostRoomClaim: HostRoomClaim | null = null;
let hostRoomRelease: Promise<void> = Promise.resolve();

export function clearHostRoom(keepResumeHint = false): void {
  const room = readStoredHostRoom("sessionStorage") ?? hostRoomClaim?.room;
  if (room) {
    removeStoredHostRoom("sessionStorage", room);
    if (!keepResumeHint) removeStoredHostRoom("localStorage", room);
  }
  releaseHostRoom();
}

export function releaseHostRoom(): void {
  if (hostRoomClaim) {
    hostRoomClaim.release();
    hostRoomRelease = hostRoomClaim.done;
  }
  hostRoomClaim = null;
}

export async function readHostRoom(): Promise<HostRoomIdentity | null> {
  const tabRoom = readStoredHostRoom("sessionStorage") ?? hostRoomClaim?.room;
  // In the App the room cannot survive the window, so a localStorage copy is
  // never restorable here; reading one would resurrect a dead room.
  const sharedRoom =
    inAppShell() || !hostRoomLocks() ? null : readStoredHostRoom("localStorage");
  const room = tabRoom ?? sharedRoom;
  if (!room) return null;
  const claim = claimHostRoom(room, Boolean(tabRoom));
  const acquired = await claim.ready;
  if (hostRoomClaim !== claim) return null;
  if (!acquired) {
    releaseHostRoom();
    removeStoredHostRoom("sessionStorage", room);
    return null;
  }
  storeHostRoom("sessionStorage", room);
  return room;
}

function readStoredHostRoom(
  storage: "localStorage" | "sessionStorage",
): HostRoomIdentity | null {
  let stored: string | null;
  try {
    stored = window[storage].getItem(HOST_ROOM_STORAGE_KEY);
  } catch {
    return null;
  }
  if (!stored) {
    return null;
  }

  try {
    const parsed = hostRoomStorageSchema.safeParse(JSON.parse(stored));
    if (!parsed.success) {
      window[storage].removeItem(HOST_ROOM_STORAGE_KEY);
      return null;
    }
    const room: HostRoomIdentity = {
      roomId: parsed.data.roomId,
      hostToken: parsed.data.hostToken,
      canonicalUrl: canonicalViewerUrl(parsed.data.inviteUrl),
    };
    return room;
  } catch {
    try {
      window[storage].removeItem(HOST_ROOM_STORAGE_KEY);
    } catch {
      /* Storage is optional. */
    }
    return null;
  }
}

function sameHostRoom(
  left: HostRoomIdentity | null,
  right: HostRoomIdentity,
): boolean {
  return left?.roomId === right.roomId && left.hostToken === right.hostToken;
}

function removeStoredHostRoom(
  storage: "localStorage" | "sessionStorage",
  room: HostRoomIdentity,
): void {
  if (!sameHostRoom(readStoredHostRoom(storage), room)) return;
  try {
    window[storage].removeItem(HOST_ROOM_STORAGE_KEY);
  } catch {
    /* Storage is optional. */
  }
}

function hostRoomLocks(): LockManager | undefined {
  try {
    return window.navigator?.locks;
  } catch {
    return undefined;
  }
}

function claimHostRoom(
  room: HostRoomIdentity,
  allowWithoutLocks: boolean,
): HostRoomClaim {
  let claim = hostRoomClaim;
  if (!claim || !sameHostRoom(claim.room, room)) {
    releaseHostRoom();
    const previousRelease = hostRoomRelease;
    let release!: () => void;
    let resolveReady!: (ready: boolean) => void;
    const lifetime = new Promise<void>((resolve) => {
      release = resolve;
    });
    claim = {
      room,
      release,
      done: Promise.resolve(),
      ready: new Promise<boolean>((resolve) => {
        resolveReady = resolve;
      }),
    };
    hostRoomClaim = claim;
    const locks = hostRoomLocks();
    const requestedClaim = claim;
    claim.done = (async () => {
      await previousRelease;
      if (hostRoomClaim !== requestedClaim) {
        resolveReady(false);
        return;
      }
      if (!locks) {
        resolveReady(allowWithoutLocks);
        return;
      }
      // The lock follows the room incarnation without exposing the Host token
      // in queryable lock names. The browser releases it when this document ends.
      const digest = await crypto.subtle.digest(
        "SHA-256",
        new TextEncoder().encode(room.hostToken),
      );
      if (hostRoomClaim !== requestedClaim) {
        resolveReady(false);
        return;
      }
      const identity = Array.from(new Uint8Array(digest), (byte) =>
        byte.toString(16).padStart(2, "0"),
      ).join("");
      await locks.request(
        `piik:host-room:${room.roomId}:${identity}`,
        { ifAvailable: true },
        async (lock) => {
          resolveReady(lock !== null);
          if (lock) await lifetime;
        },
      );
    })().catch(() => resolveReady(allowWithoutLocks));
  }
  return claim;
}

export function readPreferredRoomId(): string | null {
  let stored: string | null;
  try {
    stored = window.localStorage.getItem(HOST_ROOM_PREFERENCE_STORAGE_KEY);
  } catch {
    return null;
  }
  if (!stored) {
    return null;
  }
  try {
    const parsed = hostRoomPreferenceSchema.safeParse(JSON.parse(stored));
    if (!parsed.success) {
      clearPreferredRoom();
      return null;
    }
    return parsed.data.roomId;
  } catch {
    clearPreferredRoom();
    return null;
  }
}

export function writePreferredRoom(roomId: string): void {
  if (!roomCodeSchema.safeParse(roomId).success) {
    return;
  }
  try {
    window.localStorage.setItem(
      HOST_ROOM_PREFERENCE_STORAGE_KEY,
      JSON.stringify({ roomId }),
    );
  } catch {
    // A storage failure only disables best-effort code reuse.
  }
}

function clearPreferredRoom(): void {
  try {
    window.localStorage.removeItem(HOST_ROOM_PREFERENCE_STORAGE_KEY);
  } catch {
    // Restricted storage is equivalent to having no local preference.
  }
}

export async function writeHostRoom(
  room: HostRoomIdentity | CreateRoomResponse,
  owns: () => boolean = () => true,
): Promise<boolean> {
  if (!owns()) return false;
  const identity: HostRoomIdentity = {
    roomId: room.roomId,
    hostToken: room.hostToken,
    canonicalUrl:
      "canonicalUrl" in room
        ? room.canonicalUrl
        : canonicalViewerUrl(room.inviteUrl),
  };
  const previousClaim = hostRoomClaim;
  const claim = claimHostRoom(identity, true);
  const acquired = await claim.ready;
  if (hostRoomClaim !== claim) return false;
  if (!acquired || !owns()) {
    if (!acquired || claim !== previousClaim) releaseHostRoom();
    return false;
  }
  claim.room = identity;
  storeHostRoom("sessionStorage", identity);
  // The App's room authority is in-process memory, so its rooms cannot outlive
  // the App window. Keeping a copy in localStorage would make the next launch
  // restore a room the server has never heard of and report it as expired; the
  // session store already matches that lifetime. A Browser page keeps the
  // localStorage copy because a Hosted room does survive a reload.
  if (!inAppShell()) {
    storeHostRoom("localStorage", identity);
  }
  return true;
}

function storeHostRoom(
  storage: "localStorage" | "sessionStorage",
  room: HostRoomIdentity,
): void {
  try {
    window[storage].setItem(
      HOST_ROOM_STORAGE_KEY,
      JSON.stringify({
        roomId: room.roomId,
        hostToken: room.hostToken,
        inviteUrl: room.canonicalUrl,
      }),
    );
  } catch {
    // A storage failure must not prevent the current sharing session.
  }
}

function canonicalViewerUrl(inviteUrl: string): string {
  const parsed = new URL(inviteUrl);
  parsed.hash = "";
  parsed.search = "";
  return parsed.toString();
}
