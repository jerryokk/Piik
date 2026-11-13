// Room-access scenes: missing room, denied access, invalid invitation and a full
// room. Paper surfaces carry the verdict marks.

import type { ComicTheme } from "../../visual-kinds";
import {
  BrowserWindow,
  DANGER,
  Door,
  FAINT,
  Frame,
  InviteLink,
  Pawn,
  RedX,
  rmBlock,
} from "./cast";

/** 11. room-not-found: 2 paper panels. The door isn't there. */
export function SceneNotFound({ theme }: { theme: ComicTheme }) {
  return (
    <>
      <style>{`
.vls-rn-knock{animation:vlsRnKnock var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
.vls-rn-kn{animation:vlsRnKn var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
.vls-rn-solid{animation:vlsRnSolid var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
.vls-rn-ghost{animation:vlsRnGhost var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
.vls-rn-puff{animation:vlsRnPuff var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
@keyframes vlsRnKnock{0%{transform:translateX(0)}6%{transform:translateX(5px)}12%{transform:translateX(0)}18%{transform:translateX(5px)}25%,100%{transform:translateX(0)}}
@keyframes vlsRnKn{0%,3%{opacity:0}7%{opacity:.9}11%{opacity:0}19%{opacity:.9}24%,100%{opacity:0}}
@keyframes vlsRnSolid{0%,30%{opacity:1}45%,80%{opacity:0}92%,100%{opacity:1}}
@keyframes vlsRnGhost{0%,30%{opacity:0}45%,80%{opacity:.9}92%,100%{opacity:0}}
@keyframes vlsRnPuff{0%,38%{opacity:0;transform:translateY(3px)}50%{opacity:.6}68%,100%{opacity:0;transform:translateY(-4px)}}
${rmBlock(
  ["vls-rn-kn", "vls-rn-solid", "vls-rn-ghost", "vls-rn-puff"],
  [[".vls-rn-solid", "opacity:0"], [".vls-rn-ghost", "opacity:.9"], [".vls-rn-puff,.vls-rn-kn", "opacity:0"]],
)}
${rmBlock(["vls-rn-knock"], [[".vls-rn-knock", "transform:none"]], false)}
`}</style>
      <Frame x={4} w={152} theme={theme} />
      <Frame x={164} w={152} theme={theme} result />
      <g className="vls-rn-knock">
        <Pawn x={63} yb={74} s={11} eyes gaze={2} />
      </g>
      <g className="vls-rn-kn" stroke="var(--ink)" strokeWidth={2} strokeLinecap="round" opacity={0}>
        <path d="M84 36l-7-3M82 46h-8M84 56l-7 3" />
      </g>
      <Door x={96} y={18} />
      <g className="vls-rn-solid">
        <Door x={250} y={18} />
      </g>
      <g className="vls-rn-ghost" opacity={0}>
        <rect
          x={250}
          y={18}
          width={32}
          height={54}
          rx={5}
          fill="none"
          stroke="var(--ink)"
          strokeWidth={2.5}
          strokeDasharray="5 4"
        />
      </g>
      <g className="vls-rn-puff" fill="var(--ink)" opacity={0}>
        <circle cx={250} cy={30} r={3} />
        <circle cx={262} cy={24} r={4} />
        <circle cx={272} cy={32} r={2.5} />
      </g>
    </>
  );
}

/** 12. access-denied: 2 paper panels. Door there, won't open. */
export function SceneAccessDenied({ theme }: { theme: ComicTheme }) {
  return (
    <>
      <style>{`
.vls-ad-door{transform-box:fill-box;transform-origin:center;animation:vlsAdShake var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
.vls-ad-x{transform-box:fill-box;transform-origin:center;animation:vlsAdX var(--comic-duration,3.2s) ease-out var(--comic-repeat,1) both}
@keyframes vlsAdShake{0%{transform:translateX(0)}3%{transform:translateX(-3px)}6%{transform:translateX(3px)}9%{transform:translateX(-2px)}12%{transform:translateX(2px)}15%,100%{transform:translateX(0)}}
@keyframes vlsAdX{0%,18%{opacity:0;transform:scale(1.6) rotate(8deg)}24%,100%{opacity:1;transform:scale(1) rotate(8deg)}}
${rmBlock(["vls-ad-x"], [[".vls-ad-x", "opacity:1;transform:scale(1) rotate(8deg)"]])}
${rmBlock(["vls-ad-door"], [[".vls-ad-door", "transform:none"]], false)}
`}</style>
      <Frame x={4} w={152} theme={theme} />
      <Frame x={164} w={152} theme={theme} result />
      <Pawn x={48} yb={76} s={9} eyes />
      <rect x={60} y={38} width={16} height={11} rx={2} fill="var(--paper)" stroke="var(--ink)" strokeWidth={2} />
      <circle cx={72} cy={43.5} r={1.6} fill="var(--ink)" />
      <Door x={100} y={18} />
      <rect x={112} y={40} width={10} height={13} rx={2} fill="var(--paper)" stroke="var(--ink)" strokeWidth={1.5} />
      <circle cx={117} cy={44} r={1.6} fill="var(--ink)" />
      <Pawn x={190} yb={76} s={9} eyes />
      <rect x={202} y={46} width={16} height={11} rx={2} fill="var(--paper)" stroke="var(--ink)" strokeWidth={2} />
      <circle cx={214} cy={51.5} r={1.6} fill="var(--ink)" />
      <RedX cx={210} cy={51.5} arm={5.5} className="vls-ad-x" />
      <g className="vls-ad-door">
        <Door x={242} y={18} />
        <rect x={254} y={40} width={10} height={13} rx={2} fill="var(--paper)" stroke="var(--ink)" strokeWidth={1.5} />
        <circle cx={259} cy={44} r={2} fill={DANGER} />
      </g>
    </>
  );
}

