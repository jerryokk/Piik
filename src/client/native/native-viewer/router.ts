import type { IceConfig, SignalPayload } from "../../../shared/protocol";
import {
  type ViewerMediaPeer,
  VIEWER_MAX_PENDING_CANDIDATES,
  ViewerPeer,
  type ViewerPeerEvents,
  type ViewerPeerOptions,
  createViewerRecoveryBudget,
} from "../../webrtc/viewer-peer";
import type { NativeClient } from "../client";
import { NativePunchReceiver, punchIceConfig } from "../native-punch";
import { nativeRelayCapacity } from "../wire";
import { NativeViewerPeer } from "./backend";
import { offerHasNativeVideoCodec } from "./codec";
import type { NativeViewerSource, PeerIceConfig } from "./types";

export class NativeCapableViewerPeer implements ViewerMediaPeer {
  private backend: ViewerMediaPeer | null = null;
  private readonly pendingCandidates = new Map<
    string,
    Array<Extract<SignalPayload, { kind: "candidate" }>>
  >();
  private disposed = false;
  private identity: { parentPeerId: string; connectionId: string } | null = null;
  private signalTail: Promise<void> = Promise.resolve();
  private currentIceConfig: PeerIceConfig;
  private punch: NativePunchReceiver | null = null;

  constructor(
    iceConfig: PeerIceConfig,
    private readonly events: ViewerPeerEvents,
    private options: ViewerPeerOptions,
    private readonly acquireNativeClient: () => Promise<NativeClient | null>,
    private readonly onNativeUnavailable: () => void,
    private readonly nativeSessionId: string,
    private readonly edgeCapacity: number,
  ) {
    this.currentIceConfig = iceConfig;
    this.options = {
      ...options,
      recoveryBudget: options.recoveryBudget ?? createViewerRecoveryBudget(),
    };
  }

  acceptSignal(parentPeerId: string, payload: SignalPayload): Promise<void> {
    if (payload.kind === "punch") {
      if (this.disposed) return Promise.resolve();
      const survey = this.options.natPredictionEnabled && this.options.recoveryOwner === "route"
        ? punchIceConfig(this.currentIceConfig) : null;
      if (payload.punch.phase === "prepare") {
        if (this.identity?.connectionId === payload.connectionId) return Promise.resolve();
        if (!survey) {
          this.events.sendSignal(parentPeerId, { kind: "punch", connectionId: payload.connectionId, punch: { phase: "ready" } });
        } else if (!this.punch) {
          this.punch = new NativePunchReceiver(parentPeerId, payload.connectionId, this.nativeSessionId,
            survey, this.edgeCapacity, this.acquireNativeClient,
            signal => !this.disposed && this.events.sendSignal(parentPeerId, signal));
          void this.punch.prepare(payload.punch.secret);
        }
      } else if (payload.punch.phase === "start" && this.punch?.peerId === parentPeerId &&
        this.punch.connectionId === payload.connectionId) {
        void this.punch.run(payload.punch.prediction);
      }
      return Promise.resolve();
    }
    const work = async () => {
      if (this.disposed) return;
      if (
        payload.kind === "candidate" &&
        (!this.backend ||
          this.identity?.parentPeerId !== parentPeerId ||
          this.identity.connectionId !== payload.connectionId)
      ) {
        this.queueCandidate(payload);
        return;
      }
      if (payload.kind === "description" && payload.description.type === "offer") {
        const nextIdentity = {
          parentPeerId,
          connectionId: payload.connectionId,
        };
        if (
          !this.identity ||
          this.identity.parentPeerId !== parentPeerId ||
          this.identity.connectionId !== payload.connectionId
        ) {
          this.backend?.dispose();
          this.backend = null;
          if (this.identity && this.identity.parentPeerId !== parentPeerId) {
            this.options = { ...this.options, recoveryBudget: createViewerRecoveryBudget() };
          }
          this.identity = nextIdentity;
        }
        const preparation = this.punch;
        this.punch = null;
        const committed = payload.punched && preparation?.peerId === parentPeerId &&
          preparation.connectionId === payload.connectionId && preparation.commit();
        if (!committed) {
          preparation?.dispose();
          if (payload.punched) {
            // Adopt the offer identity before reporting failure: the route
            // owner ignores reports from a peer that does not own this edge.
            // Expiry does not disable the App's media capability.
            this.events.onRecoveryExhausted?.(parentPeerId, payload.connectionId);
            return;
          }
        }
        if (!this.backend) {
          const client = offerHasNativeVideoCodec(payload.description.sdp)
            ? await this.acquireNativeClient().catch(() => null)
            : null;
          if (this.disposed) return;
          this.backend = client
            ? new NativeViewerPeer(
                this.currentIceConfig,
                this.events,
                this.options,
                client,
                this.nativeSessionId,
                nativeRelayCapacity(this.edgeCapacity, client.health),
                this.onNativeUnavailable,
              )
            : this.newBrowserPeer();
        }
        await this.backend.acceptSignal(parentPeerId, payload);
        const pending = this.pendingCandidates.get(payload.connectionId) ?? [];
        this.pendingCandidates.delete(payload.connectionId);
        for (const candidate of pending) {
          if (this.disposed) return;
          await this.backend.acceptSignal(parentPeerId, candidate);
        }
        return;
      }
      await this.backend?.acceptSignal(parentPeerId, payload);
    };
    const result = this.signalTail.then(work, work);
    this.signalTail = result.catch(() => undefined);
    return result;
  }

