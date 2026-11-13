// Status scenes: waiting, connecting, signaling, tap-to-play, paused, recovery,
// route/playback failure, host offline and missing audio. Each scene owns its
// vls- prefixed keyframes and settles through rmBlock.

import { HostMark } from "../people/HostMark";
import type { ComicTheme } from "../../visual-kinds";
import {
  BrowserWindow,
  DANGER,
  FAINT,
  Floor,
  Frame,
  INK_STAGE,
  LINE,
  LIVE,
  MINT,
  MiniTv,
  Moon,
  Pawn,
  Plug,
  RedX,
  ServerBox,
  SKY,
  Socket,
  Spark,
  Static,
  TV_EDGE,
  WARN,
  YOU,
  rmBlock,
} from "./cast";

/** 1. waiting-for-host: 1 wide panel. */
export function SceneWaiting({ theme }: { theme: ComicTheme }) {
  return (
    <>
      <style>{`
.vls-wf-z1{animation:vlsWfZNear var(--comic-duration,3.2s) ease-out var(--comic-repeat,1) both}
.vls-wf-z2{animation:vlsWfZFar var(--comic-duration,3.2s) ease-out var(--comic-repeat,1) both}
@keyframes vlsWfZNear{0%,52%,100%{opacity:.65;transform:none}24%{opacity:.85;transform:translate(2px,-4px)}}
@keyframes vlsWfZFar{0%,6%{opacity:0;transform:translate(-12px,14px)}18%{opacity:.75}48%{opacity:.55;transform:none}76%,100%{opacity:0;transform:translate(6px,-8px)}}
${rmBlock(
  ["vls-wf-z1", "vls-wf-z2"],
  [[".vls-wf-z1", "opacity:.65;transform:none"], [".vls-wf-z2", "opacity:.55;transform:none"]],
  false,
)}
`}</style>
      <Frame x={4} w={312} theme={theme} result />
      <Floor x1={24} x2={296} />
      <Pawn x={69} yb={76} s={11} eyes gaze={2} />
      <MiniTv x={206} y={28} w={64} h={42} />
      <Moon x={229} y={48} />
      <g stroke={LINE} strokeWidth={2.5} strokeLinecap="round" fill="none">
        <path className="vls-wf-z1" opacity={0.65} d="M246 40h8l-8 6h8" />
        <g className="vls-wf-z2" opacity={0.55}>
          <path d="M258 28h9l-9 7h9" />
          <path d="M273 14h10l-10 8h10" />
        </g>
      </g>
      <circle cx={238} cy={76} r={3.5} fill={WARN} />
    </>
  );
}

