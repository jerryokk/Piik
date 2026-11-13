import { INPUT_CHANNEL } from "../../remote-control/input-route";
import type { CopyKey } from "../../ui/copy";
import type { IceConfig, SignalPayload } from "../../../shared/protocol";
import { createOpaqueId } from "../../lib/util/opaque-id";
import { waitForConnectionOperation } from "../connection-operation";
import type { BrowserEncodingPool } from "../../media/browser-encoding-pool";
import { supportsBrowserEncoding } from "../../media/browser-encoding-output";
import { debugError } from "../../lib/debug";
import { observeDebugConnection } from "../../lib/debug/webrtc";
import { cloneSenderVideoTrack, stopSenderVideoTrack } from "../../media/sender-video-track";
import { needsStartupVideoProfile } from "../../media/quality";
import { retireSenderQualityEvidence } from "../../media/sender-quality-evidence";
import { EMPTY_METRICS, type PeerSnapshot } from "../../types";
import {
  normalizeVideoOfferSdp,
  type BrowserVideoCodecPreference,
  VP8_ONLY_VIDEO_CODEC,
} from "../video-codec";
import {
  addRemoteIceCandidate,
  iceServersWithNatPrediction,
  natPredictionSurveyUrls,
  NatPredictionCandidateEmitter,
  type SignalCandidate,
} from "../nat-prediction";
import type { QualityProfile } from "../../media/quality";
import type { StatsAccumulator } from "../stats";
import { createStatsAccumulator } from "../stats";
import type { HostPeerEvents, PeerIceConfig } from "./types";

const MAX_PENDING_CANDIDATES = 64;
// An audio-direction change must not wait forever for a lost SDP answer.
// Initial acquisition and ICE restart keep their existing route/recovery owners.
export const RENEGOTIATION_ANSWER_TIMEOUT_MS = 15_000;

/**
 * Host direct-edge connection owner: peer-connection allocation, signaling,
 * negotiation epochs and lifecycle. Sender plumbing and quality sampling are
 * implemented by the layers above it.
 */
export abstract class HostPeerConnection {
  readonly connectionId: string;

  protected readonly connection: RTCPeerConnection;
  private readonly pendingCandidates: SignalCandidate[] = [];
  protected readonly natPredictionEnabled: boolean;
  private readonly localIceCandidates: NatPredictionCandidateEmitter;
  protected readonly encodedStreamsEnabled: boolean;
  protected readonly lifetime = new AbortController();
  private readonly releaseDebug: () => void;
  protected get disposed(): boolean { return this.lifetime.signal.aborted; }
  protected profileRevision = 0;
  private negotiationEpoch = 0;
  private ordinaryAnswerEpoch: number | null = null;
  private answerTimer: number | null = null;
  private pendingNegotiation = false;
  private negotiationTail: Promise<void> = Promise.resolve();
  protected startupVideoProfilePending: boolean;
  protected connectedOnce = false;
  private awaitingReconnect = false;
  protected paused = false;
  protected snapshot: PeerSnapshot;
  protected senderVideoTrack: MediaStreamTrack | null;
  // Sampling state is owned by the quality-reporting layer; source changes
  // reset it here because they invalidate any in-flight sample.
  protected statsAccumulator: StatsAccumulator = createStatsAccumulator();
  protected statsSamplingBlocked = false;
  protected startupFramesBaseline: number | null = 0;

