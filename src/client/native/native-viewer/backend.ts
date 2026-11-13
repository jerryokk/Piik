import type { IceConfig, SignalPayload } from "../../../shared/protocol";
import { observeDecodedFrameProof } from "../../media/decoded-frame-proof";
import { EMPTY_METRICS, type PeerSnapshot } from "../../types";
import {
  createStatsAccumulator,
  type StatsAccumulator,
} from "../../webrtc/stats";
import {
  type ViewerMediaPeer,
  VIEWER_AUTOMATIC_RECOVERY_TIMEOUT_MS,
  VIEWER_INITIAL_CONNECTION_TIMEOUT_MS,
  VIEWER_MAX_AUTOMATIC_RECOVERY_REQUESTS,
  type ViewerPeerEvents,
  type ViewerPeerOptions,
  type ViewerRecoveryBudget,
  createViewerRecoveryBudget,
} from "../../webrtc/viewer-peer";
import {
  iceServersWithNatPrediction,
  isNativeNatSurveyCandidate,
  NatPredictionCandidateEmitter,
  natPredictionSurveyUrls,
  type SignalCandidate,
} from "../../webrtc/nat-prediction";
import { NativeClient } from "../client";
import { NativeMediaBridge } from "../media-bridge";
import { type NativeClientEvent, type NativeVideoCodec } from "../wire";
import type { NativeViewerSource, PeerIceConfig } from "./types";

export class NativeViewerPeer implements ViewerMediaPeer {
  getRemoteInput(): RTCDataChannel | null { return this.bridge?.getRemoteInput() ?? null; }
  private currentIceConfig: PeerIceConfig;
  private parentPeerId: string | null = null;
  private connectionId: string | null = null;
  private bridge: NativeMediaBridge | null = null;
  private localCandidates: { emitter: NatPredictionCandidateEmitter; iceConfig: IceConfig } | null = null;
  private unsubscribe: (() => void) | null = null;
  private unsubscribeClose: (() => void) | null = null;
  private snapshot: PeerSnapshot | null = null;
  private statsAccumulator: StatsAccumulator = createStatsAccumulator();
  private statsTimer: number | null = null;
  private statsInFlight = false;
  private stopDecodedFrameObserver: (() => void) | null = null;
  private disconnectTimer: number | null = null;
  private initialConnectionTimer: number | null = null;
  private recoveryTimer: number | null = null;
  private readonly recoveryBudget: ViewerRecoveryBudget;
  private state: RTCPeerConnectionState = "new";
  private localCandidateType: RTCIceCandidateType | null = null;
  private remoteCandidateType: RTCIceCandidateType | null = null;
  private natTraversalPath: "unknown" | "ordinary" | "predicted" = "unknown";
  private recoveryOwner: "viewer" | "route";
  private readonly natPredictionEnabled: boolean;
  private disposed = false;
  private failureNotified = false;
  private sourceGeneration = 0;
  private sourceReady = false;
  private sourceCodec: NativeVideoCodec | null = null;
  private pendingOffer: { bridge: NativeMediaBridge | null; bridgeFailed: boolean } | null = null;

  get source(): NativeViewerSource | null {
    return this.connectionId && this.sourceReady && this.sourceCodec
      ? {
          client: this.client,
          sessionId: this.sessionId,
          connectionId: this.connectionId,
          generation: this.sourceGeneration,
          codec: this.sourceCodec,
        }
      : null;
  }

  constructor(
    iceConfig: PeerIceConfig,
    private readonly events: ViewerPeerEvents,
    options: ViewerPeerOptions,
    private readonly client: NativeClient,
    private readonly sessionId: string,
    private readonly edgeCapacity: number,
    private readonly onFailure: () => void,
  ) {
    this.currentIceConfig = iceConfig;
    this.recoveryOwner = options.recoveryOwner ?? "viewer";
    this.recoveryBudget = options.recoveryBudget ?? createViewerRecoveryBudget();
    this.natPredictionEnabled = options.natPredictionEnabled ?? false;
    this.unsubscribe = client.onEvent((event) => this.onEvent(event));
    this.unsubscribeClose = client.onClose(() => {
      this.handleBridgeFailure(this.connectionId ?? "");
    });
  }

