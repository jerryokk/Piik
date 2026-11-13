import type { RemoteControlMode } from "../remote-control/protocol";
import { debugEvent } from "../lib/debug";
import type { IceConfig, QualitySettings, SignalPayload, SfuSignalMessage, PunchPrediction } from "../../shared/protocol";
import { nativeCaptureTargetKey, type NativeCapturePath } from "./capture-selection";
import {
  captureOptionsResponseSchema,
  edgeOfferResponseSchema,
  nativeAckResponseSchema,
  remoteStopShortcutResponseSchema,
  NATIVE_CLIENT_SUBPROTOCOL,
  publicationResponseSchema,
  receiveAnswerResponseSchema,
  readyResponseSchema,
  shareStartedResponseSchema,
  shareSourceReplacedResponseSchema,
  shareUpdatedResponseSchema,
  sourceListResponseSchema,
  microphoneListResponseSchema,
  sourcePreviewResponseSchema,
  punchPreparedResponseSchema,
  punchCompleteResponseSchema,
  type NativeAdapter,
  type NativeCaptureTarget,
  type NativeIceCandidate,
  type NativeVideoCodec,
  type RemoteStopShortcut,
  type NativeHealth,
} from "./wire";
import { discoverNativeHealth, type NativeDiscoveryOptions } from "./client-discovery";
import { NativeClientTransport } from "./client-transport";

const REQUEST_TIMEOUT_MS = 8_000;

interface NativeShareInput {
  shareId: string;
  source: NativeCaptureTarget;
  audio: boolean;
  excludeAudio?: NativeCaptureTarget;
  showCaptureBorder?: boolean;
  adapterIndex: number;
  encoderIndex: number;
  edgeCapacity: number;
  profile: QualitySettings;
  codec: NativeVideoCodec | "auto";
}

/** App control commands over one authenticated local socket. */
export class NativeClient extends NativeClientTransport {
  private audioVolumeUpdates = new Map<"set-microphone" | "set-share-volume", { shareId: string; volume: number; done: Promise<void> }>();

  private constructor(
    health: NativeHealth,
    socket: WebSocket,
  ) {
    super(health, socket);
  }

  static async connect(options?: NativeDiscoveryOptions): Promise<NativeClient | null> {
    const signal = options?.signal;
    const health = await discoverNativeHealth(options);
    if (!health || signal?.aborted) return null;
    const socket = new WebSocket(`ws://127.0.0.1:${health.port}/control`, [
      `${NATIVE_CLIENT_SUBPROTOCOL}.${health.instanceToken}`,
    ]);
    const client = new NativeClient(health, socket);
    // Acquisition owns discovery, the WebSocket handshake and hello together.
    // Once returned, the caller owns the live client independently of this signal.
    const abort = () => client.close();
    signal?.addEventListener("abort", abort, { once: true });
    try {
      const opened = await new Promise<boolean>((resolveOpen) => {
        const finish = (value: boolean) => {
          window.clearTimeout(timer);
          socket.removeEventListener("open", onOpen);
          socket.removeEventListener("error", fail);
          socket.removeEventListener("close", fail);
          signal?.removeEventListener("abort", fail);
          resolveOpen(value);
        };
        const onOpen = () => finish(socket.protocol === `${NATIVE_CLIENT_SUBPROTOCOL}.${health.instanceToken}`);
        const fail = () => finish(false);
        const timer = window.setTimeout(fail, REQUEST_TIMEOUT_MS);
        socket.addEventListener("open", onOpen);
        socket.addEventListener("error", fail);
        socket.addEventListener("close", fail);
        signal?.addEventListener("abort", fail, { once: true });
      });
      if (!opened || client.closed || signal?.aborted) {
        if (!signal?.aborted) debugEvent("native", "unavailable", { stage: "control" });
        client.close();
        return null;
      }
      await client.request("hello", {}, readyResponseSchema);
      if (client.closed || signal?.aborted) { client.close(); return null; }
      debugEvent("native", "connected");
      return client;
    } catch {
      client.close();
      return null;
    } finally {
      signal?.removeEventListener("abort", abort);
    }
  }

