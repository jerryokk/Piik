# Modules And Boundaries

This file owns the repository map, crate responsibilities, interface discipline
and lifecycle ownership for the Rust core and the Web client. Product behavior
stays with [rooms](../standards/rooms-access.md),
[routing](../standards/routing-transport.md),
[media](../standards/media-quality.md) and
[presentation](../standards/presentation-lifecycle.md); wire constants in
[protocols](./protocols.md); process structure in [system](./system.md).
[AGENTS.md](../../AGENTS.md) owns authority and
[CONTRIBUTING.md](../../CONTRIBUTING.md) delivery.

## Repository Layout

Keep conventional entry points and tool-discovered configuration at the root.
`src/tsconfig.json` checks the Browser, `scripts/tsconfig.json` tooling and
tests, and root `tsconfig.json` connects them.

| Location | Contents |
| --- | --- |
| Root README, contribution and agent files | Introduction and shared constraints; detail lives in `docs/` |
| Root manifests, lockfiles, Vite/Vitest config, `index.html` | Cargo/npm entry points and Browser build/test startup; root `LICENSE` is authoritative |
| `src/client`, `src/shared`, `public/` | Browser orchestration, shared wire types and static assets; one UI serves Hosted and App |
| `src/client/ui` | React presentation: `components/` by room area, `pages/` route entries, `preview/` dev-only catalogue, `styles/` global layers |
| `src/client/{lib,media,webrtc,native,sfu,remote-control,room-voice}` | Browser runtime, grouped by owner: page state, media pipeline, peers, loopback control, SFU, input and voice |
| `crates/`, `web/dist` | Rust workspace; the embedded Vite output (only `.gitkeep` tracked) |
| `native/capture`, `native/fixtures`, `native/probes` | Windows capture, synthetic workloads and standalone probes |
| `scripts/`, `tests/` | Checks, gates, assembly and packaging; tooling tests and cross-language fixtures |
| `docs/`, `licenses/` | Guides, contracts and operations with their [owners](../standards/documentation.md); third-party notices |
| `.agents/`, `.codex/`, ignored output | Scoped agent integration; `target/`, `build/`, `node_modules/` and `coverage/` are never truth |

Move files with their consumers and update paths and checks together; a smaller
root alone does not justify another abstraction.

## Crate Map

The workspace has 33 crates: 32 shipped plus the dev-only `piik-media-testkit`.
Dependencies point one way, from foundation and contract crates toward
application composition; nothing reaches back into a caller, and both binaries
are leaves.

| Crates | Responsibility |
| --- | --- |
| `piik-ordered`, `piik-encoded`, `piik-failure`, `piik-diagnostics` | JavaScript `Map` order; encoded-video bounds and payloaders; App failure reasons; redacted logs and reports |
| `piik-protocol`, `piik-config`, `piik-app-config` | Wire contract and strict decode; Hosted/Local configuration and ICE discovery; strict App settings |
| `piik-room`, `piik-route` | Credentials, membership and SQLite authority; synchronous one-graph route decisions without timers or locks |
| `piik-media-forwarding`, `piik-media-session`, `piik-media-transport` | Sans-IO forwarding and layer selection; the `str0m` binding; sockets, shard driver, SDP policy and capture frame boundary |
| `piik-sfu`, `piik-signal`, `piik-service` | Embedded admission/media/voice; authenticated signaling and effects; shared HTTP service and listeners |
| `piik-input-route`, `piik-lan`, `piik-stun`, `piik-portmapping` | Opaque input relay; LAN invitation addresses; Binding-only STUN; gateway mapping |
| `piik-native-audio`, `piik-native-capture`, `piik-native-control` | Opus encoding of 48 kHz stereo PCM; capture supervision and v7 frames; one loopback v10 control session |
| `piik-media-edge`, `piik-native-host`, `piik-native-viewer`, `piik-remote-input` | Native edge engine; Host capture/audio/edges/publications; Viewer receivers and relays; Windows input |
| `piik-loopback`, `piik-launcher`, `piik-public-tunnel`, `piik-tls`, `piik-media-testkit` | Loopback discovery/health/control; launcher page and API; Quick Tunnel supervision; TLS; dev-only virtual clock and impaired network |
| `piik-app`, `piik-server` | Tauri shell with embedded runtime and ordered shutdown; Hosted entry point and composition |

