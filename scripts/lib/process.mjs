// Child-process helpers shared by the `scripts/run` entry points.
//
// Windows cannot `spawnSync` an `.cmd` shim directly (EINVAL since Node 20),
// and `shell: true` mangles quoted arguments. Calling npm's JS entry with the
// current Node executable avoids both.

import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { dirname, join } from "node:path";

const npmCli = join(dirname(process.execPath), "node_modules", "npm", "bin", "npm-cli.js");

/** Runs one step, inheriting stdio, and exits the caller on failure. */
export function run(root, command, args, label) {
  process.stdout.write(`\n▶ ${label}\n`);
  const result = spawnSync(command, args, { cwd: root, stdio: "inherit" });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    console.error(`\n✗ ${label} failed (exit ${result.status}).`);
    process.exit(result.status ?? 1);
  }
}

/** Runs an npm script through the Node executable, never through a `.cmd` shim. */
export function runNpm(root, args, label) {
  if (!existsSync(npmCli)) {
    process.stdout.write(`\n▶ ${label}\n`);
    // Fall back to a shell invocation on installations without the JS entry.
    const result = spawnSync("npm", args, { cwd: root, stdio: "inherit", shell: true });
    if (result.error) throw result.error;
    if (result.status !== 0) {
      console.error(`\n✗ ${label} failed (exit ${result.status}).`);
      process.exit(result.status ?? 1);
    }
    return;
  }
  run(root, process.execPath, [npmCli, ...args], label);
}

/** The debug or release Cargo profile selected by `PIIK_PROFILE`. */
export function profile() {
  return process.env.PIIK_PROFILE === "release" ? "release" : "debug";
}

/** A binary this script produced, or an actionable failure. */
export function requireBinary(root, ...segments) {
  const path = join(root, ...segments);
  if (!existsSync(path)) {
    console.error(`Missing ${path}\n\nBuild it first:\n  npm run build:app`);
    process.exit(1);
  }
  return path;
}
