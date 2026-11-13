# Piik Architecture

This directory describes Piik as it is built now: one Windows App, one Server,
one Web client, shipped together from one revision. It states the current design
and the reasons that still shape it. How the design evolved is history and lives
in Git.

Product-facing rules stay in [standards](../standards/README.md); this directory
owns system structure, protocols and the technical reasons behind them.

| Area | Document |
| --- | --- |
| Processes, trust boundary, deployment shape | [System overview](./system.md) |
| Room authority, invitations, persistence and access | [Rooms and access](./rooms.md) |
| Signaling, candidate exchange, route graph and SFU fallback | [Signaling and routing](./routing.md) |
| Capture, encoding, forwarding, audio and adaptation | [Media pipeline](./media.md) |
| Wire protocols, build identity and change policy | [Protocols](./protocols.md) |
| Windows App shell, native control and capture helper | [Native host](./native-host.md) |
| Repository layout, module boundaries and lifecycle ownership | [Modules](./modules.md) |
