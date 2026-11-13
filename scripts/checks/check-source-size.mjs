// Keeps one file from quietly growing into a second program. Production code
// must stay under 800 lines and an independent test file under 1000; a shared
// file list documents the few modules that legitimately exceed that today.
import { readFileSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { resolve } from "node:path";

const PRODUCTION = 800;
const TEST = 1000;
// A session owner whose state is shared by every branch of one state machine.
// Splitting it means turning its closures into an explicit object, which is a
// behaviour-risk rewrite of the live media path rather than a move. These two
// entries are the only accepted exceptions; anything new must stay under the
// limit.
const EXCEPTIONS = new Map([
  ["src/client/pages/viewer/use-viewer-session.ts", 1700],
  ["src/client/pages/HostPage.tsx", 4600],
]);
const root = resolve(import.meta.dirname, "..", "..");
const files = execFileSync("git", ["ls-files", "-z"], { cwd: root, encoding: "utf8" })
  .split("\0").filter((path) => /\.(rs|ts|tsx|mjs|cpp|h)$/.test(path));

const isTest = (path) => /\/tests?\//.test(path) || path.endsWith("tests.rs") || /\.test\./.test(path);
const limit = (path) => (isTest(path) ? TEST : PRODUCTION);

const over = [];
for (const path of files) {
  const lines = readFileSync(resolve(root, path), "utf8").split("\n").length - 1;
  const allowed = EXCEPTIONS.get(path) ?? limit(path);
  if (lines > allowed) over.push({ path, lines, limit: allowed });
}
if (over.length) {
  for (const { path, lines, limit } of over.sort((a, b) => b.lines - a.lines)) {
    console.error(`error: ${path}: ${lines} lines exceeds the ${limit}-line limit`);
  }
  console.error("Split by responsibility instead of extending one file.");
  process.exit(1);
}
console.log(`Source size check passed (${files.length} files).`);
