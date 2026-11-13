import { describe, expect, it, vi } from "vitest";
import { type ClientMessage, type ParticipantRouteAssignment } from "../../src/shared/protocol.ts";
import { HostSfuRoute } from "../../src/client/media/host-sfu-route.ts";
import { QUALITY_PROFILES } from "../../src/client/media/quality.ts";
import { ViewerSfuRoute } from "../../src/client/media/viewer-sfu-route";
import { type ConnectionMetrics } from "../../src/client/types.ts";
import { peerAssignment, viewerSfuAssignment, hostAssignment, sfuConfig, createFakePublisher, candidate, createFakeSubscriber, prepareSfu, receiveMetrics } from "./support.ts";

describe("minimal route transition contracts", () => {
  it("uses the subscriber first decoded frame as the only SFU prepare ready", async () => {
    const log: string[] = [];
    const messages: ClientMessage[] = [];
    const streams: MediaStream[] = [];
    const subscribers: ReturnType<typeof createFakeSubscriber>[] = [];
    const route = new ViewerSfuRoute("viewer_12345678", {
      activatePeer: () => true,
      reconcileSfuChildren: () => undefined,
      onSfuStream: (stream) => streams.push(stream),
      send: (message) => {
        messages.push(message);
        return true;
      },
      createSubscriber: (events) => {
        const subscriber = createFakeSubscriber(
          events,
          log,
          `subscriber-${subscribers.length + 1}`,
        );
        subscribers.push(subscriber);
        return subscriber;
      },
    });

    await prepareSfu(route, 1, viewerSfuAssignment(), sfuConfig(1));
    const firstStream = {} as MediaStream;
    subscribers[0]?.events.onStream(firstStream);
    subscribers[0]?.events.onFirstDecodedFrame();
    await vi.waitFor(() => expect(streams).toEqual([firstStream]));

    route.accept({
      revision: 2,
      phase: "prepare",
      assignment: viewerSfuAssignment([], "publication-generation-2"),
      candidate: candidate(2, "viewer_12345678", "sfu"),
    });
    await route.acceptConfig(sfuConfig(2, "publication-generation-2"));
    const pendingStream = {} as MediaStream;
    subscribers[1]?.events.onStream(pendingStream);
    subscribers[1]?.events.onStats?.({
      intervalFramesDecoded: 2,
    } as ConnectionMetrics);
    expect(messages.filter((message) => message.type === "route-ready")).toEqual([]);

    subscribers[1]?.events.onFirstDecodedFrame();
    subscribers[1]?.events.onFirstDecodedFrame();
    expect(messages.filter((message) => message.type === "route-ready")).toEqual([
      { type: "route-ready", revision: 2, phase: "prepare" },
    ]);
    expect(streams).toEqual([firstStream]);

    route.accept({
      revision: 2,
      phase: "active",
      assignment: viewerSfuAssignment([], "publication-generation-2"),
    });
    await vi.waitFor(() => expect(streams).toEqual([firstStream, pendingStream]));
    expect(subscribers[0]?.deactivate).toHaveBeenCalledOnce();
    expect(subscribers[0]?.disconnect).toHaveBeenCalledOnce();
  });

  it("keeps a quality SFU candidate pending until three non-regressing windows", async () => {
    const messages: ClientMessage[] = [];
    const subscribers: ReturnType<typeof createFakeSubscriber>[] = [];
    let currentMetrics = receiveMetrics(1_000);
    let qualityProbeEligible = true;
    const route = new ViewerSfuRoute("viewer_12345678", {
      activatePeer: () => true,
      reconcileSfuChildren: () => undefined,
      onSfuStream: () => undefined,
      currentPeerMetrics: () => currentMetrics,
      qualityProbeEligible: () => qualityProbeEligible,
      send: (message) => {
        messages.push(message);
        return true;
      },
      createSubscriber: (events) => {
        const subscriber = createFakeSubscriber(
          events,
          [],
          `subscriber-${subscribers.length + 1}`,
        );
        subscribers.push(subscriber);
        return subscriber;
      },
    });

    route.accept({
      revision: 1,
      phase: "active",
      assignment: peerAssignment("parent_12345678"),
    });
    route.accept({
      revision: 2,
      phase: "prepare",
      assignment: viewerSfuAssignment(),
      candidate: candidate(
        2,
        "viewer_12345678",
        "sfu",
        "candidate_2_12345678",
        true,
      ),
    });
    await route.acceptConfig(sfuConfig(2));
    const subscriber = subscribers[0]!;
    subscriber.events.onStream({} as MediaStream);
    subscriber.events.onFirstDecodedFrame();
    expect(messages.filter((message) => message.type === "route-ready")).toEqual([]);

    for (const timestampMs of [1_100, 3_100]) {
      currentMetrics = receiveMetrics(timestampMs - 100);
      subscriber.events.onStats(receiveMetrics(timestampMs));
    }
    expect(messages.filter((message) => message.type === "route-ready")).toEqual([]);

    qualityProbeEligible = false;
    for (const timestampMs of [5_100, 7_100, 9_100]) {
      currentMetrics = receiveMetrics(timestampMs - 100);
      subscriber.events.onStats(receiveMetrics(timestampMs));
    }
    expect(messages.filter((message) => message.type === "route-ready")).toEqual([]);

    qualityProbeEligible = true;
    for (const timestampMs of [11_100, 13_100, 15_100]) {
      currentMetrics = receiveMetrics(timestampMs - 100);
      subscriber.events.onStats(receiveMetrics(timestampMs));
    }
    expect(messages.filter((message) => message.type === "route-ready")).toEqual([
      { type: "route-ready", revision: 2, phase: "prepare" },
    ]);
    subscriber.isConnected.mockReturnValue(false);
    route.accept({ revision: 2, phase: "active", assignment: viewerSfuAssignment() });
    for (const timestampMs of [17_100, 19_100, 21_100]) {
      currentMetrics = receiveMetrics(timestampMs - 100);
      subscriber.events.onStats(receiveMetrics(timestampMs, { framesPerSecond: 1 }));
    }
    expect(messages.filter(message => message.type === "route-failed")).toEqual([]);
    expect(subscriber.disconnect).not.toHaveBeenCalled();
    subscriber.isConnected.mockReturnValue(true);
    subscriber.events.onState("connected");
    await route.disconnect();
  });

  it("keeps a low-quality SFU candidate until recovery proves non-regression", async () => {
    const messages: ClientMessage[] = [];
    const subscribers: ReturnType<typeof createFakeSubscriber>[] = [];
    let currentMetrics = receiveMetrics(1_000);
    const onSfuStream = vi.fn();
    const route = new ViewerSfuRoute("viewer_12345678", {
      activatePeer: () => true,
      reconcileSfuChildren: () => undefined,
      onSfuStream,
      currentPeerMetrics: () => currentMetrics,
      qualityProbeEligible: () => true,
      send: (message) => {
        messages.push(message);
        return true;
      },
      createSubscriber: (events) => {
        const subscriber = createFakeSubscriber(events, [], "candidate");
        subscribers.push(subscriber);
        return subscriber;
      },
    });

    route.accept({
      revision: 1,
      phase: "active",
      assignment: peerAssignment("parent_12345678"),
    });
    route.accept({
      revision: 2,
      phase: "prepare",
      assignment: viewerSfuAssignment(),
      candidate: candidate(
        2,
        "viewer_12345678",
        "sfu",
        "candidate_2_12345678",
        true,
      ),
    });
    await route.acceptConfig(sfuConfig(2));
    const subscriber = subscribers[0]!;
    subscriber.events.onStream({} as MediaStream);
    subscriber.events.onFirstDecodedFrame();

    for (const timestampMs of [1_100, 3_100, 5_100]) {
      currentMetrics = receiveMetrics(timestampMs - 100);
      subscriber.events.onStats(
        receiveMetrics(timestampMs, {
          framesPerSecond: 30,
          intervalFramesDecoded: 60,
        }),
      );
    }

    expect(messages.filter(message => message.type === "route-failed" || message.type === "route-ready")).toEqual([]);
    expect(subscriber.disconnect).not.toHaveBeenCalled();
    expect(onSfuStream).not.toHaveBeenCalled();

    for (const timestampMs of [7_100, 9_100, 11_100]) {
      currentMetrics = receiveMetrics(timestampMs - 100);
      subscriber.events.onStats(receiveMetrics(timestampMs));
    }
    expect(messages.filter(message => message.type === "route-failed")).toEqual([]);
    expect(messages.filter(message => message.type === "route-ready")).toEqual([
      { type: "route-ready", revision: 2, phase: "prepare" },
    ]);
    expect(subscriber.disconnect).not.toHaveBeenCalled();
    expect(onSfuStream).not.toHaveBeenCalled();
    await route.disconnect();
  });

  it("reconnects only the active SFU subscriber on the current route", async () => {
    const messages: ClientMessage[] = [];
    const streams: MediaStream[] = [];
    const subscribers: ReturnType<typeof createFakeSubscriber>[] = [];
    const resetMedia = vi.fn();
    const route = new ViewerSfuRoute("viewer_12345678", {
      activatePeer: () => true,
      resetMedia,
      reconcileSfuChildren: () => undefined,
      onSfuStream: (stream) => streams.push(stream),
      send: (message) => {
        messages.push(message);
        return true;
      },
      createSubscriber: (events) => {
        const subscriber = createFakeSubscriber(
          events,
          [],
          `subscriber-${subscribers.length + 1}`,
        );
        subscribers.push(subscriber);
        return subscriber;
      },
    });

    await prepareSfu(route, 7, viewerSfuAssignment(), sfuConfig(7));
    const firstStream = {} as MediaStream;
    subscribers[0]?.events.onStream(firstStream);
    subscribers[0]?.events.onFirstDecodedFrame();
    await vi.waitFor(() => expect(streams).toEqual([firstStream]));

    expect(route.reconnectActive()).toBe(true);
    expect(subscribers[0]?.reconnect).toHaveBeenCalledOnce();
    expect(resetMedia).not.toHaveBeenCalled();
    expect(subscribers[0]?.deactivate).not.toHaveBeenCalled();
    expect(subscribers[0]?.disconnect).not.toHaveBeenCalled();

    await route.acceptConfig(sfuConfig(7));
    expect(subscribers).toHaveLength(1);
    expect(streams).toEqual([firstStream]);
    expect(subscribers[0]?.disconnect).not.toHaveBeenCalled();
    await route.disconnect();
  });

  it("keeps an active SFU subscriber across same-publication resync", async () => {
    const subscribers: ReturnType<typeof createFakeSubscriber>[] = [];
    const streams: MediaStream[] = [];
    const resetMedia = vi.fn();
    const route = new ViewerSfuRoute("viewer_12345678", {
      activatePeer: () => true,
      resetMedia,
      reconcileSfuChildren: () => undefined,
      onSfuStream: (stream) => streams.push(stream),
      send: () => true,
      createSubscriber: (events) => {
        const subscriber = createFakeSubscriber(events, [], "active");
        subscribers.push(subscriber);
        return subscriber;
      },
    });
    const assignment = viewerSfuAssignment();
    await prepareSfu(route, 7, assignment, sfuConfig(7));
    const initialStream = {} as MediaStream;
    subscribers[0]?.events.onStream(initialStream);
    subscribers[0]?.events.onFirstDecodedFrame();
    await vi.waitFor(() => expect(streams).toEqual([initialStream]));

    await expect(
      route.resyncAuthoritative(
        { revision: 8, phase: "active", assignment },
        "viewer_12345678",
      ),
    ).resolves.toBe("accepted");
    expect(subscribers).toHaveLength(1);
    expect(subscribers[0]?.deactivate).not.toHaveBeenCalled();
    expect(subscribers[0]?.disconnect).not.toHaveBeenCalled();
    expect(resetMedia).not.toHaveBeenCalled();
    await route.disconnect();
  });

  it("retains the physical SFU subscriber when a room revision advances during reconnect", async () => {
    const messages: ClientMessage[] = [];
    const subscribers: ReturnType<typeof createFakeSubscriber>[] = [];
    const streams: MediaStream[] = [];
    const route = new ViewerSfuRoute("viewer_12345678", {
      activatePeer: () => true,
      reconcileSfuChildren: () => undefined,
      onSfuStream: (stream) => streams.push(stream),
      send: (message) => {
        messages.push(message);
        return true;
      },
      createSubscriber: (events) => {
        const subscriber = createFakeSubscriber(events, [], "active");
        subscribers.push(subscriber);
        return subscriber;
      },
    });
    const assignment = viewerSfuAssignment();
    await prepareSfu(route, 7, assignment, sfuConfig(7));
    const initialStream = {} as MediaStream;
    subscribers[0]?.events.onStream(initialStream);
    subscribers[0]?.events.onFirstDecodedFrame();
    await vi.waitFor(() => expect(streams).toEqual([initialStream]));
    messages.length = 0;
    expect(route.reconnectActive()).toBe(true);

    route.accept({ revision: 8, phase: "active", assignment });
    await vi.waitFor(() => expect(subscribers[0]?.updateConfig).toHaveBeenLastCalledWith(
      expect.objectContaining({ revision: 8 }),
    ));
    expect(messages.filter((message) => message.type === "refresh-sfu")).toEqual([]);
    expect(subscribers[0]?.disconnect).not.toHaveBeenCalled();
    await route.disconnect();
  });

  it("keeps SFU reconnect on the physical subscriber through an unrelated prepare", async () => {
    const messages: ClientMessage[] = [];
    const streams: MediaStream[] = [];
    const subscribers: ReturnType<typeof createFakeSubscriber>[] = [];
    const route = new ViewerSfuRoute("viewer_12345678", {
      activatePeer: () => true,
      reconcileSfuChildren: () => undefined,
      onSfuStream: (stream) => streams.push(stream),
      send: (message) => {
        messages.push(message);
        return true;
      },
      createSubscriber: (events) => {
        const subscriber = createFakeSubscriber(events, [], "active");
        subscribers.push(subscriber);
        return subscriber;
      },
    });
    const assignment = viewerSfuAssignment(["relay-child"]);
    await prepareSfu(route, 7, assignment, sfuConfig(7));
    subscribers[0]!.events.onStream({} as MediaStream);
    subscribers[0]!.events.onFirstDecodedFrame();
    await vi.waitFor(() => expect(streams).toHaveLength(1));
    expect(route.reconnectActive()).toBe(true);

    route.accept({
      revision: 8,
      phase: "prepare",
      assignment,
      candidate: candidate(8, "relay-child"),
    });
    route.accept({ revision: 8, phase: "active", assignment });

    await vi.waitFor(() => expect(subscribers[0]?.updateConfig).toHaveBeenLastCalledWith(
      expect.objectContaining({ revision: 8 }),
    ));
    expect(subscribers[0]?.reconnect).toHaveBeenCalledOnce();
    expect(subscribers[0]?.disconnect).not.toHaveBeenCalled();
    expect(messages.filter((message) => message.type === "refresh-sfu").every((message) => message.revision === 7)).toBe(true);
  });

  it("reports a failed subscription again after offline reauthentication without creating an empty peer", async () => {
    const messages: ClientMessage[] = [];
    const streams: MediaStream[] = [];
    let acceptsFailure = false;
    const subscribers: ReturnType<typeof createFakeSubscriber>[] = [];
    const route = new ViewerSfuRoute("viewer_12345678", {
      activatePeer: () => true,
      reconcileSfuChildren: () => undefined,
      onSfuStream: (stream) => streams.push(stream),
      send: (message) => {
        messages.push(message);
        return message.type !== "route-failed" || acceptsFailure;
      },
      createSubscriber: (events) => {
        const subscriber = createFakeSubscriber(events, [], "active");
        subscribers.push(subscriber);
        return subscriber;
      },
    });
    await prepareSfu(route, 7, viewerSfuAssignment(), sfuConfig(7));
    subscribers[0]!.events.onStream({} as MediaStream);
    subscribers[0]!.events.onFirstDecodedFrame();
    await vi.waitFor(() => expect(streams).toHaveLength(1));
    subscribers[0]!.events.onDisconnected();
    acceptsFailure = true;

    await route.resyncAuthoritative({ revision: 7, phase: "active", assignment: viewerSfuAssignment() }, "viewer_12345678");
    await route.acceptConfig(sfuConfig(7));
    expect(subscribers).toHaveLength(1);
    expect(messages.filter(message => message.type === "route-failed")).toContainEqual({
      type: "route-failed", revision: 7, phase: "active", connectionId: sfuConfig(7).connectionId,
    });
    messages.length = 0;

    route.accept({
      revision: 8,
      phase: "active",
      assignment: viewerSfuAssignment(),
    });
    await vi.waitFor(() =>
      expect(
        messages.filter((message) => message.type === "route-failed"),
      ).toEqual([
        { type: "route-failed", revision: 8, phase: "active", connectionId: sfuConfig(7).connectionId },
      ]),
    );
    await route.disconnect();
  });

  it("does not resync a retained subscriber after a newer preparation supersedes authentication", async () => {
    const subscribers: ReturnType<typeof createFakeSubscriber>[] = [];
    const streams: MediaStream[] = [];
    const route = new ViewerSfuRoute("viewer_12345678", {
      activatePeer: () => true, send: () => true,
      onSfuStream: stream => streams.push(stream),
      createSubscriber: events => {
        const subscriber = createFakeSubscriber(events, [], "retained");
        subscribers.push(subscriber);
        return subscriber;
      },
    });
    await prepareSfu(route, 7, viewerSfuAssignment(), sfuConfig(7));
    subscribers[0]!.events.onStream({} as MediaStream);
    subscribers[0]!.events.onFirstDecodedFrame();
    await vi.waitFor(() => expect(streams).toHaveLength(1));
    const resync = route.resyncAuthoritative({ revision: 7, phase: "active",
      assignment: viewerSfuAssignment() }, "viewer_12345678");
    route.accept({ revision: 8, phase: "prepare", assignment: peerAssignment("parent_12345678"),
      candidate: candidate(8) });
    await resync;
    expect(subscribers[0]!.resync).not.toHaveBeenCalled();
    expect(subscribers[0]!.disconnect).not.toHaveBeenCalled();
    await route.disconnect();
  });

  it("recovers a failed subscription through a new server-owned preparation", async () => {
    let releaseRecovery!: (connected: boolean) => void;
    const recoveryConnected = new Promise<boolean>((resolve) => {
      releaseRecovery = resolve;
    });
    const subscribers: ReturnType<typeof createFakeSubscriber>[] = [];
    const streams: MediaStream[] = [];
    const route = new ViewerSfuRoute("viewer_12345678", {
      activatePeer: () => true,
      reconcileSfuChildren: () => undefined,
      onSfuStream: (stream) => streams.push(stream),
      send: () => true,
      createSubscriber: (events) => {
        const subscriber = createFakeSubscriber(events, [], "subscriber");
        if (subscribers.length === 1) {
          subscriber.connect.mockImplementation(() => recoveryConnected);
        }
        subscribers.push(subscriber);
        return subscriber;
      },
    });
    const assignment = viewerSfuAssignment();
    await prepareSfu(route, 7, assignment, sfuConfig(7));
    const initialStream = {} as MediaStream;
    subscribers[0]?.events.onStream(initialStream);
    subscribers[0]?.events.onFirstDecodedFrame();
    await vi.waitFor(() => expect(streams).toEqual([initialStream]));

    subscribers[0]!.events.onDisconnected();
    route.accept({ revision: 8, phase: "prepare", assignment,
      candidate: candidate(8, "viewer_12345678", "sfu", "recovery_connection") });
    const recovery = route.acceptConfig({
      ...sfuConfig(8),
      connectionId: "recovery_connection",
    });
    await vi.waitFor(() => expect(subscribers).toHaveLength(2));
    expect(subscribers).toHaveLength(2);
    expect(subscribers[1]?.disconnect).not.toHaveBeenCalled();

    releaseRecovery(true);
    await recovery;
    const recoveredStream = {} as MediaStream;
    subscribers[1]?.events.onStream(recoveredStream);
    subscribers[1]?.events.onFirstDecodedFrame();
    route.accept({ revision: 8, phase: "active", assignment });
    await vi.waitFor(() =>
      expect(streams).toEqual([initialStream, recoveredStream]),
    );
    expect(subscribers[0]?.disconnect).toHaveBeenCalledOnce();
    await route.disconnect();
  });

  it("discards a paused pending subscriber and ignores its later evidence", async () => {
    const messages: ClientMessage[] = [];
    let subscriber!: ReturnType<typeof createFakeSubscriber>;
    const route = new ViewerSfuRoute("viewer_12345678", {
      activatePeer: () => true,
      reconcileSfuChildren: () => undefined,
      onSfuStream: () => undefined,
      send: (message) => {
        messages.push(message);
        return true;
      },
      createSubscriber: (events) => {
        subscriber = createFakeSubscriber(events, [], "pending");
        return subscriber;
      },
    });

    route.accept({
      revision: 3,
      phase: "prepare",
      assignment: viewerSfuAssignment(),
      candidate: candidate(3, "viewer_12345678", "sfu"),
    });
    await route.acceptConfig(sfuConfig(3));
    route.setPaused(true);
    expect(subscriber.disconnect).toHaveBeenCalledOnce();

    subscriber.events.onStream({} as MediaStream);
    subscriber.events.onFirstDecodedFrame();
    expect(messages.filter((message) => message.type === "route-ready")).toEqual([]);
  });

  it("reports paused subscription failure and waits for the server to prepare its replacement", async () => {
    const messages: ClientMessage[] = [];
    const streams: MediaStream[] = [];
    const subscribers: ReturnType<typeof createFakeSubscriber>[] = [];
    const route = new ViewerSfuRoute("viewer_12345678", {
      activatePeer: () => true,
      reconcileSfuChildren: () => undefined,
      onSfuStream: (stream) => streams.push(stream),
      send: (message) => {
        messages.push(message);
        return true;
      },
      createSubscriber: (events) => {
        const subscriber = createFakeSubscriber(
          events,
          [],
          `subscriber-${subscribers.length + 1}`,
        );
        subscribers.push(subscriber);
        return subscriber;
      },
    });

    await prepareSfu(route, 7, viewerSfuAssignment(), sfuConfig(7));
    subscribers[0]!.events.onStream({} as MediaStream);
    subscribers[0]!.events.onFirstDecodedFrame();
    await vi.waitFor(() => expect(streams).toHaveLength(1));

    route.setPaused(true);
    subscribers[0]!.events.onDisconnected();
    expect(messages).toContainEqual({ type: "route-failed", revision: 7, phase: "active", connectionId: sfuConfig(7).connectionId });

    await route.acceptConfig({ ...sfuConfig(7), connectionId: "recovery_connection" });
    expect(subscribers).toHaveLength(1);

    route.setPaused(false);
    await prepareSfu(route, 8);
    expect(subscribers[1]!.armDecodedFrameProof).toHaveBeenCalledOnce();
    await route.disconnect();
  });

  it("retires only an unreferenced paused Viewer SFU slot", async () => {
    const messages: ClientMessage[] = [];
    const activatedPeers: ParticipantRouteAssignment[] = [];
    const activatedChildren: string[][] = [];
    const streams: MediaStream[] = [];
    const subscribers: ReturnType<typeof createFakeSubscriber>[] = [];
    const route = new ViewerSfuRoute("viewer_12345678", {
      activatePeer: (assignment) => {
        activatedPeers.push(assignment);
        return true;
      },
      activateChildren: (children) => activatedChildren.push([...children]),
      onSfuStream: (stream) => streams.push(stream),
      send: (message) => {
        messages.push(message);
        return true;
      },
      createSubscriber: (events) => {
        const subscriber = createFakeSubscriber(
          events,
          [],
          `subscriber-${subscribers.length + 1}`,
        );
        subscribers.push(subscriber);
        return subscriber;
      },
    });

    await prepareSfu(route, 1, viewerSfuAssignment([], "publication-old"), sfuConfig(1, "publication-old"));
    const oldStream = {} as MediaStream;
    subscribers[0]?.events.onStream(oldStream);
    subscribers[0]?.events.onFirstDecodedFrame();
    await vi.waitFor(() => expect(streams).toEqual([oldStream]));
    route.setPaused(true);

    route.accept({
      revision: 2,
      phase: "active",
      assignment: viewerSfuAssignment(
        ["retained-child"],
        "publication-old",
      ),
    });
    await vi.waitFor(() =>
      expect(activatedChildren.at(-1)).toEqual(["retained-child"]),
    );
    expect(subscribers[0]?.disconnect).not.toHaveBeenCalled();
    expect(
      route.accept({
        revision: 2,
        phase: "active",
        assignment: viewerSfuAssignment(
          ["retained-child"],
          "publication-old",
        ),
      }),
    ).toBe("duplicate");
    expect(subscribers[0]?.disconnect).not.toHaveBeenCalled();

    route.accept({
      revision: 3,
      phase: "active",
      assignment: peerAssignment("retained-parent", ["retained-child"]),
    });
    await vi.waitFor(() =>
      expect(subscribers[0]?.disconnect).toHaveBeenCalledOnce(),
    );
    expect(activatedPeers.at(-1)).toEqual(
      peerAssignment("retained-parent", ["retained-child"]),
    );
  });

  it("preserves a committed Host publication while old source retirement delays activation", async () => {
    const publishers: ReturnType<typeof createFakePublisher>[] = [];
    const reconciledChildren: Array<{ childPeerIds: string[]; revision: number }> = [];
    const stream = {} as MediaStream;
    const route = new HostSfuRoute({
      getStream: () => stream,
      getProfile: () => QUALITY_PROFILES["720p30"],
      getVideoCodec: () => "h264",
      reconcileChildren: (assignment, revision) => {
        reconciledChildren.push({ childPeerIds: assignment.childPeerIds, revision });
      },
      send: () => true,
      createPublisher: () => {
        const publisher = createFakePublisher([], "publisher-" + publishers.length);
        publishers.push(publisher);
        return publisher;
      },
    });
    await route.acceptAndWait({
      revision: 1,
      phase: "active",
      assignment: hostAssignment("publication-old"),
    });
    await route.acceptConfig(sfuConfig(1, "publication-old"));
    route.accept({
      revision: 2,
      phase: "prepare",
      assignment: hostAssignment("publication-new", ["committed-child"]),
      candidate: candidate(2, "viewer_first_12345678", "sfu"),
    });
    await route.acceptConfig(sfuConfig(2, "publication-new"));
    expect(publishers[1]!.activate).toHaveBeenCalledOnce();

    let releaseOld!: () => void;
    const oldRetirement = new Promise<void>((resolve) => {
      releaseOld = resolve;
    });
    publishers[0]!.replaceStream.mockResolvedValue(false);
    publishers[0]!.disconnect.mockImplementation(() => oldRetirement);
    const replacing = route.replaceStream({} as MediaStream);
    const nextChild = {
      revision: 3,
      phase: "prepare" as const,
      assignment: hostAssignment("publication-new", ["committed-child", "viewer_next_12345678"]),
      candidate: candidate(3, "viewer_next_12345678", "direct"),
    };
    try {
      await vi.waitFor(() => expect(publishers[0]!.disconnect).toHaveBeenCalledOnce());
      // HostPage accepts route updates without awaiting local activation.
      route.accept({
        revision: 2,
        phase: "active",
        assignment: hostAssignment("publication-new", ["committed-child"]),
      });
      expect(reconciledChildren.at(-1)).toEqual({
        childPeerIds: ["committed-child"],
        revision: 2,
      });
      route.accept(nextChild);
      expect(publishers[1]!.disconnect).not.toHaveBeenCalled();
      releaseOld();
      await replacing;
      await route.acceptAndWait(nextChild);
      expect(publishers[1]!.disconnect).not.toHaveBeenCalled();
    } finally {
      releaseOld();
      await replacing;
      await route.disconnect();
    }
  });
});
