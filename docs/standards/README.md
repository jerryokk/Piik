# Piik standards

Current product and engineering rules live here. Start with the affected subject;
the [ownership map](./documentation.md#owners) explains where each fact belongs.
[AGENTS.md](../../AGENTS.md) owns task authority and
[Contributing](../../CONTRIBUTING.md) owns delivery.

| Subject | Standard |
| --- | --- |
| Module responsibility, resources, interfaces and contextual consistency | [Modules](../architecture/modules.md) |
| Product names, terminology, Chinese/English voice and content layers | [Naming and copy](./naming.md) |
| Room authority, invitation/password access and persistence | [Rooms and access](./rooms-access.md) |
| Automatic routes, capacity, P2P/SFU transitions and route operations | [Routing and transport](./routing-transport.md) |
| Capture, encoders, media adaptation and audio | [Media quality](./media-quality.md) |
| Workflow, playback, presentation and Browser/App lifecycle | [Presentation and lifecycle](./presentation-lifecycle.md) |
| Evidence behind each status, indicator, overlay and notice | [Media status](./media-status.md) |
| Cast, symbols, colour, animation, tooltips and playful copy | [Visual language](./visual-language.md) |
| README and reader-guide composition | [Public introduction](./public-introduction.md) |
| Runtime settings, defaults, bounds and diagnostics | [Configuration](./configuration.md) |
| Identifiers, interface change and shipping | [Versioning](./versioning.md) |
| Documentation responsibilities and updating a rule | [Documentation ownership](./documentation.md) |

Before another implementation of a familiar concept, identify its fact, owner,
scope and lifetime. Apply the existing rule, or explain the contextual difference
at that rule's owner. Source schemas and focused tests own exact implementation
details; standards need not repeat every field or function.

[Architecture](../architecture/README.md) owns system structure, protocols and
the technical reasons still shaping them. [TODO](../todo.md) owns work that
remains, including accepted targets not yet implemented; a recorded target is not
a claim of completed verification. User guides remain in the
[documentation index](../README.md).
