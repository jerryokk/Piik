import { readFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { describe, expect, it } from "vitest";

// The playback bar is one fixed row. Remote input contributes a status
// sentence of unbounded length, and once it claimed width outright the
// transport controls overlapped and collapsed to zero on narrower stages.
// These rules are the budget that keeps that from returning; the layout itself
// is verified in a real browser during acceptance.

const components = resolve(import.meta.dirname, "..", "src", "client", "ui", "components");
const read = (group: string, name: string) => readFileSync(join(components, group, name), "utf8");

/** The declaration block of every rule whose selector list contains `selector`. */
function blocks(css: string, selector: string): string[] {
  const found: string[] = [];
  const rules = /([^{}]+)\{([^{}]*)\}/g;
  for (const match of css.replaceAll(/\/\*[\s\S]*?\*\//g, "").matchAll(rules)) {
    const selectors = match[1].split(",").map((part) => part.trim());
    if (selectors.includes(selector)) found.push(match[2]);
  }
  return found;
}

function declaration(block: string, property: string): string | null {
  const match = block.match(new RegExp(`(?:^|;)\\s*${property}\\s*:([^;]+)`));
  return match ? match[1].trim() : null;
}

describe("Playback bar width budget", () => {
  it("keeps the transport group on its content basis", () => {
    // A `0` basis lets a status message take the free space first and leaves
    // play/sound at zero width.
    const [watch] = blocks(read("playback", "playback-controls.css"), ".lr-playback-watch");
    expect(watch, "playback bar keeps one watch rule").toBeDefined();
    expect(declaration(watch, "flex")).toMatch(/^1 1 auto$/);
  });

  it("lets remote input and its status sentence give up width", () => {
    const css = read("playback", "playback-controls.css");
    expect(blocks(css, ".lr-playback > .lr-playback-main > .lr-remote-controls")
      .map((block) => declaration(block, "flex")))
      .toEqual(["0 1 auto"]);

    const [feedback] = blocks(read("control", "remote-control.css"), ".lr-remote-feedback");
    expect(feedback, "status sentence is styled").toBeDefined();
    // Grow into spare room, shrink first, and never claim more than a reading width.
    expect(declaration(feedback, "flex")).toMatch(/^1 1 0$/);
    expect(declaration(feedback, "min-width")).toBe("0");
    expect(declaration(feedback, "max-width")).toMatch(/^\d+px$/);
  });

  it("truncates the sentence instead of wrapping or overflowing the bar", () => {
    const css = read("control", "remote-control.css");
    const [text] = blocks(css, ".lr-remote-feedback > span:not(.visually-hidden)");
    expect(text, "sentence text rule is present").toBeDefined();
    expect(declaration(text, "overflow")).toBe("hidden");
    expect(declaration(text, "text-overflow")).toBe("ellipsis");
    expect(declaration(text, "white-space")).toBe("nowrap");
    expect(declaration(text, "min-width")).toBe("0");
  });

  it("lets the bar hide while a remote status is showing", () => {
    // Pinning the bar whenever a status sentence existed left waiting states
    // such as "waiting for the Host" permanently on screen, which on a phone
    // covered the picture with no way to dismiss it.
    const css = read("control", "remote-control.css");
    const pinning = [...css.matchAll(/([^{}]+)\{([^{}]*)\}/g)]
      .filter(([, selector]) => selector.includes(":has(.lr-remote-feedback)"))
      .filter(([, , declarations]) => /opacity\s*:\s*1/.test(declarations));
    expect(pinning, "no rule keeps the bar visible because a status exists").toEqual([]);
  });

  it("moves the volume slider into the menu instead of dropping it", () => {
    // Hiding the inline slider on a narrow bar is only acceptable because the
    // settings menu carries the same control; otherwise volume would be
    // adjustable nowhere once the bar got narrow.
    const css = read("playback", "playback-controls.css");
    const narrow = /@container media-stage \(max-width: 560px\)\s*\{([\s\S]*?)\n\}/.exec(css)?.[1];
    expect(narrow, "the narrow bar rule is present").toBeDefined();
    expect(narrow).toMatch(/\.lr-playback-volume[^{}]*\{\s*display:\s*none/);
    expect(narrow).toMatch(/\.lr-playback-sound\s*\{\s*display:\s*block/);

    // The menu section starts hidden and only the same breakpoint reveals it.
    expect(blocks(css, ".lr-playback .lr-playback-sound").map((block) => declaration(block, "display")))
      .toEqual(["none", "block"]);

    const settings = readFileSync(resolve(components, "playback", "PlaybackSettings.tsx"), "utf8");
    expect(settings, "the menu renders a real volume control").toMatch(/className="lr-playback-sound-range"[\s\S]*type="range"/);
  });

  it("keeps every bar target tappable with a real gap", () => {
    // `clamp(24px, 8.5cqw, …)` let a phone-width stage squeeze each button to
    // 27px with no gap, so the icons read as one crowded strip and the remote
    // and keyboard buttons even overlapped at mid widths.
    const css = read("playback", "playback-controls.css");
    const shrink = blocks(css, ".lr-playback .lr-btn").map((block) => declaration(block, "width"))
      .find((width) => width?.startsWith("clamp"));
    expect(shrink, "the compact bar sizes its buttons").toBeDefined();
    const floor = Number(/clamp\((\d+)px/.exec(shrink!)?.[1]);
    expect(floor, "buttons stop shrinking at a tappable width").toBeGreaterThanOrEqual(36);

    // No rule may collapse the gaps between targets.
    expect(css).not.toMatch(/gap:\s*0\s*;/);
    // View modes leave the bar with the sound slider rather than vanishing.
    expect(blocks(css, ".lr-playback .lr-playback-window-modes").map((block) => declaration(block, "display")))
      .toEqual(["none", "block"]);
  });
});
