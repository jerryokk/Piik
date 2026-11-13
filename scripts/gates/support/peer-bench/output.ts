// Report persistence, git identity and external-server authentication.

import { mkdir, writeFile } from "node:fs/promises";
import { dirname, isAbsolute, resolve } from "node:path";
import {
  closePage,
  createPage,
  evaluate,
  startProcess,
  type CdpConnection,
} from "./harness";
import { REPO_ROOT, type BenchmarkReport, type ExternalServerTarget } from "./types";

export async function gitCommit(): Promise<string | null> {
  const gitProcess = startProcess("git", ["rev-parse", "HEAD"], process.env);
  await new Promise<void>((resolveExit) =>
    gitProcess.child.once("exit", () => resolveExit()),
  );
  return gitProcess.child.exitCode === 0
    ? gitProcess.log.tail().split(/\r?\n/).at(-1) ?? null
    : null;
}

export async function writeReport(report: BenchmarkReport, outputPath: string | null): Promise<void> {
  const json = `${JSON.stringify(report, null, 2)}\n`;
  if (!outputPath) {
    process.stdout.write(json);
    return;
  }
  const resolved = isAbsolute(outputPath) ? outputPath : resolve(REPO_ROOT, outputPath);
  await mkdir(dirname(resolved), { recursive: true });
  await writeFile(resolved, json, "utf8");
  console.error(`Peer topology loopback report: ${resolved}`);
}

export async function authenticateExternalServer(
  cdp: CdpConnection,
  target: ExternalServerTarget,
  signal: AbortSignal,
): Promise<void> {
  const page = await createPage(cdp, target.origin, {
    width: 320,
    height: 180,
    frameRate: 1,
    expectedEndpointCap: 1,
    label: "external-server-access",
    role: "host",
    viewerIndex: null,
    clearHostRoom: false,
  }, signal);
  try {
    const status = await evaluate<{ required: boolean; authenticated: boolean }>(
      cdp,
      page,
      "fetch('/api/site-access').then((response) => response.json())",
    );
    if (!status.required || status.authenticated) return;
    if (!target.accessPassword) {
      throw new Error("External benchmark server requires site access");
    }
    const authenticated = await evaluate<boolean>(
      cdp,
      page,
      `fetch('/api/site-access', { method: 'POST', headers: { Authorization: 'Bearer ' + ${JSON.stringify(target.accessPassword)} } }).then((response) => response.ok)`,
    );
    if (!authenticated) {
      throw new Error("External benchmark server rejected site access");
    }
  } finally {
    await closePage(cdp, page);
  }
}
