// Panel-comic cast and idiom helpers. Keep animated transform classes on an
// outer wrapper so CSS motion cannot replace an inner positioning transform.

import type { CSSProperties, ReactNode } from "react";
import { Glyph } from "../../icons";
import { PersonShape } from "../people/Pawn";
import type { ComicTheme } from "../../visual-kinds";

/* Control hints reuse these scene objects and stable identity colours. */
export const YOU = "var(--you)";
export const LIVE = "var(--live)";
export const WARN = "var(--warn)";
export const DANGER = "var(--danger)";
export const INK_STAGE = "#dfe8f2";
export const MINT = "#9de8bf";
export const SKY = "#7ea4f5";
export const LINE = "#8ea3b8";
export const FAINT = "#7789a8";
export const TV_BODY = "#22303e";
export const TV_EDGE = "#57697f";
export const TV_SCREEN = "#16233c";
export const STAR_GOLD = "#efb23f";

const r2 = (n: number) => Math.round(n * 100) / 100;

/** Panel frame: rounded rect, rx 14 at >100px wide, stroke 2.5. */
export function Frame({
  x,
  w,
  theme,
  result = false,
}: {
  x: number;
  w: number;
  theme: ComicTheme;
  result?: boolean;
}) {
  const paper = theme === "paper";
  const border = paper ? "var(--ink)" : "#48597a";
  return (
    <rect
      x={x}
      y={4}
      width={w}
      height={88}
      rx={w > 100 ? 14 : 12}
      fill={paper ? "var(--paper)" : "#0d1526"}
      stroke={result ? `var(--comic-tone, ${border})` : border}
      strokeWidth={2.5}
    />
  );
}

/** Floor/horizon line characters stand on (stage scenes). */
export function Floor({ x1, x2, y = 76 }: { x1: number; x2: number; y?: number }) {
  return (
    <path
      d={`M${x1} ${y} H${x2}`}
      stroke="#48597a"
      strokeWidth={2.5}
      strokeLinecap="round"
    />
  );
}

/** Mini participant, feet centered on (x, yb); s is the head diameter. */
export function Pawn({
  x,
  yb,
  s,
  color = YOU,
  eyes = false,
  host,
  gaze,
  eyeClassName,
  className,
}: {
  x: number;
  yb: number;
  s: number;
  color?: string;
  eyes?: boolean | "closed";
  host?: boolean;
  gaze?: number;
  eyeClassName?: string;
  className?: string;
}) {
  const scale = s / 16;
  const inner = <g transform={`translate(${r2(x - 24 * scale)} ${r2(yb - 50 * scale)}) scale(${scale})`} fill={color}
    style={{ "--comic-blink-delay": `${Math.round(x * .8)}ms` } as CSSProperties}>
    <PersonShape eyes={eyes} gaze={gaze} eyeClassName={eyeClassName} host={host} />
  </g>;
  return className ? <g className={className}>{inner}</g> : inner;
}

/** Shared or watched media: the same TV silhouette in every scene. */
export function MiniTv({ x, y, w, h }: { x: number; y: number; w: number; h: number }) {
  const antenna = Math.min(5, h * 0.16);
  const foot = Math.min(4, h * 0.12);
  return (
    <>
      <path
        d={`M${r2(x + w * 0.38)} ${r2(y - antenna)} L${r2(x + w * 0.5)} ${y} L${r2(x + w * 0.62)} ${r2(y - antenna)} M${r2(x + w * 0.25)} ${y + h} l${-foot} ${foot} M${r2(x + w * 0.75)} ${y + h} l${foot} ${foot}`}
        fill="none"
        stroke={TV_EDGE}
        strokeWidth={2}
        strokeLinecap="round"
        strokeLinejoin="round"
      />
      <rect
        x={x}
        y={y}
        width={w}
        height={h}
        rx={Math.min(8, h * 0.2)}
        fill={TV_BODY}
        stroke={TV_EDGE}
        strokeWidth={2}
      />
      <rect
        x={r2(x + w * 0.07)}
        y={r2(y + h * 0.11)}
        width={r2(w * 0.86)}
        height={r2(h * 0.66)}
        rx={Math.min(4, h * 0.1)}
        fill={TV_SCREEN}
      />
    </>
  );
}

