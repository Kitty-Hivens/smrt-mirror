# Direction

Where the interface is heading, and the reading of why it is where it is. This is
course, not settled decisions (those live in `docs/adr/`) and not tracked work
(that lives in the issues). It exists so the decisions have a frame, and so a new
decision can be checked against it.

## What the mirror is

Infrastructure, not a storefront. A self-hostable mirror of mods, a registry, a
pack build system, moderation, identity. The public catalog is the most visible
tip, not the substance. There is no clean line between a consumer and an author:
someone browses, forks, edits, builds. Treating half of it as a passive place to
browse packs undersells it and mis-shapes the UI.

## Why it is what it is

Much of the code was written by AI under a fixed ruleset. It is rigorous exactly
where a rule existed (security, the side and match-policy invariants, contrast
maths) and operator-brained exactly where a rule did not (newcomer UX, mod
discovery, human error messages). An AI applies a rule where it recognises the
pattern and falls to its defaults elsewhere, and those defaults are technically
correct and humanly cold.

The fake aria-labels are the seam. The rule "add an aria-label" fired everywhere;
the rule "the label names the field in the user's language" did not exist, so the
placeholder example got dropped in. The English error messages, the doctrine
written as poetry, the form fields showing raw schema names (`project_id`,
`sha1`) are the same shape: the mechanical half done, the human half left to a
default.

So the interface is not bad. It is a strong engine under roughly a third of a
product, built for one expert who holds the model in their head, and it grinds
anyone who does not.

## Posture

Progressive disclosure. A UI that leads. Depth available but not imposed,
revealed as you engage, instead of the current all-or-nothing where an operator
is assumed to know everything and a newcomer is shown nothing. The editor idiom
(ADR 0004) is the concrete form of this on the authoring surfaces. The graph
already does a little of it and is the proof it is possible here.

## What matters first, in order

The order stands; the top of it has since been built, and this list is kept as
the reading that produced the work rather than as a to-do.

1. Data. Silent config loss on concurrent edits (#52) -- done: a config carries
   an `ETag` and a save states the revision it edited. Builds that die when
   Modrinth is down (#57) -- done: the registry answers for a pin the harvest
   has read, and the build says which mods it fell back on.
2. Flow traps. Adding a mod blind, with no sight of what it pulls (#53) -- done:
   the dependency preview runs the real fill on a copy before the save. The
   editor you cannot leave with the back button (#54) -- done: the open editor
   is a location.
3. Friction and comprehension. Loading states -- done: a wait holds the shape of
   the rows that are coming instead of standing in as a line of text, and a
   filter says it has heard a keystroke on the frame it arrives rather than
   after the debounce it opens. Forms with no field names (#55), jargon in the
   user's face. Still the live class.
4. Cosmetics. Transliterations, wrong labels, tooltips in the wrong language. The
   cheapest class and the least of the evils.

Search was the largest single gap and is closed: one search over both places a
mod can come from (`/v1/search/mods`), each hit saying whether the mirror holds
the bytes and how it sits with the pack's loader. A pack no longer needs the
Modrinth website open alongside to be assembled. What is left of the complaint
is discovery rather than lookup -- browsing what the mirror holds without
knowing what to ask for.

## The shape it is heading toward

One space with several instances in it, not a stack of overlays. Panels slide
out and stay: a pack editor open beside a mod page beside what the mirror holds,
because that is how the work actually goes. `FloatDock` was the first move in
that direction and stays as the nucleus. What each kind of panel obeys is
recorded in ADR 0005.

Two things gate it, in order. The surfaces are dark-first with white tints
written literally into the tokens (`--dotfield`, `--seam`, the table zebra,
`--accent-soft`), so a light substrate is a rewrite rather than a swap -- and
the geometry only reads on a substrate where elevation can be a shadow instead
of a lighter surface, which is what the token file says it currently is not.
Then the reflow rules have to key off the container: a pane is a narrow context
inside a wide window, and every one of the 21 `@media` rules asks the window.

A spike (warm paper substrate, softer geometry, an editor that arrives rather
than cuts) said the idiom sits on this product without a fight, and said the two
places it breaks: rows lose their boundary when elevation stops being a lighter
surface, and a blanket pill radius swallows small destructive controls. Both are
per-component decisions, not token values. The spike was a look, not a
foundation; it is not in the tree.

## How to write things down

Plainly, and so they can be checked. "Motion may overshoot 4px on controls" can
be argued with in a review; "anything that springs reads as a different product"
cannot, and that is how the doctrine rotted until its own author could not read
it. A decision cited but not readable is worse than none. When two of us
understood a decision differently, the record is the reconciliation: draft it,
correct where it diverges, and treat the divergence as signal.
