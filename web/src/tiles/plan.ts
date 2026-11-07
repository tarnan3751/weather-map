// Which tiles draw a frame: a walk down the quadtree that stops, tile by
// tile, where the view has enough detail or the parts covering the tile have
// none deeper. Over open ocean that is the global model's zoom; over a radar
// it goes deep. One view can hold tiles of several zooms.
//
// The protocol's walk, read literally, would also descend into children of
// a deep tile that lie outside the deep part, whose own stacks offer nothing
// that deep; those are drawn from their parent's tile instead.
//
// Several frames can be planned together (the two being cross-faded, and the
// one shown before them): the walk then yields the common refinement of their
// tile sets, cells each frame covers with one of its own tiles (that cell or
// an ancestor of it), so a cell can be drawn in one pass with every frame's
// texture.

import type { Part } from '../api/types.ts';
import { stackMaxZoom, tileBounds, tileParts } from './stack.ts';
import type { TileID } from './tileMath.ts';

/** Deepest zoom the server renders at all (`wx_core::proj::MAX_ZOOM`). */
export const MAX_TILE_ZOOM = 14;

/** One tile of one frame: its stack and the URL that names it. */
export interface Leaf extends TileID {
  /** Part ids, top first; empty where the frame has nothing. */
  parts: readonly Part[];
  /** Null for an empty stack: there is nothing to request. */
  url: string | null;
  maxZoom: number;
}

/** A piece of the view and, per planned frame, the tile drawing it there
 *  (null when that frame was not planned). `x` is canonical; the cell is
 *  drawn at `x + wrap * 2^z`. */
export interface Cell extends TileID {
  wrap: number;
  leaves: (Leaf | null)[];
}

/** The visible area in Web-Mercator units. `x` is unwrapped: it runs past 0
 *  or 1 when the map shows more than one copy of the world. */
export interface ViewRect {
  x0: number;
  x1: number;
  y0: number;
  y1: number;
}

const tileNum = (z: number, x: number, y: number) => (z * 16384 + y) * 16384 + x;

/**
 * Stacks and URLs of one frame's tiles, worked out once per tile. A frame
 * object never changes (a changed frame is a new object), so this can be
 * kept as long as the frame is.
 */
export class FrameTiles {
  readonly parts: readonly Part[];
  private readonly urlFor: (z: number, x: number, y: number, parts: string) => string;
  private readonly memo = new Map<number, Leaf>();

  constructor(parts: readonly Part[], urlFor: (z: number, x: number, y: number, parts: string) => string) {
    this.parts = parts;
    this.urlFor = urlFor;
  }

  at(z: number, x: number, y: number): Leaf {
    const k = tileNum(z, x, y);
    let leaf = this.memo.get(k);
    if (!leaf) {
      const parts = tileParts(this.parts, tileBounds(z, x, y));
      leaf = {
        z,
        x,
        y,
        parts,
        url: parts.length ? this.urlFor(z, x, y, parts.map((p) => p.id).join(',')) : null,
        maxZoom: stackMaxZoom(parts),
      };
      // Bounded: a long session panning around the world should not keep
      // every tile it ever looked at.
      if (this.memo.size > 20_000) this.memo.clear();
      this.memo.set(k, leaf);
    }
    return leaf;
  }
}

/** The frame's tile here is final: deep enough for the view, or as deep as
 *  its stack goes, or nothing at all. */
const settles = (leaf: Leaf, zWant: number) => !leaf.url || leaf.z >= zWant || leaf.maxZoom <= leaf.z || leaf.z >= MAX_TILE_ZOOM;

/**
 * Cells covering `view` for the given frames (null entries are skipped), at
 * most `zWant` deep. Nearest the view centre first, so requests made in this
 * order fill the middle of the screen first.
 */
export function planCells(view: ViewRect, zWant: number, frames: readonly (FrameTiles | null)[]): Cell[] {
  const out: Cell[] = [];
  const y0 = Math.max(0, view.y0);
  const y1 = Math.min(1, view.y1);
  if (!(y1 > y0) || !(view.x1 > view.x0) || !frames.some(Boolean)) return out;
  // Each copy of the world in view is walked on its own, in canonical
  // coordinates; tiles keep their canonical column and say which copy.
  for (let wrap = Math.floor(view.x0); wrap < view.x1; wrap++) {
    const rx0 = Math.max(0, view.x0 - wrap);
    const rx1 = Math.min(1, view.x1 - wrap);
    if (!(rx1 > rx0)) continue;
    // `held` is each frame's settled tile (or null when not planned);
    // `parents` the tile just above, for children that cannot have their own.
    const visit = (z: number, x: number, y: number, held: (Leaf | null | undefined)[], parents: (Leaf | null)[]) => {
      const n = 2 ** z;
      if (!((x + 1) / n > rx0 && x / n < rx1 && (y + 1) / n > y0 && y / n < y1)) return;
      const here = held.slice();
      const mine: (Leaf | null)[] = parents.slice();
      let open = false;
      for (let i = 0; i < frames.length; i++) {
        if (here[i] !== undefined) continue;
        const leaf = frames[i]!.at(z, x, y);
        // A child that has left the deep part's bounds has a stack that does
        // not go this deep (the server would refuse it): its parent's tile,
        // which goes deeper and covers it, draws it instead.
        if (leaf.url && leaf.maxZoom < z && parents[i]) here[i] = parents[i];
        else if (settles(leaf, zWant)) here[i] = leaf;
        else {
          mine[i] = leaf;
          open = true;
        }
      }
      if (!open) {
        out.push({ z, x, y, wrap, leaves: here as (Leaf | null)[] });
        return;
      }
      for (let dy = 0; dy < 2; dy++) for (let dx = 0; dx < 2; dx++) visit(z + 1, 2 * x + dx, 2 * y + dy, here, mine);
    };
    visit(
      0,
      0,
      0,
      frames.map((f) => (f ? undefined : null)),
      frames.map(() => null),
    );
  }
  const cx = (view.x0 + view.x1) / 2;
  const cy = (view.y0 + view.y1) / 2;
  const d = (c: Cell) => {
    const n = 2 ** c.z;
    return ((c.x + 0.5) / n + c.wrap - cx) ** 2 + ((c.y + 0.5) / n - cy) ** 2;
  };
  return out.map((c) => ({ c, d: d(c) })).sort((a, b) => a.d - b.d).map((e) => e.c);
}

/** The leaf of `frame` that holds a point (mercator x in 0..1), as deep as
 *  the plan would go there. */
export function leafAt(frame: FrameTiles, mx: number, my: number, zWant: number): Leaf {
  let parent: Leaf | null = null;
  for (let z = 0; ; z++) {
    const n = 2 ** z;
    const leaf = frame.at(z, Math.min(n - 1, Math.floor(mx * n)), Math.min(n - 1, Math.floor(my * n)));
    if (leaf.url && leaf.maxZoom < z && parent) return parent;
    if (settles(leaf, zWant)) return leaf;
    parent = leaf;
  }
}
