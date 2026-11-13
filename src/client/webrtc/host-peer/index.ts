// The Host direct edge is split by responsibility under ./host-peer. This
// barrel preserves the single import path every caller already uses.
export { HostPeer } from "./peer";
export { RENEGOTIATION_ANSWER_TIMEOUT_MS } from "./connection";
export {
  type HostMediaPeer,
  type HostPeerEvents,
} from "./types";
