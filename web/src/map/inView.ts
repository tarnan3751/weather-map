// Which weather sources to credit for the view. The layer reports every source
// whose part's `bounds` reach the visible area, but a bounding box can be much
// larger than the data (a rotated model grid's box reaches far past its
// domain). The server's provenance answers, at the points it has been asked
// about, say what really makes the value there.

import type { LonLatBox, Part, SourceDef } from '../api/types.ts';
import { boxContainsPoint, boxesIntersect } from '../tiles/stack.ts';

/** One provenance answer: who has a share at a point. */
export interface Answer {
  layer: string;
  lon: number;
  lat: number;
  /** Sources with a share there. */
  named: ReadonlySet<string>;
  /** Sources the asked frame had a part from: the only ones it speaks for. */
  stack: ReadonlySet<string>;
}

/**
 * The sources of `found` (bounds in view) to credit. A source is kept when
 * its part is surely in view (its `solid` box meets the view), when an answer
 * in view names it, or when nothing says otherwise. It is dropped only for a
 * model or an analysis, whose domain is fixed, when answers in view were
 * asked of frames that had it and none found it: then its box, not its data,
 * reaches the view. Radars and satellites are ragged, so a point where one
 * has no echo says nothing about the rest of the view.
 */
export function creditedSources(
  found: readonly string[],
  stack: readonly Part[],
  sources: Readonly<Record<string, SourceDef>>,
  view: LonLatBox,
  layer: string,
  answers: readonly Answer[],
): string[] {
  const here = answers.filter((a) => a.layer === layer && boxContainsPoint(view, a.lon, a.lat));
  return found.filter((id) => {
    const part = stack.find((p) => p.source === id);
    // Only in the frame being faded towards: nothing to go by.
    if (!part) return true;
    if (part.solid && boxesIntersect(part.solid, view)) return true;
    const medium = sources[id]?.medium;
    if (medium !== 'model' && medium !== 'analysis') return true;
    const spoke = here.filter((a) => a.stack.has(id));
    return spoke.length === 0 || spoke.some((a) => a.named.has(id));
  });
}
