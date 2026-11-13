// The signaling client is split by responsibility under ./signaling: socket
// lifecycle versus room intent and quality reconciliation. This barrel keeps
// the single import path every caller already uses.
export { SignalingClient } from "./client";
export {
  shouldReconnectSignaling,
  type SignalingTerminationReason,
} from "./socket";
