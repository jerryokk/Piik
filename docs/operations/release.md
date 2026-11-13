# Server Release And Recovery

This file owns routine immutable Server and App releases. Initial service setup is
in [self-hosting operations](./self-hosting.md); environment and ports
are in [configuration reference](../standards/configuration.md). Exact production
identity is owned by the immutable release descriptor and runtime `REVISION`.
[Status](../status.md) owns only the compact current product and operational
snapshot; it is not a per-release ledger.

## Release Boundary

A routine Server release changes only the built Piik server executable and
the Browser assets it embeds. It does not change the host, service manager
configuration, proxy/firewall rules, secrets, media listener configuration, or
persistent room state.

Any task that touches one of those excluded surfaces must first record its exact
pre-change value and a recovery procedure for that surface. Do not complicate the
routine Server update with one-off infrastructure branches.

The prior Server release is guaranteed only until the new one passes health
and postflight. It has no retention contract afterward and is not a maintained
backup; Git and immutable artifacts own history.

## Build Host

Start from a clean exact revision using Node from [`.node-version`](../../.node-version),
npm from [`package.json`](../../package.json), and Rust at the `rust-version` in [`Cargo.toml`](../../Cargo.toml) with the MSVC toolchain:

```sh
npm ci
npm run check
node scripts/release/package-server-release.mjs <output-directory-outside-repository>
```

The packager refuses a dirty tree, builds the Browser assets and the
`x86_64-pc-windows-msvc` server executable from that exact revision, records the
full revision, emits `piik-<revision>-windows-amd64-runtime.zip` plus a
path/size/SHA-256 manifest and release descriptor, and extracts its own artifact
to verify it. The archive contains exactly `piik-server.exe`, `LICENSE`,
`THIRD-PARTY-NOTICES.txt`, and `REVISION`. Windows operation follows the
[self-hosting guide](./self-hosting.md).

Public Release pages expose the runnable packages and put package checksums in
the release body. Retain the original complete build output outside the checkout
for later deployment retries; descriptors and manifests are not public Release
attachments. Do not rebuild an existing published version to replace missing
metadata or bytes.

The same Server descriptor is also the App assembly input:

```sh
node scripts/release/assemble-app.mjs \
  /outside/repository/server-release/piik-<revision>.release.json \
  /outside/repository/piik-app \
  --target windows-amd64 \
  --capture /path/to/piik-capture.exe \
  --tunnel /path/to/cloudflared.exe
```

Assembly refuses a dirty or different revision and emits one directory with the
App executable, the Browser assets taken from that release and embedded in
it, the selected sidecars, notices, and matching `REVISION`. Every supplied
binary input must match the `windows-amd64` target. It does not create an
installer or auto-updater. Tauri embeds
the App icon (`crates/piik-app/icons/icon.ico`) in the executable.

Release-candidate builds use the same wrapper on Windows:

```sh
node scripts/release/package-app-candidate.mjs \
  /outside/repository/server-release \
  windows-amd64 \
  /outside/repository/app-candidate
```

The wrapper downloads the pinned public-link sidecar, verifies its digest,
builds the App and capture process, executes every packaged runtime from the
extracted ZIP, and emits one `.zip` plus its SHA-256 file.

App ZIPs use ordinary Store/Deflate compression and relative entry names without
`.` or `..` path segments, and include hidden files. Windows packaging also
checks every entry through Windows Explorer's ZIP reader. A successful
extraction by the writer's own library does not prove that the user's default
archive tool opens it.

Retain the descriptor and successful deployment output as release metadata. Do
not create a follow-up source commit solely to duplicate their revision, asset,
or hashes. No release-record commit is written back to main.

Do not build or run the full repository check on a constrained production host.
That host runs only the packaged executable and needs no Node, npm, or dependency
install.

## Build Identity

Builds carry a product version plus full source revision. Packagers use one
release plan or exact Git tag and inject its identity into Server, App and Web;
untagged local candidates use `development`. Schema-2 package descriptors carry
the same pair and artifact hashes. The [version policy](../standards/versioning.md)
owns ordering and first-public-release readiness.

Neither the App nor the Server checks for updates; the
[release-source policy](../standards/versioning.md#shipping) owns how
packages reach users.

## Server Update

Before replacing a running Server:

1. verify the downloaded ZIP against its published SHA-256 and extract it to a
   new directory beside the current one;
2. run the new `piik-server.exe --check-config` from the working directory that
   holds the existing `.env`;
3. stop the running Server and back up `rooms.sqlite` while it is stopped;
4. replace the executable and its notices with the new release's files, keeping
   `.env` and `rooms.sqlite`; and
5. start the Server under the same service manager and complete postflight.

Infrastructure changes need their own scoped recovery procedure under
[CONTRIBUTING](../../CONTRIBUTING.md).

## Persistent State

Stable mode stores only room authority at the configured SQLite path.
Application restart retains room ownership, invitations, policy and password
verifier; participants, signaling, routes, SFU state and media reconnect from
fresh process state.

Lightweight mode has no persistent room state and starts empty. A release does
not add a migration reader for either mode; current exact schema validation
fails closed before accepting signaling or media admission.

## Postflight

Verify only the surfaces relevant to the release:

- local and public `/healthz` return `{"status":"ok"}`;
- the public HTML references the new immutable main asset;
- Piik owns each configured STUN/SFU UDP listener and public reachability
  matches the configuration;
- the served revision matches the descriptor;
- room creation, Host authentication, one direct Viewer, and configured SFU
  fallback complete the scoped smoke; and
- infrastructure, config, secrets, ports, and persistent state not named by the
  task are unchanged.

Representative network, audio, 20-Viewer, and endurance evidence belongs
to [verification status](../verification-status.md), not routine postflight.

## Recovery

Keep the prior release directory until postflight passes. On failure, stop
Piik, restore the prior executable and notices, restart the service, and wait
for health. A failed recovery requires operator intervention; do not report the
new release as active.

Infrastructure or persistent-state recovery is separate and limited to the
surfaces changed by that task. Do not restore an entire server, old firewall, or
old secret set for an application-only failure.

If a task changes persistent room data, prepare and verify a separate
[SQLite backup](https://www.sqlite.org/backup.html) and recover matching data,
configuration and application together. Restoring old data can revive revoked
credentials; account for later authority changes before reopening ingress.
