import {
  reportActivePeerRouteFailure,
  type RouteUpdateInput,
  type RouteUpdateResult,
} from "../route-transition";
import { ViewerSfuRouteSlots } from "./slots";
import { disconnectSubscriber, sameUpstream } from "./support";

/**
 * Viewer-side SFU route: accepts authoritative route updates, then delegates
 * subscriber connection, promotion and teardown to ViewerSfuRouteSlots.
 */
export class ViewerSfuRoute extends ViewerSfuRouteSlots {
  accept(update: RouteUpdateInput, acknowledge = true): RouteUpdateResult {
    if (this.closed) {
      return "stale";
    }
    const previousActiveAssignment = this.route.getActiveAssignment();
    const result = this.route.accept(update);
    if (result === "stale") {
      return result;
    }
    if (this.active) {
      this.active.subscriber.updateConfig({ ...this.active, revision: update.revision });
    }
    const pausedActiveReconciliation =
      this.paused && update.phase === "active" && result !== "duplicate";
    const retiredActive = pausedActiveReconciliation
      ? this.detachUnreferencedActive(update.assignment)
      : null;
    // Committed child permissions cannot wait for unrelated upstream SDP.
    if (update.phase === "active" && result !== "duplicate") {
      this.activateChildren(update.assignment.childPeerIds, update.revision);
    }
    if (this.paused && update.phase === "prepare") {
      this.discardPending();
      return result;
    }
    if (result === "duplicate") {
      return result;
    }
    if (retiredActive) {
      void this.queueTransition(() =>
        this.disconnectRetiredSubscriber(retiredActive),
      );
    }
    if (update.phase === "active") {
      const token = this.route.getActiveToken()!;
      if (this.pendingPeer) {
        if (update.assignment.upstream.kind === "peer" &&
          (this.pendingPeer.revision === update.revision ||
            (this.pendingPeer.activationToken && sameUpstream(previousActiveAssignment, update.assignment)))) {
          this.pendingPeer.activationToken = token;
        } else {
          this.pendingPeer = null;
          this.events.preparePeer?.(null);
        }
      }
      if (this.pending) {
        if (update.assignment.upstream.kind === "sfu" &&
          this.pending.publicationGeneration === update.assignment.sfuPublicationGeneration &&
          (this.pending.revision === update.revision || this.pending.activationToken)) {
          this.pending.activationToken = token;
          this.pending.revision = update.revision;
          this.pending.subscriber.updateConfig(this.pending);
        } else {
          this.clearPending();
        }
      }
    }
    if (update.phase === "prepare") {
      const candidate = update.candidate;
      const ownsUpstreamCandidate = candidate.childPeerId === this.viewerPeerId;
      const ownsChildCandidate =
        candidate.transport !== "sfu" &&
        update.assignment.childPeerIds.includes(candidate.childPeerId);
      this.events.prepareChild?.(
        ownsChildCandidate ? candidate : null,
        ownsChildCandidate ? update.assignment.childPeerIds : undefined,
        ownsChildCandidate ? update.revision : undefined,
      );
      if (!ownsUpstreamCandidate) {
        // A child operation cannot retire an incoming resource already committed
        // by the server, even while its local activation is still queued.
        if (this.pendingPeer && !this.pendingPeer.activationToken) {
          this.pendingPeer = null;
          this.events.preparePeer?.(null);
        }
        if (this.pending && !this.pending.activationToken) this.clearPending();
        return result;
      }
      const replacedPeerCandidate = this.pendingPeer !== null;
      if (replacedPeerCandidate) {
        this.events.preparePeer?.(null);
        this.pendingPeer = null;
      }
      const needsPeerCandidate =
        candidate.transport !== "sfu" &&
        update.assignment.upstream.kind === "peer" &&
        candidate.connectionId.length > 0;
      if (needsPeerCandidate && update.assignment.upstream.kind === "peer") {
        this.pendingPeer = {
          revision: update.revision,
          connectionId: candidate.connectionId,
          activationToken: null,
        };
        this.events.preparePeer?.(update.assignment, update.revision, candidate);
      } else if (update.assignment.upstream.kind !== "peer" && !replacedPeerCandidate) {
        this.events.preparePeer?.(null);
      }
      if (this.pending?.revision !== update.revision) {
        this.clearPending();
      }
      if (
        candidate.transport !== "sfu" ||
        update.assignment.upstream.kind !== "sfu"
      ) {
        this.clearPending();
      }
      return result;
    }

    const token = this.route.getActiveToken();
    if (!token) {
      return result;
    }
    if (!this.pendingPeer) {
      this.preparePeer(update.assignment);
    }
    if (this.resyncing) {
      return result;
    }
    void this.queueActiveRoute(token, acknowledge, this.paused);
    return result;
  }

  setPaused(paused: boolean): void {
    this.paused = paused;
    if (paused) {
      this.pending?.qualityProbe?.reset();
      if (this.pending) {
        this.pending.qualityResult = "pending";
      }
    }
    if (paused) {
      this.discardPending();
      return;
    }
    for (const slot of new Set([this.active, this.pending])) {
      if (slot?.activated && !slot.failed) {
        slot.subscriber.armDecodedFrameProof();
      }
    }
  }

