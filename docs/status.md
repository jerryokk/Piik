# Current Status

Last updated: 2026-11-10

This is the compact execution/deployment index. Product modules own behavior,
[verification status](./verification-status.md) summarizes material evidence limits,
and Git/PRs own completed history.

## Release Contract

- One build only: App, Server and the Web client ship together from one source
  revision, with Browser/server, Native control and capture identifiers that
  reject a mismatched peer. SQLite schema 2 is current.
  [Versioning](./standards/versioning.md) owns identity and interface changes.
- Hosted Server defaults to SQLite schema 2. Room authority has no inactivity
  expiry by default; replacement/deletion or grant rotation/revocation ends the
  corresponding authority. Explicit memory mode and App Local end rooms at
  process exit. Site access retains its separate 24-hour idle lifetime within
  one service run; service restart requires site-password entry again.
- Camera sharing and room voice are product features. Source audio follows the
  picture; room voice owns conversation microphones under the same route
  controller. Optional Host commentary belongs to the share's audio and has
  separate activation. Device selection and sharing settings keep their owners;
  [media quality](./standards/media-quality.md) owns platform behavior and limits.

## Interaction Phase

Text chat, reactions, room voice and attended remote control are product
features. They use existing room authority and the shared route controller;
picture, voice and input grants retain their separate lifetimes.
[Rooms and access](./standards/rooms-access.md) owns their behavior and
[architecture](./architecture/rooms.md) the design reasons.

## Current Work

The system is the Rust workspace with the Tauri Windows App; the Web client,
Server and App share one core ([architecture](./architecture/system.md)).
[TODO](./todo.md) is the only unfinished-work ledger and owns every open item;
[verification status](./verification-status.md) owns measured acceptance and
platform boundaries.

Supported clients are the Windows x64 App Host and Browser Viewers. Room voice, shared
keyboard/mouse and optional Windows virtual gamepads use the normal App/Web
interfaces. App packages contain no optional input SDK or driver.
[Rooms and access](./standards/rooms-access.md) owns the input contract.

App Local invitations use HTTPS on the selected LAN address, with a loopback
entry for the Host. Hosted Server supports direct HTTPS with temporary or
operator-supplied certificates. The [HTTPS runbook](./operations/self-hosting.md)
owns setup. Virtual LANs use the existing room and UDP media paths.

Optional coordinated Native UDP preparation augments the existing retry owner;
[signaling and routing](./architecture/routing.md) owns its behavior. Controlled
results do not establish a public success rate, and Magicsock integration is not
planned.

[Media quality](./standards/media-quality.md) owns bounded output and
live-control behavior. Additional codecs, surround delivery and end-to-end HDR
remain out of scope.

## Deployment

The immutable release descriptor, runtime `REVISION` and operator deployment
record own exact identity and postflight results.

Server and App packages are Windows x64 only. The Server runs one Rust process for
Web, room authority/signaling, STUN and optional SFU behind an operator-supplied
HTTPS proxy or its own TLS listener. The
[self-hosting runbook](./operations/self-hosting.md) owns listeners and network
configuration.

There is no hosted release channel: the person providing Piik distributes the
Server and App packages directly. [Versioning](./standards/versioning.md#shipping)
owns release identity and compatibility.
