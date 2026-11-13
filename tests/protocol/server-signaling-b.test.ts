import { describe, expect, it } from "vitest";
import { DEFAULT_ROUTE_POLICY, MAX_MEDIA_ROUTE_REVISION, MAX_VIEWER_QUALITY_EVIDENCE_BYTES, MAX_VIEWERS_PER_ROOM_LIMIT, SIGNALING_PROTOCOL, participantRouteAssignmentSchema, preparedRouteCandidateSchema, serverMessageSchema } from "../../src/shared/protocol.js";
import { roomId, qualitySettings, qualityEvidence } from "./support.ts";

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

describe("server signaling protocol", () => {
  it("accepts a strict, unique and bounded Viewer presence snapshot", () => {
    const viewer = {
      role: "viewer",
      peerId: "viewer_12345678",
      displayName: "小明",
      upstream: { kind: "peer", peerId: "viewer_parent_12345678" },
    } as const;
    expect(
      serverMessageSchema.safeParse({
        type: "viewer-presence",
        viewers: [
          {
            ...viewer,
            upstream: { kind: "sfu" },
            mediaReady: true,
          },
        ],
      }).success,
    ).toBe(true);
    expect(
      serverMessageSchema.safeParse({
        type: "viewer-presence",
        viewers: [{ ...viewer, mediaReady: false }],
      }).success,
    ).toBe(false);
    expect(
      serverMessageSchema.safeParse({
        type: "viewer-presence",
        viewers: [viewer, viewer],
      }).success,
    ).toBe(false);
    expect(
      serverMessageSchema.safeParse({
        type: "viewer-presence",
        viewers: [{ ...viewer, mediaTopology: "peer-relay" }],
      }).success,
    ).toBe(false);
    expect(
      serverMessageSchema.safeParse({
        type: "viewer-presence",
        viewers: [{ ...viewer, ip: "203.0.113.1" }],
      }).success,
    ).toBe(false);
    expect(
      serverMessageSchema.safeParse({
        type: "viewer-presence",
        viewers: [
          viewer,
          {
            role: "host",
            peerId: "host_12345678",
            displayName: "分享者",
            upstream: { kind: "none" },
          },
        ],
      }).success,
    ).toBe(true);
  });

  it("requires a complete bounded peer-assisted assignment", () => {
    const peerAssisted = {
      ...authenticatedMessage(8),
      mediaMode: "peer-assisted",
      shareGeneration: "share_generation_12345678",
      routeRevision: 0,
      routeAssignment: {
        upstream: { kind: "none" },
        childPeerIds: ["viewer_12345678", "viewer_87654321"],
        sfuPublicationGeneration: null,
      },
      qualitySettings,
      routePolicy: DEFAULT_ROUTE_POLICY,
    };

    expect(serverMessageSchema.safeParse(peerAssisted).success).toBe(true);
    const incomplete = {
      ...authenticatedMessage(8),
    } as Record<string, unknown>;
    delete incomplete.routeAssignment;
    expect(serverMessageSchema.safeParse(incomplete).success).toBe(false);
    expect(
      serverMessageSchema.safeParse({
        ...peerAssisted,
        routeAssignment: {
          ...peerAssisted.routeAssignment,
          childPeerIds: Array.from({ length: MAX_VIEWERS_PER_ROOM_LIMIT + 1 }, (_, index) => `viewer_overflow_${index}`),
        },
      }).success,
    ).toBe(false);
    expect(
      serverMessageSchema.safeParse({
        ...peerAssisted,
        qualitySettings: { ...qualitySettings, maxBitrate: 0x1_0000_0000 },
      }).success,
    ).toBe(false);
    const missingMode = {
      ...authenticatedMessage(8),
    } as Record<string, unknown>;
    delete missingMode.mediaMode;
    expect(serverMessageSchema.safeParse(missingMode).success).toBe(false);
    expect(
      serverMessageSchema.safeParse({
        type: "quality-settings",
        qualitySettings: {
          resolution: { width: 1280, height: 720 },
          maxFramerate: 30,
          maxBitrate: 3_000_000,
          degradationPreference: "balanced",
        },
      }).success,
    ).toBe(true);
    expect(
      serverMessageSchema.safeParse({
        type: "quality-settings",
        qualitySettings: { ...qualitySettings, maxFramerate: 0 },
      }).success,
    ).toBe(false);
  });

  it("accepts only bounded connection-attempt progress on a prepared route", () => {
    const candidate = {
      childPeerId: "child_12345678", connectionId: "connection_12345678",
      transport: "direct", qualityProbe: false,
    };
    for (const total of [1, 3, 4, 5, 20]) {
      for (let current = 1; current <= total; current++) {
        expect(preparedRouteCandidateSchema.safeParse({
          ...candidate, connectionAttempt: { current, total },
        }).success).toBe(true);
      }
    }
    for (const connectionAttempt of [
      { current: 0, total: 3 }, { current: 4, total: 3 },
      { current: 5, total: 4 }, { current: 1, total: 0 }, { current: 1, total: Number.MAX_SAFE_INTEGER + 1 }, { current: 1.5, total: 3 },
    ]) {
      expect(preparedRouteCandidateSchema.safeParse({
        ...candidate, connectionAttempt,
      }).success).toBe(false);
    }
  });

  it("keeps route messages strict alongside the single authenticated mode", () => {
    const assignment = {
      upstream: { kind: "peer", peerId: "parent_12345678" },
      childPeerIds: ["child_12345678", "child_87654321"],
      sfuPublicationGeneration: null,
    };

    expect(participantRouteAssignmentSchema.safeParse(assignment).success).toBe(
      true,
    );
    expect(
      serverMessageSchema.safeParse({
        type: "route-update",
        revision: 9,
        phase: "prepare",
        assignment,
        candidate: {
          childPeerId: "child_12345678",
          connectionId: "connection_12345678",
          transport: "direct",
          qualityProbe: false,
        },
      }).success,
    ).toBe(true);
    expect(
      serverMessageSchema.safeParse({
        type: "route-update",
        revision: 10,
        phase: "prepare",
        assignment,
        candidate: {
          childPeerId: "child_12345678",
          connectionId: "connection_12345678",
          transport: "sfu",
          qualityProbe: true,
        },
      }).success,
    ).toBe(true);
    expect(
      serverMessageSchema.safeParse({
        type: "route-update",
        revision: 10,
        phase: "prepare",
        assignment,
        candidate: {
          childPeerId: "child_12345678",
          connectionId: "connection_12345678",
          transport: "direct",
          qualityProbe: false,
          unexpected: null,
        },
      }).success,
    ).toBe(false);
    expect(
      serverMessageSchema.safeParse({
        type: "sfu-config",
        revision: 9,
        publicationGeneration: "publication_12345678",
        connectionId: "connection_12345678",
      }).success,
    ).toBe(true);

    expect(
      participantRouteAssignmentSchema.safeParse({
        ...assignment,
        childPeerIds: Array.from({ length: MAX_VIEWERS_PER_ROOM_LIMIT + 1 }, (_, index) => `child_${String(index).padStart(8, "0")}`),
      }).success,
    ).toBe(false);
    expect(
      participantRouteAssignmentSchema.safeParse({
        ...assignment,
        childPeerIds: ["child_12345678", "child_12345678"],
      }).success,
    ).toBe(false);
    expect(
      serverMessageSchema.safeParse({
        type: "route-update",
        revision: 9,
        phase: "prepare",
        assignment,
        roomAssignments: [],
      }).success,
    ).toBe(false);
    expect(
      serverMessageSchema.safeParse({
        type: "sfu-config",
        revision: 9,
        publicationGeneration: "publication_12345678",
        connectionId: "",
      }).success,
    ).toBe(false);
    expect(
      serverMessageSchema.safeParse({
        ...authenticatedMessage(8),
        routeRevision: MAX_MEDIA_ROUTE_REVISION + 1,
      }).success,
    ).toBe(false);
  });

  it("accepts every bounded hybrid upstream shape", () => {
    for (const upstream of [
      { kind: "none" },
      { kind: "peer", peerId: "parent_12345678" },
      { kind: "sfu" },
    ]) {
      expect(
        participantRouteAssignmentSchema.safeParse({
          upstream,
          childPeerIds: [],
          sfuPublicationGeneration:
            upstream.kind === "peer" ? null : "generation_12345678",
        }).success,
      ).toBe(true);
    }
    expect(
      participantRouteAssignmentSchema.safeParse({
        upstream: { kind: "sfu" },
        childPeerIds: [],
        sfuPublicationGeneration: null,
      }).success,
    ).toBe(false);
  });

  it("accepts only canonical server-derived viewer evidence envelopes", () => {
    const forwarded = {
      ...qualityEvidence,
      viewerPeerId: "viewer_12345678",
      upstream: { kind: "peer", peerId: "host_12345678" },
    };
    expect(serverMessageSchema.safeParse(forwarded).success).toBe(true);
    expect(
      serverMessageSchema.safeParse({
        ...forwarded,
        metrics: {
          ...forwarded.metrics,
          freezeDurationMsDelta: 12_000,
          pauseDurationMsDelta: 6_000,
        },
      }).success,
    ).toBe(true);
    expect(
      serverMessageSchema.safeParse({
        ...forwarded,
        upstream: { kind: "sfu" },
      }).success,
    ).toBe(true);
    expect(
      Buffer.byteLength(JSON.stringify(forwarded), "utf8"),
    ).toBeLessThanOrEqual(MAX_VIEWER_QUALITY_EVIDENCE_BYTES);
    expect(
      serverMessageSchema.safeParse({ ...forwarded, roomId }).success,
    ).toBe(false);
    expect(
      serverMessageSchema.safeParse({
        ...qualityEvidence,
        viewerPeerId: "viewer_12345678",
      }).success,
    ).toBe(false);
    expect(
      serverMessageSchema.safeParse({
        ...forwarded,
        upstream: { kind: "none" },
      }).success,
    ).toBe(false);
  });

  it.each([0, 1.5, MAX_VIEWERS_PER_ROOM_LIMIT + 1])(
    "rejects viewer limit %s outside the protocol boundary",
    (maxViewers) => {
      expect(
        serverMessageSchema.safeParse(authenticatedMessage(maxViewers)).success,
      ).toBe(false);
    },
  );
});
