# Verification Status

Last updated: 2026-11-13

This file records what has actually been observed, so product claims stay within
their evidence. [Status](./status.md) owns current readiness; architecture
describes intended behavior, not measured success.

## Current Evidence

Windows App and Browser are the supported acceptance targets. Native window
sharing has passed real Chrome checks in Auto, H264 and VP8 modes; H264 display
sharing also passes. These checks cover decoded frames, live quality changes,
pause/resume and source lifetime, not only signaling. P2P and SFU room voice pass
the normal Host/Viewer lifecycle with synthetic microphones; the P2P path also
covers relay. Browser VP8 and native H264 publication through the embedded SFU
pass their media and recovery gates. The native gate proves sustained delivery
with automatic layer selection, not a fixed high-resolution or bandwidth
guarantee.

The browser fan-out gate passes: one Native Host encodes once, a direct Browser
Viewer and a Viewer whose Native relay re-forwards the received encoding both
decode at the source rate for the measured window, and the gate also covers a
live quality change, pause/resume, a source-audio change and recovery after the
relay App exits. A relay's Viewer acquires its Native receive connection and
local bridge a little after the direct one; the gate starts its steady window
once both decode, and the sustained-rate requirement is unchanged.

The remaining acceptance work is tracked in [TODO](./todo.md).

## Device Evidence Boundary

- Windows HDR-to-SDR has synthetic GPU and decoded-video evidence; it does not
  establish Browser-capture HDR conversion or end-to-end HDR.
- Audio-exclusion availability follows actual platform capability; supported
  interfaces and their isolation have been checked on Windows only.
- Room-voice acceptance uses synthetic microphones. It establishes the route and
  membership lifecycle, not acoustic echo performance on every physical device.
- Remote-control UI and routed input have local evidence. OS permissions,
  protected/admin applications and game compatibility retain their limits.
- Virtual gamepads are covered by mapping, slot-contention and lifecycle tests
  against a recorded backend. Real ViGEmBus enumeration, the four-slot budget
  shared with the Host's own controllers, and in-game player assignment have not
  been measured on hardware.
- A two-device Windows virtual-LAN trial established direct packaged-App web
  access and remote video after scoped firewall permission. Other networks must
  likewise carry HTTP/WebSocket and media UDP.
- iPhone/iPad Viewer playback uses WebKit; the WebKit-specific fullscreen and
  picture-in-picture paths are covered by their unit tests, not by a device run.

## Adaptation Boundaries

WebRTC/str0m own adaptation. Native recovery fixes are distinct from
Browser-controlled recovery and sustained-loss limits. Browser scripts cannot
tune the encoder's internal QP thresholds.

For the persistent-loss hold (a 12 Mbps link, 320 ms RTT and 15% loss), a
full-budget video settled near 1.16 Mbps while a quarter-budget video fell to
roughly 13 kbps. That is why the estimator is not replaced: the observed floor is
the mature controller trading quality for delivery, not a defect to override.

Stereo evidence measured more than 77 dB separation through the SFU, more than
57 dB through a relay and a Native regression of 20 dB or better. HDR-to-SDR
conversion measured GPU medians of 0.045/0.240 ms on an RTX device and
2.39/11.17 ms on an AMD device. Audio-exclusion probes took about 300/163 ms, and
retirement 42/64 ms. The virtual-LAN trial carried 1920x1080 at 20 fps with 32 ms
latency between two packaged Apps.

Persistence was exercised to storage exhaustion: with the SQLite page count
capped, room creation failed with `SQLITE_FULL` without consuming authority,
free codes or room rows, and succeeded again once capacity returned. The
restored store then read the created room back. This is the current Rust store,
not an injected-failure simulation.

## Interpretation

Evidence applies to its tested revision, device and workload. Builds and local
controls establish their named behavior; they do not promise every remote network
or device or a population NAT success rate. Browser/OS suspension is not a
keepalive guarantee.
