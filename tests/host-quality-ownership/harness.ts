// Exercise the actual page owners without mounting capture hardware or a Browser.
import { readFileSync } from "node:fs";
import ts from "typescript";
import { vi } from "vitest";
import { QUALITY_PROFILES, type QualitySettings } from "../../src/client/media/quality";

const readPage = (path: string) => readFileSync(new URL(path, import.meta.url), "utf8");
// The page owner set spans the page component and the pure helpers it imports.
const sources = [
  ts.createSourceFile("HostPage.tsx", readPage("../../src/client/pages/HostPage.tsx"), ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX),
  ts.createSourceFile("types.ts", readPage("../../src/client/pages/host/types.ts"), ts.ScriptTarget.Latest, true, ts.ScriptKind.TS),
];
const owners = new Set(["changeQuality", "commitQuality", "rememberQuality", "loadAppQualityPreference", "handleSignalMessage",
  "switchSource", "watchCaptureEnd", "switchNativeSource", "switchToNativeSource", "createBrowserAudio", "retireNativeSenderPeers", "finishSourceSwitch", "replaceBrowserStream", "recoverBrowserFanout", "disposeNativeShare",
  "acquireNativeClient", "requestSharing", "startNativeShare", "startBrowserNativeIngress",
  "connectNativeClient", "handleNativeClientClosed", "releaseUnusedNativeClient", "closeCaptureSourcePicker",
  "openCaptureSourcePicker", "startBrowserShareFromPicker", "startNativeShareFromPicker",
  "startSharing", "connectRoomSignal", "createReplacementRoom", "beginRoomMutation", "finishRoomMutation", "setCaptureError", "toggleSharingPause",
  "startPeer", "reconcileHostChildren", "setNotice", "setNoticeKey", "setStatusNotice", "setNoticeError", "setNoticeErrorKey", "endSharing", "disposeResources", "hostTerminationKey", "copyRoomLink", "isCurrentRoomAuthority",
  "handleRoomAccessFailure", "replaceCurrentRoom", "forgetRoom", "endRoomSession", "readableError"]);

const functions: string[] = [];
let noticeRendering = "";
function collect(node: ts.Node): void {
  if (ts.isFunctionDeclaration(node) && node.name && owners.has(node.name.text)) {
    functions.push(node.getText(node.getSourceFile()));
    owners.delete(node.name.text);
  }
  if (ts.isVariableDeclaration(node) && node.name.getText(node.getSourceFile()) === "noticeText") {
    noticeRendering = node.initializer!.getText(node.getSourceFile());
  }
  ts.forEachChild(node, collect);
}
for (const file of sources) collect(file);
if (owners.size || !noticeRendering) throw new Error(`Missing Host owner: ${[...owners].join(", ")}`);
// Moved helpers bring their own export keywords and import block; the sandbox
// resolves neither, so both are stripped before the function bodies run.
const body = functions.join("\n")
  .replace(/^\s*import\s[\s\S]*?\sfrom\s"[^"]*";\s*$/gm, "")
  .replace(/\bexport\s+(function|const|class)\s/g, "$1 ");
export const executable = ts.transpileModule(body, {
  compilerOptions: { target: ts.ScriptTarget.ES2022 },
}).outputText;

export const original = QUALITY_PROFILES["1080p30"];
export const lower = QUALITY_PROFILES["720p30"];
export function ref<T>(current: T) { return { current }; }
export function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((next, fail) => { resolve = next; reject = fail; });
  return { promise, resolve, reject };
}
export function noticeTextSource(): string {
  return noticeRendering;
}
export function ingress() {
  return { updateProfile: vi.fn(async (_profile: QualitySettings) => true), dispose: vi.fn() };
}
