import { RUST_TARGET } from "./app-package-targets.mjs";

// The Server ships one Windows x64 ZIP: the `.exe` binary and its runtime files.
export const SERVER_PACKAGE_TARGETS = [
  { id: "server-windows-amd64", rustTarget: RUST_TARGET, suffix: "-windows-amd64" },
];
