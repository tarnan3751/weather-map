// Basemap styles: the Protomaps basemap from the server's PMTiles archive,
// tuned for a weather map (quiet land and water, borders and labels that stay
// legible over bright colours), and a fallback built from bundled Natural
// Earth / Census outlines for when there is no archive or it fails to load.

import { layers as protomapsLayers, namedFlavor, type Flavor } from '@protomaps/basemaps';
import type { LayerSpecification, StyleSpecification } from '@maplibre/maplibre-gl-style-spec';
import { mesh, polygons, type Topology } from './topojson.ts';

// Protomaps' basemap fonts and sprites, served from `public/basemap-assets`
// so the page needs no other host (licences next to them). MapLibre fetches
// them from a worker, so the URLs must be absolute.
const ASSETS = `${typeof location === 'undefined' ? 'http://localhost' : location.origin}/basemap-assets`;
export const GLYPHS = `${ASSETS}/fonts/{fontstack}/{range}.pbf`;
export const SPRITE = `${ASSETS}/sprites/v4/dark`;

export const COLORS = {
  water: '#0b0f15',
  land: '#171b22',
  border: '#8a93a3',
  state: '#5d6574',
  coast: '#3b4352',
  label: '#d5dae2',
  labelHalo: '#07090c',
};

// The weather sources are credited next to this, as they come into view.
const ATTRIBUTION = '<a href="https://protomaps.com">Protomaps</a> © <a href="https://openstreetmap.org/copyright">OpenStreetMap</a>';

function flavor(): Flavor {
  const f = namedFlavor('dark');
  return {
    ...f,
    background: COLORS.water,
    earth: COLORS.land,
    water: COLORS.water,
    park_a: '#182019',
    park_b: '#18211b',
    wood_a: '#181d1c',
    wood_b: '#181d1c',
    scrub_a: '#1a1d1c',
    scrub_b: '#1a1d1c',
    glacier: '#1d2026',
    sand: '#1c1d1f',
    beach: '#1d1e20',
    industrial: '#1a1b1f',
    hospital: '#1c1b1e',
    school: '#1c1b1e',
    aerodrome: '#1b1c20',
    pedestrian: '#191b20',
    military: '#1b1b1e',
    zoo: '#1a1d1c',
    buildings: '#1e2128',
    highway: '#4c5361',
    major: '#3a404b',
    minor_a: '#2c313a',
    minor_b: '#2a2e36',
    link: '#3a404b',
    other: '#2a2e36',
    boundaries: COLORS.border,
    city_label: '#d9dee6',
    city_label_halo: COLORS.labelHalo,
    state_label: '#8e97a6',
    state_label_halo: COLORS.labelHalo,
    country_label: '#6d7684',
    ocean_label: '#56627a',
    subplace_label: '#9aa2ae',
    subplace_label_halo: COLORS.labelHalo,
    roads_label_major: '#8d95a1',
    roads_label_major_halo: COLORS.labelHalo,
    roads_label_minor: '#737b87',
    roads_label_minor_halo: COLORS.labelHalo,
    landcover: {
      grassland: '#181d19',
      barren: '#1b1c1d',
      urban_area: '#1c1e24',
      farmland: '#181c1a',
      glacier: '#1d2026',
      scrub: '#1a1d1b',
      forest: '#161c1a',
    },
  };
}

/** Labels are drawn over vivid weather colours, so they get firmer halos. */
function tune(layers: LayerSpecification[]): LayerSpecification[] {
  return layers
    .filter((l) => l.id !== 'pois' && l.id !== 'address_label' && l.id !== 'roads_oneway')
    .map((l) => {
      if (l.type === 'symbol') {
        return { ...l, paint: { ...l.paint, 'text-halo-width': 1.6, 'text-halo-blur': 0.4 } } as LayerSpecification;
      }
      if (l.id === 'boundaries_country') {
        return { ...l, paint: { ...l.paint, 'line-width': ['interpolate', ['linear'], ['zoom'], 2, 0.8, 8, 1.6], 'line-opacity': 0.9 } } as LayerSpecification;
      }
      if (l.id === 'boundaries') {
        return { ...l, paint: { ...l.paint, 'line-color': COLORS.state, 'line-width': ['interpolate', ['linear'], ['zoom'], 3, 0.5, 8, 1.1], 'line-opacity': 0.85 } } as LayerSpecification;
      }
      return l;
    });
}

