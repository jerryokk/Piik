// Route hints: how a Viewer's media reaches the Host — automatic choice,
// direct peer path, server forwarding, and local-versus-site entry. Scenes
// follow ../Comic.tsx conventions: 320x96 canvas, 2-panel before→after idiom
// (Frame x={4} w={152} + Frame x={164} w={152}), vls-prefixed keyframes in an
// inline <style>, rmBlock for reduced motion.
import {
  FAINT,
  Frame,
  Pawn,
  RedX,
  SKY,
  ServerBox,
  rmBlock,
} from "../comic/Comic";
import type { HintScene, RouteHintKind } from "../../visual-kinds";

const INK = "var(--ink)";

const HintRouteAuto: HintScene = ({ theme }) => <>
  <style>{`
.vls-auto-direct{animation:vlsAutoDirect var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
.vls-auto-forward{animation:vlsAutoForward var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
@keyframes vlsAutoDirect{0%,12%,62%,100%{opacity:0}26%,48%{opacity:1}}
@keyframes vlsAutoForward{0%,56%{opacity:0}74%,100%{opacity:1}}
${rmBlock(["vls-auto-direct", "vls-auto-forward"], [[".vls-auto-direct", "opacity:1"], [".vls-auto-forward", "opacity:0"]])}
  `}</style>
  {[0, 160].map((x, index) => <g key={x}>
    <Frame x={x + 4} w={152} theme={theme} result={Boolean(index)} />
    <path d={`M${x + 38} 59h84M${x + 38} 55l30-25m24 0 30 25`} fill="none"
      stroke={FAINT} strokeWidth={2} strokeDasharray="3 4" />
    <ServerBox x={x + 66} y={17} w={28} h={24} />
    {index === 1 && <>
      <path className="vls-auto-direct" d={`M${x + 38} 59h84`} fill="none" stroke={INK} strokeWidth={3} strokeLinecap="round" />
      <path className="vls-auto-forward" d={`M${x + 38} 55l30-25m24 0 30 25`} fill="none" stroke={INK} strokeWidth={3} strokeLinecap="round" />
    </>}
    <Pawn x={x + 32} yb={80} s={10} eyes host gaze={1} />
    <Pawn x={x + 128} yb={80} s={10} color={SKY} eyes gaze={-1} />
  </g>)}
</>;

/* hint-route-p2p: [two pawns apart] → [direct neutral arc pawn→pawn,
   route dot on the apex]. Demo: arc draws 6-28%, both pawns hop 30-42%,
   panel-1 pair blinks at 64%; rest ~58%. */
const HintRouteP2p: HintScene = ({ theme }) => (
  <>
    <style>{`
.vls-p2p-eyes{transform-box:fill-box;transform-origin:center;animation:vlsP2pBlink var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
.vls-p2p-arc{stroke-dasharray:1;animation:vlsP2pArc var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
.vls-p2p-hop{animation:vlsP2pHop var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
@keyframes vlsP2pBlink{0%,60%,68%,100%{transform:scaleY(1)}64%{transform:scaleY(.12)}}
@keyframes vlsP2pArc{0%,6%{stroke-dashoffset:1}28%,100%{stroke-dashoffset:0}}
@keyframes vlsP2pHop{0%,30%{transform:translateY(0)}36%{transform:translateY(-4px)}42%,100%{transform:translateY(0)}}
${rmBlock(
  ["vls-p2p-eyes", "vls-p2p-arc", "vls-p2p-hop"],
  [
    [".vls-p2p-arc", "stroke-dashoffset:0"],
    [".vls-p2p-eyes,.vls-p2p-hop", "transform:none"],
  ],
)}
`}</style>
    <Frame x={4} w={152} theme={theme} />
    <Frame x={164} w={152} theme={theme} result />
    <Pawn x={42} yb={74} s={9} eyes gaze={2} eyeClassName="vls-p2p-eyes" />
    <Pawn x={118} yb={74} s={9} color={SKY} eyes gaze={-2} eyeClassName="vls-p2p-eyes" />
    <g fill={FAINT}>
      <circle cx={74} cy={60} r={1.5} />
      <circle cx={88} cy={60} r={1.5} />
    </g>
    <g className="vls-p2p-hop">
      <Pawn x={202} yb={74} s={9} eyes />
      <Pawn x={278} yb={74} s={9} color={SKY} eyes />
    </g>
    <path
      className="vls-p2p-arc"
      d="M213 58 Q240 30 267 58"
      pathLength={1}
      stroke={INK}
      strokeWidth={3}
      strokeLinecap="round"
      fill="none"
    />
    <circle cx={240} cy={44} r={3.5} fill={INK} />
  </>
);

