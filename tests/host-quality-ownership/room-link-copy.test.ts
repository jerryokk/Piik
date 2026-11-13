import { describe, expect, it, vi } from "vitest";
import { fixture, ref, deferred } from "./support.ts";

describe("Host room-link copy feedback", () => {
  function copyFixture(includeInviteCredential = true) {

    const current = fixture();
    let copiedUrl: string | null = null;
    let notice: unknown = null;
    current.setCopiedRoomLink.mockImplementation((next: string | null) => { copiedUrl = next; });
    const room = { roomId: "1234", hostToken: "host-token", inviteUrl: "https://example.test/r/1234#v=example",
      canonicalUrl: "https://example.test/r/1234" };
    Object.assign(current.roomRef, { current: room });
    current.setNoticeValue.mockImplementation((next: unknown) => {
      notice = typeof next === "function" ? next(notice) : next;
    });
    const writeText = vi.fn(async (_value: string) => {});
    Object.assign(current.context, {

      navigator: { clipboard: { writeText } },
      copiedResetTimerRef: ref<number | null>(null),
      copyRoomLinkRequestRef: ref<object | null>(null),
      includeInviteCredential, roomLinkBlocked: false, roomMutating: false,
      window: { setTimeout: vi.fn(() => 1), clearTimeout: vi.fn() },
    });
    return { ...current, writeText, copied: () => copiedUrl !== null && copiedUrl === (current.context.includeInviteCredential
      ? current.context.roomRef.current?.inviteUrl : current.context.roomRef.current?.canonicalUrl),
      replaceInvite: (inviteUrl: string | null) => { current.context.roomRef.current = { ...room, inviteUrl }; }, notice: () => notice,
      copy: current.context.copyRoomLink as () => Promise<void> };
  }

  it("copies the canonical room URL when password-free invitation is off", async () => {
    const current = copyFixture(false);
    await current.copy();
    expect(current.writeText).toHaveBeenCalledWith("https://example.test/r/1234");
    expect(current.copied()).toBe(true);
  });

  it("does not copy a plain link that cannot admit a Viewer", async () => {
    const current = copyFixture(false);
    current.context.roomLinkBlocked = true;
    await current.copy();
    expect(current.writeText).not.toHaveBeenCalled();
    expect(current.copied()).toBe(false);
  });

  it("clears an earlier success when the next copy fails", async () => {
    const current = copyFixture();
    await current.copy();
    expect(current.copied()).toBe(true);
    current.writeText.mockRejectedValueOnce(new Error("clipboard denied"));
    await current.copy();
    expect(current.copied()).toBe(false);
    expect(current.notice()).toMatchObject({ key: "host.invite.copyFailed", comic: "copy-failed", tone: "bad" });
  });

  it("clears its own failure after a successful retry", async () => {
    const current = copyFixture();
    current.writeText.mockRejectedValueOnce(new Error("clipboard denied"));
    await current.copy();
    expect(current.notice()).toMatchObject({ key: "host.invite.copyFailed" });
    await current.copy();
    expect(current.copied()).toBe(true);
    expect(current.notice()).toBeNull();
  });

  it("preserves another notice that arrives while copying", async () => {
    const current = copyFixture();
    current.writeText.mockRejectedValueOnce(new Error("clipboard denied"));
    await current.copy();
    const pending = deferred<void>();
    current.writeText.mockReturnValueOnce(pending.promise);
    const copying = current.copy();
    current.context.setNoticeErrorKey("host.accessFailed", "access-denied");
    const newer = current.notice();
    pending.resolve();
    await copying;
    expect(current.copied()).toBe(true);
    expect(current.notice()).toBe(newer);
  });

  it.each([false, true])("ignores a retired invite's late clipboard completion (failure=%s)", async (failed) => {
    const current = copyFixture();
    const pending = deferred<void>();
    current.writeText.mockReturnValueOnce(pending.promise);
    const oldCopy = current.copy();
    current.replaceInvite("https://example.test/r/1234#v=replacement");
    expect(current.copied()).toBe(false);
    await current.copy();
    expect(current.copied()).toBe(true);
    if (failed) pending.reject(new Error("old clipboard failure"));
    else pending.resolve();
    await oldCopy;
    expect(current.copied()).toBe(true);
    expect(current.notice()).toBeNull();
  });

  it.each([false, true])("keeps the latest result for concurrent copies of the same URL (latest failed=%s)", async (latestFailed) => {
    const current = copyFixture();
    const pending = deferred<void>();
    current.writeText.mockReturnValueOnce(pending.promise);
    const older = current.copy();
    if (latestFailed) current.writeText.mockRejectedValueOnce(new Error("latest denied"));
    await current.copy();
    const latestNotice = current.notice();
    if (latestFailed) pending.resolve();
    else pending.reject(new Error("older denied"));
    await older;
    expect(current.copied()).toBe(!latestFailed);
    expect(current.notice()).toBe(latestNotice);
  });
});