/** Browser UI, not the media itself; children use the scene's coordinates. */
export function BrowserWindow({ x, y, w, h, children }: {
  x: number; y: number; w: number; h: number; children?: ReactNode;
}) {
  const bar = Math.min(9, h * 0.2);
  return <>
    <rect x={x} y={y} width={w} height={h} rx={4} fill="var(--paper)" stroke="var(--ink)" strokeWidth={2} />
    <path d={`M${x} ${y + bar} H${x + w}`} stroke="var(--ink)" strokeWidth={1.5} />
    <g fill={FAINT}>
      <circle cx={x + 4} cy={y + bar / 2} r={1} />
      <circle cx={x + 8} cy={y + bar / 2} r={1} />
      <rect x={x + w * 0.3} y={y + bar / 2 - 1} width={w * 0.5} height={2} rx={1} />
    </g>
    {children}
  </>;
}

/** Invitation URL: the same 24-unit chain as the link control icon. */
export function InviteLink({ x, y, size = 24, className }: {
  x: number; y: number; size?: number; className?: string;
}) {
  return <g className={className}>
    <g transform={`translate(${x} ${y})`} color="var(--ink)">
      <Glyph name="link" size={size} />
    </g>
  </g>;
}

/** Crescent moon (host not live / room asleep). Static; safe to transform. */
export function Moon({
  x,
  y,
  k = 1,
}: {
  x: number;
  y: number;
  k?: number;
}) {
  return (
    <g transform={`translate(${r2(x)} ${r2(y)}) scale(${k})`}>
      <path
        d="M0 -8.5A8.5 8.5 0 1 0 0 8.5A6.2 8.5 0 0 1 0 -8.5Z"
        fill="#f2e9c9"
      />
    </g>
  );
}

/** SFU relay server box; two slot lines light --live as the dot passes. */
export function ServerBox({
  x,
  y,
  w = 24,
  h = 16,
  lit = false,
  slotsClass,
}: {
  x: number;
  y: number;
  w?: number;
  h?: number;
  lit?: boolean;
  slotsClass?: string;
}) {
  const inset = w * 0.17;
  const slots = (
    <>
      <path d={`M${r2(x + inset)} ${r2(y + h * 0.31)}h${r2(w - inset * 2)}`} />
      <path d={`M${r2(x + inset)} ${r2(y + h * 0.63)}h${r2(w - inset * 2)}`} />
    </>
  );
  return (
    <>
      <rect
        x={x}
        y={y}
        width={w}
        height={h}
        rx={2.5}
        fill="#0d1526"
        stroke={lit ? LIVE : LINE}
        strokeWidth={2}
      />
      {slotsClass ? (
        <g className={slotsClass} stroke={LIVE} strokeWidth={2} strokeLinecap="round">
          {slots}
        </g>
      ) : (
        <g stroke={LIVE} strokeWidth={2} strokeLinecap="round">
          {slots}
        </g>
      )}
    </>
  );
}

/** Join-page door (paper rooms). */
export function Door({ x, y, className }: { x: number; y: number; className?: string }) {
  const inner = (
    <>
      <rect
        x={x}
        y={y}
        width={32}
        height={54}
        rx={5}
        fill="var(--wall-2)"
        stroke="var(--ink)"
        strokeWidth={2.5}
      />
      <circle cx={x + 25} cy={y + 28} r={2} fill="var(--ink)" />
    </>
  );
  return className ? <g className={className}>{inner}</g> : inner;
}

