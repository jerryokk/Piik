import { z } from "zod";
import { MAX_MEDIA_ROUTE_REVISION, MAX_VIEWERS_PER_ROOM_LIMIT, displayNameSchema, opaqueIdSchema } from "./primitives.js";

const noMediaRouteUpstreamSchema = z
  .object({ kind: z.literal("none") })
  .strict();
const peerMediaRouteUpstreamSchema = z
  .object({
    kind: z.literal("peer"),
    peerId: opaqueIdSchema,
  })
  .strict();
const sfuMediaRouteUpstreamSchema = z
  .object({ kind: z.literal("sfu") })
  .strict();

export const mediaRouteUpstreamSchema = z.discriminatedUnion("kind", [
  noMediaRouteUpstreamSchema,
  peerMediaRouteUpstreamSchema,
  sfuMediaRouteUpstreamSchema,
]);
export type MediaRouteUpstream = z.infer<typeof mediaRouteUpstreamSchema>;

export const activeMediaRouteUpstreamSchema = z.discriminatedUnion("kind", [
  peerMediaRouteUpstreamSchema,
  sfuMediaRouteUpstreamSchema,
]);

export const viewerPresenceEntrySchema = z
  .object({
    role: z.literal("viewer"),
    peerId: opaqueIdSchema,
    displayName: displayNameSchema,
    upstream: mediaRouteUpstreamSchema,
    mediaReady: z.literal(true).optional(),
  })
  .strict();
export type ViewerPresenceEntry = z.infer<typeof viewerPresenceEntrySchema>;

export const participantPresenceEntrySchema = z.discriminatedUnion("role", [
  z
    .object({
      role: z.literal("host"),
      peerId: opaqueIdSchema,
      displayName: displayNameSchema,
      upstream: z.object({ kind: z.literal("none") }).strict(),
    })
    .strict(),
  viewerPresenceEntrySchema,
]);
export type ParticipantPresenceEntry = z.infer<
  typeof participantPresenceEntrySchema
>;

export const preparedRouteCandidateSchema = z
  .object({
    childPeerId: opaqueIdSchema,
    connectionId: opaqueIdSchema,
    transport: z.enum(["direct", "sfu"]),
    qualityProbe: z.boolean(),
    connectionAttempt: z.object({
      current: z.number().int().positive().max(Number.MAX_SAFE_INTEGER),
      total: z.number().int().positive().max(Number.MAX_SAFE_INTEGER),
    }).strict().refine(({ current, total }) => current <= total).optional(),
  })
  .strict();
export type PreparedRouteCandidate = z.infer<
  typeof preparedRouteCandidateSchema
>;

export const mediaRouteRevisionSchema = z
  .number()
  .int()
  .min(0)
  .max(MAX_MEDIA_ROUTE_REVISION);

export const mediaRoutePhaseSchema = z.enum(["prepare", "active"]);
export type MediaRoutePhase = z.infer<typeof mediaRoutePhaseSchema>;

export const sfuPublicationGenerationSchema = opaqueIdSchema;

export const participantRouteAssignmentSchema = z
  .object({
    upstream: mediaRouteUpstreamSchema,
    childPeerIds: z
      .array(opaqueIdSchema)
      .max(MAX_VIEWERS_PER_ROOM_LIMIT)
      .refine((peerIds) => new Set(peerIds).size === peerIds.length),
    sfuPublicationGeneration: sfuPublicationGenerationSchema.nullable(),
  })
  .strict()
  .superRefine((assignment, context) => {
    if (
      assignment.upstream.kind === "sfu" &&
      assignment.sfuPublicationGeneration === null
    ) {
      context.addIssue({
        code: "custom",
        message: "An SFU upstream requires its publication generation",
        path: ["sfuPublicationGeneration"],
      });
    }
    if (
      assignment.upstream.kind === "peer" &&
      assignment.sfuPublicationGeneration !== null
    ) {
      context.addIssue({
        code: "custom",
        message: "A peer upstream cannot own an SFU publication generation",
        path: ["sfuPublicationGeneration"],
      });
    }
  });
export type ParticipantRouteAssignment = z.infer<
  typeof participantRouteAssignmentSchema
>;
