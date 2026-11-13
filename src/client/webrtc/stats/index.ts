// Connection statistics are split by responsibility under ./stats. This barrel
// preserves the single import path every caller already uses.

export {
  collectNativeSenderQualityFromReport,
  createNativeSenderQualityAccumulator,
} from "./sender-quality.js";
export {
  captureMetrics,
  decodedVideoFrames,
  highestActiveVideoRid,
  maxEncodedVideoFrames,
} from "./queries.js";
export { mediaSourceRecord } from "./records.js";
export {
  createStatsAccumulator,
  type StatsAccumulator,
} from "./accumulator.js";
export {
  collectConnectionMetrics,
  collectConnectionMetricsFromReport,
} from "./connection-metrics.js";
