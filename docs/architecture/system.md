# System Overview

Piik ships one Windows App, one Server and one Web client from the same
revision. This file owns the running processes, their trust boundaries, the
deployment shapes and what each surface keeps. Room, routing and media
behavior stay with the product standards; build and wire identity stay in
[protocols](./protocols.md), and module ownership in [modules](./modules.md).

## Surfaces

| Surface | Process | What it is |
| --- | --- | --- |
| Piik Server | `piik-server.exe` | One headless Rust process serving Web assets, room authority and signaling, STUN and optional embedded SFU |
| Piik App | `piik-app.exe` | One Tauri v2 WebView2 window around the same Rust core, its loopback capability service and helper processes |
| Browser client | Tab in a desktop or mobile browser | The same Web build the App and Server embed, with WebRTC capture, playback, relay and control |
| Capture helper | `piik-capture.exe` child | The Windows screen/audio capture and encoding process, supervised by the App |
| Public tunnel | `cloudflared.exe` child | Temporary public HTTPS entry for a public-link room, supervised by the App |

There is no Node, Vite or other runtime service in a running Piik. Node and
Vite only build the Browser UI in `web/dist`; both binaries embed that output.

## Piik Server

`piik-server` is a thin entry point: it reads the environment, installs
diagnostics and logging, composes `piik-service` and stops it on the first
console-close or shutdown signal. `piik-service` is the process's whole
service surface:

- the embedded Browser assets, `/healthz`, site access and the room HTTP API;
- `GET /signal`, the authenticated room WebSocket;
- zero or more in-process Binding-only STUN listeners;
- at most one embedded SFU on a single UDP port;
- one room authority over either a SQLite file or process memory.

Startup is ordered so that a failure leaves nothing half-open: the SFU's
advertised address resolves, the TCP listener binds, STUN binds, the SFU binds,
and only then is room authority restored and traffic accepted. A bind failure
releases every socket opened by that attempt. One process owns room authority
and SFU admission; Piik has no multi-process authority owner.

The Server has no tunnel and no hosted control plane. It never contacts a
release service, and it serves only the Web build compiled into it.

### Network Shape

| Listener | Configured by | Serves |
| --- | --- | --- |
| TCP 8787 | `LISTEN_HOST`/`PORT` | HTTP, or direct HTTPS when TLS is configured |
| UDP 3478 | `STUN_LISTEN_HOST` | Ordinary STUN discovery, when `STUN_URLS` is set |
| UDP 3479/3480 | derived from the 3478 authority | NAT-prediction STUN, when enabled |
| UDP 7882 (standard) | `SFU_UDP_PORT` | Embedded SFU media, when enabled |

Nothing else is opened. TURN, TCP media and TLS on 5349 are not part of Piik.
STUN and the SFU are UDP-only and independent of the HTTP listener. DNAT,
firewall rules and cloud security groups must forward each configured UDP
listener separately; binding locally does not prove reachability.

### HTTPS Entry

The Server accepts two deployment shapes, and only one of them is configured:

- **Operator proxy.** The operator terminates HTTPS and forwards to the
  private HTTP listener, normally `LISTEN_HOST=127.0.0.1` and `PORT=8787`.
  Piik trusts proxy-independent facts only: the configured public origin,
  allowed origins and its own framing and access checks. Rate limiting remains
  the proxy's job.
- **Direct TLS.** `TLS_SELF_SIGNED=true` creates an in-memory certificate for
  the exact public host; alternatively `TLS_CERT_FILE` plus `TLS_KEY_FILE`
  load an operator certificate after checking host coverage and validity.
  Both serve HTTPS on the ordinary `LISTEN_HOST`/`PORT`. Certificate renewal is
  the operator's job; the process reads the files at startup.

Values and validation rules stay in [configuration](../standards/configuration.md).
Deployment and rollback stay in [release](../operations/release.md) and
[self-hosting](../operations/self-hosting.md).

## Piik App

`piik-app` is one process with one visible window and no console. It owns:

- **The Tauri shell.** The window opens on the launcher and then navigates in
  place to a Local room or an approved Site. `target=_blank` and `window.open`
  never create a second App window; anything outside the current allow-list
  opens in the system browser. A second launch focuses the existing window.
- **The embedded runtime.** Behind the launcher URL it runs the loopback
  capability service, the launcher HTTP server, and either a Local or
  public-link room served by the same `piik-service` used by the Server.
  Local and public-link rooms never create an SFU listener.
- **Native capability.** It discovers `runtime\native\piik-capture.exe`, probes
  it, and advertises the resulting native media bits. A missing or failed
  helper only disables native capture; pages keep Browser capture and native
  receive/relay remains available.
- **Remote input.** One manager owns Windows injection for native shares; its
  absence disables remote control and nothing else.
- **The public tunnel.** Public-link mode starts the packaged `cloudflared.exe`
  as a child in a kill-on-close job object, so it cannot outlive the App.