`piik-route` returns resource effects and never executes them; `piik-signal`
executes them. The forwarding and session crates are sans-IO; their shard owner
supplies packets, time and estimates. `piik-native-control` defines media
traits and the host/viewer crates implement them, so native media never calls
back into room authority. Node and Vite are build tooling only.

## Contract Map

| Boundary | Schema and validation owners |
| --- | --- |
| Room HTTP and Browser/Server signaling | `src/shared/protocol.ts`, `piik-protocol`; HTTP dispatch in `piik-service`, WebSocket dispatch in `piik-signal` |
| Site access HTTP | `src/client/lib/api.ts`, `piik-service` (`access.rs`, `routes.rs`); behavior per [rooms](../standards/rooms-access.md) |
| Browser/App discovery and control | `src/client/native/wire.ts`, `piik-loopback` (`protocol.rs`), `piik-native-control` (`wire.rs`) |
| Remote input data channel | `src/client/remote-control/protocol.ts`, `piik-remote-input`, `piik-input-route`; admission stays with room and native-control owners |
| App/capture sidecar and producer | `piik-native-capture`, `native/capture/windows`, `piik-native-host` (`capture.rs`); probe, commands and envelope are distinct |
| Configuration and persistence | `piik-config`, `piik-app-config`, `piik-room` (`database.rs`); operator semantics in [configuration](../standards/configuration.md) |
| Build identity | Packagers inject `PIIK_RELEASE`/`PIIK_REVISION` into `crates/piik-server/src/main.rs`, `crates/piik-app/src/build_info.rs`, `crates/piik-loopback/src/buildinfo.rs`; see [protocols](./protocols.md) |

`tests/fixtures/wire-samples.json` feeds TypeScript and Rust fixture tests. It
covers room requests and signaling, not Native, capture or every HTTP route;
change both ends and the fixture together. Do not add a handwritten field list
in documentation, a parallel DTO hierarchy or schema generator without a
demonstrated reduction in ownership or drift.

Keep UI presets, requested values, capabilities, observations and hard limits
distinct. Every bound needs an owner and reason: wire representation, accepted
policy or resource protection; a menu or retry budget does not freeze values into
a wire reader. Validate representation at the interface, then supported
operations at their owner; an unsupported setting keeps the existing operation
rejection without retiring a valid session.

A capability description may exceed the reader's implementation. Intersect it
with local support and budgets; do not treat a stronger peer as absent or grant
resources from its declaration alone. Measurements are observations, not
requests; an unusable metric becomes unavailable without discarding valid
evidence or closing media control. Identity, generation, authority and required
state stay strict; never clamp a measurement into an invented fact.
Detect optional platform features through bounded checks and actual results, not
OS or browser allowlists, and keep availability, permission and success distinct.
Prove that a producer's valid aggregate output fits its consumer, including
serialization overhead. Extend a boundary only after exercising a new value, a
larger peer capability and an unchanged consumer, and checking rejection; for
media, include another source orientation and distinguish encode, decode and
forwarding support. Optional presentation additions need a bounded
unsupported-value policy separate from authority violations, following
[protocols](./protocols.md).

## Runtime Lifecycles

Read each runtime as acquisition, the authoritative commit that makes its work
visible, and the retirement that ends it. Before committing asynchronous results
or retiring state, revalidate the acquiring identity; presentation derives from
the committed fact.

| Runtime | Acquire, commit and retire |
| --- | --- |
| Browser Host share | `HostPage.tsx` captures one source and creates peers and route; the current generation plus owning identity (offer epoch, slot revision, switch and quality tokens) commits it; `disposeResources` stops tracks, disposes peers and sends `stop-sharing`, and a replaced source retires via `retiringStreamRef` |
| Browser Viewer route | `ViewerPage.tsx` binds one parent and relay children; parent, connection and message identity then playback facts commit it; a new viewer generation retires the binding, and children follow `activateChildren` revisions |
| Browser relay fanout | `viewer-relay.ts` prepares a child before it is needed; `activateChildren(revision, childPeerIds)` commits the set; replaced or emptied sets disconnect their children |
| Browser encoding pool | `host-peer.ts` attaches a carrier when capture and APIs support it; the binding owns its pooled producer while attached; source change or disposal releases it, and unsupported APIs keep senders |
| Browser/App control and room voice | `client-connection.ts` discovers lazily on request; one acquisition and exact `NativeClient` commit, with the page owning eligibility; release, retirement or App disconnect retires it. `room-voice/session.ts` binds the admitted session and endpoint; current membership and publication identity commit it; leave, retirement or session close releases owned audio, while closing a window does not leave voice |
| Floating room tool | `FloatingPanel.tsx` and `panel-window.ts` open a supported window; that request and document own presentation, not the session; close returns the tool, and opener retirement closes a late open |
| App native session | loopback admits control sessions and the page selects the native path; session identity plus the media-path choice commit it; session end, share end or exit retires the edge and its sidecar |
| Native ICE gathering | one gathering observes addresses on the retained socket; candidates retain their source ICE generation; ICE restart retires prior observations, and connection close retires the socket |
| Server room authority | `piik-signal` authenticates one command; `piik-room` writes durably before the memory commit under one serializing lock; replacement, deletion, grant rotation/revocation or exit ends it |
| Server route and SFU | `piik-signal` feeds `piik-route` one serial operation and `piik-sfu` admits one publication; one graph per controller and the forwarding publication commit them; superseded assignments retire in that operation, and deactivate retires the publication |