  constructor(
    readonly peerId: string,
    iceConfig: PeerIceConfig,
    protected stream: MediaStream,
    protected desiredProfile: QualityProfile,
    protected readonly events: HostPeerEvents,
    protected readonly videoCodec: BrowserVideoCodecPreference =
      VP8_ONLY_VIDEO_CODEC,
    connectionId = createOpaqueId(),
    natPredictionEnabled = false,
    protected readonly videoPool: BrowserEncodingPool | null = null,
  ) {
    this.connectionId = connectionId;
    this.natPredictionEnabled = natPredictionEnabled;
    const sourceVideoTrack = stream.getVideoTracks()[0] ?? null;
    this.paused = sourceVideoTrack?.enabled === false;
    this.senderVideoTrack = sourceVideoTrack
      ? this.cloneVideoTrack(sourceVideoTrack)
      : null;
    this.startupVideoProfilePending = needsStartupVideoProfile(desiredProfile);
    this.encodedStreamsEnabled = !!videoPool && supportsBrowserEncoding();
    let allocatedConnection: RTCPeerConnection | null = null;
    try {
      this.connection = allocatedConnection = new RTCPeerConnection({
        ...(this.encodedStreamsEnabled ? { encodedInsertableStreams: true } : {}),
        iceServers: iceServersWithNatPrediction(
          iceConfig.iceServers,
          this.natPredictionEnabled,
          iceConfig.natPredictionStunUrls,
        ),
      });
      if (events.onInputChannel) events.onInputChannel(this.connection.createDataChannel(INPUT_CHANNEL), this.connection);
    } catch (error) {
      allocatedConnection?.close();
      stopSenderVideoTrack(this.senderVideoTrack);
      throw error;
    }
    this.releaseDebug = observeDebugConnection(this.connection, { connectionId, peerId, role: "send" });
    this.localIceCandidates = new NatPredictionCandidateEmitter(
      this.natPredictionEnabled,
      (candidate) => this.sendIceCandidate(candidate),
      natPredictionSurveyUrls(
        iceConfig.iceServers,
        iceConfig.natPredictionStunUrls,
      ),
    );
    this.snapshot = {
      peerId,
      connectionId: this.connectionId,
      connectionState: this.connection.connectionState,
      iceConnectionState: this.connection.iceConnectionState,
      metrics: { ...EMPTY_METRICS },
      error: null,
      senderParameters: null,
      audioSenderParameters: null,
    };
    this.bindConnectionEvents();
  }

  async acceptSignal(payload: SignalPayload): Promise<void> {
    if (this.disposed || payload.connectionId !== this.connectionId || payload.kind === "punch") {
      return;
    }

    try {
      if (payload.kind === "description") {
        if (payload.description.type !== "answer") {
          return;
        }
        await this.enqueueNegotiation(() => this.acceptAnswer(payload));
      } else {
        await this.enqueueNegotiation(async () => {
          if (this.disposed) return;
          // Native trickle events may precede a restart answer. The retained
          // remote description still belongs to the previous ICE generation.
          if (this.ordinaryAnswerEpoch !== null || !this.connection.remoteDescription) {
            if (this.pendingCandidates.length < MAX_PENDING_CANDIDATES) {
              this.pendingCandidates.push(payload.candidate);
            }
          } else {
            await this.waitForOperation(() => addRemoteIceCandidate(this.connection, payload.candidate));
          }
        });
      }
    } catch (error) {
      this.setError(error, "host.err.createConnection");
    }
  }

  async restartIce(): Promise<boolean> {
    return this.enqueueNegotiation(async () => {
      if (
        this.disposed ||
        this.ordinaryAnswerEpoch !== null ||
        this.connection.signalingState !== "stable"
      ) {
        return false;
      }
      return this.createOwnedOffer(true, this.nextNegotiationEpoch());
    });
  }

  isConnected(): boolean {
    return this.connection.connectionState === "connected";
  }

  getSnapshot(): PeerSnapshot {
    return { ...this.snapshot, metrics: { ...this.snapshot.metrics } };
  }

  updateIceConfig(iceConfig: IceConfig): void {
    if (this.disposed) {
      return;
    }
    try {
      this.connection.setConfiguration({
        iceServers: iceServersWithNatPrediction(
          iceConfig.iceServers,
          this.natPredictionEnabled,
          iceConfig.natPredictionStunUrls,
        ),
      });
      this.localIceCandidates.setSurveyUrls(
        natPredictionSurveyUrls(
          iceConfig.iceServers,
          iceConfig.natPredictionStunUrls,
        ),
      );
    } catch (error) {
      this.setError(error, "host.err.createConnection");
    }
  }

