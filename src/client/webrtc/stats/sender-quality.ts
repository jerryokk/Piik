import type { ConnectionMetrics } from "../../types";
import {
  mediaTrackIdentifier,
  nativeEdgeQualityState,
  numberValue,
  qualityLimitationDurationsValue,
  stringValue,
  type QualityLimitationDurations,
  type StatsRecord,
} from "./records.js";

// Sender-side quality evidence is derived from native (App) outbound senders.

interface NativeSenderQualityPrevious {
  timestamp: number;
  durations: QualityLimitationDurations | null;
  framesEncoded: number | null;
  bytesSent: number | null;
}

interface NativeSenderQualityAccumulator {
  previousByStatsId: Map<string, NativeSenderQualityPrevious>;
}

export function createNativeSenderQualityAccumulator(): NativeSenderQualityAccumulator {
  return { previousByStatsId: new Map() };
}

export function collectNativeSenderQualityFromReport(
  report: RTCStatsReport,
  trackIdentifier: string,
  accumulator: NativeSenderQualityAccumulator,
): Pick<
  ConnectionMetrics,
  | "sampleTimestampMs"
  | "nativeEdgeQualityState"
  | "qualityLimitationReason"
  | "sampleWindowMs"
  | "intervalFramesEncoded"
  | "bitrateKbps"
> {
  const records: StatsRecord[] = [];
  report.forEach((raw) => {
    const record = raw as StatsRecord;
    if (
      record.type === "outbound-rtp" &&
      record.kind === "video" &&
      record.isRemote !== true &&
      record.active !== false &&
      mediaTrackIdentifier(report, record, "send") === trackIdentifier
    ) {
      records.push(record);
    }
  });
  const activeIds = new Set(records.map((record) => record.id));
  for (const id of accumulator.previousByStatsId.keys()) {
    if (!activeIds.has(id)) accumulator.previousByStatsId.delete(id);
  }
  const samples = records.map((record) => {
    const timestamp = numberValue(record, "timestamp");
    const durations = qualityLimitationDurationsValue(record);
    const reason = stringValue(record, "qualityLimitationReason");
    const framesEncoded = numberValue(record, "framesEncoded");
    const bytesSent = numberValue(record, "bytesSent");
    const previous = accumulator.previousByStatsId.get(record.id);
    const windowMs =
      timestamp !== null && previous && timestamp > previous.timestamp
        ? timestamp - previous.timestamp
        : null;
    const state = nativeEdgeQualityState(
      reason,
      durations,
      previous?.durations ?? null,
      windowMs !== null,
    );
    const intervalFramesEncoded =
      previous &&
      framesEncoded !== null &&
      previous.framesEncoded !== null &&
      framesEncoded >= previous.framesEncoded
        ? framesEncoded - previous.framesEncoded
        : null;
    const bitrateKbps =
      previous &&
      windowMs !== null &&
      bytesSent !== null &&
      previous.bytesSent !== null &&
      bytesSent >= previous.bytesSent
        ? ((bytesSent - previous.bytesSent) * 8) / (windowMs / 1_000) / 1_000
        : null;
    if (timestamp !== null) {
      accumulator.previousByStatsId.set(record.id, {
        timestamp,
        durations,
        framesEncoded,
        bytesSent,
      });
    } else {
      accumulator.previousByStatsId.delete(record.id);
    }
    return {
      timestamp,
      state,
      reason,
      windowMs,
      intervalFramesEncoded,
      bitrateKbps,
    };
  });
  const bitrateKbps =
    samples.length > 0 && samples.every((sample) => sample.bitrateKbps !== null)
      ? samples.reduce((total, sample) => total + sample.bitrateKbps!, 0)
      : null;
  if (
    samples.length === 0 ||
    samples.some(
      (sample) =>
        sample.state === "unknown" ||
        sample.timestamp === null ||
        sample.windowMs === null ||
        sample.intervalFramesEncoded === null,
    )
  ) {
    return {
      sampleTimestampMs: null,
      nativeEdgeQualityState: "unknown",
      qualityLimitationReason: null,
      sampleWindowMs: null,
      intervalFramesEncoded: null,
      bitrateKbps,
    };
  }
  const degraded = samples.filter((sample) => sample.state === "degraded");
  return {
    sampleTimestampMs: Math.min(...samples.map((sample) => sample.timestamp!)),
    nativeEdgeQualityState: degraded.length > 0 ? "degraded" : "healthy",
    qualityLimitationReason:
      degraded.some((sample) => sample.reason === "bandwidth")
        ? "bandwidth"
        : degraded.length > 0
          ? "cpu"
          : "none",
    sampleWindowMs: Math.min(...samples.map((sample) => sample.windowMs!)),
    intervalFramesEncoded: samples.reduce(
      (total, sample) => total + sample.intervalFramesEncoded!,
      0,
    ),
    bitrateKbps,
  };
}
