import { useLayoutEffect, useRef, type RefObject } from "react";
import { useSurfaceDocument } from "../../panel-window";

// Native popovers own visibility, dismissal and focus. This only places a
// menu beside its anchor, within the document displaying it.
export function useAnchoredPopover(
  trigger: RefObject<HTMLElement | null>, panel: RefObject<HTMLElement | null>,
  { side = "below", align = "end", maxHeight = Infinity, minVisibleHeight = Infinity, enabled = true }: {
    side?: "above" | "below"; align?: "end" | "center";
    maxHeight?: number; minVisibleHeight?: number; enabled?: boolean;
  } = {},
) {
  const ownerDocument = useSurfaceDocument();
  const reposition = useRef<(() => void) | null>(null);
  // A React layout change can move the anchor without resizing it.
  useLayoutEffect(() => {
    if (panel.current?.matches(":popover-open")) reposition.current?.();
  });
  useLayoutEffect(() => {
    const menu = panel.current;
    if (!enabled || !menu) return;
    const view = menu.ownerDocument.defaultView;
    if (!view) return;
    const viewport = view.visualViewport;
    const place = () => {
      // The anchor may belong to a parent whose ref is attached after this
      // child's layout effect. Read it when opening, not when subscribing.
      const anchor = trigger.current;
      if (!anchor || !menu.matches(":popover-open")) return;
      const rect = anchor.getBoundingClientRect();
      const left = (viewport?.offsetLeft ?? 0) + 8, top = (viewport?.offsetTop ?? 0) + 8;
      const right = left + (viewport?.width ?? view.innerWidth) - 16;
      const bottom = top + (viewport?.height ?? view.innerHeight) - 16;
      // A detached trigger is gone for good: close with it. A trigger merely
      // scrolled out of view keeps its menu: the panel is still on screen and
      // the person may be reading it, so closing here (as this used to) cut
      // them off mid-scroll on a phone. The anchors below are clamped into the
      // viewport, which also keeps an editing surface placed while a keyboard
      // covers its trigger.
      if (!anchor.getClientRects().length) {
        menu.hidePopover();
        return;
      }
      const anchorTop = Math.min(Math.max(rect.top, top), bottom);
      const anchorBottom = Math.min(Math.max(rect.bottom, top), bottom);
      const anchorLeft = Math.min(Math.max(rect.left, left), right);
      const above = Math.max(0, anchorTop - top - 8), below = Math.max(0, bottom - anchorBottom - 8);
      // Measure against the viewport, not the previous side's constrained size.
      menu.style.maxWidth = `${right - left}px`;
      menu.style.maxHeight = `${Math.min(maxHeight, bottom - top)}px`;
      const height = Math.min(menu.scrollHeight, maxHeight);
      // Scrollable palettes can prefer their requested side without requiring
      // every row to fit; compact menus retain the full-content threshold.
      const preferredHeight = Math.min(height, minVisibleHeight);
      const openAbove = side === "above" ? above >= preferredHeight || above > below : below < preferredHeight && above > below;
      const beside = openAbove ? above : below;
      menu.style.maxHeight = `${Math.min(maxHeight, beside >= preferredHeight ? beside : bottom - top)}px`;
      menu.classList.toggle("is-above", openAbove);
      const x = align === "center" ? anchorLeft + (rect.width - menu.offsetWidth) / 2 : rect.right - menu.offsetWidth;
      menu.style.left = `${Math.max(left, Math.min(x, right - menu.offsetWidth))}px`;
      const y = openAbove ? anchorTop - menu.offsetHeight - 8 : anchorBottom + 8;
      menu.style.top = `${Math.max(top, Math.min(y, bottom - menu.offsetHeight))}px`;
    };
    // The containing panel may reposition itself in the same resize. Measure
    // after its layout owner has finished, including content/translation changes.
    let frame = 0;
    const schedule = () => {
      view.cancelAnimationFrame(frame);
      frame = view.requestAnimationFrame(place);
    };
    reposition.current = schedule;
    const observer = new ResizeObserver(schedule);
    const toggled = () => {
      observer.disconnect();
      view.cancelAnimationFrame(frame);
      if (!menu.matches(":popover-open")) return;
      place(); observer.observe(menu);
      const anchor = trigger.current;
      if (anchor) observer.observe(anchor);
      const parentPopover = anchor?.closest("[popover]");
      if (parentPopover) observer.observe(parentPopover);
    };
    // `scroll` reaches here from captured scrolling anywhere in the document,
    // including the menu's own body. Repositioning on the latter re-measures a
    // panel the user is already scrolling and can flip its side mid-gesture, so
    // a scroll that starts inside the menu is left alone.
    const onScroll = (event: Event) => {
      if (event.target instanceof Node && menu.contains(event.target)) return;
      schedule();
    };
    // `toggle` is queued by the browser; position before the first painted frame.
    menu.addEventListener("beforetoggle", schedule);
    menu.addEventListener("toggle", toggled);
    view.addEventListener("resize", schedule);
    view.addEventListener("scroll", onScroll, true);
    viewport?.addEventListener("resize", schedule);
    viewport?.addEventListener("scroll", onScroll);
    toggled();
    return () => {
      reposition.current = null;
      observer.disconnect();
      view.cancelAnimationFrame(frame);
      menu.removeEventListener("beforetoggle", schedule);
      menu.removeEventListener("toggle", toggled);
      view.removeEventListener("resize", schedule);
      view.removeEventListener("scroll", onScroll, true);
      viewport?.removeEventListener("resize", schedule);
      viewport?.removeEventListener("scroll", onScroll);
    };
  }, [trigger, panel, side, align, maxHeight, minVisibleHeight, enabled, ownerDocument]);
}