  resetQualityProbe(): void {
    this.pending?.qualityProbe?.reset();
    if (this.pending) {
      this.pending.qualityResult = "pending";
    }
  }

  reconnectActive(): boolean {
    const revision = this.route.getRevision();
    const assignment = this.route.getActiveAssignment();
    const active = this.active;
    if (
      this.closed ||
      this.resyncing ||
      this.pending !== null ||
      this.paused ||
      revision === null ||
      this.route.getPhase() !== "active" ||
      assignment?.upstream.kind !== "sfu" ||
      !active ||
      active.failed ||
      !active.activated ||
      active.revision !== revision ||
      active.publicationGeneration !== assignment.sfuPublicationGeneration
    ) {
      return false;
    }

    return active.subscriber.reconnect();
  }

  async resyncAuthoritative(
    update: RouteUpdateInput,
    viewerPeerId: string,
  ): Promise<RouteUpdateResult> {
    if (this.closed) {
      return "stale";
    }
    const identityChanged = viewerPeerId !== this.viewerPeerId;
    if (identityChanged) {
      this.viewerPeerId = viewerPeerId;
    }
    const currentMediaAssignment = this.route.getMediaAssignment();
    const preservesActiveMedia = Boolean(
      !identityChanged &&
        update.phase === "active" &&
        sameUpstream(currentMediaAssignment, update.assignment) &&
        (update.assignment.upstream.kind !== "sfu" ||
          (this.active?.activated &&
            this.active.publicationGeneration ===
              update.assignment.sfuPublicationGeneration)),
    );
    if (
      !identityChanged &&
      ((!this.active && !this.pending) || preservesActiveMedia)
    ) {
      const result = this.accept(update, false);
      if (result !== "stale") {
        const active = this.active;
        const token = this.route.token();
        await this.transitionTail;
        if (!this.closed && token && this.route.owns(token) &&
            preservesActiveMedia && active && this.active === active) {
          if (active.failed) {
            this.routeFailed(update.revision, "active", active.connectionId);
          } else {
            active.subscriber.resync();
          }
        }
        return result;
      }
    }

    const resyncGeneration = ++this.resyncGeneration;
    this.resyncing = true;
    this.pendingPeer = null;
    this.route.reset();
    this.events.prepareChild?.(null);
    const pending = this.pending;
    const active = this.active;
    this.pending = null;
    this.active = null;
    if (pending) {
      pending.failed = true;
    }
    if (active) {
      active.failed = true;
    }
    const accepted = this.route.accept(update);
    this.preparePeer(update.assignment);
    this.events.resetMedia?.();
    if (update.phase === "active") {
      this.activateChildren(update.assignment.childPeerIds, update.revision);
    }

    await this.queueTransition(async () => {
      if (active?.activated) {
        try {
          active.subscriber.deactivate();
        } catch {
          // Disconnect remains the fail-closed cleanup path.
        }
      }
      if (active) {
        await disconnectSubscriber(active.subscriber);
      }
      if (pending && pending !== active) {
        await disconnectSubscriber(pending.subscriber);
      }
    });

    if (this.closed || this.resyncGeneration !== resyncGeneration) {
      return "stale";
    }
    this.resyncing = false;
    const token = this.route.getActiveToken();
    if (token) {
      await this.queueActiveRoute(token, false);
    }
    return accepted;
  }

  reportPeerFailure(parentPeerId: string, connectionId: string): boolean {
    return reportActivePeerRouteFailure(
      this.route,
      parentPeerId,
      connectionId,
      this.events.send,
    );
  }

  retryPeerActivation(connectionId: string): boolean {
    const token = this.pendingPeer?.activationToken;
    if (this.closed || this.resyncing || !token || !this.route.ownsActive(token) ||
      this.pendingPeer?.connectionId !== connectionId) return false;
    void this.queueActiveRoute(token, false);
    return true;
  }

  async disconnect(): Promise<void> {
    if (this.closed) {
      return;
    }
    this.closed = true;
    this.resyncGeneration += 1;
    this.resyncing = false;
    this.route.reset();
    this.pendingPeer = null;
    this.events.preparePeer?.(null);
    this.events.prepareChild?.(null);
    await this.queueTransition(async () => {
      const pending = this.pending;
      const active = this.active;
      this.pending = null;
      this.active = null;
      if (pending) {
        pending.failed = true;
      }
      if (active) {
        active.failed = true;
      }
      this.events.onSfuUpdate?.(
        null,
        active?.revision ?? pending?.revision ?? this.route.getRevision() ?? 0,
      );
      if (active?.activated) {
        try {
          active.subscriber.deactivate();
        } catch {
          // Disconnect remains the fail-closed cleanup path.
        }
      }
      await Promise.all([
        pending ? disconnectSubscriber(pending.subscriber) : undefined,
        active && active !== pending
          ? disconnectSubscriber(active.subscriber)
          : undefined,
      ]);
    });
  }
}