  updateIceConfig(iceConfig: IceConfig): void {
    this.currentIceConfig = iceConfig;
    this.backend?.updateIceConfig(iceConfig);
  }

  requestRecovery(rebuild = false): boolean {
    return this.backend?.requestRecovery(rebuild) ?? false;
  }

  isRecovering(): boolean { return this.backend?.isRecovering() ?? false; }
  getRemoteInput(): RTCDataChannel | null { return this.backend?.getRemoteInput?.() ?? null; }
  hasConnection(): boolean { return this.backend?.hasConnection() ?? this.identity !== null; }
  hasConnectionId(connectionId: string): boolean {
    return this.backend?.hasConnectionId(connectionId) ??
      this.identity?.connectionId === connectionId;
  }
  getConnectionIdentity(): { parentPeerId: string; connectionId: string } | null {
    return this.backend?.getConnectionIdentity() ?? this.identity;
  }
  isConnected(): boolean { return this.backend?.isConnected() ?? false; }
  get nativeSource(): NativeViewerSource | null {
    return this.backend instanceof NativeViewerPeer
      ? this.backend.source
      : null;
  }
  stopDecodedFrameProof(): void { this.backend?.stopDecodedFrameProof(); }
  activatePreparedRoute(): void {
    // Promotion belongs to this route, including any later replacement backend.
    this.options = { ...this.options, recoveryOwner: "viewer" };
    this.backend?.activatePreparedRoute();
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.punch?.dispose();
    this.punch = null;
    this.backend?.dispose();
    this.backend = null;
    this.identity = null;
    this.pendingCandidates.clear();
  }

  private queueCandidate(
    payload: Extract<SignalPayload, { kind: "candidate" }>,
  ): void {
    let queue = this.pendingCandidates.get(payload.connectionId);
    if (!queue) {
      if (this.pendingCandidates.size >= 2) {
        const oldest = this.pendingCandidates.keys().next().value;
        if (oldest) this.pendingCandidates.delete(oldest);
      }
      queue = [];
      this.pendingCandidates.set(payload.connectionId, queue);
    }
    if (queue.length < VIEWER_MAX_PENDING_CANDIDATES) queue.push(payload);
  }

  private newBrowserPeer(): ViewerPeer {
    return new ViewerPeer(this.currentIceConfig, this.events, this.options);
  }
}