/** 2/3. connecting P2P / SFU: 3 panels. */
export function SceneConnecting({ theme, sfu }: { theme: ComicTheme; sfu: boolean }) {
  const k = sfu ? "vls-cs" : "vls-cn";
  const dotKf = sfu
    ? `@keyframes ${k}Dot{0%{transform:translate(0,0);opacity:0}6%{opacity:1}26%{transform:translate(10px,-31px)}40%{transform:translate(36px,-31px);opacity:1}56%{transform:translate(46px,-12px);opacity:1}68%{transform:translate(46px,-12px);opacity:1}78%,100%{transform:translate(46px,-12px);opacity:0}}
@keyframes ${k}Slots{0%,26%{opacity:.25}34%,62%{opacity:1}72%,100%{opacity:.25}}`
    : `@keyframes ${k}Dot{0%{transform:translate(0,0);opacity:0}8%{opacity:1}55%{transform:translate(26px,0);opacity:1}70%{transform:translate(26px,0);opacity:1}80%,100%{transform:translate(26px,0);opacity:0}}`;
  const kills = sfu
    ? [`${k}-line`, `${k}-dot`, `${k}-slots`, `${k}-warm`, `${k}-led`]
    : [`${k}-line`, `${k}-dot`, `${k}-warm`, `${k}-led`];
  const pins: Array<readonly [string, string]> = [
    [`.${k}-dot`, "opacity:0"],
    [`.${k}-warm`, "opacity:.2"],
    [`.${k}-led`, "opacity:1"],
  ];
  if (sfu) pins.push([`.${k}-slots`, "opacity:1"]);
  return (
    <>
      <style>{`
.${k}-line{animation:${k}March var(--comic-duration,3.2s) linear var(--comic-repeat,1) both}
.${k}-dot{animation:${k}Dot var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
${sfu ? `.${k}-slots{animation:${k}Slots var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}` : ""}
.${k}-warm{animation:${k}Warm var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
.${k}-led{animation:${k}Led var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
@keyframes ${k}March{to{stroke-dashoffset:-16}}
${dotKf}
@keyframes ${k}Warm{0%,52%,100%{opacity:0}60%{opacity:.25}66%{opacity:.1}74%{opacity:.3}}
@keyframes ${k}Led{0%,100%{opacity:1}50%{opacity:.3}}
${rmBlock(kills, pins)}
`}</style>
      <Frame x={8} w={96} theme={theme} />
      <Frame x={112} w={96} theme={theme} />
      <Frame x={216} w={96} theme={theme} result />
      <Floor x1={20} x2={92} />
      <Pawn x={56} yb={76} s={11} eyes />
      <Pawn x={128} yb={74} s={7} eyes color={SKY} />
      {sfu ? (
        <>
          <g stroke={LINE} strokeWidth={2.5} strokeLinecap="round" fill="none" strokeDasharray="4 4">
            <path className={`${k}-line`} d="M137 57 L147 26" />
            <path className={`${k}-line`} d="M173 26 L183 45" />
          </g>
          <ServerBox x={148} y={15} slotsClass={`${k}-slots`} />
          <g className={`${k}-dot`}>
            <circle cx={137} cy={57} r={2.5} fill={MINT} />
          </g>
          <MiniTv x={170} y={40} w={24} h={16} />
          <circle cx={182} cy={61} r={2} fill={TV_EDGE} />
        </>
      ) : (
        <>
          <path
            className={`${k}-line`}
            d="M137 57 H166"
            stroke={LINE}
            strokeWidth={2.5}
            strokeLinecap="round"
            fill="none"
            strokeDasharray="4 4"
          />
          <g className={`${k}-dot`}>
            <path d="M132 52 l-6 -3 M132 62 l-6 3" stroke={FAINT} strokeWidth={2} strokeLinecap="round" />
            <circle cx={140} cy={57} r={2.5} fill={MINT} />
          </g>
          <MiniTv x={170} y={42} w={24} h={16} />
          <circle cx={182} cy={63} r={2} fill={TV_EDGE} />
        </>
      )}
      <Floor x1={228} x2={300} />
      <MiniTv x={238} y={26} w={52} h={40} />
      <rect className={`${k}-warm`} x={244} y={32} width={40} height={24} rx={4} fill={SKY} opacity={0} />
      <circle className={`${k}-led`} cx={264} cy={72} r={3.5} fill="#4a84f2" />
    </>
  );
}

/** The page's control link, not a media path: no TV or video packets here. */
export function SceneSignal({ theme, state, peer = false }: {
  theme: ComicTheme;
  state: "connecting" | "recovering" | "offline" | "connected" | "failed";
  peer?: boolean;
}) {
  const color = "var(--comic-tone, var(--ink))";
  return (
    <>
      <style>{`
.vls-signal-message{animation:vlsSignalMessage var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
.vls-signal-retry{transform-box:fill-box;transform-origin:center;animation:vlsSignalRetry var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
.vls-signal-page{animation:vlsSignalPage var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
@keyframes vlsSignalMessage{0%{transform:translateX(0);opacity:0}8%{opacity:1}40%{transform:translateX(82px);opacity:1}55%,100%{transform:translateX(82px);opacity:0}}
@keyframes vlsSignalRetry{0%{transform:rotate(0)}55%,100%{transform:rotate(360deg)}}
@keyframes vlsSignalPage{0%,12%,48%,100%{transform:none}22%{transform:translateY(-3px)}34%{transform:translateY(1px)}}
${rmBlock(["vls-signal-message", "vls-signal-retry"], [
  [".vls-signal-message", "opacity:1;transform:translateX(41px)"],
  [".vls-signal-retry", "transform:none"],
])}
${rmBlock(["vls-signal-page"], [[".vls-signal-page", "transform:none"]], false)}
`}</style>
      <Frame x={4} w={312} theme={theme} result />
      <g className={state === "connecting" || state === "recovering" ? undefined : "vls-signal-page"}>
        <BrowserWindow x={24} y={23} w={96} h={50}>
          <circle cx={37} cy={43} r={3} fill={color} />
          <path d="M47 43h56 M34 55h36 M77 55h26 M34 64h69" stroke={LINE} strokeWidth={2} strokeLinecap="round" />
        </BrowserWindow>
      </g>
      {peer ? <BrowserWindow x={251} y={25} w={47} h={46} /> : <ServerBox x={254} y={25} w={42} h={46} />}
      <path
        d={state === "connecting" ? "M130 48H244" : "M130 48H164 M204 48H244"}
        stroke={color}
        strokeWidth={2.5}
        strokeDasharray="4 5"
        strokeLinecap="round"
        fill="none"
      />
      {state === "connecting" ? (
        <>
          <path d="M140 42l-6 6 6 6 M234 42l6 6-6 6" stroke={color} strokeWidth={2.5} strokeLinecap="round" strokeLinejoin="round" fill="none" />
          <g className="vls-signal-message">
            <rect x={136} y={40} width={16} height={13} rx={2} fill="var(--paper)" stroke={color} strokeWidth={2} />
            <path d="M137 42l7 5 7-5" stroke={color} strokeWidth={1.5} strokeLinejoin="round" fill="none" />
          </g>
        </>
      ) : state === "recovering" ? (
        <g className="vls-signal-retry" stroke={color} strokeWidth={2.5} strokeLinecap="round" strokeLinejoin="round" fill="none">
          <path d="M175 43a11 11 0 0 1 19-2l2 3 M190 44h6v-6 M193 53a11 11 0 0 1-19 2l-2-3 M178 52h-6v6" />
        </g>
      ) : state === "connected" ? (
        <path d="M174 48l7 7 14-17" stroke={color} strokeWidth={3} strokeLinecap="round" strokeLinejoin="round" fill="none" />
      ) : state === "failed" ? (
        <RedX cx={184} cy={48} arm={8} />
      ) : (
        <g stroke={color} strokeWidth={2.5} strokeLinecap="round" fill="none">
          <path d="M174 43h5 M174 53h5 M189 43h5 M189 53h5 M176 59l16-22" />
        </g>
      )}
    </>
  );
}

