import { describe, expect, it, vi } from "vitest";
import { fixture, useRealResourceDisposal, ref, deferred } from "./support.ts";

describe("Native sharing pause ownership", () => {
  function pauseFixture(paused = true) {
    const current = fixture();
    useRealResourceDisposal(current);
    current.nativeModeRef.current = true;
    const setPaused = vi.fn(async (_share: string, _paused: boolean) => {});
    Object.assign(current.client, { setPaused });
    const signal = { setSharingPaused: vi.fn(() => true), confirmSharingPaused: vi.fn(), setCoPlayGrants: vi.fn(), stop: vi.fn() };
    Object.assign(current.context, {
      sharingPausedRef: ref(paused), setSharingPaused: vi.fn(), setMediaPaused: vi.fn(() => true),
      signalRef: ref(signal),
    });
    return { ...current, setPaused, signal };
  }

  it.each([false, true])("commits the current Native pause change from paused=%s", async paused => {
    const current = pauseFixture(paused);
    current.context.toggleSharingPause();
    await vi.waitFor(() => expect(current.context.setSharingPaused).toHaveBeenCalledWith(!paused));
    expect(current.setPaused).toHaveBeenCalledExactlyOnceWith("share", !paused);
    expect(current.signal.setSharingPaused).toHaveBeenCalledExactlyOnceWith(!paused);
    expect(current.context.sharingPausedRef.current).toBe(!paused);
  });

  it.each(["success", "failure", "retired"])("ignores a superseded Native resume %s", async outcome => {
    const current = pauseFixture();
    const pending = deferred<void>();
    current.setPaused.mockReturnValueOnce(pending.promise);
    current.context.toggleSharingPause();
    if (outcome === "retired") {
      current.activeGenerationRef.current = null;
      current.dispose(false);
    } else {
      current.context.handleSignalMessage({ type: "pause-sharing-source", shareGeneration: "share" }, 1, false, null);
      expect(current.setPaused).toHaveBeenLastCalledWith("share", true);
    }
    current.context.setSharingPaused.mockClear();
    current.setNoticeValue.mockClear();
    if (outcome === "failure") pending.reject(new Error("old resume failed"));
    else pending.resolve();
    await new Promise(resolve => setTimeout(resolve, 0));
    expect(current.signal.setSharingPaused).not.toHaveBeenCalled();
    expect(current.context.setSharingPaused).not.toHaveBeenCalled();
    expect(current.setNoticeValue).not.toHaveBeenCalled();
    expect(current.sharingPauseOperationRef.current).toBeNull();
  });

  it("does not publish an old resume rollback after authoritative pause takes over", async () => {
    const current = pauseFixture();
    const rollback = deferred<void>();
    current.signal.setSharingPaused.mockReturnValue(false);
    current.setPaused.mockResolvedValueOnce().mockReturnValueOnce(rollback.promise);
    current.context.toggleSharingPause();
    await vi.waitFor(() => expect(current.setPaused).toHaveBeenCalledTimes(2));
    current.context.handleSignalMessage({ type: "pause-sharing-source", shareGeneration: "share" }, 1, false, null);
    current.signal.confirmSharingPaused.mockClear();
    current.setNoticeValue.mockClear();
    rollback.resolve();
    await new Promise(resolve => setTimeout(resolve, 0));
    expect(current.signal.confirmSharingPaused).not.toHaveBeenCalled();
    expect(current.setNoticeValue).not.toHaveBeenCalled();
  });
});
