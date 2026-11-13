import { z } from "zod";
import { MAX_VIEWERS_PER_ROOM_LIMIT } from "./primitives.js";

const routeDemandReasonSchema = z.enum([
  "join",
  "edge-unavailable",
  "parent-departed",
  "capacity-reduction",
  "sfu-bootstrap",
  "direct-convergence",
  "quality-convergence",
  "root-convergence",
  "publication-attach",
]);

const routeDiagnosticFinalRouteSchema = z.enum([
  "direct",
  "sfu",
  "waiting",
  "failed",
]);

const routeDiagnosticRejectionBucketSchema = z.enum([
  "none",
  "stale",
  "endpoint-capacity",
  "sfu-admission",
  "candidate-failed",
  "first-frame-timeout",
  "operation-deadline",
  "aborted",
]);

const routeDiagnosticDurationSchema = z
  .number()
  .int()
  .min(0)
  .max(Number.MAX_SAFE_INTEGER)
  .nullable();
const routeDiagnosticOrdinalSchema = z
  .number()
  .int()
  .min(1)
  .max(MAX_VIEWERS_PER_ROOM_LIMIT);
const routeDiagnosticParentSchema = z.discriminatedUnion("kind", [
  z.object({ kind: z.literal("none") }).strict(),
  z.object({ kind: z.literal("host") }).strict(),
  z.object({ kind: z.literal("sfu") }).strict(),
  z
    .object({
      kind: z.literal("viewer"),
      ordinal: routeDiagnosticOrdinalSchema,
    })
    .strict(),
]);
const routeDiagnosticQualityValueSchema = z
  .number()
  .int()
  .min(0)
  .max(Number.MAX_SAFE_INTEGER);
const routeDiagnosticQualitySchema = z
  .object({
    eligibleWindows: routeDiagnosticQualityValueSchema,
    eligibleDurationMs: routeDiagnosticQualityValueSchema,
    freezeWindows: routeDiagnosticQualityValueSchema,
    freezeCount: routeDiagnosticQualityValueSchema,
    freezeDurationMs: routeDiagnosticQualityValueSchema,
    pauseCount: routeDiagnosticQualityValueSchema,
    pauseDurationMs: routeDiagnosticQualityValueSchema,
  })
  .strict();
const routeDiagnosticChildSchema = z
  .object({
    ordinal: routeDiagnosticOrdinalSchema,
    parent: routeDiagnosticParentSchema,
    effectiveCapacity: z
      .number()
      .int()
      .min(0)
      .max(MAX_VIEWERS_PER_ROOM_LIMIT),
    childCount: z
      .number()
      .int()
      .min(0)
      .max(MAX_VIEWERS_PER_ROOM_LIMIT),
    demandAgeMs: routeDiagnosticDurationSchema,
    queueWaitMs: routeDiagnosticDurationSchema,
    candidateStartMs: routeDiagnosticDurationSchema,
    firstDecodedFrameMs: routeDiagnosticDurationSchema,
    finalMs: routeDiagnosticDurationSchema,
    finalRoute: routeDiagnosticFinalRouteSchema,
    rejectionBucket: routeDiagnosticRejectionBucketSchema,
    quality: routeDiagnosticQualitySchema.nullable(),
  })
  .strict();
const routeDiagnosticOperationSchema = z
  .object({
    childOrdinal: routeDiagnosticOrdinalSchema,
    reason: routeDemandReasonSchema,
    stage: z.enum(["admission", "first-frame", "quality-proof"]),
    cursor: z.number().int().min(0).max(MAX_VIEWERS_PER_ROOM_LIMIT),
    candidateCount: z.number().int().min(1).max(MAX_VIEWERS_PER_ROOM_LIMIT + 1),
  })
  .strict();

export const routeDiagnosticSnapshotSchema = z
  .object({
    children: z
      .array(routeDiagnosticChildSchema)
      .max(MAX_VIEWERS_PER_ROOM_LIMIT),
    operation: routeDiagnosticOperationSchema.nullable(),
  })
  .strict()
  .superRefine((snapshot, context) => {
    const ordinals = new Set(snapshot.children.map((child) => child.ordinal));
    if (ordinals.size !== snapshot.children.length) {
      context.addIssue({
        code: "custom",
        message: "Route diagnostic ordinals must be unique",
        path: ["children"],
      });
      return;
    }
    for (const [index, child] of snapshot.children.entries()) {
      if (
        child.parent.kind === "viewer" &&
        (!ordinals.has(child.parent.ordinal) ||
          child.parent.ordinal === child.ordinal)
      ) {
        context.addIssue({
          code: "custom",
          message: "Route diagnostic parent ordinal is invalid",
          path: ["children", index, "parent"],
        });
      }
    }
    if (
      snapshot.operation &&
      (!ordinals.has(snapshot.operation.childOrdinal) ||
        snapshot.operation.cursor >= snapshot.operation.candidateCount)
    ) {
      context.addIssue({
        code: "custom",
        message: "Route diagnostic operation is invalid",
        path: ["operation"],
      });
    }
  });
export type RouteDiagnosticSnapshot = z.infer<
  typeof routeDiagnosticSnapshotSchema
>;