  async acceptSignal(parentPeerId: string, payload: SignalPayload): Promise<void> {
    if (this.disposed) return;
    if (payload.kind === "punch") return;
    if (payload.kind === "candidate") {
      if (
        this.parentPeerId !== parentPeerId ||
        this.connectionId !== payload.connectionId
      ) {
        return;
      }
      try {
        await this.client.addReceiveCandidate(
          this.sessionId,
          payload.connectionId,
          payload.candidate,
        );
      } catch {
        this.handleBridgeFailure(payload.connectionId);
      }
      return;
    }
    if (payload.description.type !== "offer") return;
    try {
      await this.acceptOffer(parentPeerId, payload);
    } catch {
      if (payload.punched) {
        this.retireFailedConnection(payload.connectionId);
      } else {
        this.handleBridgeFailure(payload.connectionId);
      }
    }
  }

  updateIceConfig(iceConfig: IceConfig): void {
    this.currentIceConfig = iceConfig;
  }

  requestRecovery(rebuild = false): boolean {
    if (
      this.recoveryOwner !== "viewer" ||
      this.recoveryTimer !== null ||
      !this.connectionId ||
      !this.parentPeerId
    ) {
      return false;
    }
    const sent = this.events.sendRestartRequest(
      this.parentPeerId,
      this.connectionId,
      rebuild,
    );
    if (sent) {
      this.clearInitialConnectionTimer();
      this.recoveryBudget.exhaustedReported = false;
      this.recoveryBudget.requests = Math.max(
        this.recoveryBudget.requests,
        rebuild ? VIEWER_MAX_AUTOMATIC_RECOVERY_REQUESTS : 1,
      );
      this.scheduleRecoveryDeadline();
    }
    return sent;
  }

  isRecovering(): boolean { return this.recoveryTimer !== null; }
  hasConnection(): boolean { return this.connectionId !== null; }
  hasConnectionId(connectionId: string): boolean {
    return this.connectionId === connectionId;
  }
  getConnectionIdentity(): { parentPeerId: string; connectionId: string } | null {
    return this.parentPeerId && this.connectionId
      ? { parentPeerId: this.parentPeerId, connectionId: this.connectionId }
      : null;
  }
  isConnected(): boolean { return this.state === "connected"; }
  stopDecodedFrameProof(): void {
    this.stopDecodedFrameObserver?.();
    this.stopDecodedFrameObserver = null;
  }
  activatePreparedRoute(): void {
    if (this.recoveryOwner === "viewer") return;
    this.recoveryOwner = "viewer";
    this.handleConnectionState(this.state);
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.clearTimers();
    this.disposeBridge();
    this.localCandidates?.emitter.discard();
    this.localCandidates = null;
    this.unsubscribe?.();
    this.unsubscribe = null;
    this.unsubscribeClose?.();
    this.unsubscribeClose = null;
    const connectionId = this.connectionId;
    this.connectionId = null;
    this.parentPeerId = null;
    this.sourceReady = false;
    this.sourceCodec = null;
    if (connectionId) {
      void this.client.closeReceiver(this.sessionId, connectionId).catch(
        () => undefined,
      );
    }
  }

