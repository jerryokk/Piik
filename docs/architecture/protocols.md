# Wire Protocols And Build Identity

Piik has one App, one Server and one Web client, always shipped together from
one revision. Every protocol below is therefore an exact match between the parts
of that revision, not a family of versions. This file owns each constant, its
rejection behavior and the change rule. Process structure stays in
[system](./system.md), module ownership in [modules](./modules.md), and room,
routing and media behavior in [standards](../standards/README.md).

## One Version Policy

- Every boundary has exactly one accepted value for its current protocol.
- A peer that presents any other value is rejected at that boundary; Piik never
  negotiates a common version, downgrades, upgrades on the client's behalf or
  falls back to an older message shape.
- There is no multi-version reader, no compatibility alias and no legacy path.
  A stale App, page, helper or database is an unsupported combination.
- Numbers that survive in names, envelopes or storage are identities of the
  current contract, not a range. They are not release versions and do not order
  releases.

### Change Rule

One protocol change is one coherent change:

1. update the single constant at its owner;
2. update the producer and consumer on both sides in the same change;
3. update the conformance fixture or contract test that pins the shape;
4. ship that change in one release of App and Server together.

A protocol change is never staged across two releases, never gated on an older
peer's capability, and never accompanied by an alias for the previous spelling.
An unchanged constant does not promise that every field is unchanged: the
strict decoders below reject an added or removed key, so a field change is a
protocol change even when the constant still reads the same.

## Boundary Summary

| Boundary | Current contract | Owner |
| --- | --- | --- |
| Browser/Server signaling and room HTTP | `piik-v24` | `src/shared/protocol.ts`, `crates/piik-protocol` |
| Browser to App native control | protocol 10, service `piik-client` | `crates/piik-loopback`, `src/client/native/wire.ts` |
| App to Windows capture helper | capture protocol v7 | `crates/piik-native-capture`, `native/capture/windows` |
| Server room persistence | SQLite schema 2 | `crates/piik-room` |
| Build identity | `PIIK_RELEASE` plus `PIIK_REVISION` | Packagers and each binary's build-info module |
| Remote input data channel | `piik-remote-input-prototype-v1` label | `crates/piik-input-route` |
| Release descriptor | schema 2 | `scripts/release/package-server-release.mjs` |

The last two are matched-bundle details, but they follow the same exact-match
rule: a channel with another label is ignored, and a descriptor with another
schema is not read as the current one.

## Signaling

`SIGNALING_PROTOCOL = "piik-v24"` is exported through the Browser wire barrel
`src/shared/protocol.ts` and mirrored as `piik_protocol::SIGNALING_PROTOCOL` in
`crates/piik-protocol/src/constants.rs`. The two are maintained together; the
Web value is importable at build time, and the Rust value is the server's
authority.

- The `authenticate` client message carries `protocol`, and the server's
  `authenticated` reply echoes it. Both sides use a literal schema, so any other
  string is a shape error, not a recognised older client.
- Client messages decode strictly in `piik-protocol`, with the same key order
  and omission rules as the TypeScript schemas. Unknown keys, missing required
  keys and unknown message types are rejected. `current_protocol` is the single
  check that the `protocol` member is `piik-v24`.
- On the server, a message that fails decoding produces the shared
  `InvalidMessage` error and closes the session with WebSocket code `1008`
  (policy violation). Authentication failures use `4003`; replaced sessions use
  `4001`. None of these codes invites a retry with another protocol.
- The Web client opens `GET /signal` on the page's own origin and sends
  `SIGNALING_PROTOCOL`; it never probes for a compatible generation.
- `MAX_SIGNAL_BYTES` (64 KiB) bounds one signaling frame; larger frames are
  rejected rather than parsed.

`/api/capabilities` carries only deployment facts — `sfu`, `sfuOnly` and
`natPrediction` — and decodes strictly like every other document: an unexpected
key fails the decode rather than being skipped. A page that cannot read it keeps
working without the optional deployment features instead of failing to open;
everything it needs to host or view is present in every build.

`tests/fixtures/wire-samples.json` is the cross-language fixture, consumed by
`crates/piik-protocol/tests/fixture.rs` and `tests/protocol-fixture.test.ts`.
`crates/piik-protocol/tests/contract.rs` and the `tests/protocol/` suite pin exact
messages and rejections, including the literal `piik-v24` value.

## Native Control

The App's loopback service uses protocol `10`:

| Constant | Value | Meaning |
| --- | --- | --- |
| `PROTOCOL_VERSION` | `10` | Numeric control protocol |
| `SERVICE_NAME` | `piik-client` | Discovery service name |
| `CONTROL_SUBPROTOCOL` | `piik-client-v10` | WebSocket subprotocol prefix |
| port range | `39721..=39730` | Loopback listener candidates |

All four are declared in `crates/piik-loopback/src/protocol.rs`; the Web mirror
is `NATIVE_CLIENT_PROTOCOL` and `NATIVE_CLIENT_SUBPROTOCOL` in
`src/client/native/wire.ts` with ports in `src/client/native/discovery-ports.ts`.

