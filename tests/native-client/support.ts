import { vi } from "vitest";
import { NATIVE_CLIENT_PORT_START } from "../../src/client/native/discovery-ports";
import { NATIVE_CLIENT_PROTOCOL } from "../../src/client/native/wire";

export const restoreNativeMocks = () => {
  vi.restoreAllMocks();
  vi.useRealTimers();
  vi.unstubAllGlobals();
};

export const health = {
  protocol: NATIVE_CLIENT_PROTOCOL,
  service: "piik-client",
  port: NATIVE_CLIENT_PORT_START,
  instanceToken: "a".repeat(43),
  nativeMedia: {
    video: true, hardwareH264: true,
    receiverReuse: false, remoteCoPlay: false, shareVolume: false,
    sourceCancellation: false, captureFailureDetails: false, captureBorderControl: false,
  },
};

export const nativeMedia = {
  receiverReuse: false, remoteCoPlay: false, shareVolume: false,
  sourceCancellation: false, captureFailureDetails: false, captureBorderControl: false,
};
