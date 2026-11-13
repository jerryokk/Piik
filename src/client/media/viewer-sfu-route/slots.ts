import type {
  MediaRoutePhase,
  ParticipantRouteAssignment,
  ServerMessage,
  SfuSignalMessage,
} from "../../../shared/protocol";
import { SfuSubscriber } from "../../sfu/subscriber";
import type { ConnectionMetrics } from "../../types";
import { SfuQualityProbe } from "../candidate-quality-probe";
import {
  MediaRouteTransition,
  type RouteOperationToken,
} from "../route-transition";
import type {
  ViewerSfuRouteEvents,
  ViewerSubscriberSlot,
} from "./types";
import { disconnectSubscriber } from "./support";

/**
 * Owns the pending and active SFU subscriber slots: connection acceptance,
 * promotion, retirements, and failure reporting. Route-graph decisions stay in
 * ViewerSfuRoute.
 */
export class ViewerSfuRouteSlots {
  getRemoteInput(): RTCDataChannel | null { return this.active?.subscriber.getRemoteInput?.() ?? null; }
  protected readonly route = new MediaRouteTransition();
  protected pending: ViewerSubscriberSlot | null = null;
  protected active: ViewerSubscriberSlot | null = null;
  protected pendingPeer: {
    revision: number;
    connectionId: string;
    activationToken: RouteOperationToken | null;
  } | null = null;
  protected transitionTail: Promise<void> = Promise.resolve();
  protected resyncGeneration = 0;
  protected resyncing = false;
  protected paused = false;
  protected closed = false;

  constructor(
    protected viewerPeerId: string,
    protected readonly events: ViewerSfuRouteEvents,
  ) {}

  protected discardPending(): void {
    this.pendingPeer = null;
    this.events.preparePeer?.(null);
    this.events.prepareChild?.(null);
    this.clearPending();
  }

