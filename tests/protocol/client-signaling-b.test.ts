import { describe, expect, it } from "vitest";
import { DEFAULT_VIEWER_DISPLAY_NAME, MAX_DISPLAY_NAME_CODE_POINTS, MAX_VIEWER_PASSWORD_LENGTH, SIGNALING_PROTOCOL, clientMessageSchema, normalizeDisplayName, roomAccessUpdateRequestSchema, roomAccessUpdateResponseSchema, viewerPasswordSchema } from "../../src/shared/protocol.js";
import { token, roomId } from "./support.ts";

describe("client signaling protocol", () => {
  it("normalizes display names and rejects misleading Unicode boundaries", () => {
    expect(normalizeDisplayName("  Cafe\u0301\u00a0朋友  ")).toBe("Café 朋友");
    expect(normalizeDisplayName("玩家 👩‍💻")).toBe("玩家 👩‍💻");
    expect(normalizeDisplayName("名".repeat(MAX_DISPLAY_NAME_CODE_POINTS))).toBe(
      "名".repeat(MAX_DISPLAY_NAME_CODE_POINTS),
    );
    expect(
      normalizeDisplayName("名".repeat(MAX_DISPLAY_NAME_CODE_POINTS + 1)),
    ).toBeNull();
    for (const invalid of ["a\nb", "a\u202eb", "a\ufeffb", "a\ud800b"]) {
      expect(normalizeDisplayName(invalid)).toBeNull();
    }

    expect(
      clientMessageSchema.safeParse({
        type: "authenticate",
        protocol: SIGNALING_PROTOCOL,
        roomId,
        role: "viewer",
        clientId: "client_12345678",
        displayName: "小明",
      }).success,
    ).toBe(true);
    expect(
      clientMessageSchema.safeParse({
        type: "authenticate",
        protocol: SIGNALING_PROTOCOL,
        roomId,
        role: "host",
        token,
        clientId: "client_12345678",
        displayName: "Host",
      }).success,
    ).toBe(true);
    expect(
      clientMessageSchema.safeParse({
        type: "set-display-name",
        displayName: " Cafe\u0301 ",
      }).success,
    ).toBe(false);
    expect(DEFAULT_VIEWER_DISPLAY_NAME).toBe("观众");
  });

  it("accepts only canonical bounded Viewer grants and access actions", () => {
    for (const malformedGrant of [
      "b".repeat(21),
      "b".repeat(23),
      `${"b".repeat(21)}=`,
      `${"b".repeat(21)}b`,
      `g1.${roomId}.1787076000.${"b".repeat(43)}`,
    ]) {
      expect(
        clientMessageSchema.safeParse({
          type: "authenticate",
          protocol: SIGNALING_PROTOCOL,
          roomId,
          role: "viewer",
          viewerGrant: malformedGrant,
          clientId: "client_12345678",
        }).success,
      ).toBe(false);
    }
    for (const policy of ["open", "private"] as const) {
      expect(
        roomAccessUpdateRequestSchema.safeParse({
          action: "set-code-entry-policy",
          policy,
        }).success,
      ).toBe(true);
    }
    expect(
      roomAccessUpdateRequestSchema.safeParse({
        action: "rotate-viewer-grant",
      }).success,
    ).toBe(true);
    expect(
      roomAccessUpdateRequestSchema.safeParse({
        action: "revoke-viewer-grant",
      }).success,
    ).toBe(true);
    expect(
      roomAccessUpdateRequestSchema.safeParse({
        action: "set-code-entry-policy",
        policy: "disabled",
      }).success,
    ).toBe(false);
    expect(
      roomAccessUpdateRequestSchema.safeParse({
        action: "set-code-entry-policy",
        policy: "password",
      }).success,
    ).toBe(false);
    for (const removedMessage of [
      { type: "set-code-entry-policy", policy: "open" },
      { type: "rotate-viewer-grant" },
      { type: "revoke-viewer-grant" },
      { type: "set-viewer-password", password: "room-password" },
    ]) {
      expect(clientMessageSchema.safeParse(removedMessage).success).toBe(false);
    }
  });

  it("accepts simple bounded Viewer passwords and access responses", () => {
    expect(viewerPasswordSchema.safeParse("x").success).toBe(true);
    expect(viewerPasswordSchema.safeParse("simple-password").success).toBe(true);
    for (const invalid of [
      "",
      "contains space",
      "line\nbreak",
      "x".repeat(MAX_VIEWER_PASSWORD_LENGTH + 1),
    ]) {
      expect(viewerPasswordSchema.safeParse(invalid).success).toBe(false);
    }

    expect(
      clientMessageSchema.safeParse({
        type: "authenticate",
        protocol: SIGNALING_PROTOCOL,
        roomId,
        role: "viewer",
        clientId: "client_12345678",
        viewerPassword: "easy-password",
      }).success,
    ).toBe(true);
    expect(
      roomAccessUpdateRequestSchema.safeParse({
        action: "set-viewer-password",
        password: null,
      }).success,
    ).toBe(true);
    expect(
      roomAccessUpdateResponseSchema.safeParse({
        type: "viewer-password-updated",
        enabled: true,
      }).success,
    ).toBe(true);
  });

  it("rejects unknown fields and malformed tokens", () => {
    const result = clientMessageSchema.safeParse({
      type: "authenticate",
      protocol: SIGNALING_PROTOCOL,
      roomId,
      role: "host",
      token: "short",
      clientId: "client_12345678",
      admin: true,
  });

    expect(result.success).toBe(false);
  });

  it("requires a host token and rejects viewer tokens or malformed room codes", () => {
    expect(
      clientMessageSchema.safeParse({
        type: "authenticate",
        protocol: SIGNALING_PROTOCOL,
        roomId,
        role: "host",
        token,
        clientId: "client_12345678",
      }).success,
    ).toBe(true);
    expect(
      clientMessageSchema.safeParse({
        type: "authenticate",
        protocol: SIGNALING_PROTOCOL,
        roomId,
        role: "viewer",
        token,
        clientId: "client_12345678",
      }).success,
    ).toBe(false);
    expect(
      clientMessageSchema.safeParse({
        type: "authenticate",
        protocol: SIGNALING_PROTOCOL,
        roomId: "1234",
        role: "viewer",
        clientId: "client_12345678",
      }).success,
    ).toBe(true);
    expect(
      clientMessageSchema.safeParse({
        type: "authenticate",
        protocol: SIGNALING_PROTOCOL,
        roomId: "0123",
        role: "viewer",
        clientId: "client_12345678",
      }).success,
    ).toBe(false);
    expect(
      clientMessageSchema.safeParse({
        type: "authenticate",
        protocol: SIGNALING_PROTOCOL,
        roomId: "1".repeat(5),
        role: "viewer",
        clientId: "client_12345678",
      }).success,
    ).toBe(false);
  });

  it("bounds SDP before routing it", () => {
    const result = clientMessageSchema.safeParse({
      type: "signal",
      targetPeerId: "viewer_12345678",
      payload: {
        kind: "description",
        connectionId: "connection_12345678",
        description: {
          type: "offer",
          sdp: "v".repeat(48 * 1024 + 1),
        },
      },
    });

    expect(result.success).toBe(false);
  });

  it("requires viewers to choose ICE restart or peer rebuild explicitly", () => {
    const restartRequest = {
      type: "restart-request",
      connectionId: "connection_12345678",
      rebuild: true,
    };

    expect(clientMessageSchema.safeParse(restartRequest).success).toBe(true);
    expect(
      clientMessageSchema.safeParse({
        ...restartRequest,
        targetPeerId: "parent_12345678",
      }).success,
    ).toBe(true);
    expect(
      clientMessageSchema.safeParse({
        type: restartRequest.type,
        connectionId: restartRequest.connectionId,
      }).success,
    ).toBe(false);
  });

  it("accepts explicit sharing-stop and abandonment messages", () => {
    expect(clientMessageSchema.safeParse({ type: "stop-sharing" }).success).toBe(
      true,
    );
    expect(
      clientMessageSchema.safeParse({
        type: "stop-sharing",
        shareGeneration: "share_generation_12345678",
      }).success,
    ).toBe(true);
    expect(
      clientMessageSchema.safeParse({
        type: "authenticate",
        protocol: SIGNALING_PROTOCOL,
        roomId,
        role: "host",
        token,
        clientId: "client_12345678",
        shareGeneration: "share_generation_12345678",
      }).success,
    ).toBe(true);
    expect(clientMessageSchema.safeParse({ type: "abandon-room" }).success).toBe(
      true,
    );
  });
});
