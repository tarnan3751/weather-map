import { VectorTile, classifyRings, type VectorTileFeature } from '@mapbox/vector-tile';
import { PbfReader } from 'pbf';
import { describe, expect, it } from 'vitest';
import type { AlertProps } from '../src/api/types.ts';
import { Alerts, EUROGEOGRAPHICS, capId } from './alerts.ts';
import { UNSHAPED_SEED, UPDATED_SEED, WORLD } from './alertsWorld.ts';
import { pointInPolygon } from './geo.ts';
import { BUFFER, EXTENT, encodeTile, mercX, mercY, ringArea2, toTile } from './mvt.ts';

const T0 = Date.UTC(2026, 10, 6, 12, 1);
const KC = { lon: -94.5786, lat: 39.0997 };
const ALL = { bbox: null, country: null, unshaped: true, limit: 5000 };
const SEV: Record<string, number> = { Extreme: 4, Severe: 3, Moderate: 2, Minor: 1, Unknown: 0 };

const world = (a: Alerts) => a.list(ALL).features.filter((f) => f.properties.source !== 'us-nws');
const bySource = (a: Alerts, source: string) => world(a).find((f) => f.properties.source === source)!;

/** The tile at zoom `z` holding a point. */
const tileAt = (z: number, lon: number, lat: number): [number, number, number] => [z, Math.floor(mercX(lon) * 2 ** z), Math.floor(mercY(lat) * 2 ** z)];

function decode(body: Uint8Array) {
  const layer = new VectorTile(new PbfReader(body)).layers.alerts;
  const features: VectorTileFeature[] = [];
  for (let i = 0; i < (layer?.length ?? 0); i++) features.push(layer.feature(i));
  return { layer, features };
}

function tileOf(a: Alerts, z: number, x: number, y: number) {
  const t = a.tile(a.revision, z, x, y);
  if (t.status !== 200) throw new Error(`tile ${z}/${x}/${y}: ${t.status}`);
  return decode(t.body);
}

const ring = (pts: { x: number; y: number }[]) => pts.map((p): [number, number] => [p.x, p.y]);

