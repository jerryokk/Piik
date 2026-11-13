# Native Host

This file owns the Windows-specific half of the App: the loopback control
boundary, the capture helper, remote input and the public tunnel. Process and
deployment shape stay in [system](./system.md); wire constants in
[protocols](./protocols.md); capture and encoding behavior in
[media pipeline](./media.md).

## The App And Its Children

The App is one Tauri window running the shared Rust core. Around that core it
supervises bounded children, each with a narrow job:

- **Capture helper** — `runtime/native/piik-capture.exe`, a console process the
  App probes and then drives over framed standard input and output.
- **Public tunnel** — `runtime/tunnel/cloudflared.exe`, started only for a
  public-link room and contained in a kill-on-close job object.

Both are replaceable without touching room, routing or signaling code, but the
framing they exchange with the App is matched to the App that ships beside them.
A missing helper disables native capture only; Browser capture and native
receive and relay still work.

## Loopback Control

The loopback capability service listens on `127.0.0.1` at the first free port in
`39721..=39730`. It is what connects a page — the App's own WebView or an
approved Site in an ordinary browser — to native capability:

- `/connect-discovery` is the only endpoint open before approval; it lets a page
  find the App and see whether this origin is approved.
- `/health`, `/presentation`, `/quality-preference` and the `/control` WebSocket
  admit only approved Sites, local origins and the launcher.
- Approval is a runtime origin list inside the App, not a static Tauri remote-IPC
  capability, so the same service behaves the same for the WebView and for a
  browser tab.
- At most two control sessions run at once; each is an independent share.
  [Protocols](./protocols.md) owns the port and limit values.

The control protocol has one exact version (`piik-client-v10`) and one
subprotocol (`piik-client`). A page that presents any other version is rejected with a
clear mismatch. Native capture and native receive then stay unavailable for that
page until it is opened from the matching App; ordinary Browser capture and
viewing continue. There is no negotiation and no other message shape to serve.

## The Capture Helper

The helper is the only Piik component with no network access. It inherits the
signed-in user's desktop, so it can open any window or display that user can,
and it reports what the machine supports through a probe: protocol version,
platform, video capture, process and system audio, software VP8, adapters and
hardware H.264 encoders. The App caches that probe for the process lifetime.

Commands and results travel as the SMED envelope over standard streams, with
video frames, audio frames and status messages framed separately. The helper owns
capture, conversion (including one HDR-to-SDR pass) and encoding; the App owns
which source, which quality, and what happens to the frames afterwards. A helper
that exits or fails is reported as capture failure by stage, and the App retires
the share rather than pretending the picture continues.

## Remote Input

One manager owns Windows injection for every native share. Input stays disabled
until a share's Host enables it; a single grant covers keyboard and mouse plus an
optional gamepad, and the grant is tied to the current source. The Host can allow
several Viewers to control one source at once. Source change, stop or explicit
revocation retire the grant; a Viewer leaving releases what it held.

Injection uses standard Windows input APIs and gamepad emulation where it exists.
Protected or elevated windows may refuse input; that is reported as a platform
result, not retried indefinitely. No driver, service or startup entry is
installed, and the App ships no optional input SDK.

Controllers are optional and external. Piik talks to an already-installed
ViGEmBus through its public device interface, without loading the client DLL or
bundling a driver; a Host who skips it keeps keyboard and mouse. XInput is the
only budget: the Host's own controllers and every virtual one share the same
four slots, so availability is read from what Windows reports rather than from
how many devices Piik created. A target the driver accepts but XInput never
lists is retired instead of counted, and enumeration runs one device at a time
because the driver assigns the free index itself. Each Browser gamepad index is
one target and one address on the wire, so a refusal, reset or stop names the
device it concerns and leaves the Viewer's others running.

## Public Link

Public-link mode exposes a Local room's authority through a temporary public
control tunnel while media stays on the existing P2P/SFU paths. The App starts the
packaged `cloudflared.exe` as a child, waits within a bounded startup window for
the public origin, and reports the invitation using that origin. Closing the App
kills the job object, so a crashed App cannot leave a tunnel running.

The tunnel carries control traffic only. Media credentials remain short-lived and
room-scoped, and the tunnel URL is a temporary invitation rather than a durable
address.

## Diagnostics

Diagnostics are opt-in and bounded. A debug run records redacted logs in the App's
log folder (beside the executable, falling back to
`%LOCALAPPDATA%\Piik\logs`) and exports a ZIP only when the run fails. Logs record
build identity, listener and service facts, routing decisions and failures; they
never contain media payloads, raw credentials or unrelated user data. Gate mode
prints plain readiness and identity lines for automation instead of a console
surface, because the App has none.
