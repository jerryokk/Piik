#!/usr/bin/env node

import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  copyFileSync,
  existsSync,
  lstatSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  realpathSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { dirname, isAbsolute, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";

import { cargoCommand, cargoRelease } from "./app-package-targets.mjs";
import { writeServerLicenseNotices } from "./package-licenses.mjs";
import { createZip, extractZip } from "./archive-tool.mjs";
import { assertCleanRevision, resetBuildWorkspace } from "./build-workspace.mjs";
import { buildVersion } from "./release-version.mjs";
import { SERVER_PACKAGE_TARGETS } from "./server-package-targets.mjs";

const SERVER_BINARY = "piik-server.exe";

// Everything the archive may contain.
const RUNTIME_PATHS = ["REVISION", "LICENSE", "THIRD-PARTY-NOTICES.txt", SERVER_BINARY];

// The Vite output the server embeds (piik-service's build script reads it at
// compile time, so it is built first). Its index.html names the main asset the
// deployment postflight proves over HTTP.
const WEB_DIST = "web/dist";

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
    fail(`${command} failed: ${(result.stderr || result.stdout).trim()}`);
  }
  return result.stdout.trim();
}

function buildWebAssets(cwd) {
  run(process.env.ComSpec || "cmd.exe", ["/d", "/s", "/c", "npm run build:web"], cwd);
}

function sha256(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

function normalizedRelative(root, path) {
  const value = relative(root, path).split(sep).join("/");
  if (
    value.length === 0 ||
    value.startsWith("../") ||
    value.includes("\\") ||
    value.split("/").some((part) => part === "" || part === "." || part === "..")
  ) {
    fail(`Invalid runtime path: ${value}`);
  }
  return value;
}

function regularFiles(root) {
  const files = [];
  const pending = [root];
  while (pending.length > 0) {
    const directory = pending.pop();
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      const absolute = join(directory, entry.name);
      const metadata = lstatSync(absolute);
      if (metadata.isSymbolicLink()) fail(`Runtime contains a link: ${absolute}`);
      if (metadata.isDirectory()) {
        pending.push(absolute);
      } else if (metadata.isFile()) {
        files.push(absolute);
      } else {
        fail(`Runtime contains an unsupported file type: ${absolute}`);
      }
    }
  }
  return files.sort((left, right) =>
    normalizedRelative(root, left).localeCompare(normalizedRelative(root, right)),
  );
}

function recordsFor(root, paths) {
  return regularFiles(root).map((absolute) => {
    const path = normalizedRelative(root, absolute);
    if (!paths.includes(path)) fail(`Unexpected packaged file: ${path}`);
    return { path, size: statSync(absolute).size, sha256: sha256(absolute) };
  });
}

function compareRecords(expected, actual) {
  if (expected.length !== actual.length) fail("Extracted runtime file count differs");
  for (let index = 0; index < expected.length; index += 1) {
    const left = expected[index];
    const right = actual[index];
    if (
      left.path !== right.path ||
      left.size !== right.size ||
      left.sha256 !== right.sha256
    ) {
      fail(`Extracted runtime differs at ${left.path}`);
    }
  }
}

// mainAsset is the hashed entry script Vite named in the embedded index.html.
// The deployment postflight proves it over HTTP, so it is read from the same
// build the binary embeds rather than from the archive.
function mainAssetOf(distributionRoot) {
  const html = readFileSync(join(distributionRoot, "index.html"), "utf8");
  const match = html.match(/<script[^>]+src="\/(assets\/index-[A-Za-z0-9_-]+\.js)"/);
  if (!match) fail("Built Web HTML has no unique main asset");
  if (!existsSync(join(distributionRoot, ...match[1].split("/")))) {
    fail("Built Web main asset is missing");
  }
  return match[1];
}

const { positionals } = parseArgs({ allowPositionals: true, options: {} });
const target = SERVER_PACKAGE_TARGETS[0];
if (positionals.length !== 1) {
  fail("Usage: node scripts/package-server-release.mjs <new-output-directory>");
}

