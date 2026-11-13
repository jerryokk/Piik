import { z } from "zod";
import { MAX_VIEWER_PASSWORD_LENGTH, MIN_VIEWER_PASSWORD_LENGTH, ROOM_CODE_LENGTH, opaqueIdSchema } from "./primitives.js";

export const coPlayGrantSchema = z.object({
  endpointId: z.string().regex(/^[0-9a-f]{32}$/),
}).strict();
export const coPlayParticipantSchema = z.object({ peerId: opaqueIdSchema }).strict();
export type CoPlayGrant = z.infer<typeof coPlayGrantSchema>;
export type CoPlayParticipant = z.infer<typeof coPlayParticipantSchema>;

export const tokenSchema = z
  .string()
  .min(32)
  .max(128)
  .regex(/^[A-Za-z0-9_-]+$/);

export const viewerGrantSchema = z
  .string()
  .length(22)
  .regex(/^[A-Za-z0-9_-]{21}[AQgw]$/);

export const viewerPasswordSchema = z
  .string()
  .min(MIN_VIEWER_PASSWORD_LENGTH)
  .max(MAX_VIEWER_PASSWORD_LENGTH)
  .regex(/^[\x21-\x7e]+$/);

export const codeEntryPolicySchema = z.enum(["open", "private"]);
export type CodeEntryPolicy = z.infer<typeof codeEntryPolicySchema>;


export const roomCodeSchema = z
  .string()
  .length(ROOM_CODE_LENGTH)
  .max(ROOM_CODE_LENGTH)
  .regex(/^[1-9]\d*$/);

// Deployment facts only: everything else is present in every current build.
export const runtimeCapabilitiesSchema = z
  .object({
    sfu: z.boolean(),
    sfuOnly: z.boolean().optional(),
    natPrediction: z.boolean(),
  })
  .strict();
export type RuntimeCapabilities = z.infer<typeof runtimeCapabilitiesSchema>;

export const createRoomResponseSchema = z
  .object({
    roomId: roomCodeSchema,
    hostToken: tokenSchema,
    inviteUrl: z.string().url().max(2048),
    codeEntryPolicy: codeEntryPolicySchema,
  })
  .strict();
export type CreateRoomResponse = z.infer<typeof createRoomResponseSchema>;

export const createRoomRequestSchema = z
  .object({
    codeEntryPolicy: codeEntryPolicySchema,
    roomPassword: viewerPasswordSchema.nullable().optional(),
    preferredRoomId: roomCodeSchema.optional(),
  })
  .strict();

export const replaceRoomRequestSchema = z
  .object({
    codeEntryPolicy: codeEntryPolicySchema,
    roomPassword: viewerPasswordSchema.nullable().optional(),
  })
  .strict();

export const roomAccessUpdateRequestSchema = z.discriminatedUnion("action", [
  z
    .object({
      action: z.literal("set-code-entry-policy"),
      policy: codeEntryPolicySchema,
    })
    .strict(),
  z.object({ action: z.literal("rotate-viewer-grant") }).strict(),
  z.object({ action: z.literal("revoke-viewer-grant") }).strict(),
  z
    .object({
      action: z.literal("set-viewer-password"),
      password: viewerPasswordSchema.nullable(),
    })
    .strict(),
]);
export type RoomAccessUpdateRequest = z.infer<
  typeof roomAccessUpdateRequestSchema
>;

export const roomAccessUpdateResponseSchema = z.discriminatedUnion("type", [
  z
    .object({
      type: z.literal("code-entry-policy-updated"),
      codeEntryPolicy: codeEntryPolicySchema,
      viewerPasswordEnabled: z.boolean(),
    })
    .strict(),
  z
    .object({
      type: z.literal("viewer-grant-updated"),
      viewerAuthorizationGeneration: opaqueIdSchema,
      inviteUrl: z.string().url().max(2048).nullable(),
    })
    .strict(),
  z
    .object({
      type: z.literal("viewer-password-updated"),
      enabled: z.boolean(),
    })
    .strict(),
]);
export type RoomAccessUpdateResponse = z.infer<
  typeof roomAccessUpdateResponseSchema
>;
