import {
  decodeServerMessage,
  SIGNAL_CLOSE_CODES,
  SIGNALING_PROTOCOL,
  type ClientMessage,
  type DisplayName,
  type IceConfig,
  type ServerMessage,
} from "../../../shared/protocol";
import type { SignalConnectionState } from "../../types";
import { debugEvent } from "../debug";
import { RoomInteractionSession } from "../room/interactions";
import { RoomVoiceSession } from "../../room-voice/session";
import { getSiteAccess } from "../api";

type WithoutProtocolEnvelope<T> = T extends {
  type: string;
  protocol: string;
}
  ? Omit<T, "type" | "protocol">
  : never;
export type SignalingIdentity = WithoutProtocolEnvelope<
  Extract<ClientMessage, { type: "authenticate" }>
>;

export interface SignalingEvents {
  onMessage: (message: ServerMessage) => void;
  onStatus: (status: SignalConnectionState) => void;
  onTerminated: (reason: SignalingTerminationReason) => void;
  onAccessRequired: () => void;
}

export type SignalingTerminationReason =
  | "STALE_CLIENT"
  | "SESSION_REPLACED"
  | "SIGNAL_TERMINATED";

const FATAL_SIGNAL_ERRORS = new Set([
  "INVALID_TOKEN",
  "ROOM_NOT_FOUND",
  "ROOM_ACCESS_DENIED",
  "ROOM_FULL",
  "HOST_ALREADY_CONNECTED",
]);
const INVALID_MESSAGE_CLOSE_CODE = 1008;
const TERMINAL_SEND_TIMEOUT_MS = 15_000;
const SIGNALING_CHALLENGE_INTERVAL_MS = 5_000;
const SIGNALING_CHALLENGE_TIMEOUT_MS = 2_000;
const SIGNALING_TIMER_LAG_TOLERANCE_MS = 1_000;
const AUTHENTICATION_TIMEOUT_MS = 8_000;

interface PendingSignalingChallenge {
  generation: number;
  sequence: number;
  confirm: boolean;
}

export function shouldReconnectSignaling(code: number): boolean {
  return (
    code !== SIGNAL_CLOSE_CODES.sessionReplaced &&
    code !== SIGNAL_CLOSE_CODES.authenticationFailed &&
    code !== INVALID_MESSAGE_CLOSE_CODE &&
    code !== SIGNAL_CLOSE_CODES.viewerAccessRevoked
  );
}

function signalUrl(): string {
  const url = new URL("/signal", window.location.href);
  url.protocol = window.location.protocol === "https:" ? "wss:" : "ws:";
  return url.toString();
}

/**
 * Signaling socket lifecycle: connect, authenticate, reconnect, the liveness
 * watchdog and terminal close handling. Room-state messages are delegated.
 */
export abstract class SignalingSocket {
  readonly interactions: RoomInteractionSession;
  readonly roomVoice: RoomVoiceSession;
  protected voiceIceConfig: IceConfig = { iceServers: [] };
  protected socket: WebSocket | null = null;
  protected stopped = false;
  protected authenticatedPeerId: string | null = null;
  private reconnectAttempt = 0;
  private reconnectTimer: number | null = null;
  private authenticationTimer: number | null = null;
  private accessCheck: AbortController | null = null;
  private terminalTimer: number | null = null;
  private terminalMessage: ClientMessage | null = null;
  private socketGeneration = 0;
  private challengeSequence = 0;
  private watchdogTimer: number | null = null;
  private watchdogDeadlineMs = 0;
  private pendingChallenge: PendingSignalingChallenge | null = null;
  private previousChallenge: PendingSignalingChallenge | null = null;
  private visibilityListenerAttached = false;

  constructor(
    protected readonly identity: SignalingIdentity,
    protected readonly events: SignalingEvents,
    protected readonly now: () => number = Date.now,
  ) {
    this.interactions = new RoomInteractionSession(message => this.send(message), this.now);
    this.roomVoice = new RoomVoiceSession(message => this.send(message), this.voiceIceConfig);
  }

  setDisplayName(displayName: DisplayName): boolean {
    this.identity.displayName = displayName;
    return this.send({ type: "set-display-name", displayName });
  }

  start(): void {
    if (this.socket || this.stopped) {
      return;
    }
    this.attachVisibilityListener();
    debugEvent("signal", "start", { role: this.identity.role });
    this.events.onStatus(this.reconnectAttempt === 0 ? "connecting" : "reconnecting");
    this.connect();
  }

