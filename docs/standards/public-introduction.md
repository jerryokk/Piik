# Public introduction

This file owns the README and reader-guide content structure. The
[visual language](./visual-language.md) owns their shared illustration and motion.

## One story, three readers

Lead with friends sharing something worth seeing: a game, a drawing, a film or
travel photos. The [copy guide](./naming.md#voice-and-terminology) owns
Chinese/English voice and role labels. A visitor should
understand what Piik does, choose an entry and complete the first useful action.

| Surface | Reader's question | Content |
| --- | --- | --- |
| README, English and Chinese | What is this, and how do I start? | Brand introduction and interface preview, features, getting started, self-hosting, contribution and license |
| Getting-started guides | Which button or mode do I use? | Browser invitation, sharing, App setup, room creation and Site approval |
| Self-hosting guides | How do I put my own site online? | Run the Windows Server, set the domain and HTTPS proxy, open ports and verify |
| Developer and operations docs | How do I build or host it? | Existing configuration, deployment, architecture and diagnostic owners |

The [reader documentation](../guide/README.md) is the public help entry. Link it
from both READMEs, with direct self-hosting, configuration and update/recovery
links. Keep the guides in this repository; do not maintain a second wiki copy of
the same instructions. The repository's [developer map](../README.md), standards and work ledgers
remain separate from that reader navigation.

Specific errors and recovery procedures belong in the troubleshooting and network
guides, linked in the selected language. App references link to the relevant
guide rather than maintaining a second copy.

Repository descriptions and topics describe the actual product: private screen
sharing, watching together and optional self-hosting. Use natural Chinese and
English phrases in the relevant introduction; preserve the brand line in the
visible headline. Avoid keyword lists, unrelated product names and unsupported
performance claims.

## Reader guidance

Separate first-use tutorials, task instructions, explanations and reference,
following [Diátaxis](https://diataxis.fr/how-to-guides/). The README helps
readers choose an entry; guides supply the steps. Check the rendered result:

- Start with the reader's task or visible symptom. Put prerequisites and the
  next useful action before optional mechanisms; keep essential safety and
  success checks beside the action, outside optional disclosures.
- Give long pages linked section indexes and descriptive headings. Split
  substantial tasks when the outline becomes hard to scan.
- Put diagrams or real UI captures beside the step or relationship they explain.
  Keep necessary text available without the image; remove decorative repetition.
- Use direct instructions and consistent UI labels. Check entry selection,
  task completion and the next check on a narrow viewport.

Reference guidance:
[Microsoft's scannable-content and navigation guidance](https://learn.microsoft.com/en-us/style-guide/scannable-content/),
[Google's large-document course](https://developers.google.com/tech-writing/two/large-docs),
[W3C's headings and link guidance](https://www.w3.org/WAI/tips/writing/), and
[Mayer and Fiorella's coherence, signaling and spatial-contiguity principles](https://doi.org/10.1017/9781108894333.019).

## Presentation

Keep two presentation responsibilities distinct:

| Layer | Owns | Review question |
| --- | --- | --- |
| Shared identity | Mascot geometry and wink, gold sparkle, wordmark/orange dot | Does this read as the same Piik at every size and entry? |
| Product demonstration | Actual components, labels, source selection, room controls and participants with sample data | Does the pictured action still match the current product? |

The mascot's curved wink and gold sparkle are recognizable features; a prop or
a pictured game does not replace its face with another expression. Use the
[shared visual language](./visual-language.md) for cast, objects, accessible
interaction and motion; this document does not own a separate product-status
vocabulary.

The room illustration shows distinct uses: an RPG, drawing, travel photos and a
movie. Treat coding and design as creative work rather than proliferating
overlapping examples. The host holds a gamepad during the game; the role badge
identifies the host independently of the activity. Keep participant drawings
consistent across READMEs, interface captures and tooltip/status comics.

Present Piik as an open-source project: plain explanations, room for tinkering,
and an invitation to contribute. Keep the friendly illustration and avoid
company-style pitches, repeated slogans or claims about a support team.
Both READMEs share the friendly opening and room illustration. Their feature,
limitation and setup sections use the factual register in the copy guide.

Use a consistent README structure:
a distinct brand opening, bold headings and key phrases, regular explanatory
text, quiet captions and optional detail. Use images and short demonstrations
to explain different things rather than filling space. Keep essential setup and
availability in readable text, and distinguish generated demo content from a
real captured workload. Markdown/HTML must remain useful on narrow viewports
and in both themes without custom CSS.

The product UI remains mascot-only. The wordmark and mascot do not transform
into one another. Interface previews use real components with generated scenes
and sample participants. Keep private invitations, user identities and personal
media out of published assets; no external fonts or tracking are needed.

## Review boundary

Review English/Chinese, light/dark and narrow viewports. Check links, accurate
platform/connection claims and a single copy of each responsibility. This is
introduction structure, not an excuse to change configuration or media policy.