  private async acceptOffer(
    parentPeerId: string,
    payload: Extract<SignalPayload, { kind: "description" }>,
  ): Promise<void> {
    const connectionId = payload.connectionId;
    const identityChanged =
      this.parentPeerId !== parentPeerId || this.connectionId !== connectionId;
    // The current App build always reuses its receiver for a repeated offer.
    const retainSource = !identityChanged && this.sourceReady && !this.failureNotified;
    if (identityChanged) {
      const previous = this.connectionId;
      this.disposeBridge();
      this.localCandidates?.emitter.discard();
      this.localCandidates = null;
      if (previous) {
        await this.client.closeReceiver(this.sessionId, previous).catch(
          () => undefined,
        );
      }
      this.parentPeerId = parentPeerId;
      this.connectionId = connectionId;
    } else if (!retainSource) {
      this.disposeBridge();
    }
    if (!retainSource) {
      this.state = "connecting";
      this.failureNotified = false;
      this.sourceGeneration += 1;
      this.sourceReady = false;
      this.sourceCodec = null;
      this.statsAccumulator = createStatsAccumulator();
      this.snapshot = {
        peerId: parentPeerId,
        connectionId,
        connectionState: "connecting",
        iceConnectionState: "checking",
        metrics: { ...EMPTY_METRICS },
        error: null,
      };
    }
    const predictionEnabled =
      this.natPredictionEnabled &&
      natPredictionSurveyUrls(
        this.currentIceConfig.iceServers,
        this.currentIceConfig.natPredictionStunUrls,
      ).size > 0;
    const nativeIceConfig: IceConfig = {
      natPredictionStunUrls: this.currentIceConfig.natPredictionStunUrls
        ? [...this.currentIceConfig.natPredictionStunUrls]
        : undefined,
      iceServers: iceServersWithNatPrediction(
        this.currentIceConfig.iceServers,
        predictionEnabled,
        this.currentIceConfig.natPredictionStunUrls,
      ),
    };
    const sendCandidate = (candidate: SignalCandidate) => {
      if (
        this.disposed ||
        this.connectionId !== connectionId ||
        this.parentPeerId !== parentPeerId
      ) {
        return;
      }
      this.events.sendSignal(parentPeerId, {
        kind: "candidate",
        connectionId,
        candidate,
      });
    };
    if (!this.localCandidates) {
      this.localCandidates = {
        emitter: new NatPredictionCandidateEmitter(this.natPredictionEnabled, sendCandidate),
        iceConfig: nativeIceConfig,
      };
    } else this.localCandidates.iceConfig = nativeIceConfig;
    this.emit();
    const pending = { bridge: retainSource ? this.bridge : null, bridgeFailed: false };
    this.pendingOffer = pending;
    let result: Awaited<ReturnType<NativeClient["receiveOffer"]>>;
    try {
      result = await this.client.receiveOffer(
        this.sessionId, connectionId, payload.description, nativeIceConfig, this.edgeCapacity,
        payload.punched === true,
      );
    } finally {
      if (this.pendingOffer === pending) this.pendingOffer = null;
    }
    if (
      this.disposed ||
      this.failureNotified ||
      this.connectionId !== connectionId ||
      this.parentPeerId !== parentPeerId
    ) {
      await this.client.closeReceiver(this.sessionId, connectionId).catch(
        () => undefined,
      );
      return;
    }
    if (retainSource && result.reused) {
      // Renegotiation belongs to the current receiver. Network ICE recovery
      // may pause upstream RTP, but it does not retire playback or relay edges.
      if (pending.bridgeFailed) {
        this.handleBridgeFailure(connectionId);
      } else if (!this.events.sendSignal(parentPeerId, {
        kind: "description", connectionId, description: result.answer,
      })) {
        this.scheduleRecoveryDeadline();
      }
      return;
    }
    if (retainSource) {
      // The App replaced a media shape that the previous bridge could not
      // follow. An old bridge failure must not close the newly installed
      // receiver.
      this.disposeBridge();
      this.sourceGeneration += 1;
      this.statsAccumulator = createStatsAccumulator();
      if (this.snapshot) this.snapshot = { ...this.snapshot, metrics: { ...EMPTY_METRICS } };
    }
    this.sourceCodec = result.codec;
    this.sourceReady = true;
    if (!this.events.sendSignal(parentPeerId, {
      kind: "description",
      connectionId,
      description: result.answer,
    })) {
      // A room-signaling outage is not evidence the local Native capability is
      // unavailable: close only this receiver and use the ordinary recovery
      // path, like the Browser peer's "signal send rejected" handling.
      this.retireFailedConnection(connectionId);
      return;
    }
    this.scheduleInitialConnectionDeadline(connectionId);
    const bridge = new NativeMediaBridge(
      this.sessionId,
      this.client,
      () => this.handlePlaybackFailure(bridge, connectionId),
      connectionId,
      () => { if (this.bridge === bridge) this.emit(); },
    );
    this.bridge = bridge;
    void bridge.start(result.audio).then((stream) => {
      if (this.disposed || this.bridge !== bridge || this.connectionId !== connectionId) {
        bridge.dispose();
        return;
      }
      this.events.onStream(stream);
      this.startObservation(bridge, connectionId);
    }).catch(() => {
      this.handlePlaybackFailure(bridge, connectionId);
    });
  }

  private handlePlaybackFailure(bridge: NativeMediaBridge, connectionId: string): void {
    if (this.bridge !== bridge) return;
    if (this.pendingOffer?.bridge === bridge) this.pendingOffer.bridgeFailed = true;
    else this.handleBridgeFailure(connectionId);
  }

  private retireFailedConnection(connectionId: string): void {
    if (this.disposed || this.connectionId !== connectionId) return;
    this.sourceReady = false;
    this.sourceCodec = null;
    void this.client.closeReceiver(this.sessionId, connectionId).catch(() => undefined);
    if (this.recoveryOwner === "route") this.reportRecoveryExhausted();
    else this.attemptAutomaticRecovery(true);
  }

