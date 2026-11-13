#!/usr/bin/env node

import { spawnSync } from "node:child_process";
import {
  existsSync,
  mkdirSync,
  realpathSync,
} from "node:fs";
import { basename, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { cargoCommand } from "../release/app-package-targets.mjs";

// piik-service embeds the Vite output and the App's Tauri build reads it as
// frontendDist, so every Cargo step needs it. App and Server build for
// Windows x64.
const WEB_INDEX = join("web", "dist", "index.html");

const root = realpathSync(resolve(dirname(fileURLToPath(import.meta.url)), "..", ".."));
const captureRoot = join(root, "native", "capture");
// Stable build paths keep Windows firewall identities fixed between runs.
const buildRoot = join(root, "build", "rust-check");
const mode = process.argv[2] ?? "--all";

if (!["--all", "--core", "--capture-only"].includes(mode)) {
  throw new Error("Usage: node scripts/checks/check-rust.mjs [--all|--core|--capture-only]");
}

function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: options.cwd ?? root,
    env: options.env ?? process.env,
    encoding: "utf8",
    stdio: options.capture ? ["ignore", "pipe", "pipe"] : "inherit",
  });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    const detail = options.capture
      ? [result.stdout, result.stderr].filter(Boolean).join("\n").trim()
      : "";
    throw new Error(`${basename(command)} failed${detail ? `: ${detail}` : ""}`);
  }
  return options.capture ? result.stdout.trim() : "";
}

function buildWebAssets() {
  if (existsSync(join(root, WEB_INDEX))) return;
  run(process.env.ComSpec || "cmd.exe", ["/d", "/s", "/c", "npm run build:web"]);
}

function checkCore() {
  buildWebAssets();
  const cargo = cargoCommand();
  run(cargo, ["fmt", "--all", "--check"]);
  run(cargo, ["clippy", "--workspace", "--all-targets", "--locked", "--", "-D", "warnings"]);
  // Test executables run from the workspace target directory, a stable path.
  run(cargo, ["test", "--workspace", "--locked"]);
  // Optional advisory check: run cargo-audit only when it is installed.
  const audit = spawnSync(cargo, ["audit", "--version"], { cwd: root, stdio: "ignore" });
  if (audit.status === 0) run(cargo, ["audit"]);
}

function checkPlatformCapture() {
  if (process.platform !== "win32") {
    throw new Error("Native capture builds only on Windows");
  }
  mkdirSync(buildRoot, { recursive: true });
  const systemPowerShell = join(
    process.env.SystemRoot || "C:\\Windows",
    "System32",
    "WindowsPowerShell",
    "v1.0",
    "powershell.exe",
  );
  const modernPowerShell = join(
    process.env.ProgramFiles || "C:\\Program Files",
    "PowerShell",
    "7",
    "pwsh.exe",
  );
  const powershell = process.env.PIIK_POWERSHELL?.trim() ||
    (existsSync(modernPowerShell)
      ? modernPowerShell
      : (existsSync(systemPowerShell) ? systemPowerShell : "pwsh"));
  run(powershell, [
    "-NoProfile",
    "-ExecutionPolicy",
    "Bypass",
    "-File",
    join(captureRoot, "windows", "build.ps1"),
    "-OutputDirectory",
    buildRoot,
    "-Check",
  ]);
  const executable = join(buildRoot, "piik-capture.exe");
  if (!existsSync(executable)) {
    throw new Error("Native capture build did not produce its executable");
  }
  const raw = run(executable, ["--probe"], { capture: true });
  const probe = JSON.parse(raw);
  if (
    probe?.protocol !== 7 ||
    probe.platform !== "windows" ||
    typeof probe.platformBuild !== "string" ||
    typeof probe.videoCapture !== "boolean" ||
    typeof probe.softwareVP8 !== "boolean" ||
    typeof probe.processAudio !== "boolean" ||
    typeof probe.systemAudio !== "boolean" ||
    !Array.isArray(probe.adapters) ||
    probe.adapters.some((adapter) =>
      !Number.isInteger(adapter?.index) ||
      typeof adapter?.name !== "string" ||
      typeof adapter?.identity !== "string" ||
      !Array.isArray(adapter?.hardwareH264) ||
      adapter.hardwareH264.some((encoder) =>
        !Number.isInteger(encoder?.index) ||
        typeof encoder?.name !== "string" ||
        typeof encoder?.identity !== "string"
      )
    )
  ) {
    throw new Error("Native capture probe returned an invalid contract");
  }
  const sources = JSON.parse(run(executable, ["--list"], { capture: true }));
  if (!Array.isArray(sources) || sources.some((target) =>
    !["window", "display", "picker"].includes(target?.kind) ||
    !/^[1-9][0-9]{0,19}$/.test(target?.sourceId) ||
    typeof target?.title !== "string" ||
    (target.kind === "window" &&
      (!Number.isInteger(target?.pid) ||
        !/^[1-9][0-9]{0,19}$/.test(target?.creationTime))) ||
    (target.kind === "display" &&
      (target?.pid !== undefined || target?.creationTime !== undefined)) ||
    (target.kind === "picker" &&
      (target?.pid !== undefined || target?.creationTime !== undefined))
  )) {
    throw new Error("Native capture process returned an invalid source list");
  }
}

if (mode !== "--capture-only") checkCore();
if (mode !== "--core") checkPlatformCapture();
process.stdout.write("Piik checks passed.\n");
