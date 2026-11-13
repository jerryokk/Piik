# Deploy Piik Server

English · [简体中文](./self-hosting.zh-CN.md) · [Documentation](../guide/README.md)

Piik Server packages the web interface, room management and optional media
forwarding in **one Windows executable**, `piik-server.exe`. Extract and run it;
room data is stored in SQLite.

The default P2P configuration handles room management, signaling and STUN; shared
video and audio travel between participants. Enabling SFU adds media-forwarding
bandwidth and processing on the server. For a small server, start with the default
and measure CPU, memory and network use with your intended number of rooms and
viewers before enabling SFU. Lowering picture resolution primarily reduces
participants' capture and encoding work.

## Try it locally

Get `piik-<revision>-windows-amd64-runtime.zip` (Windows x64) from whoever
provides your Piik packages. Extract it,
open PowerShell in the extracted directory and run:

```powershell
./piik-server.exe
```

Open `http://localhost:8787`. This is a local trial; use the configuration below
to let friends connect over the internet. To start a temporary room on your computer,
use the [Piik App guide](../guide/getting-started.md).

## Private LAN without a domain

For a home server or virtual LAN, Piik can serve HTTPS directly. Save the
following as `.env` beside `piik-server.exe`, replacing every `192.168.1.10`
with the server's address on that network:

```dotenv
PIIK_ENV=production
LISTEN_HOST=0.0.0.0
PORT=8787
PUBLIC_BASE_URL=https://192.168.1.10:8787
TLS_SELF_SIGNED=true
STUN_URLS=stun:192.168.1.10:3478
```

From that directory, run `./piik-server.exe --check-config`, then `./piik-server.exe`.
Allow TCP 8787 and UDP 3478 on that network. Open the **HTTPS** address on each
device; no public IP, domain or reverse proxy is needed.

The browser will warn about the self-signed certificate. Compare its SHA-256
fingerprint with the server's **HTTPS certificate** startup message before
continuing; see [local certificate confirmation](../guide/troubleshooting.md#local-https-certificate-warning).
The certificate stays in memory and changes when Piik restarts, so confirm the
new one after a restart. Browser microphone, camera and remote control still
require their ordinary permissions and supported APIs.

To keep your own certificate, turn off `TLS_SELF_SIGNED` and set both
`TLS_CERT_FILE` and `TLS_KEY_FILE` to its PEM files. The certificate must cover
the host in `PUBLIC_BASE_URL`. `--check-config` checks the pair and validity;
restart Piik after replacing the files. Piik does not obtain or renew these
certificates; for a long-running public site, use an HTTPS proxy that manages
them, as below.
If the web port is occupied, stop the conflicting service or change both `PORT`
and the port in `PUBLIC_BASE_URL`, then send new invitations.

## Put it online

You need a Windows server and a domain pointing to its public IP.
The example uses `share.example.com`; replace it with your domain.

### 1. Configure and start Piik

Create a `.env` file next to `piik-server.exe`:

```dotenv
PIIK_ENV=production
LISTEN_HOST=127.0.0.1
PUBLIC_BASE_URL=https://share.example.com
STUN_URLS=stun:share.example.com:3478
MAX_VIEWERS_PER_ROOM=20
SITE_ACCESS_PASSWORD=
```

Run these commands in PowerShell from that directory as a regular user:

```powershell
./piik-server.exe --check-config
./piik-server.exe
```

The server reads `.env` automatically and stores rooms in `rooms.sqlite` in the
working directory. A blank `SITE_ACCESS_PASSWORD` allows
entry without a site passphrase; enter a passphrase to require one.
Room invitations and access settings still apply.

`PUBLIC_BASE_URL` must match the address opened in the browser, including HTTPS
and any non-default port. Leave `ALLOWED_ORIGINS` unset or empty for this single
address. An existing non-empty value overrides that default; a stale value can
cause [403 when creating a room](../guide/troubleshooting.md#room-creation-returns-403).

`MAX_VIEWERS_PER_ROOM` sets the room's Viewer limit, excluding the Host. Choose
`1..20` and restart to apply it. More Viewers can require more network and relay
resources. See [room capacity](../standards/configuration.md#room-capacity)
for defaults and the difference from App rooms.

For a public site, optionally set `ROOM_EMPTY_TIMEOUT_SECONDS=3600` and restart.
Rooms with nobody connected are reclaimed after an hour; if all room codes are
used, the oldest empty room can be reclaimed earlier. Occupied rooms keep working.
Reclaimed rooms need new invitations. Unset or `0` keeps the default indefinite
retention; see [room retention](../standards/configuration.md#room-retention).

### 2. Enable HTTPS

Use your HTTPS reverse proxy to forward to `127.0.0.1:8787` with WebSocket
support. If the proxy sets `Permissions-Policy`, use
`camera=(self), microphone=(self), geolocation=()`. Empty camera/microphone
allowlists prevent Browser capture even when the user grants permission; `self`
allows this site to request access.

### 3. Open the ports and verify

Allow **TCP 80/443** for HTTPS and **UDP 3478** for STUN in Windows Defender
Firewall and any cloud security group. Keep TCP 8787 private. The STUN hostname
must resolve directly to the server; a CDN HTTP proxy does not forward its UDP traffic.
See the [complete port table](../standards/configuration.md#public-and-private-ports)
for optional services. Embedded SFU media shares the single UDP port set by
`SFU_UDP_PORT`.

Open `https://share.example.com/healthz`; it should return `{"status":"ok"}`.
Then open the site, share a screen and join from another device. This initial
configuration uses P2P media, so participants need a usable UDP path.

## Optional: media fallback

Add `SFU_UDP_PORT=7882` to `.env`, allow UDP 7882, and restart Piik to enable
automatic SFU fallback. When the server is behind NAT, also set `SFU_PUBLIC_IP`
to its reachable public IPv4 address or DDNS hostname and forward the same external
and internal UDP port as `SFU_UDP_PORT`. Use a hostname pointing directly to that
IPv4, with the CDN proxy disabled. Piik resolves it at startup and every minute.
After DDNS updates, new media connections use the updated address without a
service restart. If the address change interrupted an ongoing share, the Host
can restart sharing and Viewers can rejoin the room.
The same executable provides the fallback.
Allow server forwarding capacity and outbound bandwidth for Viewers actually
using the SFU.
The Host must turn off **Privacy mode** before sharing to allow this route.
Both direct and SFU media need a usable UDP connection.

To send every room through your server, also set `SFU_ONLY=true` and restart.
The sharing settings will show **Server media**. Plan server bandwidth for every
Viewer; this mode does not fall back to P2P if the server route fails.

## Keep it running and update

Run `piik-server.exe` under your existing service manager or Task Scheduler;
Piik does not install a Windows service.
Keep `.env` and `rooms.sqlite` across updates. Back up room data while the server is
stopped, replace the executable with the new release, then restart and check
health and room access. Read release notes before an upgrade that changes data formats.

[All settings and ports](../standards/configuration.md) ·
[Maintainer release tooling](release.md)
