// A small Mapbox Vector Tile 2.1 encoder for the mock's alert tiles: polygons
// only, one layer, protobuf written by hand so the mock needs no
// dependencies. Shapes are not simplified; they are clipped to the buffered
// tile so a zoom 16 tile does not carry coordinates far outside it.
// Plain TypeScript with only erasable syntax so Node can also run it directly.

import type { LonLatBox } from '../src/api/types.ts';

/** `[lon, lat]` pairs; closing the ring is optional. */
export type Ring = [number, number][];
/** Exterior ring first, then its holes. */
export type Polygon = Ring[];

type PropValue = string | number | boolean;

export interface MvtFeature {
  polygons: Polygon[];
  /** Null and undefined values are left out of the tile. */
  properties: Record<string, PropValue | null | undefined>;
}

export const EXTENT = 4096;
export const BUFFER = 64;
const MAX_LAT = 85.0511287798066;

// ------------------------------------------------------------- geometry

type Pt = [number, number];

/** Web Mercator, 0..1 across the world. */
export const mercX = (lon: number) => (lon + 180) / 360;
export function mercY(lat: number): number {
  const s = Math.sin((Math.max(-MAX_LAT, Math.min(MAX_LAT, lat)) * Math.PI) / 180);
  return 0.5 - Math.log((1 + s) / (1 - s)) / (4 * Math.PI);
}

/** A point in a tile's own coordinates, 0..EXTENT inside it, y down. */
export function toTile(lon: number, lat: number, z: number, x: number, y: number): Pt {
  const n = 2 ** z;
  return [(mercX(lon) * n - x) * EXTENT, (mercY(lat) * n - y) * EXTENT];
}

/**
 * Sutherland-Hodgman against one axis-aligned half-plane: keeps the part of
 * an open ring where `pt[axis] >= v` (`keepAbove`) or `<= v`. Concave rings
 * can come out with zero-width bridges along the line, which fills ignore.
 */
