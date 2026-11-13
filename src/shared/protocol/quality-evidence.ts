import { z } from "zod";
import { isCanonicalVideoCodecEvidence } from "../video-codec-evidence.js";
import { NAT_TRAVERSAL_PATHS } from "../nat-candidate.js";
import { opaqueIdSchema } from "./primitives.js";
import { mediaRouteRevisionSchema } from "./route.js";

const nullableEvidenceNumber = (maximum: number) =>
  z.number().finite().min(0).max(maximum).nullable();

const nullableSignedEvidenceNumber = (absoluteMaximum: number) =>
  z.number().finite().min(-absoluteMaximum).max(absoluteMaximum).nullable();

export const nullableSafeEvidenceNumber = z
  .number()
  .finite()
  .min(0)
  .max(Number.MAX_SAFE_INTEGER)
  .nullable();

export const nullableSafeEvidenceInteger = z
  .number()
  .int()
  .min(0)
  .max(Number.MAX_SAFE_INTEGER)
  .nullable();

export const viewerQualityEvidenceMetricsSchema = z
  .object({
    natTraversalPath: z.enum(NAT_TRAVERSAL_PATHS),
    width: z.number().int().min(1).max(Number.MAX_SAFE_INTEGER).nullable(),
    height: z.number().int().min(1).max(Number.MAX_SAFE_INTEGER).nullable(),
    framesPerSecond: nullableSafeEvidenceNumber,
    bitrateKbps: nullableSafeEvidenceNumber,
    packetsReceivedDelta: nullableSafeEvidenceInteger,
    packetsLostDelta: nullableSafeEvidenceInteger,
    rttMs: nullableSafeEvidenceNumber,
    jitterMs: nullableSafeEvidenceNumber,
    framesDecodedDelta: nullableSafeEvidenceInteger,
    framesDroppedDelta: nullableSafeEvidenceInteger,
    decodeMsPerFrame: nullableSafeEvidenceNumber,
    freezeCountDelta: nullableSafeEvidenceInteger,
    freezeDurationMsDelta: nullableSafeEvidenceNumber,
    pauseCountDelta: nullableSafeEvidenceInteger,
    pauseDurationMsDelta: nullableSafeEvidenceNumber,
    codec: z
      .string()
      .max(64)
      .regex(/^video\/[A-Za-z0-9.+-]{1,32}$/i)
      .nullable(),
    codecProfile: z
      .string()
      .max(64)
      .regex(/^[a-z0-9-]+=[a-z0-9]+$/)
      .nullable(),
    codecParameters: z
      .string()
      .max(128)
      .regex(/^[a-z0-9-]+=[a-z0-9]+(?:; [a-z0-9-]+=[a-z0-9]+)*$/)
      .nullable(),
    audioBitrateKbps: nullableSafeEvidenceNumber,
    audioPacketLossPercent: nullableEvidenceNumber(100),
    audioJitterMs: nullableSafeEvidenceNumber,
    audioVideoPlayoutDeltaMs: nullableSignedEvidenceNumber(Number.MAX_SAFE_INTEGER),
    videoJitterBufferDelayMs: nullableSafeEvidenceNumber,
    audioJitterBufferDelayMs: nullableSafeEvidenceNumber,
    audioConcealedSamplesPercent: nullableEvidenceNumber(100),
    audioConcealmentEventsDelta: nullableSafeEvidenceInteger,
    audioCodec: z
      .string()
      .max(64)
      .regex(/^audio\/[A-Za-z0-9.+-]{1,32}$/i)
      .nullable(),
  })
  .strict()
  .refine(
    (metrics) =>
      (metrics.width === null && metrics.height === null) ||
      (metrics.width !== null && metrics.height !== null),
    { message: "Viewer quality dimensions must be present together" },
  )
  .refine(isCanonicalVideoCodecEvidence, {
    message: "Viewer codec evidence is not canonical",
    path: ["codecParameters"],
  });
export type ViewerQualityEvidenceMetrics = z.infer<
  typeof viewerQualityEvidenceMetricsSchema
>;

export const viewerQualityEvidenceGuardSchema = z
  .object({
    connectionId: opaqueIdSchema,
    routeRevision: mediaRouteRevisionSchema,
    presentationEpoch: z
      .number()
      .int()
      .min(0)
      .max(Number.MAX_SAFE_INTEGER),
  })
  .strict();

