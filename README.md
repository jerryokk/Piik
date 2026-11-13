<p align="center"><img src="./docs/assets/brand-mark.svg" width="64" height="64" alt="Piik mascot"></p>
<h1 align="center">Piik</h1>
<p align="center"><strong>Share the good stuff.</strong><br>Private screen sharing for you and up to 20 friends.</p>
<p align="center">
  <a href="./docs/guide/getting-started.md">Get started</a> ·
  <a href="./docs/guide/README.md">Documentation</a>
</p>
<p align="center">
  <a href="./LICENSE"><img src="https://img.shields.io/badge/license-MIT-36564f?style=flat-square" alt="License: MIT"></a>
  <img src="https://img.shields.io/badge/viewers-up_to_20-36564f?style=flat-square" alt="Up to 20 viewers">
  <img src="https://img.shields.io/badge/media-P2P_first-36564f?style=flat-square" alt="P2P-first media">
</p>
<p align="center">English · <a href="./README.zh-CN.md">简体中文</a></p>

Piik is a free, open-source screen sharing tool for private live streams with
friends: games, movie nights, drawings and photos. Choose what to share and send
an invitation. Your friends watch in their browsers. You can also self-host Piik
on your own server.

[Features](#features) · [Get started](#get-started) · [Self-hosting](#self-hosting) · [Contributing](#contributing)

## Features

- **Watch without installing.** Friends join by invitation in a desktop or mobile browser.
- **Share from Piik App for Windows.** Choose a screen, window or camera; Viewers only need a browser.
- **Direct connections first.** Media travels between participants where possible (P2P). A self-hosted server can provide automatic media forwarding (SFU) as a fallback.
- **Rooms you control.** Invitations, room codes and optional room passwords, for one host and up to 20 viewers.
- **Talk and chat together.** Join room voice, send messages and reactions, or enable on-screen chat.
- **Hand control to friends.** Share an App window or screen and approve remote control. Allow friends to play together with shared keyboard/mouse or supported gamepads; take control back at any time. [Setup guide](./docs/guide/remote-control.md).
- **Flexible viewing.** Light and dark themes, playback controls, picture-in-picture and a connection topology view.
- **One program to host a site.** Web UI, room management and optional media forwarding are packaged together.

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="./docs/assets/room-en-dark.png">
    <img src="./docs/assets/room-en-light.png" width="860" alt="Piik room: a shared game, playback controls, chat and friends on a sofa. The host wears a small gold crown.">
  </picture><br>
  <sub>Interface preview · Sample room with a generated game scene</sub>
</p>

## Get started

**To watch:** open the invitation your friend sends you. Select **Play** if the
picture does not start automatically.

**To share:** use **Piik App** for Windows. The Host shares only from the App;
browsers are for Viewers.

| Start here | What you need to do |
| --- | --- |
| Get Piik App | Get the Windows x64 package from whoever hosts your Piik, extract it, open Piik, choose **Public invite**, then **Open Piik**. |

<p align="center"><img src="./docs/assets/quickstart.svg" width="640" alt="Choose a screen, send an invitation, watch together."></p>

1. Select **Start sharing**, then choose the picture and audio to share.
2. Select **Copy invite link** and send it to your friends. Keep the App open.

Open **Interact → Voice** to talk together, with or without picture sharing.
The Host opens voice; everyone joins to listen and chooses when to turn on their microphone.

Piik follows your system language: Chinese or English, with English as the fallback.
Use **中 / EN / ✦** in the header to switch language or try the visual mode.
The App can create a local room, a temporary public invitation, or a room on an
approved self-hosted site; the room opens in the App window. Keep it running while sharing.

[**Illustrated getting started →**](./docs/guide/getting-started.md) · [**Troubleshooting →**](./docs/guide/troubleshooting.md)

Choose a download beginning with **`piik-app`**. The guide covers installation,
source selection and invitations.

The App's temporary public link and a default Piik site use P2P and need a usable
UDP connection between participants. For campus
networks, failed media connections or virtual LANs, see [network checks and connection options](./docs/guide/network.md).

## Self-hosting

Deploy Piik Server to run a site for your group.

Piik Server is one Windows x64 program with the web interface built in.
Download `piik-<revision>-windows-amd64-runtime.zip`, extract it and run
`./piik-server.exe` in PowerShell.

Open `http://localhost:8787` to try it locally. For a public site, configure your
domain, HTTPS reverse proxy and STUN address. Room data is stored in SQLite.

[**Deploy your own site →**](./docs/operations/self-hosting.md) · [Configuration](./docs/standards/configuration.md) · [Run from source](./docs/README.md#run-from-source)

## Contributing

Bug reports, translations, documentation improvements and pull requests are welcome.
See [ways to contribute](./CONTRIBUTING.md#ways-to-help) or
[help translate Piik](./docs/guide/translating.md). Use the
[repository layout](./docs/architecture/modules.md#repository-layout) to find your way around.

## License

Piik-owned code is [MIT licensed](./LICENSE). Dependencies retain their own
[licenses and notices](./licenses/README.md).
