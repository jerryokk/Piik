# Routing And Transport

This file owns the user-visible routing and transport contract.
[Signaling and routing](../architecture/routing.md) owns the controller algorithm
and resource invariants; [configuration](./configuration.md) owns ports and
[self-hosting](../operations/self-hosting.md) owns service setup.

## Topology

- Media defaults to automatic P2P-first routing. Viewers may constrain their own
  incoming hop; parent selection, transport and adaptation remain automatic.
- The Host is the only shared-picture source. Every Viewer has at most one active upstream, and
  the committed media graph is acyclic and source-reachable.
- A Viewer may relay its received Browser track to bounded peer children. This
  is standard WebRTC receive, decode, and per-child re-encode, not shared encode.
- SFU-fed and peer-fed Viewers may both serve as ordinary peer parents.
- A route ends in usable media, explicit bounded waiting, or clear failure.

Every deployment uses this controller. Leaving embedded SFU disabled keeps the
same bounded P2P graph without its SFU fallback and fixes Privacy mode on for all
rooms, including App Local and Public Link rooms. The server enforces that policy
for waiting and active participants; there is no separate Host-star mode.

A Hosted operator may require SFU-only media through `SFU_ONLY=true`. The same
controller then admits only SFU candidates: no Peer parents, Viewer relays or
background P2P convergence. The Host still publishes once, and every Viewer
uses the existing SFU subscription ledger and first-frame commit. Capacity or
connection failure never permits a Peer route. An explicitly peer-only share
is rejected before acquiring media authority; it is never silently forwarded.
The page advertises this site policy and prevents inapplicable P2P choices.

## Per-Share Route Policy

On a P2P-first site, the Host chooses privacy and topology preferences before
sharing. The server-validated route policy remains fixed for that share generation:

- when SFU is available, default hybrid mode keeps P2P first and permits the
  bounded SFU suffix;
- peer-only mode excludes SFU publication, subscription, bootstrap, and quality
  candidates, while retaining the same bounded Peer graph and clear exhausted
  failure;
- native-edge topology convergence is enabled by default. The Host may disable
  it before sharing; when disabled, quality evidence stays diagnostic and
  availability routing is unchanged; and
