import { join, resolve } from "node:path";
import { afterEach, describe, expect, it, vi } from "vitest";

import {
  APP_PACKAGE_TARGETS,
  cargoGateBuild,
  cargoRelease,
} from "../../scripts/release/app-package-targets.mjs";
import { SERVER_PACKAGE_TARGETS } from "../../scripts/release/server-package-targets.mjs";

describe("App package targets", () => {
  afterEach(() => vi.unstubAllEnvs());

  it("builds every package natively for Windows x64", () => {
    expect(APP_PACKAGE_TARGETS.map((target) => [target.id, target.rustTarget]))
      .toEqual([["windows-amd64", "x86_64-pc-windows-msvc"]]);
    expect(SERVER_PACKAGE_TARGETS.map((target) => target.rustTarget))
      .toEqual(["x86_64-pc-windows-msvc"]);
  });

  it("injects only the requested build identity into a locked release build", () => {
    vi.stubEnv("CARGO_TARGET_DIR", "");
    vi.stubEnv("PIIK_RELEASE", "inherited-release");
    const root = resolve("fixture-repository");
    const release = cargoRelease(root, "piik-app", { version: "v1.2.3", revision: "a".repeat(40) });
    expect(release.args).toEqual(["build", "--release", "--locked", "-p", "piik-app"]);
    expect(release.executable).toBe(join(root, "target", "release", "piik-app.exe"));
    expect([release.env.PIIK_RELEASE, release.env.PIIK_REVISION]).toEqual(["v1.2.3", "a".repeat(40)]);
    // Ambient entries survive, and the caller's environment is not modified.
    expect(release.env.PATH ?? release.env.Path).toBe(process.env.PATH ?? process.env.Path);
    expect(process.env.PIIK_RELEASE).toBe("inherited-release");
    expect(cargoRelease(root, "piik-server").env.PIIK_RELEASE).toBe("inherited-release");
  });

  it("runs gate binaries from the stable debug target directory", () => {
    const target = resolve("fixture-target");
    vi.stubEnv("CARGO_TARGET_DIR", target);
    const build = cargoGateBuild(resolve("fixture-repository"), "piik-server");
    expect(build.args).toEqual(["build", "--locked", "-p", "piik-server"]);
    expect(build.env.CARGO_TARGET_DIR).toBe(target);
    expect(build.executable).toBe(join(target, "debug", "piik-server.exe"));
  });
});
