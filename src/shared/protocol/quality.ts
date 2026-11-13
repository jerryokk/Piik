import { z } from "zod";
import { positiveUint32Schema } from "./primitives.js";

export const qualityResolutionSchema = z.object({
  width: positiveUint32Schema,
  height: positiveUint32Schema,
}).strict();
export type QualityResolution = z.infer<typeof qualityResolutionSchema>;

export const degradationPreferenceSchema = z.enum([
  "maintain-resolution",
  "balanced",
  "maintain-framerate",
]);
export type DegradationPreference = z.infer<
  typeof degradationPreferenceSchema
>;

export const MIN_AUDIO_BITRATE = 6_000;
export const MAX_AUDIO_BITRATE = 510_000;
export const maxAudioBitrateSchema = z.number().int().min(MIN_AUDIO_BITRATE).max(MAX_AUDIO_BITRATE);

export const qualitySettingsSchema = z
  .object({
    resolution: qualityResolutionSchema,
    maxFramerate: z.number().positive().max(Number.MAX_SAFE_INTEGER),
    maxBitrate: positiveUint32Schema,
    degradationPreference: degradationPreferenceSchema,
    maxAudioBitrate: maxAudioBitrateSchema.optional(),
  })
  .strict();
export type QualitySettings = z.infer<typeof qualitySettingsSchema>;
export const DEFAULT_QUALITY_SETTINGS = {
  resolution: { width: 1920, height: 1080 },
  maxFramerate: 30,
  maxBitrate: 5_000_000,
  degradationPreference: "balanced",
  maxAudioBitrate: 128000,
} as const satisfies QualitySettings;

export const routePolicySchema = z
  .object({
    peerOnly: z.boolean(),
    topologyOptimization: z.boolean(),
    natPrediction: z.boolean(),
  })
  .strict();
export type RoutePolicy = z.infer<typeof routePolicySchema>;
export const viewerRoutePreferenceSchema = z.enum(["auto", "p2p", "sfu"]);
export type ViewerRoutePreference = z.infer<typeof viewerRoutePreferenceSchema>;
export const DEFAULT_ROUTE_POLICY = {
  peerOnly: false,
  topologyOptimization: true,
  natPrediction: false,
} as const satisfies RoutePolicy;
