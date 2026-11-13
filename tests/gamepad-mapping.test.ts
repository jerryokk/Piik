import { describe, expect, it } from "vitest";
import { gamepadReport } from "../src/client/remote-control/gamepad";

// The Browser's Standard Gamepad layout is the only shape the Host accepts.
// These cover the lossy parts: bit packing, signed stick scaling and trigger
// quantisation, each of which silently corrupts input when it is off by one.

type Pad = Pick<Gamepad, "buttons" | "axes">;

function pad(pressed: number[] = [], values: Record<number, number> = {}, axes: number[] = [], length = 17): Pad {
  // Longer arrays prove the mapper stops at the standard layout instead of
  // packing vendor buttons the Host would reject as unknown bits.
  const buttons = Array.from({ length }, (_, index) => ({
    pressed: pressed.includes(index),
    value: values[index] ?? (pressed.includes(index) ? 1 : 0),
  }));
  return { buttons, axes } as unknown as Pad;
}

describe("Gamepad report mapping", () => {
  it("packs the 17 standard buttons into their W3C bit positions", () => {
    expect(gamepadReport(pad([])).buttons).toBe(0);
    expect(gamepadReport(pad([0])).buttons).toBe(1);
    expect(gamepadReport(pad([1])).buttons).toBe(2);
    expect(gamepadReport(pad([0, 4, 16])).buttons).toBe(1 | (1 << 4) | (1 << 16));
  });

  it("ignores buttons outside the standard layout", () => {
    // The Host rejects a report carrying unknown bits, so packing more than
    // the 17 standard indices would fail the whole input rather than drop it.
    expect(gamepadReport(pad([17, 20, 23], {}, [], 24)).buttons).toBe(0);
    expect(gamepadReport(pad([3, 20], {}, [], 24)).buttons).toBe(1 << 3);
  });

  it("scales sticks to signed 16-bit with Y growing downward", () => {
    expect(gamepadReport(pad([], {}, [1, 1, -1, -1]))).toMatchObject({
      leftX: 32767, leftY: 32767, rightX: -32768, rightY: -32768,
    });
    expect(gamepadReport(pad([], {}, [0, 0, 0, 0]))).toMatchObject({
      leftX: 0, leftY: 0, rightX: 0, rightY: 0,
    });
  });

  it("clamps out-of-range axes instead of overflowing the wire type", () => {
    expect(gamepadReport(pad([], {}, [4, -4, 0, 0]))).toMatchObject({ leftX: 32767, leftY: -32768 });
  });

  it("treats a missing axis as centred rather than undefined", () => {
    expect(gamepadReport(pad([], {}, [])).leftX).toBe(0);
  });

  it("quantises analog triggers to 0..255", () => {
    const report = gamepadReport(pad([], { 6: 1, 7: 0.5 }));
    expect(report.leftTrigger).toBe(255);
    expect(report.rightTrigger).toBe(128);
    expect(gamepadReport(pad([])).leftTrigger).toBe(0);
  });

  it("clamps a trigger that reports beyond its nominal range", () => {
    expect(gamepadReport(pad([], { 6: 2 })).leftTrigger).toBe(255);
    expect(gamepadReport(pad([], { 6: -1 })).leftTrigger).toBe(0);
  });
});
