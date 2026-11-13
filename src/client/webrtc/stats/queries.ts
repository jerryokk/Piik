import { senderCaptureTrack } from "../../media/sender-video-track";
import type { ConnectionMetrics } from "../../types";
import {
  mediaRecord,
  mediaTrackIdentifier,
  numberValue,
  stringValue,
  type StatsMediaSelector,
  type StatsRecord,
} from "./records.js";

// Small, read-only projections used by probe and startup paths.

export function captureMetrics(
  track: MediaStreamTrack,
): Pick<
  ConnectionMetrics,
  "captureWidth" | "captureHeight" | "captureFramesPerSecond"
> {
  try {
    const settings = senderCaptureTrack(track).getSettings();
    const finite = (value: unknown): number | null =>
      typeof value === "number" && Number.isFinite(value) ? value : null;
    return {
      captureWidth: finite(settings.width),
      captureHeight: finite(settings.height),
      captureFramesPerSecond: finite(settings.frameRate),
    };
  } catch {
    return {
      captureWidth: null,
      captureHeight: null,
      captureFramesPerSecond: null,
    };
  }
}

export function decodedVideoFrames(
  report: RTCStatsReport,
  selector: StatsMediaSelector | null = null,
): number | null {
  return numberValue(mediaRecord(report, "receive", selector), "framesDecoded");
}

export function maxEncodedVideoFrames(
  report: RTCStatsReport,
  trackIdentifier: string,
): number | null {
  // An absent current-track counter is unknown, not a fresh encoder at zero.
  let frames: number | null = null;
  report.forEach((raw) => {
    const record = raw as StatsRecord;
    if (
      record.type === "outbound-rtp" &&
      record.kind === "video" &&
      record.isRemote !== true &&
      mediaTrackIdentifier(report, record, "send") === trackIdentifier
    ) {
      const encodedFrames = numberValue(record, "framesEncoded");
      if (encodedFrames !== null) {
        frames = Math.max(frames ?? 0, encodedFrames);
      }
    }
  });
  return frames;
}

export function highestActiveVideoRid(
  report: RTCStatsReport,
  trackIdentifier: string,
): string | null {
  let selectedRid: string | null = null;
  let selectedPixels = -1;
  report.forEach((raw) => {
    const record = raw as StatsRecord;
    if (
      record.type !== "outbound-rtp" ||
      record.kind !== "video" ||
      record.isRemote === true ||
      record.active === false ||
      mediaTrackIdentifier(report, record, "send") !== trackIdentifier
    ) {
      return;
    }
    const rid = stringValue(record, "rid");
    const width = numberValue(record, "frameWidth");
    const height = numberValue(record, "frameHeight");
    if (!rid || width === null || height === null || width <= 0 || height <= 0) {
      return;
    }
    const pixels = width * height;
    if (pixels > selectedPixels) {
      selectedRid = rid;
      selectedPixels = pixels;
    }
  });
  return selectedRid;
}
