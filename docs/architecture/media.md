# Media Pipeline

This file owns how a picture becomes packets: capture, encode, forward, adapt and
play, and which component owns each decision. User-visible quality behavior stays
in [media quality](../standards/media-quality.md); routing of edges is in
[signaling and routing](./routing.md).

## One Encoded Source, Many Children

The core model is that encoded frames are forwarded, not re-encoded per viewer.
A parent — the Host, a Native relay, or the embedded SFU — owns only the demand of
its direct children, and shares one encoded output among compatible children.

- A forwardable encoding is reused; a missing lower output is derived at the
  direct parent and shared.
- An encoding group produces the highest required output plus its lower
  fallbacks and stops upper outputs no child needs. That is a bounded envelope,
  not a promise that one Host encode serves everyone.
- Each child receives only its selected output. Preserving the original input,
  source clock and unaffected siblings matters more than minimizing encoders.
- Indirect descendants never appear in a parent's aggregate. `SFU upstream demand
  = max(direct-subscriber demands)`; `Host output demand = max(direct-P2P demand,
  SFU upstream demand)`.

Re-encoding per viewer would multiply CPU and latency and throw away the
encoder's own quality decisions. Sharing one output keeps the picture consistent
and lets the encoder see real frame statistics.

## Two Ways To Produce Video

**Browser capture.** The page captures a window, display, tab or camera and
sends it through ordinary WebRTC senders. `motion` is the content hint; each
share chooses H.264 with VP8 fallback through an actual sender probe unless the
Host selects one explicitly. For direct children, a source owner may share one
local encoded output among compatible children through Chromium's encoded-stream
API, which keeps the producer's real frames and the outgoing connection's RTP
clock. Each outgoing connection keeps stock ICE, DTLS-SRTP, bandwidth estimation,
pacing and loss recovery. Group compatibility covers source identity, negotiated
codec parameters, Host ceilings, content intent and degradation preference; an
unproven match falls back to ordinary senders.

**Windows native capture.** `piik-capture.exe` captures with Windows Graphics
Capture and encodes with Media Foundation (hardware H.264 when the probe finds
it) or its software VP8 path. Native capture uses the same two-output screen-share
shape within Host ceilings: the original and a half-size output at the same frame
rate, whose bitrate is `min(ceiling, max(150 kbps, ceiling/4))`. The source, its
clock and the higher output stay independent of lowest-output adaptation, so a
degraded viewer never cheapens the healthy picture.

The capture helper reports what this machine can do — resolutions, frame rate,
hardware and software codecs, process and system audio — because those are fact
about the machine, not a version. The App and page act on what it reports.

## Adaptation Ownership

WebRTC and str0m own congestion control, pacing, loss recovery and layer
selection. Piik supplies demand observations and representation lifetime; it does
not add a second congestion controller, a quality score, a resolution ladder or a
periodic rebalancer. Windows tuning stays inside WebRTC's stock adaptation: Piik
tunes the encoder's quantization history and sampling and lets WebRTC's
thresholds and controller decide. Browser scripts cannot tune the encoder's
internal QP thresholds at all.

This split is deliberate: a second controller would see a different estimator
sample and fight the real one, and a fixed ladder would ignore what the network
actually delivers.

## Startup And Bounds

A shared Browser producer starts at `maintain-resolution` and applies the
desired degradation preference only after five encoded frames. That startup
window protects a shared producer until its encoder settles, without a timer or a
periodic quality controller.

Encoded input is bounded: an access unit over 4 MiB is rejected rather than
expanding the limit, and derived output sizes have their own per-kind bounds. A
quiet or paused source is not evidence that a shared producer has failed.

## Audio

Browser capture asks for window audio on window surfaces and system audio on
monitor surfaces where the Browser exposes those hints. Generic Browser system
audio is never described as application-isolated: the UI leaves source scope to
what the Browser actually delivered. Window exclusion spans all render endpoints
rather than only the default playback device.

Source audio follows the picture. Its compact presets and 128 kbps default are
UI shortcuts; numeric bitrate requests are separate and bounded by the ceiling the
App reports. Optional Host commentary belongs to the share's audio and has its
own activation; room voice owns conversation microphones under the same route
controller. Opus output is stereo, but a stereo preference does not prove the
source has two distinct channels, so the UI never claims it does. Screen and
system audio stay unprocessed; microphone input uses browser voice processing by
default.

## Playback

A Viewer plays one video element fed by whichever upstream — direct peer, relay
or SFU — currently holds the committed route. Presentation and recovery are
fenced by the current media generation and route revision, and by a decoded
frame: a hidden page or a stalled frame is not proof of a healthy path. Switching
upstream never swaps the element's source in a way that loses the gesture needed
for playback, and recovery is bounded by the existing route operation rather than
a parallel player.

## Measured Limits

The numbers that constrain design live with their evidence in
[verification status](../verification-status.md). In short: HDR converts to SDR
once at capture; surround is out of scope; Windows capture termination and
browser motion cadence have open investigations in [TODO](../todo.md).
