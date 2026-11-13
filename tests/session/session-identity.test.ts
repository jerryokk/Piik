import { afterEach, describe, expect, it, vi } from "vitest";
import { clearHostRoom, getStableClientId, mergeAuthenticatedHostRoom, readHostRoom, releaseHostRoom, readPreferredRoomId, readViewerGrant, readViewerRoute, watchViewerInvites, replaceViewerInvite, writeHostRoom, writePreferredRoom } from "../../src/client/lib/session";
import { restoreClientState, hostRoomStorage, storedHost } from "../support/client-state.ts";

afterEach(restoreClientState);
describe("client session identity", () => {
  it("works without secure-context-only crypto.randomUUID", () => {
    const values = new Map<string, string>();
    vi.stubGlobal("window", {
      localStorage: {
        getItem: (key: string) => values.get(key) ?? null,
        setItem: (key: string, value: string) => values.set(key, value),
      },
    });
    vi.stubGlobal("crypto", {
      getRandomValues: (bytes: Uint8Array) => {
        bytes.fill(0xab);
        return bytes;
      },
    });

    const first = getStableClientId("viewer", "room-id-1234");

    expect(first).toBe("ab".repeat(16));
    expect(getStableClientId("viewer", "room-id-1234")).toBe(first);
  });

  it("persists a tab room and one validated origin resume hint without Viewer grants", async () => {
    const { local: values, session } = hostRoomStorage();
    const room = {
      roomId: "1234",
      hostToken: "a".repeat(32),
      inviteUrl: `https://share.test/r/1234#v=${"b".repeat(21)}A`,
      codeEntryPolicy: "open" as const,
    };

    await writeHostRoom(room);

    expect(await readHostRoom()).toEqual({
      roomId: "1234",
      hostToken: "a".repeat(32),
      canonicalUrl: "https://share.test/r/1234",
    });
    expect(values.get("piik:host-room:v1")).toBe(
      JSON.stringify({
        roomId: "1234",
        hostToken: "a".repeat(32),
        inviteUrl: "https://share.test/r/1234",
      }),
    );
    expect([...values.values()].join(" ")).not.toContain(`${"b".repeat(21)}A`);
    expect(session.get("piik:host-room:v1")).toBe(values.get("piik:host-room:v1"));
    clearHostRoom();
    expect(await readHostRoom()).toBeNull();
  });

  it("restores an origin resume hint only after claiming its room", async () => {
    const { local, session, request } = hostRoomStorage();
    local.set("piik:host-room:v1", JSON.stringify(storedHost("1234", "h".repeat(32))));
    expect(await readHostRoom()).toEqual({
      roomId: "1234",
      hostToken: "h".repeat(32),
      canonicalUrl: "https://share.test/r/1234",
    });
    expect(JSON.parse(session.get("piik:host-room:v1")!)).toEqual(JSON.parse(local.get("piik:host-room:v1")!));
    expect(request).toHaveBeenCalledWith(expect.stringMatching(/^piik:host-room:1234:[a-f0-9]{64}$/), { ifAvailable: true }, expect.any(Function));
    expect(request.mock.calls[0]![0]).not.toContain("h".repeat(32));
  });

  it("keeps stored room authority despite unowned metadata and discards malformed records", async () => {
    const { local: values, session } = hostRoomStorage();
    values.set("piik:host-room:v1", JSON.stringify({ ...storedHost(), unusedMetadata: 123 }));
    expect(await readHostRoom()).toEqual({
      roomId: "1234", hostToken: "a".repeat(32), canonicalUrl: "https://share.test/r/1234",
    });
    expect(JSON.parse(session.get("piik:host-room:v1")!)).toEqual(storedHost());
    releaseHostRoom();
    session.delete("piik:host-room:v1");
    values.set("piik:host-room:v1", "not-json");
    expect(await readHostRoom()).toBeNull();
    expect(values.has("piik:host-room:v1")).toBe(false);
  });

  it("rejects a copied tab room while another tab holds it without deleting that hint", async () => {
    const { local, session } = hostRoomStorage("held");
    const saved = JSON.stringify(storedHost());
    local.set("piik:host-room:v1", saved);
    session.set("piik:host-room:v1", saved);
    expect(await readHostRoom()).toBeNull();
    expect(session.has("piik:host-room:v1")).toBe(false);
    expect(local.get("piik:host-room:v1")).toBe(saved);
  });

  it("prefers this tab's room over another tab's latest hint and preserves that hint when forgotten", async () => {
    const { local, session } = hostRoomStorage();
    session.set("piik:host-room:v1", JSON.stringify(storedHost()));
    const other = JSON.stringify(storedHost("5678", "b".repeat(32)));
    local.set("piik:host-room:v1", other);
    expect(await readHostRoom()).toMatchObject({ roomId: "1234" });
    clearHostRoom();
    expect(session.has("piik:host-room:v1")).toBe(false);
    expect(local.get("piik:host-room:v1")).toBe(other);
  });

  it("retains both records on unmount, reclaims on refresh, and releases the old claim on rotation", async () => {
    const { local, session, held, request } = hostRoomStorage();
    const room = storedHost();
    await writeHostRoom({ ...room, codeEntryPolicy: "open" });
    const stored = local.get("piik:host-room:v1");
    releaseHostRoom();
    expect(session.get("piik:host-room:v1")).toBe(stored);
    expect(local.get("piik:host-room:v1")).toBe(stored);
    expect(await readHostRoom()).toMatchObject({ roomId: "1234" });
    expect(request).toHaveBeenCalledTimes(2);
    await writeHostRoom({ ...storedHost("5678", "b".repeat(32)), codeEntryPolicy: "open" });
    expect(held.size).toBe(1);
    expect([...held][0]).toContain(":5678:");
  });

  it("coalesces concurrent restoration and preserves records when an unmounted restore is cancelled", async () => {
    const { local, session, request } = hostRoomStorage();
    local.set("piik:host-room:v1", JSON.stringify(storedHost()));
    const first = await Promise.all([readHostRoom(), readHostRoom()]);
    expect(first[0]).toEqual(first[1]);
    expect(request).toHaveBeenCalledOnce();
    releaseHostRoom();
    const cancelled = readHostRoom();
    releaseHostRoom();
    expect(await cancelled).toBeNull();
    expect(session.has("piik:host-room:v1")).toBe(true);
    expect(await readHostRoom()).toMatchObject({ roomId: "1234" });
  });

  it("does not reuse the origin hint without Web Locks but keeps same-tab refresh authority", async () => {
    const { local, session } = hostRoomStorage("absent");
    const saved = JSON.stringify(storedHost());
    local.set("piik:host-room:v1", saved);
    expect(await readHostRoom()).toBeNull();
    session.set("piik:host-room:v1", saved);
    expect(await readHostRoom()).toMatchObject({ roomId: "1234" });
    releaseHostRoom();
    expect(await readHostRoom()).toMatchObject({ roomId: "1234" });
  });

  it("does not erase the resume hint when the server replaces this Host session", async () => {
    const { local, session } = hostRoomStorage();
    await writeHostRoom({ ...storedHost(), codeEntryPolicy: "open" });
    const saved = local.get("piik:host-room:v1");
    clearHostRoom(true);
    expect(session.has("piik:host-room:v1")).toBe(false);
    expect(local.get("piik:host-room:v1")).toBe(saved);
  });

  it("keeps same-tab authority when Web Locks are denied, without restoring the origin hint", async () => {
    const { local, session, request } = hostRoomStorage();
    request.mockRejectedValue(new DOMException("Blocked", "SecurityError"));
    const saved = JSON.stringify(storedHost());
    local.set("piik:host-room:v1", saved);
    expect(await readHostRoom()).toBeNull();
    session.set("piik:host-room:v1", saved);
    expect(await readHostRoom()).toMatchObject({ roomId: "1234" });
    expect(session.get("piik:host-room:v1")).toBeTruthy();
  });

  it("does not persist an obsolete creation and gives a reused code a distinct incarnation claim", async () => {
    const { local, held, request } = hostRoomStorage();
    let owns = true;
    const cancelled = writeHostRoom({ ...storedHost(), codeEntryPolicy: "open" }, () => owns);
    owns = false;
    expect(await cancelled).toBe(false);
    expect(local.size).toBe(0);
    await writeHostRoom({ ...storedHost(), codeEntryPolicy: "open" });
    const first = request.mock.calls.at(-1)![0];
    await writeHostRoom({ ...storedHost("1234", "b".repeat(32)), codeEntryPolicy: "open" });
    expect(request.mock.calls.at(-1)![0]).not.toBe(first);
    expect(held.size).toBe(1);
  });

  it("retains the tab claim when a share's metadata update becomes obsolete", async () => {
    const { held, request, local } = hostRoomStorage();
    const room = { ...storedHost(), codeEntryPolicy: "open" as const };
    await writeHostRoom(room);
    const saved = local.get("piik:host-room:v1");
    let owns = true;
    const writing = writeHostRoom({ ...room, inviteUrl: "https://new.test/r/1234" }, () => owns);
    owns = false;
    expect(await writing).toBe(false);
    expect(held.size).toBe(1);
    expect(local.get("piik:host-room:v1")).toBe(saved);
    expect(await readHostRoom()).toMatchObject({ roomId: "1234", canonicalUrl: "https://share.test/r/1234" });
    expect(request).toHaveBeenCalledOnce();
  });

  it("consumes a Viewer grant fragment once into room-scoped session storage", () => {
    const values = new Map<string, string>();
    const replaceState = vi.fn();
    const grant = `${"c".repeat(21)}g`;
    vi.stubGlobal("window", {
      location: new URL(`https://share.test/r/1234#v=${grant}`),
      history: { state: { navigation: 1 }, replaceState },
      sessionStorage: {
        getItem: (key: string) => values.get(key) ?? null,
        setItem: (key: string, value: string) => values.set(key, value),
        removeItem: (key: string) => values.delete(key),
      },
    });

    expect(readViewerRoute()).toEqual({ roomId: "1234", viewerGrant: grant });
    expect(values.get("piik:viewer-grant:1234")).toBe(grant);
    expect(replaceState).toHaveBeenCalledWith(
      { navigation: 1 },
      "",
      "/r/1234",
    );
    expect(readViewerGrant("8")).toBeNull();
  });

  it("fails malformed Viewer fragments closed without leaking across rooms", () => {
    const values = new Map<string, string>();
    const oldGrant = `${"d".repeat(21)}w`;
    values.set("piik:viewer-grant:1234", oldGrant);
    vi.stubGlobal("window", {
      location: new URL("https://share.test/r/1234#v=malformed"),
      history: { state: null, replaceState: vi.fn() },
      sessionStorage: {
        getItem: (key: string) => values.get(key) ?? null,
        setItem: (key: string, value: string) => values.set(key, value),
        removeItem: (key: string) => values.delete(key),
      },
    });

    expect(readViewerRoute()).toEqual({ roomId: "1234", invalidGrant: true });
    expect(readViewerGrant("1234")).toBeNull();
    replaceViewerInvite("1234", `https://share.test/r/1234#v=${"e".repeat(21)}Q`);
    expect(readViewerGrant("1234")).toBe(`${"e".repeat(21)}Q`);
    replaceViewerInvite("1234", `https://share.test/r/5678#v=${"f".repeat(21)}A`);
    expect(readViewerGrant("1234")).toBeNull();
    expect(readViewerGrant("5678")).toBeNull();
  });

  it("keeps the stored Viewer grant when the URL carries an unrelated fragment", () => {
    const values = new Map<string, string>();
    const grant = `${"d".repeat(21)}w`;
    values.set("piik:viewer-grant:1234", grant);
    const replaceState = vi.fn();
    vi.stubGlobal("window", {
      location: new URL("https://share.test/r/1234#:~:text=hello"),
      history: { state: null, replaceState },
      sessionStorage: {
        getItem: (key: string) => values.get(key) ?? null,
        setItem: (key: string, value: string) => values.set(key, value),
        removeItem: (key: string) => values.delete(key),
      },
    });

    expect(readViewerRoute()).toEqual({ roomId: "1234", viewerGrant: grant });
    expect(readViewerGrant("1234")).toBe(grant);
    expect(replaceState).not.toHaveBeenCalled();
  });

  it("consumes renewed same-page invitations and ignores unrelated fragments", () => {
    const events = new EventTarget();
    const values = new Map<string, string>();
    const location = new URL("https://share.test/r/1234");
    vi.stubGlobal("window", {
      location,
      addEventListener: events.addEventListener.bind(events),
      removeEventListener: events.removeEventListener.bind(events),
      history: { state: null, replaceState: (_state: unknown, _unused: string, url: string) => {
        location.href = new URL(url, location).href;
      } },
      sessionStorage: {
        getItem: (key: string) => values.get(key) ?? null,
        setItem: (key: string, value: string) => values.set(key, value),
        removeItem: (key: string) => values.delete(key),
      },
    });
    const changed = vi.fn();
    const stop = watchViewerInvites(changed);
    const first = `${"g".repeat(21)}g`;
    const renewed = `${"h".repeat(21)}w`;
    for (const grant of [first, renewed]) {
      location.hash = `v=${grant}`;
      events.dispatchEvent(new Event("hashchange"));
      expect(changed).toHaveBeenLastCalledWith({ roomId: "1234", viewerGrant: grant });
      expect(readViewerGrant("1234")).toBe(grant);
      expect(location.hash).toBe("");
    }
    location.hash = "section";
    events.dispatchEvent(new Event("hashchange"));
    expect(changed).toHaveBeenCalledTimes(2);
    location.hash = "v=invalid";
    events.dispatchEvent(new Event("hashchange"));
    expect(changed).toHaveBeenLastCalledWith({ roomId: "1234", invalidGrant: true });
    expect(readViewerGrant("1234")).toBeNull();
    stop();
    location.hash = `v=${renewed}`;
    events.dispatchEvent(new Event("hashchange"));
    expect(changed).toHaveBeenCalledTimes(3);
  });

  it("persists a rotated Viewer invitation across reload and clears it on revoke", () => {
    const values = new Map<string, string>();
    vi.stubGlobal("window", {
      sessionStorage: {
        getItem: (key: string) => values.get(key) ?? null,
        setItem: (key: string, value: string) => values.set(key, value),
        removeItem: (key: string) => values.delete(key),
      },
    });
    const firstGrant = `${"g".repeat(21)}g`;
    const rotatedGrant = `${"h".repeat(21)}w`;

    replaceViewerInvite("1234", `https://share.test/r/1234#v=${firstGrant}`);
    replaceViewerInvite("1234", `https://share.test/r/1234#v=${rotatedGrant}`);

    expect(readViewerGrant("1234")).toBe(rotatedGrant);
    expect(readViewerGrant("1234")).toBe(rotatedGrant);

    replaceViewerInvite("1234", null);
    expect(readViewerGrant("1234")).toBeNull();
  });

  it("keeps the latest rotated or revoked invite through Host re-authentication", () => {
    const values = new Map<string, string>();
    vi.stubGlobal("window", {
      sessionStorage: {
        getItem: (key: string) => values.get(key) ?? null,
        setItem: (key: string, value: string) => values.set(key, value),
        removeItem: (key: string) => values.delete(key),
      },
    });
    const oldInvite = `https://share.test/r/1234#v=${"i".repeat(21)}Q`;
    const rotatedInvite = `https://share.test/r/1234#v=${"j".repeat(21)}A`;
    replaceViewerInvite("1234", oldInvite);
    const activeRoom = {
      roomId: "1234",
      hostToken: "h".repeat(32),
      canonicalUrl: "https://share.test/r/1234",
      codeEntryPolicy: "open" as const,
      inviteUrl: oldInvite,
    };

    expect(
      mergeAuthenticatedHostRoom(
        { ...activeRoom, inviteUrl: rotatedInvite },
        activeRoom.roomId,
        "open",
      )?.inviteUrl,
    ).toBe(rotatedInvite);
    expect(
      mergeAuthenticatedHostRoom(
        { ...activeRoom, inviteUrl: null },
        activeRoom.roomId,
        "open",
      )?.inviteUrl,
    ).toBe(oldInvite);
    replaceViewerInvite(activeRoom.roomId, null);
    expect(
      mergeAuthenticatedHostRoom(
        { ...activeRoom, inviteUrl: null },
        activeRoom.roomId,
        "open",
      )?.inviteUrl,
    ).toBeNull();
  });

  it("keeps the latest preferred room without a client expiry", () => {
    const values = new Map<string, string>();
    vi.stubGlobal("window", {
      localStorage: {
        getItem: (key: string) => values.get(key) ?? null,
        setItem: (key: string, value: string) => values.set(key, value),
        removeItem: (key: string) => values.delete(key),
      },
    });

    writePreferredRoom("4321");

    clearHostRoom();
    expect(readPreferredRoomId()).toBe("4321");
    expect(values.get("piik:host-room-preference:v1")).toBe(
      JSON.stringify({ roomId: "4321" }),
    );
  });

  it("removes a legacy Viewer grant from room-scoped session storage", () => {
    const values = new Map<string, string>([
      ["piik:viewer-grant:1234", `g1.1234.1.${"f".repeat(43)}`],
    ]);
    const removeItem = vi.fn((key: string) => values.delete(key));
    vi.stubGlobal("window", {
      sessionStorage: {
        getItem: (key: string) => values.get(key) ?? null,
        setItem: (key: string, value: string) => values.set(key, value),
        removeItem,
      },
    });

    expect(readViewerGrant("1234")).toBeNull();
    expect(removeItem).toHaveBeenCalledWith("piik:viewer-grant:1234");
  });

  it("keeps a Host room out of localStorage inside the App window", async () => {
    // App Local rooms live in the running process, so a stored copy would
    // outlive the room and be reported as expired on the next launch.
    const storage = hostRoomStorage();
    vi.stubGlobal("__PIIK_APP__", Object.freeze({ version: "development" }));

    await writeHostRoom({ ...storedHost(), codeEntryPolicy: "open" });

    expect(storage.session.has("piik:host-room:v1")).toBe(true);
    expect(storage.local.has("piik:host-room:v1")).toBe(false);
  });

  it("still restores a Browser Host room across a reload", async () => {
    const storage = hostRoomStorage();

    await writeHostRoom({ ...storedHost(), codeEntryPolicy: "open" });
    releaseHostRoom();
    storage.session.clear();

    expect(storage.local.has("piik:host-room:v1")).toBe(true);
    expect(await readHostRoom()).toMatchObject({ roomId: "1234" });
  });
});
