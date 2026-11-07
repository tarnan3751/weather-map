// Mirrors of the JSON shapes in docs/protocol.md. The Rust types in
// crates/wx-core are the source of truth; keep these in step with them.

export type Iso = string;

export interface LonLatBox {
  west: number;
  south: number;
  east: number;
  north: number;
}

export interface Config {
  /** `/tiles/{layer}/{z}/{x}/{y}/{parts}.wxt`. */
  tile_url: string;
  tile_cells: number;
  basemap_url: string | null;
  vapid_public_key: string | null;
  server_time: Iso;
}

export type LayerGroup = 'precipitation' | 'temperature' | 'wind' | 'sky';
export type Quantity = 'temperature' | 'speed' | 'reflectivity' | 'precip_rate' | 'precip_depth' | 'snow_depth' | 'percent' | 'category';
export type BandKind = 'continuous' | 'categorical';
export type Aggregate = 'mean' | 'max' | 'nearest';
export type FrameKind = 'observed' | 'forecast';

export interface BandSpec {
  name: string;
  quantity: Quantity;
  unit: string;
  scale: number;
  offset: number;
  kind: BandKind;
  aggregate: Aggregate;
}

export type Render =
  | { kind: 'scalar'; band: number }
  | { kind: 'wind'; u: number; v: number; gust: number | null }
  | { kind: 'precip_type'; rate: number; ptype: number };

export interface FieldSel {
  var: string;
  level: string;
  stat: 'instant' | 'run_total';
}

export type Derive =
  | { op: 'linear'; mul: number; add: number }
  | { op: 'reflectivity' }
  | { op: 'precip_depth' }
  | { op: 'wind' }
  | { op: 'feels_like' }
  | { op: 'precip_type_rate' };

/** What one family of sources contributes to a layer (`wx_core::layer::InputSpec`). */
export interface InputSpec {
  family: string;
  product: string;
  fields: FieldSel[];
  derive: Derive;
  hold_minutes: number;
  fill_past: boolean;
}

export interface LayerSpec {
  id: string;
  title: string;
  group: LayerGroup;
  description: string;
  bands: BandSpec[];
  render: Render;
  palette: string;
  inputs: InputSpec[];
  retention_minutes: number;
  observed_tick_minutes: number;
  /** Accumulations: every frame counts from the `run` of its parts. */
  anchored: boolean;
}

/** What a source's values come from, for wording a reading: a radar "sees
 *  no echo", a satellite estimate "sees no rain". */
export type Medium = 'radar' | 'satellite' | 'analysis' | 'nowcast' | 'model';

/** One place data comes from (`wx_core::source::SourceDef` plus `medium`). */
export interface SourceDef {
  id: string;
  family: string;
  region: string;
  title: string;
  provider: string;
  /** Credit line the map must show while this source is in view. */
  attribution: string;
  attribution_url: string;
  /** The licence the data is used under. */
  licence?: string;
  kind: FrameKind;
  medium: Medium;
  /** Stacking order, higher on top. */
  rank: number;
  res_km: number;
  max_zoom: number;
}

/** One source's data for one layer at one valid time (`wx_core::Part`). */
export interface Part {
  id: string;
  source: string;
  valid: Iso;
  run: Iso | null;
  kind: FrameKind;
  /** May cross the antimeridian: `west > east`. */
  bounds: LonLatBox;
  /** Everywhere inside this box the part has data at full weight. */
  solid?: LonLatBox | null;
  max_zoom: number;
}

/** One time step: part ids, top first. Identified by `valid`. */
export interface Frame {
  valid: Iso;
  kind: FrameKind;
  parts: string[];
}

export interface Timeline {
  frames: Frame[];
  /** Every part any frame names. */
  parts: Part[];
}

export interface AllFrames {
  server_time: Iso;
  timelines: Record<string, Timeline>;
}

export type FeedState = 'starting' | 'healthy' | 'backing_off' | 'stopped';