  dispose(): void {
    if (this.disposed) {
      return;
    }
    this.releaseDebug();
    this.lifetime.abort();
    retireSenderQualityEvidence(this.connectionId);
    this.nextNegotiationEpoch();
    this.ordinaryAnswerEpoch = null;
    this.pendingNegotiation = false;
    this.stopStatsSampling();
    this.connection.close();
    this.retireSenderResources();
    this.localIceCandidates.discard();
  }

  /** Release sender-owned resources with the connection. */
  protected abstract retireSenderResources(): void;
  /** Clear the quality-sampling timer owned by the stats layer. */
  protected abstract stopStatsSampling(): void;
  /** Re-apply sender quality after an ICE/connection replay. */
  protected abstract replayProfileAfterReconnect(replay: boolean): void;
  /** The remote answer can change a negotiated audio ceiling. */
  protected abstract reconcileNegotiatedAudio(): void;

  protected cloneVideoTrack(source: MediaStreamTrack): MediaStreamTrack {
    return cloneSenderVideoTrack(source, (track) => {
      if (this.senderVideoTrack === track) this.fail();
    });
  }

  private bindConnectionEvents(): void {
    this.connection.addEventListener("icecandidate", (event) => {
      this.localIceCandidates.add(event.candidate);
    });
    this.connection.addEventListener("icegatheringstatechange", () => {
      if (
        this.natPredictionEnabled &&
        this.connection.iceGatheringState === "complete"
      ) {
        this.localIceCandidates.gatheringComplete();
      }
    });
    this.connection.addEventListener("connectionstatechange", () => {
      const state = this.connection.connectionState;
      if (state === "connected") {
        const replayProfile = this.awaitingReconnect;
        this.connectedOnce = true;
        this.awaitingReconnect = false;
        this.replayProfileAfterReconnect(replayProfile);
      } else if (this.connectedOnce && state !== "closed") {
        this.awaitingReconnect = true;
      }
      this.emit();
    });
    this.connection.addEventListener("iceconnectionstatechange", () => this.emit());
  }

  private sendIceCandidate(candidate: SignalCandidate | null): void {
    this.events.sendSignal(this.peerId, {
      kind: "candidate",
      connectionId: this.connectionId,
      candidate,
    });
  }

  protected waitForOperation<T>(operation: () => Promise<T>): Promise<T> {
    return waitForConnectionOperation(this.lifetime.signal, operation);
  }

  private enqueueNegotiation<T>(operation: () => Promise<T>): Promise<T> {
    const result = this.negotiationTail.then(operation, operation);
    this.negotiationTail = result.then(
      () => undefined,
      () => undefined,
    );
    return result;
  }

  protected createOffer(): Promise<boolean> {
    return this.enqueueNegotiation(async () => {
      if (this.disposed) {
        return false;
      }
      // Source/microphone changes may arrive before the current answer. Keep
      // one offer in flight: another offer would let its predecessor's answer
      // consume the newer epoch. The next offer reads the latest transceivers.
      if (this.ordinaryAnswerEpoch !== null) {
        this.pendingNegotiation = true;
        return true;
      }
      return this.createOwnedOffer(false, this.nextNegotiationEpoch());
    });
  }

