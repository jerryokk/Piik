#!/usr/bin/env node

// Runs the checks a change must pass before it is proposed, in increasing cost.
//
//   check:web      types, unit tests and the production build
//   check:docs     documentation links and ownership
//   check:size     per-file size limits
//   check:rust     format, Clippy with warnings denied, workspace tests, and
//                  the native capture build with its probe
//   check:hygiene  tracked paths, whitespace, and the two checks above again
//
// Browser and network acceptance live in the `gate:*` commands; they need real
// hardware or a reachable network and are deliberately not part of this run.

import { run, runNpm } from "../lib/process.mjs";

const root = process.env.PIIK_ROOT ?? process.cwd();

runNpm(root, ["run", "check:web"], "Web: types, tests, production build");
run(root, process.execPath, ["scripts/checks/check-docs.mjs"], "Docs: links and ownership");
run(root, process.execPath, ["scripts/checks/check-source-size.mjs"], "Source: file size limits");
runNpm(root, ["run", "check:rust"], "Rust: format, Clippy, workspace tests, native capture");
runNpm(root, ["run", "check:hygiene"], "Repository hygiene: tracked paths and whitespace");

console.log("\nAll checks passed.");
