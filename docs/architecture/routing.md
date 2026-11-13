# Signaling And Routing

This file owns how a room agrees on who sends what to whom: one controller, one
graph, one operation at a time, and the reasons behind those boundaries. User-
visible routing behavior stays in [routing and transport](../standards/routing-transport.md).

## The Model

One room controller owns the whole media graph:

- one committed, versioned, acyclic graph reachable from the Host;
- one event-driven reconcile loop;
- at most one room-serial child operation at any moment;
- participant sessions, effective capacity, failure facts and the Host's SFU
  publication.

Everything else is an input to that loop, not a second router. Joining, waiting,
repairing a failed edge, repairing a relay's ingress while keeping its subtree,
a confirmed departure and a capacity reduction all converge through the same
operation. The Host has no upstream; every Viewer has zero or one.

Two properties make this work and explain its shape:

- **One writer.** A single owner per fact means no lock ordering, no two
  reconcilers disagreeing, and no state that only one code path updates.
  Operations are serialized at room level because graph edits share the same
  capacity counts and cycle checks.
- **One operation per child.** A child operation has one immutable purpose, one
  candidate list and cursor, one current candidate, one route-demand owner, one
  fact version and one total deadline. Availability work preempts a background
  operation by aborting it and creating a new operation; an existing operation
  is never relabelled to a new purpose.

## Capacity

Every non-server endpoint has one server-authoritative steady outbound media-copy
cap, and the controller computes effective capacity from what the endpoint
advertises it can take. The cap and its consumers are owned by
[routing and transport](../standards/routing-transport.md#endpoint-and-server-capacity);
the architecture point is that one number, applied uniformly, is what makes the
graph's capacity arithmetic checkable. A transition may briefly reserve one extra
copy and must release it on commit or abort.

## Candidate Choice

An operation first rejects the impossible: stale sessions, unreachable sources,
cycles, insufficient steady or overlap capacity, tuples already consumed by this
operation, and unavailable SFU resources. Then it builds one deterministic
candidate list, ordered by shallowest resulting depth, greatest remaining steady
capacity, a stable child-parent rank, join order and peer identity. Address,
geography, NAT guess, user agent and any scalar quality score never select a
parent. There is no independent depth cap; admission and acyclicity bound depth,
and shallowest-first is a preference rather than a periodic balancing mandate.

One deliberate exception exists: after a new Host-root edge commits, a root with
no children while another root has at least two may prepare one deterministic
child in a make-before-break move, committing only with a fresh healthy sender
proof. It is a one-shot, event-driven convergence, never a timer or a general
load score.

## Transition Shapes

A child transition is either an overlap or a bounded gap. An overlap reserves
`min(C + 1, 3)` copies briefly so the new edge proves itself before the old one
retires. Availability repair may instead use one explicitly planned **bounded
gap** after server resources are preflighted: the old edge retires first, the
same deadline holds, the untried suffix is replanned, and one restore tuple is
retained.

Two further bounded shapes exist, both event-driven and finite:

- **SFU bootstrap.** If the Host is full and no publication exists, one intent
  owns the original waiting demand and a finite cursor of eligible Host-direct
  children. It may not cut a healthy branch first.
- **SFU waiters.** A room denied by deployment-wide SFU admission joins one
  bounded waiter set; an actual decrease in SFU usage drains that set once.

An old SFU root that still carries Peer descendants remains as an inactive graph
anchor until those descendants are reassigned; a childless old root is removed
immediately. Revision allocation belongs to the room's lifetime, including
stopped shares and replacement controllers, and ends with room authority.

## Commit And Recovery

A candidate becomes an edge only after proof: a picture edge needs a decoded
frame, and accepted quality changes add native edge proof before commit in the
same operation. Preparation reserves capacity without publishing; commit swaps
the route; abort releases every reservation. A failed attempt consumes its
opportunity. With NAT traversal enabled, the availability acquisition and direct
path may retry within the operation's deadline; without it, one failure closes
the opportunity.

A route counts as persistently limited only after three consecutive complete
degraded deltas for the same exact sender identity. One same-parent regeneration
may then repair it through the same operation. A quality commit or root
convergence starts disarmed from an unknown state and stays disarmed until the new
active identity reports a fresh healthy delta. A healthy edge is sticky:
unrelated resyncs and quality changes leave it alone, and only a real failure,
departure or capacity reduction recalculates it. Remaining direct paths converge
behind working media one at a time, round-robin across SFU Viewers, rather than
as a batch.

## What Was Rejected, And Why

- **Custom quality scores or ladders.** WebRTC and str0m already measure and
  adapt. A second score duplicates the estimator with different sight lines and
  the two would disagree; the media path would then fight itself.
- **All-pairs probes.** Every extra measurement pair costs bandwidth on exactly
  the links that are already constrained.
- **Periodic rebalancing.** Moving healthy media interrupts the picture to chase
  a marginal distribution gain.
- **Hand-built SFU representations.** The embedded SFU forwards real encoded
  sources; rewriting them would abandon the encoder's own quality decisions.
- **TURN or a TCP media fallback.** Piik's bounded fallback is the embedded SFU
  on UDP; a second relay path doubles the state for a case the SFU already covers.

## SFU Fallback

The only application fallback is one bounded embedded SFU/UDP publication from
the Host. Endpoints preferring P2P reach the Host directly; when that fails or is
disallowed, the Viewer subscribes through the SFU. The same controller and
operation handle both, because an SFU subscription is another child edge with its
own reservation. An operator may restrict a Hosted deployment to SFU-only; the
policy reuses the same ledger and deadlines and does not relax on admission
failure.

Signaling carries embedded SFU SDP/ICE and per-Viewer demand over the
authenticated room WebSocket, and transient signaling loss does not close healthy
SFU media.

## NAT Traversal

Ordinary ICE uses STUN and prefers a direct path. An operator may enable
coordinated same-host auxiliary STUN, which lets two peers on the same public
address predict and punch a path without a new signaling system.

The prediction appends at most eight bounded candidates outward from both
sequence endpoints — four above the high endpoint and four below the low one.
The page waits at most 3.5 seconds for that preparation; the native side expires
unconsumed work after five seconds, and one survey stays within 64 requests and 64
authenticated replies.
Its provenance is reported as `ordinary | predicted | unknown`, and a changed
media shape neither requires prediction nor reopens the route candidate.