  async captureOptions(): Promise<NativeAdapter[]> {
    const response = await this.request(
      "capture-options",
      {},
      captureOptionsResponseSchema,
    );
    return response.adapters;
  }

  async sources(): Promise<NativeCaptureTarget[]> {
    const response = await this.request(
      "list-sources",
      {},
      sourceListResponseSchema,
    );
    return response.sources;
  }

  async sourcePreview(target: NativeCaptureTarget): Promise<string | null> {
    const response = await this.request(
      "source-preview",
      { source: target },
      sourcePreviewResponseSchema,
    );
    if (response.sourceKey !== nativeCaptureTargetKey(target)) {
      throw new Error("Native capture preview identity changed");
    }
    return response.data
      ? `data:${response.mime};base64,${response.data}`
      : null;
  }

  async startShare(
    input: NativeShareInput,
    onRequested?: () => void,
  ): Promise<{ audio: boolean; codec: NativeVideoCodec; sourceAudio?: boolean }> {
    // Commands carry only fields that the probed helper or the requested
    // source actually needs; the rest are fixed facts of this build.
    const { showCaptureBorder = false, excludeAudio, ...shareInput } = input;
    this.validateAudioExclusion(excludeAudio);
    const response = await this.request(
      "start-share",
      {
        ...shareInput,
        ...(excludeAudio ? { excludeAudio } : {}),
        // Keep a stable output for source-audio toggles during sharing.
        microphoneMixing: true,
        ...(this.health.nativeMedia.remoteInput ? { remoteInput: true } : {}),
        ...(this.health.nativeMedia.captureBorderControl ? { showCaptureBorder } : {}),
        reportSourceCancellation: true,
        reportCaptureFailure: true,
      },
      shareStartedResponseSchema,
      input.source.kind === "picker" ? null : REQUEST_TIMEOUT_MS,
      onRequested,
    );
    if (response.shareId !== input.shareId) {
      throw new Error("Native share identity changed");
    }
    return { audio: response.audio, codec: response.codec, sourceAudio: response.sourceAudio };
  }

  async updateShare(shareId: string, profile: QualitySettings): Promise<void> {
    const response = await this.request(
      "update-share",
      { shareId, profile },
      shareUpdatedResponseSchema,
      null,
    );
    if (response.shareId !== shareId) {
      throw new Error("Native share identity changed");
    }
  }

  private validateAudioExclusion(target: NativeCaptureTarget | undefined): void {
    if (target && !this.health.nativeMedia.processAudioExclusion) {
      throw new Error("Piik App does not support audio exclusion");
    }
  }

  async replaceShareSource(
    shareId: string,
    source: NativeCaptureTarget,
    audio: boolean,
    path: NativeCapturePath,
    showCaptureBorder = false,
    excludeAudio?: NativeCaptureTarget,
  ): Promise<void> {
    this.validateAudioExclusion(excludeAudio);
    const response = await this.request(
      "replace-share-source",
      {
        shareId,
        source,
        audio,
        ...(excludeAudio ? { excludeAudio } : {}),
        adapterIndex: path.adapterIndex,
        encoderIndex: path.encoderIndex,
        ...(this.health.nativeMedia.captureBorderControl ? { showCaptureBorder } : {}),
        reportSourceCancellation: true,
      },
      shareSourceReplacedResponseSchema,
      source.kind === "picker" ? null : REQUEST_TIMEOUT_MS,
    );
    if (response.shareId !== shareId) {
      throw new Error("Native share identity changed");
    }
  }

