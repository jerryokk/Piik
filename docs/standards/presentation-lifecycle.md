# Presentation And Lifecycle

This file owns user-visible workflow, presentation authority, presence, and
Browser lifecycle promises. Component structure, CSS, exact copy, and routine UI
tests belong in source code.

## Product Surface

The first screen is the working tool, not a marketing page. Once site access is
available, starting a share and joining a room are equally discoverable; joining
expands the four-digit room form in place, and `/join` remains a direct entry.
Hosting happens only in the Piik App window; an ordinary Browser shows an
App-required notice at the Host entry and keeps Viewer/Join entry. Users never
choose a media topology.

Host entry requires site access and a validated capability description. Initial
entry, retry and password recovery load the same prerequisites, so failed
authentication cannot leave a later entry with absent capability state. A
capability-load failure stays retryable and implies nothing about service
availability. Viewer/Join entry does not need this description.

The Host workspace centers the capture preview, room code, invitation and common
share controls. The Viewer workspace centers one persistent 16:9 video stage,
room identity, current state, roster, and one manual media-reconnect action.
On a narrow portrait phone the stage drops the decorative television frame and
runs to the screen edges, because the frame there costs picture rather than
describing a room; the room, deck and panels below keep their gutter. Controls
and text stay usable without overlap or horizontal overflow on desktop and
mobile viewports.

Personal room-tool presentation, incoming-route and remote-control shortcut
preferences are remembered per Browser origin across rooms and reloads, under
their domain owners' capability and room-policy constraints (the App applies
shortcuts only in memory). Restoring preferences never restores admission,
control grants, held input, passwords or drafts, and never starts capture or
remote control.

