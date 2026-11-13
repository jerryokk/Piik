// Environment parsing for the peer benchmark. Every value a run measures comes
// from here, never from an operator shell default.

import { QUALITY_PROFILES } from "../../../../src/client/media/quality";
import {
  DEFAULT_ENDPOINT_MEDIA_COPY_CAPACITY,
  MAX_ENDPOINT_MEDIA_COPY_CAPACITY,
} from "../../../../src/shared/media-copy-accounting";
import {
  DEFAULT_VIEWER_COUNTS,
  MAX_VIEWERS,
  type BenchmarkCodecMode,
  type BenchmarkConfig,
  type ExternalServerTarget,
  type ProfileId,
} from "./types";

export const PROFILE_SETTINGS = QUALITY_PROFILES;

export function parseViewerCounts(value: string | undefined): number[] {
  if (!value?.trim()) {
    return [...DEFAULT_VIEWER_COUNTS];
  }
  const result: number[] = [];
  for (const raw of value.split(",")) {
    const parsed = Number(raw.trim());
    if (!Number.isInteger(parsed) || parsed < 1 || parsed > MAX_VIEWERS) {
      throw new Error(
        `BENCHMARK_VIEWERS must contain integers from 1 to ${MAX_VIEWERS}`,
      );
    }
    if (!result.includes(parsed)) {
      result.push(parsed);
    }
  }
  if (result.length === 0) {
    throw new Error("BENCHMARK_VIEWERS must not be empty");
  }
  return result;
}

export function parseNumber(
  value: string | undefined,
  fallback: number,
  name: string,
  minimum: number,
  maximum: number,
): number {
  if (!value?.trim()) {
    return fallback;
  }
  const parsed = Number(value);
  if (!Number.isFinite(parsed) || parsed < minimum || parsed > maximum) {
    throw new Error(`${name} must be between ${minimum} and ${maximum}`);
  }
  return parsed;
}

export function parseBoolean(value: string | undefined, fallback: boolean, name: string): boolean {
  if (!value?.trim()) {
    return fallback;
  }
  if (value === "true" || value === "1") {
    return true;
  }
  if (value === "false" || value === "0") {
    return false;
  }
  throw new Error(`${name} must be true, false, 1, or 0`);
}

export function parseBenchmarkCodecMode(
  value: string | undefined,
): BenchmarkCodecMode {
  const mode = value?.trim().toLowerCase() || "auto";
  if (mode === "vp8" || mode === "auto" || mode === "h264") return mode;
  throw new Error("BENCHMARK_CODEC_MODE must be vp8, auto, or h264");
}

export function parseExpectedEndpointCap(value: string | undefined): number {
  const parsed = value?.trim()
    ? Number(value)
    : DEFAULT_ENDPOINT_MEDIA_COPY_CAPACITY;
  if (
    !Number.isSafeInteger(parsed) ||
    parsed < 1 ||
    parsed > MAX_ENDPOINT_MEDIA_COPY_CAPACITY
  ) {
    throw new Error(
      `BENCHMARK_EXPECTED_ENDPOINT_CAP must be an integer from 1 to ${MAX_ENDPOINT_MEDIA_COPY_CAPACITY}`,
    );
  }
  return parsed;
}