- `/connect-discovery` reports `protocol: 10` and the App instance token. It is
  the only endpoint open before local approval.
- The Web client scans the port range, accepts an identity only when the exact
  port answered and the service name matches, and throws
  `NativeCompatibilityError` when the reported protocol differs from `10`. It
  does not retry with another protocol and does not use an incompatible App at
  all; the source picker offers ordinary Browser operation.
- `/health` repeats the protocol with build identity and native capability bits.
- The `/control` WebSocket requires the subprotocol
  `piik-client-v10.<instance-token>`. A socket that did not negotiate it is
  closed with `unsupported control protocol` before any session starts.
- Every control envelope carries `version: 10`. `decode_envelope` rejects any
  other number, an invalid request ID, or an empty type. Extensions decode their
  own fields from the same envelope.
- `MAX_CONTROL_MESSAGE_BYTES` (256 KiB) bounds one control message; the loopback
  service admits at most two concurrent control sessions.

Native control is an independent boundary from signaling: an installed App and a
Site update together only in the shipped product, but the loopback contract can
be exercised by an approved Site that did not build the App. A mismatch there
therefore has a user-visible failure and guidance, not a silent fallback.

## Capture Helper

The Windows capture helper is contract v7. Three shapes carry that name:

- **Probe.** `piik-capture.exe --probe` prints JSON with `"protocol": 7`,
  `platform`, adapter and encoder identity. `crates/piik-native-capture`
  requires protocol `7`, platform `windows`, valid identities and bounded
  adapter and encoder counts; anything else is an invalid contract. A failed
  probe disables native capture for that App run.
- **Command line.** Encoded and video modes require the literal
  `--protocol-v7` argument, and the helper's argument parser rejects a command
  line without it.
- **Stream.** Frames use the `SMED` envelope: a 32-byte big-endian header whose
  version byte is `2`, with kind, flags, layer, timestamp and duration in
  100 ns units, dimensions and payload size. `read_frame` rejects an unknown
  magic, a wrong envelope byte, a size outside the kind's bound, an inconsistent
  begin/end/status shape or invalid dimensions.

The v7 name and the envelope byte are two parts of one matched bundle: the App
supervises a helper built beside it, and there is no path that accepts a helper
from another contract generation. The Rust supervisor writes and reads the
contract; the C++ helper in `native/capture/windows` implements it.

## Room Schema

`crates/piik-room/src/database.rs` owns the durable room authority:

| Identity | Value |
| --- | --- |
| `PRAGMA application_id` | `1396920910` (`SCRN` in ASCII) |
| `PRAGMA user_version` | `2` |
| Objects | exactly one `rooms` table |
| Table mode | `STRICT`, exact columns |

Adoption checks identity before reading any row. A blank database is initialised
with the current schema. Any other database must match the application ID, the
schema version, the single object and the exact column set; a mismatch fails
closed and startup reports it before traffic is accepted. There is no migration
path, no partial-schema repair and no reader for an older layout.

The App's own `client.json` follows the same rule at `version: 2`: a file with
any other version, an unknown key or an invalid Site entry is rejected rather
than partially read, and the App reports the configuration problem instead of
overwriting the file.

Durable room authority is written before the in-memory commit, so a crash can
lose live presence but cannot admit a credential the file does not hold.
Restoring a database restores ownership, invitations, code policy and password
material; it never restores participants, sessions, routes or media.

## Build Identity

Packagers inject two compile-time values into each binary; they are provenance,
not protocol:

| Variable | Server | App | Loopback/Web |
| --- | --- | --- | --- |
| `PIIK_RELEASE` | `BUILD_VERSION` in `crates/piik-server/src/main.rs` | `build_info::RELEASE` | `buildinfo::RELEASE` |
| `PIIK_REVISION` | `BUILD_REVISION` in `crates/piik-server/src/main.rs` | `build_info::REVISION` | `buildinfo::REVISION` |

An unset value defaults to `development`; no build fabricates a release label.
Packagers also set `VITE_PIIK_VERSION` and `VITE_PIIK_REVISION` so the embedded
Web build reports the same identity. Windows packages use the fixed target name
`windows-amd64` and a full revision in the archive's `REVISION` file. Release
descriptors use schema 2 and repeat version, revision and artifact hashes.

Build identity is displayed and diagnosed; it is never compared to grant
access, accept a message or choose a behavior. Two builds are compatible only by
having the same protocols, and a current revision always has them.

## Rejection Behavior

The same rule applies at each boundary, with an error surface appropriate to it:

| Boundary | Mismatch result |
| --- | --- |
| Signaling | `InvalidMessage`, close `1008`; authentication failures close `4003` |
| Native discovery | `NativeCompatibilityError`; Browser continues without native capability |
| Native control socket | Close `unsupported control protocol` before a session |
| Capture probe | Invalid contract; native capture unavailable |
| Capture stream | Invalid frame header; the capture stream fails |
| Room database | Startup failure; no traffic accepted |
| Release descriptor | Packaging rejects the descriptor |

No rejection is silent, and none is repaired by guessing. A user who needs
another combination updates the shipped package; Piik never fetches, negotiates
or synthesizes one.
