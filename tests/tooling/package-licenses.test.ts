import { describe, expect, it } from "vitest";

import { linkedPackages } from "../../scripts/release/package-licenses.mjs";

type Kind = "normal" | "dev" | "build";

// A minimal `cargo metadata` graph: workspace binaries and library crates.
function metadata(edges: Record<string, Array<[string, Kind[]]>>, procMacros: string[] = []) {
  const id = (name: string) => `${name.startsWith("piik-") ? "path+file:///piik" : "registry"}#${name}@1.0.0`;
  const names = new Set([...Object.keys(edges), ...Object.values(edges).flat().map(([name]) => name)]);
  return {
    packages: [...names].map((name) => ({
      id: id(name), name, version: "1.0.0", manifest_path: `/${name}/Cargo.toml`,
      targets: [{ kind: procMacros.includes(name) ? ["proc-macro"] : ["lib"] }],
    })),
    workspace_members: [...names].filter((name) => name.startsWith("piik-")).map(id),
    resolve: {
      nodes: [...names].map((name) => ({
        id: id(name),
        deps: (edges[name] ?? []).map(([dependency, kinds]) => ({
          pkg: id(dependency),
          dep_kinds: kinds.map((kind) => ({ kind: kind === "normal" ? null : kind })),
        })),
      })),
    },
  };
}

describe("Rust third-party notices", () => {
  it("lists only third-party crates linked into the requested binaries", () => {
    const graph = metadata({
      "piik-app": [["piik-core", ["normal"]], ["tauri-build", ["build"]], ["tempfile", ["dev"]]],
      "piik-server": [["piik-core", ["normal"]], ["clap", ["normal"]]],
      "piik-core": [["tokio", ["normal", "dev"]], ["serde_derive", ["normal"]]],
      tokio: [["mio", ["normal"]], ["cc", ["build"]]],
      serde_derive: [["syn", ["normal"]]],
      "tauri-build": [["toml", ["normal"]]],
    }, ["serde_derive"]);
    const names = (roots: string[]) => linkedPackages(graph, roots).map((entry: { name: string }) => entry.name);
    expect(names(["piik-app"])).toEqual(["mio", "tokio"]);
    expect(names(["piik-server"])).toEqual(["clap", "mio", "tokio"]);
    expect(() => linkedPackages(graph, ["piik-missing"])).toThrow("no package piik-missing");
  });
});
