// Browser-surface scenes: adaptation limits, warnings, media readiness, room
// entry, browser actions, source switching, name validation and App presence.

import { HostMark } from "../people/HostMark";
import type { ComicTheme } from "../../visual-kinds";
import {
  BrowserWindow,
  DANGER,
  Door,
  FAINT,
  Floor,
  Frame,
  INK_STAGE,
  LINE,
  LIVE,
  MINT,
  MiniTv,
  Pawn,
  RedX,
  SKY,
  STAR_GOLD,
  WARN,
  rmBlock,
} from "./cast";

/** Native bandwidth adaptation: a roomy path narrows, then the delivered
 * picture shrinks. The route owner is deliberately absent; this describes the
 * Browser report without guessing which physical link is responsible. */
export function SceneBandwidthLimited({ theme }: { theme: ComicTheme }) {
  return (
    <>
      <style>{`
.vls-bw-flow-a{animation:vlsBwFlowA var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
.vls-bw-flow-b{animation:vlsBwFlowB var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
.vls-bw-throat{transform-box:fill-box;transform-origin:center;animation:vlsBwThroat var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
.vls-bw-small{transform-box:fill-box;transform-origin:center;animation:vlsBwSmall var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
@keyframes vlsBwFlowA{0%,8%{transform:translateX(-6px);opacity:.45}36%,100%{transform:none;opacity:1}}
@keyframes vlsBwFlowB{0%,18%{transform:translateX(-5px);opacity:.45}46%,100%{transform:none;opacity:1}}
@keyframes vlsBwThroat{0%,30%,100%{transform:scaleY(1)}48%,76%{transform:scaleY(.62)}}
@keyframes vlsBwSmall{0%,42%{transform:scale(1)}58%,100%{transform:scale(.82)}}
${rmBlock(
  ["vls-bw-throat", "vls-bw-small"],
  [
    [".vls-bw-throat", "transform:scaleY(.62)"],
    [".vls-bw-small", "transform:scale(.82)"],
  ],
)}
${rmBlock(["vls-bw-flow-a", "vls-bw-flow-b"], [[".vls-bw-flow-a,.vls-bw-flow-b", "opacity:1;transform:none"]], false)}
`}</style>
      <Frame x={4} w={152} theme={theme} />
      <Frame x={164} w={152} theme={theme} result />
      <Floor x1={18} x2={142} />
      <Floor x1={178} x2={302} />
      <Pawn x={28} yb={76} s={7} eyes />
      <path d="M45 48H90" stroke={SKY} strokeWidth={8} strokeLinecap="round" />
      <g className="vls-bw-flow-a" fill={MINT}>
        <circle cx={55} cy={48} r={3.5} />
        <circle cx={68} cy={48} r={3.5} />
        <circle cx={81} cy={48} r={3.5} />
      </g>
      <MiniTv x={94} y={34} w={44} h={26} />
      <path
        className="vls-bw-throat"
        d="M181 38H208L226 45V53L208 60H181"
        fill="none"
        stroke={WARN}
        strokeWidth={3}
        strokeLinejoin="round"
      />
      <path d="M226 49H250" stroke={WARN} strokeWidth={4} strokeLinecap="round" />
      <g className="vls-bw-flow-b" fill={STAR_GOLD}>
        <circle cx={192} cy={49} r={3.2} />
        <circle cx={207} cy={49} r={3.2} />
        <circle cx={239} cy={49} r={2.6} />
      </g>
      <g className="vls-bw-small">
        <MiniTv x={254} y={41} w={34} h={20} />
      </g>
      <Pawn x={298} yb={76} s={6.5} eyes />
    </>
  );
}

/** Native CPU adaptation: frames queue at the encoder, one falls away, and
 * the delivered picture shrinks. */