  async prepareEdge(
    shareId: string,
    connectionId: string,
    iceConfig: IceConfig,
    sourceConnectionId?: string,
    punched = false,
  ): Promise<RTCSessionDescriptionInit> {
    const response = await this.request(
      "prepare-edge",
      {
        shareId,
        connectionId,
        ...(punched ? { punched: true } : {}),
        ...(sourceConnectionId ? { sourceConnectionId } : {}),
        iceServers: iceConfig.iceServers.map((server) => ({
          urls: Array.isArray(server.urls) ? server.urls : [server.urls],
        })),
      },
      edgeOfferResponseSchema,
    );
    if (
      response.shareId !== shareId ||
      response.connectionId !== connectionId
    ) {
      throw new Error("Native edge identity changed");
    }
    return { type: "offer", sdp: response.sdp };
  }

  async preparePunch(shareId: string, connectionId: string, iceConfig: IceConfig,
    secret: string, edgeCapacity?: number, sourceConnectionId?: string): Promise<PunchPrediction | null> {
    const response = await this.request("prepare-punch", {
      shareId, connectionId, secret,
      ...(edgeCapacity !== undefined ? { edgeCapacity } : {}),
      ...(sourceConnectionId ? { sourceConnectionId } : {}),
      iceServers: iceConfig.iceServers.map(server => ({ urls: Array.isArray(server.urls) ? server.urls : [server.urls] })),
    }, punchPreparedResponseSchema);
    return response.prediction;
  }

  async runPunch(shareId: string, connectionId: string, prediction: PunchPrediction): Promise<boolean> {
    return (await this.request("run-punch", { shareId, connectionId, prediction }, punchCompleteResponseSchema)).connected;
  }

  async closePunch(shareId: string, connectionId: string): Promise<void> {
    await this.request("close-punch", { shareId, connectionId }, nativeAckResponseSchema);
  }

  async preparePublication(
    shareId: string,
    publicationGeneration: string,
    connectionId: string,
    iceConfig: IceConfig,
  ) {
    const response = await this.request(
      "prepare-publication",
      {
        shareId,
        publicationGeneration,
        connectionId,
        iceServers: iceConfig.iceServers.map((server) => ({
          urls: Array.isArray(server.urls) ? server.urls : [server.urls],
        })),
      },
      publicationResponseSchema,
    );
    if (
      response.type !== "publication-offer" ||
      !response.sdp ||
      response.shareId !== shareId ||
      response.publicationGeneration !== publicationGeneration ||
      response.connectionId !== connectionId
    ) {
      throw new Error("Native publication identity changed");
    }
    return {
      description: { type: "offer" as const, sdp: response.sdp },
      media: response.media,
    };
  }

  async publicationMedia(
    shareId: string,
    publicationGeneration: string,
    connectionId: string,
  ) {
    const response = await this.request(
      "publication-media",
      {
        shareId,
        publicationGeneration,
        connectionId,
      },
      publicationResponseSchema,
    );
    if (
      response.type !== "publication-media" ||
      response.shareId !== shareId ||
      response.publicationGeneration !== publicationGeneration ||
      response.connectionId !== connectionId
    ) {
      throw new Error("Native publication identity changed");
    }
    return response.media;
  }

  async acceptPublicationSignal(
    shareId: string,
    message: SfuSignalMessage,
  ): Promise<void> {
    const identity = {
      shareId,
      publicationGeneration: message.publicationGeneration,
      connectionId: message.connectionId,
    };
    if (
      message.kind === "description" &&
      message.description?.type === "answer"
    ) {
      await this.request(
        "publication-answer",
        { ...identity, sdp: message.description.sdp },
        nativeAckResponseSchema,
      );
    } else if (message.kind === "candidate") {
      await this.request(
        "publication-candidate",
        { ...identity, candidate: message.candidate ?? null },
        nativeAckResponseSchema,
      );
    } else if (message.kind === "layers") {
      await this.request(
        "publication-layers",
        { ...identity, activeCount: message.activeCount },
        nativeAckResponseSchema,
      );
    }
  }

