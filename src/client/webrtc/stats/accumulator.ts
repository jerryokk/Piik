import type { QualityLimitationDurations } from "./records.js";

// Per-connection interval bookkeeping for delta-based metrics.

export interface StatsAccumulator {
  mediaId: string | null;
  ssrc: number | null;
  trackIdentifier: string | null;
  lossSourceId: string | null;
  lossReportTimestamp: number | null;
  bytes: number | null;
  frames: number | null;
  timestamp: number | null;
  previousTotalEncodeTime: number | null;
  previousTotalDecodeTime: number | null;
  previousPacketsSent: number | null;
  previousPacketsReceived: number | null;
  previousPacketsLost: number | null;
  previousFramesDropped: number | null;
  previousFreezeCount: number | null;
  previousTotalFreezesDuration: number | null;
  previousPauseCount: number | null;
  previousTotalPausesDuration: number | null;
  previousRetransmittedPackets: number | null;
  previousRetransmittedBytes: number | null;
  previousVideoJitterBufferDelay: number | null;
  previousVideoJitterBufferEmittedCount: number | null;
  previousQualityLimitationDurations: QualityLimitationDurations | null;
  audioMediaId: string | null;
  audioSsrc: number | null;
  audioTrackIdentifier: string | null;
  audioLossSourceId: string | null;
  audioBytes: number | null;
  audioTimestamp: number | null;
  previousAudioPacketsReceived: number | null;
  previousAudioPacketsLost: number | null;
  previousAudioJitterBufferDelay: number | null;
  previousAudioJitterBufferEmittedCount: number | null;
  previousAudioTotalSamplesReceived: number | null;
  previousAudioConcealedSamples: number | null;
  previousAudioConcealmentEvents: number | null;
}

export function createStatsAccumulator(): StatsAccumulator {
  return {
    mediaId: null,
    ssrc: null,
    trackIdentifier: null,
    lossSourceId: null,
    lossReportTimestamp: null,
    bytes: null,
    frames: null,
    timestamp: null,
    previousTotalEncodeTime: null,
    previousTotalDecodeTime: null,
    previousPacketsSent: null,
    previousPacketsReceived: null,
    previousPacketsLost: null,
    previousFramesDropped: null,
    previousFreezeCount: null,
    previousTotalFreezesDuration: null,
    previousPauseCount: null,
    previousTotalPausesDuration: null,
    previousRetransmittedPackets: null,
    previousRetransmittedBytes: null,
    previousVideoJitterBufferDelay: null,
    previousVideoJitterBufferEmittedCount: null,
    previousQualityLimitationDurations: null,
    audioMediaId: null,
    audioSsrc: null,
    audioTrackIdentifier: null,
    audioLossSourceId: null,
    audioBytes: null,
    audioTimestamp: null,
    previousAudioPacketsReceived: null,
    previousAudioPacketsLost: null,
    previousAudioJitterBufferDelay: null,
    previousAudioJitterBufferEmittedCount: null,
    previousAudioTotalSamplesReceived: null,
    previousAudioConcealedSamples: null,
    previousAudioConcealmentEvents: null,
  };
}

export function intervalDelta(
  current: number | null,
  previous: number | null,
  hasInterval: boolean,
): number | null {
  if (
    !hasInterval ||
    current === null ||
    previous === null ||
    current < previous
  ) {
    return null;
  }
  return current - previous;
}

export function intervalAverageMs(
  totalTime: number | null,
  previousTotalTime: number | null,
  frames: number | null,
  previousFrames: number | null,
): number | null {
  if (
    totalTime === null ||
    previousTotalTime === null ||
    frames === null ||
    previousFrames === null
  ) {
    return null;
  }

  const frameDelta = frames - previousFrames;
  const timeDelta = totalTime - previousTotalTime;
  return frameDelta > 0 && timeDelta >= 0
    ? (timeDelta / frameDelta) * 1_000
    : null;
}

export function percentOfInterval(
  partDelta: number | null,
  totalDelta: number | null,
): number | null {
  if (
    partDelta === null ||
    totalDelta === null ||
    !Number.isFinite(partDelta) ||
    !Number.isFinite(totalDelta) ||
    partDelta < 0 ||
    totalDelta <= 0 ||
    partDelta > totalDelta
  ) {
    return null;
  }
  return (partDelta / totalDelta) * 100;
}
