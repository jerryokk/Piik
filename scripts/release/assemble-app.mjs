#!/usr/bin/env node

import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  closeSync,
  copyFileSync,
  cpSync,
  existsSync,
  lstatSync,
  mkdirSync,
  openSync,
  readdirSync,
  readSync,
  readFileSync,
  realpathSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { dirname, isAbsolute, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { appPackageTarget, cargoCommand, cargoRelease, CLOUDFLARED_VERSION } from "./app-package-targets.mjs";
import { writeAppLicenseNotices } from "./package-licenses.mjs";
import { extractZip } from "./archive-tool.mjs";
import { assertCleanRevision, resetBuildWorkspace } from "./build-workspace.mjs";
import { isReleaseVersion } from "./release-version.mjs";

function fail(message) {
  throw new Error(message);
}

function run(command, args, cwd, environment = process.env) {
  const result = spawnSync(command, args, {
    cwd,
    env: environment,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    fail(`${command} failed: ${[result.stdout, result.stderr].filter(Boolean).join("\n").trim()}`);
  }
  return result.stdout.trim();
}

function runNpm(args, cwd) {
  if (process.platform === "win32") {
    run(process.env.ComSpec || "cmd.exe", [
      "/d", "/s", "/c", `npm ${args.join(" ")}`,
    ], cwd);
    return;
  }
  run("npm", args, cwd);
}

function sha256(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

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
function executableMatchesTarget(path) {
  const header = bytesAt(path, 64);
  if (header.length < 64 || header[0] !== 0x4d || header[1] !== 0x5a) {
    return false;
  }
  const pe = bytesAt(path, 6, header.readUInt32LE(0x3c));
  return pe.length === 6 && pe.subarray(0, 4).equals(Buffer.from("PE\0\0")) &&
    pe.readUInt16LE(4) === 0x8664;
}

function assertTargetExecutable(path, target, label) {
  if (!lstatSync(path).isFile() || !executableMatchesTarget(path)) {
    fail(`${label} does not match App package target ${target.id}`);
  }
}

function plainName(value, label) {
  if (
    typeof value !== "string" ||
    !/^[A-Za-z0-9._-]+$/.test(value) ||
    value === "." ||
    value === ".."
  ) {
    fail(`Release ${label} is invalid`);
  }
  return value;
}

function readDescriptor(path) {
  let value;
  try {
    value = JSON.parse(readFileSync(path, "utf8"));
  } catch {
    fail("Server release descriptor is invalid");
  }
  if (
    value?.schema !== 2 ||
    (value.version !== "development" && !isReleaseVersion(value.version)) ||
    typeof value.revision !== "string" ||
    !/^[0-9a-f]{40}$/.test(value.revision) ||
    typeof value.artifactSha256 !== "string" ||
    !/^[0-9a-f]{64}$/.test(value.artifactSha256)
  ) {
    fail("Server release descriptor is invalid");
  }
  return {
    version: value.version,
    revision: value.revision,
    artifact: plainName(value.artifact, "artifact"),
    artifactSha256: value.artifactSha256,
  };
}

function assertOutputDirectory(repositoryRoot, outputRoot, target) {
  const path = relative(repositoryRoot, outputRoot);
  const candidateOutput = join(repositoryRoot, "build", "app-package", target.id,
    "candidate", `piik-app-${target.id}`);
  if (outputRoot !== candidateOutput &&
      (path === "" || (path.split(/[\\/]/)[0] !== ".." && !isAbsolute(path)))) {
    fail("App output must be outside the repository or its exact candidate workspace");
  }
  if (outputRoot === candidateOutput) {
    for (let parent = dirname(outputRoot); parent !== repositoryRoot; parent = dirname(parent)) {
      const metadata = lstatSync(parent, { throwIfNoEntry: false });
      if (metadata && (!metadata.isDirectory() || metadata.isSymbolicLink() ||
          realpathSync(parent) !== parent)) fail("App candidate output must not contain links");
    }
  }
  if (existsSync(outputRoot)) {
    fail("App output directory must not already exist");
  }
}

const positional = process.argv.slice(2, 4);
const options = process.argv.slice(4);
if (positional.length !== 2 || options.length % 2 !== 0) {
  fail(
    "Usage: node scripts/assemble-app.mjs <server-release.json> <new-output-directory> --target <target> [--capture <executable>] [--tunnel <executable>]",
  );
}

let targetArgument = null;
let captureArgument = null;
let tunnelArgument = null;
for (let index = 0; index < options.length; index += 2) {
  const name = options[index];
  const value = options[index + 1];
  if (
    !value ||
    (name !== "--target" && name !== "--capture" && name !== "--tunnel")
  ) {
    fail("App package option is invalid");
  }
  if (name === "--target" && targetArgument === null) {
    targetArgument = value;
  } else if (name === "--capture" && captureArgument === null) {
    captureArgument = value;
  } else if (name === "--tunnel" && tunnelArgument === null) {
    tunnelArgument = value;
  } else {
    fail("App package option is duplicated");
  }
}
const target = appPackageTarget(targetArgument);
if (!target) fail("App package target is invalid");

const repositoryRoot = realpathSync(resolve(dirname(fileURLToPath(import.meta.url)), "..", ".."));
const descriptorPath = realpathSync(resolve(positional[0]));
const outputRoot = resolve(process.cwd(), positional[1]);
const capturePath = captureArgument ? realpathSync(resolve(captureArgument)) : null;
const tunnelPath = tunnelArgument ? realpathSync(resolve(tunnelArgument)) : null;
assertOutputDirectory(repositoryRoot, outputRoot, target);
if (capturePath && !target.captureName) {
  fail("Capture runtime is invalid for the App package target");
}
if (capturePath) assertTargetExecutable(capturePath, target, "Capture runtime");
if (tunnelPath) assertTargetExecutable(tunnelPath, target, "Public tunnel runtime");

const descriptor = readDescriptor(descriptorPath);
const version = descriptor.version;
process.env.VITE_PIIK_VERSION = version;
process.env.VITE_PIIK_REVISION = descriptor.revision;
const revision = assertCleanRevision(repositoryRoot, descriptor.revision);
const artifactPath = join(dirname(descriptorPath), descriptor.artifact);
if (!existsSync(artifactPath) || sha256(artifactPath) !== descriptor.artifactSha256) {
  fail("Server release artifact does not match its descriptor");
}

const temporaryRoot = resetBuildWorkspace(repositoryRoot, "app-package", target.id, "assembly");
const packageRoot = join(temporaryRoot, "package");
const runtimeRoot = packageRoot;
const releaseRoot = join(temporaryRoot, "release");
let outputOwned = false;
try {
  // The Server release is consumed for its provenance only: the App
  // embeds the Web assets, so the revision, clean-tree and digest assertions
  // above plus this REVISION check tie the binary to that immutable release.
  mkdirSync(releaseRoot, { recursive: true });
  extractZip(artifactPath, releaseRoot);
  if (readFileSync(join(releaseRoot, "REVISION"), "ascii") !== `${revision}\n`) {
    fail("Extracted Server revision does not match the App");
  }
  // Build the Web assets the App binary embeds (Tauri frontendDist). The
  // checkout is clean and at the release revision, so this reproduces that
  // release's Web assets.
  runNpm(["run", "build:web"], repositoryRoot);
  mkdirSync(runtimeRoot, { recursive: true });

  let packagedCapture = null;
  if (capturePath) {
    const nativeRoot = join(runtimeRoot, "runtime", "native");
    mkdirSync(nativeRoot, { recursive: true });
    packagedCapture = join(nativeRoot, target.captureName);
    copyFileSync(capturePath, packagedCapture);
  }

  let packagedTunnel = null;
  if (tunnelPath) {
    const tunnelRoot = join(runtimeRoot, "runtime", "tunnel");
    mkdirSync(tunnelRoot, { recursive: true });
    const tunnelName = target.tunnelName;
    packagedTunnel = join(tunnelRoot, tunnelName);
    copyFileSync(tunnelPath, packagedTunnel);
  }

  const appName = target.appName;
  const appPath = join(runtimeRoot, appName);
  const build = cargoRelease(repositoryRoot, "piik-app", { version, revision });
  run(cargoCommand(), build.args, repositoryRoot, build.env);
  copyFileSync(build.executable, appPath);
  assertTargetExecutable(appPath, target, "App executable");
  writeAppLicenseNotices(repositoryRoot, runtimeRoot, target,
    packagedTunnel ? CLOUDFLARED_VERSION : null);
  writeFileSync(join(runtimeRoot, "REVISION"), `${revision}\n`, "ascii");

  assertCleanRevision(repositoryRoot, revision);
  mkdirSync(dirname(outputRoot), { recursive: true });
  mkdirSync(outputRoot, { recursive: false, mode: 0o700 });
  outputOwned = true;
  for (const entry of readdirSync(packageRoot)) {
    cpSync(join(packageRoot, entry), join(outputRoot, entry), {
      recursive: true,
      errorOnExist: true,
      force: false,
    });
  }
  process.stdout.write(`${JSON.stringify({
    root: outputRoot,
    version,
    revision,
    target: target.id,
    rustTarget: target.rustTarget,
    app: relative(packageRoot, appPath).replaceAll("\\", "/"),
    nativeCapture: packagedCapture ? relative(packageRoot, packagedCapture).replaceAll("\\", "/") : null,
    publicTunnel: packagedTunnel ? relative(packageRoot, packagedTunnel).replaceAll("\\", "/") : null,
  })}\n`);
} catch (error) {
  if (outputOwned) rmSync(outputRoot, { recursive: true, force: true });
  throw error;
} finally {
  rmSync(temporaryRoot, { recursive: true, force: true });
}