## Source Index

Start at the owning entry point, then follow the interfaces above instead of
searching the tree. NAT policy stays in [routing](../standards/routing-transport.md);
visual rules in [visual language](../standards/visual-language.md).

| Change | Start at |
| --- | --- |
| Host or Viewer workflow, playback, status | [HostPage.tsx](../../src/client/pages/HostPage.tsx), [ViewerPage.tsx](../../src/client/pages/ViewerPage.tsx), [viewer-presentation.ts](../../src/client/media/viewer-presentation.ts), [media-status.ts](../../src/client/ui/media-status.ts), [viewer-quality-evidence.ts](../../src/client/media/viewer-quality-evidence.ts) |
| Browser peer, relay and route mechanics | [host-peer.ts](../../src/client/webrtc/host-peer/index.ts), [viewer-peer.ts](../../src/client/webrtc/viewer-peer.ts), [viewer-relay.ts](../../src/client/webrtc/viewer-relay.ts), [route-transition.ts](../../src/client/media/route-transition.ts), [operation.rs](../../crates/piik-route/src/operation.rs), [piik-route](../../crates/piik-route) |
| Capture, profiles, sender parameters | [quality.ts](../../src/client/media/quality.ts), [browser-encoding-pool.ts](../../src/client/media/browser-encoding-pool.ts), [browser-encoding-output.ts](../../src/client/media/browser-encoding-output.ts) |
| SFU and native media | [publisher.ts](../../src/client/sfu/publisher.ts), [peer.ts](../../src/client/sfu/peer.ts), [native](../../src/client/native), [piik-sfu](../../crates/piik-sfu) |
| Native control and copy | [piik-loopback](../../crates/piik-loopback), [piik-native-control](../../crates/piik-native-control), [piik-native-host](../../crates/piik-native-host), [locales](../../src/client/locales), [media-failure.ts](../../src/client/ui/media-failure.ts) |
| Signaling, interactions, shared wire | [signaling.ts](../../src/client/lib/signaling/index.ts), [room-interactions.ts](../../src/client/lib/room/interactions.ts), [protocol.ts](../../src/shared/protocol.ts), [piik-protocol](../../crates/piik-protocol) |
| Diagnostics, ICE and NAT | [debug.ts](../../src/client/lib/debug/index.ts), [conn_socket.rs](../../crates/piik-media-transport/src/conn_socket.rs), [nat-prediction.ts](../../src/client/webrtc/nat-prediction.ts), [piik-portmapping](../../crates/piik-portmapping) |
| Voice, remote input and interactions | [session.ts](../../src/client/room-voice/session.ts), [input-session.ts](../../src/client/remote-control/input-session.ts), [RoomInteractions.tsx](../../src/client/ui/components/room/RoomInteractions.tsx) |
| Visual primitives and preview | [palette.css](../../src/client/ui/styles/palette.css), [primitives.tsx](../../src/client/ui/components/shell/primitives.tsx), [ControlsPreview.tsx](../../src/client/pages/preview/ControlsPreview.tsx) |

## Implementation Rules

- Extract a complete responsibility for its real consumers, keeping acquisition,
  replacement and retirement together. File size alone does not justify wrappers,
  a generic manager or a framework; remove superseded paths in the same change.
  One platform adapter may suffice if its boundary and consumers justify it.
- Keep simple logic at its call site; extract a helper for real reuse, a
  substantial boundary, or a concept that clarifies the caller. Keep long
  orchestration readable; avoid single-use wrappers or arbitrary size limits.