export function SceneEncoderLimited({ theme }: { theme: ComicTheme }) {
  return (
    <>
      <style>{`
.vls-en-frame-a{animation:vlsEnFlow var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
.vls-en-frame-b{animation:vlsEnFlow var(--comic-duration,3.2s) ease-in-out .3s var(--comic-repeat,1) both}
.vls-en-drop{transform-box:fill-box;transform-origin:center;animation:vlsEnDrop var(--comic-duration,3.2s) ease-in var(--comic-repeat,1) both}
.vls-en-heat{animation:vlsEnHeat var(--comic-duration,3.2s) ease-out var(--comic-repeat,1) both}
.vls-en-small{transform-box:fill-box;transform-origin:center;animation:vlsEnSmall var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
@keyframes vlsEnFlow{0%,12%{transform:translateX(-7px);opacity:0}28%,60%{transform:none;opacity:1}76%,100%{transform:translateX(8px);opacity:0}}
@keyframes vlsEnDrop{0%,42%{transform:none;opacity:1}64%,100%{transform:translateY(16px) rotate(12deg);opacity:0}}
@keyframes vlsEnHeat{0%,8%{transform:translateY(3px);opacity:.4}36%,100%{transform:none;opacity:1}}
@keyframes vlsEnSmall{0%,46%{transform:scale(1)}62%,100%{transform:scale(.82)}}
${rmBlock(
  ["vls-en-frame-a", "vls-en-frame-b", "vls-en-drop", "vls-en-small"],
  [
    [".vls-en-frame-a,.vls-en-frame-b", "opacity:1;transform:none"],
    [".vls-en-drop", "opacity:0;transform:translateY(16px)"],
    [".vls-en-small", "transform:scale(.82)"],
  ],
)}
${rmBlock(["vls-en-heat"], [[".vls-en-heat", "opacity:1;transform:none"]], false)}
`}</style>
      <Frame x={4} w={152} theme={theme} />
      <Frame x={164} w={152} theme={theme} result />
      <Floor x1={18} x2={142} />
      <Floor x1={178} x2={302} />
      <Pawn x={27} yb={76} s={6.5} eyes />
      <g fill="none" stroke={SKY} strokeWidth={2.5}>
        <rect x={52} y={34} width={36} height={28} rx={5} />
        <path d="M58 30v4m8-4v4m8-4v4m8-4v4M58 62v4m8-4v4m8-4v4m8-4v4" />
      </g>
      <g className="vls-en-frame-a" fill={MINT}>
        <rect x={96} y={38} width={8} height={8} rx={1.5} />
        <rect x={108} y={38} width={8} height={8} rx={1.5} />
      </g>
      <MiniTv x={119} y={37} w={28} h={17} />
      <g fill="none" stroke={WARN} strokeWidth={2.5}>
        <rect x={184} y={34} width={38} height={28} rx={5} />
        <path d="M190 30v4m8-4v4m8-4v4m8-4v4M190 62v4m8-4v4m8-4v4m8-4v4" />
      </g>
      <g className="vls-en-heat" stroke={WARN} strokeWidth={2} strokeLinecap="round">
        <path d="M191 26l-3-6m13 6v-7m10 7l4-6" />
      </g>
      <g className="vls-en-frame-b" fill={STAR_GOLD}>
        <rect x={229} y={35} width={8} height={8} rx={1.5} />
        <rect x={241} y={35} width={8} height={8} rx={1.5} />
      </g>
      <g className="vls-en-drop">
        <rect x={241} y={49} width={8} height={8} rx={1.5} fill={DANGER} />
        <RedX cx={245} cy={53} arm={3} />
      </g>
      <g className="vls-en-small">
        <MiniTv x={258} y={41} w={34} h={20} />
      </g>
      <Pawn x={301} yb={76} s={6} eyes />
    </>
  );
}

/** Generic warning: 1 wide panel. Persistent condition: the you-pawn
 * stands beside the big Hearth coal-bowl warning glyph; heat-rays rise in a
 * staggered 0-55% window, the pawn blinks at 70%, rest >=50%. */
