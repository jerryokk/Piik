import type { z } from "zod";
import { debugError, debugEvent } from "../lib/debug";
import { createOpaqueId } from "../lib/util/opaque-id";
import {
  nativeEventSchema,
  NATIVE_CLIENT_PROTOCOL,
  pongResponseSchema,
  requestFailedResponseSchema,
  type NativeClientEvent,
  type NativeHealth,
} from "./wire";

const REQUEST_TIMEOUT_MS = 8_000;

interface PendingRequest<T = unknown> {
  operation: string;
  schema: z.ZodType<T>;
  resolve: (value: T) => void;
  reject: (error: Error) => void;
  timer: number | null;
}

/** The control boundary knows the failed operation, not the driver's cause. */
export class NativeRequestError extends Error {
  constructor(
    readonly reason: "unavailable" | "timeout" | "send-failed" | "rejected" | "invalid-response" | "disconnected",
    readonly operation: string,
    options?: ErrorOptions,
  ) {
    super({
      unavailable: "Piik App is unavailable",
      timeout: "Piik App request timed out",
      "send-failed": "Piik App request failed",
      rejected: "Piik App request failed",
      "invalid-response": "Piik App response is invalid",
      disconnected: "Piik App disconnected",
    }[reason], options);
    this.name = "NativeRequestError";
  }
}

/**
 * Request/response transport over the App control socket: correlation ids,
 * per-request deadlines, schema validation of responses and wire events.
 */
export abstract class NativeClientTransport {
  private readonly pending = new Map<string, PendingRequest>();
  private readonly listeners = new Set<(event: NativeClientEvent) => void>();
  private readonly closeListeners = new Set<() => void>();
  protected closed = false;

  protected constructor(
    readonly health: NativeHealth,
    private readonly socket: WebSocket,
  ) {
    socket.addEventListener("message", (event) =>
      this.handleMessage(event.data),
    );
    socket.addEventListener("close", () => this.handleClose());
    socket.addEventListener("error", () => this.handleClose());
  }

  onEvent(listener: (event: NativeClientEvent) => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  onClose(listener: () => void): () => void {
    if (this.closed) {
      listener();
      return () => undefined;
    }
    this.closeListeners.add(listener);
    return () => this.closeListeners.delete(listener);
  }

  async ping(): Promise<void> {
    await this.request("ping", {}, pongResponseSchema);
  }

  close(): void {
    if (this.closed) return;
    this.closed = true;
    this.retireCommands();
    this.rejectPending();
    this.listeners.clear();
    this.closeListeners.clear();
    if (this.socket.readyState < WebSocket.CLOSING) {
      this.socket.close(1000, "page closed");
    }
  }

  /** Release command-owned state as the transport retires. */
  protected abstract retireCommands(): void;

  protected request<T>(
    type: string,
    fields: object,
    schema: z.ZodType<T>,
    timeoutMs: number | null = REQUEST_TIMEOUT_MS,
    onRequested?: () => void,
  ): Promise<T> {
    if (this.closed || this.socket.readyState !== WebSocket.OPEN) {
      return Promise.reject(new NativeRequestError("unavailable", type));
    }
    const id = createOpaqueId();
    const began = performance.now();
    const details = { type, requestId: id };
    return new Promise<T>((resolveRequest, rejectRequest) => {
      const timer =
        timeoutMs === null
          ? null
          : window.setTimeout(() => {
              this.pending.delete(id);
              debugEvent("native", "request-timeout", { ...details, durationMs: performance.now() - began });
              rejectRequest(new NativeRequestError("timeout", type));
            }, timeoutMs);
      this.pending.set(id, {
        operation: type,
        schema,
        resolve: (value) => {
          debugEvent("native", "response", { ...details, durationMs: performance.now() - began, applied: value });
          resolveRequest(value as T);
        },
        reject: (error) => {
          debugError("native", "request-failed", error, { ...details, durationMs: performance.now() - began });
          rejectRequest(error);
        },
        timer,
      });
      try {
        debugEvent("native", "request", { ...details, requested: fields, timeoutMs });
        this.socket.send(
          JSON.stringify({
            version: NATIVE_CLIENT_PROTOCOL,
            id,
            type,
            ...fields,
          }),
        );
      } catch (error) {
        if (timer !== null) window.clearTimeout(timer);
        this.pending.delete(id);
        debugError("native", "request-failed", error, { ...details, durationMs: performance.now() - began });
        rejectRequest(new NativeRequestError("send-failed", type, { cause: error }));
        return;
      }
      onRequested?.();
    });
  }

  private handleMessage(raw: unknown): void {
    let value: unknown;
    try {
      value = JSON.parse(String(raw));
    } catch {
      debugEvent("native", "protocol-failed", { stage: "json-invalid" });
      this.failConnection();
      return;
    }
    const id = requestId(value);
    if (id) {
      const pending = this.pending.get(id);
      if (!pending) return;
      this.pending.delete(id);
      if (pending.timer !== null) window.clearTimeout(pending.timer);
      const failure = requestFailedResponseSchema.safeParse(value);
      if (failure.success) {
        if (failure.data.code === "source-canceled") {
          pending.reject(new DOMException("Source selection canceled", "NotAllowedError"));
          return;
        }
        pending.reject(new NativeRequestError("rejected", pending.operation, { cause: { code: failure.data.code } }));
        return;
      }
      const parsed = pending.schema.safeParse(value);
      if (parsed.success) {
        pending.resolve(parsed.data);
      } else {
        pending.reject(new NativeRequestError("invalid-response", pending.operation, { cause: parsed.error }));
      }
      return;
    }
    const event = nativeEventSchema.safeParse(value);
    if (!event.success) {
      // A rejected observation has no control authority. Drop it without
      // retiring healthy media; its identity/state must still pass to be used.
      if (value && typeof value === "object" && "type" in value &&
        (value.type === "edge-quality" || value.type === "publication-quality")) {
        debugError("native", "quality-sample-invalid", event.error);
        return;
      }
      debugError("native", "protocol-failed", event.error, { stage: "event-invalid" });
      this.failConnection();
      return;
    }
    debugEvent("native", "event", event.data);
    for (const listener of this.listeners) {
      listener(event.data);
    }
  }

  private handleClose(): void {
    if (this.closed) return;
    debugEvent("native", "closed");
    this.closed = true;
    this.rejectPending();
    this.listeners.clear();
    const listeners = [...this.closeListeners];
    this.closeListeners.clear();
    for (const listener of listeners) listener();
  }

  private failConnection(): void {
    if (this.socket.readyState < WebSocket.CLOSING) {
      this.socket.close(1008, "invalid Client message");
    }
    this.handleClose();
  }

  private rejectPending(): void {
    for (const request of this.pending.values()) {
      if (request.timer !== null) window.clearTimeout(request.timer);
      request.reject(new NativeRequestError("disconnected", request.operation));
    }
    this.pending.clear();
  }
}

function requestId(value: unknown): string | null {
  if (!value || typeof value !== "object" || !("id" in value)) return null;
  const id = (value as { id?: unknown }).id;
  return typeof id === "string" ? id : null;
}
