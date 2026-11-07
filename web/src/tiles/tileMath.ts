// Web-Mercator tile arithmetic, in the same conventions as
// crates/wx-core/src/proj.rs: x, y in the unit square, y = 0 at the north edge.

export const MAX_MERCATOR_LAT = 85.0511287798066;
/** MapLibre's zoom is defined for 512-pixel tiles. */
export const MAP_TILE_PX = 512;

export const lonToX = (lon: number) => (lon + 180) / 360;
export function latToY(lat: number): number {
  const s = Math.sin((Math.max(-MAX_MERCATOR_LAT, Math.min(MAX_MERCATOR_LAT, lat)) * Math.PI) / 180);
  return 0.5 - Math.log((1 + s) / (1 - s)) / (4 * Math.PI);
}
export const xToLon = (x: number) => x * 360 - 180;
// `180 / PI` first, as Rust's `to_degrees` does, so tile edges agree with the
// server's to the last bit wherever the maths libraries do.
export const yToLat = (y: number) => Math.atan(Math.sinh(Math.PI * (1 - 2 * y))) * (180 / Math.PI);

export interface TileID {
  z: number;
  x: number;
  y: number;
}

export interface View {
  /** Map centre in mercator units; x may lie outside 0..1 on a world copy. */
  centerX: number;
  centerY: number;
  /** MapLibre zoom (512-pixel tiles). */
  zoom: number;
  /** Viewport in CSS pixels. */
  width: number;
  height: number;
}

/**
 * Tile zoom the view wants: the nearest integer, so a 256-cell tile covers
 * 362 to 724 CSS pixels (1.4 to 2.8 pixels per cell). Where the data goes
 * less deep the shallower tiles are simply drawn larger.
 */
export function tileZoomFor(mapZoom: number, maxZoom: number, bias = 0): number {
  return Math.max(0, Math.min(maxZoom, Math.round(mapZoom + bias)));
}

/** The visible area of a north-up, unpitched view in mercator units. */
export function viewRect(view: View, marginPx = 0): { x0: number; x1: number; y0: number; y1: number } {
  const world = MAP_TILE_PX * 2 ** view.zoom;
  const halfW = (view.width / 2 + marginPx) / world;
  const halfH = (view.height / 2 + marginPx) / world;
  return { x0: view.centerX - halfW, x1: view.centerX + halfW, y0: view.centerY - halfH, y1: view.centerY + halfH };
}

/** A texture-coordinate window: textureUV = (ox, oy) + slotUV * s. */
export interface SubRect {
  ox: number;
  oy: number;
  s: number;
}

/** Where `child` sits inside `parent`, as a texture-coordinate transform:
 *  parentUV = offset + childUV * scale. */
export function subRect(child: TileID, parent: TileID): SubRect {
  const d = child.z - parent.z;
  const k = 2 ** d;
  return { ox: child.x / k - parent.x, oy: child.y / k - parent.y, s: 1 / k };
}

export type TileState = 'ready' | 'empty' | 'missing' | 'loading' | 'error';

/** Mercator position of a point inside a tile, 0..1 within it. */
export function tileFraction(t: TileID, mx: number, my: number): { u: number; v: number } {
  const n = 2 ** t.z;
  return { u: mx * n - t.x, v: my * n - t.y };
}