  private onEvent(event: NativeClientEvent): void {
    if (this.disposed || this.failureNotified || event.shareId !== this.sessionId) {
      return;
    }
    if (event.type === "share-ended") {
      this.handleBridgeFailure(this.connectionId ?? "");
      return;
    }
    if (!("connectionId" in event) || event.connectionId !== this.connectionId) {
      return;
    }
    if (event.type === "edge-candidate") {
      if (this.localCandidates) {
        const { emitter, iceConfig } = this.localCandidates;
        emitter.addSignal(event.candidate,
          isNativeNatSurveyCandidate(event.candidate, iceConfig.iceServers, iceConfig.natPredictionStunUrls));
      } else if (this.parentPeerId && this.connectionId) {
        this.events.sendSignal(this.parentPeerId, {
          kind: "candidate",
          connectionId: this.connectionId,
          candidate: event.candidate,
        });
      }
      return;
    }
    if (event.type === "edge-path") {
      this.localCandidateType = event.localType;
      this.remoteCandidateType = event.remoteType;
      this.natTraversalPath = event.natTraversalPath;
      this.emit();
      return;
    }
    if (event.type === "edge-state") {
      this.state = event.state;
      this.handleConnectionState(event.state);
      this.emit();
    }
  }

  private startObservation(bridge: NativeMediaBridge, connectionId: string): void {
    this.stopDecodedFrameProof();
    this.stopDecodedFrameObserver = observeDecodedFrameProof({
      readFramesDecoded: () => bridge.decodedVideoFrames(),
      owns: () =>
        !this.disposed && this.bridge === bridge && this.connectionId === connectionId,
      onProof: () => this.isConnected() &&
        (this.events.onFirstDecodedFrame?.(connectionId) ?? true),
    });
    this.statsTimer = window.setInterval(() => {
      void this.updateStats(bridge, connectionId);
    }, 2_000);
    void this.updateStats(bridge, connectionId);
  }

  private async updateStats(bridge: NativeMediaBridge, connectionId: string): Promise<void> {
    if (
      this.disposed ||
      this.bridge !== bridge ||
      this.connectionId !== connectionId ||
      this.statsInFlight
    ) {
      return;
    }
    this.statsInFlight = true;
    try {
      const metrics = await bridge.collectMetrics(this.statsAccumulator);
      if (this.bridge !== bridge || this.connectionId !== connectionId || !this.snapshot) return;
      this.snapshot = {
        ...this.snapshot,
        metrics: {
          ...metrics,
          path: this.localCandidateType && this.remoteCandidateType ? "direct" : metrics.path,
          natTraversalPath: this.natTraversalPath,
          selectedCandidatePairId: null,
          iceProtocol:
            this.localCandidateType && this.remoteCandidateType ? "udp" : null,
          localCandidateType: this.localCandidateType,
          remoteCandidateType: this.remoteCandidateType,
          localCandidateAddress: null,
          localCandidatePort: null,
          remoteCandidateAddress: null,
          remoteCandidatePort: null,
          rttMs: null,
          jitterMs: null,
          audioJitterMs: null,
        },
      };
      this.emit();
    } catch {
      // Metrics are observational and never own the media connection.
    } finally {
      this.statsInFlight = false;
    }
  }

  private handleConnectionState(state: RTCPeerConnectionState): void {
    if (state === "connected") {
      this.clearDisconnectTimer();
      this.clearInitialConnectionTimer();
      this.resetAutomaticRecovery();
      return;
    }
    if (this.recoveryOwner === "route") {
      this.clearDisconnectTimer();
      if (state === "failed") this.reportRecoveryExhausted();
      return;
    }
    if (state === "failed") {
      this.clearDisconnectTimer();
      this.attemptAutomaticRecovery();
      return;
    }
    if (state === "disconnected" && this.disconnectTimer === null) {
      this.disconnectTimer = window.setTimeout(() => {
        this.disconnectTimer = null;
        if (this.state === "disconnected") this.attemptAutomaticRecovery();
      }, 3_000);
    }
  }

  private handleBridgeFailure(connectionId: string): void {
    if (this.disposed || this.failureNotified || this.connectionId !== connectionId) return;
    this.failureNotified = true;
    this.disposeBridge();
    this.sourceReady = false;
    this.sourceCodec = null;
    this.state = "failed";
    this.clearDisconnectTimer();
    // A retired local receiver cannot remain connected or regain that fact
    // from an already queued App event. Existing recovery owns the next edge.
    this.emit();
    void this.client.closeReceiver(this.sessionId, connectionId).catch(
      () => undefined,
    );
    this.onFailure();
    if (this.recoveryOwner === "route") {
      this.reportRecoveryExhausted();
      return;
    }
    this.attemptAutomaticRecovery(true);
  }

