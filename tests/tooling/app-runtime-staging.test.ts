import {
  existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { basename, join } from "node:path";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { appPackageTarget } from "../../scripts/release/app-package-targets.mjs";

const observed = vi.hoisted(() => ({
  runs: [] as { args: string[]; cwd: string }[],
  // Fixture behaviour keyed by executable base name: staging copies the helper
  // to another directory, so the fixture answers by identity, not by path.
  probes: new Map<string, { protocol: number; exit: number }>(),
}));

vi.mock("node:child_process", async (original) => ({
  ...await original<typeof import("node:child_process")>(),
  spawnSync: (command: string, args: string[], options: { cwd?: string }) => {
    observed.runs.push({ args, cwd: options?.cwd ?? "" });
    const name = basename(command);
    if (args[0] === "--version") return { status: 0, stdout: "cloudflared version fixture\n", stderr: "" };
    if (args[0] === "--probe") {
      const fixture = observed.probes.get(name);
      if (!fixture) throw new Error(`No probe fixture for ${name}`);
      return fixture.exit === 0
        ? { status: 0, stdout: JSON.stringify({ protocol: fixture.protocol }), stderr: "" }
        : { status: 1, stdout: "", stderr: "probe failed" };
    }
    if (args[args.length - 1] === "--capture-only") return { status: 0, stdout: "", stderr: "" };
    throw new Error(`Unexpected command: ${command} ${args.join(" ")}`);
  },
}));

const {
  isWindowsAmd64, prepareAppRuntime, sha256,
} = await import("../../scripts/release/prepare-app-runtime.mjs");

const target = appPackageTarget("windows-amd64")!;

function portableExecutable(header = 0x8664): Buffer {
  const bytes = Buffer.alloc(0x86);
  bytes.set([0x4d, 0x5a]);
  bytes.writeUInt32LE(0x80, 0x3c);
  bytes.write("PE\0\0", 0x80, "binary");
  bytes.writeUInt16LE(header, 0x84);
  return bytes;
}

// Registers the recorded behaviour for one fixture executable. Staging probes
// the copied file, so a helper that is renamed on the way in (as an
// already-downloaded binary would be) must answer under its staged name.
function helperFile(path: string, probe = { protocol: 7, exit: 0 }, stagedAs = basename(path)): string {
  mkdirSync(join(path, ".."), { recursive: true });
  writeFileSync(path, portableExecutable());
  observed.probes.set(stagedAs, probe);
  return path;
}

const roots: string[] = [];
function workspace(): string {
  const root = mkdtempSync(join(tmpdir(), "piik-runtime-staging-"));
  roots.push(root);
  return root;
}

/** Builds a repository fixture with a built App and staged helper sources. */
function fixture(): { root: string; targetDirectory: string; tunnel: string; capture: string } {
  const root = workspace();
  const targetDirectory = join(root, "target");
  const profile = join(targetDirectory, "release");
  mkdirSync(profile, { recursive: true });
  writeFileSync(join(profile, target.appName), portableExecutable());
  const tunnel = helperFile(join(root, "downloads", target.tunnelName));
  const capture = helperFile(join(root, "build", "rust-check", target.captureName));
  return { root, targetDirectory, tunnel, capture };
}

let stdout: ReturnType<typeof vi.spyOn>;

beforeEach(() => {
  // Staging narrates downloads and digest mismatches; the tests assert on the
  // record instead of letting it into the runner's output.
  stdout = vi.spyOn(process.stdout, "write").mockImplementation(() => true);
});

afterEach(() => {
  stdout.mockRestore();
  observed.runs = [];
  observed.probes.clear();
  for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true });
});

