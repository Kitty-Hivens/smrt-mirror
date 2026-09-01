// Motion primitives shared by the panel.
//
// Movement decelerates to a stop, short and calm. Large surfaces do not
// overshoot; small controls may settle a few pixels via --ease-out-back (see
// app.css). Durations and easings live in app.css as tokens; these are the
// behaviours that need JavaScript.

import { fade, fly, slide } from 'svelte/transition';
import type { TransitionConfig } from 'svelte/transition';
import { flip } from 'svelte/animate';
import type { FlipParams } from 'svelte/animate';

/// Requests currently in flight, for the shell's activity wire. A counter
/// rather than a boolean: overlapping requests must not have the first one to
/// finish declare the app idle.
///
/// The count itself is a plain variable and only `busy` is reactive, which is
/// not a detail. Every request passes through here, including requests started
/// inside an `$effect`; if the counter were `$state`, incrementing it would
/// both read and write reactive state inside that effect, so the effect would
/// depend on its own side effect and re-run forever. That is not theoretical --
/// the shell's one-shot health fetch turned into an unbounded request loop the
/// moment this was reactive. Writing `busy` without reading it creates no such
/// dependency.
let inflight = 0;
let busy = $state(false);

export const activity = {
  get busy(): boolean {
    return busy;
  },
  begin() {
    inflight++;
    busy = true;
  },
  end() {
    inflight = Math.max(0, inflight - 1);
    busy = inflight > 0;
  },
};

function reduced(): boolean {
  return window.matchMedia?.('(prefers-reduced-motion: reduce)').matches === true;
}

/// Reveal a list in sequence rather than all at once, so the eye follows the
/// order the rows arrive in. The per-row delay is capped by index: a 97-mod
/// pack must not take four seconds to appear, so the stagger runs out after the
/// first dozen and the rest land together.
export function stagger(node: HTMLElement, index: number) {
  const apply = (i: number) => {
    node.style.setProperty('--stagger', reduced() ? '0ms' : `${Math.min(i, 12) * 16}ms`);
  };
  apply(index);
  return { update: apply };
}

/// Count a number up to its value on first paint. Used only on the overview
/// tiles, where the number IS the content -- everywhere else a counting digit
/// would be decoration pretending to be information.
export function countUp(node: HTMLElement, value: number) {
  let raf = 0;
  const DURATION = 420;

  function run(to: number) {
    cancelAnimationFrame(raf);
    if (reduced() || to === 0) {
      node.textContent = String(to);
      return;
    }
    const from = Number(node.textContent?.replace(/\D/g, '') ?? 0) || 0;
    const t0 = performance.now();
    const tick = (now: number) => {
      const p = Math.min(1, (now - t0) / DURATION);
      // the same -out curve the CSS tokens use, so a counting number and a
      // sliding panel feel like one system
      const eased = 1 - Math.pow(1 - p, 3);
      node.textContent = String(Math.round(from + (to - from) * eased));
      if (p < 1) raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
  }

  run(value);
  return {
    update: run,
    destroy: () => cancelAnimationFrame(raf),
  };
}

// ── the shared duration source ──────────────────────────────────────────────
//
// A JavaScript transition must not carry its own timing. app.css zeroes the
// duration tokens under `prefers-reduced-motion: reduce`, and a transition that
// reads them there is disarmed by that one rule rather than by remembering to
// ask a second time. Two motion switches is how a product ends up with a
// setting that only half works.

/// Milliseconds for a duration token, as the stylesheet answers it right now.
/// Read per call rather than cached: the reduced-motion rule rewrites these,
/// and a cached copy would be the stale half of a setting.
export function dur(token: 'state' | 'enter' | 'layer'): number {
  const raw = getComputedStyle(document.documentElement)
    .getPropertyValue(`--dur-${token}`)
    .trim();
  const n = parseFloat(raw);
  if (!Number.isFinite(n)) return 0;
  return raw.endsWith('ms') ? n : raw.endsWith('s') ? n * 1000 : n;
}

/// The `--ease-out` token as a JavaScript easing, so a sliding panel and a
/// counting digit trace the same curve. Falls back to a plain cubic ease-out if
/// the token is not a cubic-bezier -- a near-miss curve is better than a
/// linear one, and better than throwing inside a transition.
function easeOut(): (t: number) => number {
  const raw = getComputedStyle(document.documentElement).getPropertyValue('--ease-out').trim();
  const m = /^cubic-bezier\(([^)]+)\)$/.exec(raw);
  const nums = m?.[1].split(',').map((s) => parseFloat(s.trim()));
  if (!nums || nums.length !== 4 || nums.some((n) => !Number.isFinite(n))) {
    return (t) => 1 - Math.pow(1 - t, 3);
  }
  return cubicBezier(nums[0], nums[1], nums[2], nums[3]);
}