export interface FeedHealth {
  name: string;
  state: FeedState;
  expected_interval_secs: number;
  started_at: Iso | null;
  last_success: Iso | null;
  last_data_time: Iso | null;
  detail: string;
  last_error: string | null;
  last_error_at: Iso | null;
  consecutive_failures: number;
  restarts: number;
  next_retry_at: Iso | null;
}

/** What one source currently contributes, across every layer. */
export interface SourceStatus {
  id: string;
  title: string;
  provider: string;
  /** Parts held, on a timeline or waiting. */
  parts: number;
  /** Layers it has at least one part on. */
  layers: number;
  newest_valid: Iso | null;
  /** Newest model run it has parts from; null for observations. */
  newest_run: Iso | null;
}

export interface StatusReply {
  server_time: Iso;
  started_at: Iso;
  clients: number;
  feeds: (FeedHealth & { stale: boolean })[];
  layers: { id: string; frames: number; oldest_valid: Iso | null; newest_valid: Iso | null; newest_observed: Iso | null }[];
  /** Every source in the registry, silent ones included. */
  sources?: SourceStatus[];
}

/** `GET /api/provenance`: which parts make the value at a point, top first,
 *  with each one's share of the blend (shares sum to 1; empty: no data). */
export interface Provenance {
  layer: string;
  valid: Iso;
  parts: { id: string; source: string; share: number }[];
}

export type WsEvent =
  | { type: 'hello'; server_time: Iso }
  | { type: 'heartbeat'; server_time: Iso }
  | { type: 'timeline'; layer: string; parts: Part[]; frames: Frame[]; removed: Iso[] }
  | { type: 'alerts'; revision: number; added: string[]; updated: string[]; removed: string[] }
  | { type: 'stations'; updated: Iso; count: number }
  | { type: 'feed'; feed: FeedHealth };

export type Severity = 'Extreme' | 'Severe' | 'Moderate' | 'Minor' | 'Unknown';

/** Where an alert's shape comes from; `none` is an alert without one. */
export type GeomSource = 'polygon' | 'zones' | 'cap' | 'nuts' | 'none';

/** Properties shared by `/api/alerts`, `/api/alerts/at` and the detail. All
 *  text is plain text: render it as text, never as HTML. */
export interface AlertProps {
  id: string;
  event: string;
  severity: Severity;
  urgency: string | null;
  certainty: string | null;
  headline: string | null;
  area_desc: string | null;
  sent: Iso | null;
  onset: Iso | null;
  /** NWS only: when the hazard ends. */
  ends: Iso | null;
  expires: Iso | null;
  /** `expires` is the server's guess (a day after onset), not the sender's:
   *  shown as "until about ...". Always false for NWS alerts. */
  expires_assumed?: boolean;
  geom_source: GeomSource;
  /** `us-nws`, or the WMO register's source id (`de-dwd-en`). */
  source: string;
  /** ISO 3166-1 alpha-2, lower case. */
  country: string | null;
  /** BCP 47 language of the text shown. */
  language: string;
  category: AlertCategory;
  /** `[lon, lat]` on the shape, or the country's point for an alert without one. */
  anchor?: [number, number] | null;
}

/** CAP category, lower case; `other` when the sender gave none. */
export type AlertCategory = 'met' | 'geo' | 'safety' | 'security' | 'rescue' | 'fire' | 'health' | 'env' | 'transport' | 'infra' | 'cbrne' | 'other';

/** One language block of a CAP message. */
export interface AlertInfo {
  language: string;
  event: string;
  headline: string | null;
  description: string | null;
  instruction: string | null;
  area_desc: string | null;
  sender_name: string | null;
  /** Always http(s) when present. */
  web: string | null;
  category: AlertCategory;
  severity: Severity | null;
  urgency: string | null;
  certainty: string | null;
  onset: Iso | null;
  expires: Iso | null;
  /** Whether the block counts: not an earlier step, a cancellation or an
   *  all-clear. Absent from older servers: read as in effect. */
  in_effect?: boolean;
}

