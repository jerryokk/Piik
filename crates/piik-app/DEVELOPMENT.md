# Piik App Development

Build, package and verify Piik App from the repository root. For installation,
mode selection and diagnostics, use the [App guide](./README.md).

## Run From Source

Use the Node/npm/Rust versions in [Run from source](../../docs/README.md#run-from-source).
The App is a Tauri v2 binary whose `frontendDist` is the Vite output in `web/dist`,
and it loads the launcher over its own loopback HTTP service rather than a Tauri
dev server, so `cargo tauri dev` is not used.

Build and start it in one step:

```sh
npm ci
npm run app
```

That builds the Browser UI, compiles the App and stages its helpers before
starting. Use `npm run app -- --no-build` to restart from the current build, and
`npm run build:app` to produce everything — App, Server and the capture helper —
without starting anything.

Cargo keeps `target/debug/piik-app.exe` stable between runs, so the Windows
firewall identity stays fixed. Rebuild the Browser assets whenever the UI
changes; Cargo embeds them at compile time.

A running App resolves its helpers beside its own executable, at
`runtime/native/piik-capture.exe` and `runtime/tunnel/cloudflared.exe`, so a
plain `cargo build` has neither: native capture is unavailable and **Public
invite** fails with `tunnel-unavailable`. `npm run app:runtime` stages the
development layout:

```sh
npm run app:runtime
```

It copies the capture helper from `build/rust-check` and the pinned cloudflared
release (digest-checked, cached under the ignored `build/app-runtime-cache`)
into `target/release/runtime`; it builds both helpers when they are missing and
probes each before reporting. `--target debug` stages the debug profile, and
`--tunnel <path>` stages an already-downloaded cloudflared instead of the
pinned release.

Build the capture helper alone, and validate its bounded capability response,
with:

```sh
npm run check:native
```

### When the App does not start

The App has no console, so a startup failure presents as a window that never
opens. Capture the reason instead of guessing:

```powershell
npm run app -- --no-build --debug --log-dir build\app-diag
Get-Content build\app-diag\client.log -Tail 20
```

The most common cause is a per-user configuration written by a different build.
`%AppData%\Piik\client.json` carries a `version`; the App accepts only the
current one and does not migrate, so a stale file is an error rather than a
silent upgrade. `npm run app -- --fresh` removes it and lets the App write a new
one. Back up the file first if it holds a Site list you want to keep.

The repository-level check, `npm run check:rust`, runs `cargo fmt --check`,
Clippy with warnings denied and the workspace tests, then the capture build.
Real capture, GPU attribution, Browser decode, and public-network paths remain
explicit physical gates rather than environment-dependent unit tests.

Headless gate mode (`PIIK_CLIENT_GATE_NO_BROWSER=true`) runs the App without a
window or event loop: it prints its control endpoint and readiness lines on
stdout and stops on any stdin byte or EOF. The gate scripts below set it and
build the App with Cargo themselves; `PIIK_CLIENT_EXE` selects a prebuilt binary.

App and Hosted share the Rust room service (`piik-service`). The Browser UI
coordinates native capture and Viewer receive/relay through the App's local
control service; Viewer receive/relay remains available without a supported
native capture encoder. Use the [module map](../../docs/architecture/modules.md#crate-map) and
[contract map](../../docs/architecture/modules.md#contract-map) to find the source owners.
The candidate packager checks that capture helpers match the App's contract.

Packaged builds report the product version and source SHA in the launcher and
diagnostics. [Versioning](../../docs/standards/versioning.md) explains build identity.

## Packaging

From a clean revision on Windows, build the application release and run the
candidate wrapper. The wrapper builds capture,
verifies the pinned public-link helper, and assembles and checks the App:

```sh
node scripts/package-server-release.mjs /outside/repository/app-release
node scripts/package-app-candidate.mjs /outside/repository/app-release windows-amd64 /outside/repository/app-candidate
```

The supported target is `windows-amd64` (`x86_64-pc-windows-msvc`).
The result is a ZIP bundle and SHA-256 file. Manual sidecar assembly,
Release packaging is documented in
[deployment](../../docs/operations/release.md); creating a candidate does not publish it.

The App runtime contains the following files:

```text
piik-app.exe
REVISION
LICENSE
THIRD-PARTY-NOTICES.txt
runtime/native/piik-capture.exe
runtime/tunnel/cloudflared.exe
```

The executable embeds the Browser assets of the consumed application release and
carries that same full Git revision as the package `REVISION`. App/Site
interoperability follows the
[public compatibility contract](../../docs/standards/versioning.md#changing-an-interface).

For a native Host smoke run, launch the App and select a window in the Host
page, then open its invitation on another device. Choose the mode for that
network as described in the [App guide](./README.md#modes).

### Optional Gamepad Drivers

App packaging does not download or include optional input SDKs or drivers.
The Windows adapter calls an externally installed ViGEmBus through its public
bus interface and checks driver availability.
See the [gamepad setup guide](../../docs/guide/remote-control.md#gamepads)
for Host requirements.

### Desktop Icons

Tauri embeds the shared Piik mark from `crates/piik-app/icons/icon.ico` into the
Windows executable; the PNG entries beside it serve the window and taskbar.

The editable badge is [public/favicon.svg](../../public/favicon.svg).
Export it at 16/24/32/48/64/128/256 px as PNG entries in `crates/piik-app/icons/icon.ico`,
and at 32, 128 and 256 px as the matching `icons/*.png` files.
Keep the Web exports in sync: `public/favicon.png` at 96 px, and `public/favicon.ico`
with 16/32/48 px entries. Check actual small sizes on light and dark backgrounds.
Render the SVG at each target size so its small-icon styling applies, then
rebuild the App.

## Gates

The Local gate starts the packaged process, loads a real built page in Chromium,
proves bootstrap access and LAN invitation construction, and verifies complete
process/profile cleanup. The loopback gate separately proves Site access with
and without Chromium Local Network Access permission. Environment syntax below
is POSIX shorthand; set the same variables in PowerShell. For the loopback gate, provide a
reachable Piik Site and run once without `PIIK_CLIENT_GATE_GRANT_LOOPBACK`, then
again with `PIIK_CLIENT_GATE_GRANT_LOOPBACK=true` to cover both permission states.

```sh
PIIK_CLIENT_LOCAL_GATE=true \
CHROME_PATH=/path/to/chrome \
PIIK_CLIENT_EXE=/path/to/piik-app \
PIIK_CLIENT_GATE_LAN_ADDRESS=192.168.1.10 \
npm run gate:app-local

PIIK_CLIENT_LOOPBACK_GATE=true \
CHROME_PATH=/path/to/chrome \
PIIK_CLIENT_EXE=/path/to/piik-app \
PIIK_CLIENT_GATE_SITE_URL=https://<test-site> \
npm run gate:app-loopback

PIIK_CLIENT_MEDIA_GATE=true \
CHROME_PATH=/path/to/chrome \
PIIK_CLIENT_GATE_STUN_URLS=stun:<stun-host>:3478 \
npm run gate:app-media

PIIK_CLIENT_NATIVE_HOST_GATE=true \
CHROME_PATH=/path/to/chrome \
npm run gate:app-native-host

PIIK_CLIENT_NATIVE_HOST_GATE=true \
PIIK_CLIENT_LINK_MEDIA_GATE=true \
CHROME_PATH=/path/to/chrome \
PIIK_CLOUDFLARED=/path/to/cloudflared \
npm run gate:app-native-host

PIIK_CLIENT_LINK_GATE=true \
PIIK_CLOUDFLARED=/path/to/cloudflared \
PIIK_REMOTE_HOST=<public-test-host> \
PIIK_REMOTE_USER=<ssh-user> \
PIIK_REMOTE_SSH_KEY=/path/to/key \
npm run gate:app-link
```

The Windows media gate proves one hardware-H.264
capture generation, shared media source, Browser decode, PLI recovery, and STUN
candidate gathering. The native Host gate proves room creation and native video
delivery through the current route; native audio is included when the capability
probe supports it. The one-link media variant carries signaling through the
App's temporary public origin and checks decoded media in a local Browser
through the public invitation; that does not establish cross-network
connectivity. Native P2P quality evidence and embedded SFU delivery have
explicit gates.
