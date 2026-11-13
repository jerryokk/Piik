# Capture, Audio, And Media Quality

This file owns the current Browser and App media contract.
[Media pipeline](../architecture/media.md) owns how capture, encoding,
forwarding and adaptation fit together, and [verification status](../verification-status.md)
records the measured limits. [Status](../status.md) indexes remaining acceptance.

## Capture And Controls

Room voice carries Host and Viewer conversation microphones, separate from
source sound and optional Host commentary. Its Browser capture owner supplies
device selection, input gain and optional speech processing, including on
App-backed pages. Settings stay local to the Browser origin; joining and
microphone activation are never remembered. Stopping picture capture does not
stop room voice. Source audio stays with the picture; room voice prioritizes
interactive latency and is not promised to stay synchronized with a delayed picture.
Microphone gain, master listening volume and each person's local volume range
0%–200%, default 100%, and master and per-person gains multiply. Per-person
levels belong to the current participant identity and retire with it. Listening
gain, deafen and per-person mute affect only local playback, never forwarding or
the picture's source audio.

Room voice defaults to 64 kbps mono, with Saver/Standard/Clear shortcuts at
32/64/96 kbps and a custom 6..320 kbps sender ceiling in the Host's voice
settings. Microphone and Peer-relay voice senders share that ceiling; WebRTC may
send less. Receive SDP accepts Opus's full 6..510 kbps range independently.

**Sharing settings → Sound → Include my microphone** mixes Host commentary into
the picture's audio. It starts off for each share and retires with it; Browser
capture reuses the stable HostAudio output and Native capture its mixer. Both
microphones may be enabled; the UI warns that listeners may hear the same speech
twice. Piik never mutes either microphone or switches between them automatically.

- The Host, in the App window, explicitly chooses Browser capture (display,
  application window, Browser tab or camera) or one native screen/window
  enumerated by the packaged capture boundary. Native capture never infers a
  target from a title. The Host may stop, synchronously pause/resume audio and
  video, or switch source.
- Only the chosen source requests permission. Camera input and thumbnails request
  an uncropped device format; sender constraints own later scaling. Camera
  capture opens no microphone; commentary and room voice need their own
  activation. A failed microphone device/processing change keeps the previous
  input and never replaces picture media. Browser processing preferences do not
  bypass device/system effects.
- Source-audio availability belongs to the Host capture owner: a live source
  input makes it available; a retained Native output track alone does not. It
  describes input presence, not loudness; quiet content, share pause and
  Viewer-local mute stay separate. Source and source-audio changes update it
  without rebuilding output or route; an opted-in room session carries it for
  the current publication, including late joins and signaling reconnects.
- An audio-only update with the same video preserves the Browser video sender,
  encoding state and startup evidence on direct and SFU paths; only a new video
  source owns a new startup baseline. On Browser direct edges, source/audio
  renegotiation waits for the current answer and coalesces newer intent into the
  next offer; a local queue alone does not serialize the remote exchange.
- Windows native capture follows an explicitly stretched active display path for
  a display or a window covering it, when the frame matches its desktop source
  dimensions; other frames keep their aspect ratio. Only encoded presentation
  changes, never game or display settings; vendor-private scaling is not inferred.
- Windows outputs preserve source aspect within the selected ceiling, rounded to
  even dimensions; small sources are not enlarged and the existing converter owns
  scaling. Source resizing creates no new room or route; frame envelopes carry
  actual output dimensions.
- Windows SDR conversion outputs limited-range BT.601 NV12 with matching H.264
  metadata. RGB capture and decoded video keep their input range/matrix through
  conversion and relay scaling; dimensions do not determine color space.
  Unspecified SDR follows the WebRTC convention.
- Windows HDR sources keep scRGB FP16 until platform tone mapping, source-display
  white-level adjustment and sRGB conversion produce SDR, once before fanout, for
  live capture and thumbnails; ordinary SDR keeps its copy path. Display color
  changes refresh capture input, not room authority or transport. This is
  HDR-to-SDR, not end-to-end HDR; [verification status](../verification-status.md)
  records the platform checks.
