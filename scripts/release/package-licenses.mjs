import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { copyFileSync, existsSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { RUST_TARGET, cargoCommand } from "./app-package-targets.mjs";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..");
const pinned = JSON.parse(readFileSync(join(root, "licenses", "upstream.json"), "utf8"));
const licenseName = /^(licen[cs]e|copying|notice|copyright|authors)([._-]|$)/i;
// REUSE-style crates keep their texts in a LICENSES directory instead.
const licenseDirectory = /^licen[cs]es$/i;
const section = (label, body) => `\n===== ${label} =====\n\n${body.trim()}\n`;

// The packages the Vite bundle ships to the Browser. Nothing installs
// node_modules at runtime any more, so this list is the notice contract rather
// than package.json's dependencies field.
const WEB_BUNDLE_PACKAGES = ["@oddbird/popover-polyfill", "qrcode-generator", "react", "react-dom", "sdp-transform", "zod"];

// Both binaries embed the Web bundle, which serves its own notice file.
const WEB_NOTICE_LINE = "Web dependencies: served at /third-party-licenses.txt\n";

function pinnedNotice(key) {
  const entry = pinned[key];
  if (!entry) throw new Error(`Missing pinned license text for ${key}`);
  const bytes = readFileSync(join(root, "licenses", entry.file));
  if (createHash("sha256").update(bytes).digest("hex") !== entry.sha256) {
    throw new Error(`Pinned license text changed for ${key}`);
  }
  return `Source: ${entry.source}\n\n${bytes.toString("utf8")}`;
}

function noticesIn(directory) {
  return readdirSync(directory, { withFileTypes: true })
    .filter((file) => file.isFile() && licenseName.test(file.name))
    .map((file) => file.name).sort();
}

export function writeWebLicenseNotices(repositoryRoot, outputFile, packages = WEB_BUNDLE_PACKAGES) {
  const packagePath = join(repositoryRoot, "package.json");
  const lock = JSON.parse(readFileSync(join(repositoryRoot, "package-lock.json"), "utf8"));
  const queue = packages.map((name) => [name, packagePath]);
  const visited = new Set();
  const entries = new Map();
  for (const [name, parent] of queue) {
    const paths = createRequire(parent).resolve.paths("piik-license-lookup");
    const directory = paths.map((path) => join(path, name))
      .find((path) => existsSync(join(path, "package.json")));
    if (!directory) throw new Error(`Missing installed runtime dependency ${name}`);
    if (visited.has(directory)) continue;
    visited.add(directory);
    const path = join(directory, "package.json");
    const dependency = JSON.parse(readFileSync(path, "utf8"));
    const installed = relative(repositoryRoot, directory).replaceAll("\\", "/");
    if (lock.packages[installed]?.version !== dependency.version) {
      throw new Error(`Runtime dependency differs from lockfile: ${name}`);
    }
    const key = `${dependency.name}@${dependency.version}`;
    const files = noticesIn(directory);
    const body = files.length
      ? files.map((file) => section(file, readFileSync(join(directory, file), "utf8"))).join("")
      : pinnedNotice(key);
    entries.set(key, section(`${key} (${dependency.license ?? "see license text"})`, body));
    for (const child of Object.keys(dependency.dependencies ?? {})) queue.push([child, path]);
  }
  mkdirSync(dirname(outputFile), { recursive: true });
  writeFileSync(outputFile, "Third-party software notices\n" +
    "Piik's MIT license does not replace the following component licenses.\n" +
    [...entries].sort(([a], [b]) => a.localeCompare(b)).map(([, body]) => body).join(""));
}

// linkedPackages walks `cargo metadata` from the named workspace binaries
// along normal dependency edges only. Build scripts, dev-dependencies and
// proc-macros run on the build host; none of them is linked into the binary.
export function linkedPackages(metadata, rootNames) {
  const packages = new Map(metadata.packages.map((entry) => [entry.id, entry]));
  const nodes = new Map(metadata.resolve.nodes.map((node) => [node.id, node]));
  const members = new Set(metadata.workspace_members);
  const roots = rootNames.map((name) => {
    const id = metadata.workspace_members.find((member) => packages.get(member)?.name === name);
    if (!id) throw new Error(`Cargo workspace has no package ${name}`);
    return id;
  });
  const linked = new Set();
  for (const pending = [...roots]; pending.length > 0;) {
    const id = pending.pop();
    if (linked.has(id)) continue;
    linked.add(id);
    for (const dependency of nodes.get(id)?.deps ?? []) {
      if (!dependency.dep_kinds.some((edge) => edge.kind === null)) continue;
      const target = packages.get(dependency.pkg);
      if (target.targets.some((entry) => entry.kind.includes("proc-macro"))) continue;
      pending.push(dependency.pkg);
    }
  }
  return [...linked].filter((id) => !members.has(id)).map((id) => packages.get(id))
    .sort((a, b) => a.name.localeCompare(b.name) || a.version.localeCompare(b.version));
}

function crateLicenseFiles(crate) {
  const directory = dirname(crate.manifest_path);
  const files = new Map();
  const add = (path) => files.set(relative(directory, path).replaceAll("\\", "/"), readFileSync(path, "utf8"));
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    if (entry.isFile() && licenseName.test(entry.name)) add(path);
    if (entry.isDirectory() && licenseDirectory.test(entry.name)) {
      for (const file of readdirSync(path, { withFileTypes: true })) {
        if (file.isFile()) add(join(path, file.name));
      }
    }
  }
  if (crate.license_file) add(resolve(directory, crate.license_file));
  return files;
}

