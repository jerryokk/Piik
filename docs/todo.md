# Current TODO Ledger

Last reviewed: 2026-11-10

This ledger contains unfinished work and explicit owner holds. Product modules
own behavior; [verification status](./verification-status.md) records measured
evidence and Git/PRs own completed history.

## Now

- [ ] **Public-invitation candidate retirement.** Capture the parent and server
  reason when a candidate is retired before its first SDP offer. Keep this
  separate from candidates that actually enter ICE and fail.
- [ ] **Browser sharing falls to low source FPS during motion.** Trace raw
  capture, sender-owned track input and encoding on Windows Chrome with mixed
  direct/SFU delivery. The observed source fell to roughly 2 fps during video
  or game playback; reproduce with real window/display capture. Keep the
  separate SFU packet-loss stage distinct from source-frame production.
- [ ] **Unclassified capture termination in earlier Windows reports.** Identify
  the Native failure stage and HRESULT from the reports collected before the Rust
  capture path. Their Browser reports show Native failure before page cleanup but
  do not identify its trigger.
- [ ] **Room voice in Native display sound.** Resolve the remaining case where
  system loopback recaptures Browser voice, while excluding the Browser process
  also removes its other audio. Browser own-document exclusion and Native
  process-tree exclusion are already implemented; acoustic echo and duplicate
  commentary retain their separate microphone owners.
- [ ] **Virtual-LAN page loading.** Investigate the UU report where only the tab
  title appears and the page never loads. Static compression reduces transfer
  size; capture the stalled document/script response to locate this failure.
- [ ] **Native H264 scene-change latency.** Reduce large-AU send-queue delay
  while preserving bitrate restoration. The delay is in the send queue, and a
  one-frame HRD override prevents recovery; do not use it.
- [ ] **Mouse input stops when Host Chrome is minimized (#502).** Follow
  public report #502 on the original Windows host. The supplied reports retain
  the grant and receive authorized mouse events, but Windows `SendInput` rejects a
  button event and subsequent release attempts. Identify that OS refusal trigger and verify held-button recovery;
  the local controlled Chrome trial did not reproduce it.
- [ ] **Acceptance gates.** Run the App media gates against release builds (the
  browser fan-out gate already passes on the packaged App) and complete
  public-link cross-network acceptance, which needs an external host. Do not
  relax frame-delivery thresholds to close this item. Current evidence is owned
  by [verification status](./verification-status.md).

## Next

- [ ] **Stronger microphone noise suppression (#518).** Assess the
  requested speech-processing option (public report #518)
  through the shared microphone input. Browser voice processing is already the
  default; keep screen and system audio unprocessed.

## Held By Owner

- [ ] **Additional collaboration cursors.** Deferred by the owner. Keep the
  shared system mouse; do not add multi-cursor or pointer-ownership visualization.
- [ ] **Persistent-loss estimator replacement.** Deferred on 2026-10-18.
  Keep mature congestion control and accept quality reduction under sustained
  high loss; concrete accounting/recovery defects remain ordinary bug fixes.
  [Verification status](./verification-status.md) records the measured basis.
- [ ] **Multiple remote-control screens.** Deferred on 2026-10-17. Keep one
  shared source; Viewer screen switching and simultaneous screens are later work.
- [ ] **32-bit Windows App.** Deferred on 2026-10-02. No package is planned.
- [ ] **Windows ARM64 App.** Deferred on 2026-10-03. Keep Windows downloads x64-only.
- [ ] **Alternative public-invitation providers.** Deferred on 2026-10-16.
  Keep Cloudflare Quick Tunnel; other providers have not been assessed against
  it.

## Parked Product Work

- **Platform output.** A registered receiver may act as an ordinary Viewer once
  its delivery path is verified.
- **Additional languages.** Review community catalogs and rendered UI through
  the [translation guide](./guide/translating.md).
- **Windows code signing.** Revisit when a trusted signing service is available;
  sign executables before archive checksums. Signing does not guarantee the end
  of antivirus cloud scanning.
- **Automatic local chat history.** Manual TXT export already exists. Automatic
  storage needs a stable room-incarnation identity, bounded retention and deletion;
  reusable room codes must not combine conversations.
- **Phone as a Host microphone.** Revisit only for a concrete use case that
  justifies the added pairing and input boundary.
- **Multiple simultaneous sharers.** Define publication/subscription ownership
  before prototyping; the current one-Host contract remains.
- **Semantic reconstruction.** Revisit the domain/room/participant model after
  the acceptance gates close: map existing behavior and ownership and remove
  accidental constraints, preserving the reviewed recovery semantics.
