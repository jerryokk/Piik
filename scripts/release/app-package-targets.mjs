import { join, resolve } from "node:path";

export const CLOUDFLARED_VERSION = "2026.10.3";

// Every package builds natively on its Windows x64 runner, so Cargo writes
// release binaries to target/release without a --target subdirectory.
export const RUST_TARGET = "x86_64-pc-windows-msvc";

export const APP_PACKAGE_TARGETS = [
  {
    id: "windows-amd64",
    nodePlatform: "win32",
    nodeArch: "x64",
    rustTarget: RUST_TARGET,
    appName: "piik-app.exe",
    tunnelName: "cloudflared.exe",
    captureName: "piik-capture.exe",
    tunnelAsset: "cloudflared-windows-amd64.exe",
    tunnelSha256: "76b2805253d0d6ffacb092a7acfbee99e7af6917835c0c5410e23f3ef5052da2",
  },
];

export function appPackageTarget(id) {
  return APP_PACKAGE_TARGETS.find((target) => target.id === id) ?? null;
}

export function cargoCommand() {
  return process.env.PIIK_CARGO?.trim() || "cargo";
}

// cargoGateBuild describes a development build of one workspace binary for
// local gates. Gates run the binary from target/debug, a stable path.
/**
 * @param {string} repositoryRoot
 * @param {string} packageName
 * @returns {{ command: string, args: string[], env: NodeJS.ProcessEnv, executable: string }}
 */
export function cargoGateBuild(repositoryRoot, packageName) {
  const targetDirectory = resolve(repositoryRoot, process.env.CARGO_TARGET_DIR?.trim() || "target");
  return {
    command: cargoCommand(),
    args: ["build", "--locked", "-p", packageName],
    env: { ...process.env, CARGO_TARGET_DIR: targetDirectory },
    executable: join(targetDirectory, "debug", `${packageName}.exe`),
  };
}

// cargoRelease describes the locked release build of one workspace binary.
// The binaries read PIIK_RELEASE/PIIK_REVISION at compile time; without an
// identity they keep the ambient values or their `development` defaults.
/**
 * @param {string} repositoryRoot
 * @param {string} packageName
 * @param {{ version: string, revision: string } | null} [identity]
 * @returns {{ args: string[], env: NodeJS.ProcessEnv, executable: string }}
 */
export function cargoRelease(repositoryRoot, packageName, identity = null) {
  const targetDirectory = resolve(repositoryRoot, process.env.CARGO_TARGET_DIR?.trim() || "target");
  return {
    args: ["build", "--release", "--locked", "-p", packageName],
    env: {
      ...process.env,
      CARGO_TARGET_DIR: targetDirectory,
      ...(identity ? { PIIK_RELEASE: identity.version, PIIK_REVISION: identity.revision } : {}),
    },
    executable: join(targetDirectory, "release", `${packageName}.exe`),
  };
}
