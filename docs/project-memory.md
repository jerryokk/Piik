# Project Memory

Last updated: 2026-11-10

Piik is private, low-latency screen sharing for one Host and up to 20
authenticated friends. The current product surface is the Windows x64 App Host,
Web Viewers and Browser relay; desktop and mobile Browsers are Viewer targets.
The Host always shares from the App window, whose source picker offers native
capture and the WebView's own Browser capture. It is not a public broadcast
service. Hosted and App run one shared Rust core
([architecture](./architecture/system.md)); Node and Vite build the Browser UI and
are not a runtime. The App runs that core as a self-contained
Local deployment or supplies native capability to explicitly approved Sites
through its WebView2 window, starting its loopback capability service before room
selection. Its launcher offers Local and temporary public-link choices and
manages per-Site approval and one starred startup Site. Site use can stay on
standby without a Local room service; native media still belongs to the
participant's Web UI.
Room source and media implementation are independent: a topology may mix
Browser and Native peers without changing participant, signaling, capacity, or
route identity. Local is LAN-first; explicit one-link mode exposes that same
authority through a temporary public control tunnel while media remains P2P.
Host authority belongs to a tab, with one separate persistent origin resume
hint. The App admits two independent native control sessions under its one
loopback service.

The Hosted Rust process embeds Binding-only STUN and optional SFU forwarding.
Native and SFU use one shared str0m media layer. Native parents reuse
encoded outputs and derive missing lower outputs from direct-child demand;
adaptation preserves the source and higher sibling outputs. Eligible Browser
capture and relay share local WebRTC producers ([media pipeline](./architecture/media.md)), with ordinary
senders as fallback. Platform producers share the encoded-frame boundary and
Host quality controls. Source replacement preserves route and connection identity.

## Product Map

- [Rooms and access](./standards/rooms-access.md) owns room codes,
  invitations, code entry, credentials, room interactions and lightweight/SQLite
  persistence.
- [Routing and transport](./standards/routing-transport.md) owns P2P-first
  distribution, endpoint capacity, SFU fallback, recovery, and privacy limits.
- [Capture and media quality](./standards/media-quality.md) owns Browser capture,
  profiles, codec selection, audio, and framework adaptation.
- [Presentation and lifecycle](./standards/presentation-lifecycle.md) owns Host and
  Viewer workflows, playback authority, roster, state, and Browser lifecycle.

[Architecture](./architecture/README.md) owns system structure, protocols and the
reasons behind them; [verification status](./verification-status.md) owns measured
limits. [Module map and source index](./architecture/modules.md#source-index)
locate implementation owners and their module, interface and lifecycle boundaries.

## Core Invariants

- Rooms use random four-digit codes, exact Host ownership, room-scoped Viewer
  invitations and independent `open | private` code entry. Room authority ends
  through replacement/deletion, with separate invitation rotation or revocation.
  Hosted operators may opt into empty-room expiry and capacity reclamation; the
  default retains rooms indefinitely. Hosted defaults to SQLite, with explicit
  process-memory opt-out; App Local remains process-only, with an optional
  site-access password. Site access has a separate 24-hour idle cookie lifetime
  within one service run; live participants, routes, and media are process-only.
- Media is automatic and distributed. Ordinary peers are STUN-only and prefer
  direct/peer UDP. A deployment may enable same-host auxiliary STUN for bounded,
  room-wide, connection-local NAT prediction; current pages use it automatically
  when available, and anonymous provenance distinguishes ordinary from predicted
  selected paths. The configuration default remains stock ICE. The only
  application fallback is one bounded embedded SFU/UDP
  publication; Piik configures no TURN or TCP media route. Hosted operators may
  require SFU-only media through the same controller. On P2P-first sites, a
  pre-share peer-only policy can exclude that SFU suffix for one share generation.
- Ordinary endpoints share one steady outbound-copy cap, default `2` and limited to
  `1..3`; the explicit relay volunteer uses the
  [capacity owner](./standards/routing-transport.md#endpoint-and-server-capacity).
  One room controller owns one committed graph, one reconcile loop, and
  one serial child operation. First decoded frame commits picture availability work;
  accepted quality work adds native edge proof before commit in that operation.
- Browser WebRTC and the shared str0m media layer own congestion control,
  media adaptation, pacing and layer selection. Parent demand is local to direct
  children and never a room-wide minimum. The
  [routing owner](./standards/routing-transport.md) owns convergence, repair and
  the rejected alternatives.
- Browser video uses `motion`; each share chooses H.264 with VP8 fallback through
  an actual sender probe unless the Host explicitly selects VP8 or H264. Screen
  audio retains its compact presets and 128 kbps default, separate from numeric
  bitrate requests; the media-quality owner holds limits and admission.
- Viewer playback uses one native video element. Presentation and recovery are
  fenced by current route/media identity and current-frame proof; hidden-page
  time alone is not media-failure evidence or a keepalive guarantee.

## Current Snapshot

Room voice shares room authority and one route controller with picture sharing.
Picture generations and explicitly joined voice publications have separate
lifetimes; optional source commentary belongs to the share, independent of room
voice. [Room media](./standards/routing-transport.md#room-media) owns them.

Optional attended input follows the committed media route back to the native
Host. One grant covers keyboard/mouse and an optional gamepad. Control starts
with one grantee; the Host may allow simultaneous Viewers for that source.
Source change, stop and revocation retire the affected grants; input adds no
separate connection planner or listener. Viewer playback preferences and the
combined interaction window remain local presentation owners.

[Status](./status.md#release-contract) owns the release contract; embedded
SFU SDP/ICE and demand use the authenticated room WebSocket.
[Deployment](./operations/release.md) owns rollback with matching Browser and App artifacts.

[TODO](./todo.md) owns unfinished work, [status](./status.md) delivery and
[verification status](./verification-status.md) evidence boundaries;
[configuration](./standards/configuration.md) and [self-hosting](./operations/self-hosting.md)
own environment and services. Release descriptors, deployment records, Git and
pull requests own deployed identity and completed history.
