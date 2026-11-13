import { z } from "zod";
import { MAX_ENDPOINT_MEDIA_COPY_CAPACITY } from "../media-copy-accounting.js";
import {
  MAX_ICE_SERVER_URLS,
  MAX_NAT_PREDICTION_AUXILIARY_STUN_URLS,
  MAX_PARTICIPANTS_PER_ROOM_LIMIT,
  opaqueIdSchema,
  positiveUint32Schema,
} from "./primitives.js";
import { MAX_AUDIO_BITRATE, maxAudioBitrateSchema } from "./quality.js";
import { mediaRouteUpstreamSchema } from "./route.js";

export const relayDownstreamEdgesSchema = z
  .number()
  .int()
  .min(0)
  .max(MAX_ENDPOINT_MEDIA_COPY_CAPACITY);

function isValidStunUrl(value: string): boolean {
  const schemeSeparator = value.indexOf(":");
  if (
    schemeSeparator <= 0 ||
    value.slice(0, schemeSeparator).toLowerCase() !== "stun"
  ) {
    return false;
  }

  const authorityText = value.slice(schemeSeparator + 1);
  if (
    !authorityText ||
    /[\\/\s?#]/.test(authorityText) ||
    authorityText.endsWith(":")
  ) {
    return false;
  }

  let authority: URL;
  try {
    authority = new URL(`http://${authorityText}`);
  } catch {
    return false;
  }
  return Boolean(
    authority.hostname &&
      !authority.username &&
      !authority.password &&
      authority.pathname === "/" &&
      !authority.search &&
      !authority.hash &&
      (!authority.port || Number(authority.port) > 0),
  );
}

const stunUrlSchema = z
  .string()
  .min(1)
  .max(512)
  .refine(isValidStunUrl, { message: "Invalid STUN URL" });

const iceServerSchema = z
  .object({
    urls: z.union([
      stunUrlSchema,
      z.array(stunUrlSchema).min(1).max(MAX_ICE_SERVER_URLS),
    ]),
  })
  .strict();

export const iceConfigSchema = z
  .object({
    iceServers: z.array(iceServerSchema).max(8),
    natPredictionStunUrls: z
      .array(stunUrlSchema)
      .max(MAX_NAT_PREDICTION_AUXILIARY_STUN_URLS),
  })
  .strict();
export type IceConfig = {
  iceServers: Array<z.infer<typeof iceServerSchema>>;
  natPredictionStunUrls?: string[];
};

export const sessionDescriptionSchema = z
  .object({
    type: z.enum(["offer", "answer"]),
    sdp: z.string().min(1).max(48 * 1024),
  })
  .strict();

export const iceCandidateSchema = z
  .object({
    candidate: z.string().max(4096),
    sdpMid: z.string().max(128).nullable().optional(),
    sdpMLineIndex: z.number().int().min(0).max(255).nullable().optional(),
    usernameFragment: z.string().max(256).nullable().optional(),
  })
  .strict();

export const voiceDescriptionSchema = z.object({
  revision: z.number().int().min(1).max(Number.MAX_SAFE_INTEGER),
  sdp: sessionDescriptionSchema,
  tracks: z.array(z.object({ mid: z.string().min(1).max(128), publicationId: opaqueIdSchema }).strict())
    .max(MAX_PARTICIPANTS_PER_ROOM_LIMIT * 2),
}).strict();
export type VoiceDescription = z.infer<typeof voiceDescriptionSchema>;

export const voiceSignalShape = {
  connectionId: opaqueIdSchema,
  description: voiceDescriptionSchema.optional(),
  candidate: iceCandidateSchema.optional(),
  renegotiate: z.literal(true).optional(),
};
export const roomVoiceCommandSchema = z.discriminatedUnion("action", [
  z.object({ action: z.literal("join") }).strict(),
  z.object({ action: z.literal("leave") }).strict(),
  z.object({ action: z.literal("publish") }).strict(),
  z.object({ action: z.literal("unpublish"), publicationId: opaqueIdSchema }).strict(),
  z.object({ action: z.literal("deafen"), deafened: z.boolean() }).strict(),
  z.object({ action: z.literal("bitrate"), bitrate: maxAudioBitrateSchema }).strict(),
]);
export type RoomVoiceCommand = z.infer<typeof roomVoiceCommandSchema>;
export const roomVoiceStateSchema = z.object({
  type: z.literal("room-voice-state"),
  bitrate: maxAudioBitrateSchema.optional(),
  revision: z.number().int().min(0).max(Number.MAX_SAFE_INTEGER),
  enabled: z.boolean(),
  hostPeerId: opaqueIdSchema.nullable(),
  members: z.array(z.object({ peerId: opaqueIdSchema, connected: z.boolean(), joined: z.boolean(),
    upstream: mediaRouteUpstreamSchema, publicationId: opaqueIdSchema.optional(), deafened: z.boolean().optional() }).strict())
    .max(MAX_PARTICIPANTS_PER_ROOM_LIMIT),
  publications: z.array(z.object({ id: opaqueIdSchema, peerId: opaqueIdSchema }).strict())
    .max(MAX_PARTICIPANTS_PER_ROOM_LIMIT),
  connections: z.array(z.object({ connectionId: opaqueIdSchema, parentPeerId: opaqueIdSchema.optional(),
    childPeerId: opaqueIdSchema, transport: z.enum(["peer", "sfu"]), pending: z.boolean(),
    revision: z.number().int().min(0).max(Number.MAX_SAFE_INTEGER),
    send: z.array(opaqueIdSchema).max(MAX_PARTICIPANTS_PER_ROOM_LIMIT),
    receive: z.array(opaqueIdSchema).max(MAX_PARTICIPANTS_PER_ROOM_LIMIT),
  }).strict()).max(MAX_PARTICIPANTS_PER_ROOM_LIMIT + 1),
  unavailable: z.array(opaqueIdSchema).max(MAX_PARTICIPANTS_PER_ROOM_LIMIT),
}).strict();
export type RoomVoiceState = z.infer<typeof roomVoiceStateSchema>;
export type VoiceConnection = RoomVoiceState["connections"][number];

export const punchPredictionSchema = z.object({
  address: z.ipv4(),
  port: z.number().int().min(1024).max(65535),
  step: z.number().int().min(0).max(16),
}).strict().refine(({ port, step }) => port + 4 * step <= 65535);
export type PunchPrediction = z.infer<typeof punchPredictionSchema>;

export const punchSignalSchema = z.discriminatedUnion("phase", [
  z.object({ phase: z.literal("prepare"), secret: z.string().regex(/^[0-9a-f]{64}$/) }).strict(),
  z.object({ phase: z.literal("ready"), prediction: punchPredictionSchema.optional() }).strict(),
  z.object({ phase: z.literal("start"), prediction: punchPredictionSchema }).strict(),
  z.object({ phase: z.literal("complete"), connected: z.boolean() }).strict(),
]);
export type PunchSignal = z.infer<typeof punchSignalSchema>;

export const signalPayloadSchema = z.discriminatedUnion("kind", [
  z.object({ kind: z.literal("punch"), connectionId: opaqueIdSchema, punch: punchSignalSchema }).strict(),
  z
    .object({
      kind: z.literal("description"),
      connectionId: opaqueIdSchema,
      description: sessionDescriptionSchema,
      punched: z.literal(true).optional(),
    })
    .strict(),
  z
    .object({
      kind: z.literal("candidate"),
      connectionId: opaqueIdSchema,
      candidate: iceCandidateSchema.nullable(),
    })
    .strict(),
]);
export type SignalPayload = z.infer<typeof signalPayloadSchema>;

export const sfuMediaSchema = z.object({
  codec: z.enum(["h264", "vp8"]),
  layers: z.array(z.object({
    rid: z.string().max(16),
    width: positiveUint32Schema,
    height: positiveUint32Schema,
    bitrate: positiveUint32Schema,
  }).strict()).min(1).max(3),
  audio: z.boolean(),
  audioBitrate: z.number().int().min(0).max(MAX_AUDIO_BITRATE),
}).strict().refine((media) =>
  (!media.audio || media.audioBitrate > 0) &&
  new Set(media.layers.map((layer) => layer.rid)).size === media.layers.length &&
  (media.layers.length === 1 || media.layers.every((layer) => layer.rid !== "")),
);
export type SfuMedia = z.infer<typeof sfuMediaSchema>;

export const sfuSignalMessageSchema = z.object({
  type: z.literal("sfu-signal"),
  revision: z.number().int().min(0).max(Number.MAX_SAFE_INTEGER),
  publicationGeneration: opaqueIdSchema,
  connectionId: opaqueIdSchema,
  kind: z.enum(["subscribe", "description", "candidate", "media", "layers"]),
  description: sessionDescriptionSchema.optional(),
  candidate: iceCandidateSchema.optional(),
  media: sfuMediaSchema.optional(),
  activeCount: z.number().int().min(0).max(3).optional(),
}).strict().refine((message) => {
  if (message.kind === "layers") return message.activeCount !== undefined && !message.description && !message.candidate && !message.media;
  if (message.activeCount !== undefined) return false;
  if (message.kind === "media") return Boolean(message.media) && !message.description && !message.candidate;
  if (message.kind === "subscribe") return !message.description && !message.candidate && !message.media;
  if (message.kind === "candidate") return !message.description && !message.media;
  return Boolean(message.description) && !message.candidate && (!message.media || message.description?.type === "offer");
});
export type SfuSignalMessage = z.infer<typeof sfuSignalMessageSchema>;

export const signalingChallengeSequenceSchema = z
  .number()
  .int()
  .min(0)
  .max(Number.MAX_SAFE_INTEGER);
