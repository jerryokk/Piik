#!/usr/bin/env node

// Builds and starts Piik App in one step, with its helpers in place.
//
// A Cargo build writes only `piik-app.exe`; the App looks for its helpers at
// `runtime/native/piik-capture.exe` beside itself, so native capture and
// Public invite stay unavailable until they are staged. `npm run app:runtime`
// stages them, which is why this runs it before starting.
//
// `--no-build` reuses the current build, for a restart after a source change
// that only affects the App. `--fresh` also clears the per-user configuration:
// a configuration written by a different build is rejected at startup (one
// current version, no migration) and the App has no console, so that failure
// looks like "it does not open". Clearing it is the recovery, not a step.

import { spawnSync } from "node:child_process";
import { existsSync, rmSync } from "node:fs";
import { join } from "node:path";

import { profile, requireBinary, run, runNpm } from "../lib/process.mjs";

// Only this script's own flags are consumed; everything else reaches the App,
// so `npm run app -- --debug --log-dir build\app-diag` works as documented.
const argv = process.argv.slice(2);
const values = {
  fresh: argv.includes("--fresh"),
  "no-build": argv.includes("--no-build"),
};
const positionals = argv.filter((arg) => arg !== "--fresh" && arg !== "--no-build");

const root = process.env.PIIK_ROOT ?? process.cwd();
const selected = profile();

if (values.fresh) {
  const configDir = join(process.env.APPDATA ?? "", "Piik");
  for (const name of ["client.json", "client.json.mode"]) {
    const path = join(configDir, name);
    if (existsSync(path)) {
      rmSync(path);
      console.log(`Removed ${path}`);
    }
  }
}

if (!values["no-build"]) {
  runNpm(root, ["run", "build:web"], "Building the Browser UI");
  run(root, "cargo", ["build", "-p", "piik-app", ...(selected === "release" ? ["--release"] : [])], "Building Piik App");
}
// Staging is cheap and must happen even with --no-build: the helper layout is
// what makes native capture available, and it can be missing after a clean.
runNpm(root, ["run", "app:runtime", "--", "--target", selected], "Staging helpers");

const executable = requireBinary(root, "target", selected, "piik-app.exe");
console.log(`\nStarting ${executable}\n`);

// The App owns its window and exits when it closes; keep this process attached
// so Ctrl+C in the terminal and the return code both reach the caller.
const result = spawnSync(executable, positionals, { cwd: root, stdio: "inherit" });
if (result.error) throw result.error;
process.exit(result.status ?? 0);
