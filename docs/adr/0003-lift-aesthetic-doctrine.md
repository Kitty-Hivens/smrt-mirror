# 0003. Lift the aesthetic bans, keep the type floor

Status: shipped (PR #73)

## Context

The token file (`web/src/app.css`) forbade colour as anything but state, forbade
depth ("Nothing is flat"), forbade shadows, and forbade motion overshoot
("anything that springs reads as a different product"). Four absolute bans,
written as prose that could not be checked. They quietly shaped every screen into
a flat, monochrome, motionless console: efficient for one operator who holds the
system in their head, hostile to a newcomer, and a wall against the layered
editor UI the authoring surfaces need. A big inspector panel over the same flat
black field is indistinguishable from the content under it.

## Decision

Replace the bans with rules a review can hold, and add the missing primitives,
without changing any existing token value, so nothing is repainted by this change
alone:

- Colour may carry category and navigation via a new `--accent-hue`, deliberately
  none of the four status hues so brand and navigation never read as state.
- Depth may show hierarchy. On a dark field a drop-shadow is invisible, so
  elevation reads as a lighter surface (the `--panel..--panel-3` ladder); real
  shadows (`--shadow-pop`) are kept only for true overlays.
- Motion may overshoot a few pixels on small controls via a new `--ease-out-back`;
  large surfaces still settle without a bounce.

The 11px type floor is the one clause kept, reworded. It is a real rule, not a
limiter: below it, body copy stops being readable.

## Rejected

- Deleting the tokens outright. That removes the ban and leaves no rule, which is
  chaos, not freedom.
- Repainting the whole app in the same change. Rushing colour and depth across
  every view at once is how you produce a mess. Application is per-surface work
  that follows, one surface at a time.

## Consequences

- The per-surface work (editor inspectors, FX, colour and depth where they earn
  their place, see 0004) is no longer building against a wall.
- Several tokens were still alpha-on-white (`--seam`, `--dotfield`, the table
  zebra, `--accent-soft`), so a genuine light theme needed real light values
  first (#56). Since paid: the token file carries both halves. The light one
  reverses this record's elevation rule rather than inheriting it -- on paper the
  surface ladder nearly vanishes and a drop shadow reads cleanly, so
  `--shadow-1/2` carry the raise there and the ladder is left to hover.
