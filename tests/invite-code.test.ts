import { describe, expect, it } from "vitest";
import { inviteCode, inviteCodePath } from "../src/client/ui/invite-code";

const LINK = "http://192.168.1.20:8787/r/1234#v=abcdefghijklmnopqrstuvwxyz0123456789";

describe("invitation code", () => {
  it("encodes a link as a square module grid with a quiet zone", () => {
    const code = inviteCode(LINK);
    expect(code).not.toBeNull();
    expect(code!.size).toBe(code!.modules.length);
    expect(code!.modules.every((row) => row.length === code!.size)).toBe(true);
    // The quiet zone is clear on every side.
    const edge = code!.modules[0]!;
    expect(edge.every((dark) => !dark)).toBe(true);
    expect(code!.modules[code!.size - 1]!.every((dark) => !dark)).toBe(true);
    expect(code!.modules.every((row) => !row[0] && !row[code!.size - 1])).toBe(true);
  });

  it("draws every dark module exactly once", () => {
    const code = inviteCode(LINK)!;
    const dark = code.modules.flat().filter(Boolean).length;
    // Each run in the path covers a contiguous horizontal span of modules.
    const covered = [...inviteCodePath(code).matchAll(/M\d+ \d+h(\d+)v1h-\1z/g)]
      .reduce((total, match) => total + Number(match[1]), 0);
    expect(covered).toBe(dark);
    expect(dark).toBeGreaterThan(0);
  });

  it("returns null instead of a partial grid", () => {
    expect(inviteCode("")).toBeNull();
    // Far past any QR version's capacity.
    expect(inviteCode("http://example.test/".padEnd(8000, "x"))).toBeNull();
  });

  it("encodes distinct links to distinct codes", () => {
    const first = inviteCode(LINK)!;
    const second = inviteCode(LINK.replace("1234", "9876"))!;
    expect(JSON.stringify(first.modules)).not.toBe(JSON.stringify(second.modules));
  });
});
