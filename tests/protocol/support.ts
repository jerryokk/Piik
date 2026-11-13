
export const token = "a".repeat(43);

export const roomId = "1234";

export const viewerGrant = `${"b".repeat(21)}g`;

export const qualitySettings = {
  resolution: { width: 1920, height: 1080 },
  maxFramerate: 60,
  maxBitrate: 8_000_000,
  degradationPreference: "maintain-resolution",
} as const;

export const qualitySettingsWithAudio = {
  ...qualitySettings,
  maxAudioBitrate: 128000,
} as const;

export const legacyCodecQualitySettings = {
  ...qualitySettings,
  videoCodec: "vp8",
} as const;

export const qualityEvidence = {
  type: "viewer-quality-evidence",
  guard: {
    connectionId: "connection_12345678",
    routeRevision: 0,
    presentationEpoch: 0,
  },
  sequence: 0,
  windowMs: 2_000,
  metrics: {
    natTraversalPath: "ordinary",
    width: 1_920,
    height: 1_080,
    framesPerSecond: 59.8,
    bitrateKbps: 7_500,
    packetsReceivedDelta: 1_500,
    packetsLostDelta: 2,
    rttMs: 18,
    jitterMs: 3.5,
    framesDecodedDelta: 120,
    framesDroppedDelta: 1,
    decodeMsPerFrame: 2.4,
    freezeCountDelta: 0,
    freezeDurationMsDelta: 0,
    pauseCountDelta: 0,
    pauseDurationMsDelta: 0,
    codec: "video/H264",
    codecProfile: "profile-level-id=42e01f",
    codecParameters:
      "packetization-mode=1; level-asymmetry-allowed=1",
    audioBitrateKbps: 192,
    audioPacketLossPercent: 0.2,
    audioJitterMs: 2.5,
    audioVideoPlayoutDeltaMs: -12.5,
    videoJitterBufferDelayMs: 24,
    audioJitterBufferDelayMs: 18,
    audioConcealedSamplesPercent: 1,
    audioConcealmentEventsDelta: 3,
    audioCodec: "audio/opus",
  },
} as const;
