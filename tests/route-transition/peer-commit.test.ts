import { describe, expect, it, vi } from "vitest";
import { type ClientMessage, type ParticipantRouteAssignment } from "../../src/shared/protocol.ts";
import { MediaRouteTransition } from "../../src/client/media/route-transition.ts";
import { ViewerSfuRoute } from "../../src/client/media/viewer-sfu-route";
import { peerAssignment, viewerSfuAssignment, emptyViewerAssignment, sfuConfig, candidate, createFakeSubscriber, prepareSfu } from "./support.ts";

describe("minimal route transition contracts", () => {
  it.each(["connected", "failed", "superseded"] as const)("retains the old media while a committed Peer waits, then handles %s", async outcome => {
    const messages: ClientMessage[] = [];
    let status: "disconnected" | "connected" | "failed" = "disconnected";
    const activatePeer = vi.fn((_assignment: ParticipantRouteAssignment, revision?: number): boolean | "waiting" =>
      revision === undefined || status === "connected" ? true : status === "disconnected" ? "waiting" : false);
    let subscriber!: ReturnType<typeof createFakeSubscriber>;
    const onSfuStream = vi.fn();
    const route = new ViewerSfuRoute("viewer_12345678", {
      activatePeer, onSfuStream,
      send: message => { messages.push(message); return true; },
      createSubscriber: events => (subscriber = createFakeSubscriber(events, [], "retained")),
    });
    await prepareSfu(route, 1);
    subscriber.events.onStream({} as MediaStream);
    subscriber.events.onFirstDecodedFrame();
    await vi.waitFor(() => expect(onSfuStream).toHaveBeenCalledOnce());
    const assignment = peerAssignment("candidate_parent");
    route.accept({ revision: 2, phase: "prepare", assignment, candidate: candidate(2) });
    route.accept({ revision: 2, phase: "active", assignment });
    await vi.waitFor(() => expect(activatePeer).toHaveBeenCalledWith(assignment, 2));
    expect(messages.filter(message => message.type === "route-failed")).toEqual([]);
    expect(subscriber.disconnect).not.toHaveBeenCalled();
    expect(route.retryPeerActivation("retired_connection")).toBe(false);

    if (outcome === "superseded") {
      const replacement = peerAssignment("replacement_parent");
      route.accept({ revision: 3, phase: "prepare", assignment: replacement, candidate: candidate(3) });
      status = "connected";
      expect(route.retryPeerActivation(candidate(2).connectionId)).toBe(false);
      expect(route.retryPeerActivation(candidate(3).connectionId)).toBe(false);
      expect(subscriber.disconnect).not.toHaveBeenCalled();
      route.accept({ revision: 3, phase: "active", assignment: replacement });
      await vi.waitFor(() => expect(subscriber.disconnect).toHaveBeenCalledOnce());
      expect(activatePeer).toHaveBeenCalledWith(replacement, 3);
    } else {
      route.accept({ revision: 3, phase: "prepare", assignment: peerAssignment("candidate_parent", ["next_child"]),
        candidate: candidate(3, "next_child") });
      status = outcome;
      expect(route.retryPeerActivation(candidate(2).connectionId)).toBe(true);
      if (outcome === "connected") await vi.waitFor(() => expect(subscriber.disconnect).toHaveBeenCalledOnce());
      else {
        await vi.waitFor(() => expect(messages).toContainEqual({
          type: "route-failed", revision: 2, phase: "active", connectionId: candidate(2).connectionId,
        }));
        expect(subscriber.disconnect).not.toHaveBeenCalled();
      }
    }
    expect(messages.filter(message => message.type === "route-failed")).toHaveLength(outcome === "failed" ? 1 : 0);
    await route.disconnect();
  });

  it("reports active failure when a prepared Peer disappears before commit delivery", async () => {
    const messages: ClientMessage[] = [];
    const onSfuStream = vi.fn();
    let subscriber!: ReturnType<typeof createFakeSubscriber>;
    const route = new ViewerSfuRoute("viewer_12345678", {
      activatePeer: () => false,
      onSfuStream,
      send: (message) => { messages.push(message); return true; },
      createSubscriber: (events) => {
        subscriber = createFakeSubscriber(events, [], "current");
        return subscriber;
      },
    });
    await prepareSfu(route, 1, viewerSfuAssignment(), sfuConfig(1));
    subscriber.events.onStream({} as MediaStream);
    subscriber.events.onFirstDecodedFrame();
    await vi.waitFor(() => expect(onSfuStream).toHaveBeenCalledOnce());
    messages.length = 0;

    const assignment = peerAssignment("host_12345678");
    route.accept({ revision: 2, phase: "prepare", assignment, candidate: candidate(2) });
    // First-frame readiness reached the server, then the local candidate failed.
    // Its prepare failure can arrive after the server committed this revision.
    route.accept({ revision: 2, phase: "active", assignment });
    await vi.waitFor(() => expect(messages).toContainEqual({
      type: "route-failed", revision: 2, phase: "active", connectionId: candidate(2).connectionId,
    }));
    expect(messages).not.toContainEqual({ type: "route-ready", revision: 2, phase: "active" });
    expect(subscriber.disconnect).not.toHaveBeenCalled();
    await route.disconnect();
  });

  it.each([false, true])("ignores a superseded Peer promotion resolving to %s", async (promoted) => {
    const messages: ClientMessage[] = [];
    const onSfuStream = vi.fn();
    let finishPromotion!: (value: boolean) => void;
    const promotion = new Promise<boolean>((resolve) => { finishPromotion = resolve; });
    const activatePeer = vi.fn((_assignment: ParticipantRouteAssignment, revision?: number) =>
      revision === 2 ? promotion : true,
    );
    let subscriber!: ReturnType<typeof createFakeSubscriber>;
    const route = new ViewerSfuRoute("viewer_12345678", {
      activatePeer,
      onSfuStream,
      send: (message) => { messages.push(message); return true; },
      createSubscriber: (events) => {
        subscriber = createFakeSubscriber(events, [], "current");
        return subscriber;
      },
    });
    await prepareSfu(route, 1, viewerSfuAssignment(), sfuConfig(1));
    subscriber.events.onStream({} as MediaStream);
    subscriber.events.onFirstDecodedFrame();
    await vi.waitFor(() => expect(onSfuStream).toHaveBeenCalledOnce());
    messages.length = 0;

    const first = peerAssignment("first_parent");
    route.accept({ revision: 2, phase: "prepare", assignment: first, candidate: candidate(2) });
    route.accept({ revision: 2, phase: "active", assignment: first });
    await vi.waitFor(() => expect(activatePeer).toHaveBeenCalledWith(first, 2));
    const next = peerAssignment("next_parent");
    route.accept({ revision: 3, phase: "prepare", assignment: next, candidate: candidate(3) });
    finishPromotion(promoted);
    await promotion;
    await Promise.resolve();
    expect(messages).toEqual([]);
    expect(subscriber.disconnect).not.toHaveBeenCalled();

    route.accept({ revision: 3, phase: "active", assignment: next });
    await vi.waitFor(() => expect(activatePeer).toHaveBeenCalledWith(next, 3));
    await route.disconnect();
  });

  it("rebinds Viewer route ownership before a post-restart prepare", async () => {
    const prepared: Array<{ parentPeerId: string | null; revision?: number }> = [];
    const route = new ViewerSfuRoute("viewer_old_12345678", {
      activatePeer: () => true,
      preparePeer: (assignment, revision) =>
        prepared.push({
          parentPeerId:
            assignment?.upstream.kind === "peer"
              ? assignment.upstream.peerId
              : null,
          ...(revision === undefined ? {} : { revision }),
        }),
      resetMedia: () => undefined,
      reconcileSfuChildren: () => undefined,
      onSfuStream: () => undefined,
      send: () => true,
    });
    route.accept({
      revision: 9,
      phase: "active",
      assignment: peerAssignment("old_parent_12345678"),
    });

    const resync = route.resyncAuthoritative(
      {
        revision: 0,
        phase: "active",
        assignment: emptyViewerAssignment(),
      },
      "viewer_new_12345678",
    );
    expect(
      route.accept({
        revision: 1,
        phase: "prepare",
        assignment: peerAssignment("new_parent_12345678"),
        candidate: candidate(
          1,
          "viewer_new_12345678",
          "direct",
          "post_restart_connection_12345678",
        ),
      }),
    ).toBe("accepted");
    await resync;

    expect(prepared.at(-1)).toEqual({
      parentPeerId: "new_parent_12345678",
      revision: 1,
    });
  });

  it.each(["prepare", "active"] as const)("promotes a committed upstream behind SDP while preserving the next child %s", async (phase) => {
    let finishSdp!: () => void;
    const sdp = new Promise<void>((resolve) => { finishSdp = resolve; });
    const activatePeer = vi.fn((_assignment: ParticipantRouteAssignment, revision?: number) =>
      revision === undefined ? sdp : true);
    const preparePeer = vi.fn();
    const prepareChild = vi.fn();
    const route = new ViewerSfuRoute("viewer_12345678", {
      activatePeer, preparePeer, prepareChild,
      onSfuStream: () => undefined,
      send: () => true,
    });
    const committed = peerAssignment("next_parent");
    const withChild = peerAssignment("next_parent", ["next_child"]);
    const nextChild = candidate(3, "next_child");
    try {
      route.accept({ revision: 1, phase: "active", assignment: peerAssignment("old_parent") });
      await vi.waitFor(() => expect(activatePeer).toHaveBeenCalledOnce());
      route.accept({ revision: 2, phase: "prepare", assignment: committed, candidate: candidate(2) });
      // Active is the server's acceptance of this prepared input's readiness.
      route.accept({ revision: 2, phase: "active", assignment: committed });
      const preparations = preparePeer.mock.calls.length;
      route.accept({ revision: 3, phase: "prepare", assignment: withChild, candidate: nextChild });
      if (phase === "active") route.accept({ revision: 3, phase, assignment: withChild });
      expect.soft(preparePeer.mock.calls.slice(preparations)).not.toContainEqual([null]);
      finishSdp();
      await vi.waitFor(() => expect(activatePeer).toHaveBeenCalledWith(phase === "active" ? withChild : committed, 2));
      expect(prepareChild).toHaveBeenLastCalledWith(nextChild, ["next_child"], 3);
    } finally {
      finishSdp();
      await route.disconnect();
    }
  });

  it.each(["promote", "fail"] as const)("keeps committed SFU input behind SDP during a child operation, then %s", async outcome => {
    let finishSdp!: () => void;
    const sdp = new Promise<void>(resolve => { finishSdp = resolve; });
    const onSfuStream = vi.fn();
    const messages: ClientMessage[] = [];
    let subscriber!: ReturnType<typeof createFakeSubscriber>;
    const route = new ViewerSfuRoute("viewer_12345678", {
      activatePeer: () => sdp, onSfuStream,
      send: message => { messages.push(message); return true; },
      createSubscriber: events => (subscriber = createFakeSubscriber(events, [], "committed")),
    });
    const stream = {} as MediaStream;
    const assignment = viewerSfuAssignment(["next_child"]);
    try {
      route.accept({ revision: 1, phase: "active", assignment: peerAssignment("old_parent") });
      // Let the preceding active transition enter its pending SDP operation.
      await Promise.resolve();
      route.accept({ revision: 2, phase: "prepare", assignment: viewerSfuAssignment(),
        candidate: candidate(2, "viewer_12345678", "sfu") });
      await route.acceptConfig(sfuConfig(2));
      subscriber.events.onStream(stream);
      subscriber.events.onFirstDecodedFrame();
      expect(messages).toContainEqual({ type: "route-ready", revision: 2, phase: "prepare" });
      route.accept({ revision: 2, phase: "active", assignment: viewerSfuAssignment() });
      route.accept({ revision: 3, phase: "prepare", assignment, candidate: candidate(3, "next_child") });
      route.accept({ revision: 3, phase: "active", assignment });
      expect(subscriber.disconnect).not.toHaveBeenCalled();
      if (outcome === "fail") subscriber.events.onDisconnected();
      finishSdp();
      if (outcome === "promote") {
        await vi.waitFor(() => expect(onSfuStream).toHaveBeenCalledWith(stream, assignment, true, 3));
      } else {
        expect(messages).toContainEqual({ type: "route-failed", revision: 3, phase: "active", connectionId: sfuConfig(2).connectionId });
        expect(onSfuStream).not.toHaveBeenCalled();
      }
    } finally {
      finishSdp();
      await route.disconnect();
    }
  });

  it("keeps old media through prepare and commits only exact active authority", () => {
    const route = new MediaRouteTransition();
    const oldAssignment = peerAssignment("old-parent");
    const nextAssignment = peerAssignment("next-parent");

    route.accept({ revision: 1, phase: "active", assignment: oldAssignment });
    const oldToken = route.token()!;
    expect(route.markMediaActive(oldToken)).toBe(true);

    route.accept({
      revision: 2,
      phase: "prepare",
      assignment: nextAssignment,
      candidate: candidate(2),
    });
    expect(route.getMediaAssignment()).toEqual(oldAssignment);
    expect(route.markMediaActive(route.token()!)).toBe(false);
    expect(route.ownsActive(oldToken)).toBe(true);

    route.accept({ revision: 2, phase: "active", assignment: nextAssignment });
    expect(route.markMediaActive(route.token()!)).toBe(true);
    expect(route.getMediaAssignment()).toEqual(nextAssignment);
    expect(
      route.accept({ revision: 1, phase: "active", assignment: oldAssignment }),
    ).toBe("stale");
    const committed = route.getActiveToken()!;
    route.reset();
    route.accept({ revision: 2, phase: "active", assignment: nextAssignment });
    expect(route.ownsActive(committed)).toBe(false);
  });

  it("treats child assignment order as presentation-only", () => {
    const route = new MediaRouteTransition();
    route.accept({
      revision: 1,
      phase: "active",
      assignment: peerAssignment("parent", ["first", "second"]),
    });
    route.accept({
      revision: 2,
      phase: "prepare",
      assignment: peerAssignment("parent", ["second", "first"]),
      candidate: candidate(2, "second"),
    });

    expect(
      route.accept({
        revision: 2,
        phase: "active",
        assignment: peerAssignment("parent", ["first", "second"]),
      }),
    ).toBe("accepted");
  });
});
