import type {
  CoPlayGrant,
  QualitySettings,
  RoutePolicy,
  ServerMessage,
  ViewerRoutePreference,
} from "../../../shared/protocol";
import { qualitySettingsEqual } from "../../media/quality";
import { SignalingSocket, type SignalingEvents, type SignalingIdentity } from "./socket";

interface HostQualityIntent {
  shareGeneration: string;
  qualitySettings: QualitySettings;
}

/**
 * Room intent owned by the authenticated signaling session: Host share and
 * quality intents, presence, co-play grants, route preference and relay
 * volunteering. Reconnection replays these intents on the same socket owner.
 */
export class SignalingClient extends SignalingSocket {
  private remoteInputEnabled = false;
  private hostAudioAvailable: boolean | null = null;
  private coPlayGrants: CoPlayGrant[] = [];
  private hostQualityIntent: HostQualityIntent | null = null;
  private pendingShareGeneration: string | null = null;
  private relayVolunteerIntent: { enabled: boolean; capacity: number } | null = null;

  constructor(
    identity: SignalingIdentity,
    events: SignalingEvents,
    now: () => number = Date.now,
  ) {
    super(identity, events, now);
  }

  // The Host receives remote-input peers for co-play; the Viewer registers its
  // encrypted input endpoint through the same authenticated session.
  enableRemoteInput(): void {
    if (this.stopped || this.identity.role !== "host") return;
    this.remoteInputEnabled = true;
    if (this.authenticatedPeerId) this.send({ type: "subscribe-remote-input" });
  }

  setCoPlayGrants(grants: CoPlayGrant[]): void {
    if (this.identity.role !== "host") return;
    this.coPlayGrants = grants;
    this.flushCoPlayPresence();
  }

  private flushCoPlayPresence(): void {
    if (!this.authenticatedPeerId) return;
    if (this.identity.role === "host" && this.identity.shareGeneration && this.pendingShareGeneration === null) {
      this.send({ type: "set-co-play-presence", shareGeneration: this.identity.shareGeneration, grants: this.coPlayGrants });
    }
  }

  setAudioAvailable(available: boolean): void {
    if (this.identity.role !== "host") return;
    this.hostAudioAvailable = available;
    this.flushAudioPresence();
  }

  private flushAudioPresence(): void {
    if (!this.authenticatedPeerId) return;
    if (this.identity.role === "host" && this.identity.shareGeneration &&
      this.pendingShareGeneration === null && this.hostAudioAvailable !== null) {
      this.send({ type: "set-audio-presence", shareGeneration: this.identity.shareGeneration, available: this.hostAudioAvailable });
    }
  }

  setViewerRoutePreference(preference: ViewerRoutePreference): boolean {
    if (this.identity.role !== "viewer" ||
      !this.send({ type: "set-viewer-route-preference", preference })) return false;
    // Replies describe applied policy; reconnect retains the personal request,
    // including when its acknowledgement is lost or an earlier reply arrives late.
    this.identity.routePreference = preference;
    return true;
  }

  get relayVolunteerRequest(): Readonly<{ enabled: boolean; capacity: number }> | null {
    return this.relayVolunteerIntent;
  }

  setRelayVolunteer(enabled: boolean, capacity: number): boolean {
    if (this.stopped || this.identity.role !== "viewer") return false;
    this.relayVolunteerIntent = { enabled, capacity };
    return this.authenticatedPeerId === null || this.flushRelayVolunteerIntent();
  }

  private flushRelayVolunteerIntent(): boolean {
    return this.relayVolunteerIntent !== null && this.send({ type: "set-relay-volunteer", ...this.relayVolunteerIntent });
  }

  setSharingPaused(paused: boolean): boolean {
    if (this.identity.role !== "host") {
      return false;
    }
    this.identity.sharingPaused = paused;
    if (!this.identity.shareGeneration) {
      return false;
    }
    try {
      return this.send({
        type: "set-sharing-paused",
        shareGeneration: this.identity.shareGeneration,
        paused,
      });
    } catch {
      return false;
    }
  }

