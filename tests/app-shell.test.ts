import { readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import { runInNewContext } from "node:vm";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, describe, expect, it, vi } from "vitest";
import { appShellInitScript, inAppShell } from "../src/client/lib/app-shell";
import { HostNeedsAppPanel } from "../src/client/ui/components/shell/HostNeedsApp";
import { AppConnectionButton } from "../src/client/ui/components/shell/AppConnectionButton";
import { COMIC_KINDS } from "../src/client/ui/components/comic/comic-presentation";
import { setCopy } from "../src/client/ui/copy";

afterEach(() => {
  vi.unstubAllGlobals();
  setCopy({ lang: "zh", vis: false });
});

describe("App window marker", () => {
  it("defines a frozen, read-only marker that page code cannot replace", () => {
    const window = {} as { __PIIK_APP__?: { version: string } };
    runInNewContext(appShellInitScript('3.0.0"x'), { window, Object });
    expect(window.__PIIK_APP__).toEqual({ version: '3.0.0"x' });
    expect(Object.isFrozen(window.__PIIK_APP__)).toBe(true);
    expect(() => { window.__PIIK_APP__ = { version: "spoof" }; }).toThrow(TypeError);
    expect(Object.keys(window)).toEqual([]);

    vi.stubGlobal("__PIIK_APP__", window.__PIIK_APP__);
    expect(inAppShell()).toBe(true);
  });

  it.each([
    ["absent", undefined],
    ["mutable object", { version: "3.0.0" }],
    ["missing version", Object.freeze({})],
    ["truthy primitive", "3.0.0"],
  ])("treats an ordinary browser as outside the App: %s", (_, marker) => {
    if (marker !== undefined) vi.stubGlobal("__PIIK_APP__", marker);
    expect(inAppShell()).toBe(false);
  });
});

describe("browser Host entry", () => {
  it.each([
    ["zh", "房主请使用 Piik App"],
    ["en", "Hosting needs the Piik App"],
  ] as const)("explains App hosting and keeps joining available (%s)", (lang, title) => {
    setCopy({ lang, vis: false });
    const html = renderToStaticMarkup(createElement(HostNeedsAppPanel));
    expect(html).toContain(title);
    expect(html).toContain('aria-labelledby="host-needs-app-title"');
    expect(html).toContain('id="host-needs-app-title"');
    expect(html).toMatch(/<button[^>]+is-primary/);
    expect(html).not.toMatch(/href="https?:/);
  });

  it("keeps the accessible explanation in visual mode", () => {
    setCopy({ lang: "en", vis: true });
    const html = renderToStaticMarkup(createElement(HostNeedsAppPanel));
    expect(html).toContain('class="visually-hidden"');
    expect(html).toContain("Hosting needs the Piik App");
  });

  it("gates the Host page module and route on the App marker", () => {
    const app = readFileSync(new URL("../src/client/App.tsx", import.meta.url), "utf8");
    expect(app).toContain('const hostAvailable = appRoute.kind === "host" && inAppShell();');
    expect(app).toContain("hostAvailable ? import(\"./pages/HostPage\") : null");
    expect(app).toContain("hostAvailable ? <SiteAccessGate surface=\"host\" /> : <HostNeedsApp />");
  });

  it("registers the scene in the shared catalogue without the removed update scene", () => {
    expect(COMIC_KINDS).toContain("host-needs-app");
    expect(COMIC_KINDS).not.toContain("update-available" as never);
  });
});

describe("no project website or repository links", () => {
  const files = (dir: string): string[] => readdirSync(dir).flatMap(name => {
    const path = join(dir, name);
    return statSync(path).isDirectory() ? files(path) : [path];
  });

  it("ships no piik.tv or GitHub references in the Web UI", () => {
    const root = new URL("..", import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, "$1");
    const scanned = [...files(join(root, "src")), ...files(join(root, "public")), join(root, "index.html")]
      .filter(path => /\.(tsx?|css|html|svg|json|txt)$/.test(path));
    expect(scanned.some(path => path.endsWith("AppConnectionButton.tsx"))).toBe(true);
    // A third-party driver link may appear only where it is gated behind the
    // App shell: the Web UI still never sends anyone to a project or
    // repository page. A file that carries one must guard it with inAppShell.
    const gated = scanned.filter(path => {
      const text = readFileSync(path, "utf8");
      if (!/piik\.tv|github\.com/i.test(text)) return false;
      return !/inAppShell\(\)/.test(text);
    });
    expect(gated).toEqual([]);
  });

  it("offers no App download link when App connection fails", () => {
    setCopy({ lang: "en", vis: false });
    const html = renderToStaticMarkup(createElement(AppConnectionButton));
    expect(html).not.toContain("<a ");
  });
});
