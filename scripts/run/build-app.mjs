#!/usr/bin/env node

// Builds everything needed to run Piik, without starting anything.
//
// Use this once after cloning or switching revisions: it produces the Browser
// UI in `web/dist`, both Rust binaries and the native capture helper, and
// stages the helpers beside the App. `npm run dev` + `npm run dev:server` then
// need no further build, and `npm run app` can skip its own build.
//
// The capture helper needs MSVC and downloads its pinned WebRTC SDK on first
// run; skip it with `--no-native` when only the Browser UI is of interest.

import { spawnSync } from "node:child_process";
import { parseArgs } from "node:util";

import { profile, run, runNpm } from "../lib/process.mjs";

const { values } = parseArgs({ options: { "no-native": { type: "boolean" } } });
const root = process.env.PIIK_ROOT ?? process.cwd();

runNpm(root, ["run", "build:web"], "Building the Browser UI");
run(root, "cargo", ["build", "-p", "piik-server", "-p", "piik-app"], "Building the Rust binaries");

if (values["no-native"]) {
  console.log("\nSkipping the native capture helper (--no-native).");
} else {
  run(root, process.execPath, ["scripts/checks/check-rust.mjs", "--capture-only"], "Building the native capture helper");
}
runNpm(root, ["run", "app:runtime", "--", "--target", profile()], "Staging helpers beside the App");

console.log(`
Ready.

  npm run dev          Browser UI on http://localhost:8787
  npm run dev:server   Hosted Server on port 8788 (second terminal)
  npm run app          Piik App

  npm run app -- --no-build   reuse this build
`);
