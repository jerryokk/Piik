import { describe, expect, it, vi } from "vitest";
import { type ClientMessage } from "../../src/shared/protocol.ts";
import { HostSfuRoute } from "../../src/client/media/host-sfu-route.ts";
import { NativeSfuPublisher } from "../../src/client/native/native-sfu-publisher.ts";
import { QUALITY_PROFILES } from "../../src/client/media/quality.ts";
import { invalidateSenderQualityEvidence, sfuPublisherQualityEvidenceFromMetrics } from "../../src/client/media/sender-quality-evidence.ts";
import { ViewerSfuRoute } from "../../src/client/media/viewer-sfu-route";
import { EMPTY_METRICS } from "../../src/client/types.ts";
import { viewerSfuAssignment, hostAssignment, sfuConfig, createFakePublisher, candidate, createFakeSubscriber, prepareSfu } from "./support.ts";

describe("minimal route transition contracts", () => {
  it("changes Host children only after active authority commits", async () => {
    const reconciledChildren: string[][] = [];
    const route = new HostSfuRoute({
      getStream: () => ({}) as MediaStream,
      getProfile: () => QUALITY_PROFILES["720p30"],
      getVideoCodec: () => "vp8",
      reconcileChildren: (assignment) => reconciledChildren.push(assignment.childPeerIds),
      send: () => true,
      createPublisher: () => createFakePublisher([], "publisher"),
    });
    await route.acceptAndWait({
      revision: 1,
      phase: "active",
      assignment: hostAssignment("publication", ["committed-child"]),
    });
    await route.acceptConfig(sfuConfig(1, "publication"));
    reconciledChildren.length = 0;

    route.accept({
      revision: 2,
      phase: "prepare",
      assignment: hostAssignment(null, ["candidate-child"]),
      candidate: candidate(2, "candidate-child"),
    });
    expect(reconciledChildren).toEqual([]);

    await route.acceptAndWait({
      revision: 2,
      phase: "active",
      assignment: hostAssignment(null, ["candidate-child"]),
    });
    expect(reconciledChildren).toEqual([["candidate-child"]]);
  });

  it("replaces a Host physical connection within one publication and fences stale signaling", async () => {
    invalidateSenderQualityEvidence();
    const metrics = {
      ...EMPTY_METRICS, sampleTimestampMs: 1_000, sampleWindowMs: 2_000,
      intervalFramesEncoded: 30, nativeEdgeQualityState: "healthy" as const,
      qualityLimitationReason: "none",
    };
    const evidence = () => sfuPublisherQualityEvidenceFromMetrics(metrics, 1, "publication");
    const publishers: ReturnType<typeof createFakePublisher>[] = [];
    const route = new HostSfuRoute({
      getStream: () => ({}) as MediaStream,
      getProfile: () => QUALITY_PROFILES["720p30"],
      getVideoCodec: () => "vp8",
      reconcileChildren: () => undefined,
      send: () => true,
      createPublisher: () => {
        const publisher = createFakePublisher([], "publisher");
        publishers.push(publisher);
        return publisher;
      },
    });
    await route.acceptAndWait({ revision: 1, phase: "active", assignment: hostAssignment("publication") });
    const first = sfuConfig(1, "publication");
    await route.acceptConfig(first);
    expect(evidence()?.state).toBe("unknown");
    metrics.sampleTimestampMs += 2_000;
    expect(evidence()?.state).toBe("healthy");
    const replacement = { ...first, connectionId: "replacement_connection" };
    await route.acceptConfig(replacement);
    expect(publishers).toHaveLength(2);
    expect(publishers[0]!.disconnect).toHaveBeenCalledOnce();
    expect(publishers[1]!.disconnect).not.toHaveBeenCalled();
    metrics.sampleTimestampMs += 2_000;
    expect(evidence()?.state).toBe("healthy");
    await route.acceptSignal({ ...first, type: "sfu-signal", kind: "candidate", candidate: { candidate: "old" } });
    expect(publishers[0]!.acceptSignal).not.toHaveBeenCalled();
    await route.acceptSignal({ ...replacement, type: "sfu-signal", kind: "candidate", candidate: { candidate: "current" } });
    expect(publishers[1]!.acceptSignal).toHaveBeenCalledOnce();
    await route.disconnect();
    metrics.sampleTimestampMs += 2_000;
    expect(evidence()?.state).toBe("unknown");
  });

  it("rebinds source backends through the existing SFU authority after retiring the old publisher", async () => {
    const send = vi.fn(() => true);
    let native = false;
    let stream = {} as MediaStream;
    let releaseBrowser!: () => void;
    const retiring = new Promise<void>(resolve => { releaseBrowser = resolve; });
    const browser = createFakePublisher([], "browser");
    browser.disconnect.mockReturnValueOnce(retiring);
    const media = { codec: "h264" as const, layers: [{ rid: "", width: 1280, height: 720, bitrate: 1_000_000 }], audio: false, audioBitrate: 0 };
    const control = {
      preparePublication: vi.fn(async () => ({ description: { type: "offer" as const, sdp: "native encoded source" }, media })),
      publicationMedia: vi.fn(async () => media), acceptPublicationSignal: vi.fn(async () => {}),
      closePublication: vi.fn(async () => {}), onEvent: vi.fn(() => () => {}),
    };
    const publishers: Array<NativeSfuPublisher | typeof browser> = [];
    const route = new HostSfuRoute({
      getStream: () => stream, getProfile: () => QUALITY_PROFILES["720p30"],
      getVideoCodec: () => native ? "h264" : "vp8", reconcileChildren: () => {}, send,
      createPublisher: onDisconnected => {
        const publisher = native ? new NativeSfuPublisher(control, "native-source", { iceServers: [] }, { send, onDisconnected })
          : publishers.length === 0 ? browser : createFakePublisher([], "camera");
        publishers.push(publisher);
        return publisher;
      },
    });
    await route.acceptAndWait({ revision: 1, phase: "active", assignment: hostAssignment("browser-publication") });
    await route.acceptConfig(sfuConfig(1, "browser-publication"));
    send.mockClear();
    native = true;
    stream = {} as MediaStream;
    const replacing = route.replaceStream(stream, true);
    expect(send).toHaveBeenCalledWith({ type: "route-failed", revision: 1, phase: "active", connectionId: sfuConfig(1).connectionId });
    expect(send).not.toHaveBeenCalledWith({ type: "refresh-sfu", revision: 1 });
    expect(browser.replaceStream).not.toHaveBeenCalled();
    const accepting = route.acceptAndWait({ revision: 2, phase: "active", assignment: hostAssignment("native-publication") });
    const configuring = route.acceptConfig(sfuConfig(2, "native-publication"));
    await vi.waitFor(() => expect(publishers[1]).toBeInstanceOf(NativeSfuPublisher));
    expect(control.preparePublication).not.toHaveBeenCalled();
    releaseBrowser();
    await Promise.all([replacing, accepting, configuring]);
    expect(control.preparePublication).toHaveBeenCalledExactlyOnceWith(
      "native-source", "native-publication", sfuConfig(2).connectionId, { iceServers: [] },
    );
    expect(send).toHaveBeenCalledWith(expect.objectContaining({
      type: "sfu-signal", kind: "description", description: { type: "offer", sdp: "native encoded source" }, media,
    }));

    native = false;
    stream = {} as MediaStream;
    await route.replaceStream(stream, true);
    expect(control.closePublication).toHaveBeenCalledExactlyOnceWith("native-source", "native-publication", sfuConfig(2).connectionId);
    expect(send).toHaveBeenCalledWith({ type: "route-failed", revision: 2, phase: "active", connectionId: sfuConfig(2).connectionId });
    await route.acceptAndWait({ revision: 3, phase: "active", assignment: hostAssignment("camera-publication") });
    await route.acceptConfig(sfuConfig(3, "camera-publication"));
    expect(publishers[2]).not.toBeInstanceOf(NativeSfuPublisher);
    expect(publishers[2]).not.toBe(browser);
    expect(publishers[2]!.activate).toHaveBeenCalledOnce();
    await route.disconnect();
  });

  it("reports and retires an exact active SFU source replacement failure", async () => {
    const messages: ClientMessage[] = [];
    const publishers: ReturnType<typeof createFakePublisher>[] = [];
    const route = new HostSfuRoute({
      getStream: () => ({}) as MediaStream,
      getProfile: () => QUALITY_PROFILES["720p30"],
      getVideoCodec: () => "vp8",
      reconcileChildren: () => undefined,
      send: (message) => {
        messages.push(message);
        return true;
      },
      createPublisher: () => {
        const publisher = createFakePublisher([], "publisher-active");
        publishers.push(publisher);
        return publisher;
      },
    });

    await route.acceptAndWait({
      revision: 1,
      phase: "active",
      assignment: hostAssignment("publication-active"),
    });
    await route.acceptConfig(sfuConfig(1, "publication-active"));
    publishers[0]!.replaceStream.mockResolvedValue(false);

    await expect(route.replaceStream({} as MediaStream)).resolves.toBe(false);
    expect(publishers[0]!.disconnect).toHaveBeenCalledOnce();
    expect(messages).toContainEqual({ type: "refresh-sfu", revision: 1 });
  });

  it("retires a failed pending source without invalidating healthy active SFU", async () => {
    const messages: ClientMessage[] = [];
    const publishers: ReturnType<typeof createFakePublisher>[] = [];
    const route = new HostSfuRoute({
      getStream: () => ({}) as MediaStream,
      getProfile: () => QUALITY_PROFILES["720p30"],
      getVideoCodec: () => "vp8",
      reconcileChildren: () => undefined,
      send: (message) => {
        messages.push(message);
        return true;
      },
      createPublisher: () => {
        const publisher = createFakePublisher(
          [],
          `publisher-${publishers.length + 1}`,
        );
        publishers.push(publisher);
        return publisher;
      },
    });

    await route.acceptAndWait({
      revision: 1,
      phase: "active",
      assignment: hostAssignment("publication-active"),
    });
    await route.acceptConfig(sfuConfig(1, "publication-active"));
    route.accept({
      revision: 2,
      phase: "prepare",
      assignment: hostAssignment("publication-pending"),
      candidate: candidate(2, "viewer_12345678", "sfu"),
    });
    await route.acceptConfig(sfuConfig(2, "publication-pending"));
    publishers[1]!.replaceStream.mockResolvedValue(false);

    await expect(route.replaceStream({} as MediaStream)).resolves.toBe(true);
    expect(publishers[0]!.disconnect).not.toHaveBeenCalled();
    expect(publishers[1]!.disconnect).toHaveBeenCalledOnce();
    expect(messages).toContainEqual({
      type: "route-failed",
      revision: 2,
      phase: "prepare",
      connectionId: null,
    });
  });

  it("reports success when a pending SFU promotion supersedes an old failure", async () => {
    const publishers: ReturnType<typeof createFakePublisher>[] = [];
    const route = new HostSfuRoute({
      getStream: () => ({}) as MediaStream,
      getProfile: () => QUALITY_PROFILES["720p30"],
      getVideoCodec: () => "vp8",
      reconcileChildren: () => undefined,
      send: () => true,
      createPublisher: () => {
        const publisher = createFakePublisher(
          [],
          `publisher-${publishers.length + 1}`,
        );
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
      assignment: hostAssignment("publication-new"),
      candidate: candidate(2, "viewer_12345678", "sfu"),
    });
    await route.acceptConfig(sfuConfig(2, "publication-new"));

    let resolveOldReplacement!: (replaced: boolean) => void;
    publishers[0]!.replaceStream.mockImplementation(
      () =>
        new Promise<boolean>((resolve) => {
          resolveOldReplacement = resolve;
        }),
    );
    const oldReplacementCalls = publishers[0]!.replaceStream.mock.calls.length;
    const replacing = route.replaceStream({} as MediaStream);
    await vi.waitFor(() =>
      expect(publishers[0]!.replaceStream).toHaveBeenCalledTimes(
        oldReplacementCalls + 1,
      ),
    );

    await route.acceptAndWait({
      revision: 2,
      phase: "active",
      assignment: hostAssignment("publication-new"),
    });
    resolveOldReplacement(false);

    await expect(replacing).resolves.toBe(true);
    expect(publishers[1]!.disconnect).not.toHaveBeenCalled();
  });

  it("reconciles paused Host active truth by exact SFU publication", async () => {
    const reconciledChildren: string[][] = [];
    const publishers: ReturnType<typeof createFakePublisher>[] = [];
    const route = new HostSfuRoute({
      getStream: () => ({}) as MediaStream,
      getProfile: () => QUALITY_PROFILES["720p30"],
      getVideoCodec: () => "vp8",
      reconcileChildren: (assignment) => reconciledChildren.push(assignment.childPeerIds),
      send: () => true,
      createPublisher: () => {
        const publisher = createFakePublisher(
          [],
          `publisher-${publishers.length + 1}`,
        );
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
    route.setPaused(true);
    expect(publishers[0]?.setPaused).toHaveBeenCalledWith(true);

    await route.acceptAndWait({
      revision: 2,
      phase: "active",
      assignment: hostAssignment("publication-old", ["retained-child"]),
    });
    expect(publishers[0]?.disconnect).not.toHaveBeenCalled();
    expect(reconciledChildren.at(-1)).toEqual(["retained-child"]);
    expect(
      route.accept({
        revision: 2,
        phase: "active",
        assignment: hostAssignment("publication-old", ["retained-child"]),
      }),
    ).toBe("duplicate");

    await route.acceptAndWait({
      revision: 3,
      phase: "active",
      assignment: hostAssignment(null, ["retained-child"]),
    });
    expect(publishers[0]?.deactivate).toHaveBeenCalledOnce();
    expect(publishers[0]?.disconnect).toHaveBeenCalledOnce();
    expect(reconciledChildren.at(-1)).toEqual(["retained-child"]);
  });

  it("rebuilds a failed Host SFU publisher while sharing is paused", async () => {
    const messages: ClientMessage[] = [];
    const publishers: ReturnType<typeof createFakePublisher>[] = [];
    const disconnects: Array<() => void> = [];
    const route = new HostSfuRoute({
      getStream: () => ({}) as MediaStream,
      getProfile: () => QUALITY_PROFILES["720p30"],
      getVideoCodec: () => "vp8",
      reconcileChildren: () => undefined,
      send: (message) => {
        messages.push(message);
        return true;
      },
      createPublisher: (onDisconnected) => {
        disconnects.push(onDisconnected);
        const publisher = createFakePublisher(
          [],
          `publisher-${publishers.length + 1}`,
        );
        publishers.push(publisher);
        return publisher;
      },
    });

    await route.acceptAndWait({
      revision: 1,
      phase: "active",
      assignment: hostAssignment("publication-paused"),
    });
    await route.acceptConfig(sfuConfig(1, "publication-paused"));
    route.setPaused(true);
    disconnects[0]!();
    expect(messages).toContainEqual({ type: "refresh-sfu", revision: 1 });

    await route.acceptConfig({ ...sfuConfig(1, "publication-paused"), connectionId: "recovery_connection" });
    expect(publishers).toHaveLength(2);
    expect(publishers[1]!.setPaused).toHaveBeenCalledWith(true);

    route.setPaused(false);
    expect(publishers[1]!.setPaused).toHaveBeenLastCalledWith(false);
  });

  it("does not activate a fresh SFU publisher after its exact route becomes stale", async () => {
    let releaseFreshPublisher!: () => void;
    const freshPublisherGate = new Promise<void>((resolve) => {
      releaseFreshPublisher = resolve;
    });
    const publishers: ReturnType<typeof createFakePublisher>[] = [];
    const route = new HostSfuRoute({
      getStream: () => ({}) as MediaStream,
      getProfile: () => QUALITY_PROFILES["720p30"],
      getVideoCodec: () => "vp8",
      reconcileChildren: () => undefined,
      send: () => true,
      createPublisher: () => {
        const publisher = createFakePublisher(
          [],
          `publisher-${publishers.length + 1}`,
        );
        if (publishers.length === 1) {
          publisher.activate.mockImplementation(async () => {
            await freshPublisherGate;
            return true;
          });
        }
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
      assignment: hostAssignment("publication-fresh"),
      candidate: candidate(
        2,
        "viewer_12345678",
        "sfu",
        "candidate_2_12345678",
      ),
    });
    const freshPublication = route.acceptConfig(sfuConfig(2, "publication-fresh"));
    await vi.waitFor(() =>
      expect(publishers[1]?.activate).toHaveBeenCalledOnce(),
    );

    const staleRoute = route.acceptAndWait({
      revision: 3,
      phase: "active",
      assignment: hostAssignment("publication-old"),
    });
    releaseFreshPublisher();
    await Promise.all([freshPublication, staleRoute]);

    expect(publishers[0]?.disconnect).not.toHaveBeenCalled();
    expect(publishers[1]?.disconnect).toHaveBeenCalled();
  });

  it.each(["disconnect", "resync"])("cancels pending Host preparation before joining its queue during %s", async (reason) => {
    let cancel!: (result: boolean) => void;
    const activating = new Promise<boolean>(resolve => { cancel = resolve; });
    const publisher = createFakePublisher([], "pending");
    publisher.activate.mockImplementation(() => activating);
    publisher.disconnect.mockImplementation(async () => { cancel(false); });
    const route = new HostSfuRoute({
      getStream: () => ({}) as MediaStream,
      getProfile: () => QUALITY_PROFILES["720p30"],
      getVideoCodec: () => "vp8",
      reconcileChildren: () => undefined,
      send: () => true,
      createPublisher: () => publisher,
    });
    await route.acceptAndWait({
      revision: 2, phase: "active", assignment: hostAssignment("pending"),
    });
    const preparing = route.acceptConfig(sfuConfig(2, "pending"));
    await vi.waitFor(() => expect(publisher.activate).toHaveBeenCalledOnce());
    const retiring = reason === "disconnect" ? route.disconnect() : route.resyncAuthoritative({
      revision: 1, phase: "active", assignment: hostAssignment(null),
    });
    expect(publisher.disconnect).toHaveBeenCalled();
    await Promise.all([preparing, retiring]);
    expect(publisher.setPaused).not.toHaveBeenCalled();
    await route.disconnect();
  });

  it("ignores a retired publisher failure after same-generation resync", async () => {
    let releaseOldConnect!: () => void;
    const oldConnectGate = new Promise<void>((resolve) => {
      releaseOldConnect = resolve;
    });
    const messages: ClientMessage[] = [];
    const publishers: ReturnType<typeof createFakePublisher>[] = [];
    const route = new HostSfuRoute({
      getStream: () => ({}) as MediaStream,
      getProfile: () => QUALITY_PROFILES["720p30"],
      getVideoCodec: () => "vp8",
      reconcileChildren: () => undefined,
      send: (message) => {
        messages.push(message);
        return true;
      },
      createPublisher: () => {
        const publisher = createFakePublisher(
          [],
          `publisher-${publishers.length + 1}`,
        );
        if (publishers.length === 1) {
          publisher.connect.mockImplementation(async () => {
            await oldConnectGate;
            throw new Error("retired publisher");
          });
        }
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
      assignment: hostAssignment("publication-new"),
      candidate: candidate(2, "viewer_12345678", "sfu"),
    });
    const stale = route.acceptConfig(sfuConfig(2, "publication-new"));
    await vi.waitFor(() => expect(publishers[1]?.connect).toHaveBeenCalledOnce());

    await route.resyncAuthoritative({
      revision: 1,
      phase: "active",
      assignment: hostAssignment("publication-old"),
    });
    route.accept({
      revision: 2,
      phase: "prepare",
      assignment: hostAssignment("publication-new"),
      candidate: candidate(2, "viewer_12345678", "sfu"),
    });
    const current = route.acceptConfig(sfuConfig(2, "publication-new"));
    await vi.waitFor(() => expect(publishers).toHaveLength(3));
    releaseOldConnect();
    await Promise.all([stale, current]);

    expect(messages.filter((message) => message.type === "route-failed")).toEqual(
      [],
    );
    expect(publishers[2]?.activate).toHaveBeenCalledOnce();
  });

  it("waits for pending retirement before activating a same-key replacement", async () => {
    let finishClose!: () => void;
    const closing = new Promise<void>(resolve => { finishClose = resolve; });
    const publishers: ReturnType<typeof createFakePublisher>[] = [];
    const route = new HostSfuRoute({
      getStream: () => ({}) as MediaStream,
      getProfile: () => QUALITY_PROFILES["720p30"], getVideoCodec: () => "vp8",
      reconcileChildren: () => {}, send: () => true,
      createPublisher: () => {
        const publisher = createFakePublisher([], `publisher-${publishers.length}`);
        if (!publishers.length) publisher.disconnect.mockImplementation(() => closing);
        publishers.push(publisher);
        return publisher;
      },
    });
    route.accept({ revision: 1, phase: "prepare", assignment: hostAssignment("publication"),
      candidate: candidate(1, "viewer_12345678", "sfu") });
    await route.acceptConfig(sfuConfig(1, "publication"));
    route.setPaused(true);
    route.setPaused(false);
    const replacement = route.acceptConfig(sfuConfig(1, "publication"));
    await vi.waitFor(() => expect(publishers[1]?.connect).toHaveBeenCalledOnce());
    expect(publishers[1]?.activate).not.toHaveBeenCalled();
    finishClose();
    await replacement;
    expect(publishers[1]?.activate).toHaveBeenCalledOnce();
    await route.disconnect();
  });

  it("ignores a retired subscriber failure after same-generation resync", async () => {
    const onSfuFailure = vi.fn();
    let releaseOldConnect!: () => void;
    const oldConnectGate = new Promise<void>((resolve) => {
      releaseOldConnect = resolve;
    });
    const messages: ClientMessage[] = [];
    const subscribers: ReturnType<typeof createFakeSubscriber>[] = [];
    const route = new ViewerSfuRoute("viewer_12345678", {
      onSfuFailure,
      activatePeer: () => true,
      reconcileSfuChildren: () => undefined,
      onSfuStream: () => undefined,
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
        if (subscribers.length === 1) {
          subscriber.connect.mockImplementation(async () => {
            await oldConnectGate;
            throw new Error("retired subscriber");
          });
        }
        subscribers.push(subscriber);
        return subscriber;
      },
    });

    await prepareSfu(route, 1, viewerSfuAssignment([], "publication-old"), sfuConfig(1, "publication-old"));
    route.accept({
      revision: 2,
      phase: "prepare",
      assignment: viewerSfuAssignment([], "publication-new"),
      candidate: candidate(2, "viewer_12345678", "sfu"),
    });
    const stale = route.acceptConfig(sfuConfig(2, "publication-new"));
    await vi.waitFor(() => expect(subscribers[1]?.connect).toHaveBeenCalledOnce());

    await route.resyncAuthoritative(
      {
        revision: 1,
        phase: "active",
        assignment: viewerSfuAssignment([], "publication-old"),
      },
      "viewer_12345678",
    );
    route.accept({
      revision: 2,
      phase: "prepare",
      assignment: viewerSfuAssignment([], "publication-new"),
      candidate: candidate(2, "viewer_12345678", "sfu"),
    });
    const current = route.acceptConfig(sfuConfig(2, "publication-new"));
    await vi.waitFor(() => expect(subscribers).toHaveLength(3));
    releaseOldConnect();
    await Promise.all([stale, current]);

    expect(messages.filter((message) => message.type === "route-failed")).toEqual(
      [],
    );
    expect(subscribers[2]?.activate).toHaveBeenCalledOnce();
    onSfuFailure.mockClear();
    subscribers[1]?.events.onDisconnected("VIDEO_REJECTED");
    expect(onSfuFailure).not.toHaveBeenCalled();
    subscribers[2]?.events.onDisconnected("VIDEO_REJECTED");
    expect(onSfuFailure).toHaveBeenCalledExactlyOnceWith("VIDEO_REJECTED", 2);
    await route.disconnect();
  });
});