const repositoryRoot = realpathSync(resolve(dirname(fileURLToPath(import.meta.url)), "..", ".."));
const outputRoot = resolve(process.cwd(), positionals[0]);
const relativeOutput = relative(repositoryRoot, outputRoot);
if (relativeOutput === "" || (relativeOutput.split(/[\\/]/)[0] !== ".." && !isAbsolute(relativeOutput))) {
  fail("Output directory must be outside the repository");
}
if (existsSync(outputRoot)) fail("Output directory must not already exist");

const revision = assertCleanRevision(repositoryRoot);
const version = buildVersion(repositoryRoot, revision);
process.env.VITE_PIIK_VERSION = version;
process.env.VITE_PIIK_REVISION = revision;

if (!existsSync(join(repositoryRoot, "LICENSE"))) fail("Missing release input: LICENSE");
buildWebAssets(repositoryRoot);
assertCleanRevision(repositoryRoot, revision);
const mainAsset = mainAssetOf(join(repositoryRoot, WEB_DIST));

const releaseId = revision.slice(0, 7);
const temporaryRoot = resetBuildWorkspace(repositoryRoot, "server-package", "assembly");
const runtimeRoot = join(temporaryRoot, "runtime");
const verifyRoot = join(temporaryRoot, "verify");
let outputOwned = false;

try {
  mkdirSync(runtimeRoot, { recursive: true });
  copyFileSync(join(repositoryRoot, "LICENSE"), join(runtimeRoot, "LICENSE"));
  const serverPath = join(runtimeRoot, SERVER_BINARY);
  const build = cargoRelease(repositoryRoot, "piik-server", { version, revision });
  run(cargoCommand(), build.args, repositoryRoot, build.env);
  copyFileSync(build.executable, serverPath);
  writeServerLicenseNotices(repositoryRoot, join(runtimeRoot, "THIRD-PARTY-NOTICES.txt"));
  assertCleanRevision(repositoryRoot, revision);
  writeFileSync(join(runtimeRoot, "REVISION"), `${revision}\n`, "ascii");

  const records = recordsFor(runtimeRoot, RUNTIME_PATHS);
  const stem = `piik-${releaseId}${target.suffix}`;
  const manifestName = `${stem}.manifest.tsv`;
  const artifactName = `${stem}-runtime.zip`;
  const descriptorName = `${stem}.release.json`;
  mkdirSync(outputRoot, { recursive: false, mode: 0o700 });
  outputOwned = true;
  const manifestPath = join(outputRoot, manifestName);
  const artifactPath = join(outputRoot, artifactName);
  const descriptorPath = join(outputRoot, descriptorName);
  writeFileSync(
    manifestPath,
    records.map((record) => `${record.sha256}\t${record.size}\t${record.path}\n`).join(""),
    "ascii",
  );

  createZip(runtimeRoot, artifactPath);
  mkdirSync(verifyRoot);
  extractZip(artifactPath, verifyRoot);
  compareRecords(records, recordsFor(verifyRoot, RUNTIME_PATHS));

  const descriptor = {
    schema: 2,
    version,
    revision,
    releaseId,
    target: target.id,
    artifact: artifactName,
    artifactSha256: sha256(artifactPath),
    manifest: manifestName,
    manifestSha256: sha256(manifestPath),
    fileCount: records.length,
    mainAsset,
  };
  assertCleanRevision(repositoryRoot, revision);
  writeFileSync(descriptorPath, `${JSON.stringify(descriptor, null, 2)}\n`, "ascii");
  process.stdout.write(`${JSON.stringify({ descriptor: descriptorPath, ...descriptor })}\n`);
} catch (error) {
  if (outputOwned) rmSync(outputRoot, { recursive: true, force: true });
  throw error;
} finally {
  rmSync(temporaryRoot, { recursive: true, force: true });
}