/** 4. tap-to-play: 2 panels. Companion to the real big button. */
export function SceneTap({ theme }: { theme: ComicTheme }) {
  return (
    <>
      <style>{`
.vls-tp-finger{animation:vlsTpFinger var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
.vls-tp-btn{transform-box:fill-box;transform-origin:center;animation:vlsTpBtn var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
.vls-tp-ripple{transform-box:fill-box;transform-origin:center;animation:vlsTpRipple var(--comic-duration,3.2s) ease-out var(--comic-repeat,1) both}
.vls-tp-flash{animation:vlsTpFlash var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
.vls-tp-play{transform-box:fill-box;transform-origin:center;animation:vlsTpPlay var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
.vls-tp-led{animation:vlsTpLed var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
.vls-tp-arcs{animation:vlsTpArcs var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
@keyframes vlsTpFinger{0%,18%{transform:translate(0,0)}42%,52%{transform:translate(30px,-18px)}78%,100%{transform:translate(0,0)}}
@keyframes vlsTpBtn{0%,40%{transform:scale(1)}46%{transform:scale(.86)}54%{transform:scale(1.05)}60%,100%{transform:scale(1)}}
@keyframes vlsTpRipple{0%,42%{transform:scale(1);opacity:0}45%{opacity:.75}72%,100%{transform:scale(1.65);opacity:0}}
@keyframes vlsTpFlash{0%,48%{opacity:0}54%{opacity:.4}66%,100%{opacity:0}}
@keyframes vlsTpPlay{0%,50%{transform:scale(1)}57%{transform:scale(1.18)}66%,100%{transform:scale(1)}}
@keyframes vlsTpLed{0%,50%{opacity:.3}56%,82%{opacity:1}100%{opacity:.3}}
@keyframes vlsTpArcs{0%,52%{opacity:0}58%{opacity:.9}64%{opacity:.15}72%{opacity:.9}86%,100%{opacity:0}}
${rmBlock(
  ["vls-tp-finger", "vls-tp-btn", "vls-tp-ripple", "vls-tp-flash", "vls-tp-play", "vls-tp-led", "vls-tp-arcs"],
  [
    [".vls-tp-ripple,.vls-tp-flash", "opacity:0"],
    [".vls-tp-arcs", "opacity:.5"],
    [".vls-tp-led", "opacity:1"],
  ],
)}
`}</style>
      <Frame x={4} w={152} theme={theme} />
      <Frame x={164} w={152} theme={theme} result />
      <circle className="vls-tp-ripple" cx={80} cy={48} r={24} fill="none" stroke={SKY} strokeWidth={3} />
      <g className="vls-tp-btn">
        <circle cx={80} cy={48} r={24} fill="#2f6fed" />
        <path d="M74 39 L91 48 L74 57 Z" fill="#fff" />
      </g>
      <g className="vls-tp-finger">
        <path d="M22 80 l7 -7 M27 85 l7 -7" stroke={FAINT} strokeWidth={2.5} strokeLinecap="round" />
        <circle cx={34} cy={72} r={7} fill={INK_STAGE} />
      </g>
      <MiniTv x={200} y={22} w={88} h={52} />
      <rect className="vls-tp-flash" x={206} y={28} width={76} height={36} rx={4} fill={INK_STAGE} opacity={0} />
      <path className="vls-tp-play" d="M236 38 L254 46 L236 54 Z" fill={MINT} />
      <circle className="vls-tp-led" cx={244} cy={80} r={3.5} fill={LIVE} opacity={0.3} />
      <g className="vls-tp-arcs" opacity={0} stroke={MINT} strokeWidth={2.5} strokeLinecap="round" fill="none">
        <path d="M294 38 a12 12 0 0 1 0 16" />
        <path d="M300 33 a20 20 0 0 1 0 26" />
      </g>
    </>
  );
}

