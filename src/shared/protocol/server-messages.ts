import { z } from "zod";
import { MAX_ENDPOINT_MEDIA_COPY_CAPACITY } from "../media-copy-accounting.js";
import {
  MAX_PARTICIPANTS_PER_ROOM_LIMIT,
  MAX_VIEWERS_PER_ROOM_LIMIT,
  SIGNALING_PROTOCOL,
  displayNameSchema,
  opaqueIdSchema,
} from "./primitives.js";
import {
  codeEntryPolicySchema,
  coPlayParticipantSchema,
} from "./room.js";
import {
  DEFAULT_ROUTE_POLICY,
  qualitySettingsSchema,
  routePolicySchema,
  viewerRoutePreferenceSchema,
} from "./quality.js";
import {
  activeMediaRouteUpstreamSchema,
  mediaRouteRevisionSchema,
  participantPresenceEntrySchema,
  participantRouteAssignmentSchema,
  preparedRouteCandidateSchema,
} from "./route.js";
import { interactionPayloadSchema } from "./interactions.js";
import {
  iceConfigSchema,
  roomVoiceStateSchema,
  sfuSignalMessageSchema,
  signalPayloadSchema,
  signalingChallengeSequenceSchema,
  voiceSignalShape,
} from "./signaling.js";
import { routeDiagnosticSnapshotSchema } from "./diagnostics.js";
import {
  viewerQualityEvidenceGuardSchema,
  viewerQualityEvidenceWindowShape,
} from "./quality-evidence.js";

const errorCodeSchema = z.enum([
  "AUTH_REQUIRED",
  "INVALID_MESSAGE",
  "INVALID_TOKEN",
  "ROOM_ACCESS_DENIED",
  "ROOM_NOT_FOUND",
  "ROOM_FULL",
  "HOST_ALREADY_CONNECTED",
  "PEER_NOT_FOUND",
  "FORBIDDEN",
  "SERVER_ERROR",
]);

const authenticatedMessageShape = {
  type: z.literal("authenticated"),
  protocol: z.literal(SIGNALING_PROTOCOL),
  peerId: opaqueIdSchema,
  maxViewers: z.number().int().min(1).max(MAX_VIEWERS_PER_ROOM_LIMIT),
  endpointMediaCopyCapacity: z
    .number()
    .int()
    .min(1)
    .max(MAX_ENDPOINT_MEDIA_COPY_CAPACITY),
  hostOnline: z.boolean(),
  hostPaused: z.boolean().optional(),
  connectionId: opaqueIdSchema.nullable(),
  iceConfig: iceConfigSchema,
  routePolicy: routePolicySchema.default(DEFAULT_ROUTE_POLICY),
  codeEntryPolicy: codeEntryPolicySchema,
  viewerAuthorizationGeneration: opaqueIdSchema,
};

const authenticatedHostMessageShape = {
  ...authenticatedMessageShape,
  role: z.literal("host"),
  viewerPasswordEnabled: z.boolean(),
};

const authenticatedViewerMessageShape = {
  ...authenticatedMessageShape,
  role: z.literal("viewer"),
};

const routeAuthenticatedShape = {
  mediaMode: z.literal("peer-assisted"),
  shareGeneration: opaqueIdSchema.nullable(),
  routeRevision: mediaRouteRevisionSchema,
  routeAssignment: participantRouteAssignmentSchema,
  qualitySettings: qualitySettingsSchema,
};

const authenticatedMessageSchema = z.union([
  z
    .object({
      ...authenticatedHostMessageShape,
      ...routeAuthenticatedShape,
    })
    .strict(),
  z
    .object({
      ...authenticatedViewerMessageShape,
      ...routeAuthenticatedShape,
    })
    .strict(),
]);

