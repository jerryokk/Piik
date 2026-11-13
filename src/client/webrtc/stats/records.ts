import { deriveVideoCodecEvidence } from "../../../shared/video-codec-evidence";
import type { ConnectionMetrics } from "../../types";

// Read-only accessors over a browser RTCStatsReport. No aggregation state lives
// here so every consumer reads the same fields and identity rules.

export type StatsRecord = Record<string, unknown> & {
  id: string;
  type: string;
  timestamp: number;
};

export type QualityLimitationDurations = Record<
  "none" | "bandwidth" | "cpu" | "other",
  number
>;

export interface StatsMediaSelector {
  kind?: "audio" | "video";
  trackIdentifier: string | null;
  rid?: string | null;
  audioTrackIdentifier?: string | null;
}

export function numberValue(record: StatsRecord | null, key: string): number | null {
  const value = record?.[key];
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

export function stringValue(record: StatsRecord | null, key: string): string | null {
  const value = record?.[key];
  return typeof value === "string" && value.length > 0 ? value : null;
}

export function qualityLimitationDurationsValue(
  record: StatsRecord | null,
): QualityLimitationDurations | null {
  const value = record?.qualityLimitationDurations;
  if (!value || typeof value !== "object") {
    return null;
  }
  const durations = value as Record<string, unknown>;
  const result = {} as QualityLimitationDurations;
  for (const reason of ["none", "bandwidth", "cpu", "other"] as const) {
    const duration = durations[reason];
    if (typeof duration !== "number" || !Number.isFinite(duration) || duration < 0) {
      return null;
    }
    result[reason] = duration;
  }
  return result;
}

export function nativeEdgeQualityState(
  reason: string | null,
  current: QualityLimitationDurations | null,
  previous: QualityLimitationDurations | null,
  hasInterval: boolean,
): ConnectionMetrics["nativeEdgeQualityState"] {
  if (!hasInterval || !current || !previous) {
    return "unknown";
  }
  const deltas = {} as QualityLimitationDurations;
  for (const key of ["none", "bandwidth", "cpu", "other"] as const) {
    if (current[key] < previous[key]) {
      return "unknown";
    }
    deltas[key] = current[key] - previous[key];
  }
  if (
    reason === "none" &&
    deltas.none > 0 &&
    deltas.bandwidth === 0 &&
    deltas.cpu === 0 &&
    deltas.other === 0
  ) {
    return "healthy";
  }
  if (
    (reason === "bandwidth" || reason === "cpu") &&
    deltas[reason] > 0 &&
    deltas.none === 0 &&
    deltas.other === 0 &&
    deltas[reason === "bandwidth" ? "cpu" : "bandwidth"] === 0
  ) {
    return "degraded";
  }
  return "unknown";
}

export function booleanValue(record: StatsRecord | null, key: string): boolean | null {
  const value = record?.[key];
  return typeof value === "boolean" ? value : null;
}

export function getRecord(report: RTCStatsReport, id: string | null): StatsRecord | null {
  if (!id) {
    return null;
  }
  return (report.get(id) as StatsRecord | undefined) ?? null;
}

export function mediaTrackIdentifier(
  report: RTCStatsReport,
  media: StatsRecord,
  direction: "send" | "receive",
): string | null {
  if (direction === "receive") {
    return stringValue(media, "trackIdentifier");
  }
  const source = mediaSourceRecord(report, media);
  return stringValue(source, "trackIdentifier");
}

export function mediaSourceRecord(
  report: RTCStatsReport,
  media: StatsRecord | null,
): StatsRecord | null {
  if (!media || media.type !== "outbound-rtp" || media.isRemote === true) return null;
  if (media.mediaSourceId !== undefined) {
    const source = getRecord(report, stringValue(media, "mediaSourceId"));
    return source?.type === "media-source" ? source : null;
  }
  if (media.kind !== "video" && media.kind !== "audio") return null;
  // Some browsers omit the link. Infer it only for one sender and one source
  // of the same kind; stale/multiple senders or sources remain ambiguous.
  const senders: StatsRecord[] = [];
  const sources: StatsRecord[] = [];
  report.forEach((raw) => {
    const record = raw as StatsRecord;
    if (record.kind !== media.kind) return;
    if (record.type === "outbound-rtp" && record.isRemote !== true) senders.push(record);
    if (record.type === "media-source") sources.push(record);
  });
  return senders.length === 1 && senders[0]!.id === media.id && sources.length === 1
    ? sources[0]!
    : null;
}

export function mediaRecord(
  report: RTCStatsReport,
  direction: "send" | "receive",
  selector: StatsMediaSelector | null,
  kind: "audio" | "video" = "video",
): StatsRecord | null {
  const expectedType = direction === "send" ? "outbound-rtp" : "inbound-rtp";
  const isExpectedMedia = (record: StatsRecord): boolean =>
    record.type === expectedType &&
    record.kind === kind &&
    record.isRemote !== true;

  const candidates: StatsRecord[] = [];
  report.forEach((raw) => {
    const record = raw as StatsRecord;
    if (isExpectedMedia(record)) {
      candidates.push(record);
    }
  });
  const selectedTrackIdentifier = selector?.trackIdentifier ?? null;
  const selectedRid = selector?.rid ?? null;
  const narrowed = candidates.filter(
    (candidate) =>
      (selectedTrackIdentifier === null ||
        mediaTrackIdentifier(report, candidate, direction) ===
          selectedTrackIdentifier) &&
      (selectedRid === null || stringValue(candidate, "rid") === selectedRid),
  );
  return narrowed.length === 1 ? narrowed[0]! : null;
}

export function transportRecord(
  report: RTCStatsReport,
  media: StatsRecord | null,
): StatsRecord | null {
  const referenced = getRecord(report, stringValue(media, "transportId"));
  return referenced?.type === "transport" ? referenced : null;
}

export function selectedCandidatePair(
  report: RTCStatsReport,
  transport: StatsRecord | null,
): StatsRecord | null {
  if (!transport) {
    return null;
  }
  const candidatePairs: StatsRecord[] = [];
  report.forEach((raw) => {
    const record = raw as StatsRecord;
    if (record.type === "candidate-pair") {
      candidatePairs.push(record);
    }
  });

  const pairId = stringValue(transport, "selectedCandidatePairId");
  if (pairId) {
    const referenced = getRecord(report, pairId);
    return referenced?.type === "candidate-pair" &&
      stringValue(referenced, "transportId") === transport.id
      ? referenced
      : null;
  }
  const selected = candidatePairs.filter(
    (pair) =>
      stringValue(pair, "transportId") === transport.id &&
      pair.state === "succeeded" &&
      (pair.nominated === true || pair.selected === true),
  );
  return selected.length === 1 ? selected[0]! : null;
}

export function linkedRemoteInbound(
  report: RTCStatsReport,
  outbound: StatsRecord | null,
  kind: "audio" | "video" = "video",
): StatsRecord | null {
  const remote = getRecord(report, stringValue(outbound, "remoteId"));
  return remote?.type === "remote-inbound-rtp" && remote.kind === kind
    ? remote
    : null;
}

export function linkedMediaCodec(
  report: RTCStatsReport,
  media: StatsRecord | null,
  transport: StatsRecord | null,
  kind: "audio" | "video",
): StatsRecord | null {
  const codec = getRecord(report, stringValue(media, "codecId"));
  const mimeType = stringValue(codec, "mimeType");
  return codec?.type === "codec" &&
    transport !== null &&
    stringValue(codec, "transportId") === transport.id &&
    mimeType !== null &&
    new RegExp(`^${kind}\\/[A-Za-z0-9.+-]{1,32}$`, "i").test(mimeType)
    ? codec
    : null;
}

export function deriveCodecEvidence(codec: StatsRecord | null) {
  const mimeType = stringValue(codec, "mimeType");
  return deriveVideoCodecEvidence(
    mimeType,
    stringValue(codec, "sdpFmtpLine"),
  );
}

export function positiveIntegerValue(
  record: StatsRecord | null,
  key: string,
): number | null {
  const value = numberValue(record, key);
  return value !== null && Number.isInteger(value) && value > 0 ? value : null;
}

export function candidateAddressValue(record: StatsRecord | null): string | null {
  const value = stringValue(record, "address");
  return value !== null &&
    value.length <= 255 &&
    !/[\u0000-\u0020\u007f]/.test(value)
    ? value
    : null;
}

export function candidatePortValue(record: StatsRecord | null): number | null {
  const value = numberValue(record, "port");
  return value !== null &&
    Number.isInteger(value) &&
    value > 0 &&
    value <= 65_535
    ? value
    : null;
}

export function audioCodecParameters(codec: StatsRecord | null): string | null {
  const value = stringValue(codec, "sdpFmtpLine");
  return value !== null && value.length <= 512 && /^[\x20-\x7e]+$/.test(value)
    ? value
    : null;
}

export function scalabilityModeValue(media: StatsRecord | null): string | null {
  const value = stringValue(media, "scalabilityMode");
  return value && /^[A-Za-z0-9_-]{1,32}$/.test(value) ? value : null;
}