describe("App runtime staging", () => {
  it("stages both helpers at the paths the App resolves beside itself", async () => {
    const { root, targetDirectory, tunnel, capture } = fixture();
    const result = await prepareAppRuntime({
      repositoryRoot: root, targetDirectory, profile: "release",
      tunnelPath: tunnel, capturePath: capture,
    });
    expect(result.nativeCapture).toBe(join(targetDirectory, "release", "runtime", "native", target.captureName));
    expect(result.publicTunnel).toBe(join(targetDirectory, "release", "runtime", "tunnel", target.tunnelName));
    for (const path of [result.nativeCapture, result.publicTunnel]) {
      expect(existsSync(path)).toBe(true);
      expect(isWindowsAmd64(path)).toBe(true);
    }
    expect(result.captureProtocol).toBe(7);
    // The staged helpers are probed, not the sources.
    const probed = observed.runs.filter((run) => run.args[0] === "--probe").map((run) => run.args);
    expect(probed).toHaveLength(1);
  });

  it("reports a tunnel that is not the pinned release instead of failing", async () => {
    const { root, targetDirectory, tunnel, capture } = fixture();
    const result = await prepareAppRuntime({
      repositoryRoot: root, targetDirectory, profile: "release",
      tunnelPath: tunnel, capturePath: capture,
    });
    expect(stdout.mock.calls.flat().join("")).toContain(sha256(tunnel));
    expect(stdout.mock.calls.flat().join("")).toContain("differs from the pinned");
    expect(existsSync(result.publicTunnel)).toBe(true);
  });

  it("refuses a helper that is not a Windows x64 executable", async () => {
    const { root, targetDirectory, capture } = fixture();
    const foreign = join(root, "downloads", "arm64.exe");
    writeFileSync(foreign, portableExecutable(0xaa64));
    await expect(prepareAppRuntime({
      repositoryRoot: root, targetDirectory, profile: "release",
      tunnelPath: foreign, capturePath: capture,
    })).rejects.toThrow("not a windows-amd64 executable");
  });

  it("refuses a capture helper whose probe protocol is not current", async () => {
    const { root, targetDirectory, tunnel } = fixture();
    // Staging copies it to the packaged name, which is what gets probed.
    const stale = join(root, "build", "rust-check", "stale.exe");
    helperFile(stale, { protocol: 6, exit: 0 }, target.captureName);
    await expect(prepareAppRuntime({
      repositoryRoot: root, targetDirectory, profile: "release",
      tunnelPath: tunnel, capturePath: stale,
    })).rejects.toThrow("probe protocol is 6, not 7");
  });

  it("surfaces a capture probe that fails to run", async () => {
    const { root, targetDirectory, tunnel } = fixture();
    const broken = join(root, "build", "rust-check", "broken.exe");
    helperFile(broken, { protocol: 7, exit: 1 }, target.captureName);
    await expect(prepareAppRuntime({
      repositoryRoot: root, targetDirectory, profile: "release",
      tunnelPath: tunnel, capturePath: broken,
    })).rejects.toThrow("failed its capability probe");
  });

  it("requires the App to be built before staging", async () => {
    const { root, targetDirectory, tunnel, capture } = fixture();
    rmSync(join(targetDirectory, "release", target.appName));
    await expect(prepareAppRuntime({
      repositoryRoot: root, targetDirectory, profile: "release",
      tunnelPath: tunnel, capturePath: capture,
    })).rejects.toThrow("Build the App first");
  });

  it("verifies the pinned digest and replaces a stale cache entry", async () => {
    const { root, targetDirectory, capture } = fixture();
    // A cached file with the wrong bytes must be refetched, never staged.
    const cacheRoot = join(root, "build", "app-runtime-cache");
    mkdirSync(cacheRoot, { recursive: true });
    writeFileSync(join(cacheRoot, target.tunnelAsset), "truncated");
    const fixtureBytes = portableExecutable();
    const fetchImpl = vi.fn(async () => new Response(new Uint8Array(fixtureBytes), { status: 200 }));
    await expect(prepareAppRuntime({
      repositoryRoot: root, targetDirectory, profile: "release",
      capturePath: capture, fetchImpl,
    })).rejects.toThrow("does not match the pinned release");
    expect(fetchImpl).toHaveBeenCalledOnce();
    expect(readFileSync(join(cacheRoot, target.tunnelAsset))).toEqual(fixtureBytes);
  });
});