- Where Windows supports border control, the native picker offers **Show capture
  border**, off by default, kept by the Host page across native source and quality
  changes. Previews request borderless capture independently. Windows owns consent
  and final visibility; unsupported, pending, denied or failed access leaves
  capture and stop usable, and other capture sessions may keep the border.
- Display/window/tab capture asks for available audio by default; missing audio
  is reported but does not block video. Native screen capture can include system
  playback and native window capture selected-process audio when exposed.
- Shared Browser encoders normalize capture orientation into pixels before
  reusing frames across receivers; ordinary senders, or Browsers lacking the
  normalization APIs, keep Browser-owned orientation.
- Authoritative Browser pause disables the source and sender-owned tracks while
  keeping the room and routes. The connection owns its encoded-output pause even
  without shared encoding; the pool pauses a shared producer only when all
  members pause. Native pause keeps capture alive but stops session output.
  Black frames, track mute or network failure are not a user pause.
- The Host preview shows the capture stream directly and creates no Viewer or
  route. Hiding the page may pause only that local element, never capture,
  encoding or upload.
- Live quality changes update capture and sender-track constraints and sender
  ceilings without reopening selection or replacing a healthy route. Browser
  constraints express size ceilings, not ideal dimensions or fixed aspect:
  lowering one never requests a crop.
- Native quality changes prepare a new capture/encoder generation and install it
  behind the same encoded source and PeerConnections; audio-only changes update
  the current Opus encoder. A pending update may wait while the source produces
  no frames, but that wait never blocks pause, stop or connection control.
  Applied settings change only once the replacement is installed; required-output
  failure keeps the previous capture and profile, and failed lower outputs stay
  unavailable without rejecting a healthy original. Windows capture-device loss
  installs a fresh video generation, keeping source, connections and source
  audio; source closure and permission cancellation still end capture. Hardware
  H.264 uses the encoder's standard quality-versus-speed hint; VP8 uses libvpx
  realtime mode without that control or per-edge Browser adaptation. str0m owns
  Native transport, bandwidth estimation, pacing and recovery;
  `piik-media-forwarding` owns forwarding allocation.
- With the App available and topology optimization enabled, Browser H.264
  capture can use one local sender into the Native encoded fanout
  ([media pipeline](../architecture/media.md)). The Browser keeps
  preview, pause, settings and source selection; losing that ingress keeps
  capture and rebuilds the assigned Browser edges. VP8 uses Browser senders.

## Video Profiles

The Host remembers explicitly applied picture and source-audio settings, per
origin or, on App-opened pages, in the authorized App's optional preference
store (including across temporary public-link origins); a page without an
approved App keeps Browser storage. They seed a new page, never an active share. Explicit
edits and room recovery win over a late read; failed or superseded changes and
server snapshots never replace the preference, pending saves keep only the latest
applied choice, and storage failure never fails capture or startup. Capture permission, device/session
identity, routing policy and microphone activation are not retained, and reading
settings claims no Native media session.

The recommended profiles are ceilings, not delivery guarantees:

| Profile | Target | Video bitrate ceiling |
| --- | --- | ---: |
| `720p30` | 1280x720 at 30 fps | 3 Mbps |
| `1080p30` (default) | 1920x1080 at 30 fps | 5 Mbps |
| `1080p60` | 1920x1080 at 60 fps | 8 Mbps |

The wire carries numeric width/height ceilings; 480p–4K are UI shortcuts and
advanced settings accept custom even dimensions, typed values and sliders.
Width and height are one edit, and a size change caps frame rate in the same
operation when needed. Capture admits integer 1..240 fps and 0.1..240 Mbps,
further limited by size, even dimensions up to 8192 per axis and 36,864
macroblocks per picture, within an H.264 Level 5.2 budget (2,073,600 macroblocks/s,
240 Mbps). This admits 1080p240, 1440p144, 5120x1440p60 and 4K60 in either
orientation, but not full 8K. These are admission limits, not codec limits or a
device performance promise. Wire-valid requests never authorize exceeding local
capture, codec or allocation limits: controls use the intersection of App-reported
and local limits, and a remote claim cannot raise them. An unsupported request
fails the operation, not signaling; an unsupported saved preference neither seeds
capture nor is overwritten. Derived outputs stay within their source bitrate
ceiling, even below a usual lower-output floor. Codec preflight must observe real
encoded progress. Frame rate, bitrate and `maintain-resolution | balanced |
maintain-framerate` keep their controls; display video uses `contentHint = "motion"`.

