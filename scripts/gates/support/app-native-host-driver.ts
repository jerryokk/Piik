// Browser- and process-driving helpers for the native Host gate: bounded
// waits, DOM control clicks, codec readback and the App endpoint handshake.

import {
  spawnSync,
  type ChildProcess,
  type ChildProcessWithoutNullStreams,
} from "node:child_process";
import { resolve } from "node:path";
import {
  CdpConnection,
  evaluate,
  type PageHandle,
  withDeadline,
} from "./browser-gate-harness";
import {
  decodeAppEndpoint,
  type AppEndpoint as Endpoint,
} from "./app-gate-endpoint";

const ROOT = resolve(import.meta.dirname, "..", "..", "..");

export type GateMode = "local" | "one-link";
export type VideoCodec = "h264" | "vp8";

export function run(
  command: string,
  args: string[],
  cwd = ROOT,
  environment: NodeJS.ProcessEnv = process.env,
  timeoutMs = 120_000,
): string {
  const result = spawnSync(command, args, {
    cwd,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
    env: environment,
    timeout: timeoutMs,
    killSignal: "SIGKILL",
    windowsHide: true,
  });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    throw new Error((result.stderr || result.stdout).trim() || command + " failed");
  }
  return result.stdout.trim();
}

export async function waitForChild(
  child: ChildProcessWithoutNullStreams | ChildProcess,
  timeoutMs: number,
): Promise<number | null> {
  if (child.exitCode !== null) return child.exitCode;
  return withDeadline(
    () => new Promise<number | null>((resolveExit, rejectExit) => {
      child.once("error", rejectExit);
      child.once("exit", resolveExit);
    }),
    Date.now() + timeoutMs,
  );
}

export async function stopChild(child: ChildProcessWithoutNullStreams | ChildProcess | null): Promise<boolean> {
  if (!child || child.exitCode !== null || child.signalCode !== null) return true;
  child.kill();
  try {
    await waitForChild(child, 5_000);
  } catch {
    if (process.platform === "win32" && child.pid !== undefined) {
      spawnSync("taskkill.exe", ["/PID", String(child.pid), "/T", "/F"], {
        stdio: "ignore",
        timeout: 5_000,
        windowsHide: true,
      });
      try {
        await waitForChild(child, 2_000);
      } catch {
        // The process state below is the final cleanup check.
      }
    }
  }
  return child.exitCode !== null || child.signalCode !== null;
}

export async function selectNativeSource(
  cdp: CdpConnection,
  page: PageHandle,
  kind: "window" | "display",
  title?: string,
): Promise<void> {
  await waitForValue(
    (deadline) => evaluate<boolean>(
      cdp,
      page,
      `(() => {
        const tab = document.querySelector('button[data-source-tab="${kind}"]');
        if (!tab || tab.disabled) return false;
        tab.click();
        return true;
      })()`,
      deadline,
    ),
    Boolean,
    15_000,
  );
  const selector = kind === "display"
    ? "button[data-native-source^='display:']"
    : "button[data-native-source]";
  const predicate = title
    ? `(button) => button.textContent?.includes(${JSON.stringify(title)})`
    : "() => true";
  await waitForValue(
    (deadline) => evaluate<boolean>(
      cdp,
      page,
      `([...document.querySelectorAll(${JSON.stringify(selector)})].some(${predicate}))`,
      deadline,
    ),
    Boolean,
    15_000,
  );
  await evaluate<void>(
    cdp,
    page,
    `([...document.querySelectorAll(${JSON.stringify(selector)})].find(${predicate}))?.click()`,
    Date.now() + 5_000,
  );
}

export async function clickHostControl(
  cdp: CdpConnection,
  page: PageHandle,
  buttonExpression: string,
): Promise<void> {
  await cdp.call("Page.bringToFront", {}, page.sessionId, Date.now() + 5_000);
  const point = await evaluate<{ x: number; y: number }>(
    cdp,
    page,
    `new Promise((resolve) => {
      const button = ${buttonExpression};
      if (!button || button.disabled || button.getAttribute('aria-disabled') === 'true')
        throw new Error('Host control is unavailable');
      button.scrollIntoView({block: 'center'});
      requestAnimationFrame(() => requestAnimationFrame(() => {
        const rect = button.getBoundingClientRect();
        resolve({x: rect.x + rect.width / 2, y: rect.y + rect.height / 2});
      }));
    })`,
    Date.now() + 5_000,
  );
  for (const type of ["mouseMoved", "mousePressed", "mouseReleased"]) {
    await cdp.call("Input.dispatchMouseEvent", {
      type, ...point, button: "left", clickCount: 1,
    }, page.sessionId, Date.now() + 5_000);
  }
}

export async function readVideoCodec(cdp: CdpConnection, page: PageHandle): Promise<VideoCodec> {
  const codec = await waitForValue(
    (deadline) => evaluate<VideoCodec | null>(cdp, page, "window.__piikGateVideoCodec()", deadline),
    (value) => value === "vp8" || value === "h264",
    5_000,
  );
  if (codec === null) throw new Error("No decoded video codec was observed");
  return codec;
}

export async function assertVideoCodec(cdp: CdpConnection, page: PageHandle, expected: VideoCodec): Promise<void> {
  const codec = await readVideoCodec(cdp, page);
  if (codec !== expected) throw new Error(`Native codec changed from ${expected} to ${codec}`);
}


export async function waitForValue<T>(
  sample: (deadline: number) => Promise<T>,
  accept: (value: T) => boolean,
  timeoutMs: number,
): Promise<T> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const value = await withDeadline(() => sample(deadline), deadline);
      if (accept(value)) return value;
    } catch {
      // A transient page or signaling state is retried within the gate bound.
    }
    await new Promise((resolveWait) => setTimeout(resolveWait, 100));
  }
  throw new Error("Native Host gate timed out");
}

export async function readAppEndpoint(
  child: ChildProcessWithoutNullStreams,
  requirePublicOrigin = false,
): Promise<{
  endpoint: Endpoint;
  password: string;
  publicOrigin: string | null;
}> {
  let buffered = "";
  const lines: string[] = [];
  let endpoint: Endpoint | null = null;
  return withDeadline(
    () => new Promise((resolveEndpoint, rejectEndpoint) => {
      const onData = (chunk: Buffer) => {
        buffered += chunk.toString();
        for (;;) {
          const newline = buffered.indexOf("\n");
          if (newline < 0) return;
          const line = buffered.slice(0, newline).trim();
          buffered = buffered.slice(newline + 1);
          if (!line) continue;
          lines.push(line);
          try {
            endpoint = decodeAppEndpoint(line);
          } catch {
            // Informational lines are printed after the endpoint.
          }
          const passwordLine = lines.find((entry) => entry.startsWith("Local access password: "));
          const openAccessLine = lines.find((entry) => entry === "Local access: open");
          const publicOriginLine = lines.find((entry) =>
            entry.startsWith("Public invitation origin: ")
          );
          if (endpoint && (passwordLine || openAccessLine) &&
            (!requirePublicOrigin || publicOriginLine)) {
            child.stdout.off("data", onData);
            resolveEndpoint({
              endpoint,
              password: passwordLine
                ? passwordLine.slice("Local access password: ".length)
                : "",
              publicOrigin:
                publicOriginLine?.slice("Public invitation origin: ".length) ?? null,
            });
            return;
          }
        }
      };
      child.stdout.on("data", onData);
      child.once("error", rejectEndpoint);
      child.once("exit", (code) =>
        rejectEndpoint(new Error("App exited before readiness (" + String(code) + ")")),
      );
    }),
    Date.now() + 15_000,
  );
}

