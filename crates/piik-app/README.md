# Piik App

English · [简体中文](./README.zh-CN.md) · [Getting started](../../docs/guide/getting-started.md)

Piik App is the Windows Host for Piik. It provides local rooms, temporary public
invitations and native screen capture in its own window. Viewers join in a browser.

## Run A Package

For the step-by-step workflow, follow
[Getting started](../../docs/guide/getting-started.md#share-with-piik-app).

Get the Windows x64 package from whoever hosts your Piik. Extract it in full,
keep `runtime` beside the App executable and run `piik-app.exe`. The launcher
appears in the App window, and the room opens in that same window.
Starting `piik-app.exe` again focuses the existing window.

## Modes

Choose **Local room** or **Public invite**, then **Open Piik**, to start a room
service. To host on a self-hosted site instead, [approve that site](#approved-site).

App remembers the last confirmed room mode. On first use, **Public invite** is
selected and waits for confirmation. A starred site opens on startup instead.

Follow the [upgrade steps](#upgrading) to replace App.

### Local Room

Choose **Local room** for a reachable LAN. Rooms last for this App run.

App selects a sole active address or sole private IPv4 automatically. When
several are available, choose **Local invitation address** in the launcher.
For a [virtual LAN](../../docs/guide/network.md#optional-a-virtual-lan), choose its virtual IPv4.

Friends use the HTTPS invitation on that address; the Host uses the local
loopback page in the App window. On first access, friends must [confirm the certificate](../../docs/guide/troubleshooting.md#local-https-certificate-warning)
against the Host's App. Repeat confirmation after App restarts.

### Public Invite

Choose **Public invite**, create a room in the App window, and send its invitation.
The packaged Cloudflare Tunnel helper supplies a temporary `trycloudflare.com`
HTTPS address. It expires when App closes; a later run gets a new address.
For a permanent address, use a self-hosted Piik site.

Local and Public invite use P2P picture and sound, outside the web tunnel.
They need a usable UDP media path and provide no server media forwarding (SFU)
or TURN relay. If the tunnel or media fails, use the [network checks](../../docs/guide/network.md)
and [public-invitation checks](../../docs/guide/troubleshooting.md#public-invitation-fails-or-shows-1033).

Public invite uses Cloudflare, MiWiFi (Xiaomi) and Bilibili STUN services to
discover reachable addresses. Each contacted operator can see your public IP,
port and request timing; these requests contain no media, chat or room password.
To choose your own STUN services for public invitations, set `PIIK_STUN_URLS`
before starting App; see [App configuration](../../docs/standards/configuration.md#piik-app-configuration).
An approved Site supplies its own `STUN_URLS`; App Local rooms configure no public STUN.

### Approved Site

Enter the Piik site address in the launcher, then confirm its App access. The
site opens in the App window. See the [connection steps](../../docs/guide/getting-started.md#use-app-with-an-existing-site).

The launcher lists approved sites:

- Click a site to open it. **×** revokes its App connections while other sites and Local rooms continue.
- **☆** selects one site to open on startup. Clear the star to restore the launcher;
  revoking the starred site also clears it. Approving another site keeps existing approvals.
- Click a connected site's **App** button to return to the launcher.

A new public-invite address needs its own approval. The site continues to own
its rooms and enabled SFU forwarding.

## Capture

The Host page offers Camera capture alongside available native windows or
screens; select the source explicitly. Windows native capture offers VP8/Auto/H264;
Auto chooses one codec for the share. Setup is covered in the
[Windows capture guide](../../native/capture/windows/README.md).
Adjust native capture through [Sharing settings](../../docs/guide/sharing.md#picture-and-sound-settings).

When the Windows App's capture runtime supports it, the **Apps / Windows** and
**Screens** tabs offer **Show capture border**, off by default. Change it when selecting or switching a
source; the Host page remembers your choice. Windows 10, system policy or another
active capture can keep the yellow border visible; sharing remains available.
See [capture-border troubleshooting](../../docs/guide/troubleshooting.md#yellow-capture-border-on-windows)
for an optional Windows 10 workaround.

## Configuration And Command Line

The App configuration keeps an optional Local site passphrase. Leave it
blank for an open Local site, or enter a password in the launcher before
starting. Viewer invitations grant access to their room independently; see
[rooms and access](../../docs/standards/rooms-access.md).

For saved preferences, configuration paths and environment variables, see
[App configuration](../../docs/standards/configuration.md#piik-app-configuration).
To bypass the launcher in development or automation, select a mode explicitly:

| Flag | Mode |
| --- | --- |
| `--local` | Local room; add `--lan-address <address>` when several LAN addresses are possible |
| `--link` | Temporary Public invite |
| `--site <origin>` | Save and open a Piik Site |

## Upgrading

The App, Server and site come from one Piik build. Use the packages from the
same download; a page from another build is rejected.

1. Get the new package and its release notes from whoever hosts your Piik.
2. For Local room or Public invite, stop sharing, close App and extract the
   complete new package. Start it and send a fresh invitation.
3. For a self-hosted site, the administrator [updates Server](../../docs/operations/self-hosting.md#keep-it-running-and-update)
   first. Then update connected Apps and refresh all viewing pages.

Restarting App or Server interrupts active shares.

## Window And Shutdown

Piik App runs in one window. Starting it again focuses that window instead of
opening a second App. Close the window to quit: this ends Local rooms and stops
the local server and temporary public link.

## Troubleshooting

### Chromium WebRTC Connections

Viewer browser settings, extensions or managed policies can block WebRTC UDP,
including a media connection from a browser on the Host's computer. Follow the documentation center's
[browser WebRTC checks](../../docs/guide/network.md#browser-webrtc-restrictions).
For codec-specific failures, see [Windows H264 troubleshooting](../../docs/guide/troubleshooting.md#h264-sharing-fails-on-windows).

### Diagnostics

Enable **Debug launch** in the launcher before starting a room or connecting a Site, reproduce the
problem, then export the diagnostic bundle from the App as a local ZIP.
Use `--debug` for failures before the launcher appears. Exporting does not stop
the share or upload the archive. Send the bundle to whoever provides your Piik. For Browser diagnostics, open **Help → Debug**
after the theme control, confirm the reload, then use **Help → Web report** to download.
For cooperation failures, include both reports from the same reproduction. The
[diagnostic reference](../../docs/standards/configuration.md#diagnostics) owns
log locations, export commands, retention and privacy boundaries.

## For Developers

[App development](./DEVELOPMENT.md) covers source builds, packaging, desktop icons
and validation gates. [Deployment](../../docs/operations/release.md) covers maintained
release publication.