export const serverMessageSchema = z.union([
  z.object({ type: z.literal("relay-volunteer"), enabled: z.boolean(), capacity: z.number().int().min(0).max(MAX_VIEWERS_PER_ROOM_LIMIT) }).strict(),
  roomVoiceStateSchema,
  z.object({ type: z.literal("room-voice-signal"), ...voiceSignalShape }).strict()
    .refine(value => Number(!!value.description) + Number(!!value.candidate) + Number(!!value.renegotiate) === 1),
  z.object({ type: z.literal("room-voice-error"),
    code: z.enum(["not-allowed", "not-enabled", "connection-unavailable"]) }).strict(),
  z.object({ type: z.literal("viewer-route-preference"), preference: viewerRoutePreferenceSchema, accepted: z.boolean() }).strict(),
  z.object({ type: z.literal("audio-presence"), available: z.boolean().nullable() }).strict(),
  z.object({ type: z.literal("co-play-presence"), participants: z.array(coPlayParticipantSchema).max(MAX_VIEWERS_PER_ROOM_LIMIT)
    .refine(participants => new Set(participants.map(person => person.peerId)).size === participants.length) }).strict(),
  z.object({ type: z.literal("remote-input-peers"), peers: z.array(z.object({ peerId: opaqueIdSchema,
    endpointId: z.string().regex(/^[0-9a-f]{32}$/), secret: z.string().regex(/^[0-9a-f]{64}$/) }).strict()).max(MAX_VIEWERS_PER_ROOM_LIMIT) }).strict(),
  z.object({ type: z.literal("room-interactions-ready"), serverTime: z.number().int().min(0).max(Number.MAX_SAFE_INTEGER) }).strict(),
  z.object({ type: z.literal("room-interaction-rejected"), requestId: opaqueIdSchema,
    reason: z.enum(["rate-limited", "target-unavailable", "busy"]) }).strict(),
  z.object({ type: z.literal("room-interaction"), id: opaqueIdSchema, requestId: opaqueIdSchema,
    occurredAt: z.number().int().min(0).max(Number.MAX_SAFE_INTEGER),
    sender: z.object({ peerId: opaqueIdSchema, role: z.enum(["host", "viewer"]), displayName: displayNameSchema }).strict(),
    payload: interactionPayloadSchema }).strict(),
  authenticatedMessageSchema,
  z.object({
    type: z.literal("sharing-start-failed"),
    shareGeneration: opaqueIdSchema,
    code: z.enum(["FORBIDDEN", "SERVER_ERROR"]),
  }).strict(),
  z
    .object({
      type: z.literal("sharing-started"),
      shareGeneration: opaqueIdSchema,
      routePolicy: routePolicySchema,
      routeRevision: mediaRouteRevisionSchema,
      routeAssignment: participantRouteAssignmentSchema,
      qualitySettings: qualitySettingsSchema,
      paused: z.boolean(),
    })
    .strict(),
  z
    .object({
      type: z.literal("signaling-challenge-response"),
      sequence: signalingChallengeSequenceSchema,
    })
    .strict(),
  z
    .object({
      type: z.literal("signal"),
      fromPeerId: opaqueIdSchema,
      payload: signalPayloadSchema,
    })
    .strict(),
  z
    .object({
      type: z.literal("restart-request"),
      fromPeerId: opaqueIdSchema,
      connectionId: opaqueIdSchema,
      rebuild: z.boolean(),
    })
    .strict(),
  z.discriminatedUnion("phase", [
    z
      .object({
        type: z.literal("route-update"),
        revision: mediaRouteRevisionSchema,
        phase: z.literal("prepare"),
        assignment: participantRouteAssignmentSchema,
        candidate: preparedRouteCandidateSchema,
      })
      .strict(),
    z
      .object({
        type: z.literal("route-update"),
        revision: mediaRouteRevisionSchema,
        phase: z.literal("active"),
        assignment: participantRouteAssignmentSchema,
      })
      .strict(),
  ]),
  z.discriminatedUnion("state", [
    z
      .object({
        type: z.literal("route-status"),
        revision: mediaRouteRevisionSchema,
        state: z.literal("waiting"),
        reason: z.literal("sfu-admission"),
      })
      .strict(),
    z
      .object({
        type: z.literal("route-status"),
        revision: mediaRouteRevisionSchema,
        state: z.literal("failed"),
        reason: z.literal("route-exhausted"),
      })
      .strict(),
  ]),
  z
    .object({
      type: z.literal("route-diagnostic-snapshot"),
      snapshot: routeDiagnosticSnapshotSchema,
    })
    .strict(),
  z
    .object({
      type: z.literal("sfu-config"),
      revision: mediaRouteRevisionSchema,
      publicationGeneration: opaqueIdSchema,
      connectionId: opaqueIdSchema,
    })
    .strict(),
  sfuSignalMessageSchema,
  z
    .object({
      type: z.literal("quality-settings"),
      qualitySettings: qualitySettingsSchema,
    })
    .strict(),
  z
    .object({
      type: z.literal("route-policy"),
      shareGeneration: opaqueIdSchema,
      routePolicy: routePolicySchema,
    })
    .strict(),
  z
    .object({
      type: z.literal("pause-sharing-source"),
      shareGeneration: opaqueIdSchema,
    })
    .strict(),
  z
    .object({
      type: z.literal("viewer-quality-evidence"),
      viewerPeerId: opaqueIdSchema,
      upstream: activeMediaRouteUpstreamSchema,
      guard: viewerQualityEvidenceGuardSchema,
      ...viewerQualityEvidenceWindowShape,
    })
    .strict(),
  z
    .object({
      type: z.literal("host-status"),
      online: z.boolean(),
      paused: z.boolean(),
    })
    .strict(),
  z
    .object({
      type: z.literal("viewer-presence"),
      viewers: z
        .array(participantPresenceEntrySchema)
        .max(MAX_PARTICIPANTS_PER_ROOM_LIMIT),
    })
    .strict()
    .superRefine((message, context) => {
      const peerIds = new Set<string>();
      for (const viewer of message.viewers) {
        if (peerIds.has(viewer.peerId)) {
          context.addIssue({
            code: "custom",
            message: "Viewer presence peer IDs must be unique",
            path: ["viewers"],
          });
          return;
        }
        peerIds.add(viewer.peerId);
      }
    }),
  z
    .object({
      type: z.literal("viewer-grant-revoked"),
      viewerAuthorizationGeneration: opaqueIdSchema,
    })
    .strict(),
  z.object({ type: z.literal("sharing-stopped") }).strict(),
  z
    .object({
      type: z.literal("room-closed"),
      reason: z.literal("host-ended"),
    })
    .strict(),
  z
    .object({
      type: z.literal("error"),
      code: errorCodeSchema,
      message: z.string().min(1).max(256),
    })
    .strict(),
]);
export type ServerMessage = z.infer<typeof serverMessageSchema>;

export function decodeServerMessage(value: string): ServerMessage {
  return serverMessageSchema.parse(JSON.parse(value));
}
