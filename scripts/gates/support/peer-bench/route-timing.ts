// Route-timing distributions and the check that requires complete
// current-child samples. Timing never decides route status by threshold.

import {
  MAX_VIEWERS,
  ROUTE_TIMING_KEYS,
  type BenchmarkRouteTimingSamples,
  type BenchmarkRouteTimingSummary,
  type BenchmarkRun,
  type RouteTimingDistribution,
  type RunCheck,
} from "./types";

export function summarizeBenchmarkRouteTiming(
  samples: BenchmarkRouteTimingSamples,
): BenchmarkRouteTimingSummary {
  return Object.fromEntries(
    ROUTE_TIMING_KEYS.map((key) => [
      key,
      summarizeRouteTiming(samples[key]),
    ]),
  ) as BenchmarkRouteTimingSummary;
}

export function summarizeRouteTiming(
  values: readonly (number | null)[],
): RouteTimingDistribution {
  const rawMs = values
    .filter((value): value is number => value !== null)
    .toSorted((left, right) => left - right);
  return {
    sampleCount: rawMs.length,
    pendingCount: values.length - rawMs.length,
    rawMs,
    p50Ms: nearestRank(rawMs, 0.5),
    p95Ms: nearestRank(rawMs, 0.95),
    maxMs: rawMs.at(-1) ?? null,
  };
}

export function nearestRank(
  sortedValues: readonly number[],
  percentile: number,
): number | null {
  if (sortedValues.length === 0) return null;
  return sortedValues[Math.ceil(percentile * sortedValues.length) - 1] ?? null;
}

export function buildRouteTimingCheck(
  status: BenchmarkRun["routeTimingStatus"],
  summary: BenchmarkRouteTimingSummary | null,
  expectedChildCount = MAX_VIEWERS,
): RunCheck {
  const completeDistributionCount =
    status === "captured" && summary
      ? ROUTE_TIMING_KEYS.filter((key) => {
          const distribution = summary[key];
          return (
            distribution.sampleCount + distribution.pendingCount ===
            expectedChildCount
          );
        }).length
      : 0;
  return {
    name: "route-timing-current-children",
    passed: completeDistributionCount === ROUTE_TIMING_KEYS.length,
    actual: completeDistributionCount,
    expected:
      `${ROUTE_TIMING_KEYS.length} captured timing distributions with ` +
      `${expectedChildCount} current-child sample or pending values each`,
  };
}