  async closePublication(
    shareId: string,
    publicationGeneration: string,
    connectionId: string,
  ): Promise<void> {
    await this.request(
      "close-publication",
      { shareId, publicationGeneration, connectionId },
      nativeAckResponseSchema,
    );
  }

  async prepareLocalEdge(
    shareId: string,
    connectionId: string,
    sourceConnectionId?: string,
    restart = false,
  ): Promise<RTCSessionDescriptionInit & { offerId: string }> {
    const response = await this.request(
      "prepare-local-edge",
      {
        shareId,
        connectionId,
        restart,
        ...(sourceConnectionId ? { sourceConnectionId } : {}),
      },
      edgeOfferResponseSchema,
    );
    if (
      response.shareId !== shareId ||
      response.connectionId !== connectionId
    ) {
      throw new Error("Native local edge identity changed");
    }
    return { type: "offer", sdp: response.sdp, offerId: response.id };
  }

  async receiveOffer(
    shareId: string,
    connectionId: string,
    offer: RTCSessionDescriptionInit,
    iceConfig: IceConfig,
    edgeCapacity: number,
    punched = false,
  ): Promise<{
    answer: { type: "answer"; sdp: string };
    audio: boolean;
    codec: NativeVideoCodec;
    reused: boolean;
  }> {
    if (offer.type !== "offer" || !offer.sdp) {
      throw new Error("Native receiver requires an SDP offer");
    }
    const response = await this.request(
      "receive-offer",
      {
        shareId,
        connectionId,
        reuseReceiver: true,
        edgeCapacity,
        ...(punched ? { punched: true } : {}),
        iceServers: iceConfig.iceServers.map((server) => ({
          urls: Array.isArray(server.urls) ? server.urls : [server.urls],
        })),
        sdp: offer.sdp,
      },
      receiveAnswerResponseSchema,
    );
    if (
      response.shareId !== shareId ||
      response.connectionId !== connectionId
    ) {
      throw new Error("Native receiver identity changed");
    }
    return {
      answer: { type: "answer", sdp: response.sdp },
      audio: response.audio,
      codec: response.codec,
      reused: response.reused === true,
    };
  }

  async addReceiveCandidate(
    shareId: string,
    connectionId: string,
    candidate: NativeIceCandidate | null,
  ): Promise<void> {
    await this.request(
      "receive-candidate",
      { shareId, connectionId, candidate },
      nativeAckResponseSchema,
    );
  }

  async closeReceiver(shareId: string, connectionId: string): Promise<void> {
    await this.request(
      "close-receiver",
      { shareId, connectionId },
      nativeAckResponseSchema,
    );
  }

  async stopReceive(shareId: string): Promise<void> {
    await this.request("stop-receive", { shareId }, nativeAckResponseSchema);
  }

  async acceptSignal(
    shareId: string,
    connectionId: string,
    payload: SignalPayload,
    offerId?: string,
  ): Promise<void> {
    if (payload.connectionId !== connectionId || payload.kind === "punch") return;
    if (payload.kind === "description") {
      if (payload.description.type !== "answer") return;
      await this.request(
        "edge-answer",
        { shareId, connectionId, sdp: payload.description.sdp, ...(offerId ? { offerId } : {}) },
        nativeAckResponseSchema,
      );
      return;
    }
    await this.request(
      "edge-candidate",
      { shareId, connectionId, candidate: payload.candidate },
      nativeAckResponseSchema,
    );
  }

  async closeEdge(shareId: string, connectionId: string): Promise<void> {
    await this.request(
      "close-edge",
      { shareId, connectionId },
      nativeAckResponseSchema,
    );
  }

  async microphones(): Promise<{ id: string; label: string }[]> {
    if (!this.health.nativeMedia.microphone) throw new Error("Piik App microphone is unavailable");
    return (await this.request("list-microphones", {}, microphoneListResponseSchema)).devices;
  }