/** 5. host-paused: 2 panels. Cozy, not broken — no z's, no red. */
export function ScenePaused({ theme }: { theme: ComicTheme }) {
  return (
    <>
      <style>{`
.vls-hp-bars{animation:vlsHpBars var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
.vls-hp-led{animation:vlsHpLed var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
.vls-hp-eyes{transform-box:fill-box;transform-origin:center;animation:vlsHpBlink var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
.vls-hp-steam1{animation:vlsHpSteam var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
.vls-hp-steam2{animation:vlsHpSteam var(--comic-duration,3.2s) ease-in-out 1.5s var(--comic-repeat,1) both}
@keyframes vlsHpBars{0%,100%{opacity:.55}50%{opacity:1}}
@keyframes vlsHpLed{0%,100%{opacity:1}50%{opacity:.3}}
@keyframes vlsHpBlink{0%,44%,52%,100%{transform:scaleY(1)}48%{transform:scaleY(.12)}}
@keyframes vlsHpSteam{0%{transform:translateY(2px);opacity:.4}20%{opacity:.9}45%,100%{transform:translateY(-2px);opacity:.55}}
${rmBlock(
  ["vls-hp-bars", "vls-hp-led", "vls-hp-eyes", "vls-hp-steam2"],
  [
    [".vls-hp-bars,.vls-hp-led", "opacity:1"],
    [".vls-hp-steam2", "opacity:0"],
  ],
)}
${rmBlock(["vls-hp-steam1"], [[".vls-hp-steam1", "opacity:.55;transform:translateY(-2px)"]], false)}
`}</style>
      <Frame x={4} w={152} theme={theme} />
      <Frame x={164} w={152} theme={theme} result />
      <MiniTv x={36} y={16} w={88} h={54} />
      <rect x={42} y={22} width={76} height={38} rx={4} fill="#0a101c" opacity={0.5} />
      <g className="vls-hp-bars" fill={INK_STAGE}>
        <rect x={70} y={30} width={9} height={26} rx={3.5} />
        <rect x={85} y={30} width={9} height={26} rx={3.5} />
      </g>
      <circle className="vls-hp-led" cx={80} cy={76} r={3.5} fill={WARN} />
      <Floor x1={176} x2={304} y={78} />
      <Pawn x={216} yb={78} s={14} eyes host eyeClassName="vls-hp-eyes" />
      <rect x={240} y={66} width={12} height={12} rx={3} fill="#e4572e" />
      <path d="M252 69 a5 5 0 0 1 0 6" stroke="#e4572e" strokeWidth={2.5} fill="none" />
      <path className="vls-hp-steam1" d="M243 60 q3 -4 0 -8" stroke={LINE} strokeWidth={2} strokeLinecap="round" fill="none" opacity={0} />
      <path className="vls-hp-steam2" d="M249 60 q-3 -4 0 -8" stroke={LINE} strokeWidth={2} strokeLinecap="round" fill="none" opacity={0} />
    </>
  );
}

