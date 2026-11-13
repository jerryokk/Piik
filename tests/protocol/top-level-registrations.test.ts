import { expect, it } from "vitest";
import { clientMessageSchema, serverMessageSchema } from "../../src/shared/protocol.js";

it("keeps audio availability nullable and generation-scoped for updates", () => {
  for (const available of [false, true, null]) {
    expect(serverMessageSchema.safeParse({ type: "audio-presence", available }).success).toBe(true);
    expect(clientMessageSchema.safeParse({ type: "set-audio-presence", shareGeneration: "audio_share_generation", available }).success)
      .toBe(available !== null);
  }
  expect(serverMessageSchema.safeParse({ type: "audio-presence" }).success).toBe(false);
  expect(clientMessageSchema.safeParse({ type: "set-audio-presence", shareGeneration: "bad", available: true }).success).toBe(false);
});

it("keeps one registered reaction set for commands and events", () => {
  const event = { type: "room-interaction", id: "event_future_1234", requestId: "request_future_1234", occurredAt: 1,
    sender: { peerId: "sender_peer_1234", role: "viewer", displayName: "Friend" },
    payload: { kind: "reaction", reaction: "wave", targetPeerId: "target_peer_1234" } };
  expect(serverMessageSchema.safeParse(event).success).toBe(true);
  expect(serverMessageSchema.safeParse({ ...event, payload: { ...event.payload, reaction: "future-effect" } }).success).toBe(false);
  for (const role of ["host", "viewer", "", "HOST", "admin"]) {
    expect(serverMessageSchema.safeParse({ ...event, sender: { ...event.sender, role } }).success)
      .toBe(role === "host" || role === "viewer");
  }
  expect(clientMessageSchema.safeParse({ type: "send-room-interaction", requestId: event.requestId, payload: event.payload }).success).toBe(true);
  expect(clientMessageSchema.safeParse({ type: "send-room-interaction", requestId: event.requestId,
    payload: { kind: "reaction", reaction: "future-effect" } }).success).toBe(false);
  for (const payload of [
    { kind: "reaction", reaction: "" }, { kind: "reaction", reaction: "a".repeat(33) },
    { kind: "reaction", reaction: "future effect" }, { kind: "reaction", reaction: "👋" },
    { kind: "reaction", reaction: "future-effect", targetPeerId: "bad" },
    { kind: "reaction", reaction: "future-effect", text: "hidden command" },
    { kind: "reaction", reaction: "tomato" }, { kind: "future-kind", reaction: "wave" },
  ]) expect(serverMessageSchema.safeParse({ ...event, payload }).success).toBe(false);
  expect(serverMessageSchema.safeParse({ ...event, sender: { ...event.sender, peerId: "bad" } }).success).toBe(false);
});