A capable, opted-in authenticated room session outlives its current publication.
Stopping capture retires media, not chat; restoring an existing room joins it
without starting capture, and entering the page alone creates no room. Room
close, lost authority and page teardown retire the session, including its App
control connection and pending discovery, without waiting for the denied page to
unmount. Connected Viewers may keep interacting while the Host page is absent,
provided the room service and their sessions remain.
[Rooms and access](./rooms-access.md#room-interactions) owns interaction authority
and retention; [status](../status.md#interaction-phase) owns readiness.

The Host can explicitly **Create room** before sharing. Chat and voice are tabs
in the interaction window, including its detached presentation; closing or
changing that window neither leaves voice nor stops capture. Microphone,
admission and connection states stay literal: an enabled microphone is not proof
of speech. The [room voice contract](./rooms-access.md#room-voice) owns
activation and membership. The voice roster follows connected members at once;
server reconnect grace does not keep a departed person on screen.

On capable desktop browsers the interaction window can move to Document
Picture-in-Picture, keeping the session's messages, reactions and draft; read
visibility and guidance follow its current document. Closing it returns it to the
page; room retirement or opener teardown closes it, including a late open. This
promises no game-overlay transparency, click-through or exclusive-fullscreen coverage.

The Piik App shows its launcher and this same application in its one WebView2
window; destinations outside App-owned and approved origins open in the system
browser. The launcher offers Local and temporary public invitation, and the
running App is already available to its approved Sites before either mode
starts. A Site's explicit App action leads to local approval of its exact origin;
the launcher lists and revokes approvals independently. Starring one approved
Site makes an ordinary launch open it, otherwise the launcher opens; the star
grants no access, starts no room and is cleared by revoking that Site. Explicit
command-line entry takes precedence. The ready App button returns to the
launcher in every mode. A pending or healthy Local/public room keeps its startup
selection; reopening the launcher does not stop or replace it. After a failed
attempt has fully retired the launcher can retry or switch mode in the same run;
changing a healthy room's mode requires restarting the App.

An App-entry page selects that process's control endpoint and keeps it through
reloads in its Browser session; if the endpoint becomes unavailable, discovery
stays with it until a new explicit entry. Endpoint selection grants neither Site
access nor capture permission. Activating a page supplies native capability
without restarting its room or healthy media. App-created Local/public Host pages
open at their automatically approved loopback origin; an approved origin needs no
repeat confirmation but still verifies authorized App health. The shared App
button reflects verified health only, never a remembered activation, and idle
verification holds no media session or polling loop. Viewer links at an activated
origin may use the running App without changing the Viewer UI. Existing Browser
shares stay unchanged when the App connects; explicit source replacement uses the
share's source transaction.

Server and App share one control layout and vocabulary. Deployment capabilities
may fix a control on or off but keep its place; disabled controls explain why in
text and pure-visual modes. Real differences stay explicit: site authorization,
opt-in diagnostics and device capture/audio capabilities are not alternate
versions of the product UI.

The shared source selector orders Browser, Camera, Window and Screen, defaulting
to Window once the App is detected and otherwise to the first supported Browser or
Camera choice. Choices need their APIs; temporary restrictions disable a supported
choice. It never chooses a window automatically. Discovery never blocks Browser
selection or overrides a chosen tab; the App entry stays in the header and a new
connection refreshes the open list. Refresh stays reachable after failed
discovery; pending attempts are shared and completed failures do not suppress
later ones. Idle Host pages hold no App control session. Cancelling the selector
or ending a share releases its unused control session, covering discovery and the
opening handshake; closing the selector does not cancel a separate site approval,
and an obsolete operation cannot install or retire another's media. Camera
thumbnails own only temporary inputs retired with their tab or picker; an active
shared camera is borrowed, never reopened or stopped. Optional native ingress
uses an already available connection; if not ready at Browser startup, that share
keeps Browser senders.

Native Host startup acquires its Browser media connection before capture.
Cancellation owns that connection immediately; native share ownership and required
audio tracks come from the accepted App response. Browser initialization failure
stays distinct from capture permission failure.

A reachable App that answers with a different control protocol is distinct from
an absent App: discovery keeps looking for the matching App this page belongs to
before reporting the mismatch, and the selector keeps Browser operation. A false
capability reads as unavailable, and commands keep their strict protocol. An
empty source list means enumeration succeeded; connection, capability and
enumeration failures keep their own feedback. Discovery has one bounded deadline
that allows for
browser local-network consent; a failed fetch alone does not prove denial.
Optional Viewer reception uses Browser media immediately when local App access
still needs consent or is denied, rechecks on a later attempt, and a later grant
does not replace healthy Browser media. App Local needs no cross-address-space
consent; Browsers without a permission query keep bounded discovery.

## Visual Language

Chinese, English and pure-visual modes express the same typed state and command
catalog. On first use the App and Server UI match the Browser's primary language
against registered locales, else English; every Simplified and Traditional
Chinese tag (such as `zh-CN`, `zh-TW`, `zh-HK`, `zh-Hans`, `zh-Hant`) selects the
current Chinese catalog. Pure-visual mode is an explicit choice. A later explicit
choice persists and wins, including over App launch handoff, which applies only
the presentation domains the user chose; unmarked handoff hints never overwrite
saved preferences. The launcher falls back to English for other UI languages.
Text modes use concise copy.

The [visual language](./visual-language.md) is the single owner of illustrative
roles and objects, semantic colour, panel grammar, motion, accessible input and
responsive hierarchy.

## Playback Ownership

One `<video>` element owns Viewer playback and frame proof. The shared playback
bar reflects it and provides local play/pause, mute, volume, theater,
picture-in-picture, fullscreen and the existing reconnect action, staying
reachable while waiting or disconnected. Theater and fullscreen are separate;
page fullscreen includes the bar, while Safari's video-only fullscreen uses system
controls and native events. Identity, connection details and topology stay in the
deck. Room tools outside the player close on fullscreen entry; a tool returned
during fullscreen stays closed and reopens afterwards with its session and draft.
The fullscreen bar keeps the danmaku toggle, not an interaction-window entry.

Playback settings reuse shared controls in an anchored popover without resizing
video. Local fit defaults to contain; cover may crop, stretch changes proportions.
Each rotation turns the local picture 90° clockwise, independent of device
auto-rotation, in page playback including theater and DOM fullscreen; system video
presentations keep browser behavior. None of these change capture or encoding.
Remote input and magnification use the rendered picture geometry. Incoming-route
choice follows the [routing contract](./routing-transport.md#viewer-route-choice);
its value is acknowledged intent, not evidence of the playing route.

Replacement video bindings ignore Viewer-local pause: while the Host plays they
attempt playback subject to autoplay permission, and while the Host is paused they
wait for resume. There is no remembered playback intent across source changes or
reconnects. Audio changes on the same binding update only its audio consumer,
keeping frame proof and local playback. A pending `play()` canceled by pause or
rebinding is not a media failure and does not invalidate proof.

The bar is one translucent capsule at the picture's lower edge, grouping playback
and sound, remote input, viewing options (including on-screen chat) and window
actions, with PiP, theater and fullscreen together at the trailing edge, all on one
row. Narrow players shorten the volume slider without its percentage, and move
fit/rotation into settings. A bar that cannot hold a full-size slider keeps every
button at a tappable size with a real gap and gives the slider up instead: the
settings menu then carries it as a sound section. At phone width PiP and theater
also leave the bar and appear there, so the row keeps mute, remote input,
keyboard, settings and fullscreen at a usable size. Playing
video hides the bar after two idle seconds, following
[Media Chrome's default](https://github.com/muxinc/media-chrome/blob/main/docs/src/pages/docs/en/components/media-controller.md#autohide);
pause or unavailable playback keeps it visible, and hover, focus, dragging or an
open tooltip hold it. Mouse movement reveals it; a touch tap toggles only
visibility. During remote control, picture input belongs to the remote and the
bezel pull reveals the bar; pointer-locked movement never does. One presentation
hook with one idle timer owns visibility and never changes playback or route;
controls follow the [motion grammar](./visual-language.md#motion-grammar).

A picture click does not pause; the playback button and focused-video shortcut do.
Desktop double-click toggles fullscreen. Remote input consumes its own clicks and
keys; controls and overlays keep their immediate behavior.

Host source-audio feedback describes the selected source independently of room
voice. Viewer picture-sound controls require a received audio track and, when the
Host supplies availability, an available source or microphone input (including
commentary). Otherwise they are disabled with the no-audio explanation while the
Viewer's volume and mute preferences persist; Hosts without this fact keep
received-track feedback. Silence never implies input absence, and these controls
never change capture, transport or relay audio.

Viewer volume starts on native audio output. A setting other than 100% activates
a local [Web Audio](https://www.w3.org/TR/webaudio/) stream source and gain, 0–200%
amplitude (also avoiding iPhone's system-owned `video.volume`); loud peaks can
clip above 100%. That graph exclusively owns sound while running, follows pause
and mute, rebinds on track change, never modifies the received or relayed track,
and closes on unmount. Without gain support, or in video-only system fullscreen,
native controls own audio at up to 100%, never two outputs at once.

Picture-in-picture uses the browser's native video window, including Safari's
presentation-mode API. Native enter/leave events own its state; closing it or
replacing the stream creates no other playback owner. Entry exits this video's
other fullscreen mode; unmount closes only this video's window. Unavailable
capabilities stay disabled with an explanation. The window holds video and
system controls, not page overlays. See the [PiP specification](https://w3c.github.io/picture-in-picture/)
and [Safari integration](https://developer.apple.com/documentation/webkitjs/adding_picture_in_picture_to_your_safari_media_controls).

Viewer-local actions never change Host capture or another Viewer; reconnect keeps
the existing route-operation owner. The Host preview reuses the bar and starts
muted; local pause, fit, rotation, theater and fullscreen affect only the preview,
while labelled sharing controls own authoritative pause and stop. Its speaker
control sets the shared output volume ([source audio](./media-quality.md#screen-audio)),
not local monitoring; when the connected App lacks source volume, sharing
continues and the control is disabled with an update hint. By default a
hidden or unfocused Host page suspends local preview except in Picture-in-Picture;
the Host can disable this in playback settings (remembered per origin). Returning
or changing this preference keeps an explicit local pause.

Autoplay rejection exposes the play action. A connection, track or `playing`
event alone does not prove the route is visible: the Viewer removes its blocking
presentation only after a current-generation frame is composited, using
`requestVideoFrameCallback()` where available and decoded progress otherwise.

## State And Recovery

Access, Host presence and pause, route prepare/active, P2P/SFU connection, media
binding, frame proof, autoplay and terminal failure are typed facts interpreted by
one presentation reducer. Older route or media generations cannot overwrite the
current result, and raw server errors are never rendered directly.

The room route revision orders control messages; it is not a media identity.
Reauthentication replaces the route snapshot authoritatively even if a new
controller restarted numbering; ordinary updates stay monotonic within that
authority. A Viewer frame belongs to its local media generation and stays current
across unrelated revisions until that binding is replaced, invalidated or
terminally failed. Pending candidates never own the badge, status line or overlay
while committed media remains proved. One visual projection maps facts to title,
status, notices and overlays, and no view owns another's state. Connection,
playback and quality limitation stay separate; transport snapshots prove neither
playback nor quality. The [status preview](./media-status.md) records the visual
vocabulary and evidence for participant lamps.

Excluding the Browser's capture/play authorization UI, a Viewer request targets a
first visible frame within three seconds, continuously showing a truthful
accessible connection stage meanwhile, never a black screen, ICE-connected state
or unproved `playing` event.

- A proved current frame stays visible behind non-terminal recovery state.
- The [status projection](./media-status.md#projection-rules) owns television,
  Header, notices, participant lamps and title, and also the Host couch and Viewer
  overview; none may turn an unproved frame into playback or ordinary quality into
  failure. The Host's own pawn has no lamp. Committed `mediaReady` owns picture
  readiness; diagnostics and quality samples cannot override it either way. Room
  presence and a committed voice route stay valid without a picture.
- Viewers never infer another Viewer's quality or treat missing readiness as
  terminal failure.
- Overlays explain only a missing usable picture, authoritative Host pause or a
  required user action; ordinary quality warnings never cover playback.
- Host pause keeps the current frame and waits for resume. Host signaling loss
  alone does not invalidate healthy media.
- Exact media or terminal route failure invalidates frame proof before showing a
  retained frozen background with recovery or failure state.
- Room not found is distinct from access denial and does not prompt for a password.
- Manual reconnect recovers the same route; it does not select a new parent.
- Actionable overlays may appear in fullscreen; state never forces an exit.

Decoded-frame silence is not a route-failure signal: a live source can stop
repainting, and media-framework failures, not an application timer, own recovery.
First-frame admission and candidate-relative proof still need decoded progress.
Visibility, freeze and pagehide invalidate quality observations, not proved
playback; returning re-arms observation, and existing recovery still needs a fresh
frame to clear. SFU media survives transient room-signaling loss, but Browser/OS
suspension and page reclamation stay outside Web guarantees.

The local Native media bridge owns bounded ICE recovery on its existing connection
and stream; its offer identity fences each answer, and retirement cancels pending
negotiation without recreating a source or receiver. Connection recovery does not
assert fresh media, and an intentionally paused source is not a failure. Terminal
bridge failure disables Native reception for the rest of the Viewer session before
route recovery; future peers use Browser reception, healthy peers stay. Parent
network failure alone does not prove Native unavailable.

The Native Host's bridge stays required by its Browser share owner, supplying
preview and Browser quality candidates through source and quality changes; its
terminal failure retires the share through the Host lifecycle. A downstream
network failure recovers only that edge. Browser capture's optional Native fanout
can fall back to Browser senders because the Browser owns its source.

## Presence And Diagnostics

Host and Viewer pages show the authoritative online Viewer count and roster,
independent of the media graph. A display name is Browser-local, NFC-normalized,
at most 24 Unicode code points, and rejects unsafe control or bidirectional
characters. It is a label, not an account, authorization identity or routing input.
Duplicates are allowed and get a room-scoped peer suffix only when needed. Chat
applies the same rule to recorded senders; repeated messages from one peer create
no new identity, and media readiness never owns identity.

Connection details are progressive, with one card shape for P2P, peer relay and
SFU Viewers. Summaries prioritize actual resolution, FPS, bitrate and loss; deeper
fields appear only when meaningful, and topology is a separate view. Native
bandwidth or CPU warnings describe the exact outbound sender that adapted, not the
physical bottleneck or the Viewer's receive path. The Host's participant freeze
warning needs fresh receive evidence for the current upstream with a positive
freeze count or duration in that window; `*Delta` fields never inherit older
values, and the existing evidence expiry clears stale warnings. Per-edge detail
stays available without a global warning chosen from the first limited child.
Selected-candidate addresses may appear only on the Browser owning that
PeerConnection and are never uploaded, persisted or used for identity or routing.
Explicit Debug mode can export a bounded report through the
[diagnostic owner](./configuration.md#diagnostics); ordinary details never collect it.

Current scope is the Windows x64 App Host (Browser and native capture in its
window), Web Viewers, Browser relay, and optional Native Viewer media beneath the
same pages. Desktop and mobile Browsers are Viewer targets. Reliable background
relay and App-owned SFU remain later work.

## Primary References

- [WHATWG Web Storage](https://html.spec.whatwg.org/multipage/webstorage.html)
- [ECMAScript string normalization](https://tc39.es/ecma262/multipage/text-processing.html#sec-string.prototype.normalize)
- [Unicode UAX #31](https://www.unicode.org/reports/tr31/)
