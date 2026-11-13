# Your first Piik room

English · [简体中文](./getting-started.zh-CN.md) · [Back to Piik](../../README.md)

Piik lets one Host share with up to 20 invited Viewers. Choose your starting point:

| You want to… | Start here |
| --- | --- |
| Watch a friend | [Open their invitation](#join-a-friends-room). No installation. |
| Share with friends elsewhere | [Piik App → Public invite](#share-with-piik-app). Keep App running. |
| Host on your group's site | [Open that site in Piik App](#use-app-with-an-existing-site). |

![Three steps: pick a window, copy its room invitation, and join friends on the sofa.](../assets/quickstart.svg)

## Join a friend's room

1. Open the invitation link your friend sent in a desktop or mobile browser.
2. Wait for their screen to appear. If the browser blocks autoplay, press **Play**.
3. Move the pointer over the picture or tap it to show the playback bar.

The playback bar controls only your own picture and sound.
See [playback controls](./sharing.md#playback-controls) for fullscreen, display and sound options.

Only have a four-digit code? Open the same Piik site and select **Join a room**.
Enter the site passphrase or room password if asked. If code entry is unavailable
or an old link fails, ask the Host for their current invitation.

Each App's Local room or Public invite creates a separate site, so your friend's
code does not work on your own App page. Open their full invitation instead;
you do not need to create a room in your App to watch them.

## Share with Piik App

1. Get the Windows x64 **Piik App** package (`piik-app-windows-amd64`) from
   whoever hosts your Piik. Extract the whole archive and keep its directory
   structure intact.
2. Open `piik-app.exe`. The launcher appears in the App window; the room opens
   in that same window.
3. Choose **Public invite** for friends outside your local network, then select
   **Open Piik**. Other modes are explained below.
4. Choose **Start sharing**, then select **Camera**, **Apps / Windows**, or
   **Screens** as available. Pick the actual source and sound option.
5. Copy and send the room invitation. Keep the App open while sharing.

![App source selector with Browser, Camera, Apps / Windows and Screens tabs, window previews and source-audio controls.](../assets/guide/share-source.en.png#gh-light-mode-only)
![App source selector with Browser, Camera, Apps / Windows and Screens tabs, window previews and source-audio controls.](../assets/guide/share-source.en.dark.png#gh-dark-mode-only)

See the [App guide](../../crates/piik-app/README.md) for package and runtime details.

### Choose an App mode

![The App mode selector: Public invite is selected; Local room is the alternative.](../assets/guide/app-modes.en.png#gh-light-mode-only)
![The App mode selector: Public invite is selected; Local room is the alternative.](../assets/guide/app-modes.en.dark.png#gh-dark-mode-only)

- **Public invite:** send a temporary web link to friends elsewhere. It needs
  Internet access and lasts until you close the App.
- **Local room:** friends are on the same LAN or virtual LAN. Your computer
  must be reachable from theirs. If needed, expand **Local invitation address**
  and select the network interface your friends use.

For Local room and Public invite, expand **Site passphrase** to set an optional
access passphrase.
Local invitations use HTTPS. On first access, confirm the local certificate
against the Host's App before continuing; see the
[certificate guide](./troubleshooting.md#local-https-certificate-warning).

<details>
<summary>Return to the launcher or change mode</summary>

Once the room service starts, click the connected **App** button at the top right
to return to the launcher, manage site access or reopen the room. To change
between Local room and Public invite, close the App window, reopen the App,
then choose again. This ends the previous room
and generates a new public-invitation address.

</details>

### Use App with an existing site

Use an HTTPS Piik site provided by your group or administrator.
To run your own site, follow [self-hosting](../operations/self-hosting.md).

1. In the App launcher, enter the site address and choose **Open site**.
   No room mode needs to be selected.
2. Check the site address, then confirm its App access. The site opens in the
   App window. Enter its site passphrase if asked.
3. Choose **Start sharing**, then **Apps / Windows** or **Screens**.
   Select a source and send the new room invitation to your friends.

The site manages rooms and invitations; the App supplies capture.
If its operator has enabled SFU forwarding, turn off **Privacy mode** before
sharing to allow that fallback.

Manage [approved sites and the startup favorite](../../crates/piik-app/README.md#approved-site)
in the App launcher. If video cannot connect, use the [network guide](./network.md).

### Upgrading

The App and the site it serves come from the same Piik build. Replace them
together by following the [upgrade steps](../../crates/piik-app/README.md#upgrading);
an App and a page from different builds refuse to work with each other.

## Next steps

- [Sharing and playback](./sharing.md): picture settings, camera inputs and source sound.
- [Chat, voice and remote control](./chat-and-voice.md): talk, react or let a friend take control.
- [Troubleshooting](./troubleshooting.md): start from the symptom you see.