export function protomapsStyle(archiveUrl: string): StyleSpecification {
  const abs = new URL(archiveUrl, location.href).href;
  const layers = tune(protomapsLayers('protomaps', flavor(), { lang: 'en' }));
  // A faint coastline above the weather so shores stay readable under colour.
  const waterIdx = layers.findIndex((l) => l.id === 'boundaries_country');
  layers.splice(waterIdx, 0, {
    id: 'wx-coastline',
    type: 'line',
    source: 'protomaps',
    'source-layer': 'water',
    filter: ['==', ['geometry-type'], 'Polygon'],
    paint: { 'line-color': COLORS.coast, 'line-width': 0.7, 'line-opacity': ['interpolate', ['linear'], ['zoom'], 2, 0.5, 8, 0.8] },
  } as LayerSpecification);
  return {
    version: 8,
    glyphs: GLYPHS,
    sprite: SPRITE,
    sources: { protomaps: { type: 'vector', url: `pmtiles://${abs}`, attribution: ATTRIBUTION } },
    layers,
  };
}

/**
 * Where the weather goes in a style: above land, water and landcover, below
 * roads, borders and every label.
 */
export function weatherBeforeId(layers: { id: string; type: string }[]): string | undefined {
  const water = layers.findIndex((l) => l.id === 'water' || l.id === 'fallback-land');
  for (let i = Math.max(0, water + 1); i < layers.length; i++) {
    const l = layers[i];
    if (l.type === 'symbol' || l.type === 'line' || l.id.startsWith('roads') || l.id.startsWith('boundaries') || l.id === 'buildings') {
      if (l.id === 'water_stream' || l.id === 'water_river') continue;
      return l.id;
    }
  }
  return undefined;
}

/** Overlays (alerts) sit above the weather and borders but under labels. */
export function overlayBeforeId(layers: { id: string; type: string }[]): string | undefined {
  return layers.find((l) => l.type === 'symbol')?.id;
}

// ---------------------------------------------------------------------------
// Fallback
// ---------------------------------------------------------------------------