  stop(): void {
    this.interactions.close();
    this.roomVoice.close();
    this.stopped = true;
    this.authenticatedPeerId = null;
    this.clearTimers();
    this.detachVisibilityListener();
    this.terminalMessage = null;
    this.resetIntent();
    const socket = this.socket;
    this.socket = null;
    if (socket && socket.readyState < WebSocket.CLOSING) {
      socket.close(1000, "client closed");
    }
    this.events.onStatus("offline");
    debugEvent("signal", "stop", { role: this.identity.role });
  }

  send(message: ClientMessage): boolean {
    if (
      !this.authenticatedPeerId ||
      !this.socket ||
      this.socket.readyState !== WebSocket.OPEN
    ) {
      return false;
    }
    if (message.type === "send-room-interaction" &&
      this.socket.bufferedAmount > 16 * 1024) return false;
    this.socket.send(JSON.stringify(message));
    debugEvent("signal", "sent", { generation: this.socketGeneration, message });
    return true;
  }

  sendThenStop(message: ClientMessage): void {
    if (this.stopped) {
      return;
    }
    if (this.send(message)) {
      this.stop();
      return;
    }

    this.terminalMessage = message;
    if (this.terminalTimer === null) {
      this.terminalTimer = window.setTimeout(() => {
        this.terminalTimer = null;
        this.stop();
      }, TERMINAL_SEND_TIMEOUT_MS);
    }
  }

  /** Clear room-owned intent as the session retires. */
  protected abstract resetIntent(): void;

  /** Apply a decoded message that belongs to the room session. */
  protected abstract handleRoomMessage(message: ServerMessage, generation: number): void;

  /** Adopt an authenticated transport session. The terminal message, when it
   *  was queued before authentication, is sent before the session stops. */
  protected acceptAuthenticated(peerId: string, iceConfig: IceConfig, generation: number): boolean {
    this.authenticatedPeerId = peerId;
    this.reconnectAttempt = 0;
    this.clearAuthenticationTimer();
    if (this.terminalMessage) {
      const terminalMessage = this.terminalMessage;
      if (this.send(terminalMessage)) {
        this.terminalMessage = null;
        this.stop();
        return false;
      }
    }
    this.events.onStatus("connected");
    debugEvent("signal", "authenticated", { role: this.identity.role, generation });
    this.refreshSignalingWatchdog();
    this.interactions.authenticated(peerId);
    this.voiceIceConfig = iceConfig;
    this.roomVoice.updateIceConfig(iceConfig);
    this.roomVoice.authenticated(peerId, this.identity.role);
    return true;
  }

