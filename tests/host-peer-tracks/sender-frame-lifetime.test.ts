import { FakePeerConnection, createTrack, createStream, createPeer, installHostPeerBrowser, restoreHostPeerBrowser } from "./support.ts";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { QUALITY_PROFILES } from "../../src/client/media/quality.ts";
import { senderQualityEvidenceFromSnapshot } from "../../src/client/media/sender-quality-evidence.ts";
import { HostPeer } from "../../src/client/webrtc/host-peer";
import * as senderTracks from "../../src/client/media/sender-video-track";

beforeEach(installHostPeerBrowser);
afterEach(restoreHostPeerBrowser);
describe("HostPeer sender-frame lifetime", () => {
  it("retires sender evidence with its connection", () => {
    const peer = createPeer(createStream(createTrack("video", "source"), null));
    const snapshot = peer.getSnapshot();
    expect(senderQualityEvidenceFromSnapshot(snapshot, 1)?.state).toBe("unknown");
    expect(senderQualityEvidenceFromSnapshot(snapshot, 1)).toBeNull();
    peer.dispose();
    expect(senderQualityEvidenceFromSnapshot(snapshot, 1)?.state).toBe("unknown");
  });

  // Channel setup can fail before the peer reaches its disposal owner.
  it.each(["allocation", "binding"] as const)(
    "closes the allocated connection when input channel %s fails",
    (failure) => {
      const video = createTrack("video", "input-setup");
      const error = new DOMException("Input setup failed", "OperationError");
      class InputConnection extends FakePeerConnection {
        createDataChannel(): RTCDataChannel {
          if (failure === "allocation") throw error;
          return {} as RTCDataChannel;
        }
      }
      vi.stubGlobal("RTCPeerConnection", InputConnection);
      expect(() => new HostPeer(
        "viewer-peer",
        { iceServers: [] },
        createStream(video, null),
        QUALITY_PROFILES["720p30"],
        {
          sendSignal: () => true,
          onUpdate: () => {},
          onInputChannel: () => { throw error; },
        },
      )).toThrow(error);
      expect(FakePeerConnection.latest?.connectionState).toBe("closed");
      expect(FakePeerConnection.activeCount).toBe(0);
      const clone = vi.mocked(video.clone).mock.results[0]!.value;
      expect(clone.stop).toHaveBeenCalledOnce();
      expect(video.stop).not.toHaveBeenCalled();
    },
  );

  it.each(["current", "replaced", "pending"])("handles a %s frame input ending in its owning operation", async (phase) => {
    const endings: Array<{ track: MediaStreamTrack; end: () => void }> = [];
    vi.spyOn(senderTracks, "cloneSenderVideoTrack").mockImplementation((source, onEnded) => {
      const track = source.clone();
      endings.push({ track, end() {
        Object.defineProperty(track, "readyState", { value: "ended" });
        onEnded(track);
      } });
      return track;
    });
    const source = createTrack("video", "source"), next = createTrack("video", "next");
    const peer = createPeer(createStream(source, null));
    try {
      await peer.start();
      const pc = FakePeerConnection.latest!, original = pc.senders[0]!.track;
      if (phase === "current") endings[0]!.end();
      else if (phase === "replaced") {
        expect(await peer.replaceStream(createStream(next, null))).toBe(true);
        endings[0]!.end();
        expect(pc.senders[0]!.track).toBe(endings[1]!.track);
      } else {
        pc.senders[1]!.deferReplaceCall = 1;
        const replacing = peer.replaceStream(createStream(next, null));
        await vi.waitFor(() => expect(pc.senders[1]!.replaceTrack).toHaveBeenCalled());
        endings[1]!.end();
        pc.senders[1]!.releaseDeferredReplaceTrack();
        expect(await replacing).toBe(false);
        expect(pc.senders[0]!.track).toBe(original);
        expect(endings[1]!.track.stop).toHaveBeenCalled();
      }
      expect(pc.connectionState === "closed").toBe(phase === "current");
      expect(source.stop).not.toHaveBeenCalled();
      expect(next.stop).not.toHaveBeenCalled();
    } finally { peer.dispose(); }
  });
});
