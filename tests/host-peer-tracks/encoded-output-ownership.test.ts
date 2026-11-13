import { FakePeerConnection, createTrack, createStream, acceptPeerAnswer, installHostPeerBrowser, restoreHostPeerBrowser, encodedPeerFixture, hostProvisionalInput } from "./support.ts";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { QUALITY_PROFILES } from "../../src/client/media/quality.ts";
import { HostProvisionalChild } from "../../src/client/media/host-provisional-child.ts";
import { HostPeer } from "../../src/client/webrtc/host-peer";
import { VP8_ONLY_VIDEO_CODEC } from "../../src/client/webrtc/video-codec.ts";

beforeEach(installHostPeerBrowser);
afterEach(restoreHostPeerBrowser);
describe("HostPeer encoded output ownership", () => {
  it.each(["initial", "replacement"])("owns output pause when the pool declines the %s source", async (phase) => {
    const fixture = encodedPeerFixture();
    const source = createTrack("video", "source");
    const peer = new HostPeer("child", { iceServers: [] }, createStream(source, null),
      QUALITY_PROFILES["720p30"], { sendSignal: () => true, onUpdate() {} },
      VP8_ONLY_VIDEO_CODEC, undefined, false, fixture.pool);
    try {
      if (phase === "initial") {
        peer.setPaused(true);
        fixture.create.mockReturnValueOnce(null);
      }
      await peer.start();
      if (phase === "replacement") {
        peer.setPaused(true);
        fixture.create.mockReturnValueOnce(null);
        expect(await peer.replaceStream(createStream(createTrack("video", "declined-source"), null))).toBe(true);
      }
      const sender = FakePeerConnection.latest!.senders[0]!;
      expect(sender.track!.enabled).toBe(false);
      expect(fixture.output.passthrough).toHaveBeenCalled();
      expect.soft(fixture.output.setPaused).toHaveBeenLastCalledWith(true);

      peer.setPaused(false);
      expect(sender.track!.enabled).toBe(true);
      expect(fixture.output.setPaused).toHaveBeenLastCalledWith(false);
      expect(source.stop).not.toHaveBeenCalled();
    } finally {
      peer.dispose();
    }
  });

  it("keeps capture constraints on real tracks across pool attachment, profile changes and fallback", async () => {
    const fixture = encodedPeerFixture();
    const source = createTrack("video", "source");
    const peer = new HostPeer("child", { iceServers: [] }, createStream(source, null),
      QUALITY_PROFILES["720p30"], { sendSignal: () => true, onUpdate() {} },
      VP8_ONLY_VIDEO_CODEC, undefined, false, fixture.pool);
    await peer.start();
    await acceptPeerAnswer(peer);
    const sender = FakePeerConnection.latest!.senders[0]!;
    await peer.updateCaptureProfile(QUALITY_PROFILES["720p30"]);
    expect(sender.track!.applyConstraints).toHaveBeenCalled();
    fixture.binding.carrierScale.mockReturnValue(1);
    expect(await fixture.create.mock.calls[0]![5]()).toBe(true);
    expect(sender.track).toBe(fixture.output.track);
    for (const id of ["1080p60", "720p30"] as const) {
      expect(await peer.updateCaptureProfile(QUALITY_PROFILES[id])).toBe(true);
      expect(sender.getParameters().encodings[0]).not.toHaveProperty("maxFramerate");
    }
    expect(fixture.output.track.applyConstraints).not.toHaveBeenCalled();
    fixture.binding.carrierScale.mockReturnValue(undefined);
    expect(await fixture.create.mock.calls[0]![5]()).toBe(true);
    expect(sender.track).not.toBe(source);
    expect(sender.track).not.toBe(fixture.output.track);
    expect(sender.track!.applyConstraints).toHaveBeenCalledWith(expect.objectContaining({ frameRate: { ideal: 30, max: 30 } }));
    expect(sender.getParameters().encodings[0]!.maxFramerate).toBe(30);
    peer.dispose();
    expect(source.stop).not.toHaveBeenCalled();
  });

  it.each(["video", "audio", "fallback", "rollback"])("reports %s failure once and keeps borrowed source tracks alive", async (kind) => {
    const fixture = encodedPeerFixture();
    const source = createTrack("video", "source"), audio = createTrack("audio", "audio");
    const onUpdate = vi.fn();
    const peer = new HostPeer("child", { iceServers: [] }, createStream(source, audio), QUALITY_PROFILES["720p30"],
      { sendSignal: () => true, onUpdate }, VP8_ONLY_VIDEO_CODEC, "exact-connection", false, fixture.pool);
    await peer.start();
    const connection = FakePeerConnection.latest!, owned = connection.senders[0]!.track!;
    onUpdate.mockClear();
    if (kind === "video") fixture.failVideo();
    else if (kind === "audio") fixture.failAudio();
    else if (kind === "fallback") fixture.create.mock.calls[0]![7]();
    else {
      fixture.binding.carrierScale.mockReturnValue(1);
      connection.senders[0]!.failNextSetParameters = true;
      connection.senders[0]!.replaceTrack.mockResolvedValueOnce(undefined).mockRejectedValueOnce(new Error("rollback failed"));
      // Reflect that the first attachment succeeded before configuration failed.
      connection.senders[0]!.track = fixture.output.track;
      await fixture.create.mock.calls[0]![5]();
    }
    await vi.waitFor(() => expect(onUpdate).toHaveBeenCalledOnce());
    expect(onUpdate.mock.calls[0]![0]).toMatchObject({ peerId: "child", connectionId: "exact-connection", connectionState: "failed" });
    expect(peer.getSnapshot().connectionState).toBe("failed");
    expect(connection.connectionState).toBe("closed");
    expect(owned.stop).toHaveBeenCalledOnce();
    expect(source.stop).not.toHaveBeenCalled();
    expect(audio.stop).not.toHaveBeenCalled();
    fixture.failVideo();
    fixture.create.mock.calls[0]![7]();
    expect(onUpdate).toHaveBeenCalledOnce();
  });

  it("keeps expected disposal and late failures silent", async () => {
    const fixture = encodedPeerFixture(), onUpdate = vi.fn();
    const peer = new HostPeer("child", { iceServers: [] }, createStream(createTrack("video", "source"), null),
      QUALITY_PROFILES["720p30"], { sendSignal: () => true, onUpdate }, VP8_ONLY_VIDEO_CODEC, undefined, false, fixture.pool);
    await peer.start();
    onUpdate.mockClear();
    peer.dispose();
    fixture.failVideo();
    fixture.failAudio();
    await new Promise<void>((resolve) => setImmediate(resolve));
    expect(onUpdate).not.toHaveBeenCalled();
  });

  it("reports the exact prepared child immediately instead of waiting for the route deadline", async () => {
    const fixture = encodedPeerFixture(), onPreparedChildFailed = vi.fn();
    const owner = new HostProvisionalChild({ sendSignal: () => true, onPreparedChildFailed });
    owner.prepare({ ...hostProvisionalInput(7, ["candidate-child"], createStream(createTrack("video", "source"), null)), videoPool: fixture.pool });
    await vi.waitFor(() => expect(FakePeerConnection.latest!.localDescription).not.toBeNull());
    fixture.failVideo();
    expect(onPreparedChildFailed).toHaveBeenCalledExactlyOnceWith(7, "candidate-connection-7");
    owner.discard();
  });
});