  async setMicrophone(shareId: string, enabled: boolean, volume: number, deviceId = ""): Promise<void> {
    if (!this.health.nativeMedia.microphone) throw new Error("Piik App microphone is unavailable");
    await this.request("set-microphone", { shareId, enabled, volume, deviceId }, nativeAckResponseSchema, null);
  }

  // Dragging keeps at most one request in flight and one latest value. It cannot
  // queue dozens of slider events ahead of Stop or reopen a lost microphone.
  setMicrophoneVolume(shareId: string, volume: number): Promise<void> {
    if (!this.health.nativeMedia.microphone) return Promise.reject(new Error("Piik App microphone is unavailable"));
    return this.setAudioVolume("set-microphone", shareId, volume);
  }

  setShareVolume(shareId: string, volume: number): Promise<void> {
    if (!this.health.nativeMedia.shareVolume) return Promise.reject(new Error("Piik App share volume is unavailable"));
    return this.setAudioVolume("set-share-volume", shareId, volume);
  }

  private setAudioVolume(type: "set-microphone" | "set-share-volume", shareId: string, volume: number): Promise<void> {
    const active = this.audioVolumeUpdates.get(type);
    if (active?.shareId === shareId) { active.volume = volume; return active.done; }
    const update = { shareId, volume, done: Promise.resolve() };
    this.audioVolumeUpdates.set(type, update);
    update.done = (async () => {
      do {
        const sent = update.volume;
        await this.request(type, { shareId, volume: sent }, nativeAckResponseSchema);
        if (sent === update.volume) break;
      } while (this.audioVolumeUpdates.get(type) === update && !this.closed);
    })().finally(() => { if (this.audioVolumeUpdates.get(type) === update) this.audioVolumeUpdates.delete(type); });
    return update.done;
  }

  async setRemoteControl(shareId: string, requestId: string, allow: boolean): Promise<void> {
    await this.request("set-remote-control", { shareId, requestId, allow }, nativeAckResponseSchema);
  }

  async setRemoteControlPolicy(shareId: string, mode: RemoteControlMode, password?: string): Promise<void> {
    await this.request("set-remote-control-policy", { shareId, mode, ...(password !== undefined ? { password } : {}), coPlay: true }, nativeAckResponseSchema, null);
  }

  async remoteStopShortcut(shareId: string, key?: number): Promise<RemoteStopShortcut> {
    if (!this.health.nativeMedia.remoteStopShortcut) throw new Error("Piik App shortcut settings are unavailable");
    return this.request(key === undefined ? "get-remote-stop-shortcut" : "set-remote-stop-shortcut",
      { shareId, ...(key === undefined ? {} : { key }) }, remoteStopShortcutResponseSchema);
  }

  async setRemoteInputPeers(shareId: string, peers: { peerId: string; endpointId: string; secret: string }[]): Promise<void> {
    await this.request("set-remote-input-peers", { shareId, peers }, nativeAckResponseSchema);
  }

  async setRemoteControlSimultaneous(shareId: string, enabled: boolean): Promise<void> {
    await this.request("set-remote-control-simultaneous", { shareId, enabled }, nativeAckResponseSchema);
  }

  async stopShare(shareId: string): Promise<void> {
    for (const [type, update] of this.audioVolumeUpdates) if (update.shareId === shareId) this.audioVolumeUpdates.delete(type);
    await this.request("stop-share", { shareId }, nativeAckResponseSchema);
  }

  async setPaused(shareId: string, paused: boolean): Promise<void> {
    await this.request(
      "pause-share",
      { shareId, paused },
      nativeAckResponseSchema,
    );
  }

  protected retireCommands(): void {
    this.audioVolumeUpdates.clear();
  }
}

export {
  discoverNativeHealth,
  discoverNativeConnect,
  notifyNativePresentation,
  isLoopbackPage,
  NativeCompatibilityError,
} from "./client-discovery";
export { NativeRequestError } from "./client-transport";