  async acceptConfig(
    message: Extract<ServerMessage, { type: "sfu-config" }>,
  ): Promise<void> {
    if (
      this.closed ||
      !this.route.acceptsConfig(message.revision)
    ) {
      return;
    }
    const token = this.route.token();
    const assignment = this.route.getPlannedAssignment();
    const phase = this.route.getPhase();
    const candidate = this.route.getPreparedCandidate();
    const publicationGeneration = assignment?.sfuPublicationGeneration;
    if (
      !token ||
      !phase ||
      assignment?.upstream.kind !== "sfu" ||
      !publicationGeneration ||
      message.publicationGeneration !== publicationGeneration ||
      (phase === "prepare" &&
        (candidate?.childPeerId !== this.viewerPeerId ||
          candidate.transport !== "sfu"))
    ) {
      return;
    }

    if (
      phase === "active" &&
      this.active?.publicationGeneration === publicationGeneration &&
      this.active.activated &&
      this.active.connectionId === message.connectionId
    ) {
      if (this.active.failed) {
        this.routeFailed(message.revision, "active", message.connectionId);
        return;
      }
      this.active.subscriber.updateConfig(message);
      await this.queueActiveRoute(token, true, this.paused);
      return;
    }
    if (
      this.pending?.revision === message.revision &&
      this.pending.publicationGeneration === publicationGeneration &&
      this.pending.connectionId === message.connectionId &&
      !this.pending.failed
    ) {
      this.pending.subscriber.updateConfig(message);
      return;
    }

    // Active config only describes an existing server-side connection. A new
    // local peer would have no offer; ask the graph owner to replace that edge.
    if (phase === "active") {
      this.routeFailed(message.revision, "active", message.connectionId);
      return;
    }

    this.clearPending();
    let slot: ViewerSubscriberSlot;
    const subscriberEvents = {
      send: this.events.send,
      onInputChanged: () => { if (this.active === slot) this.events.onInputChanged?.(); },
      onStream: (stream: MediaStream | null) => {
        if (stream) {
          this.handleStream(slot, stream);
        }
      },
      onVideoAvailability: (available: boolean) => {
        this.handleVideoAvailability(slot, available);
      },
      onStats: (metrics: ConnectionMetrics) => {
        if (this.active === slot && !slot.failed) {
          this.events.onSfuUpdate?.(metrics, slot.revision);
        } else if (
          this.pending === slot &&
          !slot.failed &&
          !slot.readySent &&
          this.route.getPhase() === "prepare" &&
          slot.qualityProbe
        ) {
          if (this.events.qualityProbeEligible?.() === false) {
            slot.qualityProbe.reset();
            slot.qualityResult = "pending";
          } else {
            slot.qualityResult = slot.qualityProbe.observe(
              this.events.currentPeerMetrics?.() ?? null,
              metrics,
            );
            if (slot.qualityResult === "approved") {
              this.sendPendingReady(slot);
            }
          }
        }
      },
      onFirstDecodedFrame: () => this.handlePendingDecodedFrame(slot),
      onState: (state: "connected" | "reconnecting") => {
        if (this.active === slot && !slot.failed) {
          this.events.onSfuState?.(state, slot.revision);
        } else if (this.pending === slot && !slot.failed && state === "connected") {
          this.sendPendingReady(slot);
          void this.queueTransition(() => this.promotePendingSfu(slot));
        }
      },
      onDisconnected: (failure?: "VIDEO_REJECTED") => this.failSubscriberSlot(slot, failure),
    };
    const subscriber =
      this.events.createSubscriber?.(subscriberEvents) ??
      new SfuSubscriber(subscriberEvents);
    const preparedCandidate = this.route.getPreparedCandidate();
    slot = {
      connectionId: message.connectionId,
      revision: message.revision,
      publicationGeneration,
      subscriber,
      connected: false,
      activated: false,
      stream: null,
      decodedFrame: false,
      readySent: false,
      qualityProbe:
        preparedCandidate?.transport === "sfu" &&
        preparedCandidate.qualityProbe
          ? new SfuQualityProbe()
          : null,
      qualityResult: "pending",
      failed: false,
      activationToken: null,
    };
    this.pending = slot;
    this.events.onSfuFailure?.(null, slot.revision);
    try {
      const connected = await subscriber.connect(message);
      if (!connected) {
        const currentToken = (this.route.owns(token) ? token : null);
        if (this.pending === slot && currentToken) {
          slot.revision = currentToken.revision;
          this.handleFailure(slot);
        } else {
          if (this.pending === slot) {
            this.pending = null;
          }
          slot.failed = true;
        }
        await disconnectSubscriber(subscriber);
        return;
      }
      const currentToken = (this.route.owns(token) ? token : null);
      if (this.pending !== slot || !currentToken) {
        if (this.pending === slot) {
          this.pending = null;
        }
        slot.failed = true;
        await disconnectSubscriber(subscriber);
        return;
      }
      slot.revision = currentToken.revision;
      slot.connected = true;
      if (!subscriber.activate()) {
        this.failSubscriberSlot(slot);
        await disconnectSubscriber(subscriber);
        return;
      }
      slot.activated = true;
      if (!this.paused) {
        subscriber.armDecodedFrameProof();
      }
    } catch {
      this.failSubscriberSlot(slot);
      await disconnectSubscriber(subscriber);
    }
  }

  async acceptSignal(message: SfuSignalMessage): Promise<void> {
    const slot = [this.pending, this.active].find((slot) =>
      slot && !slot.failed && slot.connectionId === message.connectionId &&
      slot.publicationGeneration === message.publicationGeneration,
    );
    if (slot === this.pending && !slot?.activationToken && slot?.revision !== message.revision) return;
    await slot?.subscriber.acceptSignal(message);
  }

  protected preparePeer(assignment: ParticipantRouteAssignment): void {
    this.events.preparePeer?.(
      assignment.upstream.kind === "peer" ? assignment : null,
    );
  }

