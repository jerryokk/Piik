import { describe, expect, it } from "vitest";
import { DEFAULT_QUALITY_SETTINGS, DEFAULT_ROUTE_POLICY, MAX_MEDIA_ROUTE_REVISION, MAX_VIEWER_QUALITY_EVIDENCE_BYTES, SIGNALING_PROTOCOL, clientMessageSchema, createRoomRequestSchema, decodeClientMessage, replaceRoomRequestSchema, serverMessageSchema } from "../../src/shared/protocol.js";
import { token, roomId, viewerGrant, qualitySettingsWithAudio, qualityEvidence } from "./support.ts";

describe("client signaling protocol", () => {
  it("keeps full observations with wide numeric values within the message budget", () => {
    for (const floating of [Number.MAX_SAFE_INTEGER, 1.2345678901234567e-100]) {
      const metrics = Object.fromEntries(Object.entries(qualityEvidence.metrics).map(([key, value]) => [key,
        typeof value !== "number" ? value : key.endsWith("Percent") ? 99.99999999999999 :
          ["width", "height", "packetsReceivedDelta", "packetsLostDelta", "framesDecodedDelta",
            "framesDroppedDelta", "freezeCountDelta", "pauseCountDelta", "audioConcealmentEventsDelta"].includes(key)
            ? Number.MAX_SAFE_INTEGER : floating,
      ]));
      const message = { ...qualityEvidence, sequence: Number.MAX_SAFE_INTEGER,
        guard: { connectionId: "a".repeat(128), routeRevision: MAX_MEDIA_ROUTE_REVISION,
          presentationEpoch: Number.MAX_SAFE_INTEGER },
        metrics: { ...metrics, audioCodec: `audio/${"a".repeat(32)}` } };
      expect(clientMessageSchema.safeParse(message).success).toBe(true);
      expect(Buffer.byteLength(JSON.stringify(message))).toBeLessThanOrEqual(MAX_VIEWER_QUALITY_EVIDENCE_BYTES);
    }
  });

  it("uses the current strict signaling generation", () => {
    expect(SIGNALING_PROTOCOL).toBe("piik-v24");
  });

  it("keeps signaling challenges strict and sequence-only", () => {
    expect(
      clientMessageSchema.safeParse({
        type: "signaling-challenge",
        sequence: Number.MAX_SAFE_INTEGER,
      }).success,
    ).toBe(true);
    expect(
      serverMessageSchema.safeParse({
        type: "signaling-challenge-response",
        sequence: 0,
      }).success,
    ).toBe(true);
    for (const invalid of [
      { type: "signaling-challenge", sequence: -1 },
      { type: "signaling-challenge", sequence: 1.5 },
      {
        type: "signaling-challenge",
        sequence: Number.MAX_SAFE_INTEGER + 1,
      },
      { type: "signaling-challenge", sequence: 1, roomId },
      { type: "signaling-challenge-response", sequence: "1" },
      { type: "signaling-challenge-response", sequence: 1, sessionId: token },
    ]) {
      expect(
        (invalid.type === "signaling-challenge"
          ? clientMessageSchema
          : serverMessageSchema
        ).safeParse(invalid).success,
      ).toBe(false);
    }
  });

  it("accepts only an empty route diagnostic request", () => {
    expect(
      clientMessageSchema.safeParse({ type: "request-route-diagnostic" })
        .success,
    ).toBe(true);
    expect(
      clientMessageSchema.safeParse({
        type: "request-route-diagnostic",
        roomId,
      }).success,
    ).toBe(false);
  });

  it("accepts a persistent Host session and rejects removed opt-in fields", () => {
    const host = {
      type: "authenticate" as const,
      protocol: SIGNALING_PROTOCOL,
      roomId,
      role: "host" as const,
      token,
      clientId: "host_12345678",
      routePolicy: DEFAULT_ROUTE_POLICY,
    };
    expect(clientMessageSchema.parse(host)).toMatchObject({ routePolicy: DEFAULT_ROUTE_POLICY });
    for (const removed of [{ roomSession: true }, { roomOnly: true }, { viewerPresence: true }]) {
      expect(clientMessageSchema.safeParse({ ...host, ...removed }).success).toBe(false);
    }
    expect(clientMessageSchema.safeParse({
      type: "start-sharing",
      shareGeneration: "share_12345678",
      routePolicy: DEFAULT_ROUTE_POLICY,
    }).success).toBe(true);
    expect(serverMessageSchema.safeParse({
      type: "sharing-started",
      shareGeneration: "share_12345678",
      routePolicy: DEFAULT_ROUTE_POLICY,
      routeRevision: 0,
      routeAssignment: {
        upstream: { kind: "none" },
        childPeerIds: [],
        sfuPublicationGeneration: null,
      },
      qualitySettings: DEFAULT_QUALITY_SETTINGS,
      paused: false,
    }).success).toBe(true);
    expect(serverMessageSchema.safeParse({
      type: "sharing-start-failed", shareGeneration: "share_12345678", code: "SERVER_ERROR",
    }).success).toBe(true);
    expect(serverMessageSchema.safeParse({
      type: "sharing-start-failed", code: "SERVER_ERROR",
    }).success).toBe(false);
  });

  it("keeps Viewer route status as a strict crossed union", () => {
    expect(
      serverMessageSchema.parse({
        type: "route-status",
        revision: 3,
        state: "waiting",
        reason: "sfu-admission",
      }),
    ).toEqual({
      type: "route-status",
      revision: 3,
      state: "waiting",
      reason: "sfu-admission",
    });
    expect(
      serverMessageSchema.parse({
        type: "route-status",
        revision: 4,
        state: "failed",
        reason: "route-exhausted",
      }),
    ).toEqual({
      type: "route-status",
      revision: 4,
      state: "failed",
      reason: "route-exhausted",
    });
    for (const invalid of [
      {
        type: "route-status",
        revision: 3,
        state: "waiting",
        reason: "route-exhausted",
      },
      {
        type: "route-status",
        revision: 3,
        state: "failed",
        reason: "sfu-admission",
      },
      {
        type: "route-status",
        revision: 3,
        state: "waiting",
        reason: "sfu-admission",
        retryAfterMs: 1_000,
      },
      {
        type: "route-status",
        revision: 3,
        state: "waiting",
        reason: "sfu-admission",
        peerId: "private-peer-id",
      },
    ]) {
      expect(serverMessageSchema.safeParse(invalid).success).toBe(false);
    }
  });

  it("accepts an atomic room creation profile", () => {
    expect(
      createRoomRequestSchema.parse({ codeEntryPolicy: "open" }),
    ).toEqual({ codeEntryPolicy: "open" });
    expect(
      createRoomRequestSchema.parse({
        codeEntryPolicy: "private",
        roomPassword: "room-password",
      }),
    ).toEqual({
      codeEntryPolicy: "private",
      roomPassword: "room-password",
    });
    expect(
      createRoomRequestSchema.parse({ codeEntryPolicy: "private" }),
    ).toEqual({ codeEntryPolicy: "private" });
    expect(
      createRoomRequestSchema.parse({
        codeEntryPolicy: "open",
        preferredRoomId: "4321",
      }),
    ).toEqual({ codeEntryPolicy: "open", preferredRoomId: "4321" });
    expect(
      createRoomRequestSchema.safeParse({
        codeEntryPolicy: "open",
        preferredRoomId: "0432",
      }).success,
    ).toBe(false);
    for (const codeEntryPolicy of [
      "password",
      "private-link",
      "public-watch",
      1,
    ]) {
      expect(
        createRoomRequestSchema.safeParse({
          codeEntryPolicy,
        }).success,
      ).toBe(false);
    }
  });

  it("accepts only a replacement creation profile", () => {
    expect(
      replaceRoomRequestSchema.parse({
        codeEntryPolicy: "private",
        roomPassword: "room-password",
      }),
    ).toEqual({
      codeEntryPolicy: "private",
      roomPassword: "room-password",
    });
    expect(
      replaceRoomRequestSchema.safeParse({
        codeEntryPolicy: "open",
        preferredRoomId: "4321",
      }).success,
    ).toBe(false);
  });

  it("rejects the removed all-room ICE refresh request", () => {
    expect(clientMessageSchema.safeParse({ type: "refresh-ice" }).success).toBe(
      false,
    );
  });

  it("accepts a bounded authentication message", () => {
    expect(DEFAULT_ROUTE_POLICY).toEqual({
      peerOnly: false,
      topologyOptimization: true,
      natPrediction: false,
    });
    expect(
      decodeClientMessage(
        JSON.stringify({
          type: "authenticate",
          protocol: SIGNALING_PROTOCOL,
          roomId,
          role: "viewer",
          clientId: "client_12345678",
        }),
      ),
    ).toMatchObject({ type: "authenticate", role: "viewer" });
    expect(
      decodeClientMessage(
        JSON.stringify({
          type: "authenticate",
          protocol: SIGNALING_PROTOCOL,
          roomId,
          role: "viewer",
          viewerGrant,
          clientId: "client_12345678",
        }),
      ),
    ).toMatchObject({ viewerGrant });
    expect(
      decodeClientMessage(
        JSON.stringify({
          type: "authenticate",
          protocol: SIGNALING_PROTOCOL,
          roomId,
          role: "host",
          token,
          clientId: "host_client_12345678",
          shareGeneration: "share_generation_12345678",
          qualitySettings: qualitySettingsWithAudio,
        }),
      ),
    ).toMatchObject({
      role: "host",
      shareGeneration: "share_generation_12345678",
      qualitySettings: qualitySettingsWithAudio,
      routePolicy: {
        peerOnly: false,
        topologyOptimization: true,
        natPrediction: false,
      },
    });
    expect(
      clientMessageSchema.safeParse({
        type: "authenticate",
        protocol: SIGNALING_PROTOCOL,
        roomId,
        role: "viewer",
        clientId: "client_12345678",
        capabilities: { peerIceTurn: true },
      }).success,
    ).toBe(false);
    expect(
      clientMessageSchema.safeParse({
        type: "authenticate",
        roomId,
        role: "viewer",
        clientId: "client_12345678",
      }).success,
    ).toBe(false);
    expect(
      clientMessageSchema.safeParse({
        type: "authenticate",
        protocol: "invalid-protocol",
        roomId,
        role: "viewer",
        clientId: "client_12345678",
      }).success,
    ).toBe(false);
  });
});
