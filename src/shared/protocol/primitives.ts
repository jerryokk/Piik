import { z } from "zod";

export const MAX_VIEWERS_PER_ROOM_LIMIT = 20;
export const MAX_PARTICIPANTS_PER_ROOM_LIMIT = MAX_VIEWERS_PER_ROOM_LIMIT + 1;
export const MAX_SIGNAL_BYTES = 64 * 1024;
export const SIGNALING_PROTOCOL = "piik-v24";
export const SIGNAL_CLOSE_CODES = {
  serviceRestart: 1012,
  sessionReplaced: 4001,
  clientReconnect: 4002,
  authenticationFailed: 4003,
  viewerAccessRevoked: 4004,
} as const;
export const ROOM_CODE_LENGTH = 4;
export const MAX_MEDIA_ROUTE_REVISION = Number.MAX_SAFE_INTEGER;
export const MAX_ICE_SERVER_URLS = 8;
export const MAX_NAT_PREDICTION_AUXILIARY_STUN_URLS = 2;
export const MAX_VIEWER_QUALITY_EVIDENCE_BYTES = 2 * 1024;
export const VIEWER_QUALITY_EVIDENCE_INTERVAL_MS = 2_000;
export const VIEWER_QUALITY_EVIDENCE_EXPIRY_MS = 5_000;
export const PERSISTENT_NATIVE_EDGE_DEGRADED_WINDOWS = 3;
export const MAX_DISPLAY_NAME_CODE_POINTS = 24;
export const DEFAULT_VIEWER_DISPLAY_NAME = "观众";
export const DEFAULT_HOST_DISPLAY_NAME_PREFIX = "房主";
export const MIN_VIEWER_PASSWORD_LENGTH = 1;
export const MAX_VIEWER_PASSWORD_LENGTH = 64;

const FORBIDDEN_DISPLAY_NAME_CHARACTERS =
  /[\p{Cc}\p{Zl}\p{Zp}\u061c\u200b\u200e\u200f\u202a-\u202e\u2060\u2066-\u2069\ufeff]/u;

export function normalizeDisplayName(value: string): string | null {
  if (FORBIDDEN_DISPLAY_NAME_CHARACTERS.test(value)) {
    return null;
  }
  for (const character of value) {
    const codePoint = character.codePointAt(0)!;
    if (codePoint >= 0xd800 && codePoint <= 0xdfff) {
      return null;
    }
  }

  const normalized = value.normalize("NFC").trim().replace(/\p{Zs}+/gu, " ");
  const codePointCount = Array.from(normalized).length;
  return codePointCount >= 1 && codePointCount <= MAX_DISPLAY_NAME_CODE_POINTS
    ? normalized
    : null;
}

export const displayNameSchema = z
  .string()
  .min(1)
  .max(MAX_DISPLAY_NAME_CODE_POINTS * 4)
  .refine((value) => normalizeDisplayName(value) === value, {
    message: "Display name must be canonical",
  });
export type DisplayName = z.infer<typeof displayNameSchema>;

export const opaqueIdSchema = z
  .string()
  .min(8)
  .max(128)
  .regex(/^[A-Za-z0-9_-]+$/);

// Describes a requested ceiling, not encoder availability or an allocation.
// WebRTC uses unsigned-long dimensions/bitrates and a double frame rate.
// Alignment, usable ranges and allocation budgets belong to the media adapter.
export const positiveUint32Schema = z.number().int().positive().max(0xffff_ffff);