  protected queueActiveRoute(
    token: RouteOperationToken,
    acknowledge: boolean,
    retireUnreferenced = false,
  ): Promise<void> {
    const pendingPeer = this.pendingPeer;
    const pendingSfu = this.pending;
    return this.queueTransition(async () => {
      if (this.closed || this.resyncing || !this.route.ownsActive(token) ||
        (pendingPeer?.activationToken && this.pendingPeer !== pendingPeer) ||
        (pendingSfu?.activationToken && this.pending !== pendingSfu && this.active !== pendingSfu)) {
        return;
      }
      const assignment = this.route.getActiveAssignment();
      if (!assignment) {
        return;
      }
      const assignedPublicationGeneration =
        assignment.upstream.kind === "sfu"
          ? assignment.sfuPublicationGeneration
          : null;
      if (
        retireUnreferenced &&
        this.active?.publicationGeneration !== assignedPublicationGeneration
      ) {
        await this.retireActive();
        if (!this.route.ownsActive(token)) {
          return;
        }
      }
      if (assignment.upstream.kind === "sfu") {
        const mediaAssignment = this.route.getMediaAssignment();
        if (
          mediaAssignment?.upstream.kind === "sfu" &&
          this.active?.publicationGeneration ===
            assignment.sfuPublicationGeneration &&
          this.active?.activated &&
          !this.active.failed &&
          this.pending?.publicationGeneration !== assignment.sfuPublicationGeneration
        ) {
          if (this.pending === pendingSfu) this.clearPending();
          this.active.revision = token.revision;
          this.active.subscriber.updateConfig(this.active);
          this.commitMedia(token);
          return;
        }
        await this.activateSfu(token, acknowledge);
        return;
      }

      const promoted =
        pendingPeer?.activationToken && this.route.ownsActive(pendingPeer.activationToken)
          ? await this.events.activatePeer(assignment, pendingPeer.revision)
          : false;
      if (pendingPeer) {
        if (this.pendingPeer !== pendingPeer) return;
        if (pendingPeer.activationToken) token = pendingPeer.activationToken;
      }
      if (!this.route.ownsActive(token)) {
        return;
      }
      if (promoted === "waiting") return;
      if (pendingPeer?.activationToken && promoted !== true) {
        // Readiness may reach the server before a candidate's local failure.
        // Once committed, report against the exact active connection for recovery.
        this.routeFailed(token.revision, "active", pendingPeer.connectionId);
        return;
      }
      if (this.pendingPeer === pendingPeer) this.pendingPeer = null;
      if (this.pending === pendingSfu) this.clearPending();
      if (this.active) {
        await this.retireActive();
      }
      if (!this.route.ownsActive(token)) {
        return;
      }
      if (!promoted) {
        await this.events.activatePeer(assignment);
      }
      if (!this.route.ownsActive(token)) {
        return;
      }
      this.commitMedia(token);
    });
  }

  protected queueTransition(work: () => Promise<void>): Promise<void> {
    const next = this.transitionTail.then(work, work);
    this.transitionTail = next.catch(() => undefined);
    return this.transitionTail;
  }

  protected activateChildren(childPeerIds: readonly string[], revision: number): void {
    if (this.events.activateChildren) {
      this.events.activateChildren(childPeerIds, revision);
    } else {
      this.events.reconcileSfuChildren?.([...childPeerIds]);
    }
  }

  protected async activateSfu(
    token: RouteOperationToken,
    acknowledge: boolean,
  ): Promise<void> {
    const assignment = this.route.getActiveAssignment();
    const publicationGeneration = assignment?.sfuPublicationGeneration;
    if (this.active?.failed && this.active.publicationGeneration === publicationGeneration && !this.pending) {
      this.routeFailed(token.revision, "active", this.active.connectionId);
      return;
    }
    if (
      this.active?.revision === token.revision &&
      this.active.publicationGeneration === publicationGeneration &&
      this.active.activated && this.pending === null
    ) {
      return;
    }
    const pending = this.pending;
    if (
      !pending ||
      pending.revision !== token.revision ||
      pending.publicationGeneration !== publicationGeneration ||
      !pending.connected
    ) {
      if (
        acknowledge &&
        this.route.ownsActive(token) &&
        pending === null
      ) {
        this.events.send({ type: "refresh-sfu", revision: token.revision });
      }
      return;
    }
    await this.promotePendingSfu(pending);
  }

