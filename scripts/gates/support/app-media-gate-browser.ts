import { fittedCaptureSize, matchesCaptureSize } from "./media-gate-geometry";
import type { AppEndpoint as Endpoint } from "./app-gate-endpoint";
import type { MediaEvidence } from "./app-media-gate-types";

/**
 * Browser-side probe: drives the App control socket from a page and measures
 * the media that reaches two WebRTC receivers. It is serialized into the
 * Browser through Function.prototype.toString, so it must stay self-contained
 * apart from the geometry helpers passed as its second argument.
 */
export async function browserMediaGate(input: {
  endpoint: Endpoint;
  protocol: number;
  subprotocol: string;
  sourceTitle: string;
  sourceKind: "window" | "display";
  stunUrls: string;
}, geometry: { fit: typeof fittedCaptureSize; matches: typeof matchesCaptureSize }): Promise<MediaEvidence> {
  let socket: WebSocket | null = null;
  const peers: RTCPeerConnection[] = [];
  const result: MediaEvidence = {
    captureActive: false,
    connected: false,
    connectedEdges: 0,
    frames: 0,
    edgeFrames: [],
    audioPackets: 0,
    edgeAudioPackets: [],
    audioEnergy: 0,
    edgeAudioEnergy: [],
    audioSamples: 0,
    edgeAudioSamples: [],
    audioBytes: 0,
    edgeAudioBytes: [],
    audioAvailable: false,
    width: 0,
    height: 0,
    localType: null,
    remoteType: null,
    nativeCandidateTypes: [],
    liveProfileUpdated: false,
    pausedProfileUpdated: false,
    error: null,
  };
  try {
    const healthResponse = await fetch(input.endpoint.url + "/health", {
      cache: "no-store",
      targetAddressSpace: "loopback",
    } as RequestInit);
    const health = await healthResponse.json();
    if (
      health.protocol !== input.protocol ||
      health.service !== "piik-client" ||
      health.instanceToken !== input.endpoint.instanceToken ||
      health.nativeMedia?.video !== true ||
      health.nativeMedia?.hardwareH264 !== true
    ) {
      throw new Error("Native App health is not ready");
    }
    result.audioAvailable = input.sourceKind === "window"
      ? health.nativeMedia.processAudio === true
      : health.nativeMedia.systemAudio === true;

    socket = new WebSocket(
      "ws://127.0.0.1:" + input.endpoint.port + "/control",
      [input.subprotocol + "." + input.endpoint.instanceToken],
    );
    await new Promise<void>((resolveOpen, rejectOpen) => {
      const timer = window.setTimeout(
        () => rejectOpen(new Error("Native control open timed out")),
        5_000,
      );
      socket!.onopen = () => {
        window.clearTimeout(timer);
        resolveOpen();
      };
      socket!.onerror = () => {
        window.clearTimeout(timer);
        rejectOpen(new Error("Native control failed to open"));
      };
    });

    let requestSequence = 0;
    let eventError: Error | null = null;
    const edges = new Map<string, {
      peer: RTCPeerConnection;
      video: HTMLVideoElement;
      stream: MediaStream;
      connected: boolean;
      remoteDescriptionSet: boolean;
      queuedCandidates: Array<RTCIceCandidateInit | null>;
    }>();
    const pending = new Map<string, {
      resolve: (value: any) => void;
      reject: (error: Error) => void;
      timer: number;
    }>();
    const request = (type: string, fields: Record<string, unknown> = {}) => {
      requestSequence += 1;
      const id = "request_" + String(requestSequence).padStart(8, "0");
      return new Promise<any>((resolveRequest, rejectRequest) => {
        const timer = window.setTimeout(() => {
          pending.delete(id);
          rejectRequest(new Error("Native request timed out: " + type));
        }, 8_000);
        pending.set(id, { resolve: resolveRequest, reject: rejectRequest, timer });
        socket!.send(JSON.stringify({ version: input.protocol, id, type, ...fields }));
      });
    };
    socket.onmessage = (event) => {
      const message = JSON.parse(String(event.data));
      if (typeof message.id === "string") {
        const active = pending.get(message.id);
        if (active) {
          pending.delete(message.id);
          window.clearTimeout(active.timer);
          active.resolve(message);
        }
        return;
      }
      if (message.type === "capture-state" && message.state === "active") {
        result.captureActive = true;
      } else if (message.type === "edge-state") {
        const edge = edges.get(message.connectionId);
        if (edge) edge.connected = message.state === "connected";
      } else if (message.type === "edge-path") {
        result.localType = message.localType;
        result.remoteType = message.remoteType;
      } else if (message.type === "edge-candidate") {
        const candidate = message.candidate as RTCIceCandidateInit | null;
        const fields = typeof candidate?.candidate === "string"
          ? candidate.candidate.split(/\s+/)
          : [];
        const typeIndex = fields.indexOf("typ");
        if (typeIndex >= 0 && fields[typeIndex + 1] &&
            !result.nativeCandidateTypes.includes(fields[typeIndex + 1]!)) {
          result.nativeCandidateTypes.push(fields[typeIndex + 1]!);
        }
        const edge = edges.get(message.connectionId);
        if (edge?.remoteDescriptionSet) {
          void edge.peer.addIceCandidate(candidate).catch((error) => {
            eventError = error instanceof Error ? error : new Error(String(error));
          });
        } else if (edge) {
          edge.queuedCandidates.push(candidate);
        }
      } else if (message.type === "share-ended" && message.failed) {
        eventError = new Error("Native share ended unexpectedly");
      }
    };
    socket.onclose = () => {
      for (const active of pending.values()) {
        window.clearTimeout(active.timer);
        active.reject(new Error("Native control closed"));
      }
      pending.clear();
    };

    const ready = await request("hello");
    if (ready.type !== "ready") throw new Error("Native hello was rejected");
    const options = await request("capture-options");
    const adapter = options.adapters?.find(
      (candidate: { hardwareH264?: unknown[] }) => candidate.hardwareH264?.length,
    );
    const encoder = adapter?.hardwareH264?.[0];
    const sources = await request("list-sources");
    const target = sources.sources?.find(
      (candidate: { kind?: string; title?: string }) =>
        candidate.kind === input.sourceKind &&
        (input.sourceKind === "display" ||
          candidate.title?.includes(input.sourceTitle)),
    );
    if (!adapter || !encoder || !target) {
      throw new Error("Native capture selection is unavailable");
    }

    const shareId = "share_gate_0001";
    const connectionId = "edge_gate_0001";
    const started = await request("start-share", {
      shareId,
      codec: "h264",
      source: target,
      audio: result.audioAvailable,
      adapterIndex: adapter.index,
      encoderIndex: encoder.index,
      edgeCapacity: 2,
      profile: {
        resolution: { width: 2560, height: 1440 },
        maxFramerate: 30,
        maxBitrate: 3_000_000,
        degradationPreference: "maintain-resolution",
        maxAudioBitrate: 128000,
      },
    });
    if (started.shareId !== shareId || started.audio !== result.audioAvailable) {
      throw new Error("Native share audio capability was not reported consistently");
    }

    const connectEdge = async (nextConnectionId: string) => {
      const peer = new RTCPeerConnection();
      peers.push(peer);
      if (result.audioAvailable) {
        peer.addTransceiver("audio", { direction: "recvonly" });
      }
      const video = document.createElement("video");
      video.autoplay = true;
      video.volume = 0.001;
      video.playsInline = true;
      document.body.append(video);
      const stream = new MediaStream();
      video.srcObject = stream;
      const edge = {
        peer,
        video,
        stream,
        connected: false,
        remoteDescriptionSet: false,
        queuedCandidates: [] as Array<RTCIceCandidateInit | null>,
      };
      edges.set(nextConnectionId, edge);
      peer.ontrack = (event) => {
        if (!stream.getTrackById(event.track.id)) {
          stream.addTrack(event.track);
        }
        void video.play();
      };
      peer.onicecandidate = (event) => {
        void request("edge-candidate", {
          shareId,
          connectionId: nextConnectionId,
          candidate: event.candidate?.toJSON() ?? null,
        }).catch((error) => {
          eventError = error instanceof Error ? error : new Error(String(error));
        });
      };
      const offer = await request("prepare-edge", {
        shareId,
        connectionId: nextConnectionId,
        iceServers: [{ urls: input.stunUrls.split(",") }],
      });
      await peer.setRemoteDescription({ type: "offer", sdp: offer.sdp });
      edge.remoteDescriptionSet = true;
      for (const candidate of edge.queuedCandidates.splice(0)) {
        await peer.addIceCandidate(candidate);
      }
      const answer = await peer.createAnswer();
      // This fixture is an unchanged-Opus receiver, like the product Viewer.
      // Declare its receive capacity; the offer's fmtp is the opposite direction.
      const opus = answer.sdp?.match(/^a=rtpmap:(\d+) opus\/48000\/2\r?$/m)?.[1];
      if (opus) answer.sdp = answer.sdp!.replace(new RegExp(`^(a=fmtp:${opus} [^\\r\\n]*)`, "m"),
        "$1;stereo=1;maxaveragebitrate=510000");
      await peer.setLocalDescription(answer);
      await request("edge-answer", {
        shareId,
        connectionId: nextConnectionId,
        sdp: answer.sdp,
      });
    };
    await Promise.all([
      connectEdge(connectionId),
      connectEdge("edge_gate_0002"),
    ]);

    const deadline = performance.now() + 20_000;
    while (performance.now() < deadline) {
      if (eventError) throw eventError;
      result.connectedEdges = [...edges.values()].filter((edge) => edge.connected).length;
      result.connected = result.connectedEdges === 2;
      result.edgeFrames = [...edges.values()].map(
        (edge) => edge.video.getVideoPlaybackQuality().totalVideoFrames,
      );
      result.frames = Math.min(...result.edgeFrames);
      result.edgeAudioPackets = [];
      result.edgeAudioEnergy = [];
      result.edgeAudioSamples = [];
      result.edgeAudioBytes = [];
      for (const edge of edges.values()) {
        let packets = 0;
        let energy = 0;
        let samples = 0;
        let bytes = 0;
        for (const [, report] of await edge.peer.getStats()) {
          if (report.type === "inbound-rtp" &&
              (report.kind === "audio" || report.mediaType === "audio")) {
            packets = Math.max(packets, Number(report.packetsReceived) || 0);
            energy = Math.max(energy, Number(report.totalAudioEnergy) || 0);
            samples = Math.max(samples, Number(report.totalSamplesReceived) || 0);
            bytes = Math.max(bytes, Number(report.bytesReceived) || 0);
          }
        }
        result.edgeAudioPackets.push(packets);
        result.edgeAudioEnergy.push(energy);
        result.edgeAudioSamples.push(samples);
        result.edgeAudioBytes.push(bytes);
      }
      result.audioPackets = result.edgeAudioPackets.length > 0
        ? Math.min(...result.edgeAudioPackets)
        : 0;
      result.audioEnergy = result.edgeAudioEnergy.length > 0
        ? Math.min(...result.edgeAudioEnergy)
        : 0;
      result.audioSamples = result.edgeAudioSamples.length > 0
        ? Math.min(...result.edgeAudioSamples)
        : 0;
      result.audioBytes = result.edgeAudioBytes.length > 0
        ? Math.min(...result.edgeAudioBytes)
        : 0;
      const videos = [...edges.values()].map((edge) => edge.video);
      result.width = videos.every((video) => video.videoWidth === videos[0]!.videoWidth && video.videoWidth <= 2560) ? videos[0]!.videoWidth : 0;
      result.height = videos.every((video) => video.videoHeight === videos[0]!.videoHeight && video.videoHeight <= 1440) ? videos[0]!.videoHeight : 0;
      if (
        result.captureActive &&
        result.connected &&
        result.frames >= 30 &&
        (!result.audioAvailable ||
          (result.audioPackets > 10 && result.audioEnergy > 0)) &&
        result.width >= 320 &&
        result.height >= 240 &&
        result.nativeCandidateTypes.includes("srflx")
      ) {
        break;
      }
      await new Promise((resolveWait) => window.setTimeout(resolveWait, 100));
    }
    if (
      !result.captureActive ||
      !result.connected ||
      result.frames < 30 ||
      (result.audioAvailable &&
        (result.audioPackets <= 10 || result.audioEnergy <= 0)) ||
      result.width < 320 ||
      result.height < 240 ||
      !result.nativeCandidateTypes.includes("srflx")
    ) {
      throw new Error("Native H264 did not reach decoded Browser video");
    }

    const videos = [...edges.values()].map((edge) => edge.video);
    const sourceSize = { width: result.width, height: result.height };
    const liveSize = geometry.fit(sourceSize, 1280, 720);
    const pausedSize = geometry.fit(sourceSize, 854, 480);
    const framesBeforeLiveUpdate = Math.min(
      ...videos.map((video) => video.getVideoPlaybackQuality().totalVideoFrames),
    );
    const liveUpdate = await request("update-share", {
      shareId,
      profile: {
        resolution: { width: 1280, height: 720 },
        maxFramerate: 60,
        maxBitrate: 12_000_000,
        degradationPreference: "maintain-framerate",
        maxAudioBitrate: 192000,
      },
    });
    const liveDeadline = performance.now() + 15_000;
    while (performance.now() < liveDeadline) {
      const frames = Math.min(
        ...videos.map((video) => video.getVideoPlaybackQuality().totalVideoFrames),
      );
      result.liveProfileUpdated = liveUpdate.type === "share-updated" &&
        videos.every((video) => geometry.matches(video.videoWidth, video.videoHeight, liveSize)) &&
        frames >= framesBeforeLiveUpdate + 10;
      if (result.liveProfileUpdated) break;
      await new Promise((resolveWait) => window.setTimeout(resolveWait, 100));
    }
    if (!result.liveProfileUpdated) {
      throw new Error("Native live profile update did not reach both Viewers");
    }

    await request("pause-share", { shareId, paused: true });
    const pausedUpdate = await request("update-share", {
      shareId,
      profile: {
        resolution: { width: 854, height: 480 },
        maxFramerate: 15,
        maxBitrate: 2_000_000,
        degradationPreference: "maintain-resolution",
        maxAudioBitrate: 64000,
      },
    });
    const framesBeforeResume = Math.min(
      ...videos.map((video) => video.getVideoPlaybackQuality().totalVideoFrames),
    );
    await request("pause-share", { shareId, paused: false });
    const resumeDeadline = performance.now() + 15_000;
    while (performance.now() < resumeDeadline) {
      const frames = Math.min(
        ...videos.map((video) => video.getVideoPlaybackQuality().totalVideoFrames),
      );
      result.pausedProfileUpdated = pausedUpdate.type === "share-updated" &&
        videos.every((video) => geometry.matches(video.videoWidth, video.videoHeight, pausedSize)) &&
        frames >= framesBeforeResume + 10;
      if (result.pausedProfileUpdated) {
        result.frames = frames;
        result.width = videos[0]!.videoWidth;
        result.height = videos[0]!.videoHeight;
        break;
      }
      await new Promise((resolveWait) => window.setTimeout(resolveWait, 100));
    }
    if (!result.pausedProfileUpdated) {
      throw new Error("Native paused profile update did not resume both Viewers");
    }
  } catch (error) {
    result.error = error instanceof Error ? error.message : String(error);
  } finally {
    for (const peer of peers) peer.close();
    socket?.close(1000, "gate complete");
  }
  return result;
}