  private async createOwnedOffer(
    restart: boolean,
    epoch: number,
  ): Promise<boolean> {
    if (!this.ownsLocalOffer(epoch)) {
      return false;
    }
    if (!restart && this.connection.remoteDescription) {
      this.answerTimer = window.setTimeout(() => {
        if (this.ownsLocalOffer(epoch)) this.fail(new Error("SDP renegotiation answer timed out"));
      }, RENEGOTIATION_ANSWER_TIMEOUT_MS);
    }
    try {
      if (restart) {
        this.connection.restartIce();
      }
      const offer = await this.waitForOperation(() => this.connection.createOffer());
      if (!this.ownsLocalOffer(epoch)) {
        return false;
      }
      await this.waitForOperation(() => this.connection.setLocalDescription(offer));
      if (
        !this.ownsLocalOffer(epoch) ||
        !this.connection.localDescription
      ) {
        return false;
      }
      this.ordinaryAnswerEpoch = epoch;
      if (
        !this.events.sendSignal(this.peerId, {
          kind: "description",
          connectionId: this.connectionId,
          description: {
            type: "offer",
            sdp: normalizeVideoOfferSdp(this.connection.localDescription.sdp),
          },
        })
      ) {
        throw new Error("signal send rejected");
      }
      this.snapshot = { ...this.snapshot, error: null };
      this.emit();
      return true;
    } catch (error) {
      if (!this.ownsLocalOffer(epoch)) {
        return false;
      }
      if (this.ordinaryAnswerEpoch === epoch) {
        this.ordinaryAnswerEpoch = null;
      }
      this.clearAnswerTimer();
      this.setError(error, "host.err.createConnection");
      return false;
    }
  }

  private async flushCandidates(): Promise<void> {
    const candidates = this.pendingCandidates.splice(0);
    for (const candidate of candidates) {
      await this.waitForOperation(() => addRemoteIceCandidate(this.connection, candidate));
    }
  }

  private async acceptAnswer(
    payload: Extract<SignalPayload, { kind: "description" }>,
  ): Promise<void> {
    const epoch = this.ordinaryAnswerEpoch;
    if (epoch === null || !this.ownsAnswer(epoch)) {
      return;
    }
    try {
      await this.waitForOperation(() => this.connection.setRemoteDescription(payload.description));
      if (!this.ownsAnswer(epoch)) {
        return;
      }
      this.reconcileNegotiatedAudio();
      await this.flushCandidates();
      if (this.ownsAnswer(epoch)) {
        this.clearAnswerTimer();
        this.ordinaryAnswerEpoch = null;
        if (this.pendingNegotiation) {
          this.pendingNegotiation = false;
          // Already inside negotiationTail; enqueueing here would deadlock.
          if (!(await this.createOwnedOffer(false, this.nextNegotiationEpoch()))) {
            this.fail();
          }
        }
      }
    } catch (error) {
      if (!this.ownsAnswer(epoch)) {
        return;
      }
      this.fail(error);
    }
  }

  private ownsAnswer(epoch: number): boolean {
    return (
      !this.disposed &&
      epoch === this.negotiationEpoch &&
      this.ordinaryAnswerEpoch === epoch
    );
  }

  private ownsLocalOffer(epoch: number): boolean {
    return !this.disposed && epoch === this.negotiationEpoch;
  }

  private nextNegotiationEpoch(): number {
    this.clearAnswerTimer();
    this.ordinaryAnswerEpoch = null;
    this.negotiationEpoch += 1;
    return this.negotiationEpoch;
  }

  private clearAnswerTimer(): void {
    if (this.answerTimer !== null) window.clearTimeout(this.answerTimer);
    this.answerTimer = null;
  }

  protected setError(error: unknown, key: CopyKey): void {
    if (this.disposed) return;
    debugError("webrtc", "sender-failed", error, { connectionId: this.connectionId, reason: key });
    this.snapshot = { ...this.snapshot, error: { key } };
    this.emit();
  }

  protected fail(error?: unknown): void {
    if (this.disposed) return;
    debugError("webrtc", "sender-media-failed", error, { connectionId: this.connectionId });
    this.snapshot = { ...this.snapshot, connectionState: "failed", error: { key: "host.fail.connection" } };
    this.dispose();
    // Expected disposal is silent; terminal media failure must still reach
    // its preparation/recovery owner after owned resources have retired.
    this.events.onUpdate(this.getSnapshot());
  }

  protected emit(): void {
    if (this.disposed) {
      return;
    }
    this.snapshot = {
      ...this.snapshot,
      connectionState: this.connection.connectionState,
      iceConnectionState: this.connection.iceConnectionState,
    };
    this.events.onUpdate({
      ...this.snapshot,
      metrics: { ...this.snapshot.metrics },
    });
  }
}