The App's loopback capability service listens on `127.0.0.1`, at the first free
port in `39721..=39730`. `/connect-discovery` is the only endpoint open before
local approval; `/health`, `/presentation`, `/quality-preference` and the
`/control` WebSocket admit only approved Sites, local origins and the launcher.
At most two control sessions run concurrently.
Approval is a runtime origin list in the App, not a static Tauri remote-IPC
capability, so the same service serves the App's WebView and an approved Site
in an ordinary browser.

The capture helper is a separate, bounded child process. The App writes
commands and reads framed video, audio and status over its standard streams;
the helper inherits the signed-in user's desktop access and has no network
listener. The helper can be replaced without changing the App's room, routing
or signaling code, but its probe and frame contract remain matched to the App
that ships beside it.

## Browser Clients

The Browser client is the Vite build in `src/` with `src/shared` wire types. It
runs in the App's WebView2 and in desktop and mobile browsers; iPhone and iPad
use WebKit. Input handling supports mouse, keyboard and touch, and layout works
from narrow phone widths upward. The Browser has no native code: capture uses
the browser's own pickers and WebRTC, and native capability is reached only
through the App's loopback service when a page is eligible.

The Web build contains the Host, Viewer, relay, room voice and remote-control
surfaces. Native capture and Local rooms exist only behind the App. The
supported product Host surface is the App window; the same page set is what
developers exercise against a Server.

## Trust Boundary

Piik has no shared secret between the App and the Server. Each boundary has its
own authority, and a claim at one boundary never substitutes for another.

| Boundary | Authority | Enforcement |
| --- | --- | --- |
| Operator to Server | Process environment, `.env`, database file, TLS files | Startup validation; `--check-config`; no remote configuration |
| Browser to Server | Site access cookie, Host token, Viewer grant or code entry | Constant-time secret comparison; room-scoped credentials; strict signaling |
| Page to App | Approved origin or local origin | `/connect-discovery` is open; every other endpoint and `/control` requires approval |
| App to capture helper | Local child process in the signed-in session | Bounded probe, commands and frames; no network, no room authority |
| App to public tunnel | Local child process | Temporary HTTPS entry only; room authority and media remain Piik's |
| Viewer to Host media | DTLS-SRTP between WebRTC peers | Media-path credentials are process-local; the SFU relay is not end-to-end encrypted |
| Viewer to Host input | Source-scoped grant plus per-packet authenticated encryption | Windows injection only from an admitted Host control session |

Local native capability never grants room authority. A Server-issued room
credential, room session and route graph remain authoritative even when the
App supplies the media edge. The App can serve an approved Site while running
no Local room, and the Site's own authority applies then.

## Deployment Shapes

### Hosted

One Server process behind an operator HTTPS proxy or with direct TLS. Room
authority defaults to `rooms.sqlite` in the working directory; `:memory:` is
an explicit opt-out. STUN is enabled by configuration, and the SFU only when
`SFU_UDP_PORT` is set. Operators distribute the immutable archive themselves;
there is no update check.

### Local

The App serves the Host on a loopback HTTP origin and Viewers on the selected
LAN IPv4 address over HTTPS at the same port. The certificate and key exist
only in memory for that run, so Viewers confirm the new fingerprint after a
restart. The same LAN address answers Binding-only STUN on the App's numeric
UDP port, so routed LANs without multicast name discovery still learn
addresses. Rooms, participants and media end with the process. Public STUN is
not used for Local invitations.

### Public Link

The App starts a room as in Local mode, then exposes its loopback HTTP entry
through a temporary `trycloudflare.com` address supplied by the packaged
tunnel. Room authority and the tunnel stay on the Host; media remains P2P and
never crosses the tunnel. The address expires with the App run, and a new run
gets a new address.

## Ephemeral And Persisted

| Data | Lifetime | Owner |
| --- | --- | --- |
| SQLite room authority | Across Hosted restart at the configured path | `piik-room` |
| App Local rooms | Process only | App runtime |
| Participants, sessions, presence, signaling | Process and connection | `piik-signal` |
| Route graph, candidates, generations | Process and share/room generation | `piik-route` |
| SFU publications, subscriptions, room voice | Process and publication generation | `piik-sfu` |
| LAN certificate and private key | One App run, memory only | `piik-tls` |
| Site-access cookie | 24-hour idle lifetime within one service run | `piik-service` |
| Public-link address | One App run | Public tunnel child |
| App `client.json` and its side files | Across launches | `piik-app-config` |
| Browser preferences and host resume hint | Browser storage | Web client |
| Diagnostic logs and reports | Until the user removes them | `piik-diagnostics` |

Persisted authority never includes live presence or media. Restoring a room
database restores ownership, invitations, code policy and password material;
sessions and routes reconnect from fresh process state. Removing or replacing
a room through Piik retires its authority; editing the database outside Piik
is not a supported operation.

## One Version

The App, Server and Web client are always deployed together from one revision.
No surface negotiates a version, accepts an older peer or carries a
compatibility fallback. A mismatched peer is rejected at its own boundary.
[Protocols](./protocols.md) owns each constant, its rejection behavior and the
change rule.