  protected handleStream(slot: ViewerSubscriberSlot, stream: MediaStream): void {
    if (this.closed || slot.failed || !slot.activated) {
      return;
    }
    slot.stream = stream;
    if (this.active === slot) {
      const assignment = this.route.getMediaAssignment();
      if (assignment?.upstream.kind === "sfu") {
        this.events.onSfuStream(stream, assignment, false, slot.revision);
      }
      return;
    }
    void this.queueTransition(() => this.promotePendingSfu(slot));
  }

  protected handlePendingDecodedFrame(slot: ViewerSubscriberSlot): boolean {
    if (this.closed || slot.failed || this.pending !== slot) {
      return true;
    }
    if (this.paused || !slot.activated) {
      return false;
    }
    const committed = slot.activationToken && this.route.ownsActive(slot.activationToken);
    const assignment = committed ? this.route.getActiveAssignment() : this.route.getPlannedAssignment();
    const phase = committed ? "active" : this.route.getPhase();
    const revision = committed ? this.route.getActiveRevision() : this.route.getRevision();
    if (
      revision !== slot.revision ||
      assignment?.upstream.kind !== "sfu" ||
      assignment.sfuPublicationGeneration !== slot.publicationGeneration ||
      phase === null
    ) {
      return true;
    }
    slot.decodedFrame = true;
    if (phase === "prepare") {
      this.sendPendingReady(slot);
      return slot.qualityProbe !== null || slot.readySent;
    }
    void this.queueTransition(() => this.promotePendingSfu(slot));
    return true;
  }

  protected sendPendingReady(slot: ViewerSubscriberSlot): void {
    if (
      this.pending !== slot ||
      slot.failed ||
      slot.readySent ||
      !slot.subscriber.isConnected() ||
      !slot.decodedFrame ||
      (slot.qualityProbe !== null && slot.qualityResult !== "approved") ||
      (slot.qualityProbe !== null &&
        this.events.qualityProbeEligible?.() === false) ||
      this.route.getPhase() !== "prepare" ||
      this.route.getRevision() !== slot.revision
    ) {
      return;
    }
    slot.readySent = this.events.send({
      type: "route-ready",
      revision: slot.revision,
      phase: "prepare",
    });
  }

  protected async promotePendingSfu(slot: ViewerSubscriberSlot): Promise<void> {
    const token = slot.activationToken;
    const assignment = this.route.getActiveAssignment();
    if (
      this.pending !== slot ||
      !slot.subscriber.isConnected() ||
      !slot.stream ||
      !slot.decodedFrame ||
      !token ||
      !this.route.ownsActive(token) ||
      assignment?.upstream.kind !== "sfu"
    ) {
      return;
    }
    slot.subscriber.stopDecodedFrameProof();
    this.pending = null;
    const previous = this.active;
    this.active = slot;
    if (!this.route.markMediaActive(token)) {
      this.active = previous;
      slot.failed = true;
      await disconnectSubscriber(slot.subscriber);
      return;
    }
    this.events.onSfuStream(slot.stream, assignment, true, slot.revision);
    if (previous && previous !== slot) {
      previous.failed = true;
      try {
        previous.subscriber.deactivate();
      } catch {
        // Disconnect remains the fail-closed cleanup path.
      }
      await disconnectSubscriber(previous.subscriber);
    }
  }