/// y(x) of a CSS cubic-bezier easing. x is solved by bisection rather than
/// Newton-Raphson: the curve is monotonic in x by CSS's own constraint, twenty
/// halvings put the error below a thousandth of a frame, and there is no
/// derivative to get wrong.
function cubicBezier(x1: number, y1: number, x2: number, y2: number): (t: number) => number {
  const axis = (a: number, b: number, t: number) => {
    const u = 1 - t;
    return 3 * u * u * t * a + 3 * u * t * t * b + t * t * t;
  };
  return (x: number) => {
    if (x <= 0) return 0;
    if (x >= 1) return 1;
    let lo = 0;
    let hi = 1;
    for (let i = 0; i < 20; i++) {
      const mid = (lo + hi) / 2;
      if (axis(x1, x2, mid) < x) lo = mid;
      else hi = mid;
    }
    return axis(y1, y2, (lo + hi) / 2);
  };
}

/// A surface arriving rather than being present: it rises the last few pixels
/// into place as it fades in. Used where a whole view replaces another, so the
/// eye is given the direction the new thing came from.
///
/// At the use site this needs `|global` whenever the element belongs to a
/// component that a parent block creates -- a local transition does not play
/// then. Measured, not assumed (#114).
export function arrive(node: Element, params?: { y?: number }): TransitionConfig {
  return fly(node, {
    y: params?.y ?? 8,
    duration: dur('layer'),
    easing: easeOut(),
  });
}

/// A block unrolling in place, and rolling back up when it closes: a mod's
/// builds under the mod they belong to. Height, so the rows below are pushed
/// rather than covered -- the list stays one surface.
export function unroll(node: Element): TransitionConfig {
  return slide(node, { duration: dur('enter'), easing: easeOut() });
}

/// A row taking its new place in a list rather than appearing in it.
///
/// Sorting, filtering and paging all rewrite a list, and without this the rows
/// that survive teleport: the same twenty items are on screen before and after,
/// and nothing connects the two frames, so the eye has to re-read the list to
/// find what it was looking at. Rows arriving are handled by `.row-in`, which
/// fires on the element being created; this is the other half -- what happens to
/// everything that stayed.
///
/// `--dur-state`, not `--dur-enter`: a row moving is a state change, not an
/// arrival, and it should be over before it is noticed. Reading the token is
/// also what disarms it under `prefers-reduced-motion` -- the same one rule that
/// disarms the rest, rather than a second switch to remember.
export function settle(node: Element, params: FlipParams & { from: DOMRect; to: DOMRect }) {
  return flip(node, params, { duration: dur('state'), easing: easeOut() });
}

/// A row leaving a list it was removed from. Quick and downward-weighted, so the
/// gap closes while the eye is still on it -- the alternative is a row vanishing
/// between frames and the list below jumping up to fill a space nobody saw.
///
/// For a list that is replaced wholesale (a new search) this is the wrong tool:
/// every old row would linger over every new one. Use it where rows leave one at
/// a time.
export function depart(node: Element): TransitionConfig {
  return fade(node, { duration: dur('state'), easing: easeOut() });
}
