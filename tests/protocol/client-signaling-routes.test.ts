import { describe, expect, it } from "vitest";
import { MAX_MEDIA_ROUTE_REVISION, clientMessageSchema } from "../../src/shared/protocol.js";

describe("client signaling protocol", () => {
  it("accepts bounded route acknowledgements and failures", () => {
    expect(
      clientMessageSchema.safeParse({
        type: "route-ready",
        revision: 7,
        phase: "prepare",
      }).success,
    ).toBe(true);
    expect(
      clientMessageSchema.safeParse({
        type: "route-ready",
        revision: 7,
        phase: "prepare",
        qualityApproved: true,
      }).success,
    ).toBe(true);
    expect(
      clientMessageSchema.safeParse({
        type: "route-ready",
        revision: 7,
        phase: "prepare",
        qualityApproved: false,
      }).success,
    ).toBe(false);
    expect(
      clientMessageSchema.safeParse({
        type: "route-transport-connected",
        revision: 7,
        connectionId: "connection_12345678",
      }).success,
    ).toBe(true);
    expect(
      clientMessageSchema.safeParse({
        type: "route-media-unavailable",
        revision: 7,
      }).success,
    ).toBe(true);
    expect(
      clientMessageSchema.safeParse({
        type: "route-failed",
        revision: 7,
        phase: "active",
        connectionId: null,
      }).success,
    ).toBe(true);
    expect(
      clientMessageSchema.safeParse({
        type: "refresh-sfu",
        revision: MAX_MEDIA_ROUTE_REVISION,
      }).success,
    ).toBe(true);

    expect(
      clientMessageSchema.safeParse({
        type: "route-ready",
        revision: -1,
        phase: "prepare",
      }).success,
    ).toBe(false);
    expect(
      clientMessageSchema.safeParse({
        type: "route-transport-connected",
        revision: 7,
      }).success,
    ).toBe(false);
    expect(
      clientMessageSchema.safeParse({
        type: "route-media-unavailable",
        revision: -1,
      }).success,
    ).toBe(false);
    expect(
      clientMessageSchema.safeParse({
        type: "route-failed",
        revision: 7,
        phase: "active",
      }).success,
    ).toBe(false);
    expect(
      clientMessageSchema.safeParse({
        type: "refresh-sfu",
        revision: 1.5,
      }).success,
    ).toBe(false);
  });
});