- A production file stays within 800 lines and an independent test file within
  1000; `scripts/checks/check-source-size.mjs` enforces this at integration. Two session
  owners are documented exceptions because their shared closures are one state
  machine. When a file grows past the limit, split by responsibility rather than
  extending it.
- Review dependency direction for source and type imports, not only runtime
  imports. Shared types belong to a common owner; a type-only cycle is an
  ownership concern, not a runtime defect.
- Group by the owner a file serves, not the kind of code it is: a component
  joins the room area it presents, and a module with a lifecycle and several
  files becomes a folder with a one-line charter. A stylesheet a component
  imports sits beside it; a stylesheet only the entry imports is a global layer.
  A public barrel is the folder's `index.ts`, never `x.ts` beside `x/`.
- Rust modules follow the same rule: `impl` fragments for one type live in that
  type's folder (`router/pump.rs`, `server/close.rs`), not as `router_pump`.
  Tests are `<module>/tests.rs` for one module, `src/tests.rs` with `src/tests/`
  for a crate, and `tests/` for the public API. `#[path]` stays only where
  Rust's module resolution requires it — integration-test submodules.
- State has one writer and a clear lifetime. Separate permission, capability,
  readiness, requested and applied settings, and observation. Derive
  presentation from facts; do not synchronize parallel booleans or let a cached
  failure outlive its evidence. Selecting a runtime mode is not an edit to saved
  preferences; compare CLI, launcher and page writes against one user intent.
  Cumulative observations need a valid baseline for their generation; missing
  measurements are not zero. Serialized recovery reads current committed intent
  when it executes, not a queued snapshot.
- The operation that acquires a connection, clone, listener or queue owns its
  retirement. After an await or callback, and in cleanup, validate the original
  operation and resource identity before changing state. Cancellation, failure,
  missing observation and success keep distinct meanings.
  - Queued events keep physical-owner validity until the final serialized write;
    readiness commits also need the resource's current state. Reused logical IDs
    do not identify a replacement instance.
  - A child's attachment failure does not retire its shared producer. Replacing
    an exclusive subscription waits for the owner's retirement, and repeated
    cleanup callers await the same work; for shared leases, the owning controller
    confirms the replacement before releasing a healthy lease.
  - Closing a Browser connection may leave its promises pending; its owner
    cancels those waits so a serialized successor never depends on a retired
    connection. Late results still need identity checks.
  - Failure effects need the final identity check as successful results,
    serialized with replacement; later cancellation must not erase an accepted
    failure. Blocking retirement runs after releasing the state lock.
  - A caller deadline does not cancel a dependency that ignores it. Keep late
    work bounded, serialize its cleanup and verify the dependency's lifetime.
  - Construction is acquisition: if it throws, release resources created before
    the failure. Initialize callback-visible state before registration; library
    constructors can start background work.
  - Ordered shutdown may require a ready auxiliary transport to outlive the stop
    request; its owner closes it after dependants retire. Convenience actions
    must not block lifecycle handling, and retries stay bounded.
- Keep decisions separate from effects; respect lock ordering and revalidate
  after unlocked work. Local native capability never grants room authority; the
  shared Rust service stays authoritative. Injected callbacks run in their
  caller's context, and synchronous authority hooks must not hide blocking I/O
  or release the caller's lock; schedule slow work at an owned effect boundary.
- Validate and normalize unknown input once at each owning trust boundary, then
  pass typed values inward. Do not repeat checks guaranteed by a schema,
  constructor or internal contract. Authorization and resource identity stay
  checked where they can change.
  - Keep domain error meanings stable and map them into shared localized UI.
    [Protocols](./protocols.md) determines compatible extensions; an optional
    field is not automatically compatible.
  - Dispatch separates protocol rejection, per-request operation failure and an
    already-satisfied teardown. Late teardown is idempotent and cannot retire a
    replacement; validate strictly before looking up its target.
  - Relayed content uses the same units and limits at each boundary; its failure
    cannot invalidate a valid local control session. Slow capture mutations share
    one cancellable slot while control stays responsive. Bounded queues apply
    backpressure rather than failing a valid burst.
  - Retained media state carries failure facts; presentation resolves localized
    copy during render and raw exceptions stay in diagnostics. Transient event
    notices may resolve once when the event occurs.
  - Values with a canonical form, such as HTTP origins, use one shared normalizer
    before storage and comparison. Verify producer and consumer together instead
    of weakening an authorization comparison.