// rustNotices lists every third-party crate linked into the named binaries
// with its license expression and texts, plus the Rust standard library
// notice every binary statically links. A crate without license texts in its
// source or a pinned upstream copy fails closed.
function rustNotices(repositoryRoot, rootNames) {
  const options = { cwd: repositoryRoot, encoding: "utf8", windowsHide: true,
    maxBuffer: 256 * 1024 * 1024, stdio: ["ignore", "pipe", "pipe"] };
  const metadata = JSON.parse(execFileSync(cargoCommand(), ["metadata", "--format-version", "1",
    "--locked", "--offline", "--filter-platform", RUST_TARGET], options));
  let text = "";
  for (const crate of linkedPackages(metadata, rootNames)) {
    const key = `${crate.name}@${crate.version}`;
    const files = crateLicenseFiles(crate);
    const body = files.size
      ? [...files].sort(([a], [b]) => a.localeCompare(b)).map(([file, content]) => section(file, content)).join("")
      : pinnedNotice(key);
    text += section(`${key} (${crate.license ?? "see license text"})`, body);
  }
  const rustc = process.env.RUSTC?.trim() || "rustc";
  const sysroot = execFileSync(rustc, ["--print", "sysroot"], options).trim();
  const copyright = join(sysroot, "share", "doc", "rust", "COPYRIGHT-library.html");
  if (!existsSync(copyright)) throw new Error("Rust toolchain has no standard library notice");
  const toolchain = section(`Rust standard library (${execFileSync(rustc, ["-V"], options).trim()})`,
    htmlText(readFileSync(copyright, "utf8")));
  return toolchain + text;
}

// htmlText renders rustc's generated standard library notice as plain text.
function htmlText(html) {
  const entities = { amp: "&", lt: "<", gt: ">", quot: "\"", apos: "'", nbsp: " " };
  return html
    .replace(/<(head|style|script)[\s\S]*?<\/\1>/gi, "")
    .replace(/<\/(p|div|h\d|li|summary|pre)>|<br\s*\/?>/gi, "\n")
    .replace(/<[^>]+>/g, "")
    .replace(/&(#x[0-9a-f]+|#\d+|\w+);/gi, (match, name) => {
      if (name[0] !== "#") return entities[name.toLowerCase()] ?? match;
      return String.fromCodePoint(Number.parseInt(name.slice(name[1] === "x" ? 2 : 1), name[1] === "x" ? 16 : 10));
    })
    .replace(/[ \t]+$/gm, "")
    .replace(/\n{3,}/g, "\n\n");
}

// writeServerLicenseNotices produces the Server release notice file,
// which ships beside the single piik-server binary.
export function writeServerLicenseNotices(repositoryRoot, outputFile) {
  writeFileSync(outputFile, "Piik Server third-party software notices\n" +
    WEB_NOTICE_LINE + rustNotices(repositoryRoot, ["piik-server"]));
}

export function writeAppLicenseNotices(repositoryRoot, packageRoot, target, tunnelVersion) {
  copyFileSync(join(repositoryRoot, "LICENSE"), join(packageRoot, "LICENSE"));
  if (tunnelVersion) {
    writeFileSync(join(packageRoot, "runtime", "tunnel", "THIRD-PARTY-NOTICES.txt"),
      pinnedNotice(`cloudflared@${tunnelVersion}`));
  }
  let text = "Piik App third-party software notices\n" +
    WEB_NOTICE_LINE +
    (tunnelVersion ? "Cloudflared: runtime/tunnel/THIRD-PARTY-NOTICES.txt\n" : "");
  if (existsSync(join(packageRoot, "runtime", "native", target.captureName))) {
    const dependencies = JSON.parse(readFileSync(join(repositoryRoot, "native", "capture",
      "windows", "webrtc-dependencies.json"), "utf8"));
    const name = `webrtc@${dependencies.webrtc.version}`;
    text += section(name, pinnedNotice(name));
  }
  writeFileSync(join(packageRoot, "THIRD-PARTY-NOTICES.txt"), text + rustNotices(repositoryRoot, ["piik-app"]));
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  if (process.argv.length !== 3) throw new Error("Usage: node scripts/package-licenses.mjs <web-notices-output>");
  writeWebLicenseNotices(root, resolve(process.argv[2]));
}
