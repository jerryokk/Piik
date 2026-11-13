import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, resolve } from "node:path";
import { describe, expect, it } from "vitest";

// One visual scale, not an accumulation. Corner radius, type size, weight and
// colour timing each had a long tail of near-identical values — 11px beside
// 11.5px, border-radius 13px beside 14px, font-weight 650 beside 700. Those
// differences are invisible, make the system unpredictable to change, and were
// only ever historical. This test is the floor: adding a new value outside the
// scale fails here instead of quietly widening the tail again.

const styles = resolve(import.meta.dirname, "..", "src", "client");

function cssFiles(dir: string): string[] {
  const found: string[] = [];
  for (const entry of readdirSync(dir)) {
    const path = join(dir, entry);
    if (statSync(path).isDirectory()) found.push(...cssFiles(path));
    else if (entry.endsWith(".css")) found.push(path);
  }
  return found;
}

const files = cssFiles(styles).map((path) => ({ path, text: readFileSync(path, "utf8") }));

function values(property: RegExp): { value: string; where: string }[] {
  const found: { value: string; where: string }[] = [];
  for (const { path, text } of files) {
    for (const match of text.matchAll(property)) {
      found.push({ value: match[1].trim(), where: path.replace(/\\/g, "/").split("src/client/")[1] });
    }
  }
  return found;
}

describe("Visual scale", () => {
  it("uses one corner-radius scale", () => {
    // Two deliberate exceptions: the remote mouse is drawn as a physical
    // object and keeps the proportions that make it read as a mouse; the
    // television frame, the launch card and the chat bubble's tail are
    // one-off shapes rather than the shared control surface. Everything else
    // uses the four-step scale.
    const ownShapes = [
      "ui/components/control/remote-control.css",
      "ui/styles/tv.css",
      "ui/styles/join-access.css",
      "ui/components/room/room-interactions.css",
    ];
    const literals = values(/border-radius:\s*([^;{}\n]+)/g)
      .filter(({ where }) => !ownShapes.includes(where))
      .flatMap(({ value, where }) => value.split(/\s+\/\s+|\s+/).map((part) => ({ value: part, where })))
      .filter(({ value }) => /^\d+px$/.test(value));
    // Hairline dividers round only enough to avoid a hard pixel edge.
    const allowed = new Set(["0px", "2px", "8px", "12px", "16px", "18px", "20px", "24px", "999px"]);
    const stray = literals.filter(({ value }) => !allowed.has(value));
    expect(stray.map(({ value, where }) => `${value} in ${where}`), "corners use the shared scale").toEqual([]);
  });

  it("uses one type scale with no half pixels", () => {
    const sizes = values(/font-size:\s*([^;{}\n]+)/g).filter(({ value }) => /px$/.test(value));
    const allowed = new Set(["11px", "12px", "14px", "16px", "20px", "21px", "22px", "24px", "26px", "28px", "30px", "34px", "36px", "40px"]);
    const stray = sizes.filter(({ value }) => !allowed.has(value));
    expect(stray.map(({ value, where }) => `${value} in ${where}`), "font sizes use the shared scale").toEqual([]);
  });

  it("uses three font weights a system font can actually render", () => {
    const weights = values(/font-weight:\s*(\d{3})/g).map(({ value }) => value);
    expect([...new Set(weights)].sort(), "no synthetic in-between weights").toEqual(["400", "600", "700", "800"]);
  });

  it("tints through one token instead of a scattered 140ms", () => {
    // `--tint-motion` owns colour settling; a bare literal would drift again.
    const literals = values(/(?:background-color|border-color|color|box-shadow)\s+140ms/g);
    expect(literals.map(({ where }) => where), "colour timing goes through the token").toEqual([]);
  });
});
