import { z } from "zod";
import {
  MAX_VIEWERS_PER_ROOM_LIMIT,
  SIGNALING_PROTOCOL,
  displayNameSchema,
  opaqueIdSchema,
} from "./primitives.js";
import {
  coPlayGrantSchema,
  roomCodeSchema,
  tokenSchema,
  viewerGrantSchema,
  viewerPasswordSchema,
} from "./room.js";
import {
  DEFAULT_ROUTE_POLICY,
  qualitySettingsSchema,
  routePolicySchema,
  viewerRoutePreferenceSchema,
} from "./quality.js";
import {
  mediaRoutePhaseSchema,
  mediaRouteRevisionSchema,
} from "./route.js";
import { interactionPayloadSchema } from "./interactions.js";
import {
  relayDownstreamEdgesSchema,
  roomVoiceCommandSchema,
  sfuSignalMessageSchema,
  signalPayloadSchema,
  signalingChallengeSequenceSchema,
  voiceSignalShape,
} from "./signaling.js";
import {
  senderQualityEvidenceMessageSchema,
  sfuPublisherQualityEvidenceMessageSchema,
  viewerQualityEvidenceMessageSchema,
} from "./quality-evidence.js";

const authenticateMessageSchema = z.discriminatedUnion("role", [
  z
    .object({
      type: z.literal("authenticate"),
      protocol: z.literal(SIGNALING_PROTOCOL),
      roomId: roomCodeSchema,
      role: z.literal("host"),
      token: tokenSchema,
      clientId: opaqueIdSchema,
      shareGeneration: opaqueIdSchema.optional(),
      sharingPaused: z.boolean().optional(),
      qualitySettings: qualitySettingsSchema.optional(),
      routePolicy: routePolicySchema.default(DEFAULT_ROUTE_POLICY),
      displayName: displayNameSchema.optional(),
    })
    .strict(),
  z
    .object({
      type: z.literal("authenticate"),
      protocol: z.literal(SIGNALING_PROTOCOL),
      roomId: roomCodeSchema,
      role: z.literal("viewer"),
      clientId: opaqueIdSchema,
      viewerGrant: viewerGrantSchema.optional(),
      viewerPassword: viewerPasswordSchema.optional(),
      routePreference: viewerRoutePreferenceSchema.optional(),
      displayName: displayNameSchema.optional(),
    })
    .strict(),
]);

export const clientMessageSchema = z.union([
  z.object({ type: z.literal("set-relay-volunteer"), enabled: z.boolean(), capacity: z.number().int().min(0).max(MAX_VIEWERS_PER_ROOM_LIMIT) }).strict(),
  z.object({ type: z.literal("room-voice-command"), command: roomVoiceCommandSchema }).strict(),
  z.object({ type: z.literal("room-voice-signal"), ...voiceSignalShape }).strict()
    .refine(value => Number(!!value.description) + Number(!!value.candidate) + Number(!!value.renegotiate) === 1),
  z.object({ type: z.literal("room-voice-transport"), connectionId: opaqueIdSchema,
    state: z.enum(["connected", "failed"]) }).strict(),
  z.object({ type: z.literal("set-viewer-route-preference"), preference: viewerRoutePreferenceSchema }).strict(),
  z.object({ type: z.literal("set-co-play-presence"), shareGeneration: opaqueIdSchema,
    grants: z.array(coPlayGrantSchema).max(MAX_VIEWERS_PER_ROOM_LIMIT)
      .refine(grants => new Set(grants.map(grant => grant.endpointId)).size === grants.length) }).strict(),
  z.object({ type: z.literal("set-audio-presence"), shareGeneration: opaqueIdSchema, available: z.boolean() }).strict(),
  z.object({ type: z.literal("subscribe-remote-input"), endpointId: z.string().regex(/^[0-9a-f]{32}$/).optional(),
    secret: z.string().regex(/^[0-9a-f]{64}$/).optional() }).strict(),
  z.object({ type: z.literal("send-room-interaction"), requestId: opaqueIdSchema,
    payload: interactionPayloadSchema }).strict(),
  authenticateMessageSchema,
  z
    .object({
      type: z.literal("signaling-challenge"),
      sequence: signalingChallengeSequenceSchema,
    })
    .strict(),
  z
    .object({
      type: z.literal("signal"),
      targetPeerId: opaqueIdSchema.optional(),
      payload: signalPayloadSchema,
    })
    .strict(),
  z
    .object({
      type: z.literal("restart-request"),
      targetPeerId: opaqueIdSchema.optional(),
      connectionId: opaqueIdSchema,
      rebuild: z.boolean(),
    })
    .strict(),
  z
    .object({
      type: z.literal("set-quality-settings"),
      qualitySettings: qualitySettingsSchema,
    })
    .strict(),
  z
    .object({
      type: z.literal("relay-capacity"),
      downstreamEdges: relayDownstreamEdgesSchema,
    })
    .strict(),
  z
    .object({
      type: z.literal("route-ready"),
      revision: mediaRouteRevisionSchema,
      phase: mediaRoutePhaseSchema,
      qualityApproved: z.literal(true).optional(),
    })
    .strict(),
  z
    .object({
      type: z.literal("route-transport-connected"),
      revision: mediaRouteRevisionSchema,
      connectionId: opaqueIdSchema,
    })
    .strict(),
  z
    .object({
      type: z.literal("route-media-unavailable"),
      revision: mediaRouteRevisionSchema,
    })
    .strict(),
  z
    .object({
      type: z.literal("route-failed"),
      revision: mediaRouteRevisionSchema,
      phase: mediaRoutePhaseSchema,
      connectionId: opaqueIdSchema.nullable(),
    })
    .strict(),
  z
    .object({
      type: z.literal("refresh-sfu"),
      revision: mediaRouteRevisionSchema,
    })
    .strict(),
  sfuSignalMessageSchema,
  z.object({ type: z.literal("request-route-diagnostic") }).strict(),
  viewerQualityEvidenceMessageSchema,
  senderQualityEvidenceMessageSchema,
  sfuPublisherQualityEvidenceMessageSchema,
  z.object({ type: z.literal("reset-sender-quality") }).strict(),
  z
    .object({
      type: z.literal("set-display-name"),
      displayName: displayNameSchema,
    })
    .strict(),
  z
    .object({
      type: z.literal("set-sharing-paused"),
      shareGeneration: opaqueIdSchema,
      paused: z.boolean(),
    })
    .strict(),
  z
    .object({
      type: z.literal("stop-sharing"),
      shareGeneration: opaqueIdSchema.optional(),
    })
    .strict(),
  z
    .object({
      type: z.literal("start-sharing"),
      shareGeneration: opaqueIdSchema,
      sharingPaused: z.boolean().optional(),
      qualitySettings: qualitySettingsSchema.optional(),
      routePolicy: routePolicySchema,
    })
    .strict(),
  z.object({ type: z.literal("abandon-room") }).strict(),
]);
export type ClientMessage = z.infer<typeof clientMessageSchema>;

export function decodeClientMessage(value: string): ClientMessage {
  return clientMessageSchema.parse(JSON.parse(value));
}
