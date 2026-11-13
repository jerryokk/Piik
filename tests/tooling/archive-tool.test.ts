import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { describe, expect, it } from "vitest";

import { createZip, extractZip, tarExecutable } from "../../scripts/release/archive-tool.mjs";

describe("App ZIP distribution", () => {
  // Spawns the system tar twice; under a full parallel run that exceeds the
  // default 5 s budget even though the work itself is fast.
  it("round-trips hidden files and spaced paths", { timeout: 30_000 }, () => {
    const root = mkdtempSync(join(tmpdir(), "piik-archive-"));
    try {
      const source = join(root, "assembled app");
      const extracted = join(root, "extracted app");
      const archive = join(root, "Piik App.zip");
      const files = {
        "piik-app.exe": "fixture executable\n",
        "runtime/.native/画面.txt": "Native capture\n",
        ".release-metadata": "Hidden metadata\n",
      };
      for (const [name, content] of Object.entries(files)) {
        const path = join(source, name);
        mkdirSync(dirname(path), { recursive: true });
        writeFileSync(path, content);
      }
      createZip(source, archive);
      expect(readFileSync(archive).subarray(0, 4)).toEqual(Buffer.from([0x50, 0x4b, 0x03, 0x04]));
      const entries = execFileSync(tarExecutable(), ["-tf", archive],
        { encoding: "utf8", windowsHide: true }).trim().split(/\r?\n/);
      expect(entries).toHaveLength(readdirSync(source, { recursive: true }).length);
      for (const name of entries) expect(name).not.toMatch(/(^|\/)\.\.?($|\/)/);
      mkdirSync(extracted);
      extractZip(archive, extracted);
      expect(readdirSync(extracted, { recursive: true }).sort()).toEqual(
        readdirSync(source, { recursive: true }).sort(),
      );
      for (const [name, content] of Object.entries(files)) {
        expect(readFileSync(join(extracted, name), "utf8")).toBe(content);
      }
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });
});