// Unavailable server forwarding still leaves the ordinary direct route.
const HintRouteP2pRequired: HintScene = ({ theme }) => (
  <>
    <HintRouteP2p theme={theme} />
    <style>{`
.vls-p2p-unavailable{animation:vlsP2pUnavailable var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
@keyframes vlsP2pUnavailable{0%,8%,36%,100%{transform:none}16%{transform:translateX(-4px)}26%{transform:translateX(3px)}}
${rmBlock(["vls-p2p-unavailable"], [[".vls-p2p-unavailable", "transform:none"]], false)}
`}</style>
    <g className="vls-p2p-unavailable">
      <ServerBox x={64} y={16} w={32} h={24} />
      <RedX cx={80} cy={28} arm={7} />
    </g>
  </>
);

/* hint-route-sfu: [two pawns apart] → [two neutral arcs detour up
   through a server box]. Demo: left arc 6-24%, right arc 14-32%,
   both pawns hop 34-46%, panel-1 pair blinks at 64%; rest ~54%. */
const HintRouteSfu: HintScene = ({ theme }) => (
  <>
    <style>{`
.vls-sfu-eyes{transform-box:fill-box;transform-origin:center;animation:vlsSfuBlink var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
.vls-sfu-a1{stroke-dasharray:1;animation:vlsSfuA1 var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
.vls-sfu-a2{stroke-dasharray:1;animation:vlsSfuA2 var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
.vls-sfu-hop{animation:vlsSfuHop var(--comic-duration,3.2s) ease-in-out var(--comic-repeat,1) both}
@keyframes vlsSfuBlink{0%,60%,68%,100%{transform:scaleY(1)}64%{transform:scaleY(.12)}}
@keyframes vlsSfuA1{0%,6%{stroke-dashoffset:1}24%,100%{stroke-dashoffset:0}}
@keyframes vlsSfuA2{0%,14%{stroke-dashoffset:1}32%,100%{stroke-dashoffset:0}}
@keyframes vlsSfuHop{0%,34%{transform:translateY(0)}40%{transform:translateY(-4px)}46%,100%{transform:translateY(0)}}
${rmBlock(
  ["vls-sfu-eyes", "vls-sfu-a1", "vls-sfu-a2", "vls-sfu-hop"],
  [
    [".vls-sfu-a1,.vls-sfu-a2", "stroke-dashoffset:0"],
    [".vls-sfu-eyes,.vls-sfu-hop", "transform:none"],
  ],
)}
`}</style>
    <Frame x={4} w={152} theme={theme} />
    <Frame x={164} w={152} theme={theme} result />
    <Pawn x={42} yb={74} s={9} eyes gaze={2} eyeClassName="vls-sfu-eyes" />
    <Pawn x={118} yb={74} s={9} color={SKY} eyes gaze={-2} eyeClassName="vls-sfu-eyes" />
    <g fill={FAINT}>
      <circle cx={74} cy={60} r={1.5} />
      <circle cx={88} cy={60} r={1.5} />
    </g>
    <ServerBox x={228} y={12} />
    <g className="vls-sfu-hop">
      <Pawn x={200} yb={76} s={9} eyes />
      <Pawn x={280} yb={76} s={9} color={SKY} eyes />
    </g>
    <path
      className="vls-sfu-a1"
      d="M209 60 Q219 34 232 29"
      pathLength={1}
      stroke={INK}
      strokeWidth={3}
      strokeLinecap="round"
      fill="none"
    />
    <path
      className="vls-sfu-a2"
      d="M271 60 Q261 34 248 29"
      pathLength={1}
      stroke={INK}
      strokeWidth={3}
      strokeLinecap="round"
      fill="none"
    />
  </>
);

// A site requiring server forwarding excludes the direct peer path.
const HintRouteSfuRequired: HintScene = ({ theme }) => <>
  <HintRouteSfu theme={theme} />
  <path d="M58 58h44" fill="none" stroke={FAINT} strokeWidth={2} strokeDasharray="3 3" />
  <RedX cx={80} cy={58} arm={7} />
</>;

// The home mark distinguishes local sharing from a public direct route.
const HintClientLocal: HintScene = ({ theme }) => (
  <>
    <HintRouteP2p theme={theme} />
    <g fill="none" stroke={INK} strokeWidth={1.8} strokeLinecap="round" strokeLinejoin="round">
      <path d="M173 20 180 14 187 20v7h-14Z" />
      <path d="M178 27v-5h4v5" />
    </g>
  </>
);

export const ROUTE_SCENES: Record<RouteHintKind, HintScene> = {
  "hint-route-auto": HintRouteAuto,
  "hint-route-p2p": HintRouteP2p,
  "hint-route-p2p-required": HintRouteP2pRequired,
  "hint-route-sfu": HintRouteSfu,
  "hint-route-sfu-required": HintRouteSfuRequired,
  "hint-client-local": HintClientLocal,
};