/** 6. recovering: 3 panels. Plug re-seats, LEDs chase, one sweat drop. */
export function SceneRecovering({ theme }: { theme: ComicTheme }) {
  return (
    <>
      <style>{`
.vls-rc-spark{animation:vlsRcSpark var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
.vls-rc-plug{transform-box:fill-box;transform-origin:center;animation:vlsRcPlug var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
.vls-rc-plug-lines{animation:vlsRcPlugLines var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
.vls-rc-sweat{transform-box:fill-box;transform-origin:center;animation:vlsRcSweat var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
.vls-rc-c1{animation:vlsRcC1 var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
.vls-rc-c2{animation:vlsRcC2 var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
.vls-rc-c3{animation:vlsRcC3 var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
@keyframes vlsRcSpark{0%,30%,100%{opacity:.55}10%{opacity:1}}
@keyframes vlsRcPlug{0%{transform:translate(-4px,-9px) rotate(-6deg)}12%{transform:translate(-2px,-4px) rotate(5deg)}22%{transform:translate(0,-1px) rotate(-2deg)}30%,100%{transform:translate(0,0) rotate(0)}}
@keyframes vlsRcPlugLines{0%{opacity:0}6%{opacity:.9}28%,100%{opacity:0}}
@keyframes vlsRcSweat{0%,53%{opacity:0;transform:translateY(0)}56%{opacity:1}72%,100%{opacity:0;transform:translateY(9px)}}
@keyframes vlsRcC1{0%,24%{opacity:.25}32%,58%{opacity:1}66%,100%{opacity:.25}}
@keyframes vlsRcC2{0%,34%{opacity:.25}42%,64%{opacity:1}72%,100%{opacity:.25}}
@keyframes vlsRcC3{0%,44%{opacity:.25}52%,70%{opacity:1}78%,100%{opacity:.25}}
${rmBlock(
  ["vls-rc-spark", "vls-rc-plug", "vls-rc-plug-lines", "vls-rc-sweat", "vls-rc-c1", "vls-rc-c2", "vls-rc-c3"],
  [
    [".vls-rc-plug", "transform:none"],
    [".vls-rc-plug-lines", "opacity:0"],
    [".vls-rc-spark", "opacity:1"],
    [".vls-rc-c1,.vls-rc-c2,.vls-rc-c3", "opacity:1"],
    [".vls-rc-sweat", "opacity:0"],
  ],
)}
`}</style>
      <Frame x={8} w={96} theme={theme} />
      <Frame x={112} w={96} theme={theme} />
      <Frame x={216} w={96} theme={theme} result />
      <Floor x1={20} x2={92} />
      <Pawn x={40} yb={76} s={8} eyes />
      <MiniTv x={66} y={44} w={26} h={18} />
      <path d="M48 58 H53 M59 58 H64" stroke={LINE} strokeWidth={2.5} strokeLinecap="round" fill="none" />
      <Spark x={56} y={52} className="vls-rc-spark" />
      <Socket x={170} y={50} />
      <g className="vls-rc-plug-lines" opacity={0}>
        <path d="M156 34 l-7 -4 M158 44 l-8 0" stroke={FAINT} strokeWidth={2} strokeLinecap="round" />
      </g>
      <Plug x={180} y={30} className="vls-rc-plug" />
      <Floor x1={228} x2={300} />
      <Pawn x={252} yb={76} s={9} eyes />
      <path
        className="vls-rc-sweat"
        d="M264 46 c2 3 2 5 0 6 c-2 -1 -2 -3 0 -6 Z"
        fill={SKY}
        opacity={0}
      />
      <circle className="vls-rc-c1" cx={272} cy={34} r={3} fill={`var(--comic-tone, ${WARN})`} opacity={0.25} />
      <circle className="vls-rc-c2" cx={284} cy={34} r={3} fill={`var(--comic-tone, ${WARN})`} opacity={0.25} />
      <circle className="vls-rc-c3" cx={296} cy={34} r={3} fill={`var(--comic-tone, ${WARN})`} opacity={0.25} />
    </>
  );
}

