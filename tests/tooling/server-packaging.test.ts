import { cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, it, vi } from "vitest";

const source = vi.hoisted(() => ({
  revision: "a".repeat(40), workspace: "", scenario: "", archived: false,
  cargo: [] as string[],
}));

// Keep the packager's descriptor/export flow real while replacing
// external build tools and their Git observations.
vi.mock("node:child_process", async (original) => ({
  ...await original<typeof import("node:child_process")>(),
  execFileSync: (_command: string, args: string[]) => {
    if (args[0] === "rev-parse") return source.archived && source.scenario === "revision"
      ? "b".repeat(40) : source.revision;
    if (args[0] === "status") return source.archived && source.scenario === "dirty"
      ? " M crates/piik-service/src/lib.rs\n" : "";
    throw new Error(`Unexpected Git observation: ${args.join(" ")}`);
  },
  spawnSync: (command: string, args: string[], options: { env: NodeJS.ProcessEnv }) => {
    if (args[0] === "build") {
      source.cargo = [...args, options.env.PIIK_RELEASE!, options.env.PIIK_REVISION!];
      const release = join(options.env.CARGO_TARGET_DIR!, "release");
      mkdirSync(release, { recursive: true });
      writeFileSync(join(release, "piik-server.exe"), "fixture Server");
    } else if (!args.join(" ").includes("build:web")) {
      throw new Error(`Unexpected build operation: ${command} ${args.join(" ")}`);
    }
    return { status: 0, stdout: "", stderr: "" };
  },
}));
vi.mock("node:fs", async (original) => {
  const fs = await original<typeof import("node:fs")>();
  return {
    ...fs,
    readFileSync: (...args: Parameters<typeof fs.readFileSync>) => String(args[0]).replaceAll("\\", "/").endsWith("web/dist/index.html")
      ? '<script src="/assets/index-fixture.js"></script>' : fs.readFileSync(...args),
    existsSync: (path: Parameters<typeof fs.existsSync>[0]) => String(path).endsWith("index-fixture.js") || fs.existsSync(path),
  };
});
vi.mock("../../scripts/release/build-workspace.mjs", async (original) => ({
  ...await original<typeof import("../../scripts/release/build-workspace.mjs")>(),
  resetBuildWorkspace: () => { mkdirSync(source.workspace); return source.workspace; },
}));
vi.mock("../../scripts/release/package-licenses.mjs", () => ({
  writeServerLicenseNotices: (_root: string, path: string) => writeFileSync(path, "fixture notices"),
}));
vi.mock("../../scripts/release/release-version.mjs", () => ({ buildVersion: () => "development" }));
vi.mock("../../scripts/release/archive-tool.mjs", () => ({
  createZip: (_root: string, archive: string) => {
    writeFileSync(archive, "fixture archive");
    source.archived = true;
    if (source.scenario === "archive-failure") throw new Error("archive interrupted");
  },
  extractZip: (_archive: string, destination: string) =>
    cpSync(join(source.workspace, "runtime"), destination, { recursive: true }),
}));

it.each(["clean", "dirty", "revision", "archive-failure", "existing"] as const)(
  "exports a Server release only after all tracked inputs are settled (%s)", async (scenario) => {
    const root = mkdtempSync(join(tmpdir(), "piik-server-package-"));
    const output = join(root, "release");
    const previousArguments = process.argv;
    const stdout = vi.spyOn(process.stdout, "write").mockImplementation(() => true);
    vi.stubEnv("VITE_PIIK_VERSION", "development");
    vi.stubEnv("VITE_PIIK_REVISION", "");
    vi.stubEnv("PIIK_CARGO", "piik-fixture-cargo");
    vi.stubEnv("CARGO_TARGET_DIR", join(root, "target"));
    source.cargo = [];
    source.workspace = join(root, "workspace");
    source.scenario = scenario;
    source.archived = false;
    try {
      if (scenario === "existing") {
        mkdirSync(output);
        writeFileSync(join(output, "user.txt"), "keep");
      }
      process.argv = [process.execPath, "package-server-release.mjs", output];
      vi.resetModules();
      const packaging = import("../../scripts/release/package-server-release.mjs");
      if (scenario === "clean") {
        await packaging;
        const descriptor = JSON.parse(readFileSync(join(output, "piik-aaaaaaa-windows-amd64.release.json"), "utf8"));
        expect(descriptor.revision).toBe(source.revision);
        expect(descriptor.artifact).toBe("piik-aaaaaaa-windows-amd64-runtime.zip");
        expect(existsSync(join(output, descriptor.artifact))).toBe(true);
        expect(source.cargo).toEqual(["build", "--release", "--locked", "-p", "piik-server",
          "development", source.revision]);
      } else if (scenario === "existing") {
        await expect(packaging).rejects.toThrow("must not already exist");
        expect(readFileSync(join(output, "user.txt"), "utf8")).toBe("keep");
      } else {
        const expected = { dirty: "clean Git checkout", revision: "package source",
          "archive-failure": "archive interrupted" }[scenario];
        await expect(packaging).rejects.toThrow(expected);
        expect(existsSync(output)).toBe(false);
      }
      expect(existsSync(source.workspace)).toBe(false);
      expect(source.archived).toBe(scenario !== "existing");
    } finally {
      process.argv = previousArguments;
      stdout.mockRestore();
      vi.unstubAllEnvs();
      rmSync(root, { recursive: true, force: true });
    }
  },
);