export function SceneWarning({ theme }: { theme: ComicTheme }) {
  return (
    <>
      <style>{`
.vls-wn-ray1{animation:vlsWnRay var(--comic-duration,3.2s) ease-out var(--comic-repeat,1) both}
.vls-wn-ray2{animation:vlsWnRay var(--comic-duration,3.2s) ease-out .25s var(--comic-repeat,1) both}
.vls-wn-ray3{animation:vlsWnRay var(--comic-duration,3.2s) ease-out .5s var(--comic-repeat,1) both}
.vls-wn-eyes{transform-box:fill-box;transform-origin:center;animation:vlsWnBlink var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
@keyframes vlsWnRay{0%,44%,100%{opacity:1;transform:none}12%{opacity:.7;transform:translateY(3px)}26%{opacity:1;transform:translateY(-2px)}}
@keyframes vlsWnBlink{0%,66%,74%,100%{transform:scaleY(1)}70%{transform:scaleY(.12)}}
${rmBlock(
  ["vls-wn-ray1", "vls-wn-ray3", "vls-wn-eyes"],
  [[".vls-wn-ray1,.vls-wn-ray3", "opacity:1;transform:none"]],
)}
${rmBlock(["vls-wn-ray2"], [[".vls-wn-ray2", "opacity:1;transform:translateY(-2px)"]], false)}
`}</style>
      <Frame x={4} w={312} theme={theme} result />
      <Floor x1={24} x2={296} />
      <Pawn x={96} yb={76} s={11} eyes gaze={2} eyeClassName="vls-wn-eyes" />
      {/* Hearth warning glyph x3.5 (~54% panel height), bowl feet on the floor:
          M6 13a6 6 0 0 0 12 0 + rays M12 9V5.5 / M8.5 9.5 7 7.5 / M15.5 9.5 17 7.5,
          baked at origin (168, 9.5). */}
      <path
        d="M189 55 a21 21 0 0 0 42 0"
        stroke="var(--comic-tone, var(--warn))"
        strokeWidth={2.5}
        strokeLinecap="round"
        fill="none"
      />
      <g stroke="var(--comic-tone, var(--warn))" strokeWidth={2.5} strokeLinecap="round" fill="none">
        <g className="vls-wn-ray1" opacity={0}>
          <path d="M197.75 42.75 L192.5 35.75" />
          <path d="M188.5 41 l-4 -3.5" stroke={FAINT} strokeWidth={2} />
        </g>
        <g className="vls-wn-ray2" opacity={0}>
          <path d="M210 41 V28.75" />
        </g>
        <g className="vls-wn-ray3" opacity={0}>
          <path d="M222.25 42.75 L227.5 35.75" />
          <path d="M231.5 41 l4 -3.5" stroke={FAINT} strokeWidth={2} />
        </g>
      </g>
    </>
  );
}

export function SceneMediaStatus({ theme, state }: {
  theme: ComicTheme; state: "playing" | "ready" | "sharing" | "ended" | "preview-paused";
}) {
  const preview = state === "preview-paused";
  const colour = "var(--comic-tone, var(--ink))";
  return <>
    <style>{`
.vls-media-watcher{transform-origin:54px 77px;animation:vlsMediaWatch var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
.vls-media-ended{animation-name:vlsMediaRest}
@keyframes vlsMediaWatch{0%{transform:none}18%{transform:translateX(3px) rotate(4deg)}42%,100%{transform:none}}
@keyframes vlsMediaRest{0%,36%,100%{transform:none}12%{transform:translateX(5px) rotate(3deg)}}
${rmBlock(["vls-media-watcher"], [], false)}
`}</style>
    <Frame x={4} w={312} theme={theme} result />
    <Floor x1={24} x2={296} />
    <Pawn x={54} yb={77} s={13} eyes host={state === "sharing" || state === "ended" || preview} gaze={2}
      className={`vls-media-watcher${state === "ended" ? " vls-media-ended" : ""}`} />
    {preview ? <>
      <BrowserWindow x={92} y={18} w={106} h={60}>
        <MiniTv x={115} y={38} w={60} h={30} />
        <path d="M137 44v13m8-13v13" stroke={colour} strokeWidth={4} />
      </BrowserWindow>
      <path d="M207 49h17m-6-6 6 6-6 6" stroke={LINE} strokeWidth={2} fill="none" />
      <MiniTv x={234} y={32} w={59} h={35} />
      <path d="m258 41 13 8-13 8Z" fill={LIVE} />
    </> : <>
      <MiniTv x={130} y={21} w={124} h={53} />
      {state === "ready"
        ? <path d="m175 47 12 11 24-26" stroke={colour} strokeWidth={4} strokeLinecap="round" strokeLinejoin="round" fill="none" />
        : state === "ended"
          ? <rect x={182} y={36} width={23} height={23} rx={3} fill={INK_STAGE} />
          : <path d="m183 32 27 16-27 16Z" fill={colour} />}
    </>}
  </>;
}

