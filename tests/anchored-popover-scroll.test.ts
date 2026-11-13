import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

// An attached settings menu can be much taller than a phone viewport. Scrolling
// it used to re-run placement on every scroll event — the handler was captured
// from anywhere in the document — which re-measured the panel mid-gesture and
// could flip its side. Worse, a trigger scrolled out of view made placement
// close the menu outright, so the person could never reach the lower controls.
// These rules keep scrolling inside the menu a local act, keep a scroll that
// merely hides the trigger from closing it, and clamp the anchor so an
// off-screen trigger still yields a placed menu.

const source = readFileSync(
  resolve(import.meta.dirname, "..", "src", "client", "ui", "components", "shell", "use-anchored-popover.ts"),
  "utf8",
);

describe("Anchored popover scrolling", () => {
  it("ignores scrolls that start inside the menu", () => {
    const handler = /const onScroll = \(event: Event\) => \{([\s\S]*?)\n {4}\}/.exec(source)?.[1];
    expect(handler, "the scroll handler guards its own subtree").toBeDefined();
    expect(handler).toMatch(/menu\.contains\(event\.target\)/);
    expect(handler).toMatch(/return;/);
  });

  it("registers the guarded handler, not the raw scheduler", () => {
    expect(source).toContain('view.addEventListener("scroll", onScroll, true)');
    expect(source).toContain('viewport?.addEventListener("scroll", onScroll)');
    expect(source).not.toContain('addEventListener("scroll", schedule');
  });

  it("closes only a detached trigger and clamps an off-screen one", () => {
    // Folding `anchorOutside` into the removal check is what closed the menu
    // while it was still on screen, mid-scroll on a phone.
    expect(source).toMatch(/if \(!anchor\.getClientRects\(\)\.length\) \{\s*\n\s*menu\.hidePopover\(\);\s*\n\s*return;/);
    // An off-screen trigger is clamped into the viewport rather than skipped,
    // so the first open still places the menu instead of leaving it at 0,0.
    expect(source, "the anchor is clamped, not skipped").toMatch(/const anchorTop = Math\.min\(Math\.max\(rect\.top, top\), bottom\)/);
    expect(source).not.toMatch(/if \(anchorOutside\) return;/);
    // The old shape combined both conditions in one closing branch.
    expect(source).not.toMatch(/getClientRects\(\)\.length \|\|/);
  });
});