  protected connect(): void {
    if (this.stopped) {
      return;
    }
    // An explicit start may supersede a pending reconnect for the same room.
    if (this.reconnectTimer !== null) {
      window.clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    this.accessCheck?.abort();
    this.accessCheck = null;

    const generation = ++this.socketGeneration;
    debugEvent("signal", "connecting", { role: this.identity.role, generation });
    const socket = new WebSocket(signalUrl());
    this.socket = socket;

    socket.addEventListener("open", () => {
      if (
        this.socket !== socket ||
        generation !== this.socketGeneration ||
        this.stopped
      ) {
        return;
      }
      const authenticate: ClientMessage = {
        type: "authenticate",
        protocol: SIGNALING_PROTOCOL,
        ...this.identity,
      };
      socket.send(JSON.stringify(authenticate));
      this.authenticationTimer = window.setTimeout(() => {
        if (!this.authenticatedPeerId && this.socket === socket) {
          socket.close(4000, "authentication timeout");
        }
      }, AUTHENTICATION_TIMEOUT_MS);
    });

    socket.addEventListener("message", (event) => {
      if (
        this.socket !== socket ||
        generation !== this.socketGeneration ||
        typeof event.data !== "string"
      ) {
        return;
      }
      let message: ServerMessage;
      try {
        message = decodeServerMessage(event.data);
      } catch {
        debugEvent("signal", "invalid-message", { role: this.identity.role });
        this.terminateForProtocolMismatch();
        return;
      }

      if (message.type === "signaling-challenge-response") {
        this.acceptSignalingChallengeResponse(message.sequence, generation);
        return;
      }
      debugEvent("signal", "received", { generation, message });

      if (message.type === "error") {
        debugEvent("signal", "server-error", { role: this.identity.role, code: message.code });
      } else if (message.type === "route-update" || message.type === "route-status") {
        debugEvent("signal", "route", { role: this.identity.role, type: message.type, revision: message.revision });
      }

      if (
        message.type === "error" &&
        message.code === "INVALID_MESSAGE" &&
        !this.authenticatedPeerId
      ) {
        this.terminateForProtocolMismatch();
        return;
      }

      if (message.type === "error" && message.code === "AUTH_REQUIRED") {
        void this.recoverAuthentication(generation);
        return;
      }

      if (message.type === "error" && FATAL_SIGNAL_ERRORS.has(message.code)) {
        this.stop();
        this.events.onMessage(message);
        return;
      }

      this.handleRoomMessage(message, generation);
    });

    socket.addEventListener("close", (event) => {
      if (
        this.socket !== socket ||
        generation !== this.socketGeneration
      ) {
        return;
      }
      this.socket = null;
      this.authenticatedPeerId = null;
      this.interactions.disconnected();
      this.roomVoice.disconnected();
      debugEvent("signal", "closed", { role: this.identity.role, code: event.code,
        reconnecting: !this.stopped && shouldReconnectSignaling(event.code) });
      this.clearSignalingWatchdog();
      this.clearAuthenticationTimer();
      if (!this.stopped && shouldReconnectSignaling(event.code)) {
        this.scheduleReconnect();
      } else if (!this.stopped) {
        this.stop();
        this.events.onTerminated(
          event.code === SIGNAL_CLOSE_CODES.sessionReplaced
            ? "SESSION_REPLACED"
            : event.code === INVALID_MESSAGE_CLOSE_CODE
              ? "STALE_CLIENT"
              : "SIGNAL_TERMINATED",
        );
      }
    });

    socket.addEventListener("error", () => {
      // The close event owns reconnect scheduling and user-visible state.
    });
  }

  private scheduleReconnect(): void {
    this.events.onStatus("reconnecting");
    const exponent = Math.min(this.reconnectAttempt, 4);
    const delay = Math.min(8_000, 500 * 2 ** exponent) + Math.random() * 250;
    this.reconnectAttempt += 1;
    this.reconnectTimer = window.setTimeout(() => {
      this.reconnectTimer = null;
      this.connect();
    }, delay);
  }

  private async recoverAuthentication(generation: number): Promise<void> {
    this.retireSocketForRecovery();
    // AUTH_REQUIRED also means the room handshake timed out or was incomplete.
    // Only the site-access owner can require a Host to enter a site password.
    if (this.identity.role === "host") {
      this.events.onStatus("reconnecting");
      if (this.stopped || generation !== this.socketGeneration) return;
      const controller = new AbortController();
      this.accessCheck = controller;
      const timer = window.setTimeout(() => controller.abort(), AUTHENTICATION_TIMEOUT_MS);
      try {
        const access = await getSiteAccess(controller.signal);
        if (this.stopped || generation !== this.socketGeneration) return;
        if (access.required && !access.authenticated) {
          this.stop();
          this.events.onAccessRequired();
          return;
        }
      } catch {
        // An unavailable check is a connection failure, not evidence of denial.
      } finally {
        window.clearTimeout(timer);
        if (this.accessCheck === controller) this.accessCheck = null;
      }
    }
    if (!this.stopped && generation === this.socketGeneration) this.scheduleReconnect();
  }

  private terminateForProtocolMismatch(): void {
    this.stop();
    this.events.onTerminated("STALE_CLIENT");
  }

  protected clearAuthenticationTimer(): void {
    if (this.authenticationTimer !== null) {
      window.clearTimeout(this.authenticationTimer);
      this.authenticationTimer = null;
    }
  }

  private clearTimers(): void {
    this.accessCheck?.abort();
    this.accessCheck = null;
    this.clearAuthenticationTimer();
    this.clearSignalingWatchdog();
    if (this.reconnectTimer !== null) {
      window.clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    if (this.terminalTimer !== null) {
      window.clearTimeout(this.terminalTimer);
      this.terminalTimer = null;
    }
  }

  private refreshSignalingWatchdog(): void {
    if (!this.isWatchdogEligible()) {
      this.clearSignalingWatchdog();
      return;
    }
    if (this.watchdogTimer === null && this.pendingChallenge === null) {
      this.armSignalingWatchdog(SIGNALING_CHALLENGE_INTERVAL_MS);
    }
  }

  private armSignalingWatchdog(delayMs: number): void {
    const generation = this.socketGeneration;
    this.watchdogDeadlineMs = this.now() + delayMs;
    this.watchdogTimer = window.setTimeout(() => {
      this.watchdogTimer = null;
      this.handleSignalingWatchdogTick(generation);
    }, delayMs);
  }

  private handleSignalingWatchdogTick(generation: number): void {
    if (generation !== this.socketGeneration) {
      return;
    }
    if (
      !this.isWatchdogEligible() ||
      Math.abs(this.now() - this.watchdogDeadlineMs) >
        SIGNALING_TIMER_LAG_TOLERANCE_MS
    ) {
      this.rebaselineSignalingWatchdog();
      return;
    }
    const pending = this.pendingChallenge;
    if (!pending) {
      this.sendSignalingChallenge(false, generation);
    } else if (pending.confirm) {
      this.replaceUnresponsiveSocket(generation);
    } else {
      this.sendSignalingChallenge(true, generation);
    }
  }

  private sendSignalingChallenge(confirm: boolean, generation: number): void {
    const socket = this.socket;
    if (!socket || generation !== this.socketGeneration) {
      return;
    }
    this.challengeSequence =
      this.challengeSequence === Number.MAX_SAFE_INTEGER
        ? 0
        : this.challengeSequence + 1;
    const sequence = this.challengeSequence;
    socket.send(JSON.stringify({ type: "signaling-challenge", sequence }));
    this.previousChallenge = this.pendingChallenge?.generation === generation
      ? this.pendingChallenge
      : null;
    this.pendingChallenge = { generation, sequence, confirm };
    this.armSignalingWatchdog(SIGNALING_CHALLENGE_TIMEOUT_MS);
  }

  private acceptSignalingChallengeResponse(
    sequence: number,
    generation: number,
  ): void {
    const pending = this.pendingChallenge;
    const previous = this.previousChallenge;
    if (
      ![pending, previous].some(
        (challenge) =>
          challenge !== null &&
          challenge.generation === generation &&
          challenge.sequence === sequence,
      ) ||
      generation !== this.socketGeneration
    ) {
      return;
    }
    this.clearSignalingWatchdog();
    this.refreshSignalingWatchdog();
  }

  private replaceUnresponsiveSocket(generation: number): void {
    if (!this.socket || generation !== this.socketGeneration || this.stopped) {
      return;
    }
    this.retireSocketForRecovery();
    this.events.onStatus("reconnecting");
    this.connect();
  }

  private retireSocketForRecovery(): void {
    const socket = this.socket;
    this.socket = null;
    this.authenticatedPeerId = null;
    this.interactions.disconnected();
    this.roomVoice.disconnected();
    this.clearAuthenticationTimer();
    this.clearSignalingWatchdog();
    if (socket && socket.readyState < WebSocket.CLOSING) {
      socket.close(SIGNAL_CLOSE_CODES.clientReconnect, "signaling recovery");
    }
  }

  private rebaselineSignalingWatchdog(): void {
    this.clearSignalingWatchdog();
    this.refreshSignalingWatchdog();
  }

  private clearSignalingWatchdog(): void {
    this.pendingChallenge = null;
    this.previousChallenge = null;
    if (this.watchdogTimer !== null) {
      window.clearTimeout(this.watchdogTimer);
      this.watchdogTimer = null;
    }
  }

  private readonly onVisibilityChange = (): void => {
    this.rebaselineSignalingWatchdog();
  };

  private attachVisibilityListener(): void {
    if (typeof document === "undefined" || this.visibilityListenerAttached) {
      return;
    }
    document.addEventListener("visibilitychange", this.onVisibilityChange);
    this.visibilityListenerAttached = true;
  }

  private detachVisibilityListener(): void {
    if (typeof document === "undefined" || !this.visibilityListenerAttached) {
      return;
    }
    document.removeEventListener("visibilitychange", this.onVisibilityChange);
    this.visibilityListenerAttached = false;
  }

  private isDocumentVisible(): boolean {
    return (
      typeof document === "undefined" || document.visibilityState === "visible"
    );
  }

  private isWatchdogEligible(): boolean {
    return (
      this.authenticatedPeerId !== null &&
      this.isDocumentVisible() &&
      this.socket?.readyState === WebSocket.OPEN
    );
  }
}