describe('mock alerts: data', () => {
  const a = new Alerts(T0);

  it('has a believable world with stable, well-formed ids', () => {
    const w = world(a);
    expect(w.length).toBeGreaterThanOrEqual(25);
    expect(w.length).toBeLessThanOrEqual(40);
    for (const f of w) expect(f.properties.id).toMatch(/^cap:[0-9a-f]{20}$/);
    const ids = a.list(ALL).features.map((f) => f.properties.id);
    expect(new Set(ids).size).toBe(ids.length);
    expect(new Alerts(T0 + 3_600_000).list(ALL).features.map((f) => f.properties.id).sort()).toEqual([...ids].sort());
    expect(capId(UPDATED_SEED)).toBe(bySource(a, 'de-dwd-de').properties.id);
  });

  it('names the issuer, the category and an assumed end as the server does', () => {
    const cats = new Set(['met', 'geo', 'safety', 'security', 'rescue', 'fire', 'health', 'env', 'transport', 'infra', 'cbrne', 'other']);
    for (const f of a.list(ALL).features) {
      const p = f.properties;
      expect(cats.has(p.category), p.id).toBe(true);
      expect(typeof p.expires_assumed).toBe('boolean');
      // The issuer's names are for the detail only.
      expect(p).not.toHaveProperty('source_name');
    }
    const nws = a.list(ALL).features.find((f) => f.properties.source === 'us-nws')!.properties;
    const d = a.detail(nws.id)!;
    expect([d.country, d.source_name, d.country_name, d.expires_assumed]).toEqual(['us', 'United States of America: National Weather Service', 'United States of America', false]);
    // The Icelandic swarm came with no expiry: a day after onset, assumed.
    const quake = WORLD.find((t) => t.expires === null)!;
    const q = a.detail(capId(quake.n))!;
    expect(q.expires_assumed).toBe(true);
    expect(Date.parse(q.expires!)).toBe(T0 + (quake.onset + 24) * 3_600_000);
    expect(q.infos.every((i) => i.expires === null)).toBe(true);
  });

  it('shows English when the message has it, else its first language', () => {
    for (const t of WORLD) {
      const d = a.detail(capId(t.n))!;
      const en = t.infos.find((i) => /^en(-|$)/.test(i.language));
      const shown = en ?? t.infos[0];
      expect(d.language, t.source).toBe(shown.language);
      expect(d.event).toBe(shown.event);
      expect(d.headline).toBe(shown.headline ?? null);
      expect(d.infos.map((i) => i.language)).toEqual(t.infos.map((i) => i.language));
    }
  });

  it('gives world alerts expires in the future and no ends; NWS alerts have ends', () => {
    for (const f of a.list(ALL).features) {
      const p = f.properties;
      if (p.source === 'us-nws') {
        expect(p.ends).not.toBeNull();
        expect(p.country).toBe('us');
        expect(p.language).toBe('en-US');
      } else {
        expect(p.ends).toBeNull();
        expect(Date.parse(p.expires!)).toBeGreaterThan(T0);
      }
    }
  });

  it('puts every shaped alert\'s anchor on its own shape', () => {
    for (const f of a.list(ALL).features) {
      const p = f.properties;
      if (p.geom_source === 'none') continue;
      expect(p.anchor, p.id).toBeTruthy();
      expect(a.at(p.anchor![0], p.anchor![1]).map((x) => x.id), p.event).toContain(p.id);
    }
  });

  it('keeps every ring on one side of the antimeridian', () => {
    for (const f of a.list(ALL).features) {
      if (!f.geometry) continue;
      const polys = (f.geometry.type === 'Polygon' ? [f.geometry.coordinates] : f.geometry.coordinates) as [number, number][][][];
      for (const r of polys.flat()) {
        for (let i = 1; i < r.length; i++) {
          expect(Math.abs(r[i][0])).toBeLessThanOrEqual(180);
          expect(Math.abs(r[i][0] - r[i - 1][0])).toBeLessThan(90);
        }
      }
    }
  });

  it('has the alerts the panel is tested against', () => {
    const swiss = a.detail(bySource(a, 'ch-meteoswiss-en').properties.id)!;
    expect(swiss.infos.map((i) => i.language.slice(0, 2))).toEqual(['de', 'fr', 'it', 'en']);
    const arabic = a.detail(bySource(a, 'sa-ncm-ar').properties.id)!;
    expect(arabic.language).toMatch(/^ar/);
    const bare = a.detail(bySource(a, 'br-inmet-pt').properties.id)!;
    expect([bare.headline, bare.instruction, bare.sender_name, bare.infos[0].web]).toEqual([null, null, null, null]);
    const nuts = world(a).filter((f) => f.properties.geom_source === 'nuts');
    expect(nuts.length).toBeGreaterThanOrEqual(5);
    expect(new Set(nuts.map((f) => f.properties.severity)).size).toBeGreaterThan(1);
    expect(a.tileJson().attribution).toBe(EUROGEOGRAPHICS);
  });
});