/** 7. route-failed: the viewer cannot reach the same TV; no route history implied. */
export function SceneRouteFailed({ theme }: { theme: ComicTheme }) {
  return (
    <>
      <style>{`
.vls-rf-panel{transform-box:fill-box;transform-origin:center;animation:vlsRfIn var(--comic-duration,3.2s) cubic-bezier(.3,1.5,.5,1) var(--comic-repeat,1) both}
.vls-rf-p2{animation-delay:.2s}
.vls-rf-march{animation:vlsRfMarch var(--comic-duration,3.2s) linear .4s var(--comic-repeat,1) both}
.vls-rf-x{transform-box:fill-box;transform-origin:center;animation:vlsRfX var(--comic-duration,3.2s) ease-out .8s var(--comic-repeat,1) both}
.vls-rf-sweat{animation:vlsRfSweat var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
@keyframes vlsRfIn{from{opacity:0;transform:translateY(6px) scale(.9)}}
@keyframes vlsRfMarch{from{stroke-dashoffset:0}to{stroke-dashoffset:-16}}
@keyframes vlsRfX{from{opacity:0;transform:scale(1.5)}to{opacity:1;transform:scale(1)}}
@keyframes vlsRfSweat{0%,50%,100%{opacity:.7;transform:translateY(0)}24%{opacity:1;transform:translateY(3px)}}
${rmBlock(
  ["vls-rf-panel", "vls-rf-march", "vls-rf-x"],
  [
    [".vls-rf-x", "opacity:1;transform:none"],
  ],
)}
${rmBlock(["vls-rf-sweat"], [[".vls-rf-sweat", "opacity:.7;transform:none"]], false)}
`}</style>
      <g className="vls-rf-panel">
        <Frame x={4} w={152} theme={theme} />
        <Floor x1={16} x2={144} />
        <Pawn x={32} yb={76} s={10} eyes />
        <MiniTv x={94} y={36} w={44} h={30} />
        <path className="vls-rf-march" d="M44 55 H61 M75 55 H92" stroke={LINE} strokeWidth={2.5} strokeDasharray="4 4" strokeLinecap="round" fill="none" />
        <path d="M66 49 l-3 6 7 0 -3 6" stroke={DANGER} strokeWidth={2} strokeLinecap="round" strokeLinejoin="round" fill="none" />
      </g>
      <g className="vls-rf-panel vls-rf-p2">
        <Frame x={164} w={152} theme={theme} result />
        <Floor x1={176} x2={304} />
        <Pawn x={192} yb={76} s={10} eyes />
        <MiniTv x={254} y={36} w={44} h={30} />
        <circle cx={288} cy={62} r={2} fill={DANGER} />
        <path d="M204 55 H218 M238 55 H252" stroke={FAINT} strokeWidth={2.5} strokeLinecap="round" fill="none" />
        <RedX cx={228} cy={55} arm={6} className="vls-rf-x" />
        <path className="vls-rf-sweat" d="M206 46 c2 3 2 5 0 6 c-2 -1 -2 -3 0 -6 Z" fill={SKY} opacity={0.35} />
      </g>
    </>
  );
}

/** 8. playback-failed: 2 panels. The current picture is unavailable. */
export function ScenePlaybackFailed({ theme }: { theme: ComicTheme }) {
  return (
    <>
      <style>{`
.vls-pf-s1{animation:vlsPfStaticA var(--comic-duration,3.2s) steps(3) var(--comic-repeat,1) both}
.vls-pf-s2{animation:vlsPfStaticB var(--comic-duration,3.2s) steps(3) var(--comic-repeat,1) both}
.vls-pf-rip{animation:vlsPfRip var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
.vls-pf-pawn{transform-box:fill-box;transform-origin:50% 100%;animation:vlsPfTap var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
.vls-pf-ring{animation:vlsPfRing var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
@keyframes vlsPfStaticA{0%{transform:translate(0,0);opacity:.5}33%{transform:translate(1px,-1px);opacity:.8}66%{transform:translate(-1px,1px);opacity:.35}100%{transform:translate(0,0);opacity:.5}}
@keyframes vlsPfStaticB{0%{transform:translate(0,0);opacity:.35}33%{transform:translate(-1px,1px);opacity:.7}66%{transform:translate(1px,-1px);opacity:.45}100%{transform:translate(0,0);opacity:.35}}
@keyframes vlsPfRip{0%,18%{opacity:.35}22%{opacity:.7}28%{opacity:.35}33%{opacity:.7}42%,100%{opacity:.35}}
@keyframes vlsPfTap{0%,16%{transform:translate(0,0)}20%{transform:translate(4px,0)}24%{transform:translate(0,0)}31%{transform:translate(0,0)}35%{transform:translate(4px,0)}39%,100%{transform:translate(0,0)}}
@keyframes vlsPfRing{0%,55%{opacity:.25}63%{opacity:1}78%,100%{opacity:.25}}
${rmBlock(
  ["vls-pf-s2", "vls-pf-rip", "vls-pf-pawn", "vls-pf-ring"],
  [
    [".vls-pf-s2,.vls-pf-rip", "opacity:.35;transform:none"],
    [".vls-pf-ring", "opacity:1"],
  ],
)}
${rmBlock(["vls-pf-s1"], [[".vls-pf-s1", "opacity:.5;transform:none"]], false)}
`}</style>
      <Frame x={4} w={152} theme={theme} />
      <Frame x={164} w={152} theme={theme} result />
      <Floor x1={16} x2={60} />
      <Pawn x={34} yb={76} s={8} eyes />
      <path d="M44 62 H68" stroke={LINE} strokeWidth={2.5} strokeLinecap="round" fill="none" />
      <MiniTv x={70} y={30} w={64} h={40} />
      <Static
        cls="vls-pf-s1"
        pts={[[78, 38], [92, 34], [108, 40], [122, 36], [84, 50], [100, 46], [116, 52], [126, 48]]}
      />
      <Static
        cls="vls-pf-s2"
        pts={[[86, 42], [104, 36], [118, 44], [80, 56], [96, 54], [112, 48], [124, 56], [90, 57]]}
      />
      <Floor x1={176} x2={304} y={78} />
      <Pawn x={200} yb={78} s={9} eyes className="vls-pf-pawn" />
      <MiniTv x={232} y={26} w={66} h={42} />
      <Static
        cls="vls-pf-rip"
        pts={[[240, 34], [254, 32], [270, 38], [284, 34], [246, 46], [262, 44], [278, 48], [252, 52], [268, 54], [284, 50]]}
      />
      <g className="vls-pf-ring" opacity={0.25}>
        <circle cx={306} cy={18} r={9} fill="none" stroke={SKY} strokeWidth={2.5} />
        <path d="M311 11 l3 -2 M312 13 l4 0" stroke={SKY} strokeWidth={2.5} strokeLinecap="round" fill="none" />
      </g>
    </>
  );
}

