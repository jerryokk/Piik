import { afterEach, describe, expect, it, vi } from "vitest";
import { ApiError, authenticateSiteAccess, createRoom, getSiteAccess, replaceOwnedRoom, updateRoomAccess } from "../../src/client/lib/api.ts";
import { say, setCopy } from "../../src/client/ui/copy.ts";
import { restoreClientState } from "../support/client-state.ts";

afterEach(restoreClientState);
describe("site access API", () => {
  it.each(["zh", "en"] as const)("explains room capacity across creation and replacement in %s", async (lang) => {
    setCopy({ lang, vis: false });
    vi.stubGlobal("fetch", vi.fn<typeof fetch>().mockImplementation(async () =>
      new Response(JSON.stringify({ error: "Room capacity reached" }), { status: 503 }),
    ));
    for (const request of [
      () => createRoom("open", null),
      () => replaceOwnedRoom("4321", "host-token", "open", null),
    ]) {
      await expect(request()).rejects.toMatchObject({ status: 503, message: say("host.err.roomCapacity") });
    }
    vi.stubGlobal("fetch", vi.fn<typeof fetch>().mockResolvedValue(
      new Response(JSON.stringify({ error: "Room creation unavailable" }), { status: 503 }),
    ));
    await expect(createRoom("open", null)).rejects.toMatchObject({
      status: 503, message: say("host.err.createRoomStatus", { status: "503" }),
    });
  });

  it.each(["zh", "en"] as const)("explains rejected origins across write APIs in %s", async (lang) => {
    setCopy({ lang, vis: false });
    vi.stubGlobal("fetch", vi.fn<typeof fetch>().mockImplementation(async () =>
      new Response(JSON.stringify({ error: "Origin not allowed" }), { status: 403 }),
    ));
    const message = lang === "zh"
      ? "当前网址不在服务器允许的范围内 (403)，请联系管理员。"
      : "The server does not allow this address (403); contact the site administrator.";
    for (const request of [
      () => authenticateSiteAccess("password"),
      () => createRoom("open", null),
      () => replaceOwnedRoom("4321", "host-token", "open", null),
      () => updateRoomAccess("4321", "host-token", { action: "rotate-viewer-grant" }),
    ]) {
      await expect(request()).rejects.toMatchObject({ status: 403, message });
    }
  });

  it("does not diagnose arbitrary proxy rejection bodies as an origin mismatch", async () => {
    for (const body of [JSON.stringify({ error: "Forbidden" }), "<html>Forbidden</html>"]) {
      vi.stubGlobal("fetch", vi.fn<typeof fetch>().mockResolvedValue(new Response(body, { status: 403 })));
      await expect(createRoom("open", null)).rejects.toMatchObject({
        status: 403,
        message: expect.not.stringContaining(say("api.originNotAllowed")),
      });
    }
  });

  it("checks site access and sends the exact password in a Unicode-capable JSON body", async () => {
    const fetchMock = vi
      .fn<typeof fetch>()
      .mockResolvedValueOnce(
        new Response(JSON.stringify({ required: true, authenticated: false }), {
          status: 200,
          headers: { "Content-Type": "application/json" },
        }),
      )
      .mockResolvedValueOnce(
        new Response(JSON.stringify({ required: true, authenticated: true }), {
          status: 200,
          headers: { "Content-Type": "application/json" },
        }),
      );
    vi.stubGlobal("fetch", fetchMock);

    await expect(getSiteAccess()).resolves.toEqual({
      required: true,
      authenticated: false,
    });
    await expect(authenticateSiteAccess("  中文 +&  ")).resolves.toEqual({
      required: true,
      authenticated: true,
    });

    expect(fetchMock.mock.calls[0]).toEqual([
      "/api/site-access",
      { headers: { Accept: "application/json" }, cache: "no-store", signal: undefined },
    ]);
    const post = fetchMock.mock.calls[1][1];
    expect(post?.method).toBe("POST");
    expect(JSON.parse(String(post?.body))).toEqual({ password: "  中文 +&  " });
    expect(new Headers(post?.headers).get("Content-Type")).toBe("application/json");
    expect(new Headers(post?.headers).has("Authorization")).toBe(false);
  });

  it("uses a clear Chinese message for a rejected password", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn<typeof fetch>().mockResolvedValue(
        new Response(JSON.stringify({ error: "Unauthorized" }), {
          status: 401,
          headers: { "Content-Type": "application/json" },
        }),
      ),
    );

    await expect(authenticateSiteAccess("wrong-password")).rejects.toMatchObject({
      status: 401,
      message: "站点口令不正确，请重试",
    });
  });

  it("does not expose an English server error body to the access page", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn<typeof fetch>().mockResolvedValue(
        new Response(JSON.stringify({ error: "Internal Server Error" }), {
          status: 503,
          headers: { "Content-Type": "application/json" },
        }),
      ),
    );

    await expect(getSiteAccess()).rejects.toMatchObject({
      status: 503,
      message: "站点服务暂不可用 (503)，请稍后重试",
    });
  });

  it("does not send a raw bearer token when creating a room by default", async () => {
    const fetchMock = vi.fn<typeof fetch>().mockResolvedValue(
      new Response(JSON.stringify({ error: "test response" }), {
        status: 500,
        headers: { "Content-Type": "application/json" },
      }),
    );
    vi.stubGlobal("fetch", fetchMock);

    await expect(createRoom("open", null)).rejects.toBeInstanceOf(ApiError);

    const headers = new Headers(fetchMock.mock.calls[0][1]?.headers);
    expect(headers.has("Authorization")).toBe(false);
    expect(fetchMock.mock.calls[0][1]?.body).toBe(
      JSON.stringify({ codeEntryPolicy: "open" }),
    );
  });

  it("sends only the preferred room code when one is current", async () => {
    const fetchMock = vi.fn<typeof fetch>().mockResolvedValue(
      new Response(JSON.stringify({ error: "test response" }), {
        status: 500,
        headers: { "Content-Type": "application/json" },
      }),
    );
    vi.stubGlobal("fetch", fetchMock);

    await expect(createRoom("open", null, "4321")).rejects.toBeInstanceOf(
      ApiError,
    );

    expect(fetchMock.mock.calls[0][1]?.body).toBe(
      JSON.stringify({ codeEntryPolicy: "open", preferredRoomId: "4321" }),
    );
  });

  it("sends exact Host authority and no preferred code for room replacement", async () => {
    const fetchMock = vi.fn<typeof fetch>().mockResolvedValue(
      new Response(JSON.stringify({ error: "test response" }), {
        status: 500,
        headers: { "Content-Type": "application/json" },
      }),
    );
    vi.stubGlobal("fetch", fetchMock);

    await expect(
      replaceOwnedRoom("4321", "host-token", "private", "room-password"),
    ).rejects.toBeInstanceOf(ApiError);

    expect(fetchMock.mock.calls[0][0]).toBe("/api/rooms/4321/replacement");
    const headers = new Headers(fetchMock.mock.calls[0][1]?.headers);
    expect(headers.get("Authorization")).toBe("Bearer host-token");
    expect(fetchMock.mock.calls[0][1]?.body).toBe(
      JSON.stringify({
        codeEntryPolicy: "private",
        roomPassword: "room-password",
      }),
    );
  });

  it("accepts room creation with permanent authority", async () => {
    const room = {
      roomId: "1234",
      hostToken: "c".repeat(32),
      inviteUrl: `https://share.test/r/1234#v=${"f".repeat(21)}A`,
      codeEntryPolicy: "open",
    };
    vi.stubGlobal(
      "fetch",
      vi.fn<typeof fetch>().mockResolvedValue(
        new Response(JSON.stringify(room), {
          status: 201,
          headers: { "Content-Type": "application/json" },
        }),
      ),
    );

    await expect(createRoom("open", null)).resolves.toEqual(room);
  });
});