export interface AlertDetail extends AlertProps {
  description: string | null;
  instruction: string | null;
  sender_name: string | null;
  message_type: string | null;
  status: string | null;
  active: boolean;
  source_name: string | null;
  country_name: string | null;
  /** Every language block of the message (NWS alerts have one). */
  infos: AlertInfo[];
}

export interface Geometry {
  type: 'Polygon' | 'MultiPolygon' | 'Point';
  coordinates: unknown;
}

export interface Feature<P> {
  type: 'Feature';
  id?: string | number;
  geometry: Geometry;
  properties: P;
}

export interface FeatureCollection<P> {
  type: 'FeatureCollection';
  features: Feature<P>[];
}

/** An alert in `/api/alerts`: `geometry` is null for one without a shape. */
export interface AlertFeature {
  type: 'Feature';
  id?: string;
  geometry: Geometry | null;
  properties: AlertProps;
}

export interface AlertCollection {
  type: 'FeatureCollection';
  revision: number;
  /** More alerts matched than `limit`. */
  truncated: boolean;
  features: AlertFeature[];
}

/** `GET /api/alerts/tiles.json`. */
export interface AlertTileJson {
  tilejson: string;
  name: string;
  revision: number;
  tiles: string[];
  minzoom: number;
  maxzoom: number;
  attribution?: string;
  vector_layers: { id: string; minzoom: number; maxzoom: number; fields: Record<string, string> }[];
}

export type StationKind = 'airport' | 'buoy' | 'station';

/** A measurement a station does not report is absent. */
export interface StationProps {
  id: string;
  name: string;
  kind: StationKind;
  /** A key of the collection's `sources`. */
  source: string;
  observed: Iso;
  temp_c?: number;
  dewpoint_c?: number;
  water_temp_c?: number;
  /** Where the wind blows from, 1 to 360; absent for calm or variable. */
  wind_dir_deg?: number;
  wind_speed_ms?: number;
  wind_gust_ms?: number;
  visibility_m?: number;
  wave_height_m?: number;
  altimeter_hpa?: number;
  flight_category?: string;
  raw?: string;
}

export interface StationCollection extends FeatureCollection<StationProps> {
  sources: Record<string, { name: string; attribution: string }>;
}

export interface PlaceAlert {
  id: string;
  event: string;
  severity: Severity;
  headline: string | null;
  ends: Iso | null;
}

export interface Place {
  id: string;
  name: string;
  lat: number;
  lon: number;
  notify: boolean;
  created_at: Iso;
  alerts: PlaceAlert[];
}

/** One step of the point forecast. Any number may be null. */
export interface HourlyPoint {
  /** Start of the step, UTC. */
  time: Iso;
  /** 1, or 6 further out, when `precip_mm`, `pop` and `condition` cover all six hours. */
  span_h: number;
  temp_c: number | null;
  /** Percent; often absent outside the US and the Nordic region. */
  pop: number | null;
  precip_mm: number | null;
  wind_ms: number | null;
  /** Where the wind blows from, 0 to 359. */
  wind_dir_deg: number | null;
  /** A MET Norway symbol code, perhaps with `_day`/`_night`/`_polartwilight`. */
  condition: string | null;
  /** The condition in English. */
  text: string | null;
}

/** One local calendar day. */
export interface DailyPoint {
  /** `YYYY-MM-DD`, local. */
  date: string;
  low_c: number | null;
  high_c: number | null;
  pop: number | null;
  precip_mm: number | null;
  condition: string | null;
  /** The provider's own written forecast, in its own units; often null. */
  text: string | null;
}

export interface PointForecast {
  lat: number;
  lon: number;
  /** `nws`, `met-no`, `model`, ... */
  source: string;
  /** The credit line to show with the forecast. */
  attribution: string;
  place: string | null;
  /** IANA zone, when the provider gives one. */
  time_zone: string | null;
  generated_at: Iso | null;
  hourly: HourlyPoint[];
  daily: DailyPoint[];
}
