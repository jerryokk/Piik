// Panel comics (四宫格漫画): pictorial state strips that carry meaning with no
// human language. Scenes live in ./comic by subject; shared presentation owns
// tone, duration and repetition; settled states and reduced motion share
// informative poses. SSR-safe: pure static markup, no hooks.

import { memo, type CSSProperties, type ReactNode } from "react";
import { comicStyle, getComicPresentation, type ComicMotion, type ComicTone } from "./comic-presentation";
import type { ComicKind, ComicTheme } from "../../visual-kinds";
import * as BasicScenes from "./scenes-status";
import * as RoomScenes from "./scenes-rooms";
import * as BrowserScenes from "./scenes-browser";

export type { ComicKind, ComicTheme };
export * from "./cast";

const DEFAULT_THEME: Record<ComicKind, ComicTheme> = {
  "waiting-for-host": "stage",
  "connecting-p2p": "stage",
  "connecting-sfu": "stage",
  "signal-connecting": "paper",
  "signal-recovering": "paper",
  "signal-offline": "paper",
  "signal-connected": "paper",
  "signal-failed": "paper",
  "media-playing": "stage",
  "media-ready": "paper",
  "share-live": "stage",
  "share-ended": "stage",
  "room-closed": "paper",
  "room-code-invalid": "paper",
  "page-refresh": "paper",
  "site-access": "paper",
  "source-switching": "stage",
  "source-starting": "stage",
  "preview-paused": "stage",
  "host-needs-app": "paper",
  "debug-start": "paper",
  "debug-export-failed": "paper",
  "copy-failed": "paper",
  "source-failed": "stage",
  "settings-failed": "paper",
  "name-invalid": "paper",
  "transport-connected": "paper",
  "tap-to-play": "stage",
  "host-paused": "stage",
  recovering: "stage",
  "route-failed": "stage",
  "playback-failed": "stage",
  "host-offline": "stage",
  "no-audio": "stage",
  "room-not-found": "paper",
  "access-denied": "paper",
  "invalid-invite": "paper",
  "room-full": "paper",
  "bandwidth-limited": "paper",
  "encoder-limited": "paper",
  warning: "paper",
};


/* ------------------------------ component ------------------------------ */
const SCENES: Record<ComicKind, (props: { theme: ComicTheme }) => ReactNode> = {
  "waiting-for-host": BasicScenes.SceneWaiting,
  "connecting-p2p": (p) => <BasicScenes.SceneConnecting {...p} sfu={false} />,
  "connecting-sfu": (p) => <BasicScenes.SceneConnecting {...p} sfu />,
  "signal-connecting": (p) => <BasicScenes.SceneSignal {...p} state="connecting" />,
  "signal-recovering": (p) => <BasicScenes.SceneSignal {...p} state="recovering" />,
  "signal-offline": (p) => <BasicScenes.SceneSignal {...p} state="offline" />,
  "signal-connected": (p) => <BasicScenes.SceneSignal {...p} state="connected" />,
  "transport-connected": (p) => <BasicScenes.SceneSignal {...p} state="connected" peer />,
  "signal-failed": (p) => <BasicScenes.SceneSignal {...p} state="failed" />,
  "media-playing": (p) => <BrowserScenes.SceneMediaStatus {...p} state="playing" />,
  "media-ready": (p) => <BrowserScenes.SceneMediaStatus {...p} state="ready" />,
  "share-live": (p) => <BrowserScenes.SceneMediaStatus {...p} state="sharing" />,
  "share-ended": (p) => <BrowserScenes.SceneMediaStatus {...p} state="ended" />,
  "preview-paused": (p) => <BrowserScenes.SceneMediaStatus {...p} state="preview-paused" />,
  "room-closed": (p) => <BrowserScenes.SceneRoomEntry {...p} state="closed" />,
  "room-code-invalid": (p) => <BrowserScenes.SceneRoomEntry {...p} state="invalid" />,
  "site-access": (p) => <BrowserScenes.SceneRoomEntry {...p} state="password" />,
  "page-refresh": (p) => <BrowserScenes.SceneBrowserAction {...p} action="refresh" />,
  "host-needs-app": BrowserScenes.SceneHostNeedsApp,
  "debug-start": (p) => <BrowserScenes.SceneBrowserAction {...p} action="debug" />,
  "debug-export-failed": (p) => <BrowserScenes.SceneBrowserAction {...p} action="export-failed" />,
  "copy-failed": (p) => <BrowserScenes.SceneBrowserAction {...p} action="copy-failed" />,
  "source-switching": BrowserScenes.SceneSourceSwitching,
  "source-starting": (p) => <BrowserScenes.SceneSourceSwitching {...p} state="starting" />,
  "source-failed": (p) => <BrowserScenes.SceneSourceSwitching {...p} state="failed" />,
  "settings-failed": (p) => <BrowserScenes.SceneBrowserAction {...p} action="settings-failed" />,
  "name-invalid": BrowserScenes.SceneNameInvalid,
  "tap-to-play": BasicScenes.SceneTap,
  "host-paused": BasicScenes.ScenePaused,
  recovering: BasicScenes.SceneRecovering,
  "route-failed": BasicScenes.SceneRouteFailed,
  "playback-failed": BasicScenes.ScenePlaybackFailed,
  "host-offline": BasicScenes.SceneHostOffline,
  "no-audio": BasicScenes.SceneNoAudio,
  "room-not-found": RoomScenes.SceneNotFound,
  "access-denied": RoomScenes.SceneAccessDenied,
  "invalid-invite": RoomScenes.SceneInvalidInvite,
  "room-full": RoomScenes.SceneRoomFull,
  "bandwidth-limited": BrowserScenes.SceneBandwidthLimited,
  "encoder-limited": BrowserScenes.SceneEncoderLimited,
  warning: BrowserScenes.SceneWarning,
};

/**
 * Self-contained pictorial state strip. Renders a 320x96 inline SVG at
 * width min(320px, 86%). The SVG is aria-hidden: wrap it in a role="status"
 * container with a localized aria-label (README §4).
 */
export const Comic = memo(function Comic({
  kind,
  size,
  theme,
  tone,
  motion,
}: {
  kind: ComicKind;
  size?: number;
  theme?: ComicTheme;
  tone?: ComicTone;
  motion?: ComicMotion;
}) {
  const resolvedTheme = theme ?? DEFAULT_THEME[kind];
  const presentation = getComicPresentation(kind);
  const resolvedTone = tone ?? presentation.tone;
  const resolvedMotion = motion ?? presentation.motion;
  const style = {
    ...comicStyle(resolvedTone),
    "--comic-neutral": resolvedTheme === "paper" ? "var(--ink)" : "#48597a",
    width: size != null ? `${size}px` : "min(320px, 86%)",
    height: "auto",
    display: "block",
  } as CSSProperties;
  return (
    <svg
      key={kind}
      viewBox="0 0 320 96"
      style={style}
      data-comic-tone={resolvedTone}
      data-comic-motion={resolvedMotion}
      aria-hidden="true"
      focusable="false"
      xmlns="http://www.w3.org/2000/svg"
    >
      <g className="lr-comic-content">{SCENES[kind]({ theme: resolvedTheme })}</g>
    </svg>
  );
});
