import type { ParticipantRouteAssignment } from "../../../shared/protocol";
import type { ViewerSubscriberTransport } from "./types";

// Small ordering and teardown helpers shared by the route and its slots.

export function sameUpstream(
  current: ParticipantRouteAssignment | null,
  next: ParticipantRouteAssignment,
): boolean {
  if (!current || current.upstream.kind !== next.upstream.kind) {
    return false;
  }
  if (current.upstream.kind === "peer" && next.upstream.kind === "peer") {
    return current.upstream.peerId === next.upstream.peerId;
  }
  if (current.upstream.kind === "sfu" && next.upstream.kind === "sfu") {
    return (
      current.sfuPublicationGeneration === next.sfuPublicationGeneration
    );
  }
  return current.upstream.kind === "none";
}

export async function disconnectSubscriber(
  subscriber: ViewerSubscriberTransport,
): Promise<void> {
  subscriber.stopDecodedFrameProof();
  await subscriber.disconnect().catch(() => undefined);
}