/** 9. host-offline: 2 panels. Plug pulled; dashed Host crown = gone. */
export function SceneHostOffline({ theme }: { theme: ComicTheme }) {
  return (
    <>
      <style>{`
.vls-ho-plug{transform-box:fill-box;transform-origin:50% 0%;animation:vlsHoSway var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
.vls-ho-pawn{transform-box:fill-box;transform-origin:50% 100%;animation:vlsHoWave var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
.vls-ho-badge{animation:vlsHoBadge var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
@keyframes vlsHoSway{0%{transform:rotate(-3deg)}14%{transform:rotate(3deg)}28%{transform:rotate(-2deg)}42%,100%{transform:rotate(0)}}
@keyframes vlsHoWave{0%,10%{transform:rotate(0)}14%{transform:rotate(-8deg)}18%{transform:rotate(8deg)}22%{transform:rotate(-8deg)}26%{transform:rotate(8deg)}30%,100%{transform:rotate(0)}}
@keyframes vlsHoBadge{0%,30%{opacity:.2}40%{opacity:.7}50%,100%{opacity:.2}}
${rmBlock(
  ["vls-ho-pawn", "vls-ho-badge"],
  [[".vls-ho-badge", "opacity:.5"]],
)}
${rmBlock(["vls-ho-plug"], [[".vls-ho-plug", "transform:none"]], false)}
`}</style>
      <Frame x={4} w={152} theme={theme} />
      <Frame x={164} w={152} theme={theme} result />
      <MiniTv x={30} y={22} w={70} h={44} />
      <rect x={36} y={28} width={58} height={30} rx={4} fill="#0a101c" />
      <path d="M65 66 c0 4 -4 6 -8 7" stroke={TV_EDGE} strokeWidth={2.5} fill="none" strokeLinecap="round" />
      <Plug x={57} y={73} className="vls-ho-plug" />
      <Floor x1={176} x2={304} y={78} />
      <Pawn x={210} yb={78} s={9} eyes className="vls-ho-pawn" />
      <HostMark x={257} y={43} width={30} dashed className="vls-ho-badge" opacity={0.5} />
    </>
  );
}

/** 10. no-audio: 2 panels. Before: TV alive (mint play triangle,
 * flicker ticks), three healthy LIVE-green sound arcs. After: picture still
 * alive, sound dashed + red-slashed, and a big you-pawn (s=18) presses BOTH
 * mitten-hands to its head — arms stamp up once and hold, head micro-shakes
 * "I hear nothing". Rest >=44%. */
