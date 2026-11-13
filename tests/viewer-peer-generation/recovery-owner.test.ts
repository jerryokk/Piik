import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { type SignalPayload } from "../../src/shared/protocol.ts";
import { type PeerSnapshot } from "../../src/client/types.ts";
import { ViewerPeer } from "../../src/client/webrtc/viewer-peer.ts";
import { createDeferred, FakePeerConnection, timeoutCallbacks, timeoutDelays, offer, candidate, decodedReport, createPeer, flushAsyncWork, installFakeBrowser, restoreBrowserGlobals } from "./support.ts";

beforeEach(installFakeBrowser);
afterEach(restoreBrowserGlobals);
describe("ViewerPeer connection generations", () => {
  it("returns rejected video to the existing route owner with its receiving cause", async () => {
    const restart = vi.fn(() => true), exhausted = vi.fn(() => true), sendSignal = vi.fn(() => true);
    const updates: PeerSnapshot[] = [];
    const peer = new ViewerPeer({ iceServers: [] }, { sendSignal, sendRestartRequest: restart,
      onStream: () => undefined, onUpdate: value => updates.push(value), onRecoveryExhausted: exhausted },
      { recoveryOwner: "route" });
    FakePeerConnection.plans.push({ answerSdp: "m=video 0 UDP/TLS/RTP/SAVPF 120\r\n" });
    await peer.acceptSignal("host", { kind: "description", connectionId: "rejected",
      description: { type: "offer", sdp: "m=video 9 UDP/TLS/RTP/SAVPF 96\r\na=sendonly\r\n" } });
    expect(updates.at(-1)?.error).toEqual({ key: "viewer.msg.videoRejected" });
    expect(exhausted).toHaveBeenCalledExactlyOnceWith("host", "rejected");
    expect(sendSignal).not.toHaveBeenCalled();
    expect(restart).not.toHaveBeenCalled();
    expect(timeoutCallbacks.size).toBe(0);
    peer.dispose();
  });

  it("answers after a rejected queued candidate and still applies later candidates", async () => {
    const signals: SignalPayload[] = [];
    const snapshots: PeerSnapshot[] = [];
    const peer = createPeer(signals, snapshots);
    await peer.acceptSignal("host", candidate("queued", "stale-candidate"));
    const current = candidate("queued", "current-candidate");
    await peer.acceptSignal("host", current);
    const answering = peer.acceptSignal("host", offer("queued"));
    const connection = FakePeerConnection.instances[0]!;
    connection.addIceCandidate.mockRejectedValueOnce(new DOMException("Unknown ICE username fragment", "OperationError"));
    await answering;
    expect(connection.addIceCandidate).toHaveBeenCalledTimes(2);
    expect(signals).toHaveLength(1);
    expect(signals[0]).toMatchObject({ kind: "description", description: { type: "answer" } });
    expect(snapshots.at(-1)?.error).toBeNull();
    peer.dispose();
  });

  it("keeps separately delivered audio and video on one remote stream", async () => {
    const streams: MediaStream[] = [];
    const peer = new ViewerPeer(
      { iceServers: [] },
      {
        sendSignal: () => true,
        sendRestartRequest: () => true,
        onStream: (stream) => streams.push(stream),
        onUpdate: () => undefined,
      },
    );
    await peer.acceptSignal("host", offer("separate-tracks"));
    const connection = FakePeerConnection.instances[0]!;
    const video = { id: "remote-video", kind: "video" } as MediaStreamTrack;
    const audio = { id: "remote-audio", kind: "audio" } as MediaStreamTrack;
    connection.emitTrack(video, [{} as MediaStream]);
    connection.emitTrack(audio, [{} as MediaStream]);

    expect(streams).toHaveLength(2);
    expect(streams[0]).toBe(streams[1]);
    expect(streams[1]!.getVideoTracks()).toEqual([video]);
    expect(streams[1]!.getAudioTracks()).toEqual([audio]);
    peer.dispose();
  });

  it("proves a fresh exact connection from its first cumulative decoded frame", async () => {
    FakePeerConnection.plans.push({
      statsGate: Promise.resolve(decodedReport(1)),
    });
    const decoded = vi.fn(() => true);
    const peer = new ViewerPeer(
      { iceServers: [] },
      {
        sendSignal: () => true,
        sendRestartRequest: () => true,
        onStream: () => undefined,
        onUpdate: () => undefined,
        onFirstDecodedFrame: decoded,
      },
    );

    const accepting = peer.acceptSignal("host", offer("fresh-exact"));
    FakePeerConnection.instances[0]!.connectionState = "connected";
    await accepting;
    await flushAsyncWork();

    expect(decoded).toHaveBeenCalledOnce();
    expect(decoded).toHaveBeenCalledWith("fresh-exact");
    peer.dispose();
  });

  it("defers an in-flight decoded proof until the same prepared transport reconnects", async () => {
    vi.useFakeTimers();
    const stats = createDeferred<RTCStatsReport>();
    FakePeerConnection.plans.push({ statsGate: stats.promise });
    const decoded = vi.fn(() => true), restart = vi.fn(() => true);
    const peer = new ViewerPeer({ iceServers: [] }, {
      sendSignal: () => true, sendRestartRequest: restart,
      onStream: () => undefined, onUpdate: () => undefined, onFirstDecodedFrame: decoded,
    }, { recoveryOwner: "route" });
    try {
      await peer.acceptSignal("host", offer("prepared"));
      const connection = FakePeerConnection.instances[0]!;
      connection.connectionState = "connected";
      connection.dispatchEvent(new Event("connectionstatechange"));
      connection.connectionState = "disconnected";
      connection.dispatchEvent(new Event("connectionstatechange"));
      stats.resolve(decodedReport(1));
      await vi.advanceTimersByTimeAsync(100);
      expect(decoded).not.toHaveBeenCalled();
      connection.connectionState = "connected";
      connection.dispatchEvent(new Event("connectionstatechange"));
      await vi.advanceTimersByTimeAsync(100);
      expect(decoded).toHaveBeenCalledExactlyOnceWith("prepared");
      expect(FakePeerConnection.instances).toHaveLength(1);
      expect(restart).not.toHaveBeenCalled();
      expect(timeoutCallbacks.size).toBe(0);
    } finally { peer.dispose(); vi.useRealTimers(); }
  });

  it("does not publish an in-flight decoded proof from a replaced connection", async () => {
    const oldStats = createDeferred<RTCStatsReport>();
    FakePeerConnection.plans.push(
      { statsGate: oldStats.promise },
      { statsGate: Promise.resolve(new Map() as unknown as RTCStatsReport) },
    );
    const decoded = vi.fn(() => true);
    const peer = new ViewerPeer(
      { iceServers: [] },
      {
        sendSignal: () => true,
        sendRestartRequest: () => true,
        onStream: () => undefined,
        onUpdate: () => undefined,
        onFirstDecodedFrame: decoded,
      },
    );

    await peer.acceptSignal("host-a", offer("old-exact"));
    await peer.acceptSignal("host-b", offer("new-exact"));
    oldStats.resolve(decodedReport(3));
    await flushAsyncWork();

    expect(decoded).not.toHaveBeenCalled();
    peer.dispose();
  });

  it("sets and signals stereo audio bitrate for every answer", async () => {
    const signals: SignalPayload[] = [];
    const peer = createPeer(signals, []);

    await peer.acceptSignal("host", offer("screen-audio"));
    await peer.acceptSignal("host", offer("screen-audio"));
    await peer.acceptSignal("host", offer("screen-audio-rebuild"));

    const answers = signals.filter((signal) => signal.kind === "description");
    const localDescriptions = FakePeerConnection.instances.flatMap((connection) =>
      connection.setLocalDescription.mock.calls.map(([description]) => description),
    );
    expect(answers).toHaveLength(3);
    for (const [index, description] of localDescriptions.entries()) {
      expect(description).toEqual(expect.objectContaining({
        type: "answer",
        sdp: expect.stringContaining(
          "a=fmtp:111 minptime=10;useinbandfec=1;stereo=1;maxaveragebitrate=510000",
        ),
      }));
      expect(answers[index]?.description.sdp).toBe(description.sdp);
    }
  });

  it("applies STUN-only ICE configuration at creation and update", async () => {
    const peer = createPeer([], [], [], {
      iceServers: [{ urls: ["stun:stun-a.example.test:3478"] }],
    });
    await peer.acceptSignal("host", offer("stun-config"));
    const connection = FakePeerConnection.instances[0]!;

    peer.updateIceConfig({
      iceServers: [{ urls: ["stun:stun-b.example.test:3478"] }],
    });

    expect(connection.configurations).toEqual([
      { iceServers: [{ urls: ["stun:stun-a.example.test:3478"] }] },
      { iceServers: [{ urls: ["stun:stun-b.example.test:3478"] }] },
    ]);
    expect(connection.setConfiguration).toHaveBeenCalledOnce();
    for (const configuration of connection.configurations) {
      expect(
        configuration.iceServers?.every(
          (server) =>
            !Reflect.has(server, "username") &&
            !Reflect.has(server, "credential") &&
            (typeof server.urls === "string"
              ? server.urls.startsWith("stun:")
              : server.urls.every((url) => url.startsWith("stun:"))),
        ),
      ).toBe(true);
    }
  });

  it("uses the room NAT policy for Viewer-originated ICE candidates", async () => {
    const signals: SignalPayload[] = [];
    const peer = createPeer(
      signals,
      [],
      [],
      {
        iceServers: [{ urls: "stun:share.example.test:3478" }],
        natPredictionStunUrls: [
          "stun:share.example.test:3479",
          "stun:share.example.test:3480",
        ],
      },
      true,
    );
    await peer.acceptSignal("host", offer("nat-room-policy"));
    const connection = FakePeerConnection.instances[0]!;
    expect(connection.configurations[0]?.iceServers).toEqual([
      { urls: "stun:share.example.test:3478" },
      { urls: "stun:share.example.test:3479" },
      { urls: "stun:share.example.test:3480" },
    ]);

    const surveyUrls = [
      "stun:share.example.test:3478",
      "stun:share.example.test:3479",
      "stun:share.example.test:3480",
    ];
    const emitCandidate = (port: number, url: string): void => {
      const event = new Event("icecandidate");
      Object.defineProperty(event, "candidate", {
        value: {
          candidate:
            `candidate:base 1 udp 2122260223 203.0.113.7 ${port} ` +
            "typ srflx raddr 192.0.2.7 rport 50000 generation 0 ufrag test",
          sdpMid: "0",
          sdpMLineIndex: 0,
          usernameFragment: "test",
          url,
        },
      });
      connection.dispatchEvent(event);
    };
    emitCandidate(40_000, surveyUrls[0]!);
    emitCandidate(40_003, surveyUrls[1]!);
    emitCandidate(40_006, surveyUrls[2]!);
    connection.iceGatheringState = "complete";
    connection.dispatchEvent(new Event("icegatheringstatechange"));

    const candidates = signals.filter(
      (signal): signal is Extract<SignalPayload, { kind: "candidate" }> =>
        signal.kind === "candidate",
    );
    expect(
      candidates.some((signal) =>
        signal.candidate?.candidate.startsWith("candidate:sp"),
      ),
    ).toBe(true);
    expect(
      candidates.filter((signal) =>
        signal.candidate?.candidate.startsWith("candidate:base"),
      ),
    ).toHaveLength(3);
    expect(candidates.filter((signal) => signal.candidate?.candidate === "")).toHaveLength(1);
    expect(candidates.at(-1)?.candidate).toMatchObject({ usernameFragment: "test" });
    peer.dispose();
  });

  it("restarts ICE when an answered initial connection stays stuck", async () => {
    const restartRequests: Array<{
      peerId: string;
      connectionId: string;
      rebuild: boolean;
    }> = [];
    const peer = new ViewerPeer(
      { iceServers: [] },
      {
        sendSignal: () => true,
        sendRestartRequest: (peerId, connectionId, rebuild) => {
          restartRequests.push({ peerId, connectionId, rebuild });
          return true;
        },
        onStream: () => undefined,
        onUpdate: () => undefined,
      },
    );

    await peer.acceptSignal("relay-parent", offer("stuck-connection"));

    expect(timeoutDelays.get(1)).toBe(15_000);
    timeoutCallbacks.get(1)!();
    expect(restartRequests).toEqual([
      {
        peerId: "relay-parent",
        connectionId: "stuck-connection",
        rebuild: false,
      },
    ]);
    expect(timeoutDelays.get(2)).toBe(3_000);
  });

  it("returns NAT-enabled candidate failure to its route owner without local retries", async () => {
    const restartRequests: string[] = [];
    const exhausted: string[] = [];
    const peer = new ViewerPeer(
      { iceServers: [] },
      {
        sendSignal: () => true,
        sendRestartRequest: (_peerId, connectionId) => {
          restartRequests.push(connectionId);
          return true;
        },
        onStream: () => undefined,
        onUpdate: () => undefined,
        onRecoveryExhausted: (_peerId, connectionId) => {
          exhausted.push(connectionId);
          return true;
        },
      },
      { recoveryOwner: "route", natPredictionEnabled: true },
    );

    await peer.acceptSignal("host", offer("route-candidate"));
    const connection = FakePeerConnection.instances[0]!;
    connection.connectionState = "disconnected";
    connection.dispatchEvent(new Event("connectionstatechange"));

    expect(timeoutDelays).toEqual(new Map());
    expect(restartRequests).toEqual([]);
    expect(exhausted).toEqual([]);

    connection.connectionState = "failed";
    connection.dispatchEvent(new Event("connectionstatechange"));
    expect(restartRequests).toEqual([]);
    expect(timeoutDelays).toEqual(new Map());
    expect(exhausted).toEqual(["route-candidate"]);
  });

  it("transfers recovery ownership when a prepared route fails during commit", async () => {
    const restartRequests: Array<{
      connectionId: string;
      rebuild: boolean;
    }> = [];
    const peer = new ViewerPeer(
      { iceServers: [] },
      {
        sendSignal: () => true,
        sendRestartRequest: (_peerId, connectionId, rebuild) => {
          restartRequests.push({ connectionId, rebuild });
          return true;
        },
        onStream: () => undefined,
        onUpdate: () => undefined,
      },
      { recoveryOwner: "route" },
    );

    await peer.acceptSignal("host", offer("committed-candidate"));
    const connection = FakePeerConnection.instances[0]!;
    connection.connectionState = "failed";
    peer.activatePreparedRoute();

    expect(restartRequests).toEqual([
      { connectionId: "committed-candidate", rebuild: false },
    ]);
    expect([...timeoutDelays.values()]).toEqual([3_000]);
  });
});