  ownsHostRoom(roomId: string, hostToken: string): boolean {
    return this.identity.role === "host" && this.identity.roomId === roomId && this.identity.token === hostToken;
  }

  wantsHostPublication(shareGeneration: string): boolean {
    return this.identity.role === "host" &&
      this.identity.shareGeneration === shareGeneration;
  }

  /** Keep the authenticated room session and claim a fresh media generation. */
  startSharing(
    shareGeneration: string,
    qualitySettings: QualitySettings,
    routePolicy: RoutePolicy,
    sharingPaused = false,
  ): boolean {
    if (this.identity.role !== "host" || this.stopped || !shareGeneration) {
      return false;
    }
    if (this.identity.shareGeneration !== shareGeneration) this.coPlayGrants = [];
    this.identity.shareGeneration = shareGeneration;
    this.identity.sharingPaused = sharingPaused;
    this.identity.qualitySettings = { ...qualitySettings };
    this.identity.routePolicy = { ...routePolicy };
    this.pendingShareGeneration = shareGeneration;
    const sent = this.flushHostShareIntent();
    return sent || this.authenticatedPeerId === null;
  }

  /** Stop only the media publication; the room and interaction session stay alive. */
  stopSharing(shareGeneration: string): boolean {
    if (this.identity.role !== "host" || this.stopped || !shareGeneration ||
      this.identity.shareGeneration !== shareGeneration) {
      return false;
    }
    const sent = this.send({ type: "stop-sharing", shareGeneration });
    this.identity.shareGeneration = undefined;
    this.identity.sharingPaused = false;
    this.pendingShareGeneration = null;
    this.hostQualityIntent = null;
    this.hostAudioAvailable = null;
    this.coPlayGrants = [];
    return sent;
  }

  confirmSharingPaused(): void {
    if (this.identity.role === "host") {
      this.identity.sharingPaused = true;
    }
  }

  setHostQualitySettings(qualitySettings: QualitySettings): boolean {
    if (this.identity.role !== "host" || !this.identity.shareGeneration) {
      return false;
    }
    const intent: HostQualityIntent = {
      shareGeneration: this.identity.shareGeneration,
      qualitySettings: { ...qualitySettings },
    };
    this.identity.qualitySettings = { ...qualitySettings };
    this.hostQualityIntent = intent;
    return this.flushHostQualityIntent();
  }

  pendingHostQualitySettings(
    shareGeneration: string,
  ): QualitySettings | null {
    const intent = this.hostQualityIntent;
    return intent?.shareGeneration === shareGeneration
      ? { ...intent.qualitySettings }
      : null;
  }

  protected resetIntent(): void {
    this.hostQualityIntent = null;
    this.pendingShareGeneration = null;
    this.relayVolunteerIntent = null;
  }

  protected handleRoomMessage(message: ServerMessage, generation: number): void {
    if (message.type === "authenticated") {
      if (!this.acceptAuthenticated(message.peerId, message.iceConfig, generation)) return;
      if (this.remoteInputEnabled) this.send({ type: "subscribe-remote-input" });
      this.reconcileHostShareState(message);
      this.flushAudioPresence();
      this.flushCoPlayPresence();
    }
    if ((message.type === "sharing-started" || message.type === "sharing-start-failed") &&
      this.pendingShareGeneration === message.shareGeneration) {
      this.pendingShareGeneration = null;
      if (message.type === "sharing-started") {
        this.flushAudioPresence();
        this.flushCoPlayPresence();
      }
    }
    if (message.type === "sharing-start-failed" && this.identity.role === "host" &&
      this.identity.shareGeneration === message.shareGeneration) {
      this.identity.shareGeneration = undefined;
      this.identity.sharingPaused = false;
      this.hostQualityIntent = null;
    }
    if (this.interactions.receive(message)) return;
    if (this.roomVoice.receive(message)) return;
    if (
      message.type === "host-status" &&
      this.identity.role === "host" &&
      message.online &&
      !message.paused
    ) {
      this.identity.sharingPaused = false;
    }
    this.events.onMessage(message);
    if (message.type === "authenticated") {
      // The page sends its ordinary relay report first; restore only this
      // page's explicit intent after it, in wire order.
      this.flushRelayVolunteerIntent();
      this.reconcileHostQualityIntent(message);
    }
  }