export function SceneNoAudio({ theme }: { theme: ComicTheme }) {
  return (
    <>
      <style>{`
.vls-na-flick{animation:vlsNaFlick var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
.vls-na-a1{animation:vlsNaArc var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
.vls-na-a2{animation:vlsNaArc var(--comic-duration,3.2s) ease-in-out -.3s var(--comic-repeat,1) both}
.vls-na-a3{animation:vlsNaArc var(--comic-duration,3.2s) ease-in-out -.6s var(--comic-repeat,1) both}
.vls-na-pawn{transform-box:fill-box;transform-origin:50% 100%;animation:vlsNaShake var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
.vls-na-arm{transform-box:fill-box;transform-origin:50% 100%;animation:vlsNaArm var(--comic-duration,3.2s) cubic-bezier(.3,1.4,.5,1) var(--comic-repeat,1) both}
.vls-na-slash{transform-box:fill-box;transform-origin:center;animation:vlsNaSlash var(--comic-duration,3.2s) ease-out var(--comic-repeat,1) both}
@keyframes vlsNaFlick{0%{opacity:.15}10%{opacity:.8}20%{opacity:.2}30%{opacity:.7}40%,100%{opacity:.15}}
@keyframes vlsNaArc{0%{opacity:.45}12%{opacity:1}26%{opacity:.45}42%,100%{opacity:.8}}
@keyframes vlsNaArm{0%,8%{opacity:0;transform:translateY(10px)}20%,100%{opacity:1;transform:translateY(0)}}
@keyframes vlsNaSlash{0%,22%{opacity:0;transform:scale(1.7)}30%,100%{opacity:1;transform:scale(1)}}
@keyframes vlsNaShake{0%,10%,42%,100%{transform:rotate(0)}16%{transform:rotate(-2.5deg)}22%{transform:rotate(2.5deg)}28%{transform:rotate(-1.8deg)}34%{transform:rotate(1.8deg)}}
${rmBlock(
  ["vls-na-flick", "vls-na-a1", "vls-na-a2", "vls-na-a3", "vls-na-arm", "vls-na-slash"],
  [
    [".vls-na-flick", "opacity:.5"],
    [".vls-na-a1,.vls-na-a2,.vls-na-a3", "opacity:.8"],
    [".vls-na-slash", "opacity:1;transform:none"],
    [".vls-na-arm", "transform:none"],
  ],
)}
${rmBlock(["vls-na-pawn"], [[".vls-na-pawn", "transform:none"]], false)}
`}</style>
      <Frame x={4} w={152} theme={theme} />
      <Frame x={164} w={152} theme={theme} result />
      {/* panel 1 (before): picture alive, three healthy sound arcs */}
      <MiniTv x={26} y={20} w={76} h={48} />
      <path d="M54 34 L74 42 L54 50 Z" fill={MINT} />
      <g className="vls-na-flick" stroke={SKY} strokeWidth={2} strokeLinecap="round" opacity={0.15}>
        <path d="M38 30v6 M42 40v5" />
        <path d="M92 31v6 M88 41v5" />
      </g>
      <circle cx={64} cy={74} r={3} fill={LIVE} />
      <g stroke={LIVE} strokeWidth={2.5} fill="none" strokeLinecap="round">
        <path className="vls-na-a1" d="M106 36a10 10 0 0 1 0 16" />
        <path className="vls-na-a2" d="M112 31a16 16 0 0 1 0 26" />
        <path className="vls-na-a3" d="M118 26a22 22 0 0 1 0 36" />
      </g>
      {/* panel 2 (after): picture still alive on the mini TV, but the sound
          arcs arrive dashed and carry an unavailable mark; the pawn raises
          two floating round hands toward its ears. */}
      <MiniTv x={172} y={30} w={42} h={30} />
      <path d="M187 37.5 L199 43 L187 48.5 Z" fill={MINT} />
      <Floor x1={176} x2={308} y={85} />
      <g
        stroke={LINE}
        strokeWidth={2.5}
        fill="none"
        strokeLinecap="round"
        strokeDasharray="4 3"
        opacity={0.5}
      >
        <path d="M218 38a8 8 0 0 1 0 12" />
        <path d="M223 33.5a14 14 0 0 1 0 21" />
        <path d="M228 29a20 20 0 0 1 0 32" />
      </g>
      <path
        className="vls-na-slash"
        d="M216 29 L250 63"
        stroke="var(--comic-tone)"
        strokeWidth={3.5}
        strokeLinecap="round"
        fill="none"
      />
      <g className="vls-na-pawn">
        <Pawn x={282} yb={85} s={16} eyes gaze={-2} />
        <g className="vls-na-arm">
          <circle cx={269} cy={52} r={3.5} fill={YOU} />
        </g>
        <g className="vls-na-arm">
          <circle cx={295} cy={52} r={3.5} fill={YOU} />
        </g>
      </g>
    </>
  );
}
