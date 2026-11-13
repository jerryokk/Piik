import { describe, expect, it } from "vitest";
import { DEFAULT_ROUTE_POLICY, MAX_VIEWERS_PER_ROOM_LIMIT, SIGNALING_PROTOCOL, serverMessageSchema } from "../../src/shared/protocol.js";
import { qualitySettings } from "./support.ts";

describe("server signaling protocol", () => {
  it("keeps route status pairings revision-fenced and strict", () => {
    for (const message of [
      {
        type: "route-status",
        revision: 3,
        state: "waiting",
        reason: "sfu-admission",
      },
      {
        type: "route-status",
        revision: 4,
        state: "failed",
        reason: "route-exhausted",
      },
    ]) {
      expect(serverMessageSchema.safeParse(message).success).toBe(true);
    }
    for (const message of [
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
        state: "failed",
        reason: "route-exhausted",
        retry: true,
      },
    ]) {
      expect(serverMessageSchema.safeParse(message).success).toBe(false);
    }
  });

  it("accepts only the privacy-safe route snapshot payload", () => {
    const message = {
      type: "route-diagnostic-snapshot",
      snapshot: {
        children: [
          {
            ordinal: 1,
            parent: { kind: "host" },
            effectiveCapacity: 2,
            childCount: 1,
            demandAgeMs: 100,
            queueWaitMs: 10,
            candidateStartMs: 20,
            firstDecodedFrameMs: 80,
            finalMs: 80,
            finalRoute: "direct",
            rejectionBucket: "none",
            quality: {
              eligibleWindows: 3,
              eligibleDurationMs: 6_000,
              freezeWindows: 1,
              freezeCount: 1,
              freezeDurationMs: 250,
              pauseCount: 0,
              pauseDurationMs: 0,
            },
          },
          {
            ordinal: 2,
            parent: { kind: "viewer", ordinal: 1 },
            effectiveCapacity: 0,
            childCount: 0,
            demandAgeMs: 90,
            queueWaitMs: null,
            candidateStartMs: null,
            firstDecodedFrameMs: null,
            finalMs: null,
            finalRoute: "waiting",
            rejectionBucket: "sfu-admission",
            quality: null,
          },
        ],
        operation: {
          childOrdinal: 2,
          reason: "join",
          stage: "admission",
          cursor: 1,
          candidateCount: 2,
        },
      },
    } as const;
    expect(serverMessageSchema.safeParse(message).success).toBe(true);
    for (const invalid of [
      {
        ...message,
        snapshot: { ...message.snapshot, peerId: "viewer_private_12345678" },
      },
      {
        ...message,
        snapshot: {
          ...message.snapshot,
          children: [
            {
              ...message.snapshot.children[0],
              connectionId: "connection_private_12345678",
            },
          ],
        },
      },
      {
        ...message,
        snapshot: {
          ...message.snapshot,
          children: [
            {
              ...message.snapshot.children[0],
              quality: {
                ...message.snapshot.children[0].quality,
                presentationEpoch: 1,
              },
            },
          ],
        },
      },
      {
        ...message,
        snapshot: {
          ...message.snapshot,
          children: [
            {
              ...message.snapshot.children[0],
              quality: {
                ...message.snapshot.children[0].quality,
                freezeCount: -1,
              },
            },
          ],
        },
      },
      {
        ...message,
        snapshot: {
          ...message.snapshot,
          children: [
            message.snapshot.children[0],
            {
              ...message.snapshot.children[1],
              parent: { kind: "viewer", ordinal: 2 },
            },
          ],
        },
      },
      {
        ...message,
        snapshot: {
          ...message.snapshot,
          operation: {
            ...message.snapshot.operation,
            rejectionBucket: "raw-error",
          },
        },
      },
    ]) {
      expect(serverMessageSchema.safeParse(invalid).success).toBe(false);
    }
  });

  it("accepts only STUN URLs in the authenticated peer ICE config", () => {
    for (const url of ["stun:stun.test:80/", "stun:stun.test:3478/"]) {
      expect(serverMessageSchema.safeParse({
        ...authenticatedMessage(20),
        iceConfig: { iceServers: [{ urls: url }], natPredictionStunUrls: [] },
      }).success).toBe(false);
    }
    expect(
      serverMessageSchema.safeParse({
        type: "ice-config",
        iceConfig: { iceServers: [] },
      }).success,
    ).toBe(false);
    expect(
      serverMessageSchema.safeParse({
        ...authenticatedMessage(8),
        iceConfig: {
          iceServers: [
            {
              urls: "turn:relay.example.test:3478?transport=udp",
              username: `1787076000:${"a".repeat(32)}`,
              credential: "credential",
            },
          ],
        },
      }).success,
    ).toBe(false);
    expect(
      serverMessageSchema.safeParse({
        ...authenticatedMessage(8),
        iceConfig: {
          iceServers: [{ urls: "turn:relay.example.test:3478?transport=udp" }],
        },
      }).success,
    ).toBe(false);
    expect(
      serverMessageSchema.safeParse({
        ...authenticatedMessage(8),
        iceConfig: {
          iceServers: [{ urls: "stuns:stun.example.test:5349" }],
        },
      }).success,
    ).toBe(false);
  });

  function authenticatedMessage(maxViewers: number) {
    return {
      type: "authenticated",
      protocol: SIGNALING_PROTOCOL,
      role: "host",
      peerId: "host_12345678",
      maxViewers,
      endpointMediaCopyCapacity: 2,
      hostOnline: true,
      connectionId: null,
      mediaMode: "peer-assisted",
      shareGeneration: "share_generation_12345678",
      routeRevision: 0,
      routeAssignment: {
        upstream: { kind: "none" },
        childPeerIds: [],
        sfuPublicationGeneration: null,
      },
      qualitySettings,
      routePolicy: DEFAULT_ROUTE_POLICY,
      codeEntryPolicy: "open",
      viewerPasswordEnabled: false,
      viewerAuthorizationGeneration: "viewer_generation_12345678",
      iceConfig: {
        iceServers: [],
        natPredictionStunUrls: [],
      },
    };
  }

  it("scopes password configuration state to authenticated Hosts", () => {
    const host = authenticatedMessage(8) as Record<string, unknown>;
    expect(serverMessageSchema.safeParse(host).success).toBe(true);
    delete host.viewerPasswordEnabled;
    expect(serverMessageSchema.safeParse(host).success).toBe(false);

    const viewer = {
      ...authenticatedMessage(8),
      role: "viewer",
      peerId: "viewer_12345678",
    } as Record<string, unknown>;
    delete viewer.viewerPasswordEnabled;
    expect(serverMessageSchema.safeParse(viewer).success).toBe(true);
    viewer.viewerPasswordEnabled = true;
    expect(serverMessageSchema.safeParse(viewer).success).toBe(false);
  });

  it("requires the v20 NAT observation configuration", () => {
    const message = authenticatedMessage(8);
    const { natPredictionStunUrls: _urls, ...legacyIceConfig } =
      message.iceConfig;
    expect(
      serverMessageSchema.safeParse({
        ...message,
        iceConfig: legacyIceConfig,
      }).success,
    ).toBe(false);
  });

  it("accepts dynamic viewer limits within the protocol boundary", () => {
    expect(MAX_VIEWERS_PER_ROOM_LIMIT).toBe(20);
    expect(serverMessageSchema.safeParse(authenticatedMessage(1)).success).toBe(
      true,
    );
    expect(serverMessageSchema.safeParse(authenticatedMessage(8)).success).toBe(
      true,
    );
    expect(
      serverMessageSchema.safeParse(
        authenticatedMessage(MAX_VIEWERS_PER_ROOM_LIMIT),
      ).success,
    ).toBe(true);
  });

  it("requires the authenticated endpoint capacity to be 1, 2, or 3", () => {
    for (const endpointMediaCopyCapacity of [1, 2, 3]) {
      expect(
        serverMessageSchema.safeParse({
          ...authenticatedMessage(8),
          endpointMediaCopyCapacity,
        }).success,
      ).toBe(true);
    }
    for (const endpointMediaCopyCapacity of [undefined, 0, 4, 1.5]) {
      const message = authenticatedMessage(8) as Record<string, unknown>;
      if (endpointMediaCopyCapacity === undefined) {
        delete message.endpointMediaCopyCapacity;
      } else {
        message.endpointMediaCopyCapacity = endpointMediaCopyCapacity;
      }
      expect(serverMessageSchema.safeParse(message).success).toBe(false);
    }
  });
});
