import { describe, expect, it, vi } from "vitest";
import { ApiError } from "../../src/client/lib/api";
import { fixture, useRealResourceDisposal, ref, deferred } from "./support.ts";

describe("Host room ownership", () => {
  function roomFixture(nativeConnected = true) {
    const current = fixture(true, nativeConnected);
    const room = { roomId: "1234", hostToken: "host-token", canonicalUrl: "https://example.test/r/1234", codeEntryPolicy: "open", inviteUrl: null };
    const signals: Signal[] = [];
    class Signal {
      interactions = {};
      roomVoice = {};
      stop = vi.fn();
      stopSharing = vi.fn();
      startSharing = vi.fn();
      setAudioAvailable = vi.fn();
      setCoPlayGrants = vi.fn();
      enableRemoteInput = vi.fn();
      constructor(public identity: { roomId: string; token: string; shareGeneration?: string },
        public events: {
          onMessage(message: unknown): void;
          onTerminated(reason: "SIGNAL_TERMINATED"): void;
          onAccessRequired(): void;
        }) { signals.push(this); }
      ownsHostRoom(roomId: string, token: string) { return this.identity.roomId === roomId && this.identity.token === token; }
      wantsHostPublication(generation: string) { return this.identity.shareGeneration === generation; }
    }
    const forgetRoom = vi.fn(current.context.forgetRoom);
    Object.assign(current.roomRef, { current: room });
    Object.assign(current.signalRef, { current: null });
    current.generationRef.current = 1;
    Object.assign(current.context, {
      SignalingClient: Signal,
      visRef: ref(false), hostClientIdRef: ref(null), displayNameRef: ref("Host"),
      getStableClientId: () => "host-client", defaultHostDisplayName: () => "Host", readDisplayName: () => "Host",
      setDisplayName: vi.fn(), setDisplayNameDraft: vi.fn(), setDisplayNameError: vi.fn(),
      setInteractionSession: vi.fn(), setRoomVoiceSession: vi.fn(), setSignalStatus: vi.fn(), forgetRoom,
      onAuthorizationRequired: vi.fn(), clearHostRoom: vi.fn(), writePreferredRoom: vi.fn(),
    });
    return { ...current, room, signals, forgetRoom, connect: (value = room) => current.context.connectRoomSignal(value) as Signal };
  }

  it("restores one idle connection without creating a publication", () => {
    const current = roomFixture();
    current.activeGenerationRef.current = null;
    current.shareGenerationRef.current = null;
    const signal = current.connect();
    expect(signal.identity).toMatchObject({ roomId: "1234" });
    expect(signal.identity.shareGeneration).toBeUndefined();
    expect(signal.startSharing).not.toHaveBeenCalled();
    expect(current.connect()).toBe(signal);
    expect(current.signals).toHaveLength(1);
  });

  it("does not mistake capture permission in progress for publication intent", () => {
    const current = roomFixture();
    const signal = current.connect();
    signal.events.onMessage({ type: "error", code: "INVALID_TOKEN" });
    expect(current.forgetRoom).toHaveBeenCalledWith(current.room);
    expect(current.createRoom).not.toHaveBeenCalled();
    expect(current.disposeResources).not.toHaveBeenCalled();
    expect(current.activeGenerationRef.current).toBe(1);
    expect(current.setPhase).not.toHaveBeenCalled();
    expect(current.setNoticeValue).not.toHaveBeenCalled();
  });

  it("shows a rejected restored room on the television before any sharing starts", () => {
    const current = roomFixture();
    current.activeGenerationRef.current = null;
    current.shareGenerationRef.current = null;
    current.connect().events.onMessage({ type: "error", code: "INVALID_TOKEN" });
    expect(current.setNoticeValue).toHaveBeenCalledExactlyOnceWith({
      kind: "key", key: "host.roomInvalid", vars: undefined,
      target: "television", comic: "room-not-found", tone: "bad",
    });
  });

  it.each(["INVALID_TOKEN", "HOST_ALREADY_CONNECTED", "room-closed", "terminated", "access-required"])(
    "retires idle native selection and its late results on %s", async event => {
      for (const stage of ["ready", "discovery", "enumeration"]) {
        const current = roomFixture(stage !== "discovery");
        useRealResourceDisposal(current);
        current.context.phase = "idle";
        current.activeGenerationRef.current = null;
        current.shareGenerationRef.current = null;
        current.nativeShareGenerationRef.current = null;
        const discovery = deferred<typeof current.client>();
        const enumeration = deferred<never[]>();
        if (stage === "discovery") {
          current.NativeClient.connect.mockReturnValueOnce(discovery.promise);
        } else if (stage === "enumeration") current.client.sources.mockReturnValueOnce(enumeration.promise);
        const signal = current.connect();
        const selecting = current.openPicker();
        if (stage === "ready") await selecting;
        else await vi.waitFor(() => expect(stage === "discovery"
          ? current.NativeClient.connect : current.client.sources).toHaveBeenCalledOnce());
        expect(current.nativeSourceRequestRef.current).not.toBeNull();

        if (event === "terminated") signal.events.onTerminated("SIGNAL_TERMINATED");
        else if (event === "access-required") signal.events.onAccessRequired();
        else signal.events.onMessage(event === "room-closed" ? { type: event } : { type: "error", code: event });
        expect(current.nativeSourceRequestRef.current).toBeNull();
        expect(current.nativeSourcePathRef.current).toBeNull();
        discovery.resolve(current.client);
        enumeration.resolve([]);
        await selecting;
        expect(current.nativeConnection.current).toBeNull();
        expect(current.client.close).toHaveBeenCalledOnce();
        expect(current.setNativeSources).toHaveBeenLastCalledWith(null);
        expect(current.createRoom).not.toHaveBeenCalled();
        if (stage === "discovery") expect(current.client.sources).not.toHaveBeenCalled();
      }
    },
  );

  it.each([false, true])("retires a room rejected by an access update (sharing=%s)", async sharing => {
    const current = roomFixture();
    useRealResourceDisposal(current);
    current.context.phase = sharing ? "live" : "idle";
    current.nativeModeRef.current = sharing;
    if (!sharing) {
      current.activeGenerationRef.current = null;
      current.shareGenerationRef.current = null;
      current.nativeShareGenerationRef.current = null;
      current.streamRef.current = null;
    }
    const signal = current.connect();
    await current.openPicker();
    current.context.handleRoomAccessFailure(new ApiError("Room not found", 404), current.room);
    expect(current.roomRef.current).toBeNull();
    expect(current.signalRef.current).toBeNull();
    expect(signal.stop).toHaveBeenCalledOnce();
    expect(current.activeGenerationRef.current).toBeNull();
    expect(current.track.stop).toHaveBeenCalledTimes(sharing ? 1 : 0);
    expect(current.client.close).toHaveBeenCalledOnce();
    expect(current.setNativeSources).toHaveBeenLastCalledWith(null);
    expect(current.setNoticeValue).toHaveBeenLastCalledWith(expect.objectContaining({
      key: "host.roomInvalid", target: "television", comic: "room-not-found", tone: "bad",
    }));
  });

  it("retires the room session when a replacement fails after capture was stopped", async () => {
    const current = roomFixture();
    useRealResourceDisposal(current);
    const response = deferred<never>();
    current.context.replaceOwnedRoom = vi.fn(() => response.promise);
    const signal = current.connect();
    const replacing = current.context.replaceCurrentRoom();
    current.context.endSharing({ key: "host.stopNotice" });
    expect(signal.stop).not.toHaveBeenCalled();
    expect(current.signalRef.current).toBe(signal);
    response.reject(new ApiError("Room not found", 404));
    await replacing;
    expect(current.roomRef.current).toBeNull();
    expect(current.signalRef.current).toBeNull();
    expect(signal.stop).toHaveBeenCalledOnce();
    expect(current.createRoom).not.toHaveBeenCalled();
    expect(current.roomMutationRef.current).toBeNull();
  });

  it.each(["replace", "missing-room"] as const)("retires an unclaimed %s result after page exit", async path => {
    for (const stage of ["response", "claim", "accepted"] as const) {
      const current = roomFixture();
      current.context.phase = "idle";
      current.activeGenerationRef.current = null;
      const replacement = { ...current.room, roomId: "5678", hostToken: "replacement-token" };
      const response = deferred<typeof replacement>();
      const claim = deferred<void>();
      const writeHostRoom = vi.fn(async (_room: unknown, owns: () => boolean) => {
        await claim.promise;
        return owns();
      });
      const closeAbandonedRoom = vi.fn();
      const request = vi.fn(() => response.promise);
      Object.assign(current.context, {
        replaceOwnedRoom: path === "replace" ? request : async () => { throw new ApiError("Room not found", 404); },
        createRoom: request, hostRoomFromCreated: (room: unknown) => room,
        replaceViewerInvite: vi.fn(), writeHostRoom, closeAbandonedRoom,
      });
      const replacing = current.context.replaceCurrentRoom();
      await vi.waitFor(() => expect(request).toHaveBeenCalledOnce());
      if (stage === "response") current.roomMutationRef.current = null;
      response.resolve(replacement);
      if (stage === "claim") {
        await vi.waitFor(() => expect(writeHostRoom).toHaveBeenCalledOnce());
        current.roomMutationRef.current = null;
      }
      claim.resolve();
      await replacing;
      if (stage === "accepted") {
        expect(closeAbandonedRoom).not.toHaveBeenCalled();
        expect(current.roomRef.current).toBe(replacement);
        expect(current.setRoom).toHaveBeenCalledWith(replacement);
      } else {
        expect(closeAbandonedRoom).toHaveBeenCalledExactlyOnceWith(replacement);
        expect(current.roomRef.current).toBe(current.room);
        expect(current.setRoom).not.toHaveBeenCalled();
        expect(current.setNoticeError).not.toHaveBeenCalled();
        if (stage === "response") expect(writeHostRoom).not.toHaveBeenCalled();
      }
    }
  });

  it("does not recreate a missing room after its replacement operation was retired", async () => {
    const current = roomFixture();
    current.context.phase = "idle";
    current.activeGenerationRef.current = null;
    const response = deferred<never>();
    current.context.replaceOwnedRoom = () => response.promise;
    const replacing = current.context.replaceCurrentRoom();
    current.roomMutationRef.current = null;
    response.reject(new ApiError("Room not found", 404));
    await replacing;
    expect(current.createRoom).not.toHaveBeenCalled();
    expect(current.setNoticeError).not.toHaveBeenCalled();
  });

  it("ignores an old room's failed access update after another room owns the session", () => {
    const current = roomFixture();
    useRealResourceDisposal(current);
    const replacement = { ...current.room, hostToken: "replacement-token" };
    Object.assign(current.roomRef, { current: replacement });
    const signal = current.connect(replacement);
    current.context.handleRoomAccessFailure(new ApiError("Room not found", 404), current.room);
    expect(current.roomRef.current).toBe(replacement);
    expect(current.signalRef.current).toBe(signal);
    expect(signal.stop).not.toHaveBeenCalled();
    expect(current.client.close).not.toHaveBeenCalled();
    expect(current.setNoticeValue).not.toHaveBeenCalled();
  });

  it.each(["INVALID_TOKEN", "HOST_ALREADY_CONNECTED", "room-closed", "terminated", "access-required"])(
    "keeps %s in the same status surface with and without a source", event => {
      const outcomes: unknown[] = [];
      for (const sharing of [false, true]) {
        const current = roomFixture();
        current.activeGenerationRef.current = sharing ? 1 : null;
        current.shareGenerationRef.current = sharing ? "share" : null;
        const signal = current.context.connectRoomSignal(current.room, undefined, false) as ReturnType<typeof current.connect>;
        if (event === "terminated") signal.events.onTerminated("SIGNAL_TERMINATED");
        else if (event === "access-required") signal.events.onAccessRequired();
        else signal.events.onMessage(event === "room-closed" ? { type: event } : { type: "error", code: event });
        expect(current.setNoticeValue).toHaveBeenCalledOnce();
        const notice = current.setNoticeValue.mock.calls[0][0];
        expect(notice).toMatchObject({ target: "television", tone: event === "room-closed" ? "off" : "bad" });
        outcomes.push({ text: notice.kind === "key" ? notice.key : notice.text, comic: notice.comic });
        expect(current.activeGenerationRef.current).toBeNull();
        expect(current.disposeResources).toHaveBeenCalledOnce();
      }
      expect(outcomes[0]).toEqual(outcomes[1]);
    },
  );

  it("checks the existing connection's authority before reusing it", () => {
    const current = roomFixture();
    const original = current.connect();
    const replacement = { ...current.room, roomId: "5678", hostToken: "new-token" };
    Object.assign(current.roomRef, { current: replacement });
    const next = current.connect(replacement);
    expect(original.stop).toHaveBeenCalledOnce();
    expect(next).not.toBe(original);
    expect(next.identity.roomId).toBe("5678");
  });

  it("retires only the matching rejected publication without sending an invalid stop", () => {
    const current = roomFixture();
    const signal = current.connect();
    signal.events.onMessage({ type: "sharing-start-failed", shareGeneration: "obsolete", code: "SERVER_ERROR" });
    expect(current.disposeResources).not.toHaveBeenCalled();
    signal.events.onMessage({ type: "sharing-start-failed", shareGeneration: "share", code: "SERVER_ERROR" });
    // The Host session is persistent; only the rejected publication retires.
    expect(current.disposeResources).toHaveBeenCalledExactlyOnceWith(false, true);
    expect(current.activeGenerationRef.current).toBeNull();
    expect(signal.stop).not.toHaveBeenCalled();
  });
});
