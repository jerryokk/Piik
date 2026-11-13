import { FakePeerConnection, createTrack, createStream, installHostPeerBrowser, restoreHostPeerBrowser, hostAssignment, hostProvisionalInput } from "./support.ts";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { HostProvisionalChild } from "../../src/client/media/host-provisional-child.ts";
import { RENEGOTIATION_ANSWER_TIMEOUT_MS } from "../../src/client/webrtc/host-peer";

beforeEach(installHostPeerBrowser);
afterEach(restoreHostPeerBrowser);
describe("Host provisional child runtime ownership", () => {
  it("reports a lost prepared audio answer to its existing route owner", async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    const failed = vi.fn();
    const sendSignal = vi.fn(() => true);
    const video = createTrack("video", "video");
    const owner = new HostProvisionalChild({ sendSignal, onPreparedChildFailed: failed });
    const input = hostProvisionalInput(7, ["child"], createStream(video, null));
    try {
      expect(owner.prepare(input)).toBe(true);
      await vi.waitFor(() => expect(sendSignal).toHaveBeenCalledOnce());
      await owner.acceptSignal("child", { kind: "description", connectionId: input.candidate.connectionId,
        description: { type: "answer", sdp: "initial-answer" } });
      await owner.replaceStream(createStream(video, createTrack("audio", "microphone")));
      await vi.advanceTimersByTimeAsync(RENEGOTIATION_ANSWER_TIMEOUT_MS);
      expect(failed).toHaveBeenCalledExactlyOnceWith(7, input.candidate.connectionId);
      expect(FakePeerConnection.latest!.connectionState).toBe("closed");
    } finally { owner.discard(); }
  });

  it("rejects failed construction, releases the clone and permits the next preparation", async () => {
    const source = createTrack("video", "construction-source");
    const sendSignal = vi.fn(() => true);
    const owner = new HostProvisionalChild({ sendSignal });
    const input = hostProvisionalInput(7, ["child"], createStream(source, null));
    FakePeerConnection.constructorsFailing = 2;
    expect(owner.prepare(input)).toBe(false);
    expect(owner.prepare(input)).toBe(false);
    expect(sendSignal).not.toHaveBeenCalled();
    for (const result of vi.mocked(source.clone).mock.results) {
      expect((result.value as MediaStreamTrack).stop).toHaveBeenCalledOnce();
    }
    expect(source.stop).not.toHaveBeenCalled();
    expect(FakePeerConnection.activeCount).toBe(0);
    expect(owner.prepare({ ...input, revision: 8 })).toBe(true);
    await vi.waitFor(() => expect(sendSignal).toHaveBeenCalledOnce());
    owner.discard();
    expect(FakePeerConnection.activeCount).toBe(0);
  });

  it("reports connected preparation once, retries unsent progress and fences replacement", async () => {
    const progress = vi.fn().mockReturnValueOnce(false).mockReturnValue(true);
    const owner = new HostProvisionalChild({ sendSignal: () => true, onPreparedChildConnected: progress });
    const stream = createStream(createTrack("video", "progress-video"), null);
    owner.prepare(hostProvisionalInput(7, ["child"], stream));
    const first = FakePeerConnection.latest!;
    first.connectionState = "connected";
    first.dispatchEvent(new Event("connectionstatechange"));
    first.dispatchEvent(new Event("connectionstatechange"));
    first.dispatchEvent(new Event("connectionstatechange"));
    expect(progress).toHaveBeenCalledTimes(2);
    expect(progress).toHaveBeenLastCalledWith(7, "candidate-connection-7");
    owner.prepare(hostProvisionalInput(8, ["replacement"], stream));
    first.dispatchEvent(new Event("connectionstatechange"));
    expect(progress).toHaveBeenCalledTimes(2);
    const second = FakePeerConnection.latest!;
    second.connectionState = "connected";
    second.dispatchEvent(new Event("connectionstatechange"));
    expect(progress).toHaveBeenLastCalledWith(8, "candidate-connection-8");
    owner.discard();
    second.dispatchEvent(new Event("connectionstatechange"));
    expect(progress).toHaveBeenCalledTimes(3);
  });

  it("accepts only the exact connection and promotes the same peer", async () => {
    const signals: Array<{ peerId: string; connectionId: string }> = [];
    const promotedUpdates = vi.fn();
    const owner = new HostProvisionalChild({
      sendSignal: (peerId, payload) => {
        signals.push({ peerId, connectionId: payload.connectionId });
        return true;
      },
      onPromotedUpdate: promotedUpdates,
    });
    const stream = createStream(createTrack("video", "host-probe-video"), null);
    const input = hostProvisionalInput(7, ["probe-child"], stream);

    expect(owner.prepare(input)).toBe(true);
    await vi.waitFor(() => expect(signals).toHaveLength(1));
    const preparedConnection = FakePeerConnection.latest!;
    const preparedConnectionId = signals[0]!.connectionId;
    expect(FakePeerConnection.activeCount).toBe(1);

    expect(owner.acceptSignal("wrong-child", {
      kind: "description",
      connectionId: preparedConnectionId,
      description: { type: "answer", sdp: "wrong-peer" },
    })).toBe(false);
    expect(owner.acceptSignal("probe-child", {
      kind: "description",
      connectionId: "wrong-connection",
      description: { type: "answer", sdp: "wrong-connection" },
    })).toBe(false);
    expect(owner.acceptSignal("probe-child", {
      kind: "candidate",
      connectionId: preparedConnectionId,
      candidate: { candidate: "host-probe-candidate" },
    })).toBe(true);
    expect(owner.acceptSignal("probe-child", {
      kind: "description",
      connectionId: preparedConnectionId,
      description: { type: "answer", sdp: "right-answer" },
    })).toBe(true);
    await vi.waitFor(() =>
      expect(preparedConnection.remoteDescription?.sdp).toBe("right-answer"),
    );
    expect(preparedConnection.addedIceCandidates).toEqual([
      { candidate: "host-probe-candidate" },
    ]);

    const activation = owner.activate(input);
    expect(activation.kind).toBe("promote");
    if (activation.kind !== "promote") {
      throw new Error("expected the prepared Host child to promote");
    }
    expect(activation.peer.connectionId).toBe(preparedConnectionId);
    expect(FakePeerConnection.latest).toBe(preparedConnection);
    expect(FakePeerConnection.activeCount).toBe(1);
    await expect(activation.peer.restartIce()).resolves.toBe(true);
    expect(signals).toEqual([
      { peerId: "probe-child", connectionId: preparedConnectionId },
      { peerId: "probe-child", connectionId: preparedConnectionId },
    ]);
    expect(promotedUpdates).toHaveBeenCalledWith(
      activation.peer,
      expect.objectContaining({ connectionId: preparedConnectionId }),
    );
    activation.peer.dispose();
  });

  it("disposes stale prepares on replacement, rollback, and auth reset", async () => {
    const onPreparedChildFailed = vi.fn();
    const owner = new HostProvisionalChild({ sendSignal: () => true, onPreparedChildFailed });
    const stream = createStream(createTrack("video", "host-cleanup-video"), null);

    expect(owner.prepare(hostProvisionalInput(7, ["first-probe"], stream))).toBe(true);
    const first = FakePeerConnection.latest!;
    expect(owner.prepare(hostProvisionalInput(8, ["second-probe"], stream))).toBe(true);
    const second = FakePeerConnection.latest!;
    expect(first.connectionState).toBe("closed");

    expect(owner.activate({
      revision: 9,
      assignment: hostAssignment([]),
      activeChildPeerIds: [],
      maxMediaEdges: 2,
    })).toEqual({ kind: "ordinary" });
    expect(second.connectionState).toBe("closed");

    expect(owner.prepare(hostProvisionalInput(10, ["auth-probe"], stream))).toBe(true);
    const authProbe = FakePeerConnection.latest!;
    owner.discard();
    expect(authProbe.connectionState).toBe("closed");
    expect(FakePeerConnection.activeCount).toBe(0);
    expect(onPreparedChildFailed).not.toHaveBeenCalled();
  });

  it("reports failed Host preparation once and fences retired attempts", async () => {
    const onPreparedChildFailed = vi.fn();
    const owner = new HostProvisionalChild({ sendSignal: () => true, onPreparedChildFailed });
    const stream = createStream(createTrack("video", "host-failure-video"), null);
    FakePeerConnection.offersFailing = 1;
    owner.prepare(hostProvisionalInput(7, ["first"], stream));
    await vi.waitFor(() => expect(onPreparedChildFailed).toHaveBeenCalledExactlyOnceWith(7, "candidate-connection-7"));

    owner.prepare(hostProvisionalInput(8, ["second"], stream));
    const connection = FakePeerConnection.latest!;
    connection.connectionState = "failed";
    connection.dispatchEvent(new Event("connectionstatechange"));
    connection.dispatchEvent(new Event("connectionstatechange"));
    expect(onPreparedChildFailed).toHaveBeenCalledTimes(2);
    expect(onPreparedChildFailed).toHaveBeenLastCalledWith(8, "candidate-connection-8");
    owner.discard();
    connection.dispatchEvent(new Event("connectionstatechange"));
    expect(onPreparedChildFailed).toHaveBeenCalledTimes(2);
  });

  it("updates the prepared stream before promotion", async () => {
    const owner = new HostProvisionalChild({ sendSignal: () => true });
    const initial = createStream(createTrack("video", "initial-video"), null);
    const replacement = createTrack("video", "replacement-video");
    const input = hostProvisionalInput(12, ["stream-child"], initial);

    expect(owner.prepare(input)).toBe(true);
    const connection = FakePeerConnection.latest!;
    await expect(
      owner.replaceStream(createStream(replacement, null)),
    ).resolves.toBe(true);
    expect(connection.senders[0]!.track).not.toBe(replacement);
    expect(connection.senders[0]!.track?.id).toBe(replacement.id);

    const activation = owner.activate({
      revision: input.revision,
      assignment: input.assignment,
      activeChildPeerIds: [],
      maxMediaEdges: 2,
    });
    expect(activation.kind).toBe("promote");
    if (activation.kind === "promote") {
      activation.peer.dispose();
    }
  });
});
