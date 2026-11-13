# Versions And Identity

Piik currently has exactly one version: the build you have. App, Server and the
Web client in that build always ship together from one source revision, and
there is no older installation, negotiated downgrade or compatibility range to
support. [Status](../status.md) owns the current state; Git owns release history.

## Give Each Identifier One Job

| Identifier | Meaning and owner | Change rule |
| --- | --- | --- |
| Full Git SHA / `REVISION` | Exact source revision | Injected by the packagers; identifies the build, never orders or compares releases |
| Artifact SHA-256 | Exact packaged bytes in the release descriptor | Per artifact; one source revision can produce different binaries per toolchain |
| Signaling identifier | `SIGNALING_PROTOCOL` in `src/shared/protocol.ts`, mirrored by `piik-protocol` | Change with the WebSocket contract; a mismatch is rejected |
| App control identifier | `PROTOCOL_VERSION` in `piik-loopback`; the page's copy in `src/client/native/wire.ts` | Change with the loopback contract; a mismatch is rejected |
| Capture helper identifier | `piik-native-capture` and `native/capture/windows` | Change with the helper's framing; the App requires the version it ships |
| SQLite schema, App config, Browser storage keys | Their storage owners | Change with the actual stored representation |
| Room authorization, share/route/publication generations, connection IDs | Current authority and resource owners | Runtime freshness only; never tied to release numbers |

The numeric identifiers exist so a mismatched peer fails immediately and
visibly instead of misbehaving. They are not a compatibility range: every
change updates the one constant in the same commit that changes the sides.

## Changing An Interface

The protected interfaces are those the App, Server and Web client use between
themselves: the WebSocket signaling messages, the App loopback control messages,
the capture helper's framing, room authority and persisted data. Changing one
means changing every side and its tests in the same commit; there is no staged
rollout to design and no field to keep for an older peer.

Prefer the smallest change that expresses the new fact: an optional field is
still justified when its absence is meaningful (for example, a value the helper
may not produce on this machine), never merely so an older build can ignore it.
Unknown fields are rejected, so a typo fails loudly instead of being skipped.

## Shipping

A release is a local build: the App ZIP and the Server archive, tagged and
handed to whoever provides Piik. Neither checks for updates. The App and Server
report their version and revision in their own surfaces and logs. Upgrading
means replacing both packages together; there is no partial upgrade, migration
path or mixed-version operation to plan for.
