import { describe, expect, it } from "vitest";
import { DEFAULT_QUALITY_SETTINGS, MAX_VIEWER_QUALITY_EVIDENCE_BYTES, clientMessageSchema, serverMessageSchema } from "../../src/shared/protocol.js";
import { roomId, qualitySettings, qualitySettingsWithAudio, legacyCodecQualitySettings, qualityEvidence } from "./support.ts";

describe("client signaling protocol", () => {
  it("keeps intentional pause updates strict and generation-bound", () => {
    const pause = {
      type: "set-sharing-paused",
      shareGeneration: "share_generation_12345678",
      paused: true,
    };
    expect(clientMessageSchema.safeParse(pause).success).toBe(true);
    expect(
      clientMessageSchema.safeParse({ ...pause, paused: false }).success,
    ).toBe(true);
    expect(
      clientMessageSchema.safeParse({ ...pause, shareGeneration: undefined })
        .success,
    ).toBe(false);
    expect(
      clientMessageSchema.safeParse({ ...pause, paused: "true" }).success,
    ).toBe(false);
    expect(
      clientMessageSchema.safeParse({ ...pause, unexpected: 1 }).success,
    ).toBe(false);
    expect(
      serverMessageSchema.safeParse({
        type: "host-status",
        online: true,
        paused: true,
      }).success,
    ).toBe(true);
    expect(
      serverMessageSchema.safeParse({ type: "host-status", online: true })
        .success,
    ).toBe(false);
    const authoritativePause = {
      type: "pause-sharing-source",
      shareGeneration: "share_generation_12345678",
    };
    expect(serverMessageSchema.safeParse(authoritativePause).success).toBe(true);
    expect(
      serverMessageSchema.safeParse({
        ...authoritativePause,
        unexpected: null,
      }).success,
    ).toBe(false);
  });

  it("separates numeric requests from capture implementation limits", () => {
    expect(DEFAULT_QUALITY_SETTINGS).not.toHaveProperty("videoCodec");
    expect(
      clientMessageSchema.safeParse({
        type: "set-quality-settings",
        qualitySettings,
      }).success,
    ).toBe(true);
    expect(
      clientMessageSchema.safeParse({
        type: "set-quality-settings",
        qualitySettings: qualitySettingsWithAudio,
      }).success,
    ).toBe(true);
    expect(
      clientMessageSchema.safeParse({
        type: "set-quality-settings",
        qualitySettings: { ...qualitySettings, resolution: { width: 854, height: 480 } },
      }).success,
    ).toBe(true);
    for (const maxAudioBitrate of [
      6_000, 96_000, 256_000, 320_000, 510_000,
    ] as const) {
      expect(
        clientMessageSchema.safeParse({
          type: "set-quality-settings",
          qualitySettings: { ...qualitySettings, maxAudioBitrate },
        }).success,
      ).toBe(true);
    }
    for (const next of [
      { ...qualitySettings, resolution: { width: 16384, height: 8192 } },
      { ...qualitySettings, resolution: { width: 1921, height: 1081 } },
      { ...qualitySettings, maxFramerate: 14 },
      { ...qualitySettings, maxFramerate: 61 },
      { ...qualitySettings, maxFramerate: 30.5 },
      { ...qualitySettings, maxBitrate: 1_999_999 },
      { ...qualitySettings, maxBitrate: 12_000_001 },
    ]) expect(clientMessageSchema.safeParse({ type: "set-quality-settings", qualitySettings: next }).success).toBe(true);
    for (const invalid of [
      { ...qualitySettings, resolution: "2160p" },
      { ...qualitySettings, resolution: { width: 0x1_0000_0000, height: 1080 } },
      { ...qualitySettings, maxFramerate: 0 },
      { ...qualitySettings, maxFramerate: Infinity },
      { ...qualitySettings, maxFramerate: Number.MAX_SAFE_INTEGER + 1 },
      { ...qualitySettings, maxBitrate: 0 },
      { ...qualitySettings, maxBitrate: 0x1_0000_0000 },
      { ...qualitySettings, maxBitrate: 5_000_000.5 },
      { ...qualitySettings, degradationPreference: "automatic" },
      legacyCodecQualitySettings,
      { ...qualitySettings, maxAudioBitrate: "lossless" },
      { ...qualitySettings, maxAudioBitrate: "music" },
      { ...qualitySettings, maxAudioBitrate: 0 },
      { ...qualitySettings, maxAudioBitrate: 5_999 },
      { ...qualitySettings, maxAudioBitrate: 510_001 },
      { ...qualitySettings, maxAudioBitrate: 128_000.5 },
      { ...qualitySettings, maxAudioBitrate: null },
      { ...qualitySettings, maxAudioBitrate: 960_000 },
      { ...qualitySettings, codec: "video/VP9" },
    ]) {
      expect(
        clientMessageSchema.safeParse({
          type: "set-quality-settings",
          qualitySettings: invalid,
        }).success,
      ).toBe(false);
    }
    const { maxFramerate: _missing, ...missingField } = qualitySettings;
    expect(
      clientMessageSchema.safeParse({
        type: "set-quality-settings",
        qualitySettings: missingField,
      }).success,
    ).toBe(false);
  });

  it("accepts only a bounded browser relay capacity", () => {
    expect(
      clientMessageSchema.safeParse({
        type: "relay-capacity",
        downstreamEdges: 0,
      }).success,
    ).toBe(true);
    expect(
      clientMessageSchema.safeParse({
        type: "relay-capacity",
        downstreamEdges: 1,
      }).success,
    ).toBe(true);
    expect(
      clientMessageSchema.safeParse({
        type: "relay-capacity",
        downstreamEdges: 2,
      }).success,
    ).toBe(true);
    expect(
      clientMessageSchema.safeParse({
        type: "relay-capacity",
        downstreamEdges: 3,
      }).success,
    ).toBe(true);
    expect(
      clientMessageSchema.safeParse({
        type: "relay-capacity",
        downstreamEdges: 4,
      }).success,
    ).toBe(false);
    expect(
      clientMessageSchema.safeParse({
        type: "relay-capacity",
        downstreamEdges: 1,
        score: 100,
      }).success,
    ).toBe(false);
  });

  it("accepts only strict, bounded viewer quality evidence", () => {
    expect(clientMessageSchema.safeParse(qualityEvidence).success).toBe(true);
    const { natTraversalPath: _path, ...legacyMetrics } =
      qualityEvidence.metrics;
    expect(
      clientMessageSchema.safeParse({
        ...qualityEvidence,
        metrics: legacyMetrics,
      }).success,
    ).toBe(false);
    expect(
      clientMessageSchema.safeParse({
        ...qualityEvidence,
        metrics: {
          ...qualityEvidence.metrics,
          freezeDurationMsDelta: 12_000,
          pauseDurationMsDelta: 6_000,
        },
      }).success,
    ).toBe(true);
    expect(
      clientMessageSchema.safeParse({
        ...qualityEvidence,
        metrics: {
          ...qualityEvidence.metrics,
          freezeCountDelta: null,
          freezeDurationMsDelta: null,
          pauseCountDelta: null,
          pauseDurationMsDelta: null,
        },
      }).success,
    ).toBe(true);
    expect(
      Buffer.byteLength(JSON.stringify(qualityEvidence), "utf8"),
    ).toBeLessThanOrEqual(MAX_VIEWER_QUALITY_EVIDENCE_BYTES);

    for (const invalid of [
      { ...qualityEvidence, roomId },
      {
        ...qualityEvidence,
        guard: { ...qualityEvidence.guard, peerId: "viewer_12345678" },
      },
      { ...qualityEvidence, windowMs: 999 },
      { ...qualityEvidence, sequence: -1 },
      {
        ...qualityEvidence,
        metrics: { ...qualityEvidence.metrics, width: null },
      },
      {
        ...qualityEvidence,
        metrics: { ...qualityEvidence.metrics, bitrateKbps: Number.POSITIVE_INFINITY },
      },
      {
        ...qualityEvidence,
        metrics: { ...qualityEvidence.metrics, freezeCountDelta: -1 },
      },
      {
        ...qualityEvidence,
        metrics: {
          ...qualityEvidence.metrics,
          freezeCountDelta: Number.MAX_SAFE_INTEGER + 1,
        },
      },
      {
        ...qualityEvidence,
        metrics: {
          ...qualityEvidence.metrics,
          freezeDurationMsDelta: Number.MAX_SAFE_INTEGER + 1,
        },
      },
      {
        ...qualityEvidence,
        metrics: { ...qualityEvidence.metrics, pauseCountDelta: -1 },
      },
      {
        ...qualityEvidence,
        metrics: {
          ...qualityEvidence.metrics,
          pauseDurationMsDelta: Number.MAX_SAFE_INTEGER + 1,
        },
      },
      {
        ...qualityEvidence,
        metrics: {
          ...qualityEvidence.metrics,
          codecParameters: "sprop-parameter-sets=deadbeef",
        },
      },
      {
        ...qualityEvidence,
        metrics: {
          ...qualityEvidence.metrics,
          codecProfile: "device-id=deadbeef",
        },
      },
      {
        ...qualityEvidence,
        metrics: {
          ...qualityEvidence.metrics,
          scalabilityMode: "DEVICE_ABC123",
        },
      },
      {
        ...qualityEvidence,
        metrics: {
          ...qualityEvidence.metrics,
          decoderImplementation: "device-specific-decoder",
        },
      },
    ]) {
      expect(clientMessageSchema.safeParse(invalid).success).toBe(false);
    }
  });

  it("accepts only native sender quality state with exact known identity", () => {
    const sender = {
      type: "sender-quality-evidence",
      childPeerId: "viewer_12345678",
      connectionId: "connection_12345678",
      rtpStatsId: "rtp-stats-1",
      trackIdentifier: "track-1",
      sampleTimestampMs: 2_000,
      routeRevision: 3,
      state: "degraded",
      diagnostics: {
        natTraversalPath: "ordinary",
        reason: "bandwidth",
        framesPerSecond: 24,
        bitrateKbps: 1_500,
      },
    } as const;
    expect(clientMessageSchema.safeParse(sender).success).toBe(true);
    const { natTraversalPath: _senderPath, ...legacyDiagnostics } =
      sender.diagnostics;
    expect(
      clientMessageSchema.safeParse({
        ...sender,
        diagnostics: legacyDiagnostics,
      }).success,
    ).toBe(false);
    expect(
      clientMessageSchema.safeParse({
        ...sender,
        state: "healthy",
        rtpStatsId: null,
        diagnostics: { ...sender.diagnostics, reason: "none" },
      }).success,
    ).toBe(false);
    expect(
      clientMessageSchema.safeParse({
        ...sender,
        sampleTimestampMs: null,
      }).success,
    ).toBe(false);
    expect(
      clientMessageSchema.safeParse({
        type: "sfu-publisher-quality-evidence",
        publicationGeneration: "publication_generation_12345678",
        routeRevision: 4,
        state: "healthy",
        sampleTimestampMs: 2_000,
        diagnostics: {
          natTraversalPath: "ordinary",
          reason: "none",
          framesPerSecond: 30,
          bitrateKbps: 2_000,
        },
      }).success,
    ).toBe(true);
  });

  it.each([
    ["video/VP8", null, "max-fr=60; max-fs=8160"],
    ["video/VP9", "profile-id=2", "max-fs=8160"],
    ["video/AV1", "profile=1", "level-idx=31; tier=0"],
    ["video/unknown", null, null],
  ])(
    "accepts canonical viewer codec evidence for %s",
    (codec, codecProfile, codecParameters) => {
      expect(
        clientMessageSchema.safeParse({
          ...qualityEvidence,
          metrics: {
            ...qualityEvidence.metrics,
            codec,
            codecProfile,
            codecParameters,
          },
        }).success,
      ).toBe(true);
    },
  );
});
