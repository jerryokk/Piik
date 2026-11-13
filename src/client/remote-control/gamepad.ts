import type { RemoteInputEvent } from "./input-session";
import type { RemoteGamepadReport } from "./protocol";

// Standard Gamepad layout. Platform backends own native device formats.
// https://www.w3.org/TR/gamepad/#remapping
export function gamepadReport(pad: Pick<Gamepad, "buttons" | "axes">): RemoteGamepadReport {
  let buttons = 0;
  for (let index = 0; index < 17; index++) {
    if (pad.buttons[index]?.pressed) buttons |= 1 << index;
  }
  const axis = (index: number) => {
    const value = Math.max(-1, Math.min(1, pad.axes[index] ?? 0));
    return Math.round(value * (value < 0 ? 32768 : 32767));
  };
  const trigger = (index: number) => Math.round(Math.max(0, Math.min(1, pad.buttons[index]?.value ?? 0)) * 255);
  return {
    buttons,
    leftTrigger: trigger(6), rightTrigger: trigger(7),
    leftX: axis(0), leftY: axis(1), rightX: axis(2), rightY: axis(3),
  };
}

// One collector belongs to one active grant. Focus/visibility release the current
// report, while disposing the grant also retires its animation frame and listeners.
// Every connected standard controller is collected: the Browser's gamepad index
// is the address each report carries, so a Viewer can bring several.
export class RemoteGamepad {
  private frame: number | null = null;
  private nextReport = 0;
  /** Indices currently reported to the Host. */
  private readonly known = new Set<number>();
  private connected = false;
  private disposed = false;
  /** Indices the Host refused for lack of a free slot. They keep polling so a
   *  controller is ready the moment room appears, but send nothing: reports
   *  for a refused index would only spin. Other controllers are unaffected. */
  private readonly refused = new Set<number>();

  constructor(private readonly input: (event: RemoteInputEvent) => void,
    private readonly changed: (connected: boolean) => void,
    private readonly owner: Window = window,
    private readonly page: Document = document) {
    owner.addEventListener("blur", this.pause);
    owner.addEventListener("focus", this.resume);
    owner.addEventListener("gamepaddisconnected", this.disconnected);
    page.addEventListener("visibilitychange", this.visibility);
    this.resume();
  }

  /** The Host named the indices it could not create. Only those stop; a Viewer
   *  whose third controller found no slot keeps the other two working. */
  refuse(indexes: readonly number[]): void {
    for (const index of indexes) {
      if (this.refused.has(index)) continue;
      this.refused.add(index);
      // The Host already retired this index, so it owns no device there. A
      // stop would read as an unplug and clear the refusal it just wrote.
      this.known.delete(index);
    }
  }

  /** Room reappeared. Retrying is explicit, so a controller never starts
   *  acting on a Viewer who has walked away from it. */
  retry(): void {
    if (!this.refused.size) return;
    this.refused.clear();
    this.nextReport = 0;
    this.input({ type: "gamepad-retry" });
    this.resume();
  }

  private setConnected(value: boolean): void {
    if (this.connected !== value) { this.connected = value; this.changed(value); }
  }
  /** Tells the Host to retire every controller this collector owns. */
  private stopAll(): void {
    for (const index of this.known) this.input({ type: "gamepad-stop", pad: index });
    this.known.clear();
  }
  private readonly pause = (): void => {
    if (this.frame !== null) this.owner.cancelAnimationFrame(this.frame);
    this.frame = null; this.nextReport = 0;
    this.input({ type: "gamepad-reset" });
    this.setConnected(false);
  };
  private readonly resume = (): void => {
    if (!this.disposed && !this.page.hidden && this.page.hasFocus() && this.frame === null)
      this.frame = this.owner.requestAnimationFrame(this.poll);
  };
  private readonly visibility = (): void => { if (this.page.hidden) this.pause(); else this.resume(); };
  private readonly disconnected = (event: GamepadEvent): void => {
    const index = event.gamepad.index;
    // A different controller on the same index is a new device, so it may ask
    // again rather than inheriting the refusal of the one that left. The Host
    // needs the stop either way, or it would keep the index refused forever.
    const refused = this.refused.delete(index);
    if (!this.known.delete(index) && !refused) return;
    this.input({ type: "gamepad-stop", pad: index });
    if (!this.known.size) this.setConnected(false);
  };
  private readonly poll = (time: number): void => {
    this.frame = null;
    if (this.disposed || this.page.hidden || !this.page.hasFocus()) { this.pause(); return; }
    let pads: Gamepad[] = [];
    try {
      pads = Array.from(this.owner.navigator.getGamepads())
        .filter((value): value is Gamepad => Boolean(value?.connected && value.mapping === "standard"));
    } catch { /* Browser permissions can deny controller access. Keep the grant neutral. */ }
    const present = new Set(pads.map(pad => pad.index));
    // A pad that vanished without a disconnect event still stops, and its
    // refusal ends with it: the index may carry a different controller next.
    for (const index of [...this.known]) {
      if (!present.has(index)) { this.known.delete(index); this.input({ type: "gamepad-stop", pad: index }); }
    }
    for (const index of [...this.refused]) {
      if (!present.has(index)) {
        this.refused.delete(index);
        // Tell the Host too: it holds the refusal, and would otherwise keep
        // the index refused if a different controller arrives there.
        this.input({ type: "gamepad-stop", pad: index });
      }
    }
    this.setConnected(pads.length > 0);
    if (pads.length && time >= this.nextReport) {
      const interval = 1000 / 60;
      this.nextReport = time + interval - ((time - this.nextReport) % interval);
      // Repeat the complete current report so the native driver can enumerate
      // a newly attached virtual device even when the player holds still.
      for (const pad of pads) {
        if (this.refused.has(pad.index)) continue;
        this.known.add(pad.index);
        this.input({ type: "gamepad", pad: pad.index, gamepad: gamepadReport(pad) });
      }
    }
    this.resume();
  };

  dispose(): void {
    this.disposed = true;
    if (this.frame !== null) this.owner.cancelAnimationFrame(this.frame);
    this.frame = null;
    this.nextReport = 0;
    // Retire every device: this grant is over, so the Host must not keep pads
    // nobody can drive. Ordering matters — the last thing it hears is each
    // controller's stop, not a neutral report.
    this.stopAll();
    this.setConnected(false);
    this.owner.removeEventListener("blur", this.pause);
    this.owner.removeEventListener("focus", this.resume);
    this.owner.removeEventListener("gamepaddisconnected", this.disconnected);
    this.page.removeEventListener("visibilitychange", this.visibility);
  }
}
