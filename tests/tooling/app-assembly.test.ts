import { createHash } from "node:crypto";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, it, vi } from "vitest";

const source = vi.hoisted(() => ({
  revision: "a".repeat(40), built: false, finalState: "clean", output: "",
  cargo: [] as string[],
}));

// A minimal Windows PE header; `wrong` changes only the machine field.
function portableExecutable(machine: number): Buffer {
  const bytes = Buffer.alloc(0x86);
  bytes.set([0x4d, 0x5a]);
  bytes.writeUInt32LE(0x80, 0x3c);
  bytes.write("PE\0\0", 0x80, "binary");
  bytes.writeUInt16LE(machine, 0x84);
  return bytes;
}

// Exercise the actual assembler and final export. Only external build tools,
// Git observations and license generation are replaced by the fixture.
vi.mock("node:child_process", async (original) => ({
  ...await original<typeof import("node:child_process")>(),
  execFileSync: (_command: string, args: string[]) => {
    if (args[0] === "rev-parse") {
      return source.built && source.finalState === "revision" ? "b".repeat(40) : source.revision;
    }
    if (args[0] === "status") {
      return source.built && source.finalState === "dirty" ? " M README.md\n" : "";
    }
    throw new Error(`Unexpected Git observation: ${args.join(" ")}`);
  },
  spawnSync: (_command: string, args: string[], options: { env: NodeJS.ProcessEnv }) => {
    if (args[0] === "build") {
      source.cargo = [...args, options.env.PIIK_RELEASE!, options.env.PIIK_REVISION!];
      const release = join(options.env.CARGO_TARGET_DIR!, "release");
      mkdirSync(release, { recursive: true });
      writeFileSync(join(release, "piik-app.exe"),
        portableExecutable(source.finalState === "wrong-architecture" ? 0xaa64 : 0x8664));
      source.built = true;
      if (source.finalState === "output-created") {
        mkdirSync(source.output);
        writeFileSync(join(source.output, "user.txt"), "keep");
      }
    } else if (!args.join(" ").includes("build:web")) {
      throw new Error(`Unexpected build operation: ${args.join(" ")}`);
    }
    return { status: 0, stdout: "", stderr: "" };
  },
}));
vi.mock("node:fs", async (original) => {
  const fs = await original<typeof import("node:fs")>();
  return { ...fs, cpSync: (...args: Parameters<typeof fs.cpSync>) => {
    if (source.finalState === "copy") {
      fs.writeFileSync(join(source.output, "partial"), "incomplete");
      throw new Error("copy interrupted");
    }
    return fs.cpSync(...args);
  } };
});
vi.mock("../../scripts/release/archive-tool.mjs", () => ({
  extractZip: (_archive: string, destination: string) =>
    writeFileSync(join(destination, "REVISION"), `${source.revision}\n`),
}));
vi.mock("../../scripts/release/package-licenses.mjs", () => ({ writeAppLicenseNotices: () => undefined }));

describe("App package source identity (windows-amd64)", () => {
  it.each(["clean", "nested", "dirty", "revision", "copy", "existing", "output-created", "wrong-architecture"])("exports only complete owned output from unchanged source (%s)", async (finalState) => {
    const root = mkdtempSync(join(tmpdir(), "piik-app-assembly-"));
    const output = join(root, ...(finalState === "nested" ? ["new-parent"] : []), "app");
    const descriptor = join(root, "server.release.json");
    const artifact = Buffer.from("fixture server archive");
    const previousArguments = process.argv;
    const stdout = vi.spyOn(process.stdout, "write").mockImplementation(() => true);
    vi.stubEnv("PIIK_CARGO", "piik-fixture-cargo");
    vi.stubEnv("CARGO_TARGET_DIR", join(root, "target"));
    vi.stubEnv("VITE_PIIK_VERSION", "development");
    vi.stubEnv("VITE_PIIK_REVISION", "");
    source.built = false;
    source.cargo = [];
    source.finalState = finalState;
    source.output = output;
    try {
      if (finalState === "existing") {
        mkdirSync(output);
        writeFileSync(join(output, "user.txt"), "keep");
      }
      writeFileSync(join(root, "server.zip"), artifact);
      writeFileSync(descriptor, JSON.stringify({
        schema: 2, version: "development", revision: source.revision,
        artifact: "server.zip", artifactSha256: createHash("sha256").update(artifact).digest("hex"),
      }));
      process.argv = [process.execPath, "assemble-app.mjs", descriptor, output, "--target", "windows-amd64"];
      vi.resetModules();
      const assembly = import("../../scripts/release/assemble-app.mjs");
      if (finalState === "clean" || finalState === "nested") {
        await assembly;
        expect(readFileSync(join(output, "REVISION"), "ascii")).toBe(`${source.revision}\n`);
        expect(existsSync(join(output, "piik-app.exe"))).toBe(true);
      } else if (finalState === "existing" || finalState === "output-created") {
        await expect(assembly).rejects.toThrow(finalState === "existing" ? "must not already exist" : "EEXIST");
        expect(readFileSync(join(output, "user.txt"), "utf8")).toBe("keep");
        expect(existsSync(join(output, "piik-app.exe"))).toBe(false);
      } else if (finalState === "copy") {
        await expect(assembly).rejects.toThrow("copy interrupted");
        expect(existsSync(output)).toBe(false);
      } else if (finalState === "wrong-architecture") {
        await expect(assembly).rejects.toThrow("App executable does not match App package target windows-amd64");
        expect(existsSync(output)).toBe(false);
      } else {
        await expect(assembly).rejects.toThrow(finalState === "dirty" ? "clean Git checkout" : "package source");
        expect(existsSync(output)).toBe(false);
      }
      expect(source.built).toBe(finalState !== "existing");
      if (source.built) {
        // The release build carries the descriptor's identity into the binary.
        expect(source.cargo).toEqual(["build", "--release", "--locked", "-p", "piik-app",
          "development", source.revision]);
      }
    } finally {
      process.argv = previousArguments;
      stdout.mockRestore();
      vi.unstubAllEnvs();
      rmSync(root, { recursive: true, force: true });
    }
  });
});