export function SceneRoomEntry({ theme, state }: {
  theme: ComicTheme; state: "closed" | "invalid" | "password";
}) {
  const colour = "var(--comic-tone, var(--ink))";
  return <>
    <style>{`
.vls-entry-closed{transform-origin:51px 76px;animation:vlsEntryStepBack var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
.vls-entry-invalid{animation:vlsEntryRefuse var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
.vls-entry-key{transform-origin:215px 53px;animation:vlsEntryKey var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
@keyframes vlsEntryStepBack{0%,38%,100%{transform:none}12%{transform:translateX(6px) rotate(4deg)}}
@keyframes vlsEntryRefuse{0%{transform:none}10%{transform:translateX(-3px)}18%{transform:translateX(3px)}26%,100%{transform:none}}
@keyframes vlsEntryKey{0%,40%,100%{transform:none}14%{transform:translateX(10px) rotate(8deg)}}
${rmBlock(["vls-entry-closed", "vls-entry-invalid", "vls-entry-key"], [], false)}
`}</style>
    <Frame x={4} w={312} theme={theme} result />
    <Floor x1={24} x2={296} />
    <Pawn x={51} yb={76} s={12} eyes gaze={2} className={state === "closed" ? "vls-entry-closed" : undefined} />
    {state === "password" ? <BrowserWindow x={95} y={15} w={195} h={62}>
      <rect x={154} y={44} width={30} height={24} rx={4} fill="var(--paper)" stroke={colour} strokeWidth={2.5} />
      <path d="M161 44v-8a8 8 0 0 1 16 0v8" stroke={colour} strokeWidth={2.5} fill="none" />
      <g className="vls-entry-key">
        <circle cx={215} cy={53} r={7} stroke={colour} strokeWidth={2.5} fill="none" />
        <path d="M222 53h22m-5 0v6m-8-6v4" stroke={colour} strokeWidth={2.5} fill="none" />
      </g>
    </BrowserWindow> : state === "invalid" ? <>
      <g className="vls-entry-invalid">{[104, 133, 162, 191].map((x, index) => <g key={x}>
        <rect x={x} y={34} width={22} height={29} rx={4} fill="var(--paper)" stroke={index === 3 ? colour : LINE} strokeWidth={2} />
        {index < 3 && <circle cx={x + 11} cy={48} r={3} fill={LINE} />}
      </g>)}</g>
      <RedX cx={251} cy={48} arm={10} />
    </> : <>
      <Door x={166} y={18} />
      <path d="M208 31h51" stroke={colour} strokeWidth={3} />
      <rect x={221} y={41} width={24} height={24} rx={3} fill={colour} />
    </>}
  </>;
}