export function parseBenchmarkConfig(
  environment: NodeJS.ProcessEnv = process.env,
): BenchmarkConfig {
  const chromePath = environment.CHROME_PATH?.trim();
  if (!chromePath) {
    throw new Error("CHROME_PATH must point to a Chromium or Chrome executable");
  }
  const profileId = (environment.BENCHMARK_PROFILE?.trim() || "720p30") as ProfileId;
  if (!(profileId in PROFILE_SETTINGS)) {
    throw new Error("BENCHMARK_PROFILE must be 1080p60, 1080p30, or 720p30");
  }
  const viewerCounts = parseViewerCounts(environment.BENCHMARK_VIEWERS);
  const qualityControlSmoke = parseBoolean(
    environment.BENCHMARK_QUALITY_SMOKE,
    false,
    "BENCHMARK_QUALITY_SMOKE",
  );
  if (qualityControlSmoke && !viewerCounts.some((count) => count >= 3)) {
    throw new Error(
      "BENCHMARK_QUALITY_SMOKE requires a selected viewer count of at least 3",
    );
  }
  const recoveryText = environment.BENCHMARK_RECOVERY_VIEWERS?.trim();
  const recoveryViewerCount = recoveryText ? Number(recoveryText) : null;
  if (
    recoveryViewerCount !== null &&
    (!Number.isInteger(recoveryViewerCount) ||
      recoveryViewerCount < 3 ||
      !viewerCounts.includes(recoveryViewerCount))
  ) {
    throw new Error(
      "BENCHMARK_RECOVERY_VIEWERS must be a selected viewer count of at least 3",
    );
  }
  return {
    chromePath,
    viewerCounts,
    expectedEndpointCap: parseExpectedEndpointCap(
      environment.BENCHMARK_EXPECTED_ENDPOINT_CAP,
    ),
    profileId,
    codecMode: parseBenchmarkCodecMode(environment.BENCHMARK_CODEC_MODE),
    durationMs:
      parseNumber(
        environment.BENCHMARK_DURATION_SECONDS,
        30,
        "BENCHMARK_DURATION_SECONDS",
        2,
        7_200,
      ) * 1_000,
    settleMs:
      parseNumber(
        environment.BENCHMARK_SETTLE_SECONDS,
        5,
        "BENCHMARK_SETTLE_SECONDS",
        0,
        120,
      ) * 1_000,
    sampleIntervalMs: parseNumber(
      environment.BENCHMARK_SAMPLE_INTERVAL_MS,
      2_000,
      "BENCHMARK_SAMPLE_INTERVAL_MS",
      500,
      10_000,
    ),
    connectionTimeoutMs:
      parseNumber(
        environment.BENCHMARK_CONNECTION_TIMEOUT_SECONDS,
        // Includes the whole burst's queue: the room admits one child at a time.
        90,
        "BENCHMARK_CONNECTION_TIMEOUT_SECONDS",
        3,
        120,
      ) * 1_000,
    recoveryViewerCount,
    recoveryTimeoutMs:
      parseNumber(
        environment.BENCHMARK_RECOVERY_TIMEOUT_SECONDS,
        20,
        "BENCHMARK_RECOVERY_TIMEOUT_SECONDS",
        8,
        120,
      ) * 1_000,
    outputPath:
      environment.BENCHMARK_OUTPUT?.trim() === "-"
        ? null
        : environment.BENCHMARK_OUTPUT?.trim() ||
          "benchmark-results/peer-assisted.json",
    headless: parseBoolean(environment.BENCHMARK_HEADLESS, true, "BENCHMARK_HEADLESS"),
    noSandbox: parseBoolean(
      environment.BENCHMARK_CHROME_NO_SANDBOX,
      false,
      "BENCHMARK_CHROME_NO_SANDBOX",
    ),
    qualityControlSmoke,
  };
}

export function parseExternalServerTarget(
  environment: NodeJS.ProcessEnv = process.env,
): ExternalServerTarget | null {
  const value = environment.BENCHMARK_SERVER_URL?.trim();
  if (!value) return null;
  let parsed: URL;
  try {
    parsed = new URL(value);
  } catch {
    throw new Error("BENCHMARK_SERVER_URL must be an HTTP or HTTPS origin");
  }
  if (
    (parsed.protocol !== "http:" && parsed.protocol !== "https:") ||
    parsed.username ||
    parsed.password ||
    parsed.pathname !== "/" ||
    parsed.search ||
    parsed.hash
  ) {
    throw new Error("BENCHMARK_SERVER_URL must be an HTTP or HTTPS origin");
  }
  return {
    origin: parsed.origin,
    accessPassword: environment.BENCHMARK_SITE_ACCESS_PASSWORD?.trim() || null,
  };
}
