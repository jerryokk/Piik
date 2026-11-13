// Proves the invitation code a Viewer would scan carries the exact invitation
// link: the gate opens the Host's code panel, screenshots it, and decodes that
// image with an independent QR reader (jsQR). It never asks the page what it
// encoded, so a wrong or stale link fails here.
import { spawn, spawnSync, type ChildProcessWithoutNullStreams } from "node:child_process";
import { mkdtemp, mkdir, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import jsQR from "jsqr";
import { PNG } from "pngjs";
import {
  CdpConnection, cleanupRun, createPage, evaluate, fetchJsonBefore, launchChrome, reservePort,
  waitForSample, waitForVersion,
} from "./support/browser-gate-harness";
import { appShellInitScript } from "../../src/client/lib/app-shell";
import { cargoGateBuild } from "../release/app-package-targets.mjs";

const ROOT = resolve(import.meta.dirname, "..", "..");
const BUILD_ROOT = join(ROOT, "build/rust-check");
// A documentation-only LAN address, as in the fanout gate: the server publishes
// a LAN invitation whose host the gate rewrites to its loopback origin.
const LAN = "192.0.2.1";

function build(command: string, args: string[]): void {
  const result = spawnSync(command, args, {
    cwd: ROOT, encoding: "utf8", windowsHide: true, timeout: 900_000,
  });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error((result.stderr || result.stdout).trim() || `${command} failed`);
}

// CDP's screenshot is a PNG; decode it here rather than reading modules back
// from the page, so the check sees the pixels a phone camera would see.
function decodePng(data: Buffer): { data: Uint8ClampedArray; width: number; height: number } {
  const png = PNG.sync.read(data);
  return { data: new Uint8ClampedArray(png.data), width: png.width, height: png.height };
}

// cropPixels returns one viewport-relative rectangle of a full-page capture.
function cropPixels(
  image: { data: Uint8ClampedArray; width: number; height: number },
  box: { x: number; y: number; width: number; height: number },
  scale: number,
): { data: Uint8ClampedArray; width: number; height: number } {
  const x = Math.round(box.x * scale), y = Math.round(box.y * scale);
  const width = Math.min(Math.round(box.width * scale), image.width - x);
  const height = Math.min(Math.round(box.height * scale), image.height - y);
  const out = new Uint8ClampedArray(width * height * 4);
  for (let row = 0; row < height; row++) {
    const from = ((y + row) * image.width + x) * 4;
    out.set(image.data.subarray(from, from + width * 4), row * width * 4);
  }
  return { data: out, width, height };
}

async function main(): Promise<void> {
  if (process.env.PIIK_CLIENT_INVITE_CODE_GATE !== "true") throw new Error("Invite code gate was not enabled");
  const chromePath = process.env.CHROME_PATH;
  if (!chromePath) throw new Error("CHROME_PATH is required");
  // The harness only removes profiles from its own known prefixes; reuse the
  // media prefix so a failed run cannot leave the Chrome profile behind.
  const profile = await mkdtemp(join(tmpdir(), "piik-client-media-"));
  const port = await reservePort(), debugPort = await reservePort();
  const origin = `http://localhost:${port}`;
  let chrome: ChildProcessWithoutNullStreams | null = null;
  let cdp: CdpConnection | null = null;
  let server: ChildProcessWithoutNullStreams | null = null;
  const checks: Record<string, unknown> = {};
  let stage = "startup", error: string | null = null;
  try {
    await mkdir(BUILD_ROOT, { recursive: true });
    const serverBuild = cargoGateBuild(ROOT, "piik-server");
    build(process.env.ComSpec || "cmd.exe", ["/d", "/s", "/c", "npm run build:web"]);
    build(serverBuild.command, serverBuild.args);
    server = spawn(serverBuild.executable, [], {
      cwd: profile, stdio: "pipe", windowsHide: true,
      env: {
        PATH: process.env.PATH, SystemRoot: process.env.SystemRoot,
        PIIK_ENV: "development", ROOM_DATABASE_PATH: ":memory:", PORT: String(port),
        LISTEN_HOST: "127.0.0.1", PUBLIC_BASE_URL: `http://${LAN}:${port}`,
        ALLOWED_ORIGINS: [`http://localhost:${port}`, `http://127.0.0.1:${port}`].join(","),
      },
    });
    server.stdout.resume(); server.stderr.resume();
    await waitForSample(
      (deadline) => fetchJsonBefore<{ status: string }>(`http://127.0.0.1:${port}/healthz`, deadline),
      (value) => value.status === "ok", 20000);
    chrome = await launchChrome(chromePath, debugPort, profile, [
      "--headless=new", "--no-first-run", "--no-default-browser-check", "--no-proxy-server",
      "--autoplay-policy=no-user-gesture-required",
    ]);
    chrome.stdout.resume(); chrome.stderr.resume();
    const version = await waitForVersion(debugPort, chrome);
    cdp = await CdpConnection.connect(version.webSocketDebuggerUrl, Date.now() + 10000);
    stage = "room";
    const page = await createPage(cdp, `${origin}/#piik-client=1`,
      appShellInitScript("gate") + String.raw`(() => {
        localStorage.setItem('piik:ui-lang', 'en');
        localStorage.setItem('piik:ui-mode', 'text');
      })()`);
    // The invitation row exists once a room does; creating it is the Host's
    // own action and needs no capture.
    await waitForSample((deadline) => evaluate<boolean>(cdp!, page,
      `[...document.querySelectorAll('button')].some((button) => button.getAttribute('aria-label') === 'Create room')`,
      deadline), Boolean, 30000);
    await evaluate(cdp, page,
      `[...document.querySelectorAll('button')].find((button) => button.getAttribute('aria-label') === 'Create room').click()`,
      Date.now() + 5000);
    await waitForSample((deadline) => evaluate<boolean>(cdp!, page,
      `Boolean(document.querySelector('.lr-invite-url'))`, deadline), Boolean, 30000);
    const link = await evaluate<string>(cdp, page, `document.querySelector('.lr-invite-url').value`, Date.now() + 5000);
    checks.link = link;
    stage = "open";
    await waitForSample((deadline) => evaluate<boolean>(cdp!, page,
      `Boolean(document.querySelector('button[aria-label="Invitation QR code"]'))`, deadline), Boolean, 10000);
    // A pointer gesture on the control, as a person would use it. The Host row
    // scrolls, so the button is brought into view first.
    const rect = await evaluate<{ x: number; y: number; width: number; height: number }>(cdp, page,
      `(() => { const button = document.querySelector('button[aria-label="Invitation QR code"]');
        button.scrollIntoView({ block: 'center' });
        const r = button.getBoundingClientRect();
        return { x: r.x + r.width / 2, y: r.y + r.height / 2, width: r.width, height: r.height }; })()`,
      Date.now() + 5000);
    await cdp.call("Input.dispatchMouseEvent",
      { type: "mousePressed", x: rect.x, y: rect.y, button: "left", clickCount: 1 }, page.sessionId, Date.now() + 5000);
    await cdp.call("Input.dispatchMouseEvent",
      { type: "mouseReleased", x: rect.x, y: rect.y, button: "left", clickCount: 1 }, page.sessionId, Date.now() + 5000);
    await waitForSample((deadline) => evaluate<boolean>(cdp!, page,
      `Boolean(document.querySelector('#host-invite-code')?.matches(':popover-open'))`, deadline), Boolean, 10000);
    checks.opened = true;
    await waitForSample((deadline) => evaluate<boolean>(cdp!, page,
      `Boolean(document.querySelector('#host-invite-code svg'))`, deadline), Boolean, 10000);
    // The panel enters with a short animation; sampling mid-scale can capture a
    // half-drawn or throttled frame.
    await new Promise((resolve) => setTimeout(resolve, 400));
    const box = await evaluate<{ x: number; y: number; width: number; height: number }>(cdp, page,
      `(() => { const r = document.querySelector('#host-invite-code svg').getBoundingClientRect();
        return { x: r.x, y: r.y, width: r.width, height: r.height }; })()`, Date.now() + 5000);
    checks.imageBox = box;
    if (box.width < 120) throw new Error(`The rendered code is too small to scan: ${box.width}px`);
    stage = "decode";
    // Capture the whole viewport and crop in Node: CDP's clip semantics differ
    // between viewport and full-page capture, and a wrong crop would silently
    // "fail to decode" rather than prove the code is wrong.
    const shot = await cdp.call<{ data: string }>("Page.captureScreenshot", {
      format: "png",
    }, page.sessionId, Date.now() + 5000);
    const image = Buffer.from(shot.data, "base64");
    await mkdir(BUILD_ROOT, { recursive: true });
    await writeFile(join(BUILD_ROOT, "invite-code.png"), image);
    const pixels = decodePng(image);
    const scale = pixels.width / (await evaluate<number>(cdp, page, "innerWidth", Date.now() + 5000));
    const crop = cropPixels(pixels, box, scale);
    // A downscaled capture or a mid-animation frame can hide the finder
    // patterns; the decode is retried on an upscaled nearest-neighbour copy
    // before the gate calls the code unreadable.
    let decoded = jsQR(crop.data, crop.width, crop.height);
    if (!decoded) {
      const factor = 2;
      const wide = crop.width * factor, tall = crop.height * factor;
      const scaled = new Uint8ClampedArray(wide * tall * 4);
      for (let y = 0; y < tall; y++) {
        for (let x = 0; x < wide; x++) {
          const from = (Math.floor(y / factor) * crop.width + Math.floor(x / factor)) * 4;
          scaled.set(crop.data.subarray(from, from + 4), (y * wide + x) * 4);
        }
      }
      decoded = jsQR(scaled, wide, tall);
    }
    checks.decoded = decoded?.data ?? null;
    checks.panelWithinViewport = await evaluate<boolean>(cdp, page,
      `(() => { const r = document.querySelector('#host-invite-code').getBoundingClientRect();
        return r.left >= 0 && r.top >= 0 && r.right <= innerWidth + 1 && r.bottom <= innerHeight + 1; })()`,
      Date.now() + 3000);
    checks.size = { width: pixels.width, height: pixels.height };
    if (!decoded) throw new Error("The rendered invitation code did not decode");
    if (decoded.data !== link) {
      throw new Error(`The scanned code carries a different link: ${decoded.data}`);
    }
    // A phone-width viewport is the layout the code exists for: reopen there
    // and require the panel to stay fully visible instead of clipping.
    stage = "narrow";
    await cdp.call("Emulation.setDeviceMetricsOverride",
      { width: 390, height: 720, deviceScaleFactor: 2, mobile: true }, page.sessionId, Date.now() + 5000);
    await new Promise((resolve) => setTimeout(resolve, 500));
    const narrowRect = await evaluate<{ x: number; y: number }>(cdp, page,
      `(() => { const button = document.querySelector('button[aria-label="Invitation QR code"]');
        button.scrollIntoView({ block: 'center' });
        const r = button.getBoundingClientRect();
        return { x: r.x + r.width / 2, y: r.y + r.height / 2 }; })()`, Date.now() + 5000);
    await cdp.call("Input.dispatchMouseEvent",
      { type: "mousePressed", x: narrowRect.x, y: narrowRect.y, button: "left", clickCount: 1 }, page.sessionId, Date.now() + 5000);
    await cdp.call("Input.dispatchMouseEvent",
      { type: "mouseReleased", x: narrowRect.x, y: narrowRect.y, button: "left", clickCount: 1 }, page.sessionId, Date.now() + 5000);
    await waitForSample((deadline) => evaluate<boolean>(cdp!, page,
      `Boolean(document.querySelector('#host-invite-code')?.matches(':popover-open'))`, deadline), Boolean, 10000);
    await new Promise((resolve) => setTimeout(resolve, 400));
    const narrow = await evaluate<{ left: number; top: number; right: number; bottom: number; vw: number; vh: number; fits: boolean }>(
      cdp, page,
      `(() => { const r = document.querySelector('#host-invite-code').getBoundingClientRect();
        return { left: r.left, top: r.top, right: r.right, bottom: r.bottom,
          vw: innerWidth, vh: innerHeight,
          fits: r.width > 0 && r.left >= -1 && r.top >= -1 && r.right <= innerWidth + 1 && r.bottom <= innerHeight + 1 }; })()`,
      Date.now() + 5000);
    checks.narrow = narrow;
    if (!narrow.fits) throw new Error(`The code panel is clipped at phone width: ${JSON.stringify(narrow)}`);
    await cdp.call("Emulation.clearDeviceMetricsOverride", {}, page.sessionId, Date.now() + 5000);
    stage = "done";
  } catch (caught) {
    error = caught instanceof Error ? caught.message : String(caught);
  } finally {
    const cleanup = await cleanupRun({ cdp, chrome, server, native: null, profile, ports: [port, debugPort] });
    const result = { passed: error === null && Object.values(cleanup).every(Boolean), stage, checks, error, cleanup };
    await mkdir(BUILD_ROOT, { recursive: true });
    await writeFile(join(BUILD_ROOT, "invite-code.json"), JSON.stringify(result, null, 2));
    process.stdout.write(JSON.stringify(result) + "\n");
    if (!result.passed) process.exitCode = 1;
  }
}

await main();