export function clipAxis(ring: Pt[], axis: 0 | 1, v: number, keepAbove: boolean): Pt[] {
  const out: Pt[] = [];
  const n = ring.length;
  if (n === 0) return out;
  const inside = (p: Pt) => (keepAbove ? p[axis] >= v : p[axis] <= v);
  const cut = (a: Pt, b: Pt): Pt => {
    const t = (v - a[axis]) / (b[axis] - a[axis]);
    const p: Pt = [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
    p[axis] = v;
    return p;
  };
  let prev = ring[n - 1];
  let prevIn = inside(prev);
  for (const cur of ring) {
    const curIn = inside(cur);
    if (curIn) {
      if (!prevIn) out.push(cut(prev, cur));
      out.push(cur);
    } else if (prevIn) out.push(cut(prev, cur));
    prev = cur;
    prevIn = curIn;
  }
  return out;
}

function clipBox(ring: Pt[], lo: number, hi: number): Pt[] {
  let r = clipAxis(ring, 0, lo, true);
  r = clipAxis(r, 0, hi, false);
  r = clipAxis(r, 1, lo, true);
  return clipAxis(r, 1, hi, false);
}

/** Twice the signed area by the surveyor's formula; positive is clockwise
 *  when y points down, which MVT requires of exterior rings. */
export function ringArea2(ring: readonly Pt[]): number {
  let s = 0;
  for (let i = 0, j = ring.length - 1; i < ring.length; j = i++) s += ring[j][0] * ring[i][1] - ring[i][0] * ring[j][1];
  return s;
}

/** Open ring without the closing point, so every vertex counts once. */
function open(ring: readonly Pt[]): Pt[] {
  const r = ring.map((p): Pt => [p[0], p[1]]);
  while (r.length > 1 && r[0][0] === r[r.length - 1][0] && r[0][1] === r[r.length - 1][1]) r.pop();
  return r;
}

/** Integer tile coordinates, consecutive repeats dropped; null when nothing
 *  with an area is left. */
function quantize(ring: Pt[]): Pt[] | null {
  const out: Pt[] = [];
  for (const p of ring) {
    const q: Pt = [Math.round(p[0]), Math.round(p[1])];
    const last = out[out.length - 1];
    if (!last || last[0] !== q[0] || last[1] !== q[1]) out.push(q);
  }
  while (out.length > 1 && out[0][0] === out[out.length - 1][0] && out[0][1] === out[out.length - 1][1]) out.pop();
  return out.length >= 3 && ringArea2(out) !== 0 ? out : null;
}

/** A polygon's rings in tile coordinates, clipped, exterior clockwise and
 *  holes counter-clockwise; empty when none of it reaches the tile. */
function tilePolygon(poly: Polygon, z: number, x: number, y: number): Pt[][] {
  const rings: Pt[][] = [];
  for (let i = 0; i < poly.length; i++) {
    const projected = open(poly[i]).map(([lon, lat]) => toTile(lon, lat, z, x, y));
    const r = quantize(clipBox(projected, -BUFFER, EXTENT + BUFFER));
    if (!r) {
      // Without its exterior a polygon's holes mean nothing.
      if (i === 0) return [];
      continue;
    }
    const wantPositive = i === 0;
    if (ringArea2(r) > 0 !== wantPositive) r.reverse();
    rings.push(r);
  }
  return rings;
}

/** A ring's bounding box, by plain minimum and maximum. */
export function ringBox(r: Ring): LonLatBox {
  const b = { west: Infinity, south: Infinity, east: -Infinity, north: -Infinity };
  for (const [lon, lat] of r) {
    if (lon < b.west) b.west = lon;
    if (lon > b.east) b.east = lon;
    if (lat < b.south) b.south = lat;
    if (lat > b.north) b.north = lat;
  }
  return b;
}

/** Whether a polygon's bounding box meets the tile grown by the buffer. */
function meetsTile(poly: Polygon, z: number, x: number, y: number): boolean {
  if (!poly[0] || poly[0].length === 0) return false;
  const b = ringBox(poly[0]);
  const [x0, y0] = toTile(b.west, b.north, z, x, y);
  const [x1, y1] = toTile(b.east, b.south, z, x, y);
  return x1 >= -BUFFER && x0 <= EXTENT + BUFFER && y1 >= -BUFFER && y0 <= EXTENT + BUFFER;
}

const command = (id: number, count: number) => (id & 7) | (count << 3);
const zigzag = (n: number) => ((n << 1) ^ (n >> 31)) >>> 0;

/** MoveTo, LineTo and ClosePath commands for a polygon feature's rings. */
function geometry(rings: Pt[][]): number[] {
  const g: number[] = [];
  let cx = 0;
  let cy = 0;
  for (const r of rings) {
    g.push(command(1, 1), zigzag(r[0][0] - cx), zigzag(r[0][1] - cy));
    cx = r[0][0];
    cy = r[0][1];
    g.push(command(2, r.length - 1));
    for (let i = 1; i < r.length; i++) {
      g.push(zigzag(r[i][0] - cx), zigzag(r[i][1] - cy));
      cx = r[i][0];
      cy = r[i][1];
    }
    g.push(command(7, 1));
  }
  return g;
}

// ------------------------------------------------------------- protobuf

const utf8 = new TextEncoder();

class Writer {
  private buf = new Uint8Array(256);
  private pos = 0;

  private room(n: number) {
    if (this.pos + n <= this.buf.length) return;
    let size = this.buf.length * 2;
    while (size < this.pos + n) size *= 2;
    const b = new Uint8Array(size);
    b.set(this.buf.subarray(0, this.pos));
    this.buf = b;
  }

  /** Non-negative integers up to 2^53; division keeps it exact past 32 bits. */
  varint(v: number) {
    this.room(10);
    while (v >= 0x80) {
      this.buf[this.pos++] = (v % 0x80) | 0x80;
      v = Math.floor(v / 0x80);
    }
    this.buf[this.pos++] = v;
  }

  tag(field: number, wire: number) {
    this.varint(field * 8 + wire);
  }

  uint(field: number, v: number) {
    this.tag(field, 0);
    this.varint(v);
  }

  bytes(field: number, data: Uint8Array) {
    this.tag(field, 2);
    this.varint(data.length);
    this.room(data.length);
    this.buf.set(data, this.pos);
    this.pos += data.length;
  }

  string(field: number, s: string) {
    this.bytes(field, utf8.encode(s));
  }

  double(field: number, v: number) {
    this.tag(field, 1);
    this.room(8);
    new DataView(this.buf.buffer, this.buf.byteOffset + this.pos, 8).setFloat64(0, v, true);
    this.pos += 8;
  }

  packed(field: number, values: readonly number[]) {
    const w = new Writer();
    for (const v of values) w.varint(v);
    this.bytes(field, w.finish());
  }

  finish(): Uint8Array {
    return this.buf.slice(0, this.pos);
  }
}

/** A `Value` message: strings, booleans, integers (unsigned, or zigzag when
 *  negative) and doubles for the rest. */
function valueMessage(v: PropValue): Uint8Array {
  const w = new Writer();
  if (typeof v === 'string') w.string(1, v);
  else if (typeof v === 'boolean') w.uint(7, v ? 1 : 0);
  else if (Number.isSafeInteger(v) && v >= 0) w.uint(5, v);
  else if (Number.isSafeInteger(v)) w.uint(6, -2 * v - 1);
  else w.double(3, v);
  return w.finish();
}

/**
 * One tile with one layer of polygon features, in the order given. A feature
 * is kept when any of its polygons' bounding boxes meets the buffered tile
 * and something of it is left after clipping. With no features left the
 * tile is empty (zero bytes), as the protocol asks.
 */
export function encodeTile(layer: string, features: readonly MvtFeature[], z: number, x: number, y: number): Uint8Array {
  const keys: string[] = [];
  const keyIndex = new Map<string, number>();
  const values: PropValue[] = [];
  const valueIndex = new Map<string, number>();
  const encoded: Uint8Array[] = [];

  for (const f of features) {
    const rings: Pt[][] = [];
    for (const poly of f.polygons) if (meetsTile(poly, z, x, y)) rings.push(...tilePolygon(poly, z, x, y));
    if (rings.length === 0) continue;
    const tags: number[] = [];
    for (const [k, v] of Object.entries(f.properties)) {
      if (v === null || v === undefined) continue;
      let ki = keyIndex.get(k);
      if (ki === undefined) {
        ki = keys.push(k) - 1;
        keyIndex.set(k, ki);
      }
      const vk = `${typeof v}:${v}`;
      let vi = valueIndex.get(vk);
      if (vi === undefined) {
        vi = values.push(v) - 1;
        valueIndex.set(vk, vi);
      }
      tags.push(ki, vi);
    }
    const w = new Writer();
    w.packed(2, tags);
    w.uint(3, 3); // POLYGON
    w.packed(4, geometry(rings));
    encoded.push(w.finish());
  }
  if (encoded.length === 0) return new Uint8Array(0);

  const l = new Writer();
  l.uint(15, 2);
  l.string(1, layer);
  for (const f of encoded) l.bytes(2, f);
  for (const k of keys) l.string(3, k);
  for (const v of values) l.bytes(4, valueMessage(v));
  l.uint(5, EXTENT);
  const t = new Writer();
  t.bytes(3, l.finish());
  return t.finish();
}
