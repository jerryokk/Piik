import { execFileSync } from "node:child_process";
import { pathToFileURL } from "node:url";
import semver from "semver";

const git = (root, ...args) => execFileSync("git", args, { cwd: root, encoding: "utf8" }).trim();
export const isReleaseVersion = (value) => typeof value === "string" &&
  /^v\d+\.\d+\.\d+$/.test(value) && semver.valid(value) !== null;

function stableTags(root, ...filter) {
  return git(root, "tag", ...filter).split("\n").filter(isReleaseVersion).sort(semver.rcompare);
}

// One input for packagers: an explicit version, an exact local tag, or a dev build.
export function buildVersion(root, revision, requested = process.env.PIIK_BUILD_VERSION) {
  const version = requested || stableTags(root, "--points-at", revision)[0] || "development";
  if (version === "development") return version;
  if (!isReleaseVersion(version)) throw new Error("Build version must be vMAJOR.MINOR.PATCH or development");
  if (git(root, "tag", "--list", version) &&
      git(root, "rev-parse", `${version}^{commit}`) !== revision) {
    throw new Error("A release version cannot identify two source revisions");
  }
  return version;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const root = process.cwd();
  const revision = git(root, "rev-parse", "HEAD");
  process.stdout.write(`${JSON.stringify({ version: buildVersion(root, revision), revision })}\n`);
}