Native H.264 is Constrained Baseline, validated by coding-tool subset under
RFC 6184 (any equivalent constraint bytes) with the actual SPS profile/level
reported; the encoder picks its level from the requested format. Native ingress
and relay accept only a profile/packetization compatible with their unchanged
encoded downstream, checked before same-source renegotiation alters the live
peer. Before Native derivation, every SPS's format, coded storage and visible
crop is validated; received summaries are not decoder allocation limits.
Decoder storage uses platform defaults and stays bounded, never a maximum-square
preallocation from per-axis ceilings.

Quality observations have independent numeric bounds: a valid capacity estimate
above the send rate remains evidence, invalid optional measurements become
unavailable, and invalid Native samples are discarded without closing control.
Identity, generation and state evidence must still pass before a sample affects
routing. SFU encoding descriptions grant no resources until admitted.

Framework-driven downscaling within the degradation preference is valid; a low
decoded resolution alone is not a defect or a reason for a quality floor. Review
avoidable disruption, recovery and healthy-child isolation instead.

Acceptance targets, pending the physical matrices in verification status:

- controlled P2P (RTT ≤ 40 ms, loss ≤ 1%): glass-to-glass p50 ≤ 150 ms, p95 ≤ 250 ms;
- regional SFU/UDP root: glass-to-glass p95 ≤ 350 ms; and
- 60 fps profile: no silent five-second run below 50 decoded fps without an
  explicit adaptation or user-visible limitation.

## Codec Decision