export function SceneBrowserAction({ theme, action }: {
  theme: ComicTheme; action: "refresh" | "debug" | "export-failed" | "copy-failed" | "settings-failed";
}) {
  const colour = "var(--comic-tone, var(--ink))";
  return <>
    <style>{`
.vls-browser-refresh{transform-origin:235px 55px;animation:vlsBrowserRefresh var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
.vls-browser-debug{animation:vlsBrowserLift var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
.vls-browser-export-failed,.vls-browser-copy-failed,.vls-browser-settings-failed{animation:vlsBrowserRefuse var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
@keyframes vlsBrowserRefresh{0%{transform:rotate(-360deg)}12%{transform:rotate(-300deg) scale(.72)}32%{transform:rotate(-30deg) scale(.72)}44%,100%{transform:none}}
@keyframes vlsBrowserLift{0%,40%,100%{transform:none}10%{transform:translateY(6px)}22%{transform:translateY(-2px)}}
@keyframes vlsBrowserRefuse{0%{transform:none}10%{transform:translateX(-3px)}18%{transform:translateX(3px)}26%,100%{transform:none}}
${rmBlock(["vls-browser-refresh", "vls-browser-debug", "vls-browser-export-failed", "vls-browser-copy-failed", "vls-browser-settings-failed"], [], false)}
`}</style>
    <Frame x={4} w={152} theme={theme} />
    <Frame x={164} w={152} theme={theme} result />
    <BrowserWindow x={18} y={20} w={113} h={58}>
      {action === "settings-failed" ? <>
        <path d="M35 44h68M35 55h68M35 66h68" stroke={LINE} strokeWidth={2.5} />
        <g fill="var(--paper)" stroke={LINE} strokeWidth={2.5}>
          <circle cx={54} cy={44} r={4} /><circle cx={82} cy={55} r={4} /><circle cx={65} cy={66} r={4} />
        </g>
      </> : action === "debug" || action === "export-failed"
        ? <path d="M37 63V51m17 12V41m17 22V48m17 15V37" stroke={LINE} strokeWidth={7} strokeLinecap="round" />
        : <path d="M33 44h63M33 55h47M33 66h63" stroke={LINE} strokeWidth={3} strokeLinecap="round" />}
    </BrowserWindow>
    {action === "settings-failed" ? <BrowserWindow x={179} y={20} w={113} h={58}>
      <path d="M197 44h67M197 55h67M197 66h67" stroke={LINE} strokeWidth={2.5} />
      <g className="vls-browser-settings-failed" fill="var(--paper)" stroke={LINE} strokeWidth={2.5}>
        <circle cx={216} cy={44} r={4} /><circle cx={244} cy={55} r={4} /><circle cx={227} cy={66} r={4} />
      </g>
      <RedX cx={280} cy={69} arm={9} />
    </BrowserWindow> : action === "refresh" || action === "debug" ? <BrowserWindow x={179} y={20} w={113} h={58}>
      {action === "refresh" ? <path className="vls-browser-refresh" d="M218 44a18 18 0 1 1-1 20m1-20h14m-14 0V31" stroke={colour} strokeWidth={3} strokeLinecap="round" strokeLinejoin="round" fill="none" /> : <>
        <path className="vls-browser-debug" d="M193 64V53m15 11V44m15 20V50" stroke={LINE} strokeWidth={6} strokeLinecap="round" />
        <circle cx={262} cy={52} r={11} fill="none" stroke={colour} strokeWidth={2.5} />
        <circle cx={262} cy={52} r={5} fill={colour} />
      </>}
    </BrowserWindow> : <>
      <rect x={199} y={27} width={48} height={50} rx={5} fill="var(--paper)" stroke={LINE} strokeWidth={2.5} />
      <g className={`vls-browser-${action}`}>
        <rect x={217} y={15} width={48} height={50} rx={5} fill="var(--paper)" stroke={colour} strokeWidth={2.5} />
        <path d="M226 31h28M226 42h20M226 53h28" stroke={LINE} strokeWidth={2.5} strokeLinecap="round" />
      </g>
      <RedX cx={273} cy={70} arm={10} />
    </>}
  </>;
}