/** 13. invalid-invite: this invitation URL cannot admit the viewer. */
export function SceneInvalidInvite({ theme }: { theme: ComicTheme }) {
  return (
    <>
      <style>{`
.vls-ii-link{animation:vlsIiLink var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
@keyframes vlsIiLink{0%,8%,36%,100%{transform:none}17%{transform:translateX(-4px)}26%{transform:translateX(3px)}}
${rmBlock(["vls-ii-link"], [[".vls-ii-link", "transform:none"]], false)}
`}</style>
      <Frame x={4} w={312} theme={theme} result />
      <Pawn x={53} yb={81} s={15} eyes gaze={2} />
      <BrowserWindow x={94} y={15} w={200} h={65}>
        <rect x={106} y={33} width={176} height={35} rx={5} fill="var(--wall-2)" stroke={FAINT} strokeWidth={1.5} />
        <InviteLink x={114} y={34} size={32} className="vls-ii-link" />
        <path d="M160 44h69m-69 12h52" stroke={FAINT} strokeWidth={2} strokeLinecap="round" />
        <RedX cx={264} cy={51} arm={8} />
      </BrowserWindow>
    </>
  );
}

/** 14. room-full: 2 paper panels. Cute first, verdict clear. */
export function SceneRoomFull({ theme }: { theme: ComicTheme }) {
  const crowd = [
    "var(--pawn-1)",
    "var(--pawn-2)",
    "var(--pawn-3)",
    "var(--pawn-4)",
    "var(--pawn-5)",
    "var(--pawn-6)",
    "var(--pawn-7)",
  ];
  return (
    <>
      <style>{`
.vls-fl-c1{animation:vlsFlSquish var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
.vls-fl-c2{animation:vlsFlSquish var(--comic-duration,3.2s) ease-in-out .09s var(--comic-repeat,1) both}
.vls-fl-c3{animation:vlsFlSquish var(--comic-duration,3.2s) ease-in-out .18s var(--comic-repeat,1) both}
.vls-fl-c4{animation:vlsFlSquish var(--comic-duration,3.2s) ease-in-out .27s var(--comic-repeat,1) both}
.vls-fl-c5{animation:vlsFlSquish var(--comic-duration,3.2s) ease-in-out .36s var(--comic-repeat,1) both}
.vls-fl-c6{animation:vlsFlSquish var(--comic-duration,3.2s) ease-in-out .45s var(--comic-repeat,1) both}
.vls-fl-c7{animation:vlsFlSquish var(--comic-duration,3.2s) ease-in-out .54s var(--comic-repeat,1) both}
.vls-fl-you{transform-box:fill-box;transform-origin:50% 100%;animation:vlsFlBob var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
.vls-fl-ghost{animation:vlsFlGhost var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
@keyframes vlsFlSquish{0%{transform:translateY(0)}8%{transform:translateY(-1.5px)}16%,100%{transform:translateY(0)}}
@keyframes vlsFlBob{0%,14%{transform:translate(0,0)}20%{transform:translate(0,-3px)}26%,30%{transform:translate(0,0)}36%{transform:translate(0,-3px)}44%,100%{transform:translate(0,0)}}
@keyframes vlsFlGhost{0%,45%{opacity:.55}50%{opacity:.15}52%{opacity:.5}55%,100%{opacity:0}}
${rmBlock(
  ["vls-fl-c1", "vls-fl-c2", "vls-fl-c3", "vls-fl-c5", "vls-fl-c6", "vls-fl-c7", "vls-fl-you", "vls-fl-ghost"],
  [[".vls-fl-ghost", "opacity:0"]],
)}
${rmBlock(["vls-fl-c4"], [[".vls-fl-c4", "transform:none"]], false)}
`}</style>
      <Frame x={4} w={152} theme={theme} />
      <Frame x={164} w={152} theme={theme} result />
      <path d="M34 78v6m86-6v6M202 78v6m88-6v6" stroke="var(--ink)" strokeWidth={2} strokeLinecap="round" />
      <rect x={26} y={58} width={102} height={19} rx={9} fill="var(--couch)" stroke="none" />
      {crowd.map((color, i) => (
        <Pawn key={i} x={40 + i * 12} yb={67} s={5.5} color={color} eyes className={`vls-fl-c${i + 1}`} />
      ))}
      <g fill="var(--couch-dark)" stroke="none">
        <rect x={26} y={69} width={102} height={10} rx={4} />
        <rect x={22} y={63} width={11} height={16} rx={5} />
        <rect x={121} y={63} width={11} height={16} rx={5} />
        <rect x={196} y={58} width={100} height={19} rx={9} fill="var(--couch)" />
      </g>
      <g className="vls-fl-you">
        <Pawn x={240} yb={67} s={7} eyes />
      </g>
      <g fill="var(--couch-dark)" stroke="none">
        <rect x={196} y={69} width={100} height={10} rx={4} />
        <rect x={192} y={63} width={11} height={16} rx={5} />
        <rect x={289} y={63} width={11} height={16} rx={5} />
      </g>
      <rect
        className="vls-fl-ghost"
        x={268}
        y={40}
        width={22}
        height={16}
        rx={4}
        fill="none"
        stroke="var(--ink)"
        strokeWidth={2}
        strokeDasharray="4 3"
        opacity={0.55}
      />
    </>
  );
}