const CITIES: [string, number, number, number][] = [
  ['New York', 40.7128, -74.006, 1],
  ['Los Angeles', 34.0522, -118.2437, 1],
  ['Chicago', 41.8781, -87.6298, 1],
  ['Houston', 29.7604, -95.3698, 1],
  ['Phoenix', 33.4484, -112.074, 1],
  ['Philadelphia', 39.9526, -75.1652, 2],
  ['San Antonio', 29.4241, -98.4936, 2],
  ['San Diego', 32.7157, -117.1611, 2],
  ['Dallas', 32.7767, -96.797, 1],
  ['Austin', 30.2672, -97.7431, 2],
  ['Jacksonville', 30.3322, -81.6557, 2],
  ['Columbus', 39.9612, -82.9988, 2],
  ['Charlotte', 35.2271, -80.8431, 2],
  ['Indianapolis', 39.7684, -86.1581, 2],
  ['San Francisco', 37.7749, -122.4194, 1],
  ['Seattle', 47.6062, -122.3321, 1],
  ['Denver', 39.7392, -104.9903, 1],
  ['Washington', 38.9072, -77.0369, 1],
  ['Nashville', 36.1627, -86.7816, 2],
  ['Oklahoma City', 35.4676, -97.5164, 2],
  ['El Paso', 31.7619, -106.485, 2],
  ['Boston', 42.3601, -71.0589, 1],
  ['Portland', 45.5152, -122.6784, 2],
  ['Las Vegas', 36.1699, -115.1398, 2],
  ['Detroit', 42.3314, -83.0458, 2],
  ['Memphis', 35.1495, -90.049, 2],
  ['Louisville', 38.2527, -85.7585, 3],
  ['Milwaukee', 43.0389, -87.9065, 3],
  ['Albuquerque', 35.0844, -106.6504, 2],
  ['Kansas City', 39.0997, -94.5786, 1],
  ['Omaha', 41.2565, -95.9345, 2],
  ['Minneapolis', 44.9778, -93.265, 1],
  ['New Orleans', 29.9511, -90.0715, 2],
  ['Miami', 25.7617, -80.1918, 1],
  ['Atlanta', 33.749, -84.388, 1],
  ['St. Louis', 38.627, -90.1994, 2],
  ['Salt Lake City', 40.7608, -111.891, 2],
  ['Tampa', 27.9506, -82.4572, 2],
  ['Pittsburgh', 40.4406, -79.9959, 2],
  ['Cincinnati', 39.1031, -84.512, 3],
  ['Cleveland', 41.4993, -81.6944, 2],
  ['Buffalo', 42.8864, -78.8784, 3],
  ['Des Moines', 41.5868, -93.625, 3],
  ['Little Rock', 34.7465, -92.2896, 3],
  ['Boise', 43.615, -116.2023, 3],
  ['Billings', 45.7833, -108.5007, 3],
  ['Bismarck', 46.8083, -100.7837, 3],
  ['Sioux Falls', 43.5446, -96.7311, 3],
  ['Wichita', 37.6872, -97.3301, 3],
  ['Tulsa', 36.154, -95.9928, 3],
  ['Birmingham', 33.5186, -86.8104, 3],
  ['Raleigh', 35.7796, -78.6382, 3],
  ['Richmond', 37.5407, -77.436, 3],
  ['Toronto', 43.6532, -79.3832, 1],
  ['Montréal', 45.5017, -73.5673, 1],
  ['Vancouver', 49.2827, -123.1207, 1],
  ['Winnipeg', 49.8951, -97.1384, 2],
  ['Calgary', 51.0447, -114.0719, 2],
  ['Monterrey', 25.6866, -100.3161, 2],
  ['Mexico City', 19.4326, -99.1332, 1],
  ['Anchorage', 61.2181, -149.9003, 2],
  ['Honolulu', 21.3069, -157.8583, 2],
  ['Havana', 23.1136, -82.3666, 2],
  ['Bogotá', 4.711, -74.0721, 1],
  ['Lima', -12.0464, -77.0428, 1],
  ['São Paulo', -23.5505, -46.6333, 1],
  ['Rio de Janeiro', -22.9068, -43.1729, 2],
  ['Buenos Aires', -34.6037, -58.3816, 1],
  ['Santiago', -33.4489, -70.6693, 2],
  ['Reykjavík', 64.1466, -21.9426, 2],
  ['London', 51.5072, -0.1276, 1],
  ['Paris', 48.8566, 2.3522, 1],
  ['Madrid', 40.4168, -3.7038, 1],
  ['Berlin', 52.52, 13.405, 1],
  ['Rome', 41.9028, 12.4964, 1],
  ['Stockholm', 59.3293, 18.0686, 2],
  ['Warsaw', 52.2297, 21.0122, 2],
  ['Moscow', 55.7558, 37.6173, 1],
  ['Istanbul', 41.0082, 28.9784, 1],
  ['Cairo', 30.0444, 31.2357, 1],
  ['Lagos', 6.5244, 3.3792, 1],
  ['Nairobi', -1.2921, 36.8219, 1],
  ['Johannesburg', -26.2041, 28.0473, 1],
  ['Riyadh', 24.7136, 46.6753, 2],
  ['Tehran', 35.6892, 51.389, 2],
  ['Karachi', 24.8607, 67.0011, 2],
  ['Delhi', 28.6139, 77.209, 1],
  ['Mumbai', 19.076, 72.8777, 1],
  ['Dhaka', 23.8103, 90.4125, 2],
  ['Bangkok', 13.7563, 100.5018, 1],
  ['Singapore', 1.3521, 103.8198, 1],
  ['Jakarta', -6.2088, 106.8456, 1],
  ['Manila', 14.5995, 120.9842, 1],
  ['Beijing', 39.9042, 116.4074, 1],
  ['Shanghai', 31.2304, 121.4737, 1],
  ['Seoul', 37.5665, 126.978, 1],
  ['Tokyo', 35.6762, 139.6503, 1],
  ['Sydney', -33.8688, 151.2093, 1],
  ['Perth', -31.9505, 115.8605, 2],
  ['Auckland', -36.8485, 174.7633, 2],
];