- Deployment configuration describes available services; room preferences
  request allowed behavior. The server enforces policy even when a client asks
  for more; the UI keeps the controls and explains locked choices. See
  [configuration](../standards/configuration.md); avoid a second flag with the
  same job.
- Browser, App and Server share [visual language](../standards/visual-language.md)
  and [status projection](../standards/media-status.md); new pages and locales
  invent no palette, status model or role metaphor.
- Comment on non-obvious reasons, constraints and surprising behavior, not
  assignments or control flow. Package comments describe current responsibilities
  and invariants; history belongs in Git.
- New text files use LF and ASCII filenames (exceptions in `.gitattributes`);
  scripts use stable paths, and physical confidence stays in
  [verification status](../verification-status.md).
- Update generated files through their generator, never by hand. Retained tools
  declare their direct dependencies and join the normal type-check entry point;
  shipped templates use reserved example values. Checks provision isolated
  resources or require explicit targets, never a maintainer's workstation.

## Contextual Consistency

Repeated meaning has one owned contract across code, interaction, wording,
illustration, diagnostics and documentation; shared components can still receive
conflicting caller policies. Locate related producers, consumers and the existing
[owner](../standards/documentation.md#owners), then make its contract explicit:

- **Meaning and evidence:** the subject, fact or action; who may assert it; its
  scope and generation; how unknown differs from failure or success.
- **Context and priority:** where it applies and does not, which user action it
  supports, and which information takes precedence.
- **Lifetime:** acquisition, updates, replacement, cancellation, recovery and
  retirement; visibility or persistence when relevant.
- **Representations:** permitted copy, icons, motion and feedback surfaces;
  truthful platform adapters; shared implementation and validation owners.

New shared rules need repeated callers or a public contract, a reason and
proportionate evidence; retire replaced rules and consumers together. Equal
wording or appearance does not prove equal semantics: compare authority, scope,
evidence, lifetime and intended outcome before merging paths. Signaling
readiness and visible media, requested and applied settings, and a retired
operation versus its replacement must stay distinguishable. Runtime and layout
adaptations may differ while preserving the contract; explain such differences
at their owner, not in caller-specific flags. UI control and surface selection
belong to the [visual language](../standards/visual-language.md#control-selection).

## Ablation And Validation

Test production implementations through their real construction paths. Replace
external I/O or platform boundaries only where a focused test needs it; keep the
tested behavior real instead of copying its algorithm into the test. Keep
test-only setup in the harness, and do not export mutable resources solely for
tests. Defaults and restoration use the production path.

Judge a change by verified user value against implementation, maintenance,
compatibility, runtime and failure cost. After a material module, remove state,
dependencies, branches or abstractions the result does not need; never reduce
quality or functionality to save lines. Fewer duplicated writers or edit sites
can justify refactoring without a reproduced failure; name that gain and its
risk. An audit is bounded evidence, not a claim that every boundary is optimal.

Before changing recovery or state, trace the real producers and their guards. A
test that bypasses disabled controls or writes private state does not alone
establish a reachable defect. Reference scans, including test and preview hits,
are leads rather than proof of use or absence; follow the actual
producer-to-consumer path, since isolated helper tests can miss a mismatch.

For a shared-resource failure, review the failing child and a healthy sibling
through the real producer/consumer boundary; for overlapping connections,
preserve the actual shared local and remote endpoints, since independent
fixtures can hide demultiplexing conflicts:

- Compare preparation with live operation; exercise optional failure after
  success and a dependency that stays pending after cancellation, since a
  timeout constant or a close call alone proves no deadline.
- Keep the shared resources production retains while changing the external
  condition; new IDs alone do not prove that cached observations, failure state
  or a lease were refreshed.
- For media adaptation, include partial capacity restoration on the retained
  connection, not only recovery to a fast link. Check capture, encoded output,
  actual send and remote decode separately; a dependency's algorithm does not
  prove its adapter preserves pacing or ownership.
- Check commit and rollback while the surviving media still flows.

Keep the regression at the owning boundary and verify it rejects the old
behavior. Report which failure sequences were exercised rather than treating
test counts, file coverage or an earlier audit as proof of lifecycle coverage.
Trace acquisition, use, commit and retirement, including replacement and failure:
check A starting, B replacing it, then A completing or cleaning up. Because every
surface ships from one revision, ordinary private changes need no stale-client
support. Run proportionate checks through
[standard entry points](../../CONTRIBUTING.md#verification-entrypoints); this
discipline does not authorize an unrelated rewrite.