  private attemptAutomaticRecovery(forceRebuild = false): void {
    this.clearInitialConnectionTimer();
    if (
      this.disposed ||
      this.recoveryTimer !== null ||
      this.recoveryBudget.exhaustedReported ||
      !this.connectionId ||
      !this.parentPeerId
    ) {
      return;
    }
    if (
      this.recoveryBudget.requests >=
      VIEWER_MAX_AUTOMATIC_RECOVERY_REQUESTS
    ) {
      this.reportRecoveryExhausted();
      return;
    }
    const rebuild = forceRebuild || this.recoveryBudget.requests > 0;
    if (!this.events.sendRestartRequest(this.parentPeerId, this.connectionId, rebuild)) {
      this.reportRecoveryExhausted();
      return;
    }
    this.recoveryBudget.requests = rebuild
      ? VIEWER_MAX_AUTOMATIC_RECOVERY_REQUESTS
      : this.recoveryBudget.requests + 1;
    this.scheduleRecoveryDeadline();
  }

  private reportRecoveryExhausted(): void {
    if (
      this.recoveryBudget.exhaustedReported ||
      !this.parentPeerId ||
      !this.connectionId
    ) return;
    this.clearRecoveryTimer();
    const reported = this.events.onRecoveryExhausted?.(
      this.parentPeerId,
      this.connectionId,
    ) ?? true;
    if (reported) this.recoveryBudget.exhaustedReported = true;
    else this.scheduleRecoveryDeadline();
  }

  private scheduleInitialConnectionDeadline(connectionId: string): void {
    if (this.recoveryOwner !== "viewer") return;
    this.clearInitialConnectionTimer();
    const timer = window.setTimeout(() => {
      if (this.initialConnectionTimer !== timer) return;
      this.initialConnectionTimer = null;
      if (this.connectionId === connectionId && this.state !== "connected") {
        this.attemptAutomaticRecovery();
      }
    }, VIEWER_INITIAL_CONNECTION_TIMEOUT_MS);
    this.initialConnectionTimer = timer;
  }

  private scheduleRecoveryDeadline(): void {
    this.clearRecoveryTimer();
    this.recoveryTimer = window.setTimeout(() => {
      this.recoveryTimer = null;
      if (this.state !== "connected") this.attemptAutomaticRecovery();
      this.emit();
    }, VIEWER_AUTOMATIC_RECOVERY_TIMEOUT_MS);
  }

  private resetAutomaticRecovery(): void {
    this.clearRecoveryTimer();
    this.recoveryBudget.requests = 0;
    this.recoveryBudget.exhaustedReported = false;
  }

  private disposeBridge(): void {
    this.stopDecodedFrameProof();
    if (this.statsTimer !== null) {
      window.clearInterval(this.statsTimer);
      this.statsTimer = null;
    }
    this.statsInFlight = false;
    this.bridge?.dispose();
    this.bridge = null;
  }

  private emit(): void {
    if (!this.snapshot) return;
    this.snapshot = {
      ...this.snapshot,
      connectionState: this.state,
      iceConnectionState:
        this.state === "connected"
          ? "connected"
          : this.state === "failed"
            ? "failed"
            : this.state === "closed"
              ? "closed"
              : this.state === "disconnected"
                ? "disconnected"
                : "checking",
      error: this.state === "failed" ? { key: "native.fail.receiver" } : null,
    };
    this.events.onUpdate({
      ...this.snapshot,
      metrics: { ...this.snapshot.metrics },
    });
  }

  private clearTimers(): void {
    this.clearDisconnectTimer();
    this.clearInitialConnectionTimer();
    this.clearRecoveryTimer();
  }
  private clearDisconnectTimer(): void {
    if (this.disconnectTimer === null) return;
    window.clearTimeout(this.disconnectTimer);
    this.disconnectTimer = null;
  }
  private clearInitialConnectionTimer(): void {
    if (this.initialConnectionTimer === null) return;
    window.clearTimeout(this.initialConnectionTimer);
    this.initialConnectionTimer = null;
  }
  private clearRecoveryTimer(): void {
    if (this.recoveryTimer === null) return;
    window.clearTimeout(this.recoveryTimer);
    this.recoveryTimer = null;
  }

}
