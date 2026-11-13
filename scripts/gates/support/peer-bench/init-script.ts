// Page-side benchmark instrumentation. This module returns one classic script
// string, so it may only reference page globals and serialized values.

import { appShellInitScript } from "../../../../src/client/lib/app-shell";
import { MAX_VIEWERS, ROUTE_TIMING_KEYS } from "./types";
import type { PageRole } from "./types";

export function buildBenchmarkInitScript(options: {
  label: string;
  role: PageRole;
  viewerIndex: number | null;
  clearHostRoom: boolean;
  width: number;
  height: number;
  frameRate: number;
  expectedEndpointCap: number;
  qualityControlSmoke?: boolean;
}): string {
  const serialized = JSON.stringify(options);
  // Host pages emulate the App window, the only surface that renders hosting.
  return `${options.role === "host" ? appShellInitScript("gate") : ""}(() => {
    if (globalThis.__PIIK_BENCHMARK__) return;
    const options = ${serialized};
    const expectedRoleCap = options.expectedEndpointCap;
    // The parameter-readback arm exercises ordinary senders. Pool producers
    // and their independent transport clocks have separate probe coverage.
    if (options.qualityControlSmoke) {
      Object.defineProperty(RTCRtpSender.prototype, "createEncodedStreams", { value: undefined, configurable: true });
    }
    if (options.clearHostRoom) {
      try { localStorage.removeItem("piik:host-room:v1"); } catch {}
    }
    const state = {
      label: options.label,
      role: options.role,
      viewerIndex: options.viewerIndex,
      roomId: null,
      peerId: null,
      authenticatedAtEpochMs: null,
      authenticateSentAtEpochMs: null,
      shareRequestedAtEpochMs: null,
      signalingConnected: false,
      qualitySettings: null,
      routeRevision: null,
      routeAssignment: null,
      maxActiveOutboundMediaEdges: 0,
      maxAssignedChildren: 0,
      firstDecodedAtEpochMs: null,
      firstRenderedAtEpochMs: null,
      renderedFrames: 0,
    };
    const connections = [];
    const descriptions = [];
    const accumulators = new WeakMap();
    const detailedStatsModule = import("/src/client/webrtc/stats").catch(() => null);
    let signalingSocket = null;
    let peerAssisted = false;
    let transitionRevision = -1;
    let transitionPhase = null;
    let plannedRouteAssignment = null;
    const nativeSends = new WeakMap();
    const routeTimingKeys = ${JSON.stringify(ROUTE_TIMING_KEYS)};
    let routeDiagnosticRequested = false;
    let routeTimingSamples = null;

    function isOpaqueId(value) {
      return typeof value === "string" &&
        value.length >= 8 &&
        value.length <= 128 &&
        /^[A-Za-z0-9_-]+$/.test(value);
    }

    function cloneRouteAssignment(value) {
      if (!value || typeof value !== "object" || Array.isArray(value)) return null;
      const upstream = value.upstream;
      if (!upstream || typeof upstream !== "object" || Array.isArray(upstream)) return null;
      let clonedUpstream;
      if (upstream.kind === "peer" && isOpaqueId(upstream.peerId)) {
        clonedUpstream = { kind: "peer", peerId: upstream.peerId };
      } else if (upstream.kind === "none" || upstream.kind === "sfu") {
        clonedUpstream = { kind: upstream.kind };
      } else {
        return null;
      }
      if (
        !Array.isArray(value.childPeerIds) ||
        value.childPeerIds.length > expectedRoleCap ||
        !value.childPeerIds.every(isOpaqueId) ||
        new Set(value.childPeerIds).size !== value.childPeerIds.length ||
        (value.sfuPublicationGeneration !== null &&
          !isOpaqueId(value.sfuPublicationGeneration)) ||
        (clonedUpstream.kind === "sfu" &&
          value.sfuPublicationGeneration === null) ||
        (clonedUpstream.kind === "peer" &&
          value.sfuPublicationGeneration !== null)
      ) {
        return null;
      }
      return {
        upstream: clonedUpstream,
        childPeerIds: [...value.childPeerIds],
        sfuPublicationGeneration: value.sfuPublicationGeneration,
      };
    }

    function sameRouteAssignment(left, right) {
      return left.upstream.kind === right.upstream.kind &&
        (left.upstream.kind !== "peer" ||
          (right.upstream.kind === "peer" &&
            left.upstream.peerId === right.upstream.peerId)) &&
        left.sfuPublicationGeneration === right.sfuPublicationGeneration &&
        left.childPeerIds.length === right.childPeerIds.length &&
        left.childPeerIds.every((peerId, index) => peerId === right.childPeerIds[index]);
    }

    function resetRouteTransition() {
      transitionRevision = -1;
      transitionPhase = null;
      plannedRouteAssignment = null;
      state.routeRevision = null;
      state.routeAssignment = null;
    }

    function acceptRouteUpdate(revision, phase, value) {
      const assignment = cloneRouteAssignment(value);
      if (
        !Number.isSafeInteger(revision) ||
        revision < 0 ||
        (phase !== "prepare" && phase !== "active") ||
        !assignment
      ) {
        return false;
      }
      if (revision < transitionRevision) {
        return false;
      }
      if (revision === transitionRevision) {
        if (
          !plannedRouteAssignment ||
          !sameRouteAssignment(plannedRouteAssignment, assignment) ||
          (transitionPhase === "active" && phase === "prepare")
        ) {
          return false;
        }
        if (transitionPhase === phase) {
          return true;
        }
      }
      transitionRevision = revision;
      transitionPhase = phase;
      plannedRouteAssignment = assignment;
      if (phase === "active") {
        state.routeRevision = revision;
        state.routeAssignment = cloneRouteAssignment(assignment);
        state.maxAssignedChildren = Math.max(
          state.maxAssignedChildren,
          assignment.childPeerIds.length,
        );
      }
      return true;
    }

    function sdpKey(sdp) {
      if (typeof sdp !== "string") return null;
      return sdp.match(/^o=.*$/m)?.[0] || sdp;
    }

    function recordDescription(message, direction) {
      const payload = message.type === "sfu-signal" ? message : message.payload;
      if (!payload || payload.kind !== "description" || !payload.description) return;
      descriptions.push({
        peerId: direction === "out" ? message.targetPeerId : message.fromPeerId,
        connectionId: payload.connectionId,
        sdpKey: sdpKey(payload.description.sdp),
      });
      if (descriptions.length > 128) descriptions.shift();
      recordActiveOutboundEdges();
    }

    function sanitizeRouteTimingSamples(value) {
      const children = value && typeof value === "object" &&
        !Array.isArray(value) && Array.isArray(value.children)
        ? value.children
        : null;
      if (!children || children.length > ${MAX_VIEWERS}) return null;
      const samples = Object.fromEntries(
        routeTimingKeys.map((key) => [key, []]),
      );
      for (const child of children) {
        if (!child || typeof child !== "object" || Array.isArray(child)) {
          return null;
        }
        for (const key of routeTimingKeys) {
          const duration = child[key];
          if (
            duration !== null &&
            (!Number.isSafeInteger(duration) || duration < 0)
          ) {
            return null;
          }
          samples[key].push(duration);
        }
      }
      return samples;
    }

    function handleSignalMessage(value, direction, socket) {
      if (typeof value !== "string") return;
      let message;
      try { message = JSON.parse(value); } catch { return; }
      if (!message || typeof message.type !== "string") return;
      if (direction === "out" && message.type === "authenticate") {
        signalingSocket = socket;
        peerAssisted = false;
        state.role = message.role;
        state.roomId = message.roomId;
        state.authenticateSentAtEpochMs = Date.now();
        state.signalingConnected = false;
      } else if (socket !== signalingSocket) {
        return;
      } else if (direction === "out" && message.type === "set-quality-settings") {
        state.qualitySettings = message.qualitySettings;
      }
      if (direction === "in" && message.type === "authenticated") {
        state.peerId = message.peerId;
        state.authenticatedAtEpochMs = Date.now();
        state.signalingConnected = true;
        peerAssisted = message.mediaMode === "peer-assisted";
        resetRouteTransition();
        if (message.mediaMode === "peer-assisted") {
          acceptRouteUpdate(
            message.routeRevision,
            "active",
            message.routeAssignment,
          );
          state.qualitySettings = message.qualitySettings;
        }
      } else if (
        direction === "in" &&
        message.type === "route-update" &&
        peerAssisted
      ) {
        acceptRouteUpdate(message.revision, message.phase, message.assignment);
      } else if (direction === "in" && message.type === "quality-settings") {
        state.qualitySettings = message.qualitySettings;
      } else if (
        direction === "in" &&
        message.type === "route-diagnostic-snapshot" &&
        state.role === "host" &&
        routeDiagnosticRequested &&
        routeTimingSamples === null
      ) {
        routeTimingSamples = sanitizeRouteTimingSamples(message.snapshot);
      }
      if (message.type === "signal" || message.type === "sfu-signal") recordDescription(message, direction);
    }

    const NativeWebSocket = globalThis.WebSocket;
    globalThis.WebSocket = new Proxy(NativeWebSocket, {
      construct(Target, args) {
        const socket = Reflect.construct(Target, args, Target);
        const nativeSend = socket.send;
        nativeSends.set(socket, nativeSend);
        socket.send = function(data) {
          const result = nativeSend.call(socket, data);
          handleSignalMessage(data, "out", socket);
          return result;
        };
        socket.addEventListener("message", (event) => {
          handleSignalMessage(event.data, "in", socket);
        });
        socket.addEventListener("close", () => {
          if (socket === signalingSocket) state.signalingConnected = false;
        });
        return socket;
      },
    });

    function sendDiagnosticMessage(message) {
      const socket = signalingSocket;
      const nativeSend = socket ? nativeSends.get(socket) : null;
      if (!socket || typeof nativeSend !== "function") return false;
      try {
        nativeSend.call(socket, JSON.stringify(message));
        return true;
      } catch { return false; }
    }

    function requestRouteDiagnosticSnapshot() {
      if (
        state.role !== "host" ||
        !state.signalingConnected ||
        routeDiagnosticRequested
      ) {
        return false;
      }
      routeDiagnosticRequested = true;
      if (sendDiagnosticMessage({ type: "request-route-diagnostic" })) {
        return true;
      }
      routeDiagnosticRequested = false;
      return false;
    }

    function routeDiagnosticTimingSamples() {
      return routeTimingSamples
        ? Object.fromEntries(
            routeTimingKeys.map((key) => [key, [...routeTimingSamples[key]]]),
          )
        : null;
    }

    const NativePeerConnection = globalThis.RTCPeerConnection;
    function recordActiveOutboundEdges() {
      const active = networkConnections().filter(({ connection }) =>
        connection.connectionState !== "closed" &&
        connection.connectionState !== "failed" &&
        connection.getSenders().some((sender) => sender.track && sender.track.kind === "video")
      ).length;
      state.maxActiveOutboundMediaEdges = Math.max(
        state.maxActiveOutboundMediaEdges,
        active,
      );
    }

    globalThis.RTCPeerConnection = new Proxy(NativePeerConnection, {
      construct(Target, args) {
        const connection = Reflect.construct(Target, args, Target);
        connections.push({
          connection,
          index: connections.length + 1,
          createdAtEpochMs: Date.now(),
        });
        connection.addEventListener("connectionstatechange", recordActiveOutboundEdges);
        for (const method of ["addTrack", "addTransceiver", "removeTrack", "close"]) {
          const nativeMethod = connection[method];
          if (typeof nativeMethod !== "function") continue;
          connection[method] = function(...methodArgs) {
            const result = nativeMethod.apply(connection, methodArgs);
            queueMicrotask(recordActiveOutboundEdges);
            return result;
          };
        }
        recordActiveOutboundEdges();
        return connection;
      },
    });

    let syntheticStream = null;
    function createSyntheticStream() {
      if (syntheticStream && syntheticStream.getVideoTracks().some((track) => track.readyState === "live")) {
        return syntheticStream;
      }
      const canvas = document.createElement("canvas");
      canvas.width = options.width;
      canvas.height = options.height;
      const context = canvas.getContext("2d", { alpha: false });
      if (!context) throw new Error("Canvas 2D is unavailable");
      let frame = 0;
      const columns = 24;
      const rows = 14;
      const tileWidth = Math.ceil(canvas.width / columns);
      const tileHeight = Math.ceil(canvas.height / rows);
      function draw() {
        frame += 1;
        for (let row = 0; row < rows; row += 1) {
          for (let column = 0; column < columns; column += 1) {
            const value = (frame * 29 + row * 71 + column * 43) % 360;
            context.fillStyle = \`hsl(\${value} 80% \${35 + ((frame + row + column) % 30)}%)\`;
            context.fillRect(column * tileWidth, row * tileHeight, tileWidth, tileHeight);
          }
        }
        const sweep = (frame * Math.max(8, Math.floor(canvas.width / 120))) % canvas.width;
        context.fillStyle = "#ffffff";
        context.fillRect(sweep, 0, Math.max(4, Math.floor(canvas.width / 180)), canvas.height);
        context.fillStyle = "#000000";
        context.font = \`\${Math.max(24, Math.floor(canvas.height / 18))}px monospace\`;
        context.fillText(String(frame).padStart(8, "0"), 24, Math.max(48, Math.floor(canvas.height / 12)));
      }
      draw();
      const timer = setInterval(draw, 1000 / options.frameRate);
      syntheticStream = canvas.captureStream(options.frameRate);
      const track = syntheticStream.getVideoTracks()[0];
      if (track) track.addEventListener("ended", () => clearInterval(timer), { once: true });
      return syntheticStream;
    }

    if (!navigator.mediaDevices) {
      Object.defineProperty(navigator, "mediaDevices", { value: {}, configurable: true });
    }
    Object.defineProperty(navigator.mediaDevices, "getDisplayMedia", {
      configurable: true,
      value: async () => createSyntheticStream(),
    });

    const observedVideos = new WeakSet();
    const videoObserver = setInterval(() => {
      if (options.role !== "viewer") return;
      const video = document.querySelector("#viewer-stage video");
      if (!video || observedVideos.has(video)) return;
      observedVideos.add(video);
      if (typeof video.requestVideoFrameCallback === "function") {
        const onFrame = (now) => {
          const at = performance.timeOrigin + now;
          if (state.firstRenderedAtEpochMs === null) state.firstRenderedAtEpochMs = at;
          state.renderedFrames += 1;
          video.requestVideoFrameCallback(onFrame);
        };
        video.requestVideoFrameCallback(onFrame);
      }
    }, 50);

    function identify(connection) {
      const localSdpKey = sdpKey(connection.localDescription && connection.localDescription.sdp);
      const remoteSdpKey = sdpKey(connection.remoteDescription && connection.remoteDescription.sdp);
      for (let index = descriptions.length - 1; index >= 0; index -= 1) {
        const item = descriptions[index];
        if (item.sdpKey === localSdpKey || item.sdpKey === remoteSdpKey) {
          return { connectionId: item.connectionId || null, remotePeerId: item.peerId || null };
        }
      }
      return { connectionId: null, remotePeerId: null };
    }

    // Local codec probes and encoding-pool transports never cross room signaling.
    // Count actual Peer/SFU media edges, not every RTCPeerConnection in the page.
    function networkConnections() {
      return connections.filter(({ connection }) => identify(connection).connectionId !== null);
    }

    function totals(report, type) {
      let selected = null;
      report.forEach((raw) => {
        if (raw.type === type && raw.kind === "video" && raw.isRemote !== true) selected = raw;
      });
      if (!selected) return null;
      const framesKey = type === "outbound-rtp" ? "framesEncoded" : "framesDecoded";
      const bytesKey = type === "outbound-rtp" ? "bytesSent" : "bytesReceived";
      return {
        id: selected.id,
        framesTotal: Number.isFinite(selected[framesKey]) ? selected[framesKey] : null,
        bytesTotal: Number.isFinite(selected[bytesKey]) ? selected[bytesKey] : null,
      };
    }

    async function connectionSample(record) {
      const connection = record.connection;
      const hasOutboundVideo = connection.getSenders().some((sender) => sender.track && sender.track.kind === "video");
      const hasInboundVideo = connection.getReceivers().some((receiver) => receiver.track && receiver.track.kind === "video");
      const identity = identify(connection);
      try {
        const report = await connection.getStats();
        const sendTotals = totals(report, "outbound-rtp");
        const receiveTotals = totals(report, "inbound-rtp");
        if (receiveTotals && receiveTotals.framesTotal > 0 && state.firstDecodedAtEpochMs === null) {
          state.firstDecodedAtEpochMs = Date.now();
        }
        let accumulator = accumulators.get(connection);
        const module = await detailedStatsModule;
        if (module && !accumulator) {
          accumulator = { send: module.createStatsAccumulator(), receive: module.createStatsAccumulator() };
          accumulators.set(connection, accumulator);
        }
        const send = module && accumulator && hasOutboundVideo
          ? await module.collectConnectionMetrics(connection, "send", accumulator.send)
          : null;
        const receive = module && accumulator && hasInboundVideo
          ? await module.collectConnectionMetrics(connection, "receive", accumulator.receive)
          : null;
        return {
          index: record.index,
          createdAtEpochMs: record.createdAtEpochMs,
          ...identity,
          connectionState: connection.connectionState,
          iceConnectionState: connection.iceConnectionState,
          hasOutboundVideo,
          hasInboundVideo,
          send,
          receive,
          sendTotals,
          receiveTotals,
        };
      } catch (error) {
        return {
          index: record.index,
          createdAtEpochMs: record.createdAtEpochMs,
          ...identity,
          connectionState: connection.connectionState,
          iceConnectionState: connection.iceConnectionState,
          hasOutboundVideo,
          hasInboundVideo,
          send: null,
          receive: null,
          sendTotals: null,
          receiveTotals: null,
          error: error instanceof Error ? error.message : String(error),
        };
      }
    }

    function baseSnapshot() {
      return {
        ...state,
        routeAssignment: cloneRouteAssignment(state.routeAssignment),
      };
    }

    async function sample() {
      const sampledConnections = await Promise.all(networkConnections().map(connectionSample));
      return { ...baseSnapshot(), connections: sampledConnections };
    }

    async function progress() {
      const values = [];
      for (const record of networkConnections()) {
        const connection = record.connection;
        const hasOutboundVideo = connection.getSenders().some((sender) => sender.track && sender.track.kind === "video");
        const hasInboundVideo = connection.getReceivers().some((receiver) => receiver.track && receiver.track.kind === "video");
        try {
          const report = hasInboundVideo ? await connection.getStats() : null;
          const receiveTotals = report ? totals(report, "inbound-rtp") : null;
          if (receiveTotals && receiveTotals.framesTotal > 0 && state.firstDecodedAtEpochMs === null) {
            state.firstDecodedAtEpochMs = Date.now();
          }
          values.push({
            index: record.index,
            ...identify(connection),
            connectionState: connection.connectionState,
            iceConnectionState: connection.iceConnectionState,
            hasOutboundVideo,
            hasInboundVideo,
            receiveTotals,
          });
        } catch {}
      }
      return { ...baseSnapshot(), connections: values };
    }

    function snapshot() {
      return {
        ...baseSnapshot(),
        connections: networkConnections().map((record) => ({
          index: record.index,
          ...identify(record.connection),
          connectionState: record.connection.connectionState,
          iceConnectionState: record.connection.iceConnectionState,
          hasOutboundVideo: record.connection.getSenders().some((sender) => sender.track && sender.track.kind === "video"),
          hasInboundVideo: record.connection.getReceivers().some((receiver) => receiver.track && receiver.track.kind === "video"),
        })),
      };
    }

    function senderDegradationPreferences() {
      return networkConnections().flatMap(({ connection }) => {
        if (
          connection.connectionState === "closed" ||
          connection.connectionState === "failed"
        ) return [];
        return connection.getSenders()
          .filter((sender) => sender.track?.kind === "video")
          .map((sender) => sender.getParameters().degradationPreference ?? null);
      });
    }
    function markShareRequested() {
      state.shareRequestedAtEpochMs = Date.now();
    }
    Object.defineProperty(globalThis, "__PIIK_BENCHMARK__", {
      configurable: false,
      value: { sample, progress, snapshot, senderDegradationPreferences, markShareRequested, requestRouteDiagnosticSnapshot, routeDiagnosticTimingSamples, stop: () => clearInterval(videoObserver) },
    });
  })();`;
}