- NAT traversal enhancement requires an exact three-destination STUN survey: a
  Site's self-hosted 3478/3479/3480, Public Link's bounded public survey, and
  none on pure LAN. Current pages apply it automatically to Browser and Native
  P2P edges, without a Host-facing switch; SFU-only policy excludes it. Ordinary
  ICE and Native gateway mapping remain independent. Each Native connection uses
  one media socket for IPv4 and, where available, IPv6 direct connections; its
  STUN survey and best-effort gateway mapping use IPv4.
  Availability and background direct acquisition share four actual connection
  attempts per eligible parent/session opportunity. Attempts after the first
  show retry 1/3, 2/3 and 3/3 on pages supporting that progress format, use the
  same serial controller and deadlines, and yield to waiting Viewers and untried
  parents. A retry may use bounded two-App socket preparation before its one
  str0m connection; the first attempt and Browser-only transport stay ordinary,
  and failed optional preparation cannot disable the App for the session.
  This is not a participant capability, route score, or SFU preference;
  diagnostics retain only `ordinary | predicted | unknown` provenance.
  [Signaling and routing](../architecture/routing.md#nat-traversal) owns the
  mechanism and evidence boundary.

These are route-policy gates, not new routing algorithms. Changing any
policy requires stopping the current share and starting another generation.
Voice can establish the room graph before a picture share, using the site's
default route policy. Starting a share applies its Host-selected policy to that
same room controller; voice has no separate policy or route planner.

## Viewer Route Choice

A capable Site offers `auto | p2p | sfu` for the current viewing session.
Auto keeps ordinary P2P-first routing; the other choices allow only that kind of
incoming hop. Host peer-only and site SFU-only policy take precedence. A choice
does not change room privacy: a P2P parent may itself receive through SFU.
The server acknowledges the choice and reuses the same bounded serial operation,
capacity ledger and first-frame commit. Existing media remains during permitted
preparation; exhaustion retires a disallowed route and reports normal failure.
Returning to Auto restores eligible direct opportunities. A relay's children
repair through normal availability routing if its ingress is finally retired.
Restricting the incoming kind does not remove a Viewer from the graph: eligible
P2P quality repair and service to downstream children keep their existing owners.

The applied choice survives signaling reconnect and source/share replacement.
The Viewer's explicit preference is remembered within the Browser origin and
requested again after a reconnect or a source change. An incompatible site or
share policy applies Auto without overwriting that personal preference, and the
automatic route remains the default when the Viewer has no preference.

## Endpoint And Server Capacity

Every non-server endpoint has one server-authoritative steady outbound media-
copy cap `C`, default `2` and configurable only as `1`, `2`, or `3`:

- one peer child consumes one copy;
- the Host's single SFU publication consumes one copy;
- upstream receive is free;
- role, Browser, UA, visibility, and codec do not create another tier.

A Viewer can also volunteer to relay to more participants for the current
visit. It starts with one extra child above the ordinary cap and may select a
higher maximum within its reported capability, at most the room's 20-Viewer
limit. The requested maximum survives backend rediscovery within that visit; a
smaller available capability limits the applied value without replacing the
choice. Withdrawing consent schedules affected children through the same
operation rather than abruptly dropping the subtree. This remains an ordinary
Peer relay, with no separate SFU service or authority.

A make-before-break transition may use the one admitted overlap slot and must
return to steady capacity at commit or abort. SFU publication ingress and
per-Viewer subscription egress use an independent server admission ledger;
ordinary endpoint child counts are not server capacity.

## Room Media

One room controller and its existing serial operation serve both picture and
voice. Opening and joining room voice can acquire the graph before sharing;
picture start attaches a new video generation and picture stop removes that
generation while voice remains. Empty rooms allocate no media connections, and
page entry alone creates no room.

The directed tree still places each Viewer under one parent. Independent Opus
publications travel both ways along its permitted edges, excluding the ingress
branch. Voice uses a companion WebRTC audio connection on each selected edge;
picture retains its existing Browser/Native sender and adaptation owners.
There is no all-pairs mesh or second route planner. SFU voice reuses its existing
UDP listener and admission, including on SFU-only sites.
An audio input's lifetime is separate from its reusable listening output.
Changing publishers preserves the negotiated output while the forwarding
component maintains its RTP clock and feedback. Revoking a source stops both
new and already-buffered delivery; stale source callbacks cannot affect its
replacement.

A candidate carrying picture and voice commits only after both decoded-picture
proof and the required voice transports connect. A silent voice-only candidate
uses actual transport connection, without requiring samples or a video frame.
Video-only attachment to an already committed room edge uses its own fresh
physical identity and decoded-frame proof. Failed attachment evidence ends when
an involved endpoint session changes; proven picture and room transports remain.
Retired facet callbacks cannot invalidate a replacement. Reusing a room
reservation waits for its previous picture facet's physical close and reader
join; full retirement joins those facets before releasing admission. Stop, pause
and failure statuses describe the affected consumer; a voice failure does not
become a video error.
When current decoded-picture proof still establishes a healthy committed path,
a failed companion voice transport is recreated on that same path without
invalidating picture readiness or consuming a topology candidate. Without that
proof, the existing route operation owns repair and alternative selection.
The topology diagram projects connected room edges for voice as well as picture;
voice readiness never becomes evidence that a video frame has arrived.

## Reconciliation

One room controller owns the committed graph and one bounded child transition.
Committed child membership applies immediately, independently of asynchronous
upstream activation. A later child preparation does not retire an already
committed upstream that is still being promoted locally.
Healthy current edges remain sticky. When admitted overlap exists, a usable old
route stays active until the exact candidate child decodes a first new frame;
the candidate must still be connected when that proof is accepted and when it
replaces playback. ICE connected alone is not commit proof. Availability repair
may instead use the explicit bounded-gap plan defined by
[signaling and routing](../architecture/routing.md#transition-shapes) when no overlap
slot exists.
Failure or stale authority releases the candidate and preserves unaffected
branches.

Direct peers receive the first bounded opportunity, with SFU used for
availability when direct paths cannot provide media. Remaining direct paths may
converge behind a working SFU route. Relay-ingress failure reparents that relay
without discarding its subtree; departure and capacity changes move only affected
children. [Signaling and routing](../architecture/routing.md#the-model) owns operation purpose, filtering, ordering, cursor, revision,
reservation, opportunity and rollback mechanics.

Reauthentication rebinds retained media. Newly eligible parents and session
opportunities restore direct convergence behind healthy SFU edges through that
same continuation owner. The existing session-scoped ledger bounds these
attempts; repeated capacity or session updates do not reopen spent opportunities.

Trickled candidates and their completion retain the gathering's ICE identity
across SDP exchanges, so a delayed completion cannot close a newer gathering;
ordinary renegotiation does not create a new ICE generation. Address observations
belong to that gathering even when ICE restart retains the physical socket, and
retiring one gathering does not cancel another or close healthy media.

Transport readiness uses the current PeerConnection state when consumed,
including after Native or SFU event queues. Historical transport loss
still retires input permission; a recovered connection requests a fresh input
handshake rather than restoring the previous grant.

This file owns acquisition timing. Its five-second no-progress window is
scheduling, not terminal ICE failure: the final or only Peer candidate, and one with connected
transport, waits for the unchanged total deadline, so peer-only acquisition does
not abandon its only route at the foreground boundary. Either exact preparing
endpoint may report that progress, which never commits the route or extends the
deadline; first-frame readiness stays Viewer-owned. Either endpoint reports local
rejection or connection-start failure to the same operation immediately, without
waiting out a deadline or starting its own recovery loop. The waiting display
uses the actual server-issued attempt ordinal, never elapsed time.

Committed P2P recovery first requests an ICE restart, then a rebuild if
necessary, within the existing two-request budget. A sender without restart
support, including the current Native sender, replaces its connection on that
first request. Restart authority follows the exact retained edge and its
endpoint sessions; an ancestor repairing its own ingress does not revoke that
edge's recovery. The budget belongs to the current parent and survives physical
connection or Browser/Native backend replacement. Only a connected transport or
a new parent resets it; replacing an unconnected backend cannot renew recovery
indefinitely. Host signaling status is not transport-failure evidence: offline
status alone does not cancel current-edge recovery or retire a relay's
downstream connections.

Capable Native receivers renegotiate that connection while retaining the
encoded source, local playback and downstream edges when its codec/audio shape
is unchanged; a changed media shape replaces the source and its dependent edges
without requiring NAT prediction or reopening the route candidate.
str0m owns ICE restart. Each actual gathering owns fresh supplemental candidates,
shares an in-flight Native STUN transaction only within that gathering,
and refreshes a previously successful gateway mapping, since a requested lease
does not prove the gateway kept it; ordinary renegotiation refreshes nothing and
there is no background poller. A Native connection owns its UDP socket and
optional gateway mapping, used by both discovery and media; a new connection
receives an independent socket, so an admitted overlap never collapses into one
remote address/port, while ICE restart retains the current one. Encoded sources
remain shared; this isolation changes neither route authority nor copy capacity.
[Engineering](../architecture/modules.md#runtime-lifecycles) owns gathering lifecycle.
A prepared candidate follows its route operation's failure path, without an
independent restart loop.

A Browser sender's subsequent audio-direction SDP transaction has a 15-second
completion deadline, so a lost offer/answer cannot leave later microphone/source
changes queued forever behind otherwise connected media. Timeout retires that
exact edge through its existing preparation/recovery owner; coalesced changes
do not renew the deadline. This also applies if audio changes during preparation.
Initial acquisition and ICE restart retain their existing route/recovery deadlines;
ordinary signaling loss without a pending media transaction preserves healthy media.

Host-root fanout convergence under
[Signaling and routing](../architecture/routing.md#candidate-choice)
is one-shot and event-driven: it is not periodic balancing and never moves a
healthy branch without admitted overlap and a proved candidate sender.

Framework reconnect runs before route reassignment. Manual media reconnect
recovers only the current P2P parent or current SFU subscription; it does not
perform quality selection or choose another route.
A Viewer that loses room signaling keeps its membership and committed edges for
20 seconds, covering the nominal backoff for the Browser's first five reconnect
attempts; connection and authentication time can extend those attempts.
Reconnecting with the same client rebinds retained edges. Signaling absence alone
does not retire a media edge before then; its route failure or that window's
expiry does.
Current-edge P2P signaling remains valid during candidate overlap. Accepting a
replacement from that parent cancels conflicting optional preparation so the
replacement can complete; a retired candidate cannot take its place afterward.
Failure reports belong to the failed physical connection, not another
preparation's phase. A committed connection's identified failure remains valid
across unrelated room revisions only while its endpoint session and connection
identity still match; future revisions and retired connections are rejected.
Preparation reports retain their exact operation fence; reports without a
connection identity retain their exact room-revision fence.

An individually rejected ICE candidate is disposable input. Browser
`addIceCandidate` candidate-processing errors are recorded without aborting the
remaining queue or SDP answer; missing-description/lifecycle errors remain
visible. Native input, SFU and local Browser/App media connections and codec
probes follow the same boundary: actual transport state and the route
operation's deadline own failure, not one rejected candidate.

## SFU Fallback

The only application fallback is one admitted embedded SFU publication from the
Host, with independently admitted Viewer subscriptions. The controller may
create it through the same bounded transition when Host capacity is occupied.
No Viewer creates a second publication.

SFU offer/answer, ICE and output demand use the existing authenticated room
WebSocket. The server binds each physical connection to the current participant,
share and publication; it issues no separate media token or room-service URL.
Browser SFU PeerConnections use the server's UDP candidates and no external
ICE-server list. Ordinary peer connections use deployment STUN. Hosted Piik
owns its configured Binding-only STUN listeners and optional SFU UDP listener in
the same Rust process. Piik configures no TURN, ICE/TCP, media TCP, or
TLS-relayed media path. HTTPS/WSS is a separate control transport and remains
TLS/TCP.

SFU resources remain bounded throughout reservation, use, and cleanup; stale
signaling cannot recreate off-ledger media. Grant revocation, Viewer leave,
and SFU-to-P2P replacement close the exact physical subscription before releasing
its egress reservation. Publication teardown closes its ingress and descendants
before releasing their reservations. No external token outlives those resources;
[Signaling and routing](../architecture/routing.md#sfu-fallback) owns the
embedded lifecycle.

Transient room-signaling loss does not close healthy SFU media. The current
physical connection can restart ICE; actual terminal media failure returns to
the same bounded route recovery, identifying the failed physical connection.
An active configuration describes that connection; it cannot create a replacement
Browser subscriber. Replacement needs the controller's prepared subscription and
first-frame proof. Reauthentication during an unfinished restart also returns
that subscription to its route owner; sending an answer is not connection proof.
Retained Peer and SFU media keep their original acquisition opportunity when
signaling sessions change: their failure cannot consume a new session's untried
opportunity, and a new physical connection binds the current one. An exhausted
opportunity remains an explicit failure until an eligible new fact reopens
acquisition. Source capture remains Host-owned throughout.

## Quality And Privacy Boundaries

Browser WebRTC and the shared str0m media layer own ICE, consent,
congestion control, bitrate, frame rate, resolution, retransmission, pacing and
layer selection. Direct-child media demand does not create a topology operation.

When the per-share convergence gate is enabled, persistent limitation of one
exact native sender may trigger one measured experiment through the existing
serial operation. The old route keeps playing; a candidate commits only when the
same Viewer proves, over fresh overlapping windows, no freeze, pause or delivered
picture regression, and otherwise the old route stays. Ordinary quality moves
are Peer-to-Peer; SFU joins only as Host fanout relief when every current
Host-origin Peer edge, with at least two, is persistently degraded. One Viewer's
proof never authorizes another. Disabling the gate leaves quality diagnostic.
Piik has no weighted route score, general parent-wide prediction, all-pairs
probe, periodic rebalance, persistent parent blacklist, NAT classification, or
independent depth cap. [Signaling and routing](../architecture/routing.md#commit-and-recovery)
owns the three-window trigger, proof, same-parent regeneration and rearm rules.

Direct P2P exposes endpoint network addresses to the trusted peer. SFU media is
encrypted hop-by-hop with DTLS-SRTP but terminates at the SFU; the product does
not claim operator-blind media without a separately accepted application E2EE
design. All-UDP-blocked networks currently end in bounded failure.

App Local mode runs this graph without SFU or NAT prediction. Its selected LAN
address supplies Binding-only STUN on the App's UDP port, so routed virtual
LANs need not carry Browser mDNS; it uses no public discovery destination.
Public Link mode tunnels only HTTP/WebSocket control and adds public STUN to the
same Browser/Native P2P edges. Reachable peers form one mixed relay tree; an
unreachable path adds no route type or score. App-enabled Site pages use that
Site's transport configuration.

A Native Host reserves one loopback edge for Browser preview; it consumes no
route-copy capacity or gateway mapping. Direct children and the admitted Native
SFU publisher reuse the encoded source through the shared media adapter. The SFU
publication still consumes one route copy and one aggregate upstream budget.

A native Viewer terminates only its P2P upstream, reserves one Browser playback
edge, and reuses compatible H.264/VP8/Opus for bounded children, deriving a missing
lower output only on direct-child demand. Browser keeps SFU subscription,
route revisions, frame proof, and fallback.
