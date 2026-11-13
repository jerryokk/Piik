import { describe, expect, it, vi } from "vitest";
import { type ClientMessage } from "../../src/shared/protocol.ts";
import { HostSfuRoute } from "../../src/client/media/host-sfu-route.ts";
import { QUALITY_PROFILES } from "../../src/client/media/quality.ts";
import { exactPeerSignalOwner, MediaRouteTransition, reportActivePeerRouteFailure } from "../../src/client/media/route-transition.ts";
import { ViewerSfuRoute } from "../../src/client/media/viewer-sfu-route";
import { t } from "../../src/client/ui/copy.ts";
import { resolveMediaFailure } from "../../src/client/ui/media-failure.ts";
import { peerAssignment, viewerSfuAssignment, hostAssignment, sfuConfig, createFakePublisher, type SubscriberEvents, candidate, createFakeSubscriber, prepareSfu } from "./support.ts";

describe("minimal route transition contracts", () => {
  it("keeps duplicate peer prepare idempotent and releases it before takeover", async () => {
    const prepared: Array<{ parentPeerId: string | null; revision?: number }> = [];
    const activatePeer = vi.fn(() => true);
    const route = new ViewerSfuRoute("viewer_12345678", {
      activatePeer,
      preparePeer: (assignment, revision) =>
        prepared.push({
          parentPeerId:
            assignment?.upstream.kind === "peer"
              ? assignment.upstream.peerId
              : null,
          ...(revision === undefined ? {} : { revision }),
        }),
      reconcileSfuChildren: () => undefined,
      onSfuStream: () => undefined,
      send: () => true,
    });
    const oldAssignment = peerAssignment("old-parent");
    const nextAssignment = peerAssignment("next-parent");

    route.accept({ revision: 1, phase: "active", assignment: oldAssignment });
    await vi.waitFor(() => expect(activatePeer).toHaveBeenCalled());
    prepared.length = 0;

    expect(
      route.accept({
        revision: 2,
        phase: "prepare",
        assignment: nextAssignment,
        candidate: candidate(2),
      }),
    ).toBe("accepted");
    expect(prepared).toEqual([
      { parentPeerId: "next-parent", revision: 2 },
    ]);

    expect(
      route.accept({
        revision: 2,
        phase: "prepare",
        assignment: nextAssignment,
        candidate: candidate(2),
      }),
    ).toBe("duplicate");
    expect(prepared).toEqual([
      { parentPeerId: "next-parent", revision: 2 },
    ]);

    route.accept({
      revision: 3,
      phase: "prepare",
      assignment: peerAssignment("third-parent"),
      candidate: candidate(3),
    });
    expect(prepared.slice(-2)).toEqual([
      { parentPeerId: null },
      { parentPeerId: "third-parent", revision: 3 },
    ]);

    route.accept({
      revision: 4,
      phase: "prepare",
      assignment: viewerSfuAssignment(),
      candidate: candidate(4, "viewer_12345678", "sfu"),
    });
    expect(prepared.at(-1)).toEqual({ parentPeerId: null });
  });

  it("routes same-parent overlap signals only by exact connection identity", () => {
    expect(exactPeerSignalOwner("pending", "pending", "active")).toBe(
      "pending",
    );
    expect(exactPeerSignalOwner("active", "pending", "active")).toBe(
      "active",
    );
    expect(exactPeerSignalOwner("unknown", "pending", "active")).toBeNull();
  });

  it("reports only the exact active peer failure", () => {
    const route = new MediaRouteTransition();
    const sent: ClientMessage[] = [];
    route.accept({
      revision: 4,
      phase: "active",
      assignment: peerAssignment("parent"),
    });

    expect(
      reportActivePeerRouteFailure(route, "other", "connection", (message) => {
        sent.push(message);
        return true;
      }),
    ).toBe(true);
    expect(sent).toEqual([]);

    reportActivePeerRouteFailure(route, "parent", "connection", (message) => {
      sent.push(message);
      return true;
    });
    expect(sent).toEqual([
      {
        type: "route-failed",
        revision: 4,
        phase: "active",
        connectionId: "connection",
      },
    ]);
  });

  it("reports committed Peer failure during another preparation using active authority", () => {
    const route = new MediaRouteTransition();
    const send = vi.fn(() => false);
    route.accept({ revision: 7, phase: "active", assignment: peerAssignment("parent") });
    route.accept({
      revision: 8, phase: "prepare",
      assignment: peerAssignment("parent", ["child"]), candidate: candidate(8, "child"),
    });

    expect(reportActivePeerRouteFailure(route, "parent", "active-connection", send)).toBe(false);
    expect(send).toHaveBeenLastCalledWith({
      type: "route-failed", revision: 7, phase: "active", connectionId: "active-connection",
    });
    send.mockReturnValue(true);
    expect(reportActivePeerRouteFailure(route, "parent", "active-connection", send)).toBe(true);

    route.accept({ revision: 9, phase: "active", assignment: peerAssignment("replacement") });
    send.mockClear();
    expect(reportActivePeerRouteFailure(route, "parent", "active-connection", send)).toBe(true);
    route.reset();
    expect(reportActivePeerRouteFailure(route, "replacement", "new-connection", send)).toBe(true);
    expect(send).not.toHaveBeenCalled();
  });

  it("reports the committed SFU publisher when a Peer child is preparing", async () => {
    const send = vi.fn(() => true);
    let fail!: () => void;
    const route = new HostSfuRoute({
      getStream: () => ({}) as MediaStream,
      getProfile: () => QUALITY_PROFILES["720p30"],
      getVideoCodec: () => "h264",
      reconcileChildren: () => undefined,
      send,
      createPublisher: (onDisconnected) => {
        fail = onDisconnected;
        return createFakePublisher([], "active");
      },
    });
    const config = sfuConfig(7);
    await route.acceptAndWait({ revision: 7, phase: "active", assignment: hostAssignment(config.publicationGeneration) });
    await route.acceptConfig(config);
    send.mockClear();
    route.accept({
      revision: 8, phase: "prepare",
      assignment: hostAssignment(config.publicationGeneration, ["child"]), candidate: candidate(8, "child"),
    });
    fail();
    fail();
    expect(send.mock.calls).toEqual([[{
      type: "route-failed", revision: 7, phase: "active", connectionId: config.connectionId,
    }]]);
    await route.disconnect();
  });

  it("reports the committed SFU subscription when a downstream Peer is preparing", async () => {
    const send = vi.fn(() => true);
    const stream = vi.fn();
    let events!: SubscriberEvents;
    const route = new ViewerSfuRoute("viewer_12345678", {
      activatePeer: () => true,
      onSfuStream: stream,
      send,
      createSubscriber: (callbacks) => {
        events = callbacks;
        return createFakeSubscriber(callbacks, [], "active");
      },
    });
    const config = sfuConfig(7);
    await prepareSfu(route, 7, viewerSfuAssignment(), config);
    events.onStream({} as MediaStream);
    events.onFirstDecodedFrame();
    await vi.waitFor(() => expect(stream).toHaveBeenCalledOnce());
    send.mockClear();
    route.accept({
      revision: 8, phase: "prepare",
      assignment: viewerSfuAssignment(["child"]), candidate: candidate(8, "child"),
    });
    events.onDisconnected();
    events.onDisconnected();
    expect(send.mock.calls).toEqual([[{
      type: "route-failed", revision: 7, phase: "active", connectionId: config.connectionId,
    }]]);
    await route.disconnect();
  });

  it("publishes SFU media during prepare and promotes before retiring old media", async () => {
    const log: string[] = [];
    const messages: ClientMessage[] = [];
    const publishers: ReturnType<typeof createFakePublisher>[] = [];
    const route = new HostSfuRoute({
      getStream: () => ({}) as MediaStream,
      getProfile: () => QUALITY_PROFILES["720p30"],
      getVideoCodec: () => "h264",
      reconcileChildren: () => undefined,
      send: (message) => {
        messages.push(message);
        return true;
      },
      createPublisher: () => {
        const publisher = createFakePublisher(
          log,
          `publisher-${publishers.length + 1}`,
        );
        publishers.push(publisher);
        return publisher;
      },
    });

    await route.acceptAndWait({
      revision: 1,
      phase: "active",
      assignment: hostAssignment("publication-a"),
    });
    await route.acceptConfig(sfuConfig(1, "publication-a"));
    expect(publishers[0]?.activate).toHaveBeenCalledOnce();
    expect(publishers[0]?.activate).toHaveBeenCalledWith(
      expect.anything(),
      QUALITY_PROFILES["720p30"],
      "h264",
    );

    route.accept({
      revision: 2,
      phase: "prepare",
      assignment: hostAssignment("publication-b"),
      candidate: candidate(2, "viewer_12345678", "sfu"),
    });
    await route.acceptConfig(sfuConfig(2, "publication-b"));
    expect(publishers[1]?.activate).toHaveBeenCalledOnce();
    expect(publishers[0]?.deactivate).not.toHaveBeenCalled();
    expect(messages.filter((message) => message.type === "route-ready")).toEqual([]);

    await route.acceptAndWait({
      revision: 2,
      phase: "active",
      assignment: hostAssignment("publication-b"),
    });
    expect(publishers[0]?.deactivate).toHaveBeenCalledOnce();
    expect(publishers[0]?.disconnect).toHaveBeenCalledOnce();
    expect(publishers[1]?.disconnect).not.toHaveBeenCalled();
  });

  it("renders an SFU connection failure in each language without placeholders", async () => {
    const publisher = createFakePublisher([], "publisher");
    publisher.connect.mockResolvedValue(false);
    const route = new HostSfuRoute({
      getStream: () => ({}) as MediaStream,
      getProfile: () => QUALITY_PROFILES["720p30"],
      getVideoCodec: () => "vp8",
      reconcileChildren: () => undefined,
      send: () => true,
      createPublisher: () => ({
        ...publisher,
        getFailureStage: () => "connect",
      }),
    });
    route.accept({
      revision: 1,
      phase: "prepare",
      assignment: hostAssignment("publication"),
      candidate: candidate(1, "viewer_12345678", "sfu"),
    });
    await route.acceptConfig(sfuConfig(1, "publication"));
    const warning = route.getQualityWarning();

    for (const lang of ["en", "zh"] as const) {
      const text = resolveMediaFailure(warning, {
        lang,
        t: (key, vars) => t(lang, key, vars),
      });
      expect(text).toBe(t(lang, "host.warn.sfuRecover", {
        params: t(lang, "host.warn.sfuStage.connect"),
      }));
      expect(text).not.toMatch(/\{[^}]+\}/);
    }
  });
});
