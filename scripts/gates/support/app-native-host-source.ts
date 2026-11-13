// Deterministic capture source fixture and browser launcher shared by the
// native Host gates. Only gate tooling consumes this module.

import type { ChildProcessWithoutNullStreams } from "node:child_process";
import { createServer } from "node:http";
import { join, resolve } from "node:path";
import {
  CdpConnection,
  cleanupRun,
  launchChrome,
  waitForVersion,
} from "./browser-gate-harness";
import { run, stopChild, waitForChild, waitForValue } from "./app-native-host-driver";

const ROOT = resolve(import.meta.dirname, "..", "..", "..");
export const SOURCE_TITLE = "Piik Native Gate Source";

export function powershell(): string {
  return process.env.PIIK_POWERSHELL?.trim() || join(
    process.env.SystemRoot || "C:\\Windows",
    "System32",
    "WindowsPowerShell",
    "v1.0",
    "powershell.exe",
  );
}

function sourceHTML(): string {
  return [
    "<!doctype html><title>" + SOURCE_TITLE + "</title>",
    "<style>html,body,canvas{margin:0;width:100%;height:100%;overflow:hidden;background:#102030}</style>",
    "<canvas width=\"1280\" height=\"720\"></canvas><script>",
    "const c=document.querySelector('canvas'),x=c.getContext('2d');let n=0;",
    "function f(){x.fillStyle='hsl('+(n*5%360)+' 70% 45%)';x.fillRect(0,0,1280,720);",
    "x.fillStyle='#fff';x.fillRect((n*19)%1100,160,180,400);x.font='60px sans-serif';x.fillText(n,42,92);n++;requestAnimationFrame(f)}f();",
    "</script>",
  ].join("");
}

export async function sourceServer(port: number): Promise<{ close(): Promise<void> }> {
  const server = createServer((_request, response) => {
    response.writeHead(200, {
      "Content-Type": "text/html; charset=utf-8",
      "Cache-Control": "no-store",
    });
    response.end(sourceHTML());
  });
  await new Promise<void>((resolveListen, rejectListen) => {
    server.once("error", rejectListen);
    server.listen(port, "127.0.0.1", resolveListen);
  });
  return {
    close: () => new Promise<void>((resolveClose, rejectClose) => {
      server.close((error) => error ? rejectClose(error) : resolveClose());
    }),
  };
}

export async function startSourceBrowser(
  chromePath: string,
  profile: string,
  debugPort: number,
  sourcePort: number,
): Promise<{
  child: ChildProcessWithoutNullStreams;
  cdp: CdpConnection;
}> {
  const child = await launchChrome(chromePath, debugPort, profile, [
    "--no-first-run", "--no-default-browser-check",
    "--disable-extensions", "--disable-logging",
    "--disable-background-timer-throttling",
    "--disable-backgrounding-occluded-windows",
    "--disable-renderer-backgrounding",
    "--app=http://127.0.0.1:" + sourcePort + "/",
  ]);
  child.stdout.resume();
  child.stderr.resume();
  try {
    const version = await waitForVersion(debugPort, child);
    const cdp = await CdpConnection.connect(
      version.webSocketDebuggerUrl,
      Date.now() + 10_000,
    );
    return { child, cdp };
  } catch (error) {
    await cleanupRun({ chrome: child, cdp: null, native: null, server: null, profile: null, ports: [debugPort] });
    throw error;
  }
}

export async function closeSourceBrowser(
  child: ChildProcessWithoutNullStreams,
  cdp: CdpConnection,
): Promise<boolean> {
  try {
    await cdp.call("Browser.close", {}, undefined, Date.now() + 2_000);
  } catch {}
  cdp.close();
  try {
    await waitForChild(child, 5_000);
    return true;
  } catch {
    return stopChild(child);
  }
}

export async function waitForCaptureWindow(captureBinary: string): Promise<void> {
  await waitForValue(
    async () => JSON.parse(run(captureBinary, ["--list"], ROOT, process.env, 5_000)) as Array<{
      title?: unknown;
    }>,
    (targets) => targets.some((target) =>
      typeof target.title === "string" && target.title.includes(SOURCE_TITLE)
    ),
    10_000,
  );
}
