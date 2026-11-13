#!/usr/bin/env node

// Starts the Hosted Server for local development, paired with `npm run dev`.
//
// `npm run dev` serves the Browser UI on 8787 and proxies /api, /healthz and
// /signal to 8788, so this process listens on 8788 and advertises 8787 as its
// public origin. Starting the binary directly would leave the two ports
// disagreeing and every invitation unreachable from the browser.
//
// Extra arguments pass through to the server, e.g. `npm run dev:server -- --debug`.

import { spawnSync } from "node:child_process";
import { parseArgs } from "node:util";

import { profile, requireBinary } from "../lib/process.mjs";

const { positionals } = parseArgs({ allowPositionals: true });
const root = process.env.PIIK_ROOT ?? process.cwd();
const port = process.env.PORT ?? "8788";
const publicBaseUrl = process.env.PUBLIC_BASE_URL ?? "http://localhost:8787";

const executable = requireBinary(root, "target", profile(), "piik-server.exe");

console.log(`piik-server  listening on ${port}`);
console.log(`public origin ${publicBaseUrl}  (run \`npm run dev\` for the Browser UI)\n`);

const result = spawnSync(executable, positionals, {
  cwd: root,
  stdio: "inherit",
  env: { ...process.env, PORT: port, PUBLIC_BASE_URL: publicBaseUrl },
});
if (result.error) throw result.error;
process.exit(result.status ?? 1);
