import { afterEach, describe, expect, it, vi } from "vitest";
import { clientLaunchURL, isValidRoomId, parseAppRoute, roomRouteForExplicitEntry, roomRouteFromInput, takeClientLaunchBootstrap } from "../../src/client/lib/session";
import { restoreClientState } from "../support/client-state.ts";

afterEach(restoreClientState);
describe("room codes", () => {
  it("accepts exactly four decimal digits without a leading zero", () => {
    expect(isValidRoomId("1000")).toBe(true);
    expect(isValidRoomId("9999")).toBe(true);
    expect(isValidRoomId("123")).toBe(false);
    expect(isValidRoomId("12345")).toBe(false);
    expect(isValidRoomId("0")).toBe(false);
    expect(isValidRoomId("0123")).toBe(false);
    expect(isValidRoomId("123a")).toBe(false);
    expect(roomRouteFromInput("1234")).toBe("/r/1234");
    expect(roomRouteFromInput(" 1234 ")).toBeNull();
    expect(roomRouteFromInput("１２３４")).toBeNull();
    expect(roomRouteFromInput("0123")).toBeNull();
  });

  it("drops only the stored grant for an explicit valid room-code entry", () => {
    const values = new Map<string, string>([
      ["piik:viewer-grant:1234", "AAAAAAAAAAAAAAAAAAAAAA"],
      ["piik:viewer-grant:5678", "AAAAAAAAAAAAAAAAAAAAAA"],
    ]);
    vi.stubGlobal("window", {
      sessionStorage: {
        removeItem: (key: string) => values.delete(key),
      },
    });

    expect(roomRouteForExplicitEntry("1234")).toBe("/r/1234");
    expect(values.has("piik:viewer-grant:1234")).toBe(false);
    expect(values.has("piik:viewer-grant:5678")).toBe(true);

    expect(roomRouteForExplicitEntry(" 5678 ")).toBeNull();
    expect(values.has("piik:viewer-grant:5678")).toBe(true);
  });

  it("classifies routes without normalizing malformed room input", () => {
    expect(parseAppRoute("/client")).toEqual({ kind: "client" });
    expect(parseAppRoute("/client/")).toEqual({ kind: "client" });
    expect(parseAppRoute("/")).toEqual({ kind: "host" });
    expect(parseAppRoute("/join")).toEqual({ kind: "join" });
    expect(parseAppRoute("/join/")).toEqual({ kind: "join" });
    expect(parseAppRoute("/r/1000")).toEqual({
      kind: "viewer",
      roomId: "1000",
    });
    expect(parseAppRoute("/r/9999/")).toEqual({
      kind: "viewer",
      roomId: "9999",
    });
    for (const pathname of [
      "/r",
      "/r/",
      "/r/123",
      "/r/12345",
      "/r/0000",
      "/r/abcd",
      "/r/１２３４",
      "/r/1234/extra",
    ]) {
      expect(parseAppRoute(pathname)).toEqual({ kind: "malformed-room" });
    }
    expect(parseAppRoute("/other")).toEqual({ kind: "unknown" });
  });

  it("consumes one exact App access bootstrap without retaining it in the URL", () => {
    const replaceState = vi.fn();
    vi.stubGlobal("window", {
      location: {
        hash: `#client-access=${"a".repeat(32)}`,
        pathname: "/",
        search: "?mode=local",
      },
      history: { state: { current: true }, replaceState },
    });

    expect(takeClientLaunchBootstrap()).toMatchObject({
      accessToken: "a".repeat(32),
      launchedByClient: false,
    });
    expect(replaceState).toHaveBeenCalledWith(
      { current: true },
      "",
      "/?mode=local",
    );
  });

  it("removes an empty App bootstrap without authenticating it", () => {
    const replaceState = vi.fn();
    vi.stubGlobal("window", {
      location: { hash: "#client-access=", pathname: "/", search: "" },
      history: { state: null, replaceState },
    });

    expect(takeClientLaunchBootstrap().accessToken).toBeNull();
    expect(replaceState).toHaveBeenCalledWith(null, "", "/");
  });

  it.each(["x", "中文", " ", "  中文 +&  ", "x".repeat(256)])("preserves a chosen App access value: %s", (password) => {
    const replaceState = vi.fn();
    vi.stubGlobal("window", {
      location: {
        hash: `#${new URLSearchParams({ "client-access": password })}`,
        pathname: "/",
        search: "",
      },
      history: { state: null, replaceState },
    });

    expect(takeClientLaunchBootstrap().accessToken).toBe(password);
    expect(replaceState).toHaveBeenCalledWith(null, "", "/");
  });

  it("consumes the App launch marker from a server-private fragment", () => {
    const replaceState = vi.fn();
    const session = new Map<string, string>();
    vi.stubGlobal("window", {
      location: {
        hash: `#client-access=${"a".repeat(32)}&piik-client=1&retained=yes`,
        pathname: "/",
        search: "?room=6020",
      },
      history: { state: null, replaceState },
      localStorage: {
        getItem: (key: string) => session.get(key) ?? null,
        setItem: (key: string, value: string) => session.set(key, value),
      },
    });

    expect(takeClientLaunchBootstrap()).toEqual({
      accessToken: "a".repeat(32),
      launchedByClient: true,
      presentation: null,
    });
    expect(replaceState).toHaveBeenCalledWith(
      null,
      "",
      "/?room=6020#retained=yes",
    );

    window.location.hash = "";
    expect(takeClientLaunchBootstrap()).toEqual({
      accessToken: null,
      launchedByClient: true,
      presentation: null,
    });
  });

  it.each([
    { lang: "zh", vis: false, theme: "light" },
    { lang: "en", vis: true, theme: null },
  ] as const)("carries launcher presentation across origins: %j", (presentation) => {
    const target = new URL(clientLaunchURL(
      "https://site.example/?room=9527#piik-client=1&client-access=a%2Bb%26c%3Fd%3De&retained=yes",
      presentation,
    ));
    const replaceState = vi.fn();
    vi.stubGlobal("window", {
      location: target,
      history: { state: null, replaceState },
    });

    expect(target.origin).toBe("https://site.example");
    expect(takeClientLaunchBootstrap()).toEqual({
      accessToken: "a+b&c?d=e",
      launchedByClient: true,
      presentation: { ...presentation, explicit: [] },
    });
    expect(replaceState).toHaveBeenCalledWith(null, "", "/?room=9527#retained=yes");
  });

  it.each([
    ["piik-lang=zh&piik-mode=text&piik-theme=light", null],
    ["piik-client=1&piik-lang=other&piik-mode=text&piik-theme=light", { vis: false, theme: "light", explicit: [] }],
    ["piik-client=1&piik-lang=zh&piik-mode=other&piik-theme=light", { lang: "zh", theme: "light", explicit: [] }],
    ["piik-client=1&piik-lang=zh&piik-mode=text&piik-theme=other", { lang: "zh", vis: false, explicit: [] }],
  ])("ignores invalid preference fields without losing App access: %s", (fragment, presentation) => {
    const replaceState = vi.fn();
    vi.stubGlobal("window", {
      location: { hash: `#${fragment}&client-access=${"a".repeat(32)}`, pathname: "/", search: "" },
      history: { state: null, replaceState },
    });
    expect(takeClientLaunchBootstrap()).toMatchObject({
      accessToken: "a".repeat(32),
      presentation,
    });
    expect(replaceState).toHaveBeenCalledWith(null, "", "/");
  });
});