export const viewerQualityEvidenceWindowShape = {
  sequence: z.number().int().min(0).max(Number.MAX_SAFE_INTEGER),
  windowMs: z.number().int().min(1_000).max(5_000),
  metrics: viewerQualityEvidenceMetricsSchema,
};

export const senderQualityDiagnosticsSchema = z
  .object({
    natTraversalPath: z.enum(NAT_TRAVERSAL_PATHS),
    reason: z.enum(["none", "bandwidth", "cpu"]).nullable(),
    framesPerSecond: nullableSafeEvidenceNumber,
    bitrateKbps: nullableSafeEvidenceNumber,
    captureFramesPerSecond: nullableSafeEvidenceNumber.optional(),
    mediaSourceFramesPerSecond: nullableSafeEvidenceNumber.optional(),
    width: nullableSafeEvidenceInteger.optional(),
    height: nullableSafeEvidenceInteger.optional(),
    videoEncodingCount: nullableSafeEvidenceInteger.optional(),
    activeVideoEncodingCount: nullableSafeEvidenceInteger.optional(),
    availableOutgoingKbps: nullableSafeEvidenceNumber.optional(),
    rttMs: nullableSafeEvidenceNumber.optional(),
    packetLossPercent: nullableEvidenceNumber(100).optional(),
  })
  .strict();

export const viewerQualityEvidenceMessageSchema = z
  .object({
    type: z.literal("viewer-quality-evidence"),
    guard: viewerQualityEvidenceGuardSchema,
    ...viewerQualityEvidenceWindowShape,
  })
  .strict();

export const senderQualityEvidenceMessageSchema = z
  .object({
    type: z.literal("sender-quality-evidence"),
    childPeerId: opaqueIdSchema,
    connectionId: opaqueIdSchema,
    rtpStatsId: z.string().min(1).max(256).nullable(),
    trackIdentifier: z.string().min(1).max(256).nullable(),
    sampleTimestampMs: z
      .number()
      .finite()
      .min(0)
      .max(Number.MAX_SAFE_INTEGER)
      .nullable(),
    routeRevision: mediaRouteRevisionSchema,
    state: z.enum(["unknown", "healthy", "degraded"]),
    diagnostics: senderQualityDiagnosticsSchema,
  })
  .strict()
  .superRefine((message, context) => {
    if (
      (message.state !== "unknown" &&
        (message.rtpStatsId === null ||
          message.trackIdentifier === null ||
          message.sampleTimestampMs === null))
    ) {
      context.addIssue({
        code: "custom",
        message: "Known sender quality needs exact RTP identity",
        path: ["rtpStatsId"],
      });
    }
    const reason = message.diagnostics.reason;
    if (
      (message.state === "healthy" && reason !== "none") ||
      (message.state === "degraded" &&
        reason !== "bandwidth" &&
        reason !== "cpu") ||
      (message.state === "unknown" && reason !== null)
    ) {
      context.addIssue({
        code: "custom",
        message: "Sender quality reason must match its native state",
        path: ["diagnostics", "reason"],
      });
    }
  });

export const sfuPublisherQualityEvidenceMessageSchema = z
  .object({
    type: z.literal("sfu-publisher-quality-evidence"),
    publicationGeneration: opaqueIdSchema,
    routeRevision: mediaRouteRevisionSchema,
    state: z.enum(["unknown", "healthy", "degraded"]),
    sampleTimestampMs: z
      .number()
      .finite()
      .min(0)
      .max(Number.MAX_SAFE_INTEGER)
      .nullable(),
    diagnostics: senderQualityDiagnosticsSchema,
  })
  .strict()
  .superRefine((message, context) => {
    if (message.state !== "unknown" && message.sampleTimestampMs === null) {
      context.addIssue({
        code: "custom",
        message: "Known SFU publisher quality needs an exact sample",
        path: ["sampleTimestampMs"],
      });
    }
    const reason = message.diagnostics.reason;
    if (
      (message.state === "healthy" && reason !== "none") ||
      (message.state === "degraded" &&
        reason !== "bandwidth" &&
        reason !== "cpu") ||
      (message.state === "unknown" && reason !== null)
    ) {
      context.addIssue({
        code: "custom",
        message: "SFU publisher reason must match its native state",
        path: ["diagnostics", "reason"],
      });
    }
  });