  protected handleVideoAvailability(
    slot: ViewerSubscriberSlot,
    available: boolean,
  ): void {
    if (
      this.closed ||
      this.active !== slot ||
      slot.failed ||
      !slot.activated ||
      this.route.getMediaAssignment()?.upstream.kind !== "sfu"
    ) {
      return;
    }
    this.events.onSfuVideoAvailability?.(available, slot.revision);
  }

  protected commitMedia(token: RouteOperationToken): void {
    if (!this.route.markMediaActive(token)) {
      return;
    }
  }

  protected clearPending(): void {
    if (!this.pending) {
      return;
    }
    const pending = this.pending;
    this.pending = null;
    pending.failed = true;
    void disconnectSubscriber(pending.subscriber);
  }

  protected async retireActive(): Promise<void> {
    if (!this.active) {
      return;
    }
    const active = this.active;
    this.active = null;
    active.failed = true;
    this.events.onSfuUpdate?.(null, active.revision);
    await this.disconnectRetiredSubscriber(active);
  }

  protected detachUnreferencedActive(
    assignment: ParticipantRouteAssignment,
  ): ViewerSubscriberSlot | null {
    const publicationGeneration =
      assignment.upstream.kind === "sfu"
        ? assignment.sfuPublicationGeneration
        : null;
    if (
      !this.active ||
      this.active.publicationGeneration === publicationGeneration
    ) {
      return null;
    }
    const active = this.active;
    this.active = null;
    active.failed = true;
    this.events.onSfuUpdate?.(null, active.revision);
    return active;
  }

  protected async disconnectRetiredSubscriber(
    active: ViewerSubscriberSlot,
  ): Promise<void> {
    if (active.activated) {
      try {
        active.subscriber.deactivate();
      } catch {
        // Disconnect remains the fail-closed cleanup path.
      }
    }
    await disconnectSubscriber(active.subscriber);
  }

  protected handleFailure(slot: ViewerSubscriberSlot, failure?: "VIDEO_REJECTED"): void {
    if (this.closed || slot.failed) {
      return;
    }
    const wasActive = this.active === slot;
    const wasPending = this.pending === slot;
    if (!wasActive && !wasPending) {
      slot.failed = true;
      return;
    }
    this.events.onSfuFailure?.(failure ?? null, slot.revision);
    slot.failed = true;
    slot.subscriber.stopDecodedFrameProof();
    if (this.pending === slot) {
      this.pending = null;
    }
    if (wasActive) {
      this.events.onSfuUpdate?.(null, slot.revision);
    }

    const assignment = this.route.getPlannedAssignment();
    const revision = this.route.getRevision();
    const phase = this.route.getPhase();
    const matchesPlannedRoute =
      revision === slot.revision &&
      assignment?.upstream.kind === "sfu" &&
      assignment.sfuPublicationGeneration === slot.publicationGeneration;
    if (revision === null || phase === null) {
      return;
    }
    if (wasActive || (slot.activationToken && this.route.ownsActive(slot.activationToken))) {
      const activeRevision = this.route.getActiveRevision();
      const activeAssignment = this.route.getActiveAssignment();
      if (
        activeRevision !== null &&
        activeAssignment?.upstream.kind === "sfu" &&
        activeAssignment.sfuPublicationGeneration === slot.publicationGeneration
      ) {
        this.routeFailed(activeRevision, "active", slot.connectionId);
      }
    } else if (matchesPlannedRoute) {
      this.routeFailed(revision, phase, phase === "active" ? slot.connectionId : null);
    }
  }

  protected failSubscriberSlot(slot: ViewerSubscriberSlot, failure?: "VIDEO_REJECTED"): void {
    if (this.pending !== slot && this.active !== slot) {
      slot.failed = true;
      return;
    }
    this.handleFailure(slot, failure);
  }

  protected routeFailed(
    revision: number,
    phase: MediaRoutePhase,
    connectionId: string | null = null,
  ): boolean {
    return this.events.send({
      type: "route-failed",
      revision,
      phase,
      connectionId,
    });
  }
}
