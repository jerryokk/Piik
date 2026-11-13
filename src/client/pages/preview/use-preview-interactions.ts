import { useEffect, useState } from "react";
import type { InteractionPayload } from "../../../shared/protocol";
import { createOpaqueId } from "../../lib/util/opaque-id";
import { RoomInteractionSession } from "../../lib/room/interactions";

/** A local participant and friend exercise the real interaction view, without signaling. */
export function usePreviewInteractions(viewer: boolean) {
  const [session, setSession] = useState<RoomInteractionSession | null>(null);
  useEffect(() => {
    const sender = viewer
      ? { peerId: "preview-friend", role: "viewer" as const, displayName: "Piik friend" }
      : { peerId: "preview-host", role: "host" as const, displayName: "Piik" };
    const model = new RoomInteractionSession(message => {
      queueMicrotask(() => {
        if (message.type === "send-room-interaction") model.receive({ type: "room-interaction", id: createOpaqueId(),
          requestId: message.requestId, occurredAt: Date.now(), sender, payload: message.payload });
      });
      return true;
    }, Date.now);
    model.authenticated(sender.peerId);
    // The Server announces readiness with the authenticated session.
    model.receive({ type: "room-interactions-ready", serverTime: Date.now() });
    setSession(model);
    return () => model.close();
  }, [viewer]);
  const receiveSample = (payload: InteractionPayload) => session?.receive({
    type: "room-interaction", id: createOpaqueId(), requestId: createOpaqueId(), occurredAt: Date.now(),
    sender: viewer ? { peerId: "preview-host", role: "host", displayName: "Piik" }
      : { peerId: "preview-friend", role: "viewer", displayName: "Piik friend" }, payload,
  });
  return { session, receiveSample };
}
