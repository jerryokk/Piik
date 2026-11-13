// The Native Viewer edge is split under ./native-viewer: route selection, the
// Native backend and its SDP codec preflight. This barrel preserves the path.
export { NativeCapableViewerPeer } from "./router";
export { offerHasNativeVideoCodec } from "./codec";
