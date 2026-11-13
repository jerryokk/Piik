#!/usr/bin/env node

// Stages the helper processes a Piik App build looks for beside its own
// executable (`runtime/native/piik-capture.exe` and
// `runtime/tunnel/cloudflared.exe`). A plain `cargo build -p piik-app` writes
// only the executable, so native capture and Public invite fail until the
// helpers exist at the packaged paths. This script is the development stand-in
// for the packaging flow, not a release step.
//
// The tunnel helper defaults to the release pinned in app-package-targets.mjs
// and is downloaded into the ignored build cache with its digest verified.
// `--tunnel <path>` stages an already-downloaded binary instead; it is then
// checked to be a Windows x64 PE and reported when it differs from the pin.

import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  closeSync,
  copyFileSync,
  createWriteStream,
  existsSync,
  mkdirSync,
  openSync,
  readFileSync,
  readSync,
  realpathSync,
} from "node:fs";
import { basename, dirname, join, resolve } from "node:path";
import { Readable } from "node:stream";
import { pipeline } from "node:stream/promises";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";

import { appPackageTarget, CLOUDFLARED_VERSION } from "./app-package-targets.mjs";

function bytesAt(path, length, position = 0) {
  const descriptor = openSync(path, "r");
  try {
    const bytes = Buffer.alloc(length);
    return bytes.subarray(0, readSync(descriptor, bytes, 0, length, position));
  } finally {
    closeSync(descriptor);
  }
}

// Windows x64 PE: MZ header, PE signature and AMD64 machine.
export function isWindowsAmd64(path) {
  const header = bytesAt(path, 64);
  if (header.length < 64 || header[0] !== 0x4d || header[1] !== 0x5a) return false;
  const pe = bytesAt(path, 6, header.readUInt32LE(0x3c));
  return pe.length === 6 && pe.subarray(0, 4).equals(Buffer.from("PE\0\0")) &&
    pe.readUInt16LE(4) === 0x8664;
}

