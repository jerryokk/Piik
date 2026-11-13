import { useLayoutEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useSurfaceDocument } from "../../panel-window";
import { useCopy } from "../../copy";
import { Btn } from "../shell/primitives";
import { inviteCode, inviteCodePath } from "../../invite-code";
import { useAnchoredPopover } from "../shell/use-anchored-popover";

// The invitation code is a reading surface, not another copy action: the row
// already owns copying and rotating the link. Visibility is explicit rather
// than `popover="auto"`: the trigger sits inside the same scrolled control row,
// and an auto popover dismisses on the very click that opened it. Escape and
// an outside press close it, and focus returns to the trigger.
export function InviteCodeButton({ link, panelId, disabled }: {
  link: string | null;
  panelId: string;
  disabled: boolean;
}) {
  const { t } = useCopy();
  // The panel is portalled out of the scrolling control row: a popover there
  // inherits that row's clipping and stacking, and the row's own pointer
  // handling dismisses it on the click that opened it.
  const ownerDocument = useSurfaceDocument();
  const trigger = useRef<HTMLButtonElement>(null);
  const panel = useRef<HTMLDivElement>(null);
  useAnchoredPopover(trigger, panel, { side: "below", align: "end" });
  const code = useMemo(() => (link ? inviteCode(link) : null), [link]);
  const path = useMemo(() => (code ? inviteCodePath(code) : ""), [code]);
  const [isOpen, setIsOpen] = useState(false);
  const open = () => {
    if (!disabled && code && panel.current && !panel.current.matches(":popover-open")) {
      panel.current.showPopover();
      setIsOpen(true);
    }
  };
  const close = (restoreFocus: boolean) => {
    if (panel.current?.matches(":popover-open")) panel.current.hidePopover();
    setIsOpen(false);
    if (restoreFocus) trigger.current?.querySelector("button")?.focus();
  };
  useLayoutEffect(() => {
    const element = panel.current;
    return () => { if (element?.matches(":popover-open")) element.hidePopover(); };
  }, []);
  useLayoutEffect(() => { if (disabled || !code) close(false); }, [disabled, code]);
  return (
    <>
      {/* The shared control keeps the row's geometry and hit area; the anchor
          is this wrapper so the panel follows the whole button. */}
      <span ref={trigger} className="lr-invite-code-trigger">
        <Btn
          icon="grid"
          quiet
          title="host.invite.code"
          controls={panelId}
          expanded={isOpen}
          disabled={disabled || !code}
          onClick={() => {
            if (panel.current?.matches(":popover-open")) close(false);
            else open();
          }}
        />
      </span>
      {createPortal(<div
        ref={panel}
        id={panelId}
        popover="manual"
        role="dialog"
        aria-label={t("host.invite.code")}
        className="lr-floating-surface lr-invite-code"
        onKeyDown={(event) => {
          if (event.key === "Escape") { event.preventDefault(); close(true); }
        }}
        onToggle={(event) => { if (event.newState === "closed") { setIsOpen(false); close(true); } }}
        // A press elsewhere closes the panel without stealing its target's click.
        onPointerDownCapture={(event) => {
          const target = event.target as Node;
          if (!panel.current?.contains(target) && target !== trigger.current) close(false);
        }}
      >
        <b>{t("host.invite.code")}</b>
        {code ? (
          <>
            <svg
              className="lr-invite-code-image"
              viewBox={`0 0 ${code.size} ${code.size}`}
              role="img"
              aria-label={t("host.invite.codeImage")}
              shapeRendering="crispEdges"
            >
              <path d={path} fill="currentColor" />
            </svg>
            <small>{t("host.invite.codeHint")}</small>
          </>
        ) : (
          <small>{t("host.invite.codeUnavailable")}</small>
        )}
      </div>, ownerDocument?.body ?? document.body)}
    </>
  );
}
