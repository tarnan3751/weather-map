// Just enough TopoJSON to draw the fallback basemap: polygons to GeoJSON, and
// meshes of shared (border) or unshared (coast) arcs. Saves a dependency.

type Pos = [number, number];

interface TopoGeometry {
  type: string;
  arcs?: unknown;
  properties?: Record<string, unknown>;
}

export interface Topology {
  type: 'Topology';
  transform?: { scale: [number, number]; translate: [number, number] };
  arcs: Pos[][];
  objects: Record<string, { type: 'GeometryCollection'; geometries: TopoGeometry[] }>;
}

function decodeArcs(topo: Topology): Pos[][] {
  const t = topo.transform;
  return topo.arcs.map((arc) => {
    let x = 0;
    let y = 0;
    return arc.map(([dx, dy]) => {
      if (!t) return [dx, dy] as Pos;
      x += dx;
      y += dy;
      return [x * t.scale[0] + t.translate[0], y * t.scale[1] + t.translate[1]] as Pos;
    });
  });
}

function ring(arcs: Pos[][], idx: number[]): Pos[] {
  const out: Pos[] = [];
  for (const i of idx) {
    const a = i < 0 ? arcs[~i].slice().reverse() : arcs[i];
    // Consecutive arcs share their joining point.
    out.push(...(out.length ? a.slice(1) : a));
  }
  return out;
}

export function polygons(topo: Topology, name: string): GeoJSON.FeatureCollection {
  const arcs = decodeArcs(topo);
  const features: GeoJSON.Feature[] = [];
  for (const g of topo.objects[name]?.geometries ?? []) {
    let coords: Pos[][][] | null = null;
    if (g.type === 'Polygon') coords = [(g.arcs as number[][]).map((r) => ring(arcs, r))];
    else if (g.type === 'MultiPolygon') coords = (g.arcs as number[][][]).map((p) => p.map((r) => ring(arcs, r)));
    if (!coords) continue;
    features.push({ type: 'Feature', properties: g.properties ?? {}, geometry: { type: 'MultiPolygon', coordinates: coords } });
  }
  return { type: 'FeatureCollection', features };
}

/** Arcs used by two different geometries (`shared`: borders) or by only one
 *  (coastlines), as one MultiLineString. */
export function mesh(topo: Topology, name: string, shared: boolean): GeoJSON.Feature {
  const arcs = decodeArcs(topo);
  const users = new Map<number, Set<number>>();
  const visit = (v: unknown, gi: number) => {
    if (typeof v === 'number') {
      const i = v < 0 ? ~v : v;
      let s = users.get(i);
      if (!s) users.set(i, (s = new Set()));
      s.add(gi);
    } else if (Array.isArray(v)) v.forEach((x) => visit(x, gi));
  };
  (topo.objects[name]?.geometries ?? []).forEach((g, gi) => visit(g.arcs, gi));
  const lines: Pos[][] = [];
  for (const [i, s] of users) if ((s.size > 1) === shared) lines.push(arcs[i]);
  return { type: 'Feature', properties: {}, geometry: { type: 'MultiLineString', coordinates: lines } };
}