export async function fallbackStyle(): Promise<StyleSpecification> {
  // Loaded only when needed; Vite emits these as separate assets.
  const [{ default: worldUrl }, { default: statesUrl }] = await Promise.all([
    import('world-atlas/countries-50m.json?url'),
    import('us-atlas/states-10m.json?url'),
  ]);
  const [world, states] = (await Promise.all([fetch(worldUrl).then((r) => r.json()), fetch(statesUrl).then((r) => r.json())])) as [
    Topology,
    Topology,
  ];
  const cities: GeoJSON.FeatureCollection = {
    type: 'FeatureCollection',
    features: CITIES.map(([name, lat, lon, rank]) => ({
      type: 'Feature',
      properties: { name, rank },
      geometry: { type: 'Point', coordinates: [lon, lat] },
    })),
  };
  return {
    version: 8,
    glyphs: GLYPHS,
    sources: {
      land: { type: 'geojson', data: polygons(world, 'countries'), attribution: 'Natural Earth · US Census' },
      coast: { type: 'geojson', data: mesh(world, 'countries', false) },
      borders: { type: 'geojson', data: mesh(world, 'countries', true) },
      states: { type: 'geojson', data: mesh(states, 'states', true) },
      cities: { type: 'geojson', data: cities },
    },
    layers: [
      { id: 'background', type: 'background', paint: { 'background-color': COLORS.water } },
      { id: 'fallback-land', type: 'fill', source: 'land', paint: { 'fill-color': COLORS.land } },
      { id: 'fallback-coast', type: 'line', source: 'coast', paint: { 'line-color': COLORS.coast, 'line-width': 0.8 } },
      { id: 'fallback-states', type: 'line', source: 'states', paint: { 'line-color': COLORS.state, 'line-width': 0.7 } },
      { id: 'fallback-borders', type: 'line', source: 'borders', paint: { 'line-color': COLORS.border, 'line-width': 1.2 } },
      {
        id: 'fallback-cities',
        type: 'symbol',
        source: 'cities',
        filter: ['<=', ['get', 'rank'], ['step', ['zoom'], 1, 4, 2, 5.5, 3]],
        layout: {
          'text-field': ['get', 'name'],
          'text-font': ['Noto Sans Regular'],
          'text-size': ['interpolate', ['linear'], ['zoom'], 3, 11, 8, 14],
          'text-variable-anchor': ['top', 'bottom', 'left', 'right'],
          'text-radial-offset': 0.5,
          'symbol-sort-key': ['get', 'rank'],
        },
        paint: { 'text-color': COLORS.label, 'text-halo-color': COLORS.labelHalo, 'text-halo-width': 1.6 },
      },
      {
        id: 'fallback-city-dots',
        type: 'circle',
        source: 'cities',
        filter: ['<=', ['get', 'rank'], ['step', ['zoom'], 1, 4, 2, 5.5, 3]],
        paint: { 'circle-radius': 2.2, 'circle-color': COLORS.label, 'circle-stroke-color': COLORS.labelHalo, 'circle-stroke-width': 1 },
      },
    ],
  };
}