The pre-share selector is `VP8 | Auto | H264`, default Auto, locked while sharing,
local UI state only. Manual VP8 or H264 strictly selects that codec. Browser Auto
runs one bounded actual-sender probe, not an allowlist: a proved sender prefers
H.264 with VP8 fallback, otherwise VP8. The deadline covers setup, negotiation
and statistics; cancellation retires its resources even if a Browser operation
stays pending. The result is fixed per share;
[media pipeline](../architecture/media.md#two-ways-to-produce-video) owns the probe.

Codec selection keeps the primary codec's repair stream. str0m owns RTX, NACK
and feedback for Native and SFU receivers and their outgoing transport. Codec
restrictions never strip recovery or disable SRTP replay protection.
The Browser chooses its encoder; a reported H.264 codec does not prove hardware
acceleration, and Web content cannot select MFT, NVENC, AMF or QSV.

Windows native capture uses the same controls: H264 is the hardware path and VP8
bundled libvpx. Enumeration is only a candidate list; startup tries the
preferred encoder, then other adapter/encoder candidates within a four-second
selection budget, and native relay encoding uses the same owner. Manual H264
requires real H.264 output and never changes codec. Auto paces synthetic frames
at the selected size and rate through each candidate's live pipeline with the
Browser probe's half-second warmup, one-second sample and 100 ms lag allowance,
excluding cold start; a deficit rejects that
candidate at once, and a candidate starts only if the budget can cover its
measurement. A passing encoder is handed to live output; without one, Auto starts
VP8 as manual VP8 does. A faster software encode never replaces a sustaining
H.264 encoder. This is a cadence check, not a perceptual score or a promise under
future GPU load. The actual codec owns the shared source, preview and relay;
the selected device is first choice for later quality/source replacement.

## Framework-Owned Adaptation

Requested ceilings, committed network allocation and actual output are separate
facts. Congestion never turns a reduced output into a new requested ceiling;
recovery demand survives lower-layer selection and reaches the existing encoder
or dormant-output owner without recreating the connection.

Native outputs share one source presentation timeline across encoder groups,
dormancy and capture replacement; encoder completion and pipe arrival do not
create another clock. Internal RTP copies reuse the original ingress's
remote-to-local mapping, and only that ingress estimates network delay.

Each Browser direct, relay or SFU video sender owns a separate track; the
original stays a source/local track, never attached to an outbound sender.
Display capture with raw-frame streams forwards a bounded input clone into a
generated sender track so encoder feedback cannot shrink its own capture input;
other tracks use ordinary clones. A 1 fps capture constraint keeps idle refresh;
sender adaptation has no FPS or resolution floor. Replacement and retirement stop
both owned ends; an unexpected input end belongs to that sending operation, not
the source or siblings. Connection retirement also settles pending sender and
negotiation waits, since closing a PeerConnection alone may not; a replacement
still waits for the retired owner's cleanup.

Encoder-group admission uses current native demand: a new or pending join never
displaces an existing rate owner. Eligible Browser P2P parents use the
source-owned pool in [media pipeline](../architecture/media.md):
compatible direct children share one local producer, which must fit each child's
native allocation (a quiet scene's low byte rate is not proof). Reuse preserves the
current output's rate and quality through recovery-frame commitment; rejected
membership keeps current delivery. Each outgoing connection keeps native
transport, allocation and recovery via a tiny carrier that preserves its frame
rate, while the producer keeps the Host's ceilings and degradation preference.
Unsupported APIs or failed pooling use ordinary senders; codec incompatibility
retires only that connection's membership.

A new producer seeds its local WebRTC start rate from that allocation, without a
minimum or a change to the outgoing connection's estimate, and its startup
protection begins at actual outgoing publication, excluding local warmup and
paused frames. The carrier uses screen-content transport probing. Pending
separation stays required while another compatible member needs a higher native
budget; quiet output alone cannot cancel it. A recovery frame already being
written keeps its owner until completion, including across pause/resume.
Ordinary fallback restores the picture sender limits.

Each ordinary Browser PeerConnection owns stock congestion control and sender
adaptation. Ordinary clones share one source, so framework source-wants may
partially reduce frames for siblings; outputs may differ and there is no
room-wide minimum. The Host's one Browser SFU publication uses the share codec
and ceilings with screen-share simulcast: two same-rate outputs whose lower one
follows the budget with a floor, one output for tiny sources, no backup codec.
[Media pipeline](../architecture/media.md#two-ways-to-produce-video) owns the
formula.

The embedded SFU forwards an existing suitable encoding to each subscriber, with
str0m owning per-subscriber estimation, pacing and loss recovery and
`piik-media-forwarding` owning allocation and RTP projection; it never transcodes.
Direct-subscriber demand controls the publisher's active output prefix. Target,
forwarded and recovery-preparation demand stay distinct from delivery. Viewport
size never chooses a layer, since a Viewer may relay. Native capture keeps source
resolution and frame-rate ownership across its loopback preview, and Native Host
SFU publication uses the encoded source without re-encoding that preview.

Each Native parent considers only its direct children and its admitted SFU
publication; the SFU upstream request is the maximum of its direct demands,
combined with direct Peer demand under one aggregate upload budget. Native
capture uses the same two-output screen-share construction within Host ceilings;
its source, source clock and higher output stay independent of lowest-output
bitrate adaptation. A single rendition's encoder budget follows the largest
remote allocation within the ceiling, and it cannot give its children different
encoded qualities; local preview cannot mask remote demand. A locally
rate-controlled output also has unmet demand within its current layer, which raw
forwarding cannot synthesize. Probe traffic is exploration, not a capacity
reduction: allocation and encoder consumers keep the same budget through probe
settling. Compatible children reuse an encoded output; a missing lower output
starts one shared decode/scale/encode group that produces the highest needed
output and its fallbacks and stops unused upper outputs, while healthy forwarding
never starts a decoder. Each child receives only its selected output. Multiple
outputs carry real encode and upload costs. Attachment or sending failure belongs
to that edge, never the shared source or siblings.

Keyframe requests keep their output scope through coalescing; a relay asks its
upstream once per batch. Only independent output fulfills a recovery request; a
delayed keyframe leaves it pending while delta frames continue and does not fail
a healthy output, though initial admission needs a decodable start. Relay
assembly bounds packet storage and stalled progress, requesting recovery for an
incomplete frame without waiting for a memory-sized window.

Packet identity and counters belong to each outbound edge; unused outputs do not
inflate them. Bounded workers and queues isolate slow children and retire with
the source generation. Windows local outputs each run one stock WebRTC
encoding/adaptation pipeline (H.264 via NV12/MFT with parsed-QP adaptation, VP8
via libvpx); resolution/FPS changes keep capture and transport identities and
update forwarding metadata without resetting siblings. Windows tunes the stock
QP history and sampling for brief interruptions and low-frame-rate recovery;
codec thresholds and the adaptation controller stay WebRTC-owned. Compatible direct-child
requests are grouped and change rate in place; joining another group waits for
accepted recovery on the same PeerConnection and estimator. Physical slots are
bounded by admitted consumers, not fixed presets. This does not establish full
Browser parity, hardware-overload acceptance or seamless very-low-bandwidth adaptation.

A Native Viewer forwards suitable H.264/VP8 and Opus unchanged and derives only
missing lower outputs; each outbound PeerConnection owns its RTP identity and
transport feedback. For a persistently degraded edge, the existing quality
operation may prepare a Browser WebRTC sender from the local bridge as that edge's
candidate; the old edge stays live until the Viewer completes the route owner's
relative proof, failure rolls back, and a later operation may return the edge to
the shared native source. This gives one difficult path per-sender adaptation
without lowering the shared representation for healthy paths, and adds no score,
timer or room setting.

Piik has no application bitrate/resolution ladder, scene detector, periodic
quality controller, manual SFU layer selector or room-wide lowest-common target.

## Screen Audio

The Host's playback volume scales mixed source sound and commentary 0%–200%,
starting at 100% each share. Browser gain precedes outgoing fanout; native gain
precedes the shared Opus encoder. Source replacement keeps the level; room voice
is independent and Viewer volume is local only.

Opus content may be mono or stereo; Browser and Native mixing output stereo. A
stereo preference or two-channel declaration does not prove distinct channels.
Surround is out of scope: it needs capture, layout, mixing, codec and playback support.

Display audio uses `contentHint = "music"`, with 64/128/192 kbps shortcuts, a
128 kbps default and a custom ceiling; senders support 6..320 kbps and the wire
uses Opus's 6..510 kbps. The native 20 ms encoder bounds each payload to 1152
bytes, leaving room for RTP/extensions/SRTP in a 1200-byte datagram; Opus RTP
keeps frames intact. A request above the sender ceiling fails that operation
without retiring native control.

Browser, Native and SFU receive answers request stereo and the full Opus range,
including renegotiation. Browser senders honor a smaller negotiated receive limit,
else libwebrtc's default. Native/SFU unchanged forwarding rejects, before binding,
an edge whose receiver declares a smaller or no limit, without reducing the
source or siblings. The SFU publication uses stereo with DTX and RED disabled,
trading burst-loss resilience for bounded traffic.

A Native audio-process failure does not end healthy video; only a later explicit
source replacement resumes audio through the existing owner.

Optional Windows screen-audio exclusion binds one process tree by PID and
creation time. It lasts
for the share and survives source-audio off/on and refresh, but is never saved
across App runs or rebound by executable name; a window source or explicit
"no exclusion" clears it. A requested exclusion retires previous source audio
first; failure or target exit leaves source audio silent while video and the
microphone continue, until an explicit audio/source selection. When target and
options are unchanged, live sound changes update only the audio input, keeping
video generation, encoder, connections and quality evidence. A missing capability
rejects the request rather than discarding the exclusion.

## Observable Truth

Requested resolution, frame rate, bitrate, content intent, codec and audio
quality are inputs or ceilings; sender readback and current RTCStats are the
result. Connection detail may show actual path, codec, resolution, FPS, bitrate,
loss, jitter, RTT and encode/decode behavior when exposed; missing or reset
counters stay unknown.

Native-edge convergence consumes only a persistent categorical sender
limitation and same-Viewer proof over a real prepared candidate, never a weighted
or global score. Native evidence includes committed allocation deficiency, which
fitting a reduced output (or restricting a live encoder) does not clear. Source
bitrate variation does not prove congestion, a failed probe does not change the
incumbent's category, and intentionally inactive encodings are not bandwidth
failures. Missing feedback stays unknown; the category neither locates a
physical bottleneck nor ranks routes by ceilings.

Web pages cannot guarantee background capture, playback or relay execution
after Browser/OS suspension; these are lifecycle boundaries, not keepalive features.