describe('mock alerts: list and point queries', () => {
  const a = new Alerts(T0);

  it('lists shaped alerts most severe first, then newest', () => {
    const c = a.list({ ...ALL, unshaped: false });
    expect(c.revision).toBe(a.revision);
    expect(a.revision).toBeGreaterThan(0);
    expect(c.truncated).toBe(false);
    expect(c.features.every((f) => f.geometry !== null)).toBe(true);
    const p = c.features.map((f) => f.properties);
    for (let i = 1; i < p.length; i++) {
      const d = SEV[p[i - 1].severity] - SEV[p[i].severity];
      expect(d).toBeGreaterThanOrEqual(0);
      if (d === 0) expect(Date.parse(p[i - 1].sent!)).toBeGreaterThanOrEqual(Date.parse(p[i].sent!));
    }
  });

  it('lists alerts without a shape only when asked, with a null geometry', () => {
    expect(a.list({ ...ALL, unshaped: false, country: 'it' }).features).toHaveLength(0);
    const it = a.list({ ...ALL, country: 'it' }).features;
    expect(it).toHaveLength(1);
    expect(it[0].geometry).toBeNull();
    expect(it[0].properties.geom_source).toBe('none');
    expect(it[0].properties.anchor).toEqual([12.810059, 42.306769]);
    expect(a.at(12.810059, 42.306769)).toHaveLength(0);
  });

  it('matches a bbox by shape bounds, and unshaped alerts by anchor', () => {
    const italy = { west: 12, south: 41.5, east: 13.5, north: 43 };
    expect(a.list({ ...ALL, bbox: italy, unshaped: false }).features).toHaveLength(0);
    expect(a.list({ ...ALL, bbox: italy }).features.map((f) => f.properties.country)).toEqual(['it']);
    const kansas = a.list({ ...ALL, bbox: { west: -98, south: 34, east: -95, north: 36 } }).features.map((f) => f.properties.event);
    expect(kansas).toContain('Tornado Watch');
    expect(kansas).not.toContain('Red Flag Warning');
  });

  it('finds the antimeridian alert with a box that crosses 180, and only then', () => {
    const fiji = bySource(a, 'fj-fms-en').properties.id;
    const across = a.list({ ...ALL, bbox: { west: 179.5, south: -20, east: -179.5, north: -15 } }).features.map((f) => f.properties.id);
    expect(across).toContain(fiji);
    const westOnly = a.list({ ...ALL, bbox: { west: 178, south: -20, east: 179.9, north: -15 } }).features.map((f) => f.properties.id);
    const eastOnly = a.list({ ...ALL, bbox: { west: -179.9, south: -20, east: -178, north: -15 } }).features.map((f) => f.properties.id);
    expect(westOnly).toContain(fiji);
    expect(eastOnly).toContain(fiji);
    const elsewhere = a.list({ ...ALL, bbox: { west: -10, south: -20, east: 10, north: -15 } }).features.map((f) => f.properties.id);
    expect(elsewhere).not.toContain(fiji);
    expect(a.at(179.9, -16.5).map((x) => x.id)).toContain(fiji);
    expect(a.at(-179.5, -17).map((x) => x.id)).toContain(fiji);
  });

  it('caps the list at limit and says so', () => {
    const c = a.list({ ...ALL, limit: 5 });
    expect(c.features).toHaveLength(5);
    expect(c.truncated).toBe(true);
  });

  it('answers point queries from the full shape, holes included', () => {
    const tyrol = bySource(a, 'at-geosphere-de').properties.id;
    expect(a.at(10.6, 47.25).map((x) => x.id)).toContain(tyrol);
    // Innsbruck is not one of the regions named.
    expect(a.at(11.39, 47.26).map((x) => x.id)).not.toContain(tyrol);
  });
});

describe('mock alerts: live warning, updates and revisions', () => {
  it('issues and clears the Kansas City warning under one id', () => {
    const a = new Alerts(T0);
    const r0 = a.revision;
    expect(a.clearLive()).toBeNull();
    expect(a.detail(a.liveId)).toBeUndefined();
    const issued = a.issueLive(T0 + 6_000);
    expect(issued.added).toEqual([a.liveId]);
    // At least the change time in milliseconds, and rising.
    expect(r0).toBe(T0);
    expect(issued.revision).toBe(T0 + 6_000);
    expect(issued.alert.instruction).toBeTruthy();
    const here: AlertProps[] = a.at(KC.lon, KC.lat);
    expect(here[0].id).toBe(a.liveId);
    expect(here[0].event).toBe('Tornado Warning');
    expect(a.detail(a.liveId)!.active).toBe(true);
    const cleared = a.clearLive()!;
    expect(cleared.removed).toEqual([a.liveId]);
    expect(cleared.revision).toBeGreaterThan(issued.revision);
    expect(a.at(KC.lon, KC.lat).map((x) => x.id)).not.toContain(a.liveId);
    expect(a.detail(a.liveId)!.active).toBe(false);
    expect(a.issueLive(T0 + 60_000).added).toEqual([a.liveId]);
    expect(a.revision).toBe(T0 + 60_000);
    // A mock started later never starts below one started earlier.
    expect(new Alerts(T0 + 1000).revision).toBeGreaterThanOrEqual(r0);
  });

  it('ends and restores the shapeless Kenyan alert without touching the map', () => {
    const a = new Alerts(T0);
    const id = capId(UNSHAPED_SEED);
    const tile = a.tile(a.revision, 0, 0, 0);
    expect(a.list(ALL).features.some((f) => f.properties.id === id)).toBe(true);
    const r = a.toggleUnshaped(T0 + 1000);
    expect(r.active).toBe(false);
    expect(a.list(ALL).features.some((f) => f.properties.id === id)).toBe(false);
    expect(a.tile(r.revision, 0, 0, 0)).toEqual(tile);
    expect(a.toggleUnshaped(T0 + 2000)).toEqual({ revision: T0 + 2000, active: true });
    expect(a.list(ALL).features.some((f) => f.properties.id === id)).toBe(true);
  });

  it('replaces the German alert with an Update under a new id', () => {
    const a = new Alerts(T0);
    const r0 = a.revision;
    const old = capId(UPDATED_SEED);
    const u = a.updateOne(T0 + 600_000);
    expect(u.removed).toEqual([old]);
    expect(u.added).toHaveLength(1);
    expect(u.added[0]).toMatch(/^cap:[0-9a-f]{20}$/);
    expect(u.added[0]).not.toBe(old);
    expect(u.revision).toBe(Math.max(r0 + 1, T0 + 600_000));
    const ids = a.list(ALL).features.map((f) => f.properties.id);
    expect(ids).toContain(u.added[0]);
    expect(ids).not.toContain(old);
    expect(a.detail(old)!.active).toBe(false);
    const next = a.detail(u.added[0])!;
    expect(next.message_type).toBe('Update');
    expect(next.active).toBe(true);
    expect(next.sent).toBe(new Date(T0 + 600_000).toISOString().replace(/\.\d{3}Z$/, 'Z'));
    // The counter keeps the ids apart, and the same in every run.
    const u2 = a.updateOne(T0 + 1_200_000);
    expect(u2.removed).toEqual(u.added);
    expect(u2.added[0]).not.toBe(u.added[0]);
    const b = new Alerts(T0);
    expect(b.updateOne(T0 + 600_000).added).toEqual(u.added);
  });
});