/** Tiny spark at a route break: three rays. */
export function Spark({ x, y, className }: { x: number; y: number; className?: string }) {
  return (
    <g
      className={className}
      stroke={STAR_GOLD}
      strokeWidth={2}
      strokeLinecap="round"
      fill="none"
    >
      <path d={`M${x} ${y - 6}v-5 M${x - 11} ${y - 2}l-4 -3 M${x + 11} ${y - 2}l4 -3`} />
    </g>
  );
}

/** Four-point success star; the containing scene owns its motion. */
export function Star({
  x,
  y,
  r = 6,
  className,
  baseOpacity,
}: {
  x: number;
  y: number;
  r?: number;
  className?: string;
  baseOpacity?: number;
}) {
  const a = r / 3;
  const d = `M${x} ${y - r} l${a} ${r - a} ${r - a} ${a} -${r - a} ${a} -${a} ${r - a} -${a} -${r - a} -${r - a} -${a} ${r - a} -${a} Z`;
  return <path className={className} opacity={baseOpacity} d={d} fill={STAR_GOLD} />;
}

/** Unavailable verdict mark; keep it readable while the subject moves. */
export function RedX({
  cx,
  cy,
  arm = 6,
  className,
}: {
  cx: number;
  cy: number;
  arm?: number;
  className?: string;
}) {
  return (
    <path
      className={className}
      d={`M${cx - arm} ${cy - arm} L${cx + arm} ${cy + arm} M${cx + arm} ${cy - arm} L${cx - arm} ${cy + arm}`}
      stroke={`var(--comic-tone, ${DANGER})`}
      strokeWidth={3}
      strokeLinecap="round"
      fill="none"
    />
  );
}

/** Power plug, prongs pointing down, cord trailing up-right. */
export function Plug({ x, y, className }: { x: number; y: number; className?: string }) {
  const inner = (
    <>
      <path
        d={`M${x} ${y} c0-8 6-10 12-12`}
        stroke={TV_EDGE}
        strokeWidth={2.5}
        fill="none"
        strokeLinecap="round"
      />
      <rect x={x - 14} y={y} width={28} height={14} rx={4} fill={TV_BODY} stroke={TV_EDGE} strokeWidth={2} />
      <path
        d={`M${x - 4} ${y + 14}v6 M${x + 4} ${y + 14}v6`}
        stroke={LINE}
        strokeWidth={2.5}
        strokeLinecap="round"
      />
    </>
  );
  return className ? <g className={className}>{inner}</g> : inner;
}

/** Wall socket the plug seats into. */
export function Socket({ x, y }: { x: number; y: number }) {
  return (
    <>
      <rect x={x} y={y} width={20} height={15} rx={3} fill={TV_BODY} stroke={TV_EDGE} strokeWidth={2} />
      <path
        d={`M${x + 6} ${y + 5}v6 M${x + 14} ${y + 5}v6`}
        stroke="#101a2c"
        strokeWidth={2}
        strokeLinecap="round"
      />
    </>
  );
}

/** TV-static speckles; positions hand-scattered inside the given screen. */
export function Static({
  pts,
  cls,
}: {
  pts: ReadonlyArray<readonly [number, number]>;
  cls: string;
}) {
  return (
    <g className={cls} fill="#7d92a8">
      {pts.map(([px, py], i) => (
        <rect key={i} x={px} y={py} width={2.5} height={2.5} rx={0.5} />
      ))}
    </g>
  );
}

/** Hold operation results in settled scenes; decorative subject motion may opt
 * out. System and preview reduced motion always use the explicit final pose. */
export function rmBlock(kills: string[], pins: Array<readonly [string, string]>, holdOnStill = true): string {
  const rules = [...kills.map((c) => [`.${c}`, "animation:none"] as const), ...pins];
  const block = (scope: string) => rules.map(([selectors, styles]) =>
    `${selectors.split(",").map((selector) => scope + selector.trim()).join(",")}{${styles}}`,
  ).join("");
  return `${holdOnStill ? block('svg[data-comic-motion="still"] ') : ""}${block('[data-comic-reduced-motion] ')}@media (prefers-reduced-motion:reduce){${block("")}}`;
}
