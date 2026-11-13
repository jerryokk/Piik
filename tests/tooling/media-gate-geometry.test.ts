import { expect, it } from "vitest";
import { fittedCaptureSize, matchesCaptureSize, simulcastCaptureSizes } from "../../scripts/gates/support/media-gate-geometry";

it("derives native profile outputs from the source, not the last decoded simulcast layer", () => {
  const source = { width: 840, height: 720 };
  const previousLow = simulcastCaptureSizes(source, 1280, 720)[1];
  expect(previousLow).toEqual({ width: 420, height: 360 });
  // The old baseline incorrectly kept 420x360, which is neither new output.
  expect(fittedCaptureSize(previousLow, 854, 480)).toEqual(previousLow);
  const next = simulcastCaptureSizes(source, 854, 480);
  expect(next).toEqual([{ width: 560, height: 480 }, { width: 280, height: 240 }]);
  expect(next).not.toContainEqual(previousLow);
  for (const size of next) {
    expect(next.some(expected => matchesCaptureSize(size.width, size.height, expected, 2))).toBe(true);
  }
  for (const [width, height] of [[840, 720], [420, 360], [854, 480], [560, 360], [0, 0]]) {
    expect(next.some(expected => matchesCaptureSize(width, height, expected, 2))).toBe(false);
  }
});

it("preserves source orientation and even rounding at both native output ceilings", () => {
  expect(simulcastCaptureSizes({ width: 720, height: 1280 }, 854, 480))
    .toEqual([{ width: 270, height: 480 }, { width: 134, height: 240 }]);
});