describe('mock alerts: vector tiles', () => {
  const a = new Alerts(T0);
  a.issueLive(T0);

  it('answers tile addresses and revisions as the protocol says', () => {
    const rev = a.revision;
    expect(a.tile(rev, 3, 1, 2)).toMatchObject({ status: 200, immutable: true });
    expect(a.tile(rev - 1, 3, 1, 2)).toMatchObject({ status: 200, immutable: false });
    expect(a.tile(rev + 1, 3, 1, 2)).toEqual({ status: 404 });
    expect(a.tile(rev, 17, 0, 0)).toEqual({ status: 404 });
    expect(a.tile(rev, 2, 4, 0)).toEqual({ status: 404 });
    expect(a.tile(rev, 2, 0, -1)).toEqual({ status: 404 });
    const json = a.tileJson();
    expect(json.revision).toBe(rev);
    expect(json.tiles).toEqual([`/api/alerts/tiles/${rev}/{z}/{x}/{y}.mvt`]);
    expect(json.maxzoom).toBe(12);
  });

  it('sends an empty body for a tile with nothing in it', () => {
    // The middle of the South Pacific.
    const [z, x, y] = tileAt(6, -130, -40);
    const t = a.tile(a.revision, z, x, y);
    expect(t.status === 200 && t.body.length).toBe(0);
  });

  it('writes one layer with every shaped alert at zoom 0, in ascending level', () => {
    const { layer, features } = tileOf(a, 0, 0, 0);
    expect(layer.version).toBe(2);
    expect(layer.extent).toBe(EXTENT);
    const shaped = a.list({ ...ALL, unshaped: false }).features;
    expect(features).toHaveLength(shaped.length);
    const levels = features.map((f) => f.properties.level as number);
    expect([...levels].sort((x, y) => x - y)).toEqual(levels);
    const unshaped = a.list(ALL).features.filter((f) => !f.geometry).map((f) => f.properties.id);
    expect(features.some((f) => unshaped.includes(f.properties.id as string))).toBe(false);
  });

  it('carries the protocol\'s properties', () => {
    const { features } = tileOf(a, 0, 0, 0);
    const byId = new Map(features.map((f) => [f.properties.id as string, f]));
    for (const lf of a.list({ ...ALL, unshaped: false }).features) {
      const p = lf.properties;
      const f = byId.get(p.id)!;
      expect(f.type).toBe(3);
      expect(Object.keys(f.properties).sort()).toEqual(['category', 'country', 'event', 'expires', 'expires_assumed', 'geom_source', 'headline', 'id', 'level', 'severity', 'source'].sort());
      for (const k of ['event', 'severity', 'category', 'expires_assumed', 'source', 'country', 'geom_source'] as const) expect(f.properties[k], k).toBe(p[k]);
      expect(f.properties.level).toBe(SEV[p.severity]);
      expect(f.properties.expires).toBe(Math.floor(Date.parse((p.ends ?? p.expires)!) / 1000));
      const headline = f.properties.headline as string;
      expect(Array.from(headline).length).toBeLessThanOrEqual(120);
      expect((p.headline ?? p.event).startsWith(headline)).toBe(true);
    }
    // The German headline is longer than 120 characters.
    const de = byId.get(capId(UPDATED_SEED))!;
    expect(Array.from(de.properties.headline as string)).toHaveLength(120);
  });

  it('draws polygons that contain their anchors, exteriors clockwise and holes counter-clockwise', () => {
    for (const lf of a.list({ ...ALL, unshaped: false }).features) {
      const p = lf.properties;
      const [z, x, y] = tileAt(7, p.anchor![0], p.anchor![1]);
      const f = tileOf(a, z, x, y).features.find((g) => g.properties.id === p.id);
      expect(f, p.event).toBeTruthy();
      const rings = f!.loadGeometry().map(ring);
      const polys = classifyRings(f!.loadGeometry()).map((poly) => poly.map(ring));
      expect(polys.length).toBeGreaterThan(0);
      for (const poly of polys) {
        expect(ringArea2(poly[0])).toBeGreaterThan(0);
        for (const h of poly.slice(1)) expect(ringArea2(h)).toBeLessThan(0);
      }
      const [ax, ay] = toTile(p.anchor![0], p.anchor![1], z, x, y);
      const inside = polys.some((poly) => pointInPolygon(ax, ay, poly[0]) && !poly.slice(1).some((h) => pointInPolygon(ax, ay, h)));
      expect(inside, `${p.event} anchor in its tile polygon`).toBe(true);
      expect(rings.length).toBeGreaterThan(0);
    }
  });

  it('keeps the Tyrol warning\'s hole', () => {
    const p = bySource(a, 'at-geosphere-de').properties;
    const [z, x, y] = tileAt(6, 11.35, 47.22);
    const f = tileOf(a, z, x, y).features.find((g) => g.properties.id === p.id)!;
    const polys = classifyRings(f.loadGeometry());
    expect(polys).toHaveLength(1);
    expect(polys[0]).toHaveLength(2);
    const [hx, hy] = toTile(11.39, 47.26, z, x, y);
    expect(pointInPolygon(hx, hy, ring(polys[0][1]))).toBe(true);
  });

  it('clips shapes to the buffered tile at high zoom', () => {
    const [z, x, y] = tileAt(14, KC.lon, KC.lat);
    const { features } = tileOf(a, z, x, y);
    const live = features.find((f) => f.properties.id === a.liveId)!;
    expect(live).toBeTruthy();
    for (const r of live.loadGeometry()) {
      for (const pt of r) {
        expect(pt.x).toBeGreaterThanOrEqual(-BUFFER);
        expect(pt.x).toBeLessThanOrEqual(EXTENT + BUFFER);
        expect(pt.y).toBeGreaterThanOrEqual(-BUFFER);
        expect(pt.y).toBeLessThanOrEqual(EXTENT + BUFFER);
      }
    }
    // Deep inside the warning the tile is the buffered square.
    expect(Math.abs(ringArea2(ring(live.loadGeometry()[0])) / 2 - (EXTENT + 2 * BUFFER) ** 2)).toBeLessThan(1);
  });

  it('draws the Fiji alert on both sides of the antimeridian', () => {
    const id = bySource(a, 'fj-fms-en').properties.id;
    const west = tileOf(a, 4, 15, 8).features.find((f) => f.properties.id === id);
    const east = tileOf(a, 4, 0, 8).features.find((f) => f.properties.id === id);
    expect(west && east).toBeTruthy();
    for (const f of [west!, east!]) {
      for (const r of f.loadGeometry()) for (const pt of r) expect(Math.abs(pt.x - EXTENT / 2)).toBeLessThanOrEqual(EXTENT / 2 + BUFFER);
    }
  });

  it('includes a feature that only reaches into the buffer', () => {
    // A square just past the tile's east edge, inside the 64-unit buffer.
    const tile = (lonOff: number) => {
      const n = 2 ** 5;
      const lon0 = (16 / n) * 360 - 180 + lonOff;
      return encodeTile('alerts', [{ polygons: [[[[lon0, 10], [lon0 + 0.1, 10], [lon0 + 0.1, 10.1], [lon0, 10.1]]]], properties: { id: 'x' } }], 5, 15, 15);
    };
    // 64 tile units at zoom 5 are 360 / 32 / 64 degrees.
    const unit = 360 / 32 / EXTENT;
    expect(decode(tile(unit * 20)).features).toHaveLength(1);
    expect(tile(unit * 80).length).toBe(0);
  });
});
