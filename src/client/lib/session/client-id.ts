// Stable per-tab client identity. The value is an opaque per-room tag for
// signaling; it carries no authority and is regenerated when the stored value
// is missing or malformed.
import { createOpaqueId } from "../util/opaque-id";
import { readSessionValue, writeSessionValue } from "./storage";

const CLIENT_ID_PATTERN = /^[A-Za-z0-9_-]{8,128}$/;

export function getStableClientId(role: "host" | "viewer", roomId: string): string {
  const storageKey = `piik:client-id:${role}:${roomId}`;
  const existing = readSessionValue(storageKey);
  if (existing && CLIENT_ID_PATTERN.test(existing)) {
    return existing;
  }

  const clientId = createOpaqueId();
  writeSessionValue(storageKey, clientId);
  return clientId;
}
