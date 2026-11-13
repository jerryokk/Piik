import { spawnSync, type ChildProcessWithoutNullStreams } from "node:child_process";
import { createServer } from "node:http";
import { join } from "node:path";
import { withDeadline } from "./browser-gate-harness";
import type { CaptureEvidence } from "./app-media-gate-types";

export interface GateDrivers {
  run: (command: string, args: string[], cwd?: string, env?: NodeJS.ProcessEnv) => string;
  powershell: () => string;
  sourceHTML: () => string;
  startPageServer: (port: number) => Promise<{ close(): Promise<void> }>;
  writeCaptureControl: (child: ChildProcessWithoutNullStreams, command: string) => void;
  observeCapture: (child: ChildProcessWithoutNullStreams, evidence: CaptureEvidence) => void;
  observeAudioCapture: (child: ChildProcessWithoutNullStreams, evidence: CaptureEvidence) => void;
  stopCapture: (child: ChildProcessWithoutNullStreams) => Promise<void>;
}

export function gateDrivers(root: string, sourceTitle: string): GateDrivers {
  function run(command: string, args: string[], cwd = root, env: NodeJS.ProcessEnv = process.env): string {
    const result = spawnSync(command, args, {
      cwd,
      env,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    });
    if (result.error) throw result.error;
    if (result.status !== 0) {
      throw new Error((result.stderr || result.stdout).trim() || command + " failed");
    }
    return result.stdout.trim();
  }

  function powershell(): string {
    const configured = process.env.PIIK_POWERSHELL?.trim();
    if (configured) return configured;
    return join(
      process.env.SystemRoot || "C:\Windows",
      "System32",
      "WindowsPowerShell",
      "v1.0",
      "powershell.exe",
    );
  }

  function sourceHTML(): string {
    return [
      "<!doctype html><title>" + sourceTitle + "</title>",
      "<style>html,body,canvas{margin:0;width:100%;height:100%;overflow:hidden;background:#101b29}</style>",
      "<canvas width=\"1280\" height=\"720\"></canvas>",
      "<script>",
      "const canvas=document.querySelector('canvas');",
      "const context=canvas.getContext('2d');",
      "let frame=0;",
      "function draw(){",
      "const hue=(frame*3)%360;",
      "context.fillStyle='hsl('+hue+' 70% 45%)';",
      "context.fillRect(0,0,canvas.width,canvas.height);",
      "context.fillStyle='#fff';",
      "context.fillRect((frame*17)%1080,180,200,360);",
      "context.font='64px sans-serif';",
      "context.fillText(String(frame),48,96);",
      "frame+=1;requestAnimationFrame(draw);",
      "}draw();",
      "</script>",
    ].join("");
  }

  function startPageServer(port: number) {
    const server = createServer((_request, response) => {
      response.writeHead(200, {
        "Content-Type": "text/html; charset=utf-8",
        "Cache-Control": "no-store",
      });
      response.end(sourceHTML());
    });
    return new Promise<{ close(): Promise<void> }>((resolveServer, rejectServer) => {
      server.once("error", rejectServer);
      server.listen(port, "127.0.0.1", () => {
        resolveServer({
          close: () => new Promise<void>((resolveClose, rejectClose) => {
            server.close((error) => error ? rejectClose(error) : resolveClose());
          }),
        });
      });
    });
  }

  function writeCaptureControl(child: ChildProcessWithoutNullStreams, command: string): void {
    const payload = Buffer.from(command, "ascii");
    if (payload.length === 0 || payload.length > 64 || !/^[\x20-\x7e]+$/.test(command)) {
      throw new Error("Capture control exceeds its SMED bound");
    }
    const frame = Buffer.alloc(32 + payload.length);
    frame.write("SMED", 0, "ascii");
    frame[4] = 2;
    frame[5] = 7;
    frame.writeUInt32BE(payload.length, 28);
    payload.copy(frame, 32);
    child.stdin.write(frame);
  }

  function observeCapture(
    child: ChildProcessWithoutNullStreams,
    evidence: CaptureEvidence,
  ): void {
    let buffered = Buffer.alloc(0);
    let framesAtRequest = 0;
    child.stdout.on("data", (chunk: Buffer) => {
      buffered = Buffer.concat([buffered, chunk]);
      while (buffered.length >= 32) {
        if (buffered.subarray(0, 4).toString("ascii") !== "SMED" || buffered[4] !== 2) {
          child.kill();
          return;
        }
        const kind = buffered[5]!;
        const flags = buffered[6]!;
        const size = buffered.readUInt32BE(28);
        const maximum = kind === 3 ? 4 * 1024 : 4 * 1024 * 1024;
        if ((size === 0 && kind !== 5) || size > maximum) {
          child.kill();
          return;
        }
        if (buffered.length < 32 + size) return;
        const payload = buffered.subarray(32, 32 + size);
        buffered = buffered.subarray(32 + size);
        if (kind === 3) {
          const state = JSON.parse(payload.toString("utf8")) as { state?: string };
          evidence.starting ||= state.state === "starting";
          evidence.active ||= state.state === "active";
        } else if (kind === 2) {
          evidence.frames += 1;
          if (!evidence.keyFrameRequested && evidence.frames >= 5) {
            evidence.keyFrameRequested = true;
            framesAtRequest = evidence.frames;
            writeCaptureControl(child, "K -1");
          } else if (evidence.keyFrameRequested && evidence.frames > framesAtRequest && flags === 1) {
            evidence.recoveryFrame = true;
          }
        }
      }
    });
  }

  function observeAudioCapture(
    child: ChildProcessWithoutNullStreams,
    evidence: CaptureEvidence,
  ): void {
    let buffered = Buffer.alloc(0);
    child.stdout.on("data", (chunk: Buffer) => {
      buffered = Buffer.concat([buffered, chunk]);
      while (buffered.length >= 32) {
        if (buffered.subarray(0, 4).toString("ascii") !== "SMED" || buffered[4] !== 2) {
          child.kill();
          return;
        }
        const kind = buffered[5]!;
        const size = buffered.readUInt32BE(28);
        const maximum = kind === 3 ? 4 * 1024 : 4 * 1024 * 1024;
        if (size === 0 || size > maximum || buffered.length < 32 + size) {
          if (size === 0 || size > maximum) child.kill();
          return;
        }
        const payload = buffered.subarray(32, 32 + size);
        buffered = buffered.subarray(32 + size);
        if (kind === 3) {
          const state = JSON.parse(payload.toString("utf8")) as {
            state?: string;
            audio?: boolean;
          };
          evidence.audioReady ||= state.state === "active" && state.audio === true;
        } else if (kind === 1) {
          evidence.audioFrames += 1;
          for (let offset = 0; offset + 1 < payload.length; offset += 2) {
            if (payload.readInt16LE(offset) !== 0) {
              evidence.audioNonZeroSamples += 1;
            }
          }
        }
      }
    });
  }

  async function stopCapture(child: ChildProcessWithoutNullStreams): Promise<void> {
    if (child.exitCode !== null) {
      if (child.exitCode !== 0) throw new Error("Native capture exited unsuccessfully");
      return;
    }
    writeCaptureControl(child, "Q");
    const code = await withDeadline(
      () => new Promise<number | null>((resolveExit) => child.once("exit", resolveExit)),
      Date.now() + 3_000,
    );
    if (code !== 0) throw new Error("Native capture did not stop cleanly");
  }

  return { run, powershell, sourceHTML, startPageServer, writeCaptureControl, observeCapture, observeAudioCapture, stopCapture };
}