export function sha256(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

function run(command, args, cwd) {
  const result = spawnSync(command, args, {
    cwd, encoding: "utf8", windowsHide: true, timeout: 120_000,
  });
  if (result.error) throw result.error;
  return result;
}

function download(url, path, fetchImpl = fetch) {
  return (async () => {
    const response = await fetchImpl(url);
    if (!response.ok || !response.body) {
      throw new Error(`Public tunnel download failed with HTTP ${response.status}`);
    }
    mkdirSync(dirname(path), { recursive: true });
    await pipeline(Readable.fromWeb(response.body), createWriteStream(path));
  })();
}

// The pinned tunnel binary, from the ignored build cache. A cached file whose
// digest drifted is replaced rather than trusted, so an interrupted download
// cannot be staged.
export async function pinnedTunnel(repositoryRoot, target, fetchImpl = fetch) {
  const cache = join(repositoryRoot, "build", "app-runtime-cache", target.tunnelAsset);
  if (!existsSync(cache) || sha256(cache) !== target.tunnelSha256) {
    process.stdout.write(`Downloading cloudflared ${CLOUDFLARED_VERSION}\n`);
    await download(
      `https://github.com/cloudflare/cloudflared/releases/download/${CLOUDFLARED_VERSION}/${target.tunnelAsset}`,
      cache,
      fetchImpl,
    );
    if (sha256(cache) !== target.tunnelSha256) {
      throw new Error("Public tunnel digest does not match the pinned release");
    }
  }
  return cache;
}

// Capture helpers are built by the repository check into its stable directory;
// build there once rather than duplicating its configuration here.
export function captureHelper(repositoryRoot, target) {
  const helper = join(repositoryRoot, "build", "rust-check", target.captureName);
  if (!existsSync(helper)) {
    process.stdout.write("Building the native capture helper\n");
    const result = run(process.execPath,
      [join(repositoryRoot, "scripts", "check-rust.mjs"), "--capture-only"], repositoryRoot);
    if (result.status !== 0) {
      throw new Error((result.stderr || result.stdout).trim() || "Native capture build failed");
    }
  }
  if (!isWindowsAmd64(helper)) {
    throw new Error(`Native capture helper is not a ${target.id} executable`);
  }
  return helper;
}

// Copies both helpers to the packaged layout under `profileRoot` and makes each
// one answer its own identity check, so a staged binary that cannot run fails
// here rather than at the App's first capture or Public invite.
export function stageRuntime({ profileRoot, target, tunnel, capture }) {
  if (!isWindowsAmd64(tunnel)) {
    throw new Error(`Public tunnel is not a ${target.id} executable: ${tunnel}`);
  }
  if (!isWindowsAmd64(capture)) {
    throw new Error(`Native capture helper is not a ${target.id} executable: ${capture}`);
  }
  const nativeRoot = join(profileRoot, "runtime", "native");
  const tunnelRoot = join(profileRoot, "runtime", "tunnel");
  mkdirSync(nativeRoot, { recursive: true });
  mkdirSync(tunnelRoot, { recursive: true });
  const packagedCapture = join(nativeRoot, target.captureName);
  const packagedTunnel = join(tunnelRoot, target.tunnelName);
  copyFileSync(capture, packagedCapture);
  copyFileSync(tunnel, packagedTunnel);
  const version = run(packagedTunnel, ["--version"], profileRoot);
  if (version.status !== 0) throw new Error(`${basename(packagedTunnel)} did not report its version`);
  const probe = run(packagedCapture, ["--probe"], profileRoot);
  if (probe.status !== 0) {
    throw new Error(
      `${basename(packagedCapture)} failed its capability probe: ${(probe.stderr || probe.stdout).trim()}`,
    );
  }
  let protocol = null;
  try {
    protocol = JSON.parse(probe.stdout)?.protocol ?? null;
  } catch {
    throw new Error(`${basename(packagedCapture)} returned an invalid capability probe`);
  }
  if (protocol !== 7) {
    throw new Error(`${basename(packagedCapture)} probe protocol is ${protocol}, not 7`);
  }
  return {
    nativeCapture: packagedCapture,
    publicTunnel: packagedTunnel,
    cloudflared: (version.stdout || version.stderr).trim().split("\n")[0],
    captureProtocol: protocol,
  };
}

export async function prepareAppRuntime({
  repositoryRoot,
  targetDirectory,
  profile,
  tunnelPath = "",
  capturePath = "",
  fetchImpl = fetch,
}) {
  const target = appPackageTarget("windows-amd64");
  if (!target) throw new Error("App package target windows-amd64 is unavailable");
  const profileRoot = join(targetDirectory, profile);
  const app = join(profileRoot, target.appName);
  if (!existsSync(app)) {
    throw new Error(`Build the App first: cargo build${profile === "release" ? " --release" : ""} -p piik-app`);
  }
  let tunnel;
  if (tunnelPath) {
    tunnel = realpathSync(resolve(tunnelPath));
    if (!isWindowsAmd64(tunnel)) throw new Error(`Public tunnel is not a ${target.id} executable: ${tunnel}`);
    const digest = sha256(tunnel);
    if (digest !== target.tunnelSha256) {
      process.stdout.write(
        `Staging a public tunnel that differs from the pinned ${CLOUDFLARED_VERSION} release: ${digest}\n`,
      );
    }
  } else {
    if (process.platform !== "win32" || process.arch !== "x64") {
      throw new Error(`App runtime staging requires the ${target.id} host`);
    }
    tunnel = await pinnedTunnel(repositoryRoot, target, fetchImpl);
  }
  const capture = capturePath
    ? realpathSync(resolve(capturePath))
    : captureHelper(repositoryRoot, target);
  const staged = stageRuntime({ profileRoot, target, tunnel, capture });
  return { app, ...staged };
}

async function main() {
  const { values } = parseArgs({
    args: process.argv.slice(2),
    options: {
      target: { type: "string", default: "release" },
      tunnel: { type: "string" },
      capture: { type: "string" },
    },
  });
  if (!["release", "debug"].includes(values.target)) {
    throw new Error(
      "Usage: node scripts/prepare-app-runtime.mjs [--target release|debug] [--tunnel <path>] [--capture <path>]",
    );
  }
  const repositoryRoot = realpathSync(resolve(dirname(fileURLToPath(import.meta.url)), "..", ".."));
  const targetDirectory = resolve(repositoryRoot, process.env.CARGO_TARGET_DIR?.trim() || "target");
  const result = await prepareAppRuntime({
    repositoryRoot,
    targetDirectory,
    profile: values.target,
    tunnelPath: values.tunnel,
    capturePath: values.capture,
  });
  process.stdout.write(`${JSON.stringify(result)}\n`);
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  await main();
}