  private flushHostQualityIntent(): boolean {
    const intent = this.hostQualityIntent;
    if (
      !intent ||
      this.identity.role !== "host" ||
      this.identity.shareGeneration !== intent.shareGeneration
    ) {
      return false;
    }
    try {
      if (
        !this.send({
          type: "set-quality-settings",
          qualitySettings: intent.qualitySettings,
        })
      ) {
        return false;
      }
    } catch {
      return false;
    }
    return true;
  }

  private flushHostShareIntent(): boolean {
    const shareGeneration = this.pendingShareGeneration;
    if (
      !shareGeneration ||
      this.identity.role !== "host" ||
      this.identity.shareGeneration !== shareGeneration
    ) {
      return false;
    }
    try {
      if (!this.send({
        type: "start-sharing",
        shareGeneration,
        sharingPaused: this.identity.sharingPaused,
        qualitySettings: this.identity.qualitySettings,
        routePolicy: this.identity.routePolicy,
      })) {
        return false;
      }
      return true;
    } catch {
      return false;
    }
  }

  private reconcileHostShareState(
    message: Extract<ServerMessage, { type: "authenticated" }>,
  ): void {
    if (this.identity.role !== "host") {
      return;
    }
    const desiredGeneration = this.identity.shareGeneration;
    if (!desiredGeneration) {
      // An idle Host session: stop anything the Server still claims.
      this.pendingShareGeneration = null;
      if (message.shareGeneration !== null) {
        // stopSharing may have been requested while authentication was still
        // in flight. The authenticated reply is the first point where the
        // server can accept the idempotent stop for that session.
        this.send({
          type: "stop-sharing",
          shareGeneration: message.shareGeneration,
        });
      }
      return;
    }
    if (message.shareGeneration === desiredGeneration) {
      // Authentication already claimed this generation; sending a second
      // start-sharing would be rejected as a duplicate publication.
      this.pendingShareGeneration = null;
      return;
    }
    if (message.shareGeneration === null) {
      this.flushHostShareIntent();
      return;
    }
    if (this.pendingShareGeneration === desiredGeneration) {
      // A stop/start can cross the authentication response: the socket was
      // authenticated with the old generation, while the UI already owns a
      // fresh one. WebSocket ordering makes this pair one serialized server
      // transition; keep the new intent and retire the stale publication.
      if (this.send({
        type: "stop-sharing",
        shareGeneration: message.shareGeneration,
      })) {
        this.flushHostShareIntent();
      }
      return;
    }
    // Without a pending local intent, the server's active generation is
    // authoritative. Do not retry an intent that no longer describes this
    // authenticated session.
    this.pendingShareGeneration = null;
  }

  private reconcileHostQualityIntent(
    message: Extract<ServerMessage, { type: "authenticated" }>,
  ): void {
    const intent = this.hostQualityIntent;
    if (
      !intent ||
      this.identity.role !== "host" ||
      this.identity.shareGeneration !== intent.shareGeneration
    ) {
      return;
    }
    if (
      "qualitySettings" in message &&
      qualitySettingsEqual(message.qualitySettings, intent.qualitySettings)
    ) {
      if (this.hostQualityIntent === intent) {
        this.hostQualityIntent = null;
      }
      return;
    }
    this.flushHostQualityIntent();
  }
}