export function SceneSourceSwitching({ theme, state = "switching" }: {
  theme: ComicTheme; state?: "switching" | "starting" | "failed";
}) {
  return <>
    <style>{`
.vls-source-switch{animation:vlsSourceSwitch var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
.vls-source-return{animation-name:vlsSourceReturn}
.vls-source-host{transform-origin:39px 76px;animation:vlsSourceHost var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
@keyframes vlsSourceSwitch{0%{transform:translateX(-5px);opacity:.55}25%{transform:translateX(5px);opacity:1}45%,100%{transform:none;opacity:1}}
@keyframes vlsSourceReturn{0%{transform:translateX(5px);opacity:.55}25%{transform:translateX(-5px);opacity:1}45%,100%{transform:none;opacity:1}}
@keyframes vlsSourceHost{0%,38%,100%{transform:none}12%{transform:translateX(4px) rotate(4deg)}}
${rmBlock(["vls-source-switch"], [[".vls-source-switch", "opacity:1"]])}
${rmBlock(["vls-source-host"], [], false)}
`}</style>
    <Frame x={4} w={312} theme={theme} result />
    <Pawn x={39} yb={76} s={10} eyes host gaze={2} className={state === "failed" ? "vls-source-host" : undefined} />
    <BrowserWindow x={75} y={24} w={71} h={47}>
      <path d="M87 61l18-23 11 14 8-8 12 17Z" fill={SKY} />
    </BrowserWindow>
    {state === "switching" ? <BrowserWindow x={225} y={24} w={71} h={47}>
      <circle cx={260} cy={50} r={11} fill={SKY} />
    </BrowserWindow> : <MiniTv x={225} y={26} w={71} h={43} />}
    {state === "failed" ? <RedX cx={185} cy={51} arm={11} /> : <g stroke="var(--comic-tone, var(--busy))" strokeWidth={3} strokeLinecap="round" strokeLinejoin="round" fill="none">
      <path className="vls-source-switch" d={state === "switching" ? "M158 42h51m-9-8 9 8-9 8" : "M158 51h51m-9-8 9 8-9 8"} />
      {state === "switching" && <path className="vls-source-switch vls-source-return" d="M209 61h-51m9-8-9 8 9 8" />}
    </g>}
  </>;
}

export function SceneNameInvalid({ theme }: { theme: ComicTheme }) {
  return <>
    <style>{`
.vls-name-card{animation:vlsNameRefuse var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
@keyframes vlsNameRefuse{0%{transform:none}10%{transform:translateX(-3px)}18%{transform:translateX(3px)}26%,100%{transform:none}}
${rmBlock(["vls-name-card"], [], false)}
`}</style>
    <Frame x={4} w={312} theme={theme} result />
    <Pawn x={71} yb={76} s={16} eyes gaze={2} />
    <g className="vls-name-card">
      <rect x={125} y={27} width={136} height={39} rx={10} fill="var(--paper)" stroke={LINE} strokeWidth={2.5} />
      <path d="M142 40h66M142 52h47" stroke={LINE} strokeWidth={3} strokeLinecap="round" />
    </g>
    <RedX cx={246} cy={64} arm={10} />
  </>;
}

/** host-needs-app: 2 panels. A browser holds only the dashed (absent) Host
 * crown; beside the Piik App badge the person wears the crown and hops.
 * The App badge keeps its fixed tile palette in both themes. */
export function SceneHostNeedsApp({ theme }: { theme: ComicTheme }) {
  return <>
    <style>{`
.vls-ha-crown{animation:vlsHaCrown var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
.vls-ha-host{animation:vlsHaHop var(--comic-duration,3.2s) cubic-bezier(.3,1.5,.5,1) var(--comic-repeat,1) both}
@keyframes vlsHaCrown{0%,100%{opacity:.35}22%,40%{opacity:.75}}
@keyframes vlsHaHop{0%,12%{transform:none}22%{transform:translateY(-7px)}32%{transform:translateY(1px)}38%,100%{transform:none}}
${rmBlock(["vls-ha-crown", "vls-ha-host"], [[".vls-ha-crown", "opacity:.55"], [".vls-ha-host", "transform:none"]], false)}
`}</style>
    <Frame x={4} w={152} theme={theme} />
    <Frame x={164} w={152} theme={theme} result />
    <BrowserWindow x={20} y={18} w={120} h={60} />
    <Pawn x={48} yb={72} s={9} eyes gaze={2} />
    <HostMark x={78} y={38} width={30} dashed className="vls-ha-crown" opacity={0.55} />
    <rect x={180} y={16} width={56} height={56} rx={13} fill="#233841" />
    <g fill="none" stroke="#f4f3e9" strokeWidth={3} strokeLinecap="round" strokeLinejoin="round">
      <rect x={190} y={32} width={36} height={27} rx={6} />
      <path d="M200.5 26l7.5 6 7.5-6M199 63.5l3-4.5m15 4.5-3-4.5M212.5 45.5c2.25-2.7 4.8-2.7 7.05 0" />
    </g>
    <circle cx={201.25} cy={44.75} r={1.9} fill="#f4f3e9" />
    <Pawn x={272} yb={76} s={12} eyes host gaze={-2} className="vls-ha-host" />
  </>;
}
