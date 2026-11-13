# Rooms And Access

This file owns the room model: what a room is, who may enter it, what survives a
restart, and where each piece of authority lives. User-visible rules stay in
[rooms and access](../standards/rooms-access.md); the media graph that a room
hosts is in [signaling and routing](./routing.md).

## One Aggregate

A room is a small authorization aggregate owned by the Server:

- one free random code from `1000` through `9999`;
- a SHA-256 digest of the Host token;
- a SHA-256 digest of the Viewer grant;
- an opaque authorization generation;
- `open | private` code entry and optional salted scrypt password material.

Raw tokens, grants and passwords are never stored. The room has no time limit by
default: explicit replacement or deletion ends its authority, while grant
rotation or revocation retires an invitation independently of the room. An
operator may opt into empty-room reclamation, which keeps abandoned allocations
from exhausting the 9,000-code space; occupancy, including idle chat, protects a
room and the signaling heartbeat owns expiry. There is no client lease, renewal
timer or second expiry path.

The aggregate is deliberately small. Participants, display names, session and
peer identities, the share generation, pause state, quality settings, codec
decisions, the route graph, pending operations, first-frame proof, SFU
publications and diagnostics are process-only. Persisting them would make a
restart look seamless, which it is not: a restart re-authenticates and rebuilds
media on fresh decoded frames.

## Storage Modes

Hosted Server uses one SQLite file by default; an explicit absolute
`ROOM_DATABASE_PATH` selects another, and `:memory:` selects process-only rooms
whose authority ends with the process. App Local always uses the memory
composition and does not inherit Hosted defaults.

SQLite stores exactly the aggregate above, in one atomic row, with one exact
schema version, one connection and one writer. An unknown schema, wrong
application identity, corrupt row, inaccessible path or second owner fails
startup before any signaling or SFU resource changes. There is no migration
reader: a schema change is a release change that updates the schema and its
writer together.

A restart restores every saved room and nothing else. Host tabs keep their raw
Host token in session storage, so a surviving tab reclaims ownership; the server
never trusts a client fingerprint as identity. Clearing browser data or moving to
another device loses ownership, which is the intended boundary.

The Browser also keeps a Host room in localStorage so a reload can resume it
against a Hosted Server. The App does not: its rooms end with the process, so a
persisted copy could only restore a room the server has never heard of, which
would surface as an expired room on a launch the person never interrupted. The
App therefore writes only the session copy, whose lifetime matches the window.

## Admission

Three independent decisions decide what a browser may do:

1. **Site access** admits a browser to the Piik origin at all. It uses one
   operator password and a rolling 24-hour idle cookie within one service run; a
   restart requires the password again.
2. **Room access** admits a Viewer to one room, by a valid Viewer grant or by
   code entry. `open` accepts code entry; `private` disables passwordless entry
   and, with a password, additionally accepts a matching code-and-password
   attempt. A missing or deleted code returns `ROOM_NOT_FOUND`; denial of an
   existing room returns `ROOM_ACCESS_DENIED`.
3. **Interaction grants** admit a Viewer to picture, voice and input separately.

Each lifetime belongs to its owner. A picture grant follows its publication, a
voice grant follows the joined voice session, and an input grant ends with
source change, stop or explicit revocation. Admission never grants media by
itself.

## Invitations And Replacement

An invitation is a 128-bit, 22-character base64url Viewer grant carried in the
URL fragment for exactly the current room. The browser consumes it into
room-scoped session storage and removes the fragment, so it does not reappear in
history or in a shared address. Rotating or revoking updates the digest and the
authorization generation atomically, which also invalidates any grant issued
before the change.

The Host's room control can replace the current room. With the exact Host token,
one operation allocates a guaranteed different free code and deletes the old room
through the normal path. Failure before commit leaves the old room untouched;
success invalidates the old token, grant, password, authorization generation,
sessions, routes and SFU resources, and ends any active share. No old state is
migrated into the new room.

The browser may remember one non-secret preferred code as a future allocation
hint. The server remains the only owner of room existence and code allocation; it
reuses the hint only when that code is free and otherwise allocates a random one.

## Room Voice And Interactions

Room voice, text chat, reactions and remote control share the room's authority
and its single route controller, but keep separate grants. Picture generations
and explicitly joined voice publications have independent lifetimes; ending one
does not end the other. Optional Host commentary belongs to the share's audio and
is independent of room voice.

Their delivery bounds are fixed. The Server accepts at most one interaction per
800 ms per connection, skips an interaction once 16 KiB of output is queued, and
caps chat at 280 Unicode code points normalized to NFC. The page keeps the latest
1,000 received messages, shows at most eight reaction effects, and expires each
after 2.4 seconds.

All of them run on the assembled Web, Windows App and embedded clients; there is
no separate interaction service, connection planner or listener. Their user-facing
behavior and the input contract are owned by [rooms and access](../standards/rooms-access.md).
